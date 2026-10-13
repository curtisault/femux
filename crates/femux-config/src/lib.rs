//! femux configuration (SPEC §10): `~/.config/femux/config.toml` and session files under
//! `~/.config/femux/sessions/`. Every table rejects unknown keys (REQ-CFG-003) and every error
//! names its line. A missing config file means the defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------ paths

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

/// `$XDG_CONFIG_HOME/femux`, else `~/.config/femux`.
pub fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        Some(x) => PathBuf::from(x).join("femux"),
        None => home().join(".config/femux"),
    }
}

/// `$XDG_STATE_HOME/femux`, else `~/.local/state/femux`; macOS: `~/Library/Application
/// Support/femux` (REQ-STORE-001).
pub fn state_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME").filter(|x| !x.is_empty()) {
        return PathBuf::from(x).join("femux");
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/femux")
    } else {
        home().join(".local/state/femux")
    }
}

/// The store of server `name` (REQ-STORE-001).
pub fn db_path(server: &str) -> PathBuf {
    state_dir().join(format!("{server}.db"))
}

/// `$FEMUX_CONFIG`, else `config.toml` in [`config_dir`].
pub fn config_path() -> PathBuf {
    match std::env::var_os("FEMUX_CONFIG").filter(|x| !x.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => config_dir().join("config.toml"),
    }
}

pub fn sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}

