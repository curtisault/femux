//! The plugin surfaces (REQ-PLUG-002): `DaemonPlugin` (event subscriber + metadata writer),
//! `ClientPlugin` (info modules, palette actions, keybindings, overlays) and `CliPlugin`
//! (subcommands). A plugin implements any subset and talks to the core only through
//! `femux-proto` (REQ-PLUG-003), so it can also run out of process.

use std::path::Path;
use std::time::Duration;

pub use femux_proto as proto;
use femux_proto::msg as m;
use femux_proto::{ErrorBody, Event, Request, Response};

/// Built-in plugins and the milestone each ships in (REQ-PLUG-004).
pub const BUILT_IN: &[(&str, &str)] = &[
    ("agents", "0.5"),
    ("infobar", "0.4"),
    ("notify", "0.5"),
    ("git", "0.8"),
    ("clipboard", "0.2"),
    ("graphics", "0.7"),
    ("shell-integration", "0.2"),
    ("broadcast", "0.8"),
    ("which-key", "0.2"),
    ("recorder", "0.8"),
    ("hosts", "0.6"),
    ("theme", "0.4"),
    ("launcher", "0.3"),
];

/// Semantic text styles; the client maps them to its theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    #[default]
    Text,
    Muted,
    Key,
    Title,
    Accent,
    Warn,
    /// Success, e.g. an agent that is done.
    Ok,
    Error,
    /// Text on the accent color, e.g. the session chip.
    Chip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    pub fn new(text: impl Into<String>, style: Style) -> Span {
        Span {
            text: text.into(),
            style,
        }
    }
    pub fn text(text: impl Into<String>) -> Span {
        Span::new(text, Style::Text)
    }
}

pub type Spans = Vec<Span>;

/// Display width of a line of spans (one column per char; wide glyphs are rare in chrome).
pub fn width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.text.chars().count()).sum()
}

/// One tab as the info bar sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TabItem {
    pub index: usize,
    pub title: String,
    pub current: bool,
    pub urgent: bool,
}

/// A notification whose pane is not on screen (REQ-UI-033), newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteItem {
    pub title: String,
    pub level: m::NotifyLevel,
}

/// What an info module may read about the client (REQ-UI-031). Rebuilt for every frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BarCtx {
    pub session: String,
    pub tabs: Vec<TabItem>,
    /// `agent.state` value -> number of panes of the session in it.
    pub agents: std::collections::BTreeMap<String, usize>,
    /// The focused pane's working directory.
    pub cwd: Option<String>,
    pub notifications: Vec<NoteItem>,
    /// The bar is a column (`left`/`right`): modules may use several lines (`\n` in a span).
    pub vertical: bool,
}

/// An info bar module (REQ-UI-031): the bar calls `sample` every `interval` to refresh what
/// the module measures, and `render` for every frame.
pub trait InfoModule: Send {
    fn name(&self) -> &str;
    #[allow(unused_variables)]
    fn sample(&mut self, ctx: &BarCtx) {}
    /// The module's text in at most `w` columns; nothing to show is an empty `Spans`.
    fn render(&self, ctx: &BarCtx, w: usize) -> Spans;
    fn interval(&self) -> Duration;
}

/// A color theme a plugin supplies by name (`[theme] name`); colors are `[r, g, b]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub dark: bool,
    pub accent: [u8; 3],
    pub accent_fg: [u8; 3],
    pub muted: [u8; 3],
    pub warn: [u8; 3],
    pub ok: [u8; 3],
    pub error: [u8; 3],
    pub text: [u8; 3],
    pub bar_bg: [u8; 3],
    pub bar_fg: [u8; 3],
    pub selection: [u8; 3],
    pub overlay_bg: [u8; 3],
}

/// An action offered to the command palette (REQ-UI-043).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteAction {
    pub id: String,
    pub label: String,
}

/// Which key table a binding lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    Root,
    Prefix,
    Copy,
}

/// A keybinding contributed by a plugin: `key` is a chord like `ctrl+p`, `action` an action name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub table: Table,
    pub key: String,
    pub action: String,
}

/// One bound key as the client describes it to overlays: key, action name, group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundKey {
    pub key: String,
    pub action: String,
    pub group: String,
}

