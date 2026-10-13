//! Snapshots and restore (REQ-STORE-003..006, REQ-PTY-005, REQ-LIFE-001) and `server.reload`.
//! The data model lives in [`crate::store`]; this is what the daemon puts in and takes out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use femux_proto::msg as m;
use femux_vt::{Cell, Color, attrs};

use crate::bus::{Scope, now_ms};
use crate::daemon::{CLEAN, Daemon, SpawnOpts, scheme};
use crate::layout::{Node, Rect};
use crate::state::{R, Session, Tab, invalid};
use crate::store::{PaneRow, SessionRow, Snapshot, Store, TabRow};

fn internal(e: impl std::fmt::Display) -> (m::ErrorCode, String) {
    (m::ErrorCode::Internal, e.to_string())
}

fn sgr_color(out: &mut String, c: Color, base: u8) {
    match c {
        Color::Default => {}
        Color::Indexed(i) => out.push_str(&format!(";{base};5;{i}")),
        Color::Rgb(r, g, b) => out.push_str(&format!(";{base};2;{r};{g};{b}")),
    }
}

/// The SGR sequence that sets `c`'s style from a reset state.
fn sgr(c: &Cell) -> String {
    let mut s = String::from("\x1b[0");
    for (bit, code) in [
        (attrs::BOLD, "1"),
        (attrs::FAINT, "2"),
        (attrs::ITALIC, "3"),
        (attrs::BLINK, "5"),
        (attrs::INVERSE, "7"),
        (attrs::INVISIBLE, "8"),
        (attrs::STRIKETHROUGH, "9"),
        (attrs::OVERLINE, "53"),
    ] {
        if c.attrs & bit != 0 {
            s.push(';');
            s.push_str(code);
        }
    }
    let ul = (c.attrs & attrs::UNDERLINE_MASK) >> attrs::UNDERLINE_SHIFT;
    if ul != 0 {
        s.push_str(&format!(";4:{ul}"));
    }
    sgr_color(&mut s, c.fg, 38);
    sgr_color(&mut s, c.bg, 48);
    s.push('m');
    s
}

/// Lines of cells as bytes that redraw them in a terminal: text with SGR styles, one line per
/// row, trailing blanks dropped (REQ-STORE-006).
pub fn encode_history(lines: &[Vec<Cell>]) -> Vec<u8> {
    let mut out = String::new();
    let plain = Cell::default();
    for line in lines {
        let end = line
            .iter()
            .rposition(|c| !c.text.is_empty() || c.bg != Color::Default || c.attrs != 0)
            .map_or(0, |i| i + 1);
        let mut style = sgr(&plain);
        for c in &line[..end] {
            if c.width == 0 {
                continue;
            }
            let cs = sgr(c);
            if cs != style {
                out.push_str(&cs);
                style = cs;
            }
            out.push_str(if c.text.is_empty() { " " } else { &c.text });
        }
        if style != sgr(&plain) {
            out.push_str("\x1b[0m");
        }
        out.push_str("\r\n");
    }
    out.into_bytes()
}

impl Daemon {
    /// The current state as a [`Snapshot`]. Pane history is read after the state lock is
    /// released, so a busy pane never holds up requests (REQ-PTY-003).
    pub fn build_snapshot(&self) -> Snapshot {
        let (keep_scrollback, lines) = {
            let s = self.settings.lock().unwrap();
            (s.restore.scrollback, s.restore.scrollback_lines)
        };
        let st = self.state.lock().unwrap();
        let mut snap = Snapshot {
            ts: now_ms(),
            ..Default::default()
        };
        let mut terminals = Vec::new();
        for s in st.sessions.values() {
            snap.sessions.push(SessionRow {
                name: s.name.clone(),
                root: s.root.to_string_lossy().into_owned(),
                file: s.file.clone(),
                created: s.created,
            });
            let mut marks: BTreeMap<&str, String> = BTreeMap::new();
            for (letter, pane) in &s.marks {
                marks.entry(pane.as_str()).or_default().push(*letter);
            }
            for (index, tab_id) in s.tabs.iter().enumerate() {
                let Some(t) = st.tabs.get(tab_id) else {
                    continue;
                };
                snap.tabs.push(TabRow {
                    id: t.id.clone(),
                    session: s.name.clone(),
                    index: index as u32,
                    title: t.title.clone(),
                    tree: t.tree.as_ref().map(Node::to_proto),
                    floats: t.floats_proto(),
                    focus: t.focus.clone(),
                    zoomed: t.zoomed,
                    scheme: t.scheme,
                    size: t.size,
                });
                for pid in t.panes() {
                    let Some(p) = st.panes.get(&pid) else {
                        continue;
                    };
                    if keep_scrollback {
                        terminals.push((snap.panes.len(), p.vt.clone()));
                    }
                    snap.panes.push(PaneRow {
                        id: p.id.clone(),
                        tab: t.id.clone(),
                        cwd: p.cwd.clone(),
                        cmd: p.cmd.clone(),
                        env: p.saved_env.clone(),
                        restore: p.restore,
                        size: p.size,
                        marks: marks.get(p.id.as_str()).cloned().unwrap_or_default(),
                        meta: p.meta.clone(),
                        scrollback: None,
                    });
                }
            }
        }
        {
            let bus = self.bus.lock().unwrap();
            snap.seq = bus.seq();
            snap.events = bus
                .retained()
                .into_iter()
                .map(|(e, sc)| (e, sc.session, sc.pane))
                .collect();
        }
        drop(st);
        for (i, vt) in terminals {
            let mut vt = vt.lock().unwrap();
            let rows = vt.size().1 as i64;
            let from = (rows - lines as i64).max(vt.first_line());
            let count = (rows - from).max(0) as u32;
            if let Ok(mut l) = vt.read_lines(from, count) {
                while l
                    .last()
                    .is_some_and(|r| r.iter().all(|c| c.text.is_empty()))
                {
                    l.pop();
                }
                snap.panes[i].scrollback = Some(encode_history(&l));
            }
        }
        snap
    }

