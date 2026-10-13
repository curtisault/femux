//! `which-key` plugin (0.2): the overlay that lists the prefix table's chords, grouped by
//! purpose, once the prefix has been held for `ui.which_key_delay_ms` (REQ-UI-003). The
//! client decides *when* to ask (the delay); this plugin decides *what* is shown.

use femux_plugin::{
    BoundKey, ClientPlugin, Overlay, OverlayCtx, Place, Plugin, Span, Spans, Style,
};

pub fn plugin() -> Plugin {
    Plugin {
        name: "which-key",
        client: Some(Box::new(WhichKey)),
        daemon: None,
        cli: None,
    }
}

struct WhichKey;

impl ClientPlugin for WhichKey {
    fn name(&self) -> &'static str {
        "which-key"
    }
    fn overlay(&self, ctx: &OverlayCtx) -> Option<Overlay> {
        if ctx.mode != "prefix" {
            return None;
        }
        Some(render(ctx.prefix_table, ctx.prefix_key, ctx.width))
    }
}

/// Narrowest column worth having; fewer columns are used when `width` is short of four.
const MIN_COL: usize = 30;
/// Blank cells between one column's text and the next column.
const GAP: usize = 2;

/// Columns of `key  action` lines splitting `width` evenly. Groups stay whole and in order,
/// cut so the tallest column is as short as it can be; text is clipped to its column, so a
/// long line never pushes the rest of its row out of place.
pub fn render(table: &[BoundKey], prefix_key: &str, width: usize) -> Overlay {
    let title = vec![
        Span::new("prefix", Style::Title),
        Span::new(
            format!(
                "  {prefix_key} pressed · esc to cancel · {prefix_key} again sends it to the pane"
            ),
            Style::Muted,
        ),
    ];
    // Group in first-seen order.
    let mut groups: Vec<(String, Vec<&BoundKey>)> = Vec::new();
    for b in table {
        match groups.iter_mut().find(|(g, _)| *g == b.group) {
            Some((_, v)) => v.push(b),
            None => groups.push((b.group.clone(), vec![b])),
        }
    }
    let inner = width.saturating_sub(2);
    let ncols = (inner / MIN_COL).clamp(1, 4).min(groups.len()).max(1);
    let col_w = inner / ncols;
    let heights: Vec<usize> = groups.iter().map(|(_, v)| v.len()).collect();
    let mut columns: Vec<Vec<Spans>> = Vec::with_capacity(ncols);
    let mut start = 0;
    for end in cuts(&heights, ncols).into_iter().chain([groups.len()]) {
        let entries: Vec<&BoundKey> = groups[start..end]
            .iter()
            .flat_map(|(_, v)| v.iter().copied())
            .collect();
        let key_w = entries
            .iter()
            .map(|b| b.key.chars().count())
            .max()
            .unwrap_or(0);
        let room = col_w.saturating_sub(GAP);
        columns.push(
            entries
                .iter()
                .map(|b| {
                    let key = clip(&b.key, room);
                    let pad = (key_w + 2).saturating_sub(key.chars().count());
                    let action = clip(&b.action, room.saturating_sub(key_w + 2));
                    vec![
                        Span::new(key, Style::Key),
                        Span::text(" ".repeat(pad.min(room))),
                        Span::text(action),
                    ]
                })
                .collect(),
        );
        start = end;
    }
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
    let mut lines = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut line: Spans = Vec::new();
        for (ci, col) in columns.iter().enumerate() {
            let last = ci + 1 == columns.len();
            let mut used = 0;
            if let Some(spans) = col.get(r) {
                used = femux_plugin::width(spans);
                line.extend(spans.iter().cloned());
            }
            if !last && used < col_w {
                line.push(Span::text(" ".repeat(col_w - used)));
            }
        }
        lines.push(line);
    }
    Overlay {
        title,
        lines,
        place: Place::Bottom,
    }
}

