//! Actions and the default key tables (SPEC §5.1): the prefix table, the root table
//! (`ctrl+p`, optional `alt+h/j/k/l`) and copy mode. `[keys.*]` in the config overrides by
//! action name, which is also what `femux keys list|explain` and the palette show.

use std::collections::BTreeMap;

use femux_plugin::{Binding, BoundKey};
use femux_proto::msg::Direction;

use crate::config::ClientConfig;
use crate::keys::Chord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Focus(Direction),
    Resize(Direction),
    SplitDown,
    SplitRight,
    ClosePane,
    Zoom,
    FloatToggle,
    Equalize,
    Rotate,
    SwapNext,
    NewTab,
    RenameTab,
    NextTab,
    PrevTab,
    Tab(u8),
    SetMark,
    JumpMark,
    CopyMode,
    Paste,
    SessionNav,
    WindowNav,
    Palette,
    SideNav,
    /// The agent inbox (REQ-UI-044); bound by the agents plugin.
    Inbox,
    Detach,
    Help,
    NormalMode,
    SendPrefix,
    Preselect(Direction),
    Scheme(femux_proto::msg::Scheme),
    // Copy mode.
    CopyLeft,
    CopyDown,
    CopyUp,
    CopyRight,
    CopyWordNext,
    CopyWordPrev,
    CopyWordEnd,
    CopyLineStart,
    CopyLineEnd,
    CopyTop,
    CopyBottom,
    CopyHalfUp,
    CopyHalfDown,
    CopySearchDown,
    CopySearchUp,
    CopyNext,
    CopyPrev,
    CopySelectChar,
    CopySelectLine,
    CopySelectBlock,
    CopyYank,
    CopyPromptPrev,
    CopyPromptNext,
    CopyExit,
    CopyClear,
}

fn dir_name(d: Direction) -> &'static str {
    match d {
        Direction::H => "left",
        Direction::J => "down",
        Direction::K => "up",
        Direction::L => "right",
    }
}

fn dir_of(s: &str) -> Option<Direction> {
    Some(match s {
        "left" => Direction::H,
        "down" => Direction::J,
        "up" => Direction::K,
        "right" => Direction::L,
        _ => return None,
    })
}

/// Every action, in the order `femux keys list` prints them.
pub const ALL: &[Action] = &[
    Action::Focus(Direction::H),
    Action::Focus(Direction::J),
    Action::Focus(Direction::K),
    Action::Focus(Direction::L),
    Action::Resize(Direction::H),
    Action::Resize(Direction::J),
    Action::Resize(Direction::K),
    Action::Resize(Direction::L),
    Action::SplitDown,
    Action::SplitRight,
    Action::ClosePane,
    Action::Zoom,
    Action::FloatToggle,
    Action::Equalize,
    Action::Rotate,
    Action::SwapNext,
    Action::Preselect(Direction::H),
    Action::Preselect(Direction::J),
    Action::Preselect(Direction::K),
    Action::Preselect(Direction::L),
    Action::Scheme(femux_proto::msg::Scheme::Bsp),
    Action::Scheme(femux_proto::msg::Scheme::Master),
    Action::Scheme(femux_proto::msg::Scheme::Scroll),
    Action::NewTab,
    Action::RenameTab,
    Action::NextTab,
    Action::PrevTab,
    Action::Tab(1),
    Action::Tab(2),
    Action::Tab(3),
    Action::Tab(4),
    Action::Tab(5),
    Action::Tab(6),
    Action::Tab(7),
    Action::Tab(8),
    Action::Tab(9),
    Action::SetMark,
    Action::JumpMark,
    Action::CopyMode,
    Action::Paste,
    Action::SessionNav,
    Action::WindowNav,
    Action::Palette,
    Action::SideNav,
    Action::Inbox,
    Action::Detach,
    Action::Help,
    Action::NormalMode,
    Action::SendPrefix,
    Action::CopyLeft,
    Action::CopyDown,
    Action::CopyUp,
    Action::CopyRight,
    Action::CopyWordNext,
    Action::CopyWordPrev,
    Action::CopyWordEnd,
    Action::CopyLineStart,
    Action::CopyLineEnd,
    Action::CopyTop,
    Action::CopyBottom,
    Action::CopyHalfUp,
    Action::CopyHalfDown,
    Action::CopySearchDown,
    Action::CopySearchUp,
    Action::CopyNext,
    Action::CopyPrev,
    Action::CopySelectChar,
    Action::CopySelectLine,
    Action::CopySelectBlock,
    Action::CopyYank,
    Action::CopyPromptPrev,
    Action::CopyPromptNext,
    Action::CopyExit,
    Action::CopyClear,
];

