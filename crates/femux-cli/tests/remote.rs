//! 0.6.0 acceptance against the real binary: attach over SSH, a dropped link reconnects
//! while the remote keeps the session, and remote notifications play locally (REQ-PROTO-003,
//! REQ-SEC-007, REQ-NOTIFY-003). A fake `ssh` on PATH runs the remote command locally, as a
//! second "remote" femux with its own home and socket.

mod common;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use common::{Env, FEMUX, contains, exit_within, on_pty, wait_until};

struct Rig {
    local: Env,
    remote: Env,
    bin: tempfile::TempDir,
}

impl Rig {
    fn new() -> Rig {
        let local = Env::new();
        let remote = Env::new();
        let bin = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(FEMUX, bin.path().join("femux")).unwrap();
        let r = remote.dir.path();
        let script = format!(
            r#"#!/bin/sh
# Fake OpenSSH: logs its arguments and runs the remote command here, as the "remote".
echo "$*" >> '{log}'
fwd=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o|-p|-l|-i|-F|-J) shift 2 ;;
    -L) fwd="$2"; shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
shift
export HOME='{home}' XDG_CONFIG_HOME='{home}/config' XDG_STATE_HOME='{home}/state'
export FEMUX_CONFIG='{home}/config/femux/config.toml' FEMUX_SOCKET='{sock}'
unset FEMUX_PANE
if [ -n "$fwd" ]; then
  ln -sf "${{fwd#*:}}" "${{fwd%%:*}}"
  exec sleep 600
fi
echo $$ > '{pid}'
exec "$@"
"#,
            log = bin.path().join("ssh.log").display(),
            home = r.display(),
            sock = remote.socket().display(),
            pid = bin.path().join("ssh.pid").display(),
        );
        let ssh = bin.path().join("ssh");
        std::fs::write(&ssh, script).unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Rig { local, remote, bin }
    }

    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.bin.path().join("ssh.log")).unwrap_or_default()
    }

    /// `femux attach --host <host>` on a PTY of our own.
    fn attach(
        &self,
        host: &str,
    ) -> (
        std::process::Child,
        std::fs::File,
        std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ) {
        let mut cmd = self.local.cmd(&["attach", "--host", host]);
        cmd.env("PATH", self.path()).env("TERM_PROGRAM", "ghostty");
        on_pty(cmd)
    }

    fn remote_pid(&self) -> i32 {
        std::fs::read_to_string(self.bin.path().join("ssh.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
}

fn type_line(master: &mut std::fs::File, text: &str) {
    master.write_all(format!("{text}\r").as_bytes()).unwrap();
}

fn remote_clients(remote: &Env) -> u64 {
    remote.json(&["ls"])["sessions"][0]["clients"]
        .as_u64()
        .unwrap_or(0)
}

#[test]
fn attach_over_ssh_runs_the_remote_session_and_detaches() {
    let rig = Rig::new();
    rig.local.run(&["hosts", "add", "box", "me@build-box"]);
    let (mut child, mut master, out) = rig.attach("box");
    wait_until("remote daemon", Duration::from_secs(15), || {
        rig.remote.socket().exists()
    });
    wait_until("attached", Duration::from_secs(15), || {
        rig.remote.socket().exists() && remote_clients(&rig.remote) == 1
    });
    std::thread::sleep(Duration::from_millis(500));
    // Runs on the remote: its HOME is the remote's.
    type_line(&mut master, "echo remote-$((6*7))-$(basename $HOME)");
    let home = rig
        .remote
        .dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    wait_until("remote output", Duration::from_secs(10), || {
        contains(&out, &format!("remote-42-{home}"))
    });
    // The session chip names the host.
    assert!(
        contains(&out, "session box:main"),
        "{}",
        String::from_utf8_lossy(&out.lock().unwrap())
    );
    let log = rig.log();
    // First the master connection in the plain terminal, then the protocol over it.
    let first = log.find("me@build-box true").expect("master first");
    let stdio = log
        .find("-T me@build-box femux attach --stdio")
        .expect("stdio");
    assert!(first < stdio, "{log}");
    assert!(log.contains("ControlMaster=auto"), "{log}");
    assert!(log.contains("ServerAliveInterval=5"), "{log}");
    // prefix d: the client leaves, the remote keeps the session.
    master.write_all(b"\x00d").unwrap();
    let status = exit_within(&mut child, Duration::from_secs(10)).expect("client exits");
    assert!(status.success(), "{status:?}");
    wait_until("view detached", Duration::from_secs(10), || {
        remote_clients(&rig.remote) == 0
    });
    assert!(rig.remote.run(&["ls"]).starts_with("main: 1 tabs"));
    // Nothing on the local machine: no daemon was started there.
    assert!(!rig.local.socket().exists());
}

#[test]
fn a_dropped_link_reconnects_and_the_session_survives() {
    let rig = Rig::new();
    let (mut child, mut master, out) = rig.attach("me@build-box");
    wait_until("attached", Duration::from_secs(15), || {
        rig.remote.socket().exists() && remote_clients(&rig.remote) == 1
    });
    std::thread::sleep(Duration::from_millis(500));
    type_line(&mut master, "sleep 600 & echo before-$((6*7))");
    wait_until("output", Duration::from_secs(10), || {
        contains(&out, "before-42")
    });
    // The link dies: the remote end of the channel goes away.
    let first = rig.remote_pid();
    unsafe { libc::kill(first, libc::SIGKILL) };
    wait_until("link lost shown", Duration::from_secs(10), || {
        contains(&out, "link to me@build-box lost")
    });
    // Meanwhile the remote session and its processes are untouched.
    assert!(rig.remote.run(&["ls"]).starts_with("main: 1 tabs"));
    wait_until("reconnected", Duration::from_secs(20), || {
        contains(&out, "reconnected to me@build-box")
    });
    assert_ne!(rig.remote_pid(), first);
    assert!(rig.log().contains("BatchMode=yes"), "{}", rig.log());
    type_line(&mut master, "jobs; echo after-$((6*7))");
    wait_until("output after reconnect", Duration::from_secs(10), || {
        contains(&out, "after-42")
    });
    // The background job started before the drop is still there.
    let pane = rig.remote.json(&["agent", "list", "--all"])["agents"][0]["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let screen = rig.remote.run(&["pane", "cat", "--pane", &pane]);
    assert!(
        screen.contains("sleep 600") && screen.contains("Running"),
        "{screen}"
    );
    master.write_all(b"\x00d").unwrap();
    assert!(exit_within(&mut child, Duration::from_secs(10)).is_some());
}

#[test]
fn remote_notifications_play_on_the_client() {
    let rig = Rig::new();
    // The remote daemon raises agent_done; the client runs its own actions for it. The
    // remote's sound file is never used: actions come from the client's config (REQ-SEC-007).
    rig.remote
        .write_config("[notify]\nagent_done = { sound = \"/remote/only.wav\" }\n");
    rig.local
        .write_config("[notify]\nagent_done = { desktop = true }\n");
    let (mut child, mut master, out) = rig.attach("me@build-box");
    wait_until("attached", Duration::from_secs(15), || {
        rig.remote.socket().exists() && remote_clients(&rig.remote) == 1
    });
    let pane = rig.remote.json(&["agent", "list", "--all"])["agents"][0]["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.remote.run(&[
        "pane",
        "set",
        "--pane",
        &pane,
        "agent.state=done",
        "agent.name=claude-code",
    ]);
    wait_until(
        "desktop notification on the local terminal",
        Duration::from_secs(10),
        || contains(&out, "\x1b]777;notify;claude-code done;"),
    );
    assert!(!contains(&out, "only.wav"));
    master.write_all(b"\x00d").unwrap();
    assert!(exit_within(&mut child, Duration::from_secs(10)).is_some());
}

#[test]
fn forwarded_socket_attach() {
    let rig = Rig::new();
    rig.local
        .run(&["hosts", "add", "fwd", "me@build-box", "--forward"]);
    let (mut child, mut master, out) = rig.attach("fwd");
    wait_until("attached", Duration::from_secs(15), || {
        rig.remote.socket().exists() && remote_clients(&rig.remote) == 1
    });
    std::thread::sleep(Duration::from_millis(500));
    type_line(&mut master, "echo forwarded-$((6*7))");
    wait_until("output", Duration::from_secs(10), || {
        contains(&out, "forwarded-42")
    });
    let log = rig.log();
    assert!(log.contains("me@build-box femux daemon --ensure"), "{log}");
    assert!(
        log.contains("-L ") && log.contains(&format!(":{}", rig.remote.socket().display())),
        "{log}"
    );
    master.write_all(b"\x00d").unwrap();
    assert!(exit_within(&mut child, Duration::from_secs(10)).is_some());
}

#[test]
fn hosts_add_list_rm() {
    let env = Env::new();
    env.run(&[
        "hosts",
        "add",
        "box",
        "me@build-box",
        "--femux",
        "~/bin/femux",
    ]);
    env.run(&["hosts", "add", "pi", "pi", "--forward"]);
    let list = env.json(&["hosts", "list"]);
    assert_eq!(list["hosts"][0]["name"], "box");
    assert_eq!(list["hosts"][0]["femux"], "~/bin/femux");
    assert_eq!(list["hosts"][1]["forward"], true);
    let text = env.run(&["hosts", "list"]);
    assert!(
        text.contains("me@build-box") && text.contains("forward"),
        "{text}"
    );
    let file: PathBuf = env.dir.path().join("config/femux/hosts.toml");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("[host.box]")
    );
    env.run(&["hosts", "rm", "box"]);
    assert_eq!(
        env.json(&["hosts", "list"])["hosts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(env.json(&["hosts", "rm", "nope"])["error"].is_string());
}

/// ADR-021 #4: over SSH the client asks for zlib-compressed images and hands them to its host
/// compressed (`o=z`).
#[test]
fn remote_images_reach_the_host_compressed() {
    let rig = Rig::new();
    let (mut child, mut master, out) = rig.attach("me@build-box");
    // Answer the client's probe as a Kitty-capable terminal would.
    wait_until("the probe", Duration::from_secs(10), || {
        contains(&out, "\x1b[c")
    });
    master
        .write_all(b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4;22c")
        .unwrap();
    wait_until("attached", Duration::from_secs(15), || {
        rig.remote.socket().exists() && remote_clients(&rig.remote) == 1
    });
    let pane = rig.remote.json(&["agent", "list", "--all"])["agents"][0]["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.remote.run(&[
        "agent",
        "prompt",
        &pane,
        "printf '\\033_Ga=T,f=24,s=2,v=2,i=7,c=4,r=2;/wAA/wAA/wAA/wAA\\033\\\\'",
    ]);
    wait_until("a compressed transmission", Duration::from_secs(15), || {
        contains(&out, ",o=z,")
    });
    assert!(contains(&out, "a=p,"));
    master.write_all(b"\x00d").unwrap();
    assert!(exit_within(&mut child, Duration::from_secs(10)).is_some());
}
