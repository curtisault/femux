//! Client configuration (SPEC §10, the client's share). Parsed from `config.toml` by the
//! `femux` binary; tests build it directly.

use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Borders {
    #[default]
    Single,
    Heavy,
    Double,
    Spaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabsPosition {
    #[default]
    Top,
    Bottom,
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// `[input] prefix`: chords such as `ctrl+space` (REQ-UI-002).
    pub prefix: Vec<String>,
    /// `[input] prefixless_nav`: alt+h/j/k/l focus keys (REQ-UI-004).
    pub prefixless_nav: bool,
    /// `[input] mouse` (REQ-UI-053).
    pub mouse: bool,
    /// `[ui] which_key_delay_ms` (REQ-UI-003).
    pub which_key_delay: Duration,
    /// `[layout] borders`.
    pub borders: Borders,
    /// `[tabs] position`.
    pub tabs: TabsPosition,
    /// `[tabs] running_indicator` (REQ-UI-020).
    pub running_indicator: String,
    /// `[theme] name`: `terminal`, `dark` or `light` (REQ-UI-051).
    pub theme: String,
    /// `[keys.prefix]`: key -> action name overrides.
    pub keys_prefix: BTreeMap<String, String>,
    /// `[keys.root]`: prefix-less bindings.
    pub keys_root: BTreeMap<String, String>,
    /// `[keys.copy]`: copy-mode bindings.
    pub keys_copy: BTreeMap<String, String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            prefix: vec!["ctrl+space".into()],
            prefixless_nav: false,
            mouse: true,
            which_key_delay: Duration::from_millis(400),
            borders: Borders::Single,
            tabs: TabsPosition::Top,
            running_indicator: "●".into(),
            theme: "terminal".into(),
            keys_prefix: BTreeMap::new(),
            keys_root: BTreeMap::new(),
            keys_copy: BTreeMap::new(),
        }
    }
}
