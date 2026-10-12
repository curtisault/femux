//! REQ-ARCH-004 and REQ-LIFE-001 against the real `femux` binary, plus the bare viewer end to
//! end. The viewer runs on a PTY owned by the test, never on the developer's terminal.

mod common;

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Env, exit_within, wait_until};

/// Runs `cmd` with a new PTY as its controlling terminal; returns the child, the master and a
/// buffer that accumulates everything the child writes.
fn on_pty(mut cmd: Command) -> (Child, std::fs::File, Arc<Mutex<Vec<u8>>>) {
    let ws = nix::pty::Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = nix::pty::openpty(Some(&ws), None).unwrap();
    let slave: OwnedFd = pty.slave;
    cmd.stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    // SAFETY: async-signal-safe calls only.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let child = cmd.spawn().unwrap();
    let master = std::fs::File::from(pty.master);
    let out = Arc::new(Mutex::new(Vec::new()));
    let (mut r, o) = (master.try_clone().unwrap(), out.clone());
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            o.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    let _ = master.as_raw_fd();
    (child, master, out)
}

fn contains(out: &Mutex<Vec<u8>>, needle: &str) -> bool {
    String::from_utf8_lossy(&out.lock().unwrap()).contains(needle)
}

#[test]
fn arch_004_plain_femux_starts_a_daemon_and_attaches() {
    let env = Env::new();
    assert!(!env.socket().exists());
    let (mut child, mut master, out) = on_pty(env.cmd(&[]));
    wait_until("daemon socket", Duration::from_secs(10), || {
        env.socket().exists()
    });
    // The viewer is up once it has drawn the shell; then type into the pane.
    std::thread::sleep(Duration::from_millis(1000));
    master.write_all(b"echo femux-$((6*7))\r").unwrap();
    wait_until(
        "command output in the viewer",
        Duration::from_secs(10),
        || contains(&out, "femux-42"),
    );
    // prefix d: ctrl+space (NUL) then d.
    master.write_all(b"\x00d").unwrap();
    let status =
        exit_within(&mut child, Duration::from_secs(10)).expect("viewer exits on prefix d");
    assert!(status.success(), "{status:?}");
    // Detach leaves the session and its processes running.
    assert_eq!(env.run(&["ls"]).trim(), "0: 1 tabs, 0 clients");
}

#[test]
fn arch_004_daemon_subcommand_detaches_and_refuses_a_second() {
    let env = Env::new();
    env.run(&["daemon"]);
    assert!(env.socket().exists());
    let second = env.cmd(&["daemon"]).output().unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));
    env.run(&["new", "s"]);
    assert!(env.run(&["ls"]).starts_with("s: 1 tabs"));
}

fn alive(pid: i32) -> bool {
    // Zombies count as gone: they have exited.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => !s[s.rfind(')').unwrap() + 2..].starts_with('Z'),
        Err(_) => {
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
                && !cfg!(target_os = "linux")
        }
    }
}

#[test]
fn life_001_sigterm_hups_panes_and_exits() {
    let env = Env::new();
    let mut daemon = env
        .cmd(&["daemon", "--foreground"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("daemon socket", Duration::from_secs(10), || {
        env.socket().exists()
    });
    env.run(&["new", "s"]);
    let pid = shell_pid(&env.socket());
    assert!(alive(pid));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let status =
        exit_within(&mut daemon, Duration::from_secs(10)).expect("daemon exits on SIGTERM");
    assert!(status.success(), "{status:?}");
    wait_until("pane shell gone", Duration::from_secs(5), || !alive(pid));
    assert!(!env.socket().exists(), "socket removed");
}

fn shell_pid(socket: &Path) -> i32 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = femux_test::Client::connect(socket, femux_test::Client::hello(80, 24))
            .await
            .unwrap();
        let snap = c
            .call(femux_proto::msg::SessionAttach {
                name: "s".into(),
                tab: None,
                cols: None,
                rows: None,
            })
            .await
            .unwrap()
            .snapshot;
        let info = c
            .call(femux_proto::msg::PaneInfo {
                pane_id: snap.tabs[0].focus.clone(),
            })
            .await
            .unwrap();
        info.fg_pid as i32
    })
}

