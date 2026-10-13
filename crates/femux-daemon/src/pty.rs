//! PTY manager: spawning pane processes and the OS queries about them.
//!
//! All PTY master fds are owned here (REQ-LIFE-003 keeps a later fd handoff possible).

use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::Path;

use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, tcgetpgrp};
use tokio::io::unix::AsyncFd;
use tokio::process::{Child, Command};

/// What to run in a new pane.
#[derive(Debug, Clone)]
pub struct SpawnSpec<'a> {
    pub shell: &'a str,
    /// Run with `shell -c cmd`; `None` runs the shell itself.
    pub cmd: Option<&'a str>,
    pub login: bool,
    pub cwd: &'a Path,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    /// One cell in pixels, for the pty's pixel size.
    pub cell_px: (u32, u32),
}

/// A running pane process and the master side of its PTY.
pub struct Spawned {
    pub master: AsyncFd<OwnedFd>,
    pub child: Child,
    pub pid: u32,
}

/// The window size with pixel dimensions from the cell size, so programs that size images
/// from `TIOCGWINSZ` (kitten icat) see the real thing (REQ-GFX-001).
fn winsize(cols: u16, rows: u16, cell: (u32, u32)) -> Winsize {
    let (cols, rows) = (cols.max(1), rows.max(1));
    let px = |n: u16, c: u32| (n as u32 * c).min(u16::MAX as u32) as u16;
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: px(cols, cell.0),
        ws_ypixel: px(rows, cell.1),
    }
}

/// The cell size panes start with, until a client reports one (femux-vt's default).
pub const DEFAULT_CELL_PX: (u32, u32) = (10, 20);

/// Spawns `spec` on a new PTY. The child gets its own session and process group with the
/// PTY as controlling terminal (REQ-PTY-001).
pub fn spawn(spec: &SpawnSpec) -> io::Result<Spawned> {
    let pty = openpty(Some(&winsize(spec.cols, spec.rows, spec.cell_px)), None)
        .map_err(io::Error::from)?;
    let (master, slave) = (pty.master, pty.slave);
    let flags =
        OFlag::from_bits_truncate(fcntl(&master, FcntlArg::F_GETFL).map_err(io::Error::from)?);
    fcntl(&master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).map_err(io::Error::from)?;
    fcntl(&master, FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC)).map_err(io::Error::from)?;

    let shell_name = Path::new(spec.shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sh");
    let mut cmd = Command::new(spec.shell);
    match spec.cmd {
        Some(c) => {
            cmd.arg("-c").arg(c);
        }
        None if spec.login => {
            cmd.arg0(format!("-{shell_name}"));
        }
        None => {}
    }
    cmd.current_dir(spec.cwd)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave)
        .kill_on_drop(false);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    let pid = child.id().unwrap_or(0);
    Ok(Spawned {
        master: AsyncFd::new(master)?,
        child,
        pid,
    })
}

/// Sets the PTY window size; the kernel sends SIGWINCH to the foreground group.
pub fn resize(master: &impl AsFd, cols: u16, rows: u16, cell: (u32, u32)) -> io::Result<()> {
    let ws = winsize(cols, rows, cell);
    // SAFETY: TIOCSWINSZ reads a winsize from a valid pointer.
    let r = unsafe {
        libc::ioctl(
            master.as_fd().as_raw_fd(),
            libc::TIOCSWINSZ as _,
            &ws as *const Winsize,
        )
    };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Foreground process group of the PTY (REQ-PTY-002).
pub fn foreground(master: &impl AsFd) -> Option<u32> {
    tcgetpgrp(master).ok().map(|p| p.as_raw() as u32)
}

/// Sends `sig` to process group `pgid`.
pub fn signal_group(pgid: u32, sig: Signal) -> io::Result<()> {
    killpg(Pid::from_raw(pgid as i32), sig).map_err(io::Error::from)
}

/// Command name of `pid`.
#[cfg(target_os = "linux")]
pub fn proc_name(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(s.trim_end().to_owned())
}

/// Working directory of `pid`.
#[cfg(target_os = "linux")]
pub fn proc_cwd(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Command name of `pid`. macOS: best effort via libproc.
#[cfg(target_os = "macos")]
pub fn proc_name(pid: u32) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for its length.
    let n = unsafe { libc::proc_name(pid as i32, buf.as_mut_ptr() as *mut _, buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// Working directory of `pid`. macOS: best effort via libproc.
#[cfg(target_os = "macos")]
pub fn proc_cwd(pid: u32) -> Option<String> {
    // SAFETY: the struct is plain data and its size is passed to the call.
    unsafe {
        let mut info: libc::proc_vnodepathinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
        let n = libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut _,
            size,
        );
        if n != size {
            return None;
        }
        let path = std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr() as *const _);
        Some(path.to_string_lossy().into_owned())
    }
}

/// Environment entries allowed into persistence by `[env] allow` (REQ-PTY-006, REQ-SEC-005).
/// A pattern ending in `*` matches by prefix.
pub fn filter_env<'a>(env: &'a [(String, String)], allow: &[String]) -> Vec<&'a (String, String)> {
    env.iter()
        .filter(|(k, _)| {
            allow.iter().any(|a| match a.strip_suffix('*') {
                Some(prefix) => k.starts_with(prefix),
                None => k == a,
            })
        })
        .collect()
}

/// The default `[env] allow` list.
pub fn default_env_allow() -> Vec<String> {
    ["PATH", "HOME", "SHELL", "LANG", "LC_*", "TERM", "EDITOR"]
        .map(String::from)
        .to_vec()
}

#[cfg(test)]
mod tests {

    #[test]
    fn winsize_carries_pixels_from_the_cell_size() {
        let w = winsize(80, 24, (9, 18));
        assert_eq!(
            (w.ws_col, w.ws_row, w.ws_xpixel, w.ws_ypixel),
            (80, 24, 720, 432)
        );
        assert_eq!(winsize(0, 0, DEFAULT_CELL_PX).ws_xpixel, 10);
    }

    use super::*;

    #[test]
    fn env_filter_matches_names_and_prefixes() {
        let env: Vec<(String, String)> = [
            ("PATH", "/bin"),
            ("LC_ALL", "C"),
            ("AWS_SECRET", "x"),
            ("HOMEX", "y"),
        ]
        .map(|(k, v)| (k.into(), v.into()))
        .to_vec();
        let kept: Vec<&str> = filter_env(&env, &default_env_allow())
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(kept, ["PATH", "LC_ALL"]);
    }
}