/// What a plugin may read when asked for an overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayCtx<'a> {
    /// `terminal`, `prefix`, `normal` or `copy`.
    pub mode: &'a str,
    pub width: usize,
    pub height: usize,
    /// The prefix table, in display order.
    pub prefix_table: &'a [BoundKey],
    /// How the prefix is typed, e.g. `ctrl+space`.
    pub prefix_key: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Bottom,
    Center,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    pub title: Spans,
    pub lines: Vec<Spans>,
    pub place: Place,
}

/// Something a client plugin wants done on the host terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEffect {
    /// Copy text to the host clipboard (OSC 52).
    Clipboard(String),
    /// Ring the host terminal's bell.
    Bell,
    /// Play a sound file on the client's machine.
    Sound(String),
    /// A desktop notification (OSC 777 / OSC 99 / native).
    Desktop { title: String, body: String },
}

#[allow(unused_variables)]
pub trait ClientPlugin: Send {
    fn name(&self) -> &'static str;
    fn modules(&self) -> Vec<Box<dyn InfoModule>> {
        Vec::new()
    }
    fn actions(&self) -> Vec<PaletteAction> {
        Vec::new()
    }
    /// Runs a palette action by id; returns requests for the daemon.
    fn run_action(&mut self, id: &str) -> Vec<Request> {
        Vec::new()
    }
    fn bindings(&self) -> Vec<Binding> {
        Vec::new()
    }
    fn overlay(&self, ctx: &OverlayCtx) -> Option<Overlay> {
        None
    }
    fn on_event(&mut self, ev: &Event) -> Vec<HostEffect> {
        Vec::new()
    }
    /// The theme called `name`, if this plugin knows it: `Some(Err)` names what is wrong.
    fn theme(&self, name: &str) -> Option<Result<Palette, String>> {
        None
    }
}

#[allow(unused_variables)]
pub trait DaemonPlugin: Send {
    fn name(&self) -> &'static str;
    /// The events to subscribe to (default: everything).
    fn subscription(&self) -> m::EventsSubscribe {
        m::EventsSubscribe::default()
    }
    /// Requests to send once connected.
    fn start(&mut self) -> Vec<Request> {
        Vec::new()
    }
    /// Requests to send in reaction to an event (typically `pane.set_meta`).
    fn on_event(&mut self, ev: &Event) -> Vec<Request>;
    /// How often [`DaemonPlugin::tick`] runs, if at all.
    fn tick_every(&self) -> Option<Duration> {
        None
    }
    /// Periodic work; `now_ms` is ms since epoch.
    fn tick(&mut self, now_ms: u64) -> Vec<Request> {
        Vec::new()
    }
    /// The answer to a request this plugin sent (errors are dropped).
    fn on_response(&mut self, req: &Request, resp: &Response) -> Vec<Request> {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArg {
    pub name: &'static str,
    pub help: &'static str,
    pub required: bool,
}

/// A `--name` option of a plugin subcommand; with `value`, it takes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliFlag {
    pub name: &'static str,
    pub help: &'static str,
    pub value: bool,
}

/// A subcommand contributed by a plugin. Every subcommand gets `--json` (REQ-CLI-001).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliCommand {
    pub name: &'static str,
    pub about: &'static str,
    pub args: Vec<CliArg>,
    pub flags: Vec<CliFlag>,
    /// Values after `--`, under this name in help.
    pub trailing: Option<&'static str>,
}

/// What the user typed for a plugin subcommand.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliInput {
    /// Positional values in the order the command declared them (absent optional ones left out).
    pub args: Vec<String>,
    /// Flags given: `Some(value)` for valued flags, `None` for switches.
    pub flags: std::collections::BTreeMap<String, Option<String>>,
    /// Values after `--`.
    pub trailing: Vec<String>,
}