/// REQ-LIFE-002 from the CLI: `femux kill` on the last session reports success, then the
/// daemon is gone. Repeated because the reply used to race the daemon's exit.
#[test]
fn life_002_kill_of_the_last_session_succeeds() {
    for round in 0..8 {
        let env = Env::new();
        env.run(&["daemon"]);
        env.run(&["new", "s"]);
        let (ok, _, err) = env.try_run(&["kill", "s"]);
        assert!(ok, "round {round}: femux kill failed: {err}");
        wait_until("daemon exit", Duration::from_secs(5), || {
            !env.socket().exists()
        });
    }
}

/// A session's tabs, read-only.
fn session_tabs(socket: &Path, name: &str) -> Vec<femux_proto::msg::TabSnapshot> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = femux_test::Client::connect(socket, femux_test::Client::hello(80, 24))
            .await
            .unwrap();
        c.call(femux_proto::msg::SessionTabs { name: name.into() })
            .await
            .unwrap()
            .tabs
    })
}

/// What a restart must keep: ids, user titles, trees and floats.
fn shape(tabs: &[femux_proto::msg::TabSnapshot]) -> Vec<String> {
    tabs.iter()
        .map(|t| format!("{} {:?} {:?} {:?}", t.id, t.custom_title, t.tree, t.floats))
        .collect()
}

fn start_foreground(env: &Env) -> Child {
    let daemon = env
        .cmd(&["daemon", "--foreground"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("daemon socket", Duration::from_secs(10), || {
        std::os::unix::net::UnixStream::connect(env.socket()).is_ok()
    });
    daemon
}

/// 0.3.0 acceptance (SPEC §13): kill the daemon, restart it, and the layout returns; with
/// REQ-LIFE-001, SIGTERM takes a snapshot before exiting, and even SIGKILL loses nothing once
/// the debounced snapshot of the last layout change is written.
#[test]
fn acceptance_0_3_kill_the_daemon_restart_and_the_layout_returns() {
    let env = Env::new();
    env.write_config("[restore]\nenabled = true\nstagger_ms = 10\n");
    let sessions = env.dir.path().join("config/femux/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("work.toml"),
        "root = \"~\"\ntabs = 2\n\n[[tab]]\ntitle = \"long\"\ncommand = \"echo long-$((6*7)); sleep 600\"\nrestore = true\n",
    )
    .unwrap();
    for signal in [
        nix::sys::signal::Signal::SIGTERM,
        nix::sys::signal::Signal::SIGKILL,
    ] {
        let mut daemon = start_foreground(&env);
        if signal == nix::sys::signal::Signal::SIGTERM {
            env.run(&["launch", "work", "--detach"]);
            // Shape the second tab: a split, then a float of the new pane.
            let tabs = session_tabs(&env.socket(), "work");
            let out = env.json(&["pane", "split", "--pane", &tabs[1].focus, "--dir", "h"]);
            let new = out["pane_id"].as_str().unwrap().to_owned();
            env.json(&["pane", "split", "--pane", &new, "--dir", "v"]);
        } else {
            // The session came back from the SIGTERM round; change it, let the debounce pass.
            let tabs = session_tabs(&env.socket(), "work");
            env.json(&["tab", "rename", "--tab", &tabs[1].id, "renamed"]);
            std::thread::sleep(Duration::from_millis(1500));
        }
        let before = session_tabs(&env.socket(), "work");
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(daemon.id() as i32), signal).unwrap();
        exit_within(&mut daemon, Duration::from_secs(10)).expect("daemon exits");
        let _ = std::fs::remove_file(env.socket());

        let mut daemon = start_foreground(&env);
        let after = session_tabs(&env.socket(), "work");
        assert_eq!(shape(&after), shape(&before), "after {signal:?}");
        // The restore = true tab re-runs its command (its output is new: scrollback is not
        // persisted); the others are restored shells.
        wait_until("command re-run", Duration::from_secs(10), || {
            env.json(&["pane", "cat", "--pane", &after[0].focus])["lines"]
                .as_array()
                .is_some_and(|l| l.iter().any(|l| l == "long-42"))
        });
        let rerun = env.json(&["pane", "info", "--pane", &after[0].focus]);
        assert_eq!(rerun["restored"], false, "{rerun}");
        let other = env.json(&["pane", "info", "--pane", &after[1].focus]);
        assert_eq!(other["restored"], true, "{other}");
        env.stop_daemon();
        exit_within(&mut daemon, Duration::from_secs(10)).expect("stopped");
    }
}
