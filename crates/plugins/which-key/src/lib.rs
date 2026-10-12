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

/// Columns of `key  action` lines, groups kept together, as wide as `width` allows.
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
    let col_w = 30usize;
    let ncols = (width.saturating_sub(2) / col_w).clamp(1, 4);
    let mut columns: Vec<Vec<Spans>> = vec![Vec::new(); ncols];
    for (_, entries) in &groups {
        // Each group goes to the shortest column so far.
        let (ci, _) = columns
            .iter()
            .enumerate()
            .min_by_key(|(i, c)| (c.len(), *i))
            .unwrap();
        let key_w = entries
            .iter()
            .map(|b| b.key.chars().count())
            .max()
            .unwrap_or(1);
        for b in entries {
            let pad = " ".repeat(key_w - b.key.chars().count() + 2);
            columns[ci].push(vec![
                Span::new(b.key.clone(), Style::Key),
                Span::text(pad),
                Span::text(b.action.clone()),
            ]);
        }
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
