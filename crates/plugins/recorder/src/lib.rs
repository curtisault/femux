//! `recorder` plugin (0.8): asciinema v2 recordings of a pane's output, from the raw
//! `pane.output` stream (ADR-022).
//!
//! - `prefix R` (the client's `record` action) sets or clears the focused pane's
//!   `recorder.on`; the daemon side records every pane that has it into
//!   `<state dir>/recordings/` and puts the file in `recorder.file`.
//! - `femux record [pane] [file] [--duration s]` records in the foreground until the pane
//!   exits, the duration passes or it is interrupted.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use femux_plugin::{
    Binding, CliArg, CliCommand, CliCore, CliFlag, CliInput, CliOutput, CliPlugin, ClientPlugin,
    DaemonPlugin, Plugin, Table,
};
use femux_proto::msg as m;
use femux_proto::{Event, Request, Response};
use serde_json::json;

pub fn plugin() -> Plugin {
    Plugin {
        name: "recorder",
        client: Some(Box::new(Client)),
        daemon: Some(Box::new(Recorder::new(recordings_dir()))),
        cli: Some(Box::new(Cli)),
    }
}

/// The client side alone: the `prefix R` binding.
pub fn client() -> Box<dyn ClientPlugin> {
    Box::new(Client)
}

/// Where recordings go by default.
pub fn recordings_dir() -> PathBuf {
    femux_config::state_dir().join("recordings")
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A file name for a new recording of `pane`.
pub fn default_file(dir: &Path, pane: &str) -> PathBuf {
    let short: String = pane
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    dir.join(format!("femux-{}-{short}.cast", epoch_secs()))
}

/// One asciinema v2 file being written.
pub struct Cast {
    out: BufWriter<File>,
    start: Instant,
    /// The tail of a UTF-8 sequence split across two reads.
    pending: Vec<u8>,
    pub path: PathBuf,
}

impl Cast {
    /// Creates `path` with the v2 header for a `cols`x`rows` terminal.
    pub fn create(path: &Path, cols: u16, rows: u16) -> std::io::Result<Cast> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = BufWriter::new(File::create(path)?);
        let header = json!({
            "version": 2,
            "width": cols,
            "height": rows,
            "timestamp": epoch_secs(),
            "env": { "TERM": "xterm-256color" },
        });
        writeln!(out, "{header}")?;
        out.flush()?;
        Ok(Cast {
            out,
            start: Instant::now(),
            pending: Vec::new(),
            path: path.to_owned(),
        })
    }

    /// Appends output bytes as an `"o"` event; UTF-8 split across reads waits for its end.
    pub fn output(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.pending.extend_from_slice(bytes);
        let text = match std::str::from_utf8(&self.pending) {
            Ok(s) => {
                let s = s.to_owned();
                self.pending.clear();
                s
            }
            Err(e) if e.error_len().is_none() => {
                // An incomplete sequence at the end: keep it for the next read.
                let valid = e.valid_up_to();
                let s = String::from_utf8_lossy(&self.pending[..valid]).into_owned();
                self.pending.drain(..valid);
                s
            }
            Err(_) => {
                let s = String::from_utf8_lossy(&self.pending).into_owned();
                self.pending.clear();
                s
            }
        };
        if text.is_empty() {
            return Ok(());
        }
        let t = self.start.elapsed().as_secs_f64();
        writeln!(self.out, "{}", json!([(t * 1e6).round() / 1e6, "o", text]))?;
        self.out.flush()
    }
}

fn decode(data: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap_or_default()
}

struct Client;

impl ClientPlugin for Client {
    fn name(&self) -> &'static str {
        "recorder"
    }
    fn bindings(&self) -> Vec<Binding> {
        vec![Binding {
            table: Table::Prefix,
            key: "R".into(),
            action: "record".into(),
        }]
    }
}

// ---------------------------------------------------------------------------- daemon

/// Records every pane whose `recorder.on` is set.
pub struct Recorder {
    dir: PathBuf,
    casts: HashMap<String, Cast>,
    /// Panes waiting for their size (`pane.info`) before their file starts.
    starting: Vec<String>,
}

impl Recorder {
    pub fn new(dir: PathBuf) -> Recorder {
        Recorder {
            dir,
            casts: HashMap::new(),
            starting: Vec::new(),
        }
    }

    /// The subscription for the current recordings: raw output only while one runs.
    fn subscribe(&self) -> Request {
        let mut kinds = vec!["pane.meta_changed".to_owned(), "pane.exited".to_owned()];
        if !self.casts.is_empty() || !self.starting.is_empty() {
            kinds.push("pane.output".to_owned());
        }
        m::EventsSubscribe {
            kinds: Some(kinds),
            ..Default::default()
        }
        .into()
    }

    fn stop(&mut self, pane: &str) -> Vec<Request> {
        self.starting.retain(|p| p != pane);
        let Some(cast) = self.casts.remove(pane) else {
            return Vec::new();
        };
        vec![
            m::PaneSetMeta {
                pane_id: pane.to_owned(),
                set: Some(
                    [(
                        "recorder.last".to_owned(),
                        cast.path.to_string_lossy().into_owned(),
                    )]
                    .into(),
                ),
                unset: Some(vec!["recorder.file".to_owned()]),
            }
            .into(),
            self.subscribe(),
        ]
    }
}

