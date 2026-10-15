//! The TUI client's state machine: modes (REQ-UI-001), the prefix table (REQ-UI-002),
//! which-key timing (REQ-UI-003), every default binding of §5.1, copy mode, the command
//! palette (REQ-UI-043) and mouse handling (REQ-UI-011, REQ-UI-053). It is pure: input comes
//! in as protocol events, requests go out through [`App::take_out`], host-terminal side
//! effects through [`App::take_effects`]. Rendering lives in [`crate::render`].

use std::collections::HashMap;
use std::time::Instant;

use femux_plugin::{ClientPlugin, HostEffect, Overlay as PluginOverlay, OverlayCtx};
use femux_proto::msg::{self as m, Direction};
use femux_proto::{ErrorBody, Event, Push, Request, Response, ServerMsg};

use crate::bindings::{Action, Bindings, Table};
use crate::config::ClientConfig;
use crate::copy::{CopyMode, Need, Search};
use crate::keys::Chord;
use crate::model::{Effect as ModelEffect, Model};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Terminal,
    Normal,
    Copy,
}

/// What the host terminal must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Exit,
    /// Put text on the host clipboard (OSC 52).
    Clipboard(String),
    /// Write a pane's DCS sequence to the host terminal as is (REQ-SEC-004).
    Passthrough(Vec<u8>),
    /// The daemon reloaded its config; re-read ours ([`App::apply_config`]).
    Reload,
    /// Notification actions (REQ-NOTIFY-001).
    Bell,
    Sound(String),
    Desktop {
        title: String,
        body: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteRun {
    Action(Action),
    /// Types `femux <cmd> ` into the focused pane.
    Cli(String),
    /// A plugin's palette action.
    Plugin(usize, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    pub label: String,
    pub hint: String,
    pub run: PaletteRun,
}

#[derive(Debug, Clone, Default)]
pub struct Palette {
    pub input: String,
    pub selected: usize,
    pub items: Vec<PaletteItem>,
}

impl Palette {
    /// Items matching the input, best first (REQ-UI-043: fuzzy).
    pub fn matches(&self) -> Vec<&PaletteItem> {
        let mut scored: Vec<(u32, &PaletteItem)> = self
            .items
            .iter()
            .filter_map(|i| fuzzy(&self.input, &i.label).map(|s| (s, i)))
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.label.cmp(&b.1.label)));
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

/// Subsequence match; lower is better. `None` if `query` is not a subsequence of `text`.
pub fn fuzzy(query: &str, text: &str) -> Option<u32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0u32;
    let mut pos = 0usize;
    for q in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = text[pos..].iter().position(|c| *c == q)?;
        score += found as u32;
        pos += found + 1;
    }
    Some(score + text.len() as u32 / 8)
}

/// Something the side nav or a navigator can switch to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavTarget {
    Session(String),
    Tab {
        session: String,
        tab: String,
    },
    Pane {
        session: String,
        tab: String,
        pane: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    ClosePane(String),
    KillSession(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    RenameTab(String),
    NewSession,
    /// `prefix S`: create a session rooted at `root`, then switch to it.
    StartSession {
        root: Option<String>,
    },
    RenameSession(String),
}

#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    /// The which-key panel (from the plugin) once the prefix has been held long enough.
    WhichKey,
    /// Help (REQ-UI-045): scroll position and filter.
    Help(crate::help::HelpState),
    Palette(Palette),
    Confirm {
        text: String,
        what: Confirm,
    },
    Prompt {
        title: String,
        input: String,
        what: PromptKind,
    },
    /// The session or pane navigator (REQ-UI-040/041).
    Navigator(crate::nav::NavState),
    /// The agent inbox (REQ-UI-044).
    Inbox(crate::inbox::InboxState),
    /// The marks overlay (REQ-SESS-004): the selected row.
    Marks(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Second {
    Mark,
    Jump,
}

#[derive(Debug)]
enum Pending {
    Info,
    NewTab,
    /// The navigators' directory (REQ-UI-040..042).
    DirList,
    DirTabs(String),
    DirPane(String),
    Preview(String),
    /// The last screen line of an inbox pane.
    InboxLine(String),
    /// The last line of an agent's pane, for the panel's `agents` section.
    PanelLine(String),
    /// A chunk of a pane's image (pane, image id).
    ImageRead(String, u32),
    /// The pane `prefix F` split off, to float.
    ScratchFloat,
    /// A session `prefix S` created; switch to it.
    StartSession,
    /// Attaching to another session; then focus a pane, then kill the old session.
    Switch {
        focus: Option<String>,
        then_kill: Option<String>,
    },
    Read {
        from: i64,
    },
    Search {
        jump: bool,
    },
    Marks,
    Detach,
    Quiet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press(m::MouseButton),
    Release,
    Move,
    WheelUp,
    WheelDown,
}

/// A mouse selection in one pane's screen (REQ-UI-053), in that pane's cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub pane: String,
    /// Where the button went down and where the pointer is (or was at release).
    pub anchor: (u16, u16),
    pub head: (u16, u16),
    /// Released: copied and kept highlighted until the next key or click.
    pub done: bool,
}

impl Selection {
    /// Start and end in reading order.
    fn span(&self) -> ((u16, u16), (u16, u16)) {
        let key = |p: (u16, u16)| (p.1, p.0);
        if key(self.anchor) <= key(self.head) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Whether cell (`x`, `y`) of the pane is selected: linear from start to end inclusive.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        let (a, b) = self.span();
        (y > a.1 || (y == a.1 && x >= a.0)) && (y < b.1 || (y == b.1 && x <= b.0))
    }

    /// The selected text of `screen`: trailing blanks trimmed per line, lines joined with
    /// newlines.
    pub fn text(&self, screen: &crate::model::PaneScreen) -> String {
        let (a, b) = self.span();
        let mut lines = Vec::new();
        for y in a.1..=b.1 {
            let Some(row) = screen.rows.get(y as usize) else {
                break;
            };
            let from = if y == a.1 { a.0 as usize } else { 0 };
            let to = if y == b.1 {
                b.0 as usize + 1
            } else {
                row.len()
            };
            let mut line = String::new();
            for cell in row.iter().take(to.min(row.len())).skip(from) {
                match cell {
                    Some(c) if !c.ch.is_empty() => line.push_str(&c.ch),
                    // The second half of a wide character.
                    None => {}
                    Some(_) => line.push(' '),
                }
            }
            lines.push(line.trim_end().to_owned());
        }
        lines.join("\n")
    }
}

#[derive(Debug, Clone)]
struct Drag {
    pane: String,
    resize: bool,
    from: (u16, u16),
    rect: m::Rect,
    last: m::Rect,
}

/// What is under a screen cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Tab(String),
    /// A panel row, by its index in [`crate::panel::rows`] (REQ-UI-042).
    Panel(usize),
    /// The panel's right edge, which drags to resize it.
    PanelEdge,
    Pane {
        id: String,
        x: u16,
        y: u16,
    },
    FloatMove(String),
    FloatResize(String),
    Nothing,
}

pub struct App {
    pub cfg: ClientConfig,
    pub bindings: Bindings,
    prefixes: Vec<Chord>,
    pub model: Model,
    pub mode: Mode,
    prefix_since: Option<Instant>,
    pub which_key_due: bool,
    second: Option<Second>,
    pub preselect: Option<Direction>,
    pub overlay: Overlay,
    pub copy: Option<CopyMode>,
    pub message: Option<String>,
    /// The remote host this client is attached through (`attach --host`), if any.
    pub remote: Option<String>,
    /// Shown while a remote link is down (it outlives key presses, unlike `message`).
    pub link_status: Option<String>,
    /// The daemon host's values from the `hosts` plugin (`plugin.data`), for `remote:` modules.
    pub remote_data: std::collections::BTreeMap<String, String>,
    /// Images on the host (REQ-GFX-001); enabled by the run loop when the host supports them.
    pub graphics: crate::graphics::Graphics,
    /// The latest frame's image placements.
    pub frame_images: Vec<m::ImagePlacement>,
    pub cols: u16,
    pub rows: u16,
    out: Vec<(u64, Request)>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// The paste buffer: the newest clipboard text seen (own yank or `clipboard.changed`).
    pub clipboard: Option<String>,
    /// Yanks sent as clipboard.set whose clipboard.changed echo is still to come.
    yanks_in_flight: std::collections::VecDeque<String>,
    pub plugins: Vec<Box<dyn ClientPlugin>>,
    pub cli_commands: Vec<String>,
    pub dirty: bool,
    effects: Vec<Effect>,
    drag: Option<Drag>,
    /// The mouse selection, while dragging and after (REQ-UI-053).
    pub selection: Option<Selection>,
    mouse_pane: Option<String>,
    /// Tab strip hit boxes from the last render: `(x0, x1, tab)`.
    pub tab_hits: Vec<(u16, u16, String)>,
    /// Side nav rows from the last render: `(y, target)` (REQ-UI-042).
    /// The panel (REQ-UI-042).
    pub panel: crate::panel::PanelState,
    /// Info bar modules by name (REQ-UI-031), with when each last sampled.
    pub modules: Vec<(Box<dyn femux_plugin::InfoModule>, Option<Instant>)>,
    /// `notify.posted` titles, newest first (REQ-UI-033).
    pub notifications: std::collections::VecDeque<(Option<String>, String, m::NotifyLevel)>,
    /// Every session, for the navigators and the side nav.
    pub dir: crate::nav::Directory,
    dir_fetched: Option<Instant>,
    preview_fetched: Option<(String, Instant)>,
    inbox_fetched: Option<Instant>,
    /// The navigator a prompt or confirmation returns to.
    return_to: Option<crate::nav::NavState>,
}

impl App {
    pub fn new(
        cfg: ClientConfig,
        plugins: Vec<Box<dyn ClientPlugin>>,
        cli_commands: Vec<String>,
        attach: &m::SessionAttachOk,
        cols: u16,
        rows: u16,
    ) -> App {
        let mut bindings = Bindings::from_config(&cfg);
        for p in &plugins {
            bindings.add_plugin(&p.bindings());
        }
        let prefixes = cfg.prefix.iter().filter_map(|p| Chord::parse(p)).collect();
        let (model, fx) = Model::from_attach(attach);
        let panel = crate::panel::PanelState {
            open: cfg.panel_open,
            width: cfg.panel_width.max(crate::panel::MIN_WIDTH),
            ..Default::default()
        };
        let mut app = App {
            cfg,
            bindings,
            prefixes,
            model,
            mode: Mode::Terminal,
            prefix_since: None,
            which_key_due: false,
            second: None,
            preselect: None,
            overlay: Overlay::None,
            copy: None,
            message: None,
            cols,
            rows,
            out: Vec::new(),
            next_id: 1,
            pending: HashMap::new(),
            clipboard: None,
            yanks_in_flight: std::collections::VecDeque::new(),
            plugins,
            cli_commands,
            dirty: true,
            effects: Vec::new(),
            drag: None,
            selection: None,
            mouse_pane: None,
            tab_hits: Vec::new(),
            panel,
            modules: Vec::new(),
            notifications: std::collections::VecDeque::new(),
            dir: crate::nav::Directory::default(),
            dir_fetched: None,
            preview_fetched: None,
            inbox_fetched: None,
            remote: None,
            link_status: None,
            remote_data: std::collections::BTreeMap::new(),
            graphics: crate::graphics::Graphics::new(false),
            frame_images: Vec::new(),
            return_to: None,
        };
        app.modules = app
            .plugins
            .iter()
            .flat_map(|p| p.modules())
            .map(|m| (m, None))
            .collect();
        app.model_effects(fx);
        let view_id = app.model.view_id.clone();
        app.send(m::FrameSubscribe { view_id }, Pending::Quiet);
        app
    }

