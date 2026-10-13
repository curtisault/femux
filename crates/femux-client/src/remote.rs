//! Where the client's daemon is (REQ-PROTO-003): the local socket, or a remote reached through
//! the user's own OpenSSH. femux never authenticates anyone (REQ-SEC-001); it only runs `ssh`.
//!
//! - First, while the terminal is still in its normal mode, `ssh <dest> true` opens an OpenSSH
//!   master connection (`ControlMaster`), so host-key and password prompts work as they do
//!   for plain `ssh`. Every later command, reconnects included, runs over that master.
//! - Default: `ssh -T <dest> femux attach --stdio`, the protocol on the channel's stdin/stdout.
//! - `forward = true`: `ssh <dest> femux daemon --ensure` names the remote socket, then
//!   `ssh -N -L <local>:<remote> <dest>` forwards it and the client connects locally.

use std::path::PathBuf;
use std::process::Command;

/// A remote host as `attach --host` resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// What the user called it: a saved host's name or the raw destination.
    pub label: String,
    /// The `ssh` destination.
    pub ssh: String,
    /// The `femux` command on the remote.
    pub femux: String,
    pub forward: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Local(PathBuf),
    Remote(Remote),
}

impl Target {
    pub fn remote(&self) -> Option<&Remote> {
        match self {
            Target::Remote(r) => Some(r),
            Target::Local(_) => None,
        }
    }
}

/// `$FEMUX_SSH`, else `ssh`.
pub fn ssh_program() -> String {
    std::env::var("FEMUX_SSH")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "ssh".into())
}

/// Where femux keeps OpenSSH master sockets: `/tmp/femux-ssh-<uid>` (short, for the socket
/// path limit), one socket per destination (`%C`).
pub fn control_dir() -> PathBuf {
    // SAFETY: getuid has no preconditions.
    PathBuf::from(format!("/tmp/femux-ssh-{}", unsafe { libc::getuid() }))
}

impl Remote {
    /// `ssh` with the options every femux link uses: a shared master connection that
    /// outlives the client by ten minutes, and keepalives that notice a dead link within
    /// about 15 s (the "link lost" path). `batch` forbids prompts, for commands that run
    /// while the client owns the terminal.
    fn ssh(&self, batch: bool) -> Command {
        let mut c = Command::new(ssh_program());
        c.args(["-o", "ControlMaster=auto", "-o", "ControlPersist=600"])
            .arg("-o")
            .arg(format!("ControlPath={}/%C", control_dir().display()))
            .args(["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=3"]);
        if batch {
            c.args(["-o", "BatchMode=yes"]);
        }
        c
    }

    /// Opens (or reuses) the master connection in the user's terminal, before the client
    /// takes it over: prompts for host keys and passwords appear and are answered normally.
    pub fn master_command(&self) -> Command {
        let mut c = self.ssh(false);
        c.arg(&self.ssh).arg("true");
        c
    }

    /// Runs [`Remote::master_command`] on the terminal as it is.
    pub fn open_master(&self) -> std::io::Result<()> {
        let dir = control_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let status = self
            .master_command()
            .status()
            .map_err(|e| std::io::Error::other(format!("{}: {e}", ssh_program())))?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "ssh to {} failed ({status})",
                self.label
            )));
        }
        Ok(())
    }

    /// The protocol over the SSH channel.
    pub fn stdio_command(&self, batch: bool) -> Command {
        let mut c = self.ssh(batch);
        c.arg("-T")
            .arg(&self.ssh)
            .args([self.femux.as_str(), "attach", "--stdio"]);
        c
    }

    /// Starts the remote daemon if needed and prints its socket.
    pub fn ensure_command(&self, batch: bool) -> Command {
        let mut c = self.ssh(batch);
        c.arg("-T")
            .arg(&self.ssh)
            .args([self.femux.as_str(), "daemon", "--ensure"]);
        c
    }

    /// Forwards `local` to the remote daemon's socket at `remote`.
    pub fn forward_command(&self, local: &std::path::Path, remote: &str, batch: bool) -> Command {
        let mut c = self.ssh(batch);
        c.args([
            "-N",
            "-T",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "StreamLocalBindUnlink=yes",
            "-L",
        ])
        .arg(format!("{}:{remote}", local.display()))
        .arg(&self.ssh);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(c: &Command) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn ssh_commands_use_openssh_with_keepalives() {
        let r = Remote {
            label: "box".into(),
            ssh: "me@build-box".into(),
            femux: "~/bin/femux".into(),
            forward: false,
        };
        let a = args(&r.stdio_command(false));
        assert!(a.join(" ").contains("ServerAliveInterval=5"));
        // Everything shares one master connection, opened first in the user's terminal.
        assert!(a.join(" ").contains("ControlMaster=auto"));
        assert!(
            a.join(" ")
                .contains(&format!("ControlPath={}/%C", control_dir().display()))
        );
        assert!(args(&r.master_command()).ends_with(&["me@build-box", "true"].map(String::from)));
        assert!(!a.join(" ").contains("BatchMode"));
        assert!(a.ends_with(
            &["-T", "me@build-box", "~/bin/femux", "attach", "--stdio"].map(String::from)
        ));
        assert!(
            args(&r.stdio_command(true))
                .join(" ")
                .contains("BatchMode=yes")
        );
        assert!(
            args(&r.ensure_command(false)).ends_with(&["daemon", "--ensure"].map(String::from))
        );
        let f = args(&r.forward_command(std::path::Path::new("/tmp/l.sock"), "/run/r.sock", false));
        let f = f.join(" ");
        assert!(
            f.contains("-N") && f.contains("-L /tmp/l.sock:/run/r.sock me@build-box"),
            "{f}"
        );
        assert!(f.contains("ExitOnForwardFailure=yes"));
    }
}
