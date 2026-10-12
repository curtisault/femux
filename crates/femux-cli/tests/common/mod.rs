//! Shared helpers for binary tests: a temp HOME with its own socket, config and state.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub const FEMUX: &str = env!("CARGO_BIN_EXE_femux");

pub struct Env {
    pub dir: tempfile::TempDir,
}

impl Env {
    pub fn new() -> Env {
        Env {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.path().join("run/femux.sock")
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(FEMUX);
        c.arg("--socket").arg(self.socket()).args(args);
        c.env("SHELL", "/bin/sh")
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env(
                "FEMUX_CONFIG",
                self.dir.path().join("config/femux/config.toml"),
            )
            .env("TERM", "xterm-256color")
            .env_remove("FEMUX_SOCKET")
            .env_remove("FEMUX_PANE");
        c
    }

    pub fn run(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "femux {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Env {
    /// Stops the daemon at this socket, if one is running (`server.stop`). Sends the request
    /// without waiting for answers, so a daemon already on its way out cannot make it panic.
    pub fn stop_daemon(&self) {
        use std::io::Write;
        let socket = self.socket();
        let Ok(mut s) = std::os::unix::net::UnixStream::connect(&socket) else {
            return;
        };
        let hello = femux_test::Client::hello(80, 24);
        let stop = femux_proto::msg::ServerStop { kill_panes: true };
        for (id, req) in [
            (1, femux_proto::Request::from(hello)),
            (2, femux_proto::Request::from(stop)),
        ] {
            if let Ok(bytes) = req.encode(id)
                && s.write_all(&bytes).is_err()
            {
                return;
            }
        }
        // No assertion: this also runs from Drop, possibly while a test is failing.
        let deadline = Instant::now() + Duration::from_secs(5);
        while socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // Leave no daemon behind, whatever sessions it holds.
        self.stop_daemon();
    }
}

pub fn wait_until(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn exit_within(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return Some(s);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Env {
    /// Runs `femux --json args` and parses the output.
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.cmd(&["--json"]).args(args).output().unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{args:?}: {e}: {text}"))
    }

    /// Runs `femux args`; returns (success, stdout, stderr).
    pub fn try_run(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    pub fn write_config(&self, text: &str) {
        let p = self.dir.path().join("config/femux/config.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}