    /// Writes a snapshot now (REQ-STORE-003, `store.snapshot`); returns its time.
    pub fn snapshot_now(&self) -> R<u64> {
        if self.store.lock().unwrap().is_none() {
            return Err(invalid("this daemon keeps no store"));
        }
        self.store_dirty.store(CLEAN, Ordering::SeqCst);
        let snap = self.build_snapshot();
        {
            let mut store = self.store.lock().unwrap();
            let Some(store) = store.as_mut() else {
                return Err(invalid("this daemon keeps no store"));
            };
            store.write(&snap).map_err(internal)?;
        }
        self.emit(m::StoreSnapshotted::default(), Scope::default());
        Ok(snap.ts)
    }

    /// The exit snapshot, taken once (REQ-LIFE-001).
    pub fn exit_snapshot(&self) {
        if !self.exit_snapshot_done.swap(true, Ordering::SeqCst)
            && self.store.lock().unwrap().is_some()
        {
            let _ = self.snapshot_now();
        }
    }

    /// Opens the store; with `[restore] enabled`, reopens the saved sessions (REQ-STORE-004).
    /// Returns where an unloadable store was backed up, if it was (REQ-STORE-007).
    pub fn open_store(self: &Arc<Self>) -> Option<PathBuf> {
        let path = self.cfg.db.clone()?;
        let opened = Store::open(&path, &self.cfg.name).ok()?;
        *self.store.lock().unwrap() = Some(opened.store);
        if let Some(saved) = opened.saved {
            let events = saved
                .events
                .iter()
                .map(|(e, session, pane)| {
                    (
                        e.clone(),
                        Scope {
                            session: session.clone(),
                            pane: pane.clone(),
                        },
                    )
                })
                .collect();
            self.bus.lock().unwrap().seed(saved.seq, events);
            if self.settings.lock().unwrap().restore.enabled {
                self.restore(saved);
            }
        }
        opened.backup
    }

    /// Recreates saved sessions, tabs and panes with their ids (REQ-PTY-005). Panes come back
    /// as shells in their cwd marked `restored`, except those with `restore` set, whose `cmd`
    /// is re-run, staggered (REQ-STORE-004/005).
    fn restore(self: &Arc<Self>, saved: Snapshot) {
        let stagger = self.settings.lock().unwrap().restore.stagger_ms;
        let mut reruns = 0u64;
        let mut st = self.state.lock().unwrap();
        for s in &saved.sessions {
            st.sessions.insert(
                s.name.clone(),
                Session {
                    name: s.name.clone(),
                    root: PathBuf::from(&s.root),
                    created: s.created,
                    tabs: Vec::new(),
                    marks: BTreeMap::new(),
                    host: None,
                    file: s.file.clone(),
                },
            );
        }
        for t in &saved.tabs {
            if !st.sessions.contains_key(&t.session) {
                continue;
            }
            st.tabs.insert(
                t.id.clone(),
                Tab {
                    id: t.id.clone(),
                    session: t.session.clone(),
                    title: t.title.clone(),
                    tree: None,
                    floats: Vec::new(),
                    zoomed: t.zoomed,
                    focus: t.focus.clone(),
                    scheme: t.scheme,
                    size: t.size,
                    rects: Vec::new(),
                    layout_gen: 0,
                    gap: self.settings.lock().unwrap().gaps,
                },
            );
            let mut spawned = Vec::new();
            for p in saved.panes.iter().filter(|p| p.tab == t.id) {
                let rerun = p.restore && p.cmd.is_some();
                let opts = SpawnOpts {
                    id: Some(p.id.clone()),
                    restore: p.restore,
                    restored: !rerun,
                    extra_env: p.env.clone().into_iter().collect(),
                    meta: p.meta.clone(),
                    history: p.scrollback.clone(),
                    delay: rerun.then(|| {
                        let d = Duration::from_millis(stagger * reruns);
                        reruns += 1;
                        d
                    }),
                };
                let cmd = if rerun { p.cmd.clone() } else { None };
                if self
                    .spawn_pane_with(&mut st, &t.id, Path::new(&p.cwd), cmd, p.size, opts)
                    .is_ok()
                {
                    // A shell that replaced a command keeps the command for the record only
                    // when it will be re-run; otherwise the pane is just a shell now.
                    if rerun {
                        st.panes.get_mut(&p.id).unwrap().cmd = p.cmd.clone();
                    }
                    spawned.push(p.id.clone());
                    if let Some(s) = st.sessions.get_mut(&t.session) {
                        for letter in p.marks.chars() {
                            s.marks.insert(letter, p.id.clone());
                        }
                    }
                }
            }
            // Keep the saved shape, minus any pane that could not come back.
            let mut tree = t.tree.as_ref().and_then(|n| Node::from_proto(n).ok());
            if let Some(n) = tree.take() {
                tree = n
                    .panes()
                    .iter()
                    .filter(|p| !spawned.contains(p))
                    .try_fold(n.clone(), |acc, gone| acc.remove(gone).ok_or(()))
                    .ok();
            }
            let floats: Vec<(String, Rect)> = t
                .floats
                .iter()
                .filter(|f| spawned.contains(&f.id))
                .map(|f| (f.id.clone(), Rect::from(&f.rect)))
                .collect();
            let tab = st.tabs.get_mut(&t.id).unwrap();
            tab.tree = tree;
            tab.floats = floats;
            let panes = tab.panes();
            if panes.is_empty() {
                st.tabs.remove(&t.id);
                continue;
            }
            if !panes.contains(&tab.focus) {
                tab.focus = panes[0].clone();
            }
            if let Some(s) = st.sessions.get_mut(&t.session) {
                s.tabs.push(t.id.clone());
            }
            self.apply_geometry(&mut st, &t.id);
        }
        st.sessions.retain(|_, s| !s.tabs.is_empty());
        if !st.sessions.is_empty() {
            st.had_session = true;
        }
    }