impl Action {
    /// The action's name as used in config and `femux keys list`.
    pub fn name(self) -> String {
        use Action::*;
        match self {
            Focus(d) => format!("focus {}", dir_name(d)),
            Resize(d) => format!("resize {}", dir_name(d)),
            Preselect(d) => format!("preselect {}", dir_name(d)),
            Scheme(s) => format!(
                "scheme {}",
                match s {
                    femux_proto::msg::Scheme::Bsp => "bsp",
                    femux_proto::msg::Scheme::Master => "master",
                    femux_proto::msg::Scheme::Scroll => "scroll",
                }
            ),
            Tab(n) => format!("tab {n}"),
            other => other.static_name().to_owned(),
        }
    }

    fn static_name(self) -> &'static str {
        use Action::*;
        match self {
            SplitDown => "split down",
            SplitRight => "split right",
            ClosePane => "close pane",
            Zoom => "zoom",
            FloatToggle => "float",
            Equalize => "equalize",
            Rotate => "rotate",
            SwapNext => "swap",
            NewTab => "new tab",
            RenameTab => "rename tab",
            NextTab => "next tab",
            PrevTab => "previous tab",
            SetMark => "mark",
            JumpMark => "jump",
            CopyMode => "copy",
            Paste => "paste",
            SessionNav => "sessions",
            WindowNav => "windows",
            Palette => "palette",
            SideNav => "sidenav",
            Inbox => "inbox",
            Detach => "detach",
            Help => "help",
            NormalMode => "normal",
            SendPrefix => "prefix",
            CopyLeft => "copy left",
            CopyDown => "copy down",
            CopyUp => "copy up",
            CopyRight => "copy right",
            CopyWordNext => "copy word next",
            CopyWordPrev => "copy word previous",
            CopyWordEnd => "copy word end",
            CopyLineStart => "copy line start",
            CopyLineEnd => "copy line end",
            CopyTop => "copy top",
            CopyBottom => "copy bottom",
            CopyHalfUp => "copy half up",
            CopyHalfDown => "copy half down",
            CopySearchDown => "copy search down",
            CopySearchUp => "copy search up",
            CopyNext => "copy next match",
            CopyPrev => "copy previous match",
            CopySelectChar => "copy select",
            CopySelectLine => "copy select line",
            CopySelectBlock => "copy select block",
            CopyYank => "copy yank",
            CopyPromptPrev => "copy prompt previous",
            CopyPromptNext => "copy prompt next",
            CopyExit => "copy exit",
            CopyClear => "copy clear",
            Focus(_) | Resize(_) | Preselect(_) | Scheme(_) | Tab(_) => unreachable!(),
        }
    }

    pub fn from_name(name: &str) -> Option<Action> {
        let name = name.trim();
        if let Some(d) = name.strip_prefix("focus ").and_then(dir_of) {
            return Some(Action::Focus(d));
        }
        if let Some(d) = name.strip_prefix("resize ").and_then(dir_of) {
            return Some(Action::Resize(d));
        }
        if let Some(d) = name.strip_prefix("preselect ").and_then(dir_of) {
            return Some(Action::Preselect(d));
        }
        if let Some(n) = name.strip_prefix("tab ").and_then(|n| n.parse::<u8>().ok()) {
            return (1..=9).contains(&n).then_some(Action::Tab(n));
        }
        ALL.iter().copied().find(|a| a.name() == name)
    }

    /// What the action does, for help and the palette.
    pub fn describe(self) -> String {
        use Action::*;
        match self {
            Focus(d) => format!("focus the pane to the {}", dir_name(d)),
            Resize(d) => format!("grow the pane {}ward", dir_name(d)),
            Preselect(d) => format!("put the next split on the {}", dir_name(d)),
            Scheme(_) => format!("use the {} layout scheme", &self.name()[7..]),
            Tab(n) => format!("select tab {n}"),
            SplitDown => "split below".into(),
            SplitRight => "split right".into(),
            ClosePane => "close pane (confirms if a program is running)".into(),
            Zoom => "zoom toggle".into(),
            FloatToggle => "float / tile".into(),
            Equalize => "equalize".into(),
            Rotate => "rotate split".into(),
            SwapNext => "swap with next".into(),
            NewTab => "new tab".into(),
            RenameTab => "rename tab".into(),
            NextTab => "next tab".into(),
            PrevTab => "previous tab".into(),
            SetMark => "set mark a-z".into(),
            JumpMark => "jump to mark a-z".into(),
            CopyMode => "copy mode".into(),
            Paste => "paste".into(),
            SessionNav => "session navigator".into(),
            WindowNav => "window navigator".into(),
            Palette => "command palette".into(),
            SideNav => "side nav".into(),
            Inbox => "agent inbox".into(),
            Detach => "detach".into(),
            Help => "help".into(),
            NormalMode => "sticky normal mode".into(),
            SendPrefix => "send the prefix to the pane".into(),
            _ => self.name().trim_start_matches("copy ").to_owned(),
        }
    }

    /// Group shown by which-key.
    pub fn group(self) -> &'static str {
        use Action::*;
        match self {
            Focus(_) | Resize(_) | Zoom | FloatToggle | Rotate | Preselect(_) => "panes",
            SplitDown | SplitRight | ClosePane | Equalize | SwapNext | Scheme(_) => "splits",
            NewTab | RenameTab | NextTab | PrevTab | Tab(_) => "tabs",
            SetMark | JumpMark | CopyMode | Paste | Detach => "session",
            SessionNav | WindowNav | Palette | SideNav | Inbox | Help | NormalMode | SendPrefix => {
                "ui"
            }
            _ => "copy",
        }
    }

    pub fn is_copy(self) -> bool {
        self.group() == "copy"
    }
}