impl CliInput {
    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|v| v.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliOutput {
    /// Human output.
    pub text: String,
    /// `--json` output.
    pub json: serde_json::Value,
    /// Attach the client to this session afterwards, when run from a terminal outside femux.
    pub attach: Option<String>,
}

/// What the CLI host offers a plugin subcommand.
pub trait CliCore {
    fn request(&mut self, req: Request) -> Result<Response, ErrorBody>;
    /// The pane the command runs in (`$FEMUX_PANE`, REQ-CLI-002), if any.
    fn pane(&self) -> Option<String>;
    fn socket(&self) -> &Path;
    /// Reads all of stdin.
    fn stdin(&mut self) -> String;
    /// Starts the daemon if none is running (as `femux new` does).
    fn start_daemon(&mut self) -> Result<(), String>;
    /// The next event of the subscription made with `events.subscribe`, if one arrives within
    /// `timeout` (`None`: wait for ever).
    fn next_event(&mut self, timeout: Option<Duration>) -> Option<Event>;
}

pub trait CliPlugin: Send {
    fn name(&self) -> &'static str;
    fn commands(&self) -> Vec<CliCommand>;
    fn run(&self, cmd: &str, input: &CliInput, core: &mut dyn CliCore)
    -> Result<CliOutput, String>;
}

/// A built-in plugin: a name plus whichever surfaces it implements.
#[derive(Default)]
pub struct Plugin {
    pub name: &'static str,
    pub client: Option<Box<dyn ClientPlugin>>,
    pub daemon: Option<Box<dyn DaemonPlugin>>,
    pub cli: Option<Box<dyn CliPlugin>>,
}

impl Plugin {
    pub fn new(name: &'static str) -> Plugin {
        Plugin {
            name,
            ..Default::default()
        }
    }
}

/// Runs a daemon plugin against the daemon at `socket` over the protocol: handshake,
/// `events.subscribe`, then a request per `on_event` result. Returns when the connection
/// closes. In-process plugins and out-of-process ones use the same loop.
pub async fn run_daemon_plugin(
    socket: &Path,
    mut plugin: Box<dyn DaemonPlugin>,
) -> std::io::Result<()> {
    use femux_proto::io_async::read_frame;
    use femux_proto::{ServerMsg, decode_envelope};
    use tokio::io::AsyncWriteExt;

    let stream = tokio::net::UnixStream::connect(socket).await?;
    let (mut rd, mut wr) = stream.into_split();
    let mut next_id = 1u64;
    // Requests awaiting an answer, by id, for on_response.
    let mut pending: std::collections::HashMap<u64, Request> = std::collections::HashMap::new();
    let send = async |wr: &mut tokio::net::unix::OwnedWriteHalf,
                      req: Request,
                      next_id: &mut u64,
                      pending: &mut std::collections::HashMap<u64, Request>|
           -> std::io::Result<()> {
        let bytes = req.encode(*next_id)?;
        pending.insert(*next_id, req);
        *next_id += 1;
        wr.write_all(&bytes).await
    };
    let hello = m::Hello {
        proto: femux_proto::PROTO_VERSION,
        client: format!(
            "femux-plugin-{}/{}",
            plugin.name(),
            env!("CARGO_PKG_VERSION")
        ),
        features: [
            "events",
            "pane",
            "session",
            "tab",
            "layout",
            "scrollback",
            "clipboard",
            "notify",
            "plugin",
            "server",
        ]
        .map(String::from)
        .to_vec(),
        term: None,
    };
    send(&mut wr, hello.into(), &mut next_id, &mut pending).await?;
    send(
        &mut wr,
        plugin.subscription().into(),
        &mut next_id,
        &mut pending,
    )
    .await?;
    for req in plugin.start() {
        send(&mut wr, req, &mut next_id, &mut pending).await?;
    }
    // Frames are read on their own task: reading is not cancel-safe, ticks are.
    let (frames_tx, mut frames) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(Some(frame)) = read_frame(&mut rd).await {
            if frames_tx.send(frame).is_err() {
                return;
            }
        }
    });
    let every = plugin.tick_every();
    let mut tick = tokio::time::interval(every.unwrap_or(Duration::from_secs(3600)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let reqs = tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { return Ok(()) };
                let Ok(msg) = decode_envelope(&frame).and_then(ServerMsg::decode) else {
                    continue;
                };
                match msg {
                    ServerMsg::Event(ev) => plugin.on_event(&ev),
                    ServerMsg::Response(id, resp) => match pending.remove(&id) {
                        Some(req) => plugin.on_response(&req, &resp),
                        None => Vec::new(),
                    },
                    ServerMsg::Error(id, _) => {
                        pending.remove(&id);
                        Vec::new()
                    }
                    ServerMsg::Push(_) => Vec::new(),
                }
            }
            _ = tick.tick(), if every.is_some() => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                plugin.tick(now)
            }
        };
        for req in reqs {
            send(&mut wr, req, &mut next_id, &mut pending).await?;
        }
    }
}