    /// `server.reload` (ADR-013, ADR-017 #3): re-reads the config file and applies what can
    /// change while running. An invalid file changes nothing.
    pub fn reload(self: &Arc<Self>) -> R<()> {
        if let Some(path) = &self.cfg.config_path {
            let c = femux_config::Config::load(path).map_err(invalid)?;
            let gaps = c.layout.gaps;
            {
                let mut s = self.settings.lock().unwrap();
                s.keep_alive = c.daemon.keep_alive;
                s.scrollback_lines = c.terminal.scrollback_lines;
                s.restore = c.restore;
                s.env_allow = c.env.allow;
                s.gaps = gaps;
                s.scheme = scheme(c.layout.scheme);
            }
            // Gaps apply to every tab at once; the scheme only to new tabs.
            let mut st = self.state.lock().unwrap();
            let changed: Vec<String> = st
                .tabs
                .values_mut()
                .filter(|t| t.gap != gaps)
                .map(|t| {
                    t.gap = gaps;
                    t.id.clone()
                })
                .collect();
            let redraw = !changed.is_empty();
            for id in changed {
                self.layout_changed(&mut st, &id);
            }
            drop(st);
            // Reload returns before the request path's own redraw: send the new geometry.
            if redraw {
                self.dirty();
            }
        }
        self.emit(m::ConfigReloaded::default(), Scope::default());
        Ok(())
    }
}

/// Takes snapshots on its triggers until shutdown (REQ-STORE-003): a layout change (after the
/// debounce, coalescing changes that arrive meanwhile) and a periodic tick while dirty.
pub async fn store_task(d: Arc<Daemon>) {
    let mut shutdown = d.shutdown.subscribe();
    let mut tick = tokio::time::interval(d.cfg.snapshot_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        let kicked = tokio::select! {
            _ = shutdown.wait_for(|s| *s) => return,
            _ = d.store_kick.notified() => true,
            _ = tick.tick() => false,
        };
        if kicked {
            // Coalesce the changes that follow a layout change.
            tokio::select! {
                _ = shutdown.wait_for(|s| *s) => return,
                _ = tokio::time::sleep(d.cfg.snapshot_debounce) => {}
            }
        }
        if d.store_dirty.load(Ordering::SeqCst) == CLEAN || *d.shutdown.borrow() {
            continue;
        }
        let d2 = d.clone();
        let _ = tokio::task::spawn_blocking(move || d2.snapshot_now()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_bytes_redraw_text_and_styles() {
        let mut t = femux_vt::Terminal::new(20, 3, 100).unwrap();
        t.write(b"plain \x1b[1;31mred\x1b[0m\r\n\x1b[38;2;1;2;3mrgb\x1b[0m");
        let lines = t.read_lines(0, 3).unwrap();
        let bytes = encode_history(&lines[..2]);
        let mut u = femux_vt::Terminal::new(20, 3, 100).unwrap();
        u.write(&bytes);
        let again = u.read_lines(0, 2).unwrap();
        assert_eq!(again, lines[..2]);
        assert!(String::from_utf8_lossy(&bytes).ends_with("\r\n"));
    }
}