impl DaemonPlugin for Recorder {
    fn name(&self) -> &'static str {
        "recorder"
    }

    fn subscription(&self) -> m::EventsSubscribe {
        m::EventsSubscribe {
            kinds: Some(vec!["pane.meta_changed".into(), "pane.exited".into()]),
            ..Default::default()
        }
    }

    fn on_event(&mut self, ev: &Event) -> Vec<Request> {
        match ev {
            Event::PaneMetaChanged(c) => {
                if c.set.contains_key("recorder.on")
                    && !self.casts.contains_key(&c.pane_id)
                    && !self.starting.contains(&c.pane_id)
                {
                    // Output is subscribed first, so nothing between the info and the file
                    // is lost; it is written once the size is known.
                    self.starting.push(c.pane_id.clone());
                    return vec![
                        self.subscribe(),
                        m::PaneInfo {
                            pane_id: c.pane_id.clone(),
                        }
                        .into(),
                    ];
                }
                if c.unset.iter().any(|k| k == "recorder.on") {
                    return self.stop(&c.pane_id);
                }
                Vec::new()
            }
            Event::PaneOutput(o) => {
                if let Some(cast) = self.casts.get_mut(&o.pane_id) {
                    let _ = cast.output(&decode(&o.data));
                }
                Vec::new()
            }
            Event::PaneExited(e) => self.stop(&e.pane_id),
            _ => Vec::new(),
        }
    }

    fn on_response(&mut self, _req: &Request, resp: &Response) -> Vec<Request> {
        let Response::PaneInfo(i) = resp else {
            return Vec::new();
        };
        if !self.starting.contains(&i.id) {
            return Vec::new();
        }
        self.starting.retain(|p| *p != i.id);
        let path = default_file(&self.dir, &i.id);
        let (cols, rows) = (i.cols.unwrap_or(80), i.rows.unwrap_or(24));
        match Cast::create(&path, cols, rows) {
            Ok(cast) => {
                self.casts.insert(i.id.clone(), cast);
                vec![
                    m::PaneSetMeta {
                        pane_id: i.id.clone(),
                        set: Some(
                            [(
                                "recorder.file".to_owned(),
                                path.to_string_lossy().into_owned(),
                            )]
                            .into(),
                        ),
                        unset: None,
                    }
                    .into(),
                ]
            }
            Err(_) => vec![
                m::PaneSetMeta {
                    pane_id: i.id.clone(),
                    set: None,
                    unset: Some(vec!["recorder.on".to_owned()]),
                }
                .into(),
                self.subscribe(),
            ],
        }
    }
}

// ---------------------------------------------------------------------------- cli

struct Cli;

impl CliPlugin for Cli {
    fn name(&self) -> &'static str {
        "recorder"
    }

    fn commands(&self) -> Vec<CliCommand> {
        vec![CliCommand {
            name: "record",
            about: "Record a pane as an asciinema v2 file until it exits or you stop",
            args: vec![
                CliArg {
                    name: "pane",
                    help: "Pane id (default: this pane)",
                    required: false,
                },
                CliArg {
                    name: "file",
                    help: "The .cast file (default: in the state directory's recordings/)",
                    required: false,
                },
            ],
            flags: vec![CliFlag {
                name: "duration",
                help: "Stop after this many seconds",
                value: true,
            }],
            ..Default::default()
        }]
    }

    fn run(
        &self,
        _cmd: &str,
        input: &CliInput,
        core: &mut dyn CliCore,
    ) -> Result<CliOutput, String> {
        let pane = input
            .args
            .first()
            .cloned()
            .or_else(|| core.pane())
            .ok_or("no pane: run inside femux or name one")?;
        let duration = input
            .value("duration")
            .map(|d| {
                d.trim_end_matches('s')
                    .parse::<f64>()
                    .map(Duration::from_secs_f64)
                    .map_err(|_| format!("--duration {d:?} is not a number of seconds"))
            })
            .transpose()?;
        core.request(
            m::EventsSubscribe {
                kinds: Some(vec!["pane.output".into(), "pane.exited".into()]),
                pane: Some(pane.clone()),
                ..Default::default()
            }
            .into(),
        )
        .map_err(|e| e.message)?;
        let info = match core
            .request(
                m::PaneInfo {
                    pane_id: pane.clone(),
                }
                .into(),
            )
            .map_err(|e| e.message)?
        {
            Response::PaneInfo(i) => i,
            other => return Err(format!("unexpected answer {other:?}")),
        };
        let path = input
            .args
            .get(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| default_file(&recordings_dir(), &pane));
        let (cols, rows) = (info.cols.unwrap_or(80), info.rows.unwrap_or(24));
        let mut cast =
            Cast::create(&path, cols, rows).map_err(|e| format!("{}: {e}", path.display()))?;
        let deadline = duration.map(|d| Instant::now() + d);
        loop {
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if left.is_some_and(|l| l.is_zero()) {
                break;
            }
            match core.next_event(left) {
                Some(Event::PaneOutput(o)) => {
                    cast.output(&decode(&o.data))
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                }
                Some(Event::PaneExited(_)) => break,
                Some(_) => {}
                None if deadline.is_some() => break,
                None => return Err("the daemon went away".into()),
            }
        }
        Ok(CliOutput {
            text: path.display().to_string(),
            json: json!({ "pane_id": pane, "file": path }),
            attach: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn casts_are_asciinema_v2_and_keep_utf8_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.cast");
        let mut c = Cast::create(&path, 80, 24).unwrap();
        c.output(b"hi \xe2\x82").unwrap(); // "€" split across two reads
        c.output(b"\xac!\r\n").unwrap();
        drop(c);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["version"], 2);
        assert_eq!(
            (lines[0]["width"].as_u64(), lines[0]["height"].as_u64()),
            (Some(80), Some(24))
        );
        assert_eq!(lines[1][1], "o");
        assert_eq!(lines[1][2], "hi ");
        assert_eq!(lines[2][2], "€!\r\n");
        assert!(lines[2][0].as_f64().unwrap() >= lines[1][0].as_f64().unwrap());
    }
}