/// Where `heights` (groups, in order) are cut into `ncols` columns: the cuts that make the
/// tallest column shortest, the earliest such cuts on a tie.
fn cuts(heights: &[usize], ncols: usize) -> Vec<usize> {
    fn search(
        heights: &[usize],
        from: usize,
        left: usize,
        cur: &mut Vec<usize>,
        best: &mut Option<(usize, Vec<usize>)>,
    ) {
        if left == 0 {
            let mut bounds = vec![0];
            bounds.extend(cur.iter().copied());
            bounds.push(heights.len());
            let tallest = bounds
                .windows(2)
                .map(|w| heights[w[0]..w[1]].iter().sum::<usize>())
                .max()
                .unwrap_or(0);
            if best.as_ref().is_none_or(|(t, _)| tallest < *t) {
                *best = Some((tallest, cur.clone()));
            }
            return;
        }
        // Leave at least one group for every column still to come.
        for at in from..=heights.len() - left {
            cur.push(at);
            search(heights, at + 1, left - 1, cur, best);
            cur.pop();
        }
    }
    if heights.is_empty() || ncols <= 1 {
        return Vec::new();
    }
    let mut best = None;
    search(heights, 1, ncols - 1, &mut Vec::new(), &mut best);
    best.map(|(_, c)| c).unwrap_or_default()
}

/// `s` cut to `n` characters, the last one an ellipsis when something was cut.
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    if n == 0 {
        return String::new();
    }
    s.chars().take(n - 1).chain(['…']).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &Spans) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn groups_stay_together_in_columns() {
        let bk = |k: &str, a: &str, g: &str| BoundKey {
            key: k.into(),
            action: a.into(),
            group: g.into(),
        };
        let table = [
            bk("h j k l", "focus", "panes"),
            bk("z", "zoom", "panes"),
            bk("s", "split below", "splits"),
            bk("t", "new tab", "tabs"),
        ];
        let o = render(&table, "ctrl+space", 120);
        assert_eq!(o.place, Place::Bottom);
        assert!(text(&o.title).starts_with("prefix  ctrl+space pressed"));
        let lines: Vec<String> = o.lines.iter().map(text).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("h j k l  focus"));
        assert!(lines[0].contains("s  split below"));
        assert!(lines[0].contains("t  new tab"));
        assert!(lines[1].starts_with("z        zoom"));
        // Narrow terminals fall back to one column.
        let narrow = render(&table, "ctrl+space", 40);
        assert_eq!(narrow.lines.len(), 4);
    }

    #[test]
    fn long_lines_are_clipped_to_their_column() {
        let bk = |k: &str, a: &str, g: &str| BoundKey {
            key: k.into(),
            action: a.into(),
            group: g.into(),
        };
        let table = [
            bk("z", "zoom", "panes"),
            bk("x", &"a very long description ".repeat(4), "splits"),
            bk("n", "next tab", "tabs"),
            bk("S", "session navigator", "ui"),
        ];
        let o = render(&table, "ctrl+space", 122);
        let line = text(&o.lines[0]);
        let cells: Vec<char> = line.chars().collect();
        // 120 cells in four columns of 30: every column starts where it should.
        for (i, start) in ['z', 'x', 'n', 'S'].iter().enumerate() {
            assert_eq!(cells[i * 30], *start, "{line:?}");
        }
        assert!(line.contains('…'), "{line:?}");
    }

    #[test]
    fn groups_are_cut_into_even_columns_in_order() {
        // The default table's group sizes: panes, splits, tabs, session, ui.
        assert_eq!(cuts(&[6, 5, 5, 5, 7], 4), [1, 2, 4]);
        assert_eq!(cuts(&[2, 1, 1], 3), [1, 2]);
        assert_eq!(cuts(&[3, 3], 1), Vec::<usize>::new());
        assert_eq!(cuts(&[], 4), Vec::<usize>::new());
    }

    #[test]
    fn only_in_prefix_mode() {
        let p = WhichKey;
        let ctx = |mode| OverlayCtx {
            mode,
            width: 80,
            height: 24,
            prefix_table: &[],
            prefix_key: "ctrl+space",
        };
        assert!(p.overlay(&ctx("terminal")).is_none());
        assert!(p.overlay(&ctx("prefix")).is_some());
    }
}