/// Which table a chord is looked up in (the plugin crate's, so plugins can name it).
pub use femux_plugin::Table;

#[derive(Debug, Clone)]
pub struct Bindings {
    pub root: Vec<(Chord, Action)>,
    pub prefix: Vec<(Chord, Action)>,
    pub copy: Vec<(Chord, Action)>,
}

const PREFIX: &[(&str, Action)] = &[
    ("h", Action::Focus(Direction::H)),
    ("j", Action::Focus(Direction::J)),
    ("k", Action::Focus(Direction::K)),
    ("l", Action::Focus(Direction::L)),
    ("H", Action::Resize(Direction::H)),
    ("J", Action::Resize(Direction::J)),
    ("K", Action::Resize(Direction::K)),
    ("L", Action::Resize(Direction::L)),
    ("ctrl+h", Action::Preselect(Direction::H)),
    ("ctrl+j", Action::Preselect(Direction::J)),
    ("ctrl+k", Action::Preselect(Direction::K)),
    ("ctrl+l", Action::Preselect(Direction::L)),
    ("s", Action::SplitDown),
    ("v", Action::SplitRight),
    ("x", Action::ClosePane),
    ("z", Action::Zoom),
    ("f", Action::FloatToggle),
    ("=", Action::Equalize),
    ("r", Action::Rotate),
    ("w", Action::SwapNext),
    ("t", Action::NewTab),
    ("T", Action::RenameTab),
    ("n", Action::NextTab),
    ("p", Action::PrevTab),
    ("1", Action::Tab(1)),
    ("2", Action::Tab(2)),
    ("3", Action::Tab(3)),
    ("4", Action::Tab(4)),
    ("5", Action::Tab(5)),
    ("6", Action::Tab(6)),
    ("7", Action::Tab(7)),
    ("8", Action::Tab(8)),
    ("9", Action::Tab(9)),
    ("m", Action::SetMark),
    ("'", Action::JumpMark),
    ("[", Action::CopyMode),
    ("]", Action::Paste),
    ("S", Action::SessionNav),
    ("W", Action::WindowNav),
    (":", Action::Palette),
    ("b", Action::SideNav),
    ("d", Action::Detach),
    ("?", Action::Help),
    ("escape", Action::NormalMode),
];

const ROOT: &[(&str, Action)] = &[("ctrl+p", Action::Palette)];

const PREFIXLESS: &[(&str, Action)] = &[
    ("alt+h", Action::Focus(Direction::H)),
    ("alt+j", Action::Focus(Direction::J)),
    ("alt+k", Action::Focus(Direction::K)),
    ("alt+l", Action::Focus(Direction::L)),
];

const COPY: &[(&str, Action)] = &[
    ("h", Action::CopyLeft),
    ("j", Action::CopyDown),
    ("k", Action::CopyUp),
    ("l", Action::CopyRight),
    ("left", Action::CopyLeft),
    ("down", Action::CopyDown),
    ("up", Action::CopyUp),
    ("right", Action::CopyRight),
    ("w", Action::CopyWordNext),
    ("b", Action::CopyWordPrev),
    ("e", Action::CopyWordEnd),
    ("0", Action::CopyLineStart),
    ("home", Action::CopyLineStart),
    ("$", Action::CopyLineEnd),
    ("end", Action::CopyLineEnd),
    ("g", Action::CopyTop),
    ("G", Action::CopyBottom),
    ("ctrl+u", Action::CopyHalfUp),
    ("ctrl+d", Action::CopyHalfDown),
    ("pageup", Action::CopyHalfUp),
    ("pagedown", Action::CopyHalfDown),
    ("/", Action::CopySearchDown),
    ("?", Action::CopySearchUp),
    ("n", Action::CopyNext),
    ("N", Action::CopyPrev),
    ("v", Action::CopySelectChar),
    ("V", Action::CopySelectLine),
    ("ctrl+v", Action::CopySelectBlock),
    ("y", Action::CopyYank),
    ("enter", Action::CopyYank),
    ("[", Action::CopyPromptPrev),
    ("]", Action::CopyPromptNext),
    ("q", Action::CopyExit),
    ("escape", Action::CopyClear),
];

