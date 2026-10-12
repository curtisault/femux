//! Notification actions on the client's machine (REQ-NOTIFY-001, REQ-NOTIFY-003): the terminal
//! bell, playing a sound file, and desktop notifications through the host terminal (OSC 777 or
//! OSC 99) or the system's notifier.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The bell.
pub const BELL: &str = "\x07";

/// How a host terminal shows desktop notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    /// `OSC 99` (kitty's notification protocol).
    Osc99,
    /// `OSC 777;notify` (foot, ghostty, wezterm, contour, rxvt-unicode).
    Osc777,
    /// The system's notifier: `osascript` on macOS, `notify-send` elsewhere.
    Native,
}

/// Picks the method from what the terminal says it is (`TERM_PROGRAM`, then `TERM`).
pub fn desktop_for(term_program: Option<&str>, term: Option<&str>) -> Desktop {
    let name = format!(
        "{} {}",
        term_program.unwrap_or_default(),
        term.unwrap_or_default()
    )
    .to_lowercase();
    if name.contains("kitty") {
        Desktop::Osc99
    } else if ["ghostty", "wezterm", "foot", "contour", "rxvt"]
        .iter()
        .any(|t| name.contains(t))
    {
        Desktop::Osc777
    } else {
        Desktop::Native
    }
}

fn clean(s: &str) -> String {
    // Titles are untrusted (REQ-SEC-003): no control characters, no field separators.
    s.chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == ';' { ',' } else { c })
        .collect()
}

/// The escape sequence for a terminal-drawn notification; `None` for [`Desktop::Native`].
pub fn desktop_sequence(how: Desktop, title: &str, body: &str) -> Option<String> {
    let (t, b) = (clean(title), clean(body));
    match how {
        Desktop::Osc777 => Some(format!("\x1b]777;notify;{t};{b}\x1b\\")),
        Desktop::Osc99 => Some(format!(
            "\x1b]99;i=femux:d=0:p=title;{t}\x1b\\\x1b]99;i=femux:d=1:p=body;{b}\x1b\\"
        )),
        Desktop::Native => None,
    }
}

/// The first of `names` found on `path` (a `PATH`-style list).
pub fn which(names: &[&str], path: &str) -> Option<PathBuf> {
    names.iter().find_map(|n| {
        std::env::split_paths(path)
            .map(|d| d.join(n))
            .find(|p| p.is_file())
    })
}

/// The command that shows a native notification, if a notifier is installed.
pub fn native_command(title: &str, body: &str, path: &str) -> Option<Command> {
    if cfg!(target_os = "macos") {
        let script = format!(
            "display notification {:?} with title {:?}",
            clean(body),
            clean(title)
        );
        let mut c = Command::new(which(&["osascript"], path)?);
        c.args(["-e", &script]);
        Some(c)
    } else {
        let mut c = Command::new(which(&["notify-send"], path)?);
        c.args(["--app-name=femux", &clean(title), &clean(body)]);
        Some(c)
    }
}

/// The command that plays `file`: `afplay` on macOS, else the first of `paplay`, `pw-play`
/// and `aplay` installed.
pub fn sound_command(file: &Path, path: &str) -> Option<Command> {
    let players: &[&str] = if cfg!(target_os = "macos") {
        &["afplay"]
    } else {
        &["paplay", "pw-play", "aplay"]
    };
    let mut c = Command::new(which(players, path)?);
    if c.get_program().to_string_lossy().ends_with("aplay") {
        c.arg("-q");
    }
    c.arg(file);
    Some(c)
}

/// Starts `cmd` in the background with no terminal I/O.
pub fn spawn_quietly(mut cmd: Command) {
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_001_desktop_follows_the_terminal() {
        assert_eq!(desktop_for(Some("kitty"), None), Desktop::Osc99);
        assert_eq!(desktop_for(None, Some("xterm-kitty")), Desktop::Osc99);
        assert_eq!(desktop_for(Some("ghostty"), None), Desktop::Osc777);
        assert_eq!(desktop_for(Some("WezTerm"), None), Desktop::Osc777);
        assert_eq!(desktop_for(Some("Apple_Terminal"), None), Desktop::Native);
        assert_eq!(
            desktop_sequence(Desktop::Osc777, "claude done", "api · 2").as_deref(),
            Some("\x1b]777;notify;claude done;api · 2\x1b\\")
        );
        let k = desktop_sequence(Desktop::Osc99, "a;b\x07", "c").unwrap();
        assert!(
            k.contains("p=title;a,b\x1b\\") && k.contains("p=body;c"),
            "{k:?}"
        );
        assert_eq!(desktop_sequence(Desktop::Native, "a", "b"), None);
    }

    #[test]
    fn notify_001_players_and_notifiers_come_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        assert!(sound_command(Path::new("/x.wav"), &path).is_none());
        let player = if cfg!(target_os = "macos") {
            "afplay"
        } else {
            "pw-play"
        };
        std::fs::write(dir.path().join(player), "").unwrap();
        let c = sound_command(Path::new("/x.wav"), &path).unwrap();
        assert!(c.get_program().to_string_lossy().ends_with(player));
        assert_eq!(c.get_args().last().unwrap(), "/x.wav");
        let notifier = if cfg!(target_os = "macos") {
            "osascript"
        } else {
            "notify-send"
        };
        assert!(native_command("t", "b", &path).is_none());
        std::fs::write(dir.path().join(notifier), "").unwrap();
        assert!(native_command("t", "b", &path).is_some());
    }
}
