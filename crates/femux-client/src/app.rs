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
use crate::config::{ClientConfig, TabsPosition};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    ClosePane(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    RenameTab(String),
}

#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    /// The which-key panel (from the plugin) once the prefix has been held long enough.
    WhichKey,
    Help,
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
    Read { from: i64 },
    Search { jump: bool },
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
    Pane { id: String, x: u16, y: u16 },
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
    pub cols: u16,
    pub rows: u16,
    out: Vec<(u64, Request)>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// The paste buffer: the newest clipboard text seen (own yank or `clipboard.changed`).
    pub clipboard: Option<String>,
    last_sent_clipboard: Option<String>,
    pub plugins: Vec<Box<dyn ClientPlugin>>,
    pub cli_commands: Vec<String>,
    pub dirty: bool,
    effects: Vec<Effect>,
    drag: Option<Drag>,
    mouse_pane: Option<String>,
    /// Tab strip hit boxes from the last render: `(x0, x1, tab)`.
    pub tab_hits: Vec<(u16, u16, String)>,
    last_key: Option<m::KeyEvent>,
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
        let bindings = Bindings::from_config(&cfg);
        let prefixes = cfg.prefix.iter().filter_map(|p| Chord::parse(p)).collect();
        let (model, fx) = Model::from_attach(attach);
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
            last_sent_clipboard: None,
            plugins,
            cli_commands,
            dirty: true,
            effects: Vec::new(),
            drag: None,
            mouse_pane: None,
            tab_hits: Vec::new(),
            last_key: None,
        };
        app.model_effects(fx);
        let view_id = app.model.view_id.clone();
        app.send(m::FrameSubscribe { view_id }, Pending::Quiet);
        app
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

    /// Rows of chrome above the pane area.
    pub fn top_chrome(&self) -> u16 {
        u16::from(self.cfg.tabs == TabsPosition::Top)
    }

    /// Rows of chrome below the pane area (tab strip at the bottom plus the status line).
    pub fn bottom_chrome(&self) -> u16 {
        1 + u16::from(self.cfg.tabs == TabsPosition::Bottom)
    }

    /// The size the daemon sizes our view to (REQ-VIEW-001): the terminal minus chrome.
    pub fn view_size(&self) -> (u16, u16) {
        (
            self.cols.max(1),
            self.rows
                .saturating_sub(self.top_chrome() + self.bottom_chrome())
                .max(1),
        )
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

    /// Advances timers; true if something changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        if let Some(since) = self.prefix_since
            && !self.which_key_due
            && now.duration_since(since) >= self.cfg.which_key_delay
        {
            self.which_key_due = true;
            self.overlay = Overlay::WhichKey;
            self.dirty = true;
            return true;
        }
        false
    }

    /// The which-key panel from whichever plugin provides one.
    pub fn plugin_overlay(&self) -> Option<PluginOverlay> {
        let table = self.bindings.which_key_table();
        let prefix_key = self.prefix_label();
        let ctx = OverlayCtx {
            mode: self.mode_name(),
            width: self.cols as usize,
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
            }
        }
    }

    // ------------------------------------------------------------------ incoming

    pub fn on_msg(&mut self, msg: ServerMsg) {
        self.dirty = true;
        match msg {
            ServerMsg::Push(Push::Frame(f)) => {
                self.model.on_frame(&f);
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
                    if self.last_sent_clipboard.as_deref() == Some(text.as_str()) {
                        // Our own yank already went to the host.
                        self.last_sent_clipboard = None;
                    } else {
                        self.effects.push(Effect::Clipboard(text));
                    }
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
                if !matches!(p, Pending::Info | Pending::Quiet) {
                    self.message = Some(e.message);
                }
                return;
            }
        };
        match (p, r) {
            (Pending::Info, Response::PaneInfo(i)) => self.model.on_pane_info(&i),
            (Pending::NewTab, Response::TabCreate(t)) => {
                let view_id = self.model.view_id.clone();
                self.quiet(m::TabSelect {
                    view_id,
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
            _ => {}
        }
    }

    // ------------------------------------------------------------------ keys

    pub fn on_key(&mut self, k: m::KeyEvent, now: Instant) {
        self.dirty = true;
        self.message = None;
        let chord = Chord::of(&k);
        self.last_key = Some(k.clone());

        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Palette(p) => return self.palette_key(p, &k, &chord),
            Overlay::Confirm { text, what } => {
                if chord.key == "y" || chord.key == "Y" || chord.key == "enter" {
                    match what {
                        Confirm::ClosePane(p) => self.quiet(m::LayoutClose { pane_id: p }),
                    }
                } else {
                    let _ = text;
                }
                return;
            }
            Overlay::Prompt {
                title,
                mut input,
                what,
            } => {
                match chord.key.as_str() {
                    "escape" => {}
                    "enter" => match what {
                        PromptKind::RenameTab(tab_id) => {
                            self.quiet(m::TabRename {
                                tab_id,
                                title: input,
                            });
                        }
                    },
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
            Overlay::Help => {
                // Any key closes help; the key is not otherwise interpreted.
                return;
            }
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
                    None => {}
                }
            }
            Mode::Terminal => {
                if chord.is_prefix(&self.prefixes) {
                    self.prefix_since = Some(now);
                    self.which_key_due = false;
                    return;
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
                view_id: view_id.clone(),
                tab_id: tab,
            });
        }
        self.quiet(m::LayoutFocus {
            view_id,
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
                view_id,
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
                        view_id,
                        tab_id: target,
                    });
                }
            }
            Action::Tab(n) => {
                if let Some(t) = self.model.tabs.get(n as usize - 1) {
                    let target = t.id.clone();
                    if target != tab_id {
                        self.quiet(m::TabSelect {
                            view_id,
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
            Action::SessionNav | Action::WindowNav | Action::SideNav => {
                self.message = Some(format!("{} arrives in 0.4.0", a.describe()));
            }
            Action::Palette => self.open_palette(),
            Action::Detach => {
                self.send(m::SessionDetach { view_id }, Pending::Detach);
            }
            Action::Help => self.overlay = Overlay::Help,
            Action::NormalMode => {
                self.mode = if self.mode == Mode::Normal {
                    Mode::Terminal
                } else {
                    Mode::Normal
                };
            }
            Action::SendPrefix => {
                if let Some(k) = self.last_key.clone() {
                    self.send_to_pane(vec![m::InputEvent::KeyEvent(k)]);
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
                Need::Yank(text) => {
                    // OSC 52 on the host plus clipboard.set for everyone else (§5.1).
                    self.effects.push(Effect::Clipboard(text.clone()));
                    self.last_sent_clipboard = Some(text.clone());
                    self.clipboard = Some(text.clone());
                    self.quiet(m::ClipboardSet { text });
                }
                Need::Exit => self.leave_copy(),
            }
        }
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
                let hint = if prefix {
                    format!("{} {chord}", self.prefix_label())
                } else {
                    chord.to_string()
                };
                items.push(PaletteItem {
                    label: action.describe(),
                    hint,
                    run: PaletteRun::Action(*action),
                });
            }
        }
        for a in crate::bindings::ALL {
            if !a.is_copy() && seen.insert(*a) {
                items.push(PaletteItem {
                    label: a.describe(),
                    hint: String::new(),
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
        let strip_row = match self.cfg.tabs {
            TabsPosition::Top => Some(0),
            TabsPosition::Bottom => Some(self.rows.saturating_sub(2)),
            TabsPosition::Hidden => None,
        };
        if Some(y) == strip_row {
            return self
                .tab_hits
                .iter()
                .find(|(x0, x1, _)| x >= *x0 && x < *x1)
                .map(|(_, _, t)| Hit::Tab(t.clone()))
                .unwrap_or(Hit::Nothing);
        }
        let oy = self.top_chrome();
        if y < oy || y >= oy + self.view_size().1 {
            return Hit::Nothing;
        }
        let (px, py) = (x, y - oy);
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
        match (self.hit(x, y), kind) {
            (Hit::Tab(t), MouseKind::Press(_)) => {
                let view_id = self.model.view_id.clone();
                self.quiet(m::TabSelect { view_id, tab_id: t });
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
                        view_id,
                        pane_id: Some(id.clone()),
                        dir: None,
                    });
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
                        let oy = self.top_chrome();
                        (
                            x.saturating_sub(r.x).min(r.w.saturating_sub(1)),
                            y.saturating_sub(oy + r.y).min(r.h.saturating_sub(1)),
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