/// Expands a leading `~` or `~/`.
pub fn expand(path: &str) -> PathBuf {
    if path == "~" {
        return home();
    }
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

// ------------------------------------------------------------------ errors

/// TOML text to `T`, with errors as `line N: message`.
fn parse<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, String> {
    toml::from_str(text).map_err(|e| match e.span() {
        Some(span) => {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            format!("line {line}: {}", e.message().trim())
        }
        None => e.message().trim().to_owned(),
    })
}

fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

// ------------------------------------------------------------------ config.toml

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    #[default]
    Bsp,
    Master,
    Scroll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Borders {
    #[default]
    Single,
    Heavy,
    Double,
    Spaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TabsPosition {
    #[default]
    Top,
    Bottom,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BarPosition {
    Top,
    #[default]
    Bottom,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeKeys {
    #[default]
    Vi,
    Emacs,
}

macro_rules! section {
    ($(#[$m:meta])* $name:ident { $($field:ident : $ty:ty = $default:expr),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            $(pub $field: $ty,)*
        }

        impl Default for $name {
            fn default() -> Self {
                $name { $($field: $default,)* }
            }
        }
    };
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

section!(Daemon {
    name: String = "default".into(),
    keep_alive: bool = false,
});

section!(Input {
    prefix: Vec<String> = strings(&["ctrl+space"]),
    mode_keys: ModeKeys = ModeKeys::Vi,
    prefixless_nav: bool = false,
    mouse: bool = true,
});

section!(Ui {
    which_key_delay_ms: u64 = 400,
});

section!(Layout {
    scheme: Scheme = Scheme::Bsp,
    gaps: u16 = 0,
    borders: Borders = Borders::Single,
});

section!(Tabs {
    position: TabsPosition = TabsPosition::Top,
    running_indicator: String = "●".into(),
});

section!(Infobar {
    position: BarPosition = BarPosition::Bottom,
    left: Vec<String> = strings(&["session", "tabs"]),
    right: Vec<String> = strings(&["agents", "git", "cpu", "mem", "net", "clock"]),
    interval_ms: u64 = 1000,
});

section!(Terminal {
    scrollback_lines: usize = 10_000,
    kitty_graphics: bool = true,
    sixel: bool = true,
    passthrough: bool = false,
});

section!(Restore {
    enabled: bool = false,
    scrollback: bool = false,
    scrollback_lines: usize = 2000,
    stagger_ms: u64 = 100,
});

section!(Env {
    allow: Vec<String> = strings(&["PATH", "HOME", "SHELL", "LANG", "LC_*", "TERM", "EDITOR"]),
});

section!(Clipboard {
    read_allow: Vec<String> = Vec::new(),
});

section!(Theme {
    name: String = "terminal".into(),
});

section!(Agents {
    enabled: bool = true,
    blocked_after_ms: u64 = 1500,
    hold_ms: u64 = 5000,
});

section!(Plugins {
    enabled: Vec<String> = strings(&[
        "agents",
        "infobar",
        "notify",
        "clipboard",
        "shell-integration",
        "which-key",
        "launcher",
        "hosts",
    ]),
});

section!(Keys {
    root: BTreeMap<String, String> = BTreeMap::new(),
    prefix: BTreeMap<String, String> = BTreeMap::new(),
    copy: BTreeMap<String, String> = BTreeMap::new(),
});

/// One `[notify]` entry (REQ-NOTIFY-001); actions arrive with the `notify` plugin (0.5.0).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifyAction {
    pub sound: Option<String>,
    pub desktop: Option<bool>,
    pub only_when_hidden: Option<bool>,
    pub webhook: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub daemon: Daemon,
    pub input: Input,
    pub ui: Ui,
    pub layout: Layout,
    pub tabs: Tabs,
    pub infobar: Infobar,
    pub terminal: Terminal,
    pub restore: Restore,
    pub env: Env,
    pub clipboard: Clipboard,
    pub theme: Theme,
    pub agents: Agents,
    pub notify: BTreeMap<String, NotifyAction>,
    pub plugins: Plugins,
    pub keys: Keys,
}

impl Default for Config {
    fn default() -> Self {
        let notify = [(
            "agent_done".to_owned(),
            NotifyAction {
                sound: Some("bell".into()),
                desktop: Some(true),
                only_when_hidden: Some(true),
                webhook: None,
            },
        )]
        .into();
        Config {
            daemon: Daemon::default(),
            input: Input::default(),
            ui: Ui::default(),
            layout: Layout::default(),
            tabs: Tabs::default(),
            infobar: Infobar::default(),
            terminal: Terminal::default(),
            restore: Restore::default(),
            env: Env::default(),
            clipboard: Clipboard::default(),
            theme: Theme::default(),
            agents: Agents::default(),
            notify,
            plugins: Plugins::default(),
            keys: Keys::default(),
        }
    }
}

impl Config {
    /// Parses config TOML; unknown keys are errors (REQ-CFG-003).
    pub fn parse(text: &str) -> Result<Config, String> {
        parse(text)
    }

    /// Loads `path`; defaults when it does not exist. Errors read `<path>: line N: ...`.
    pub fn load(path: &Path) -> Result<Config, String> {
        match read(path)? {
            Some(text) => Config::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            None => Ok(Config::default()),
        }
    }
}

// ------------------------------------------------------------------ session files

/// One `[[tab]]` of a session file.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TabSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Re-run `command` when the session is restored (REQ-STORE-004).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub restore: bool,
}

/// A session file (SPEC §10).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Total tabs; those beyond the `[[tab]]` entries are plain shells in `root`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs: Option<u32>,
    #[serde(rename = "tab", skip_serializing_if = "Vec::is_empty")]
    pub tab: Vec<TabSpec>,
}

impl SessionFile {
    pub fn parse(text: &str) -> Result<SessionFile, String> {
        parse(text)
    }

    pub fn load(path: &Path) -> Result<SessionFile, String> {
        let text = read(path)?.ok_or_else(|| format!("{}: no such file", path.display()))?;
        SessionFile::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("session files serialize")
    }

    /// Every tab to open, in order: the listed ones, then plain shells up to `tabs`
    /// (REQ-CFG-001). At least one.
    pub fn all_tabs(&self) -> Vec<TabSpec> {
        let mut out = self.tab.clone();
        let total = (self.tabs.unwrap_or(0) as usize).max(out.len()).max(1);
        out.resize(total, TabSpec::default());
        out
    }
}

// ------------------------------------------------------------------ hosts

/// One saved remote (`femux hosts add`, SPEC §11): how to reach it over OpenSSH.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostEntry {
    /// The `ssh` destination: `host`, `user@host` or an alias from `~/.ssh/config`.
    pub ssh: String,
    /// The `femux` command on the remote (default `femux`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub femux: Option<String>,
    /// Reach the remote daemon through a socket forwarded with `ssh -L` instead of
    /// `femux attach --stdio` (REQ-PROTO-003).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub forward: bool,
}

/// `hosts.toml` next to `config.toml`: `[host.<name>]` tables.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostsFile {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub host: BTreeMap<String, HostEntry>,
}

/// `hosts.toml` in the directory of [`config_path`].
pub fn hosts_path() -> PathBuf {
    config_path()
        .parent()
        .map(|d| d.join("hosts.toml"))
        .unwrap_or_else(|| config_dir().join("hosts.toml"))
}

