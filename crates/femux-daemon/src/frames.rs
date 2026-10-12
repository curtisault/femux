//! Per-view frames (REQ-VT-002, REQ-VT-004, REQ-PROTO-040/041).
//!
//! A view remembers, per pane, the generation of every row it was last sent. A frame carries
//! the rows whose generation moved since, so it always describes the newest state; while a
//! frame is unacked no new frame is built, and the next one coalesces everything in between.

use std::collections::HashMap;
use std::sync::Arc;

use femux_proto::msg as m;
use tokio::sync::Notify;

use crate::state::State;

#[derive(Default)]
pub struct FrameState {
    pub subscribed: bool,
    pub seq: u64,
    pub unacked: Option<u64>,
    /// Next frame must be full (after attach, events_lost, tab switch).
    pub need_full: bool,
    tab: Option<String>,
    tab_gen: u64,
    panes: HashMap<String, Sent>,
    /// Woken on ack and on anything that needs a new frame for this view only.
    pub wake: Arc<Notify>,
}

#[derive(Clone)]
struct Sent {
    shape: u64,
    rows: Vec<u64>,
    cursor: u64,
}

impl FrameState {
    pub fn new() -> FrameState {
        FrameState {
            need_full: true,
            ..Default::default()
        }
    }
}

fn color(c: femux_vt::Color) -> m::Color {
    match c {
        femux_vt::Color::Default => m::Color::Default(m::DefaultColor),
        femux_vt::Color::Indexed(i) => m::Color::Indexed(i),
        femux_vt::Color::Rgb(r, g, b) => m::Color::Rgb([r, g, b]),
    }
}

/// Converts a row of cells, dropping the spacer column after wide characters.
pub fn cells(row: &[femux_vt::Cell]) -> Vec<m::Cell> {
    row.iter()
        .filter(|c| c.width != 0)
        .map(|c| m::Cell {
            ch: c.text.to_string(),
            fg: color(c.fg),
            bg: color(c.bg),
            ul: c.ul.map(color),
            attrs: c.attrs,
            width: c.width,
        })
        .collect()
}

fn cursor(c: femux_vt::Cursor) -> m::Cursor {
    m::Cursor {
        x: c.x,
        y: c.y,
        visible: c.visible,
        shape: match c.shape {
            femux_vt::CursorShape::Block => m::CursorShape::Block,
            femux_vt::CursorShape::Bar => m::CursorShape::Bar,
            femux_vt::CursorShape::Underline => m::CursorShape::Underline,
            femux_vt::CursorShape::BlockHollow => m::CursorShape::BlockHollow,
        },
    }
}

/// A visible pane: id, rectangle, terminal and what this view was last sent of it.
type PlannedPane = (
    String,
    crate::layout::Rect,
    crate::daemon::SharedTerminal,
    Option<Sent>,
);

/// What a frame needs from the state, captured under the state lock.
struct Plan {
    tab: String,
    tab_gen: u64,
    seq: u64,
    full: bool,
    info: m::TabInfo,
    panes: Vec<PlannedPane>,
}

fn plan(state: &State, view_id: &str) -> Option<Plan> {
    let view = state.views.get(view_id)?;
    let fs = &view.frames;
    if !fs.subscribed || fs.unacked.is_some() {
        return None;
    }
    let tab = state.tabs.get(&view.tab)?;
    let session = state.sessions.get(&tab.session)?;
    let full = fs.need_full
        || fs.tab.as_deref() != Some(view.tab.as_str())
        || fs.tab_gen != tab.layout_gen;
    let panes = tab
        .visible_rects()
        .into_iter()
        .filter_map(|(id, rect)| {
            let vt = state.panes.get(&id)?.vt.clone();
            let sent = if full {
                None
            } else {
                fs.panes.get(&id).cloned()
            };
            Some((id, rect, vt, sent))
        })
        .collect();
    Some(Plan {
        tab: tab.id.clone(),
        tab_gen: tab.layout_gen,
        seq: fs.seq,
        full,
        info: m::TabInfo {
            id: tab.id.clone(),
            title: state.tab_title(tab),
            index: session.tabs.iter().position(|t| *t == tab.id).unwrap_or(0) as u32,
            count: session.tabs.len() as u32,
        },
        panes,
    })
}

/// Builds the next frame for `view_id`, or `None` if nothing changed or a frame is unacked.
///
/// Pane terminals are read without holding the state lock, so a busy pane can delay its
/// own frames but never requests (REQ-PTY-003).
pub fn build(state: &std::sync::Mutex<State>, view_id: &str) -> Option<m::Frame> {
    let plan = plan(&state.lock().unwrap(), view_id)?;
    let mut sent_updates = Vec::new();
    let mut panes = Vec::new();
    for (pane_id, rect, vt, prev) in &plan.panes {
        let vt = vt.lock().unwrap();
        let grid = vt.grid();
        let prev = prev.as_ref().filter(|s| s.shape == grid.shape_generation);
        let mut runs = Vec::new();
        for (y, row) in grid.rows.iter().enumerate() {
            if prev.is_none_or(|p| p.rows.get(y).is_none_or(|&g| row.generation > g)) {
                runs.push(m::CellRun {
                    x: 0,
                    y: y as u16,
                    run: cells(&row.cells),
                });
            }
        }
        let cursor_changed = prev.is_none_or(|p| grid.cursor_generation > p.cursor);
        if prev.is_none() || !runs.is_empty() || cursor_changed {
            panes.push(m::PaneFrame {
                pane_id: pane_id.clone(),
                rect: (*rect).into(),
                cells: runs,
                cursor: cursor(grid.cursor),
                scroll_offset: 0,
            });
        }
        let rows = grid.rows.iter().map(|r| r.generation).collect();
        sent_updates.push((
            pane_id.clone(),
            Sent {
                shape: grid.shape_generation,
                rows,
                cursor: grid.cursor_generation,
            },
        ));
    }
    if !plan.full && panes.is_empty() {
        return None;
    }

    let mut st = state.lock().unwrap();
    let view = st.views.get_mut(view_id)?;
    let fs = &mut view.frames;
    // Anything that would change the plan restarts it; the pump runs again on the next epoch.
    if fs.seq != plan.seq
        || fs.unacked.is_some()
        || view.tab != plan.tab
        || (fs.need_full && !plan.full)
    {
        return None;
    }
    if plan.full {
        fs.panes.clear();
    }
    fs.panes.extend(sent_updates);
    fs.need_full = false;
    fs.tab = Some(plan.tab);
    fs.tab_gen = plan.tab_gen;
    fs.seq += 1;
    fs.unacked = Some(fs.seq);
    Some(m::Frame {
        view_id: view_id.to_owned(),
        frame_seq: fs.seq,
        full: plan.full,
        panes,
        tab: plan.info,
        images: Vec::new(),
    })
}
