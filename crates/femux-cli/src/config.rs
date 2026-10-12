//! `~/.config/femux/config.toml` (SPEC §10): the daemon's and the client's settings. Parsed
//! with line-numbered errors; a missing file means defaults. Unknown keys are ignored until
//! REQ-CFG-003 (0.3.0) turns them into errors.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use femux_client::config::{Borders, ClientConfig, TabsPosition};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Daemon {
    pub name: String,
    pub keep_alive: bool,
}

impl Default for Daemon {
    fn default() -> Self {
        Daemon {
            name: "default".into(),
            keep_alive: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Input {
    pub prefix: Vec<String>,
    pub mode_keys: String,
    pub prefixless_nav: bool,
    pub mouse: bool,
}

impl Default for Input {
    fn default() -> Self {
        Input {
            prefix: vec!["ctrl+space".into()],
            mode_keys: "vi".into(),
            prefixless_nav: false,
            mouse: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub which_key_delay_ms: u64,
}

impl Default for Ui {
    fn default() -> Self {
        Ui {
            which_key_delay_ms: 400,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub scheme: String,
    pub gaps: u16,
    pub borders: String,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            scheme: "bsp".into(),
            gaps: 0,
            borders: "single".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Tabs {
    pub position: String,
    pub running_indicator: String,
}

impl Default for Tabs {
    fn default() -> Self {
        Tabs {
            position: "top".into(),
            running_indicator: "●".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Terminal {
    pub scrollback_lines: usize,
    pub kitty_graphics: bool,
    pub sixel: bool,
    pub passthrough: bool,
}

impl Default for Terminal {
    fn default() -> Self {
        Terminal {
            scrollback_lines: 10_000,
            kitty_graphics: true,
            sixel: true,
            passthrough: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub name: String,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            name: "terminal".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Plugins {
    pub enabled: Vec<String>,
}

impl Default for Plugins {
    fn default() -> Self {
        Plugins {
            enabled: [
                "agents",
                "infobar",
                "notify",
                "clipboard",
                "shell-integration",
                "which-key",
                "launcher",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub root: BTreeMap<String, String>,
    pub prefix: BTreeMap<String, String>,
    pub copy: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub daemon: Daemon,
    pub input: Input,
    pub ui: Ui,
    pub layout: Layout,
    pub tabs: Tabs,
    pub terminal: Terminal,
    pub theme: Theme,
    pub plugins: Plugins,
    pub keys: Keys,
}

/// `$XDG_CONFIG_HOME/femux` or `~/.config/femux`.
pub fn config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return PathBuf::from(x).join("femux");
    }
    home().join(".config/femux")
}

/// `$XDG_STATE_HOME/femux` (macOS: `~/Library/Application Support/femux`).
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

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

impl Config {
    pub fn path() -> PathBuf {
        if let Some(p) = std::env::var_os("FEMUX_CONFIG") {
            return PathBuf::from(p);
        }
        config_dir().join("config.toml")
    }

    /// Loads the config at `path` (defaults when it does not exist).
    pub fn load(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Parses TOML; errors name the line.
    pub fn parse(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| match e.span() {
            Some(span) => {
                let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
                format!("line {line}: {}", e.message())
            }
            None => e.message().to_owned(),
        })
    }

    pub fn client(&self) -> ClientConfig {
        ClientConfig {
            prefix: self.input.prefix.clone(),
            prefixless_nav: self.input.prefixless_nav,
            mouse: self.input.mouse,
            which_key_delay: Duration::from_millis(self.ui.which_key_delay_ms),
            borders: match self.layout.borders.as_str() {
                "heavy" => Borders::Heavy,
                "double" => Borders::Double,
                "spaces" => Borders::Spaces,
                _ => Borders::Single,
            },
            tabs: match self.tabs.position.as_str() {
                "bottom" => TabsPosition::Bottom,
                "hidden" => TabsPosition::Hidden,
                _ => TabsPosition::Top,
            },
            running_indicator: self.tabs.running_indicator.clone(),
            theme: self.theme.name.clone(),
            keys_prefix: self.keys.prefix.clone(),
            keys_root: self.keys.root.clone(),
            keys_copy: self.keys.copy.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_to_the_defaults() {
        let text = include_str!("../../../spec/config.example.toml");
        let c = Config::parse(text).unwrap();
        let d = Config::default();
        assert_eq!(c.input.prefix, d.input.prefix);
        assert_eq!(c.plugins.enabled, d.plugins.enabled);
        assert_eq!(c.ui.which_key_delay_ms, 400);
        assert_eq!(
            c.keys.prefix.get("s").map(String::as_str),
            Some("split down")
        );
        let cc = c.client();
        assert_eq!(cc.which_key_delay, Duration::from_millis(400));
        assert!(cc.mouse);
    }

    #[test]
    fn errors_name_the_line() {
        let err = Config::parse("[input]\nprefix = 3\n").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
        assert!(Config::parse("").is_ok());
    }
}