fn table(
    defaults: &[(&str, Action)],
    overrides: &BTreeMap<String, String>,
) -> Vec<(Chord, Action)> {
    let mut out: Vec<(Chord, Action)> = defaults
        .iter()
        .map(|(k, a)| (Chord::parse(k).unwrap(), *a))
        .collect();
    for (k, name) in overrides {
        let Some(chord) = Chord::parse(k) else {
            continue;
        };
        out.retain(|(c, _)| *c != chord);
        if let Some(action) = Action::from_name(name) {
            out.push((chord, action));
        }
    }
    out
}

impl Bindings {
    pub fn from_config(cfg: &ClientConfig) -> Bindings {
        let mut root: Vec<(&str, Action)> = ROOT.to_vec();
        if cfg.prefixless_nav {
            root.extend_from_slice(PREFIXLESS);
        }
        Bindings {
            root: table(&root, &cfg.keys_root),
            prefix: table(PREFIX, &cfg.keys_prefix),
            copy: table(COPY, &cfg.keys_copy),
        }
    }

    /// Adds bindings a plugin contributes (REQ-PLUG-002); unknown action names are ignored.
    pub fn add_plugin(&mut self, bindings: &[Binding]) {
        for b in bindings {
            let (Some(chord), Some(action)) = (Chord::parse(&b.key), Action::from_name(&b.action))
            else {
                continue;
            };
            let list = match b.table {
                Table::Root => &mut self.root,
                Table::Prefix => &mut self.prefix,
                Table::Copy => &mut self.copy,
            };
            list.retain(|(c, _)| *c != chord);
            list.push((chord, action));
        }
    }

    pub fn lookup(&self, t: Table, chord: &Chord) -> Option<Action> {
        let list = match t {
            Table::Root => &self.root,
            Table::Prefix => &self.prefix,
            Table::Copy => &self.copy,
        };
        list.iter().find(|(c, _)| c == chord).map(|(_, a)| *a)
    }

    /// Keys bound to `action` in table `t`.
    pub fn keys_for(&self, t: Table, action: Action) -> Vec<String> {
        let list = match t {
            Table::Root => &self.root,
            Table::Prefix => &self.prefix,
            Table::Copy => &self.copy,
        };
        list.iter()
            .filter(|(_, a)| *a == action)
            .map(|(c, _)| c.to_string())
            .collect()
    }

    /// The prefix table as which-key shows it: keys for one action are merged (`h j k l`).
    pub fn which_key_table(&self) -> Vec<BoundKey> {
        let mut out: Vec<BoundKey> = Vec::new();
        for (chord, action) in &self.prefix {
            let desc = action.describe();
            let key = chord.to_string();
            // Merge runs of the same purpose (focus/resize/preselect/tab n).
            let merge_as = match action {
                Action::Focus(_) => Some(("h j k l", "focus", action.group())),
                Action::Resize(_) => Some(("H J K L", "resize", action.group())),
                Action::Preselect(_) => Some(("ctrl+h j k l", "preselect split", action.group())),
                Action::Tab(_) => Some(("1-9", "nth tab", action.group())),
                _ => None,
            };
            if let Some((k, a, g)) = merge_as {
                if !out.iter().any(|b| b.key == k) {
                    out.push(BoundKey {
                        key: k.into(),
                        action: a.into(),
                        group: g.into(),
                    });
                }
                continue;
            }
            out.push(BoundKey {
                key,
                action: desc,
                group: action.group().into(),
            });
        }
        out
    }

    /// `femux keys list`: `(table, key, action name, description)` for every binding.
    pub fn list(&self) -> Vec<(&'static str, String, String, String)> {
        let mut out = Vec::new();
        for (name, list) in [
            ("root", &self.root),
            ("prefix", &self.prefix),
            ("copy", &self.copy),
        ] {
            for (c, a) in list {
                out.push((name, c.to_string(), a.name(), a.describe()));
            }
        }
        out
    }