    /// Applies a re-read config (`femux reload`, ADR-013): bindings, prefixes, mouse, chrome.
    pub fn apply_config(&mut self, cfg: ClientConfig) {
        let mut bindings = Bindings::from_config(&cfg);
        for p in &self.plugins {
            bindings.add_plugin(&p.bindings());
        }
        self.bindings = bindings;
        self.prefixes = cfg.prefix.iter().filter_map(|p| Chord::parse(p)).collect();
        // The tab strip or the bar moving can change the pane area's size, which the
        // daemon must hear about; a bar moving between top and bottom only moves it.
        let before = self.view_size();
        self.cfg = cfg;
        if self.view_size() != before {
            let (cols, rows) = (self.cols, self.rows);
            self.on_resize(cols, rows);
        }
        self.message = Some("config reloaded".into());
        self.dirty = true;
    }

    // ------------------------------------------------------------------ plumbing

    fn send(&mut self, req: impl Into<Request>, pending: Pending) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.out.push((id, req.into()));
        self.pending.insert(id, pending);
        id
    }

    fn quiet(&mut self, req: impl Into<Request>) {
        self.send(req, Pending::Quiet);
    }

    /// Requests to send, in order, with their ids.
    pub fn take_out(&mut self) -> Vec<(u64, Request)> {
        std::mem::take(&mut self.out)
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    pub fn prefix_armed(&self) -> bool {
        self.prefix_since.is_some()
    }

    /// `terminal`, `prefix`, `normal` or `copy`.
    pub fn mode_name(&self) -> &'static str {
        if self.prefix_armed() {
            return "prefix";
        }
        match self.mode {
            Mode::Terminal => "terminal",
            Mode::Normal => "normal",
            Mode::Copy => "copy",
        }
    }

    pub fn prefix_label(&self) -> String {
        self.prefixes
            .first()
            .map(ToString::to_string)
            .unwrap_or_default()
    }

    /// Where everything goes on screen (REQ-UI-030, REQ-UI-042).
    /// Every table as help shows it (REQ-UI-045).
    pub fn help(&self) -> crate::help::Help {
        crate::help::Help::build(&self.bindings, |_, _| None)
    }

    /// Help's body lines and how many show at once, for scrolling.
    fn help_dims(&self, st: &crate::help::HelpState) -> (usize, usize) {
        let lines = self.help().lines(&st.filter);
        let (_, _, _, bh) = crate::help::geometry(&self.layout().panes, &lines, self.cols as usize);
        (lines.len(), crate::help::body_rows(bh))
    }

    pub fn layout(&self) -> crate::layout::Layout {
        crate::layout::Layout::compute(
            self.cols,
            self.rows,
            &self.cfg,
            self.panel.open.then_some(self.panel.width),
        )
    }

    /// The remote link dropped (`retry`: the last failed attempt, its error and the wait
    /// before the next). The remote daemon keeps the session; the client reconnects.
    pub fn link_lost(&mut self, label: &str, retry: Option<(u32, String, std::time::Duration)>) {
        let text = match retry {
            None => format!("link to {label} lost; reconnecting · prefix d quits"),
            Some((n, e, next)) => {
                let e = e.lines().last().unwrap_or("").to_owned();
                format!(
                    "link to {label} lost; attempt {n} failed ({e}); retrying in {}s · prefix d quits",
                    next.as_secs()
                )
            }
        };
        if self.link_status.as_ref() != Some(&text) {
            self.link_status = Some(text);
            self.dirty = true;
        }
    }

    /// Brings the host's images up to date with the latest frame (REQ-GFX-001): each pane's
    /// placements at its screen position, none while a full-screen overlay hides the panes.
    /// Starts the image reads that are needed; the run loop writes `graphics.take_output()`.
    pub fn sync_graphics(&mut self) {
        if !self.graphics.enabled {
            return;
        }
        let hidden = matches!(
            self.overlay,
            Overlay::Navigator(_)
                | Overlay::Inbox(_)
                | Overlay::Marks(_)
                | Overlay::Palette(_)
                | Overlay::Help(_)
        );
        let area = self.layout().panes;
        let wanted: Vec<(m::ImagePlacement, m::Rect)> = if hidden {
            Vec::new()
        } else {
            self.frame_images
                .iter()
                .filter_map(|p| {
                    let r = &self.model.panes.get(&p.pane_id)?.screen.as_ref()?.rect;
                    let rect = m::Rect {
                        x: area.x + r.x,
                        y: area.y + r.y,
                        w: r.w,
                        h: r.h,
                    };
                    Some((p.clone(), rect))
                })
                .collect()
        };
        for read in self.graphics.update(&wanted) {
            let key = (read.pane_id.clone(), read.image_id);
            self.send(read, Pending::ImageRead(key.0, key.1));
        }
    }

    /// `prefix F`: a new floating shell when the tab has no floats, else hide or show them.
    fn float_scratch(&mut self) {
        let Some(tab) = self.model.current_tab().cloned() else {
            return;
        };
        if !tab.floats.is_empty() {
            self.quiet(m::TabSet {
                tab_id: tab.id.clone(),
                broadcast: None,
                floats_hidden: Some(!tab.floats_hidden),
            });
            return;
        }
        // Split a tiled pane, then float the new one (it starts in the tab's directory).
        let Some(base) = tab.tiled().into_iter().next() else {
            return;
        };
        self.send(
            m::LayoutSplit {
                pane_id: base,
                dir: m::SplitDir::H,
                ratio: None,
                cwd: None,
                cmd: None,
                preselect: None,
            },
            Pending::ScratchFloat,
        );
    }

    /// `prefix o` / `O`: focus the next or previous float, showing hidden ones.
    fn float_cycle(&mut self, forward: bool) {
        let Some(tab) = self.model.current_tab().cloned() else {
            return;
        };
        let floats: Vec<String> = tab.floats.iter().map(|f| f.id.clone()).collect();
        if floats.is_empty() {
            self.message = Some("no floats in this tab".into());
            return;
        }
        if tab.floats_hidden {
            self.quiet(m::TabSet {
                tab_id: tab.id.clone(),
                broadcast: None,
                floats_hidden: Some(false),
            });
        }
        let n = floats.len();
        let next = match floats.iter().position(|f| *f == tab.focus) {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None if forward => 0,
            None => n - 1,
        };
        let view_id = self.model.view_id.clone();
        self.quiet(m::LayoutFocus {
            view_id: Some(view_id),
            pane_id: Some(floats[next].clone()),
            dir: None,
        });
    }

    /// Marks of this session, by letter: (letter, pane, tab number, what the pane runs).
    pub fn mark_rows(&self) -> Vec<(String, String, usize, String)> {
        self.model
            .marks
            .iter()
            .filter_map(|(letter, pane)| {
                let tab = self.model.tab_of(pane)?;
                let n = self.model.tabs.iter().position(|t| t.id == tab.id)? + 1;
                let p = self.model.panes.get(pane)?;
                let what = if !p.title.is_empty() {
                    p.title.clone()
                } else {
                    format!("{} {}", p.fg_cmd, crate::model::short_dir(&p.cwd))
                };
                Some((letter.clone(), pane.clone(), n, what))
            })
            .collect()
    }

    fn marks_key(&mut self, selected: usize, chord: &Chord) {
        let rows = self.mark_rows();
        let key = chord.key.as_str();
        match key {
            "q" | "escape" => return,
            "j" | "down" => {
                self.overlay = Overlay::Marks((selected + 1).min(rows.len().saturating_sub(1)));
                return;
            }
            "k" | "up" => {
                self.overlay = Overlay::Marks(selected.saturating_sub(1));
                return;
            }
            "enter" => {
                if let Some((letter, ..)) = rows.get(selected) {
                    let letter = letter.clone();
                    self.jump_to_mark(&letter);
                }
                return;
            }
            "x" => {
                if let Some((letter, pane, ..)) = rows.get(selected) {
                    self.quiet(m::PaneMark {
                        pane_id: pane.clone(),
                        mark: letter.clone(),
                        clear: Some(true),
                    });
                }
                self.overlay = Overlay::Marks(selected.min(rows.len().saturating_sub(2)));
                return;
            }
            _ => {}
        }
        // A mark's letter jumps to it.
        if key.len() == 1
            && key.as_bytes()[0].is_ascii_lowercase()
            && rows.iter().any(|(l, ..)| l == key)
        {
            self.jump_to_mark(key);
            return;
        }
        self.overlay = Overlay::Marks(selected);
    }

    /// The focused pane is being recorded (the recorder plugin sets `recorder.file`).
    pub fn recording(&self) -> bool {
        self.model
            .focused_id()
            .and_then(|p| self.model.panes.get(&p))
            .is_some_and(|p| p.meta.contains_key("recorder.on"))
    }

    /// The session chip, `session <name>` (`host:name` when attached through `--host`): it
    /// says what it is so a name like `0` never reads as a tab number.
    /// The session chip (REQ-UI-030): the session's name (`host:name` over `--host`), with a
    /// `session ` prefix only when the name is all digits, so it never reads as a tab number.
    pub fn session_chip(&self) -> String {
        let name = match &self.remote {
            Some(h) => format!("{h}:{}", self.model.session),
            None => self.model.session.clone(),
        };
        let digits = !self.model.session.is_empty()
            && self.model.session.chars().all(|c| c.is_ascii_digit());
        if digits {
            format!("session {name}")
        } else {
            name
        }
    }

    /// A new link attached a view again: start over from its snapshot.
    pub fn reattached(&mut self, a: &m::SessionAttachOk) {
        self.pending.clear();
        let (model, fx) = crate::model::Model::from_attach(a);
        self.model = model;
        self.model_effects(fx);
        self.copy = None;
        self.mode = Mode::Terminal;
        self.overlay = Overlay::None;
        let view_id = a.view_id.clone();
        self.quiet(m::FrameSubscribe { view_id });
        self.link_status = None;
        self.message = Some(format!(
            "reconnected to {}",
            self.remote.clone().unwrap_or_default()
        ));
        self.refresh_dir();
        self.dirty = true;
    }

    /// The size the daemon sizes our view to (REQ-VIEW-001): the pane area.
    pub fn view_size(&self) -> (u16, u16) {
        let p = self.layout().panes;
        (p.w, p.h)
    }

    pub fn on_resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        let (c, r) = self.view_size();
        let view_id = self.model.view_id.clone();
        self.quiet(m::ViewResize {
            view_id,
            cols: c,
            rows: r,
        });
        self.dirty = true;
    }

    /// Advances timers: which-key, and info bar modules due for a sample. True if something
    /// changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some(since) = self.prefix_since
            && !self.which_key_due
            && now.duration_since(since) >= self.cfg.which_key_delay
        {
            self.which_key_due = true;
            self.overlay = Overlay::WhichKey;
            changed = true;
        }
        // REQ-UI-031: each shown module samples on its own interval.
        let shown: Vec<String> = self
            .cfg
            .bar_modules
            .iter()
            .flat_map(|n| [n.clone(), n.strip_prefix("remote:").unwrap_or(n).to_owned()])
            .collect();
        let ctx = self.bar_ctx(false);
        let floor = self.cfg.bar_interval;
        for (module, last) in &mut self.modules {
            if !shown.iter().any(|n| n == module.name()) {
                continue;
            }
            let every = module.interval().max(floor);
            if last.is_none_or(|l| now.duration_since(l) >= every) {
                module.sample(&ctx);
                *last = Some(now);
                changed = true;
            }
        }
        changed |= self.tick_navigators(now);
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// Whether this client shows `pane` right now (REQ-UI-033).
    pub fn pane_visible(&self, pane: &str) -> bool {
        self.model
            .current_tab()
            .is_some_and(|t| t.panes().iter().any(|p| p == pane) && (!t.zoomed || t.focus == pane))
    }

    /// What info modules may read (REQ-UI-031).
    /// What info modules may read (REQ-UI-031); `short` asks for their short form.
    pub fn bar_ctx(&self, short: bool) -> femux_plugin::BarCtx {
        let tabs = self
            .model
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| femux_plugin::TabItem {
                index: i + 1,
                title: self.model.tab_title(t),
                current: t.id == self.model.current,
                urgent: t.urgent,
            })
            .collect();
        let mut agents = std::collections::BTreeMap::new();
        for p in self.model.panes.values() {
            if let Some(s) = p.meta.get("agent.state") {
                *agents.entry(s.clone()).or_insert(0) += 1;
            }
        }
        let notifications = self
            .notifications
            .iter()
            .filter(|(pane, _, _)| pane.as_deref().is_none_or(|p| !self.pane_visible(p)))
            .take(3)
            .map(|(_, title, level)| femux_plugin::NoteItem {
                title: title.clone(),
                level: *level,
            })
            .collect();
        femux_plugin::BarCtx {
            session: self.model.session.clone(),
            tabs,
            agents,
            cwd: self
                .model
                .focused()
                .map(|p| p.cwd.clone())
                .filter(|c| !c.is_empty()),
            notifications,
            short,
            remote: self.remote_data.clone(),
        }
    }

    /// A module's output for `name`; `None` when the module is unknown or has nothing to show.
    /// `remote:<module>` is the `hosts` plugin's module of that name (the daemon host's
    /// values), else the local `<module>` (without the plugin, or for modules it lacks).
    pub fn module_spans(
        &self,
        name: &str,
        ctx: &femux_plugin::BarCtx,
        w: usize,
    ) -> Option<femux_plugin::Spans> {
        let base = name.strip_prefix("remote:").unwrap_or(name);
        let (module, _) = self
            .modules
            .iter()
            .find(|(m, _)| m.name() == name)
            .or_else(|| self.modules.iter().find(|(m, _)| m.name() == base))?;
        let spans = module.render(ctx, w);
        (femux_plugin::width(&spans) > 0).then_some(spans)
    }

    /// The which-key panel from whichever plugin provides one, laid out for a panel `width`
    /// cells wide (the pane area, beside any side nav or vertical bar).
    pub fn plugin_overlay(&self, width: usize) -> Option<PluginOverlay> {
        let table = self.bindings.which_key_table();
        let prefix_key = self.prefix_label();
        let ctx = OverlayCtx {
            mode: self.mode_name(),
            width,
            height: self.rows as usize,
            prefix_table: &table,
            prefix_key: &prefix_key,
        };
        self.plugins.iter().find_map(|p| p.overlay(&ctx))
    }

    fn model_effects(&mut self, fx: Vec<ModelEffect>) {
        for f in fx {
            match f {
                ModelEffect::NeedInfo(p) => {
                    self.send(m::PaneInfo { pane_id: p }, Pending::Info);
                }
                ModelEffect::Exit => self.effects.push(Effect::Exit),
                ModelEffect::Clipboard(t) => self.clipboard = Some(t),
                ModelEffect::Passthrough(b) => self.effects.push(Effect::Passthrough(b)),
            }
        }
    }

    // ------------------------------------------------------------------ incoming

    pub fn on_msg(&mut self, msg: ServerMsg) {
        self.dirty = true;
        match msg {
            ServerMsg::Push(Push::Frame(f)) => {
                self.model.on_frame(&f);
                self.frame_images = f.images.clone();
                if let Some(cm) = &self.copy
                    && !self.model.panes.contains_key(&cm.pane)
                {
                    self.leave_copy();
                }
                self.quiet(m::FrameAck {
                    view_id: f.view_id.clone(),
                    frame_seq: f.frame_seq,
                });
            }
            ServerMsg::Event(e) => self.on_event(&e),
            ServerMsg::Response(id, r) => self.on_response(id, Ok(r)),
            ServerMsg::Error(id, e) => self.on_response(id, Err(e)),
        }
    }

    fn on_event(&mut self, e: &Event) {
        if let Event::ConfigReloaded(_) = e {
            self.effects.push(Effect::Reload);
        }
        if let Event::PluginData(d) = e
            && d.plugin == "hosts"
        {
            self.remote_data = d.data.clone();
        }
        if let Event::NotifyPosted(n) = e {
            self.notifications
                .push_front((n.pane_id.clone(), n.title.clone(), n.level));
            self.notifications.truncate(20);
        }
        let fx = self.model.on_event(e);
        self.model_effects(fx);
        if let Some(cm) = &self.copy
            && !self.model.panes.contains_key(&cm.pane)
        {
            self.leave_copy();
        }
        let mut host = Vec::new();
        for p in &mut self.plugins {
            host.extend(p.on_event(e));
        }
        for h in host {
            match h {
                HostEffect::Clipboard(text) => {
                    // Our own yanks already went to the host; echoes arrive in order.
                    if let Some(i) = self.yanks_in_flight.iter().position(|t| *t == text) {
                        self.yanks_in_flight.drain(..=i);
                    } else {
                        self.effects.push(Effect::Clipboard(text));
                    }
                }
                HostEffect::Bell => self.effects.push(Effect::Bell),
                HostEffect::Sound(f) => self.effects.push(Effect::Sound(f)),
                HostEffect::Desktop { title, body } => {
                    self.effects.push(Effect::Desktop { title, body });
                }
            }
        }
    }

    fn on_response(&mut self, id: u64, r: Result<Response, ErrorBody>) {
        let Some(p) = self.pending.remove(&id) else {
            return;
        };
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                // A failed detach (the view is already gone) still ends the client.
                if let Pending::ImageRead(pane, image) = &p {
                    self.graphics.read_failed(pane, *image);
                } else if matches!(p, Pending::Detach) {
                    self.effects.push(Effect::Exit);
                } else if !matches!(p, Pending::Info | Pending::Quiet) {
                    self.message = Some(e.message);
                }
                return;
            }
        };
        match (p, r) {
            (Pending::Info, Response::PaneInfo(i)) => self.model.on_pane_info(&i),
            (Pending::ImageRead(pane, _), Response::ImageRead(r)) => {
                if let Some(next) = self.graphics.on_read(&pane, &r) {
                    let image = next.image_id;
                    self.send(next, Pending::ImageRead(pane, image));
                }
            }
            (Pending::ScratchFloat, Response::LayoutSplit(r)) => {
                let view_id = self.model.view_id.clone();
                self.quiet(m::LayoutFloat {
                    pane_id: r.pane_id.clone(),
                    rect: None,
                });
                self.quiet(m::LayoutFocus {
                    view_id: Some(view_id),
                    pane_id: Some(r.pane_id),
                    dir: None,
                });
            }
            (Pending::StartSession, Response::SessionCreate(s)) => {
                self.attach_to(s.name, None, None, None);
            }
            (Pending::NewTab, Response::TabCreate(t)) => {
                let view_id = self.model.view_id.clone();
                self.quiet(m::TabSelect {
                    view_id: Some(view_id),
                    tab_id: t.tab_id,
                });
            }
            (Pending::Read { from }, Response::ScrollbackRead(r)) => {
                if let Some(cm) = &mut self.copy {
                    cm.on_read(from, &r.lines, r.first_line);
                    let more = cm.refresh();
                    self.copy_needs(more);
                }
            }
            (Pending::Search { jump }, Response::ScrollbackSearch(r)) => {
                if let Some(cm) = &mut self.copy {
                    if jump && r.matches.is_empty() {
                        self.message = Some("no match".into());
                    }
                    cm.on_search(r.matches, jump);
                    // A jump may land on lines not fetched yet.
                    let more = cm.refresh();
                    self.copy_needs(more);
                }
            }
            (Pending::Marks, Response::ScrollbackMarks(r)) => {
                if let Some(cm) = &mut self.copy {
                    cm.on_marks(r.prompts);
                }
            }
            (Pending::Detach, _) => self.effects.push(Effect::Exit),
            (Pending::DirList, Response::SessionList(l)) => {
                let names: Vec<String> = l.sessions.iter().map(|s| s.name.clone()).collect();
                self.dir.on_list(l.sessions);
                for name in names {
                    self.send(
                        m::SessionTabs { name: name.clone() },
                        Pending::DirTabs(name),
                    );
                }
            }
            (Pending::DirTabs(session), Response::SessionTabs(t)) => {
                let panes: Vec<String> = t
                    .tabs
                    .iter()
                    .flat_map(crate::nav::DirSession::tab_panes)
                    .collect();
                self.dir.on_tabs(&session, t.tabs);
                for p in panes {
                    self.send(
                        m::PaneInfo { pane_id: p },
                        Pending::DirPane(session.clone()),
                    );
                }
            }
            (Pending::DirPane(session), Response::PaneInfo(i)) => self.dir.on_pane(&session, i),
            (Pending::Preview(pane), Response::ScrollbackRead(r)) => {
                if let Overlay::Navigator(nav) = &mut self.overlay {
                    nav.preview = Some((pane, r.lines));
                }
            }
            (Pending::InboxLine(pane), Response::ScrollbackRead(r)) => {
                if let Overlay::Inbox(st) = &mut self.overlay {
                    st.lines.insert(pane, last_line(&r.lines));
                }
            }
            (Pending::PanelLine(pane), Response::ScrollbackRead(r)) => {
                self.panel.lines.insert(pane, last_line(&r.lines));
            }
            (Pending::Switch { focus, then_kill }, Response::SessionAttach(a)) => {
                self.attached(&a, focus, then_kill);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ keys

    pub fn on_key(&mut self, k: m::KeyEvent, now: Instant) {
        self.dirty = true;
        self.message = None;
        self.selection = None;
        let chord = Chord::of(&k);

        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Palette(p) => return self.palette_key(p, &k, &chord),
            Overlay::Confirm { text, what } => {
                if chord.key == "y" || chord.key == "Y" || chord.key == "enter" {
                    match what {
                        Confirm::ClosePane(p) => self.quiet(m::LayoutClose { pane_id: p }),
                        Confirm::KillSession(name) => self.kill_session(name),
                    }
                } else {
                    let _ = text;
                }
                self.back_to_navigator();
                return;
            }
            Overlay::Prompt {
                title,
                mut input,
                what,
            } => {
                match chord.key.as_str() {
                    "escape" => self.back_to_navigator(),
                    "enter" => {
                        match what {
                            PromptKind::RenameTab(tab_id) => {
                                self.quiet(m::TabRename {
                                    tab_id,
                                    title: input,
                                });
                            }
                            PromptKind::NewSession => {
                                self.quiet(m::SessionCreate {
                                    name: input,
                                    root: None,
                                    file: None,
                                });
                            }
                            PromptKind::StartSession { root } => {
                                self.send(
                                    m::SessionCreate {
                                        name: input,
                                        root,
                                        file: None,
                                    },
                                    Pending::StartSession,
                                );
                            }
                            PromptKind::RenameSession(old) => {
                                if old == self.model.session {
                                    self.model.session = input.clone();
                                }
                                self.quiet(m::SessionRename {
                                    name: old,
                                    new: input,
                                });
                            }
                        }
                        self.back_to_navigator();
                    }
                    "backspace" => {
                        input.pop();
                        self.overlay = Overlay::Prompt { title, input, what };
                    }
                    _ => {
                        if let Some(t) = &k.text {
                            input.push_str(t);
                        }
                        self.overlay = Overlay::Prompt { title, input, what };
                    }
                }
                return;
            }
            Overlay::Help(mut st) => {
                let (total, fit) = self.help_dims(&st);
                let text = k.text.as_deref();
                if st.key(&chord.to_string(), text, total, fit) == crate::help::KeyOutcome::Stay {
                    self.overlay = Overlay::Help(st);
                }
                return;
            }
            Overlay::Navigator(nav) => return self.navigator_key(nav, &k, &chord),
            Overlay::Inbox(st) => return self.inbox_key(st, &k, &chord),
            Overlay::Marks(sel) => return self.marks_key(sel, &chord),
            Overlay::WhichKey | Overlay::None => {}
        }

        if self.mode == Mode::Copy {
            self.copy_key(&k, &chord);
            return;
        }

        if let Some(second) = self.second.take() {
            if chord.key.chars().count() == 1
                && chord.key.chars().all(|c| c.is_ascii_lowercase())
                && !chord.ctrl
                && !chord.alt
            {
                match second {
                    Second::Mark => {
                        if let Some(p) = self.model.focused_id() {
                            self.quiet(m::PaneMark {
                                pane_id: p,
                                mark: chord.key.clone(),
                                clear: None,
                            });
                        }
                    }
                    Second::Jump => self.jump_to_mark(&chord.key),
                }
            } else {
                self.message = Some("marks are a-z".into());
            }
            return;
        }

        if self.prefix_armed() {
            self.prefix_since = None;
            self.which_key_due = false;
            if chord.is_prefix(&self.prefixes) {
                // prefix prefix sends the prefix itself (REQ-UI-002).
                self.send_to_pane(vec![m::InputEvent::KeyEvent(k)]);
                return;
            }
            match self.bindings.lookup(Table::Prefix, &chord) {
                Some(a) => self.run(a),
                // esc cancels the prefix quietly (REQ-UI-002).
                None if chord.key == "escape" && !chord.ctrl && !chord.alt => {}
                None => self.message = Some(format!("{chord} is not bound")),
            }
            return;
        }

        match self.mode {
            Mode::Normal => {
                if chord.is_prefix(&self.prefixes) {
                    self.mode = Mode::Terminal;
                    return;
                }
                match self.bindings.lookup(Table::Prefix, &chord) {
                    Some(Action::NormalMode) => self.mode = Mode::Terminal,
                    Some(a) => self.run(a),
                    None if chord.key == "escape" => self.mode = Mode::Terminal,
                    None => {}
                }
            }
            Mode::Terminal => {
                if chord.is_prefix(&self.prefixes) {
                    self.prefix_since = Some(now);
                    self.which_key_due = false;
                    return;
                }
                // A focused panel takes the keys; the prefix still works (REQ-UI-042).
                if self.panel.focused && self.panel.open {
                    return self.panel_key(&chord);
                }
                match self.bindings.lookup(Table::Root, &chord) {
                    Some(a) => self.run(a),
                    None => self.send_to_pane(vec![m::InputEvent::KeyEvent(k)]),
                }
            }
            Mode::Copy => unreachable!(),
        }
    }

    pub fn on_paste(&mut self, text: String) {
        self.dirty = true;
        match &mut self.overlay {
            Overlay::Palette(p) => {
                p.input.push_str(&text);
                p.selected = 0;
            }
            Overlay::Prompt { input, .. } => input.push_str(&text),
            _ => {
                if self.mode != Mode::Copy {
                    self.send_to_pane(vec![m::InputEvent::Paste(m::Paste { text })]);
                }
            }
        }
    }

    fn send_to_pane(&mut self, events: Vec<m::InputEvent>) {
        if let Some(p) = self.model.focused_id() {
            self.quiet(m::PaneInput { pane_id: p, events });
        }
    }

    fn jump_to_mark(&mut self, letter: &str) {
        let Some(pane) = self.model.marks.get(letter).cloned() else {
            self.message = Some(format!("no mark {letter}"));
            return;
        };
        let Some(tab) = self.model.tab_of(&pane).map(|t| t.id.clone()) else {
            self.message = Some(format!("mark {letter} points at a closed pane"));
            return;
        };
        let view_id = self.model.view_id.clone();
        if tab != self.model.current {
            self.quiet(m::TabSelect {
                view_id: Some(view_id.clone()),
                tab_id: tab,
            });
        }
        self.quiet(m::LayoutFocus {
            view_id: Some(view_id),
            pane_id: Some(pane),
            dir: None,
        });
    }

    // ------------------------------------------------------------------ actions

    pub fn run(&mut self, a: Action) {
        let view_id = self.model.view_id.clone();
        let tab_id = self.model.current.clone();
        let focused = self.model.focused_id();
        match a {
            Action::Focus(d) => self.quiet(m::LayoutFocus {
                view_id: Some(view_id),
                pane_id: None,
                dir: Some(d),
            }),
            Action::Resize(d) => {
                if let Some(p) = focused {
                    let delta = match d {
                        Direction::H | Direction::L => 2,
                        Direction::J | Direction::K => 1,
                    };
                    self.quiet(m::LayoutResize {
                        pane_id: p,
                        dir: d,
                        delta_cells: delta,
                    });
                }
            }
            Action::SplitDown | Action::SplitRight => {
                if let Some(p) = focused {
                    let dir = if a == Action::SplitDown {
                        m::SplitDir::V
                    } else {
                        m::SplitDir::H
                    };
                    let preselect = self.preselect.take();
                    self.quiet(m::LayoutSplit {
                        pane_id: p,
                        dir,
                        ratio: None,
                        cwd: None,
                        cmd: None,
                        preselect,
                    });
                }
            }
            Action::ClosePane => {
                if let Some(p) = self.model.focused() {
                    if p.running() {
                        self.overlay = Overlay::Confirm {
                            text: format!("close pane running {}? y/n", p.fg_cmd),
                            what: Confirm::ClosePane(p.id.clone()),
                        };
                    } else {
                        let id = p.id.clone();
                        self.quiet(m::LayoutClose { pane_id: id });
                    }
                }
            }
            Action::Zoom => self.quiet(m::LayoutZoom { tab_id, on: None }),
            Action::FloatToggle => {
                if let (Some(p), Some(tab)) = (focused, self.model.current_tab()) {
                    if tab.is_float(&p) {
                        self.quiet(m::LayoutTile { pane_id: p });
                    } else {
                        self.quiet(m::LayoutFloat {
                            pane_id: p,
                            rect: None,
                        });
                    }
                }
            }
            Action::Equalize => self.quiet(m::LayoutEqualize { tab_id }),
            Action::Rotate => {
                if let Some(p) = focused {
                    self.quiet(m::LayoutRotate { pane_id: p });
                }
            }
            Action::SwapNext => {
                if let (Some(p), Some(tab)) = (focused, self.model.current_tab()) {
                    let tiled = tab.tiled();
                    if let Some(i) = tiled.iter().position(|x| *x == p)
                        && tiled.len() > 1
                    {
                        let b = tiled[(i + 1) % tiled.len()].clone();
                        self.quiet(m::LayoutSwap { a: p, b });
                    }
                }
            }
            Action::NewTab => {
                let cwd = self
                    .model
                    .focused()
                    .map(|p| p.cwd.clone())
                    .filter(|c| !c.is_empty());
                let session = self.model.session.clone();
                self.send(
                    m::TabCreate {
                        session,
                        cwd,
                        cmd: None,
                        title: None,
                        index: None,
                        restore: None,
                    },
                    Pending::NewTab,
                );
            }
            Action::RenameTab => {
                let current = self
                    .model
                    .current_tab()
                    .map(|t| self.model.tab_title(t))
                    .unwrap_or_default();
                self.overlay = Overlay::Prompt {
                    title: "rename tab".into(),
                    input: current,
                    what: PromptKind::RenameTab(tab_id),
                };
            }
            Action::NextTab | Action::PrevTab => {
                let n = self.model.tabs.len();
                if let Some(i) = self.model.tab_index(&tab_id)
                    && n > 1
                {
                    let j = if a == Action::NextTab {
                        (i + 1) % n
                    } else {
                        (i + n - 1) % n
                    };
                    let target = self.model.tabs[j].id.clone();
                    self.quiet(m::TabSelect {
                        view_id: Some(view_id),
                        tab_id: target,
                    });
                }
            }
            Action::Tab(n) => {
                if let Some(t) = self.model.tabs.get(n as usize - 1) {
                    let target = t.id.clone();
                    if target != tab_id {
                        self.quiet(m::TabSelect {
                            view_id: Some(view_id),
                            tab_id: target,
                        });
                    }
                } else {
                    self.message = Some(format!("no tab {n}"));
                }
            }
            Action::SetMark => self.second = Some(Second::Mark),
            Action::JumpMark => self.second = Some(Second::Jump),
            Action::CopyMode => self.enter_copy(),
            Action::Paste => {
                if let Some(text) = self.clipboard.clone() {
                    self.send_to_pane(vec![m::InputEvent::Paste(m::Paste { text })]);
                } else {
                    self.message = Some("nothing to paste".into());
                }
            }
            Action::SessionNav | Action::PaneNav => {
                let kind = if a == Action::SessionNav {
                    crate::nav::NavKind::Sessions
                } else {
                    crate::nav::NavKind::Panes
                };
                self.overlay =
                    Overlay::Navigator(crate::nav::NavState::new(kind, &self.model.session));
                self.refresh_dir();
            }
            Action::NewSession => {
                // Offer the focused pane's directory as both name and root.
                let cwd = self
                    .model
                    .focused_id()
                    .and_then(|p| self.model.panes.get(&p))
                    .map(|p| p.cwd.clone())
                    .filter(|c| !c.is_empty());
                let name = cwd
                    .as_deref()
                    .filter(|c| std::env::var("HOME").ok().as_deref() != Some(*c))
                    .and_then(|c| std::path::Path::new(c).file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.overlay = Overlay::Prompt {
                    title: "new session".into(),
                    input: name,
                    what: PromptKind::StartSession { root: cwd },
                };
            }
            Action::Panel => {
                self.panel.open = !self.panel.open;
                self.panel.focused = false;
                let (cols, rows) = (self.cols, self.rows);
                self.on_resize(cols, rows);
                if self.panel.open {
                    self.refresh_dir();
                }
            }
            Action::PanelFocus => {
                if !self.panel.open {
                    self.panel.open = true;
                    let (cols, rows) = (self.cols, self.rows);
                    self.on_resize(cols, rows);
                    self.refresh_dir();
                }
                self.panel.focused = true;
            }
            Action::Palette => self.open_palette(),
            Action::Marks => self.overlay = Overlay::Marks(0),
            Action::FloatScratch => self.float_scratch(),
            Action::FloatNext | Action::FloatPrev => self.float_cycle(a == Action::FloatNext),
            Action::Broadcast => {
                let on = self.model.current_tab().is_some_and(|t| t.broadcast);
                self.quiet(m::TabSet {
                    tab_id,
                    broadcast: Some(!on),
                    floats_hidden: None,
                });
            }
            Action::Record => {
                if let Some(p) = focused {
                    // The recorder plugin's daemon side records while this is set.
                    let on = self
                        .model
                        .panes
                        .get(&p)
                        .is_some_and(|x| x.meta.contains_key("recorder.on"));
                    self.quiet(m::PaneSetMeta {
                        pane_id: p,
                        set: (!on).then(|| [("recorder.on".to_owned(), "1".to_owned())].into()),
                        unset: on.then(|| vec!["recorder.on".to_owned()]),
                    });
                }
            }
            Action::Inbox => {
                self.overlay = Overlay::Inbox(crate::inbox::InboxState::default());
                self.inbox_fetched = None;
                self.refresh_dir();
            }
            Action::Detach => {
                self.send(m::SessionDetach { view_id }, Pending::Detach);
            }
            Action::Help => self.overlay = Overlay::Help(Default::default()),
            Action::NormalMode => {
                self.mode = if self.mode == Mode::Normal {
                    Mode::Terminal
                } else {
                    Mode::Normal
                };
            }
            Action::SendPrefix => {
                // The configured prefix itself, whatever key ran this action.
                if let Some(p) = self.prefixes.first() {
                    let ev = p.event();
                    self.send_to_pane(vec![m::InputEvent::KeyEvent(ev)]);
                }
            }
            Action::Preselect(d) => {
                self.preselect = if self.preselect == Some(d) {
                    None
                } else {
                    Some(d)
                };
            }
            Action::Scheme(s) => self.quiet(m::LayoutSetScheme { tab_id, scheme: s }),
            copy if copy.is_copy() && self.mode == Mode::Copy => {
                let needs = self
                    .copy
                    .as_mut()
                    .map(|cm| cm.act(copy))
                    .unwrap_or_default();
                self.copy_needs(needs);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ copy mode

    fn enter_copy(&mut self) {
        let Some(p) = self.model.focused() else {
            return;
        };
        let Some(screen) = &p.screen else {
            self.message = Some("no screen yet".into());
            return;
        };
        let (cm, needs) = CopyMode::new(
            &p.id,
            screen.rect.w,
            screen.rect.h,
            (screen.cursor.x, screen.cursor.y),
        );
        self.copy = Some(cm);
        self.mode = Mode::Copy;
        self.copy_needs(needs);
    }

    fn leave_copy(&mut self) {
        self.copy = None;
        if self.mode == Mode::Copy {
            self.mode = Mode::Terminal;
        }
    }

    fn copy_key(&mut self, k: &m::KeyEvent, chord: &Chord) {
        let Some(cm) = &mut self.copy else {
            self.mode = Mode::Terminal;
            return;
        };
        if let Some(needs) = cm.on_search_key(k) {
            self.copy_needs(needs);
            return;
        }
        if let Some(a) = self.bindings.lookup(Table::Copy, chord) {
            let needs = cm.act(a);
            self.copy_needs(needs);
        }
    }

    fn copy_needs(&mut self, needs: Vec<Need>) {
        let Some(cm) = &self.copy else { return };
        let pane = cm.pane.clone();
        for n in needs {
            match n {
                Need::Read { from, count } => {
                    self.send(
                        m::ScrollbackRead {
                            pane_id: pane.clone(),
                            from_line: from,
                            count,
                        },
                        Pending::Read { from },
                    );
                }
                Need::Search {
                    search: Search { regex, dir },
                    from,
                    limit,
                    jump,
                } => {
                    self.send(
                        m::ScrollbackSearch {
                            pane_id: pane.clone(),
                            regex,
                            from_line: from,
                            dir,
                            limit,
                        },
                        Pending::Search { jump },
                    );
                }
                Need::Marks => {
                    self.send(
                        m::ScrollbackMarks {
                            pane_id: pane.clone(),
                        },
                        Pending::Marks,
                    );
                }
                Need::Yank(text) => self.yank(text),
                Need::Exit => self.leave_copy(),
            }
        }
    }

    /// Copies `text`: OSC 52 on the host plus `clipboard.set` for everyone else (§5.1).
    fn yank(&mut self, text: String) {
        self.effects.push(Effect::Clipboard(text.clone()));
        self.yanks_in_flight.push_back(text.clone());
        self.clipboard = Some(text.clone());
        self.quiet(m::ClipboardSet { text });
    }

    /// Answers a blocked agent: the letter, then enter (REQ-UI-044, REQ-UI-042).
    fn answer_agent(&mut self, pane: &str, key: &str, name: &str, session: &str) {
        let enter = m::InputEvent::KeyEvent(m::KeyEvent {
            key: "enter".into(),
            mods: vec![],
            text: None,
            event: m::KeyEventKind::Press,
        });
        let letter = m::InputEvent::KeyEvent(m::KeyEvent {
            key: key.into(),
            mods: vec![],
            text: Some(key.into()),
            event: m::KeyEventKind::Press,
        });
        self.quiet(m::PaneInput {
            pane_id: pane.to_owned(),
            events: vec![letter, enter],
        });
        self.message = Some(format!("answered {key} to {name} in {session}"));
        self.refresh_dir();
    }

    // ------------------------------------------------------------------ panel

    /// Keeps the panel's agent lines and branches fresh while it is open (REQ-UI-042).
    fn tick_panel(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self
            .panel
            .lines_fetched
            .is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_secs(1))
        {
            self.panel.lines_fetched = Some(now);
            if !self.panel.collapsed.contains("agents") {
                for p in crate::panel::agent_panes(&self.dir) {
                    self.send(
                        m::ScrollbackRead {
                            pane_id: p.clone(),
                            from_line: 0,
                            count: 500,
                        },
                        Pending::PanelLine(p),
                    );
                }
            }
            let mut cwds: Vec<String> = self.model.panes.values().map(|p| p.cwd.clone()).collect();
            for s in &self.dir.sessions {
                cwds.extend(s.panes.values().map(|p| p.cwd.clone()));
            }
            self.panel.git.refresh(cwds, now);
            changed = true;
        }
        changed
    }

    /// A key while the panel has focus: `j`/`k` move, `tab` jumps to the next section,
    /// `h`/`l` fold, `enter` acts, `y`/`n` answer a blocked agent, `esc`/`q` give the keys
    /// back to the pane.
    fn panel_key(&mut self, chord: &Chord) {
        let rows = crate::panel::rows(self);
        let last = rows.len().saturating_sub(1);
        let sel = self.panel.selected.min(last);
        let kind = rows.get(sel).map(|r| r.kind.clone());
        match chord.key.as_str() {
            "escape" | "q" => self.panel.focused = false,
            "j" | "down" => self.panel.selected = (sel + 1).min(last),
            "k" | "up" => self.panel.selected = sel.saturating_sub(1),
            "tab" => {
                let header =
                    |r: &crate::panel::Row| matches!(r.kind, crate::panel::RowKind::Header(_));
                self.panel.selected = rows
                    .iter()
                    .enumerate()
                    .skip(sel + 1)
                    .find(|(_, r)| header(r))
                    .or_else(|| rows.iter().enumerate().find(|(_, r)| header(r)))
                    .map_or(sel, |(i, _)| i);
            }
            "h" | "left" => self.panel_fold(kind, false),
            "l" | "right" => self.panel_fold(kind, true),
            "enter" => self.panel_act(sel),
            key @ ("y" | "n") => {
                if let Some(crate::panel::RowKind::Agent {
                    blocked: true,
                    pane,
                    name,
                    session,
                    ..
                }) = kind
                {
                    self.answer_agent(&pane, key, &name, &session);
                }
            }
            _ => {}
        }
    }

    /// Opens or closes a section, or a session of `workspace`.
    fn panel_fold(&mut self, kind: Option<crate::panel::RowKind>, open: bool) {
        use crate::panel::RowKind;
        match kind {
            Some(RowKind::Header(s)) => {
                if open {
                    self.panel.collapsed.remove(&s);
                } else {
                    self.panel.collapsed.insert(s);
                }
            }
            Some(RowKind::Session(name)) => {
                let attached = name == self.model.session;
                let expanded = attached != self.panel.toggled.contains(&name);
                if expanded != open && !self.panel.toggled.remove(&name) {
                    self.panel.toggled.insert(name);
                }
            }
            _ => {}
        }
    }

    /// `enter` on row `i`, or a click: a header folds, anything else goes where it points.
    pub fn panel_act(&mut self, i: usize) {
        use crate::panel::RowKind;
        let rows = crate::panel::rows(self);
        let Some(row) = rows.get(i) else { return };
        self.panel.selected = i;
        match &row.kind {
            RowKind::Header(s) => {
                let open = self.panel.collapsed.contains(s);
                self.panel_fold(Some(row.kind.clone()), open);
            }
            kind => {
                if let Some(t) = kind.target() {
                    self.panel.focused = false;
                    self.switch_to(t);
                }
            }
        }
    }

    // ------------------------------------------------------------------ navigators

    /// Asks for every session, its tabs and its panes (the answers fill `self.dir`).
    pub fn refresh_dir(&mut self) {
        self.dir_fetched = Some(Instant::now());
        self.send(m::SessionList {}, Pending::DirList);
    }

    fn back_to_navigator(&mut self) {
        if let Some(nav) = self.return_to.take() {
            self.overlay = Overlay::Navigator(nav);
            self.refresh_dir();
        }
    }

    /// Keeps the directory and the preview fresh while they are on screen.
    fn tick_navigators(&mut self, now: Instant) -> bool {
        let nav_open = matches!(self.overlay, Overlay::Navigator(_) | Overlay::Inbox(_));
        let mut changed = false;
        if self.panel.open {
            changed |= self.tick_panel(now);
        }
        if (nav_open || self.panel.open)
            && self
                .dir_fetched
                .is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_secs(2))
        {
            self.refresh_dir();
            changed = true;
        }
        if let Overlay::Inbox(st) = &self.overlay {
            // What each listed agent last said, once a second.
            if self
                .inbox_fetched
                .is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_secs(1))
            {
                self.inbox_fetched = Some(now);
                let panes: Vec<String> = crate::inbox::entries(&self.dir, st)
                    .into_iter()
                    .map(|e| e.pane)
                    .collect();
                for p in panes {
                    self.send(
                        m::ScrollbackRead {
                            pane_id: p.clone(),
                            from_line: 0,
                            count: 500,
                        },
                        Pending::InboxLine(p),
                    );
                }
                changed = true;
            }
            return changed;
        }
        let Overlay::Navigator(nav) = &self.overlay else {
            return changed;
        };
        let rows = crate::nav::rows(nav, &self.dir, &self.model.session);
        let Some(pane) = rows
            .get(nav.selected)
            .and_then(|r| crate::nav::preview_pane(&r.target, &self.dir))
        else {
            return changed;
        };
        let due = match &self.preview_fetched {
            Some((p, t)) => {
                *p != pane || now.duration_since(*t) >= std::time::Duration::from_millis(500)
            }
            None => true,
        };
        if due {
            self.preview_fetched = Some((pane.clone(), now));
            let rows = self.rows;
            self.send(
                m::ScrollbackRead {
                    pane_id: pane.clone(),
                    from_line: 0,
                    count: rows as u32,
                },
                Pending::Preview(pane),
            );
            changed = true;
        }
        changed
    }

    fn navigator_key(&mut self, mut nav: crate::nav::NavState, k: &m::KeyEvent, chord: &Chord) {
        let rows = crate::nav::rows(&nav, &self.dir, &self.model.session);
        if nav.filtering {
            match chord.key.as_str() {
                "escape" | "enter" => nav.filtering = false,
                "backspace" => {
                    nav.filter.pop();
                }
                _ => {
                    if let Some(t) = &k.text {
                        nav.filter.push_str(t);
                    }
                }
            }
            nav.selected = 0;
            self.overlay = Overlay::Navigator(nav);
            return;
        }
        let selected = rows.get(nav.selected).map(|r| r.target.clone());
        match chord.key.as_str() {
            "q" | "escape" => return,
            "j" | "down" => nav.selected = (nav.selected + 1).min(rows.len().saturating_sub(1)),
            "k" | "up" => nav.selected = nav.selected.saturating_sub(1),
            "/" => nav.filtering = true,
            "tab" => {
                if let Some(crate::app::NavTarget::Session(s)) = &selected
                    && !nav.expanded.remove(s)
                {
                    nav.expanded.insert(s.clone());
                }
            }
            "enter" => {
                if let Some(t) = selected {
                    self.switch_to(t);
                }
                return;
            }
            "n" if nav.kind == crate::nav::NavKind::Sessions => {
                self.return_to = Some(nav);
                self.overlay = Overlay::Prompt {
                    title: "new session".into(),
                    input: String::new(),
                    what: PromptKind::NewSession,
                };
                return;
            }
            "r" if nav.kind == crate::nav::NavKind::Sessions => {
                if let Some(crate::app::NavTarget::Session(s)) = selected {
                    self.return_to = Some(nav);
                    self.overlay = Overlay::Prompt {
                        title: "rename session".into(),
                        input: s.clone(),
                        what: PromptKind::RenameSession(s),
                    };
                    return;
                }
            }
            "x" if nav.kind == crate::nav::NavKind::Sessions => {
                if let Some(crate::app::NavTarget::Session(s)) = selected {
                    self.return_to = Some(nav);
                    self.overlay = Overlay::Confirm {
                        text: format!("kill session {s}? y/n"),
                        what: Confirm::KillSession(s),
                    };
                    return;
                }
            }
            _ => {}
        }
        self.overlay = Overlay::Navigator(nav);
    }

    /// The inbox's keys (REQ-UI-044): `1`-`9` and `enter` jump, `y`/`n`/`a` answer a blocked
    /// agent (the letter plus Enter), `x` dismisses (the pane goes back to `idle`).
    fn inbox_key(&mut self, mut st: crate::inbox::InboxState, k: &m::KeyEvent, chord: &Chord) {
        let rows = crate::inbox::entries(&self.dir, &st);
        if st.filtering {
            match chord.key.as_str() {
                "escape" | "enter" => st.filtering = false,
                "backspace" => {
                    st.filter.pop();
                }
                _ => {
                    if let Some(t) = &k.text {
                        st.filter.push_str(t);
                    }
                }
            }
            st.selected = 0;
            self.overlay = Overlay::Inbox(st);
            return;
        }
        let jump = |app: &mut App, e: &crate::inbox::Entry| {
            app.switch_to(crate::app::NavTarget::Pane {
                session: e.session.clone(),
                tab: e.tab.clone(),
                pane: e.pane.clone(),
            });
        };
        let selected = rows.get(st.selected).cloned();
        match chord.key.as_str() {
            "q" | "escape" => return,
            "j" | "down" => st.selected = (st.selected + 1).min(rows.len().saturating_sub(1)),
            "k" | "up" => st.selected = st.selected.saturating_sub(1),
            "/" => st.filtering = true,
            "enter" => {
                if let Some(e) = selected {
                    jump(self, &e);
                }
                return;
            }
            d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && d != "0" => {
                if let Some(e) = rows.get(d.parse::<usize>().unwrap() - 1) {
                    jump(self, e);
                    return;
                }
            }
            key @ ("y" | "n" | "a") => {
                if let Some(e) = selected.filter(|e| e.state == "blocked") {
                    self.answer_agent(&e.pane, key, &e.name, &e.session);
                }
            }
            "x" => {
                if let Some(e) = selected {
                    let set = [
                        ("agent.state", "idle".to_owned()),
                        ("agent.source", "hook".to_owned()),
                        ("agent.since", crate::nav::now_ms().to_string()),
                    ]
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v))
                    .collect();
                    self.quiet(m::PaneSetMeta {
                        pane_id: e.pane.clone(),
                        set: Some(set),
                        unset: None,
                    });
                    // Gone from the list at once, whatever the next refresh brings.
                    for s in &mut self.dir.sessions {
                        if let Some(p) = s.panes.get_mut(&e.pane) {
                            p.meta.insert("agent.state".into(), "idle".into());
                        }
                    }
                    st.selected = st.selected.min(rows.len().saturating_sub(2));
                }
            }
            _ => {}
        }
        self.overlay = Overlay::Inbox(st);
    }

    /// Kills a session; killing the attached one first moves to another, if there is one.
    fn kill_session(&mut self, name: String) {
        if name != self.model.session {
            self.quiet(m::SessionKill { name });
            return;
        }
        let other = self
            .dir
            .sessions
            .iter()
            .map(|s| s.summary.name.clone())
            .find(|s| *s != name);
        match other {
            Some(o) => self.attach_to(o, None, None, Some(name)),
            None => self.quiet(m::SessionKill { name }),
        }
    }

    /// Goes to a session, tab or pane, attaching to its session if it is another one.
    pub fn switch_to(&mut self, target: crate::app::NavTarget) {
        use crate::app::NavTarget as T;
        let (session, tab, pane) = match target {
            T::Session(s) => (s, None, None),
            T::Tab { session, tab } => (session, Some(tab), None),
            T::Pane { session, tab, pane } => (session, Some(tab), Some(pane)),
        };
        if session != self.model.session {
            self.attach_to(session, tab, pane, None);
            return;
        }
        let view_id = self.model.view_id.clone();
        if let Some(tab) = tab
            && tab != self.model.current
        {
            self.quiet(m::TabSelect {
                view_id: Some(view_id.clone()),
                tab_id: tab,
            });
        }
        if let Some(pane) = pane {
            self.quiet(m::LayoutFocus {
                view_id: Some(view_id),
                pane_id: Some(pane),
                dir: None,
            });
        }
    }

    fn attach_to(
        &mut self,
        session: String,
        tab: Option<String>,
        focus: Option<String>,
        then_kill: Option<String>,
    ) {
        let (cols, rows) = self.view_size();
        self.send(
            m::SessionAttach {
                name: session,
                tab,
                cols: Some(cols),
                rows: Some(rows),
            },
            Pending::Switch { focus, then_kill },
        );
    }

    /// Now attached to another session: drop the old view, follow the new session's events
    /// and frames.
    fn attached(
        &mut self,
        a: &m::SessionAttachOk,
        focus: Option<String>,
        then_kill: Option<String>,
    ) {
        let old = std::mem::take(&mut self.model.view_id);
        self.quiet(m::SessionDetach { view_id: old });
        self.quiet(m::EventsSubscribe {
            session: Some(a.snapshot.session.clone()),
            ..Default::default()
        });
        let (model, fx) = crate::model::Model::from_attach(a);
        self.model = model;
        self.model_effects(fx);
        self.copy = None;
        self.mode = Mode::Terminal;
        let view_id = a.view_id.clone();
        self.quiet(m::FrameSubscribe {
            view_id: view_id.clone(),
        });
        if let Some(p) = focus {
            self.quiet(m::LayoutFocus {
                view_id: Some(view_id),
                pane_id: Some(p),
                dir: None,
            });
        }
        if let Some(old) = then_kill {
            self.quiet(m::SessionKill { name: old });
        }
        self.refresh_dir();
        self.dirty = true;
    }

    // ------------------------------------------------------------------ palette

    fn open_palette(&mut self) {
        let mut items = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (table, list, prefix) in [
            (Table::Root, &self.bindings.root, false),
            (Table::Prefix, &self.bindings.prefix, true),
        ] {
            let _ = table;
            for (chord, action) in list {
                if !seen.insert(*action) {
                    continue;
                }
                let key = if prefix {
                    format!("{} {chord}", self.prefix_label())
                } else {
                    chord.to_string()
                };
                let desc = action.describe();
                let name = action.name();
                items.push(PaletteItem {
                    hint: if desc == name {
                        key
                    } else {
                        format!("{desc} · {key}")
                    },
                    label: name,
                    run: PaletteRun::Action(*action),
                });
            }
        }
        for a in crate::bindings::ALL {
            if !a.is_copy() && seen.insert(*a) {
                items.push(PaletteItem {
                    label: a.name(),
                    hint: a.describe(),
                    run: PaletteRun::Action(*a),
                });
            }
        }
        for (i, p) in self.plugins.iter().enumerate() {
            for a in p.actions() {
                items.push(PaletteItem {
                    label: a.label,
                    hint: p.name().to_owned(),
                    run: PaletteRun::Plugin(i, a.id),
                });
            }
        }
        for c in &self.cli_commands {
            items.push(PaletteItem {
                label: format!("femux {c}"),
                hint: "cli".into(),
                run: PaletteRun::Cli(c.clone()),
            });
        }
        self.overlay = Overlay::Palette(Palette {
            input: String::new(),
            selected: 0,
            items,
        });
    }

    fn palette_key(&mut self, mut p: Palette, k: &m::KeyEvent, chord: &Chord) {
        match chord.key.as_str() {
            "escape" => {}
            "enter" => {
                let run = p.matches().get(p.selected).map(|i| i.run.clone());
                if let Some(run) = run {
                    match run {
                        PaletteRun::Action(a) => self.run(a),
                        PaletteRun::Cli(c) => {
                            self.send_to_pane(vec![m::InputEvent::Paste(m::Paste {
                                text: format!("femux {c} "),
                            })]);
                        }
                        PaletteRun::Plugin(i, id) => {
                            let reqs = self.plugins[i].run_action(&id);
                            for r in reqs {
                                self.quiet(r);
                            }
                        }
                    }
                }
            }
            "up" => {
                p.selected = p.selected.saturating_sub(1);
                self.overlay = Overlay::Palette(p);
            }
            "down" => {
                p.selected = (p.selected + 1).min(p.matches().len().saturating_sub(1));
                self.overlay = Overlay::Palette(p);
            }
            "backspace" => {
                p.input.pop();
                p.selected = 0;
                self.overlay = Overlay::Palette(p);
            }
            _ => {
                if chord.ctrl && (chord.key == "n" || chord.key == "j") {
                    p.selected = (p.selected + 1).min(p.matches().len().saturating_sub(1));
                } else if chord.ctrl && (chord.key == "p" || chord.key == "k") {
                    p.selected = p.selected.saturating_sub(1);
                } else if let Some(t) = &k.text {
                    p.input.push_str(t);
                    p.selected = 0;
                }
                self.overlay = Overlay::Palette(p);
            }
        }
    }

    // ------------------------------------------------------------------ mouse

    /// What is at screen cell `(x, y)`.
    pub fn hit(&self, x: u16, y: u16) -> Hit {
        let layout = self.layout();
        if layout.bar.contains(x, y) {
            return self
                .tab_hits
                .iter()
                .find(|(x0, x1, _)| x >= *x0 && x < *x1)
                .map(|(_, _, t)| Hit::Tab(t.clone()))
                .unwrap_or(Hit::Nothing);
        }
        if let Some(p) = layout.panel
            && p.contains(x, y)
        {
            if x == p.x + p.w - 1 {
                return Hit::PanelEdge;
            }
            return self
                .panel
                .hits
                .iter()
                .find(|(row, _)| *row == y)
                .map(|(_, i)| Hit::Panel(*i))
                .unwrap_or(Hit::Nothing);
        }
        let area = layout.panes;
        if !area.contains(x, y) {
            return Hit::Nothing;
        }
        let (px, py) = (x - area.x, y - area.y);
        let Some(tab) = self.model.current_tab() else {
            return Hit::Nothing;
        };
        // Floats are on top, the last one topmost (REQ-UI-011).
        for f in tab.floats.iter().rev() {
            let Some(screen) = self.model.panes.get(&f.id).and_then(|p| p.screen.as_ref()) else {
                continue;
            };
            let r = &screen.rect;
            let inside = px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h;
            if inside {
                return Hit::Pane {
                    id: f.id.clone(),
                    x: px - r.x,
                    y: py - r.y,
                };
            }
            let on_top = r.y > 0 && py + 1 == r.y && px + 1 >= r.x && px <= r.x + r.w;
            if on_top {
                return Hit::FloatMove(f.id.clone());
            }
            if px == r.x + r.w && py == r.y + r.h {
                return Hit::FloatResize(f.id.clone());
            }
        }
        for id in tab.tiled() {
            let Some(screen) = self.model.panes.get(&id).and_then(|p| p.screen.as_ref()) else {
                continue;
            };
            let r = &screen.rect;
            if px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h {
                return Hit::Pane {
                    id,
                    x: px - r.x,
                    y: py - r.y,
                };
            }
        }
        Hit::Nothing
    }

    pub fn on_mouse(&mut self, x: u16, y: u16, kind: MouseKind, mods: Vec<m::Mod>) {
        if !self.cfg.mouse {
            return;
        }
        self.dirty = true;
        // Help (REQ-UI-045): the wheel over the box scrolls, a click outside closes it.
        if let Overlay::Help(st) = &self.overlay {
            let mut st = st.clone();
            let lines = self.help().lines(&st.filter);
            let (bx, by, bw, bh) =
                crate::help::geometry(&self.layout().panes, &lines, self.cols as usize);
            let (x, y) = (x as usize, y as usize);
            let inside = x >= bx && x < bx + bw && y >= by && y < by + bh;
            let fit = crate::help::body_rows(bh);
            match kind {
                MouseKind::WheelUp | MouseKind::WheelDown if inside => {
                    st.wheel(matches!(kind, MouseKind::WheelDown), lines.len(), fit);
                    self.overlay = Overlay::Help(st);
                }
                MouseKind::Press(_) if !inside => self.overlay = Overlay::None,
                _ => {}
            }
            return;
        }
        if self.panel.dragging {
            match kind {
                MouseKind::Move | MouseKind::Press(_) => {
                    let w = (x + 1).clamp(crate::panel::MIN_WIDTH, (self.cols / 2).max(1));
                    if w != self.panel.width {
                        self.panel.width = w;
                        let (cols, rows) = (self.cols, self.rows);
                        self.on_resize(cols, rows);
                    }
                }
                MouseKind::Release => self.panel.dragging = false,
                _ => {}
            }
            return;
        }
        if let Some(d) = &mut self.drag {
            match kind {
                MouseKind::Move | MouseKind::Press(_) => {
                    let dx = x as i32 - d.from.0 as i32;
                    let dy = y as i32 - d.from.1 as i32;
                    let mut r = d.rect.clone();
                    if d.resize {
                        r.w = (r.w as i32 + dx).max(1) as u16;
                        r.h = (r.h as i32 + dy).max(1) as u16;
                    } else {
                        r.x = (r.x as i32 + dx).max(0) as u16;
                        r.y = (r.y as i32 + dy).max(0) as u16;
                    }
                    if r != d.last {
                        d.last = r.clone();
                        let pane = d.pane.clone();
                        self.quiet(m::LayoutFloat {
                            pane_id: pane,
                            rect: Some(r),
                        });
                    }
                }
                MouseKind::Release => self.drag = None,
                _ => {}
            }
            return;
        }
        if self.mode == Mode::Copy {
            if let Some(cm) = &mut self.copy {
                let a = match kind {
                    MouseKind::WheelUp => Some(Action::CopyUp),
                    MouseKind::WheelDown => Some(Action::CopyDown),
                    _ => None,
                };
                if let Some(a) = a {
                    let mut needs = Vec::new();
                    for _ in 0..3 {
                        needs.extend(cm.act(a));
                    }
                    self.copy_needs(needs);
                }
            }
            return;
        }
        if self.selection.as_ref().is_some_and(|s| !s.done) {
            return self.extend_selection(x, y, kind);
        }
        if matches!(kind, MouseKind::Press(_)) {
            self.selection = None;
        }
        match (self.hit(x, y), kind) {
            (Hit::PanelEdge, MouseKind::Press(m::MouseButton::Left)) => self.panel.dragging = true,
            (Hit::Panel(i), MouseKind::Press(m::MouseButton::Left)) => self.panel_act(i),
            (Hit::Tab(t), MouseKind::Press(_)) => {
                let view_id = self.model.view_id.clone();
                self.quiet(m::TabSelect {
                    view_id: Some(view_id),
                    tab_id: t,
                });
            }
            (Hit::FloatMove(p), MouseKind::Press(m::MouseButton::Left))
            | (Hit::FloatResize(p), MouseKind::Press(m::MouseButton::Left)) => {
                let resize = matches!(self.hit(x, y), Hit::FloatResize(_));
                if let Some(rect) = self
                    .model
                    .panes
                    .get(&p)
                    .and_then(|pn| pn.screen.as_ref())
                    .map(|s| s.rect.clone())
                {
                    self.drag = Some(Drag {
                        pane: p,
                        resize,
                        from: (x, y),
                        last: rect.clone(),
                        rect,
                    });
                }
            }
            (Hit::Pane { id, x: px, y: py }, MouseKind::Press(button)) => {
                if self.model.focused_id().as_deref() != Some(id.as_str()) {
                    let view_id = self.model.view_id.clone();
                    self.quiet(m::LayoutFocus {
                        view_id: Some(view_id),
                        pane_id: Some(id.clone()),
                        dir: None,
                    });
                }
                // A drag selects text unless the program tracks the mouse; shift always
                // selects (REQ-UI-053).
                let tracks = self
                    .model
                    .panes
                    .get(&id)
                    .and_then(|p| p.screen.as_ref())
                    .is_some_and(|s| s.mouse);
                if button == m::MouseButton::Left && (!tracks || mods.contains(&m::Mod::Shift)) {
                    self.selection = Some(Selection {
                        pane: id,
                        anchor: (px, py),
                        head: (px, py),
                        done: false,
                    });
                    return;
                }
                self.mouse_pane = Some(id.clone());
                self.forward_mouse(&id, px, py, Some(button), m::MouseEventKind::Press, mods);
            }
            (Hit::Pane { id, x: px, y: py }, MouseKind::WheelUp) => {
                self.forward_mouse(&id, px, py, None, m::MouseEventKind::WheelUp, mods);
            }
            (Hit::Pane { id, x: px, y: py }, MouseKind::WheelDown) => {
                self.forward_mouse(&id, px, py, None, m::MouseEventKind::WheelDown, mods);
            }
            (hit, MouseKind::Release | MouseKind::Move) => {
                let Some(id) = self.mouse_pane.clone() else {
                    return;
                };
                // Motion and release go to the pane that got the press, clamped to it.
                let (px, py) = match &hit {
                    Hit::Pane { id: h, x, y } if *h == id => (*x, *y),
                    _ => {
                        let Some(r) = self
                            .model
                            .panes
                            .get(&id)
                            .and_then(|p| p.screen.as_ref())
                            .map(|s| s.rect.clone())
                        else {
                            return;
                        };
                        let area = self.layout().panes;
                        (
                            x.saturating_sub(area.x + r.x).min(r.w.saturating_sub(1)),
                            y.saturating_sub(area.y + r.y).min(r.h.saturating_sub(1)),
                        )
                    }
                };
                let ev = if kind == MouseKind::Release {
                    self.mouse_pane = None;
                    m::MouseEventKind::Release
                } else {
                    m::MouseEventKind::Move
                };
                self.forward_mouse(&id, px, py, None, ev, mods);
            }
            _ => {}
        }
    }

    /// Moves the selection's head to (`x`, `y`), clamped to its pane; on release copies what
    /// it covers. A click that never moved selects nothing (REQ-UI-053).
    fn extend_selection(&mut self, x: u16, y: u16, kind: MouseKind) {
        let area = self.layout().panes;
        let Some(sel) = &mut self.selection else {
            return;
        };
        let Some(screen) = self
            .model
            .panes
            .get(&sel.pane)
            .and_then(|p| p.screen.as_ref())
        else {
            self.selection = None;
            return;
        };
        let r = &screen.rect;
        sel.head = (
            x.saturating_sub(area.x + r.x).min(r.w.saturating_sub(1)),
            y.saturating_sub(area.y + r.y).min(r.h.saturating_sub(1)),
        );
        if kind != MouseKind::Release {
            return;
        }
        if sel.head == sel.anchor {
            self.selection = None;
            return;
        }
        sel.done = true;
        let text = sel.text(screen);
        if !text.trim().is_empty() {
            self.yank(text);
        }
    }

    fn forward_mouse(
        &mut self,
        pane: &str,
        x: u16,
        y: u16,
        button: Option<m::MouseButton>,
        event: m::MouseEventKind,
        mods: Vec<m::Mod>,
    ) {
        self.quiet(m::PaneInput {
            pane_id: pane.to_owned(),
            events: vec![m::InputEvent::MouseEvent(m::MouseEvent {
                x,
                y,
                button,
                mods,
                event,
            })],
        });
    }
}