impl HostsFile {
    pub fn parse(text: &str) -> Result<HostsFile, String> {
        parse(text)
    }

    /// Loads `path`; empty when it does not exist.
    pub fn load(path: &Path) -> Result<HostsFile, String> {
        match read(path)? {
            Some(text) => HostsFile::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            None => Ok(HostsFile::default()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// What `attach --host <arg>` means: a saved host, else `arg` as a raw ssh destination.
    pub fn resolve(&self, arg: &str) -> (String, HostEntry) {
        match self.host.get(arg) {
            Some(e) => (arg.to_owned(), e.clone()),
            None => (
                arg.to_owned(),
                HostEntry {
                    ssh: arg.to_owned(),
                    ..Default::default()
                },
            ),
        }
    }
}

/// Where `launch <arg>` finds its file: an existing path, else `sessions/<arg>.toml`.
pub fn session_file_path(arg: &str) -> PathBuf {
    let p = expand(arg);
    if arg.contains('/') || arg.ends_with(".toml") || p.is_file() {
        return p;
    }
    sessions_dir().join(format!("{arg}.toml"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn hosts_file_roundtrips_and_resolves_raw_destinations() {
        let text = "[host.box]\nssh = \"me@build-box\"\nforward = true\n\n[host.pi]\nssh = \"pi\"\nfemux = \"~/bin/femux\"\n";
        let h = HostsFile::parse(text).unwrap();
        assert!(h.host["box"].forward);
        assert_eq!(h.host["pi"].femux.as_deref(), Some("~/bin/femux"));
        assert_eq!(HostsFile::parse(&toml::to_string(&h).unwrap()).unwrap(), h);
        assert_eq!(h.resolve("box").1.ssh, "me@build-box");
        let (label, raw) = h.resolve("other@host");
        assert_eq!(
            (label.as_str(), raw.ssh.as_str(), raw.forward),
            ("other@host", "other@host", false)
        );
        assert!(HostsFile::parse("[host.x]\nssh = \"a\"\nport = 2\n").is_err());
    }

    use super::*;

    #[test]
    fn example_config_is_exactly_the_defaults() {
        let text = include_str!("../../../spec/config.example.toml");
        let mut c = Config::parse(text).unwrap();
        // The example's [keys.prefix] shows overrides; the defaults have none.
        assert_eq!(
            c.keys.prefix.get("s").map(String::as_str),
            Some("split down")
        );
        c.keys = Keys::default();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn cfg_003_unknown_keys_are_errors_with_lines() {
        let err = Config::parse("[input]\nprefix = [\"ctrl+a\"]\nprefx = 1\n").unwrap_err();
        assert!(err.starts_with("line 3:") && err.contains("prefx"), "{err}");
        let err = Config::parse("[daemon]\nname = \"x\"\n\n[bogus]\na = 1\n").unwrap_err();
        assert!(err.starts_with("line 4:") && err.contains("bogus"), "{err}");
        let err = Config::parse("[layout]\nscheme = \"spiral\"\n").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
        let err = Config::parse("[notify]\nagent_done = { chime = \"x\" }\n").unwrap_err();
        assert!(err.contains("chime"), "{err}");
        let err = SessionFile::parse("name = \"w\"\n[[tab]]\ncmd = \"vim\"\n").unwrap_err();
        assert!(err.starts_with("line 3:") && err.contains("cmd"), "{err}");
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn session_files_list_tabs_then_plain_shells() {
        let s = SessionFile::parse(
            "name = \"writing\"\nroot = \"~/writing\"\ntabs = 4\n\n[[tab]]\ntitle = \"editor\"\ncommand = \"nvim\"\nrestore = true\n\n[[tab]]\ncommand = \"git status\"\ncwd = \"~/writing/notes\"\n",
        )
        .unwrap();
        let tabs = s.all_tabs();
        assert_eq!(tabs.len(), 4);
        assert_eq!(tabs[0].command.as_deref(), Some("nvim"));
        assert!(tabs[0].restore && !tabs[1].restore);
        assert_eq!(tabs[2], TabSpec::default());
        assert_eq!(SessionFile::default().all_tabs().len(), 1);
        // Round trip through to_toml.
        assert_eq!(SessionFile::parse(&s.to_toml()).unwrap(), s);
    }

    #[test]
    fn tilde_expands() {
        assert_eq!(expand("~/x"), home().join("x"));
        assert_eq!(expand("/abs"), PathBuf::from("/abs"));
    }
}