    /// `femux keys explain <key>`: what `key` does in each table.
    pub fn explain(&self, key: &str) -> Vec<(&'static str, String, String)> {
        let Some(chord) = Chord::parse(key) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (name, t) in [
            ("root", Table::Root),
            ("prefix", Table::Prefix),
            ("copy", Table::Copy),
        ] {
            if let Some(a) = self.lookup(t, &chord) {
                out.push((name, a.name(), a.describe()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        for a in ALL {
            assert_eq!(Action::from_name(&a.name()), Some(*a), "{}", a.name());
        }
        assert_eq!(Action::from_name("tab 10"), None);
    }

    #[test]
    fn every_default_binding_of_spec_5_1_exists() {
        let b = Bindings::from_config(&ClientConfig::default());
        let p = |k: &str| b.lookup(Table::Prefix, &Chord::parse(k).unwrap());
        assert_eq!(p("h"), Some(Action::Focus(Direction::H)));
        assert_eq!(p("L"), Some(Action::Resize(Direction::L)));
        assert_eq!(p("s"), Some(Action::SplitDown));
        assert_eq!(p("v"), Some(Action::SplitRight));
        assert_eq!(p("x"), Some(Action::ClosePane));
        assert_eq!(p("z"), Some(Action::Zoom));
        assert_eq!(p("f"), Some(Action::FloatToggle));
        assert_eq!(p("="), Some(Action::Equalize));
        assert_eq!(p("r"), Some(Action::Rotate));
        assert_eq!(p("w"), Some(Action::SwapNext));
        assert_eq!(p("t"), Some(Action::NewTab));
        assert_eq!(p("T"), Some(Action::RenameTab));
        assert_eq!(p("n"), Some(Action::NextTab));
        assert_eq!(p("p"), Some(Action::PrevTab));
        assert_eq!(p("9"), Some(Action::Tab(9)));
        assert_eq!(p("m"), Some(Action::SetMark));
        assert_eq!(p("'"), Some(Action::JumpMark));
        assert_eq!(p("["), Some(Action::CopyMode));
        assert_eq!(p("]"), Some(Action::Paste));
        assert_eq!(p("S"), Some(Action::SessionNav));
        assert_eq!(p("W"), Some(Action::WindowNav));
        assert_eq!(p(":"), Some(Action::Palette));
        assert_eq!(p("b"), Some(Action::SideNav));
        assert_eq!(p("d"), Some(Action::Detach));
        assert_eq!(p("?"), Some(Action::Help));
        assert_eq!(p("esc"), Some(Action::NormalMode));
        assert_eq!(
            b.lookup(Table::Root, &Chord::parse("ctrl+p").unwrap()),
            Some(Action::Palette)
        );
        // REQ-UI-004: alt+h/j/k/l exist but are off by default.
        assert_eq!(b.lookup(Table::Root, &Chord::parse("alt+h").unwrap()), None);
        let on = Bindings::from_config(&ClientConfig {
            prefixless_nav: true,
            ..Default::default()
        });
        assert_eq!(
            on.lookup(Table::Root, &Chord::parse("alt+h").unwrap()),
            Some(Action::Focus(Direction::H))
        );
    }

    #[test]
    fn plugin_bindings_join_the_tables() {
        let mut b = Bindings::from_config(&ClientConfig::default());
        b.add_plugin(&[
            Binding {
                table: Table::Prefix,
                key: "g".into(),
                action: "zoom".into(),
            },
            Binding {
                table: Table::Root,
                key: "ctrl+g".into(),
                action: "no such action".into(),
            },
        ]);
        assert_eq!(
            b.lookup(Table::Prefix, &Chord::parse("g").unwrap()),
            Some(Action::Zoom)
        );
        assert_eq!(
            b.lookup(Table::Root, &Chord::parse("ctrl+g").unwrap()),
            None
        );
    }

    #[test]
    fn config_overrides_by_action_name() {
        let mut cfg = ClientConfig::default();
        cfg.keys_prefix.insert("|".into(), "split right".into());
        cfg.keys_prefix.insert("v".into(), "zoom".into());
        let b = Bindings::from_config(&cfg);
        let p = |k: &str| b.lookup(Table::Prefix, &Chord::parse(k).unwrap());
        assert_eq!(p("|"), Some(Action::SplitRight));
        assert_eq!(p("v"), Some(Action::Zoom));
        assert_eq!(b.explain("v")[0].1, "zoom");
        assert!(
            b.list()
                .iter()
                .any(|(t, k, a, _)| *t == "copy" && k == "y" && a == "copy yank")
        );
    }
}