/// What a pane last said (REQ-UI-044): see [`crate::inbox::said_line`].
fn last_line(lines: &[Vec<m::Cell>]) -> String {
    let rows: Vec<String> = lines
        .iter()
        .map(|l| {
            l.iter()
                .map(|c| if c.ch.is_empty() { " " } else { c.ch.as_str() })
                .collect::<String>()
        })
        .collect();
    crate::inbox::said_line(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::key;

    fn app(cfg: ClientConfig) -> App {
        let attach = m::SessionAttachOk {
            view_id: "V".into(),
            snapshot: m::SessionSnapshot {
                session: "s".into(),
                tabs: vec![m::TabSnapshot {
                    id: "T".into(),
                    focus: "P".into(),
                    tree: m::LayoutNode::PaneNode(m::PaneNode { id: "P".into() }),
                    ..Default::default()
                }],
                current_tab: "T".into(),
                marks: Default::default(),
            },
        };
        let mut a = App::new(cfg, Vec::new(), Vec::new(), &attach, 80, 24);
        a.take_out();
        a
    }

    fn pane_keys(out: &[(u64, Request)]) -> Vec<m::KeyEvent> {
        out.iter()
            .flat_map(|(_, r)| match r {
                Request::PaneInput(p) => p.events.clone(),
                _ => vec![],
            })
            .filter_map(|e| match e {
                m::InputEvent::KeyEvent(k) => Some(k),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_bound_prefix_action_sends_the_prefix_itself() {
        let mut cfg = ClientConfig::default();
        cfg.keys_prefix.insert("a".into(), "prefix".into());
        let mut a = app(cfg);
        let now = Instant::now();
        a.on_key(key("ctrl+space"), now);
        a.on_key(key("a"), now);
        let keys = pane_keys(&a.take_out());
        assert_eq!(keys, [key("ctrl+space")]);
    }

    #[test]
    fn yank_echoes_are_not_replayed_to_the_host() {
        let mut a = app(ClientConfig::default());
        a.plugins = vec![femux_plugin_clipboard_stub()];
        a.copy = Some(CopyMode::new("P", 80, 22, (0, 0)).0);
        a.mode = Mode::Copy;
        for text in ["foo", "bar"] {
            a.copy_needs(vec![Need::Yank(text.into())]);
        }
        assert_eq!(
            a.take_effects(),
            [
                Effect::Clipboard("foo".into()),
                Effect::Clipboard("bar".into())
            ]
        );
        for text in ["foo", "bar"] {
            a.on_event(&Event::ClipboardChanged(m::ClipboardChanged {
                text: text.into(),
                source: m::ClipboardSource::Client,
                ..Default::default()
            }));
        }
        assert!(a.take_effects().is_empty(), "own yanks are not echoed");
        a.on_event(&Event::ClipboardChanged(m::ClipboardChanged {
            text: "other".into(),
            source: m::ClipboardSource::Client,
            ..Default::default()
        }));
        assert_eq!(a.take_effects(), [Effect::Clipboard("other".into())]);
        assert_eq!(a.clipboard.as_deref(), Some("other"));
    }

    /// REQ-UI-030 (0.10.0): the chip is the session's bare name, `host:name` over `--host`,
    /// and says `session` only when the name is all digits.
    #[test]
    fn the_session_chip_is_the_bare_name_unless_it_is_a_number() {
        let mut a = app(ClientConfig::default());
        a.model.session = "api".into();
        assert_eq!(a.session_chip(), "api");
        a.remote = Some("box".into());
        assert_eq!(a.session_chip(), "box:api");
        a.model.session = "0".into();
        assert_eq!(a.session_chip(), "session box:0");
        a.remote = None;
        assert_eq!(a.session_chip(), "session 0");
    }

    /// REQ-SEC-004: a passed-through sequence reaches the host only from a pane this client
    /// shows.
    #[test]
    fn passthrough_is_written_for_shown_panes_only() {
        use base64::Engine;
        let mut a = app(ClientConfig::default());
        let seq = b"\x1bP1;2|x\x1b\\".to_vec();
        for pane in ["Q", "P"] {
            a.on_event(&Event::PanePassthrough(m::PanePassthrough {
                pane_id: pane.into(),
                data: base64::engine::general_purpose::STANDARD.encode(&seq),
                ..Default::default()
            }));
        }
        assert_eq!(a.take_effects(), [Effect::Passthrough(seq)]);
    }

    /// Two panes side by side, A (pid 10) restored and B (pid 20) not, with a full frame.
    fn restored_app() -> App {
        let pane = |id: &str| m::LayoutNode::PaneNode(m::PaneNode { id: id.into() });
        let attach = m::SessionAttachOk {
            view_id: "V".into(),
            snapshot: m::SessionSnapshot {
                session: "s".into(),
                tabs: vec![m::TabSnapshot {
                    id: "T".into(),
                    focus: "B".into(),
                    tree: m::LayoutNode::SplitNode(m::SplitNode {
                        dir: m::SplitDir::H,
                        ratio: m::Ratio { num: 1, den: 2 },
                        children: vec![pane("A"), pane("B")],
                    }),
                    ..Default::default()
                }],
                current_tab: "T".into(),
                marks: Default::default(),
            },
        };
        let mut a = App::new(
            ClientConfig::default(),
            Vec::new(),
            Vec::new(),
            &attach,
            21,
            6,
        );
        for (id, req) in a.take_out() {
            if let Request::PaneInfo(r) = req {
                let restored = r.pane_id == "A";
                a.on_msg(ServerMsg::Response(
                    id,
                    Response::PaneInfo(m::PaneInfoOk {
                        id: r.pane_id.clone(),
                        tab: "T".into(),
                        session: "s".into(),
                        pid: if restored { 10 } else { 20 },
                        fg_pid: if restored { 10 } else { 20 },
                        fg_cmd: "sh".into(),
                        restored,
                        ..Default::default()
                    }),
                ));
            }
        }
        let rect = |x, w| m::Rect { x, y: 0, w, h: 4 };
        a.on_msg(ServerMsg::Push(femux_proto::Push::Frame(m::Frame {
            view_id: "V".into(),
            frame_seq: 1,
            full: true,
            panes: vec![
                m::PaneFrame {
                    pane_id: "A".into(),
                    rect: rect(0, 10),
                    ..Default::default()
                },
                m::PaneFrame {
                    pane_id: "B".into(),
                    rect: rect(11, 10),
                    ..Default::default()
                },
            ],
            tab: m::TabInfo {
                id: "T".into(),
                title: "sh".into(),
                index: 0,
                count: 1,
            },
            images: vec![],
        })));
        a
    }

    fn screen(a: &mut App) -> Vec<String> {
        let mut s = termwiz::surface::Surface::new(a.cols as usize, a.rows as usize);
        crate::render::render(a, &crate::theme::Theme::dark(), &mut s);
        s.screen_chars_to_string()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn store_008_restored_panes_have_a_dashed_border_until_a_command_runs() {
        let mut a = restored_app();
        let rows = screen(&mut a);
        // The separator between A and B is column 10; A is restored, so it is dashed.
        assert_eq!(rows[1].chars().nth(10), Some('┆'), "{rows:?}");
        assert!(
            rows.last().unwrap().contains('↺'),
            "the bar's tabs mark restored panes: {rows:?}"
        );
        // A command runs in A: the border is ordinary again.
        a.on_msg(ServerMsg::Event(Event::PaneFgChanged(m::PaneFgChanged {
            pane_id: "A".into(),
            pid: 11,
            cmd: "vim".into(),
            ..Default::default()
        })));
        let rows = screen(&mut a);
        assert_eq!(rows[1].chars().nth(10), Some('│'), "{rows:?}");
        assert!(!rows[0].contains('↺'), "{rows:?}");
    }

    #[test]
    fn config_reloaded_asks_for_the_config_and_applies_it() {
        let mut a = app(ClientConfig::default());
        a.on_msg(ServerMsg::Event(Event::ConfigReloaded(
            m::ConfigReloaded::default(),
        )));
        assert!(a.take_effects().contains(&Effect::Reload));
        let mut cfg = ClientConfig {
            prefix: vec!["ctrl+b".into()],
            ..Default::default()
        };
        cfg.keys_prefix.insert("v".into(), "zoom".into());
        a.apply_config(cfg);
        let now = Instant::now();
        a.on_key(key("ctrl+space"), now);
        assert!(!a.prefix_armed(), "the old prefix is gone");
        a.take_out();
        a.on_key(key("ctrl+b"), now);
        assert!(a.prefix_armed());
        a.on_key(key("v"), now);
        assert!(
            a.take_out()
                .iter()
                .any(|(_, r)| matches!(r, Request::LayoutZoom(_))),
            "the new bindings apply"
        );
    }

    #[test]
    fn a_reload_that_moves_the_bar_keeps_the_view() {
        let mut a = app(ClientConfig::default());
        a.on_resize(100, 30);
        a.take_out();
        let resizes = |a: &mut App| -> Vec<(u16, u16)> {
            a.take_out()
                .into_iter()
                .filter_map(|(_, r)| match r {
                    Request::ViewResize(v) => Some((v.cols, v.rows)),
                    _ => None,
                })
                .collect()
        };
        // Bottom to top: the same pane area, so the daemon has nothing to hear.
        a.apply_config(ClientConfig {
            bar: crate::config::BarPosition::Top,
            ..Default::default()
        });
        assert!(resizes(&mut a).is_empty());
    }

    /// A client plugin that forwards clipboard events like the real `clipboard` plugin.
    fn femux_plugin_clipboard_stub() -> Box<dyn ClientPlugin> {
        struct P;
        impl ClientPlugin for P {
            fn name(&self) -> &'static str {
                "clipboard"
            }
            fn on_event(&mut self, ev: &Event) -> Vec<HostEffect> {
                match ev {
                    Event::ClipboardChanged(c) => vec![HostEffect::Clipboard(c.text.clone())],
                    _ => vec![],
                }
            }
        }
        Box::new(P)
    }
}
