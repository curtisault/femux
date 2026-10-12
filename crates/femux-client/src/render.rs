//! Drawing the client into a termwiz [`Surface`]: tab strip (REQ-UI-020), tiled panes with
//! borders in the daemon's gaps, floats on top (REQ-UI-011), copy mode, the status line and
//! overlays. The host diffs successive surfaces for the terminal (REQ-UI-050).

use femux_plugin::{Overlay as PluginOverlay, Span, Spans, Style};
use femux_proto::msg as m;
use termwiz::cell::{Blink, CellAttributes, Intensity, Underline};
use termwiz::color::{ColorAttribute, SrgbaTuple};
use termwiz::surface::{Change, CursorVisibility, Position, Surface};

use crate::app::{App, Mode, Overlay};
use crate::bindings::Table;
use crate::config::Borders;
use crate::layout::Layout;
use crate::model::PaneScreen;
use crate::theme::Theme;

fn color(c: &m::Color) -> ColorAttribute {
    match c {
        m::Color::Default(_) => ColorAttribute::Default,
        m::Color::Indexed(i) => ColorAttribute::PaletteIndex(*i),
        m::Color::Rgb([r, g, b]) => {
            ColorAttribute::TrueColorWithDefaultFallback(SrgbaTuple::from((*r, *g, *b)))
        }
    }
}

/// termwiz attributes for a protocol cell.
pub fn attributes(c: &m::Cell) -> CellAttributes {
    let mut a = CellAttributes::default();
    a.set_foreground(color(&c.fg));
    a.set_background(color(&c.bg));
    let bit = |n: u16| c.attrs & (1 << n) != 0;
    if bit(0) {
        a.set_intensity(Intensity::Bold);
    } else if bit(2) {
        a.set_intensity(Intensity::Half);
    }
    a.set_italic(bit(1));
    if bit(3) {
        a.set_blink(Blink::Slow);
    }
    a.set_reverse(bit(4));
    a.set_invisible(bit(5));
    a.set_strikethrough(bit(6));
    a.set_underline(match (c.attrs >> 8) & 0b111 {
        0 => Underline::None,
        2 => Underline::Double,
        3 => Underline::Curly,
        4 => Underline::Dotted,
        5 => Underline::Dashed,
        _ => Underline::Single,
    });
    a
}

pub(crate) fn attr(fg: ColorAttribute, bg: ColorAttribute) -> CellAttributes {
    let mut a = CellAttributes::default();
    a.set_foreground(fg);
    a.set_background(bg);
    a
}

pub(crate) fn bold(mut a: CellAttributes) -> CellAttributes {
    a.set_intensity(Intensity::Bold);
    a
}

pub(crate) struct Canvas<'a> {
    pub(crate) s: &'a mut Surface,
    pub(crate) w: usize,
    pub(crate) h: usize,
}

impl Canvas<'_> {
    pub(crate) fn put(&mut self, x: usize, y: usize, text: &str, a: &CellAttributes) -> usize {
        if y >= self.h || x >= self.w {
            return x;
        }
        let avail = self.w - x;
        let text: String = text.chars().take(avail).collect();
        let n = text.chars().count();
        self.s.add_change(Change::CursorPosition {
            x: Position::Absolute(x),
            y: Position::Absolute(y),
        });
        self.s.add_change(Change::AllAttributes(a.clone()));
        self.s.add_change(Change::Text(text));
        x + n
    }

    pub(crate) fn fill(&mut self, x: usize, y: usize, n: usize, ch: char, a: &CellAttributes) {
        if n > 0 {
            let s: String = std::iter::repeat_n(ch, n).collect();
            self.put(x, y, &s, a);
        }
    }

    pub(crate) fn spans(
        &mut self,
        x: usize,
        y: usize,
        spans: &[Span],
        theme: &Theme,
        bg: ColorAttribute,
    ) -> usize {
        let mut x = x;
        for sp in spans {
            let a = match sp.style {
                Style::Text => attr(theme.text, bg),
                Style::Muted => attr(theme.muted, bg),
                Style::Key => bold(attr(theme.accent, bg)),
                Style::Title => bold(attr(theme.accent, bg)),
                Style::Accent => attr(theme.accent, bg),
                Style::Warn => attr(theme.warn, bg),
                Style::Ok => attr(theme.ok, bg),
                Style::Error => attr(theme.error, bg),
                Style::Chip => bold(attr(theme.accent_fg, theme.accent)),
            };
            x = self.put(x, y, &sp.text, &a);
        }
        x
    }
}

fn glyphs(b: Borders) -> [&'static str; 11] {
    // v h cross t-right t-left t-down t-up tl tr bl br
    match b {
        Borders::Single => ["│", "─", "┼", "├", "┤", "┬", "┴", "┌", "┐", "└", "┘"],
        Borders::Heavy => ["┃", "━", "╋", "┣", "┫", "┳", "┻", "┏", "┓", "┗", "┛"],
        Borders::Double => ["║", "═", "╬", "╠", "╣", "╦", "╩", "╔", "╗", "╚", "╝"],
        Borders::Spaces => [" "; 11],
    }
}

/// Whether border cell `(x, y)` lies on the frame around `r`.
pub fn on_edge(x: u16, y: u16, r: &m::Rect) -> bool {
    let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
    let in_x = x + 1 >= x0 && x <= x1;
    let in_y = y + 1 >= y0 && y <= y1;
    ((x + 1 == x0 || x == x1) && in_y) || ((y + 1 == y0 || y == y1) && in_x)
}

/// The dashed form of a straight border glyph: the distinct style of restored panes
/// (REQ-STORE-008). Junctions and corners keep their shape.
pub fn dashed(glyph: &'static str) -> &'static str {
    match glyph {
        "│" | "┃" | "║" => "┆",
        "─" | "━" | "═" => "┄",
        g => g,
    }
}

/// Border cells in the gaps between tiled rectangles: `(x, y, glyph, touches_focused)`.
pub fn borders(
    rects: &[(String, m::Rect)],
    area: (u16, u16),
    focused: &str,
    style: Borders,
) -> Vec<(u16, u16, &'static str, bool)> {
    use std::collections::HashSet;
    let mut cells: HashSet<(u16, u16)> = HashSet::new();
    for (_, r) in rects {
        let right = r.x + r.w;
        let bottom = r.y + r.h;
        if right < area.0 {
            for y in r.y..bottom.min(area.1) {
                cells.insert((right, y));
            }
        }
        if bottom < area.1 {
            for x in r.x..right.min(area.0) {
                cells.insert((x, bottom));
            }
        }
        if right < area.0 && bottom < area.1 {
            cells.insert((right, bottom));
        }
    }
    let f = rects.iter().find(|(p, _)| p == focused).map(|(_, r)| r);
    let touches = |x: u16, y: u16| f.is_some_and(|r| on_edge(x, y, r));
    let g = glyphs(style);
    let mut out: Vec<(u16, u16, &'static str, bool)> = cells
        .iter()
        .map(|&(x, y)| {
            let up = y > 0 && cells.contains(&(x, y - 1));
            let down = cells.contains(&(x, y + 1));
            let left = x > 0 && cells.contains(&(x - 1, y));
            let right = cells.contains(&(x + 1, y));
            let glyph = match (up, down, left, right) {
                (true, true, true, true) => g[2],
                (true, true, false, true) => g[3],
                (true, true, true, false) => g[4],
                (false, true, true, true) => g[5],
                (true, false, true, true) => g[6],
                (false, true, false, true) => g[7],
                (false, true, true, false) => g[8],
                (true, false, false, true) => g[9],
                (true, false, true, false) => g[10],
                (_, _, true, _) | (_, _, _, true) => g[1],
                _ => g[0],
            };
            (x, y, glyph, touches(x, y))
        })
        .collect();
    out.sort();
    out
}

/// What the status row shows, if anything: prompts and confirmations take the whole row,
/// text follows the session chip and mode.
enum Transient {
    Confirm(String),
    Prompt(String, String),
    Text(Spans, bool),
}

fn transient(app: &App) -> Option<Transient> {
    match &app.overlay {
        Overlay::Confirm { text, .. } => return Some(Transient::Confirm(text.clone())),
        Overlay::Prompt { title, input, .. } => {
            return Some(Transient::Prompt(title.clone(), input.clone()));
        }
        _ => {}
    }
    let mut spans: Spans = Vec::new();
    let mut cursor = false;
    if let Some(d) = app.preselect {
        spans.push(Span::new(
            format!(
                "next split: {}",
                crate::bindings::Action::Preselect(d).describe()
            ),
            Style::Warn,
        ));
    }
    if let Some(cm) = &app.copy
        && app.mode == Mode::Copy
    {
        if !spans.is_empty() {
            spans.push(Span::text(" "));
        }
        match &cm.typing {
            Some(t) => {
                let lead = if t.dir == m::SearchDir::Down {
                    "/"
                } else {
                    "?"
                };
                spans.push(Span::text(format!("{lead}{}", t.regex)));
                cursor = true;
            }
            None => spans.push(Span::new(
                format!("line {} col {}", cm.cur.0, cm.cur.1),
                Style::Muted,
            )),
        }
    }
    if !cursor && let Some(msg) = &app.message {
        if !spans.is_empty() {
            spans.push(Span::text(" "));
        }
        spans.push(Span::new(msg.clone(), Style::Warn));
    }
    (!spans.is_empty()).then_some(Transient::Text(spans, cursor))
}

fn mode_label(app: &App, theme: &Theme) -> (&'static str, CellAttributes) {
    if let Overlay::Navigator(nav) = &app.overlay {
        let label = match nav.kind {
            crate::nav::NavKind::Sessions => "SESSIONS",
            crate::nav::NavKind::Windows => "WINDOWS",
        };
        return (label, bold(attr(theme.accent_fg, theme.accent)));
    }
    match (app.prefix_armed(), app.mode) {
        (true, _) => ("PREFIX", bold(attr(theme.accent_fg, theme.warn))),
        (false, Mode::Terminal) => ("TERMINAL", attr(theme.muted, theme.bar_bg)),
        (false, Mode::Normal) => ("NORMAL", bold(attr(theme.accent_fg, theme.ok))),
        (false, Mode::Copy) => ("COPY", bold(attr(theme.accent_fg, theme.accent))),
    }
}

/// The first `max` columns of `spans`: modules may ignore their width, the bar may not.
pub(crate) fn clip(spans: &Spans, max: usize) -> Spans {
    let mut out = Vec::new();
    let mut left = max;
    for sp in spans {
        if left == 0 {
            break;
        }
        let text: String = sp.text.chars().take(left).collect();
        left -= text.chars().count();
        out.push(Span::new(text, sp.style));
    }
    out
}

/// Splits a module's spans into lines at `\n` (vertical bars only).
fn lines_of(spans: &Spans) -> Vec<Spans> {
    let mut out: Vec<Spans> = vec![Vec::new()];
    for sp in spans {
        for (i, part) in sp.text.split('\n').enumerate() {
            if i > 0 {
                out.push(Vec::new());
            }
            if !part.is_empty() {
                out.last_mut()
                    .unwrap()
                    .push(Span::new(part.to_owned(), sp.style));
            }
        }
    }
    out
}

/// The info bar (REQ-UI-030/031): a row at the top or bottom, or a column at the left or
/// right with modules stacked.
fn draw_bar(app: &mut App, c: &mut Canvas, theme: &Theme, l: &Layout, tr: &Option<Transient>) {
    let bar = l.bar;
    let bg = theme.bar_bg;
    let (mode, mode_attr) = mode_label(app, theme);
    let chip = format!(" {} ", app.model.session);
    if !l.bar_vertical {
        let (x0, y, w) = (bar.x as usize, bar.y as usize, bar.w as usize);
        c.fill(x0, y, w, ' ', &attr(theme.bar_fg, bg));
        let ctx = app.bar_ctx(false);
        let right: Vec<Spans> = app
            .cfg
            .bar_right
            .clone()
            .iter()
            .filter_map(|n| app.module_spans(n, &ctx, w / 2))
            .collect();
        let mut right_line: Spans = Vec::new();
        for (i, sp) in right.into_iter().enumerate() {
            if i > 0 {
                right_line.push(Span::new(" │ ", Style::Muted));
            }
            right_line.extend(
                sp.into_iter()
                    .map(|s| Span::new(s.text.replace('\n', " "), s.style)),
            );
        }
        let right_line = clip(&right_line, w.saturating_sub(chip.chars().count() + 12));
        let rw = femux_plugin::width(&right_line) + 1;
        let mut x = c.put(x0, y, &chip, &bold(attr(theme.accent_fg, theme.accent)));
        x = c.put(x, y, &format!(" {mode} "), &mode_attr);
        if !matches!(tr, Some(Transient::Text(..))) {
            x = c.put(
                x,
                y,
                &format!(" prefix {}", app.prefix_label()),
                &attr(theme.muted, bg),
            );
            for name in app.cfg.bar_left.clone() {
                if name == "session" {
                    continue;
                }
                if let Some(sp) = app.module_spans(&name, &ctx, w / 2) {
                    let sp: Spans = sp
                        .into_iter()
                        .map(|s| Span::new(s.text.replace('\n', " "), s.style))
                        .collect();
                    x = c.spans(x + 2, y, &sp, theme, bg);
                }
            }
        }
        let _ = x;
        if rw < w {
            c.spans(x0 + w - rw, y, &right_line, theme, bg);
        }
        return;
    }
    // A column: the chip and mode, then the left modules from the top, the right ones at the
    // bottom (REQ-UI-030: vertical bars stack modules).
    let (x0, w) = (bar.x as usize + 1, bar.w.saturating_sub(2) as usize);
    for y in bar.y..bar.y + bar.h {
        c.fill(
            bar.x as usize,
            y as usize,
            bar.w as usize,
            ' ',
            &attr(theme.bar_fg, bg),
        );
    }
    let bottom = (bar.y + bar.h) as usize;
    let mut y = bar.y as usize + 1;
    c.put(x0, y, &chip, &bold(attr(theme.accent_fg, theme.accent)));
    y += 2;
    c.put(x0, y, "mode", &attr(theme.muted, bg));
    c.put(x0, y + 1, mode, &mode_attr);
    y += 3;
    let ctx = app.bar_ctx(true);
    let block = |app: &mut App, name: &str| -> Option<Vec<Spans>> {
        let sp = app.module_spans(name, &ctx, w)?;
        let mut lines = vec![vec![Span::new(name.to_owned(), Style::Muted)]];
        lines.extend(lines_of(&sp));
        // A column never draws past its own width.
        Some(lines.iter().map(|l| clip(l, w)).collect())
    };
    for name in app.cfg.bar_left.clone() {
        if name == "session" {
            continue;
        }
        if let Some(lines) = block(app, &name) {
            for line in lines {
                if y < bottom {
                    c.spans(x0, y, &line, theme, bg);
                }
                y += 1;
            }
            y += 1;
        }
    }
    let blocks: Vec<Vec<Spans>> = app
        .cfg
        .bar_right
        .clone()
        .iter()
        .filter_map(|n| block(app, n))
        .collect();
    let height: usize = blocks.iter().map(|b| b.len() + 1).sum();
    let mut y = y.max(bottom.saturating_sub(height));
    for b in blocks {
        for line in b {
            if y < bottom {
                c.spans(x0, y, &line, theme, bg);
            }
            y += 1;
        }
        y += 1;
    }
}

/// The status row: prompts, confirmations and messages (see [`Layout::status`]).
fn draw_status(
    app: &App,
    c: &mut Canvas,
    theme: &Theme,
    l: &Layout,
    tr: &Option<Transient>,
    cursor: &mut Option<(usize, usize)>,
) {
    let (x0, y, w) = (
        l.status.0 as usize,
        l.status.1 as usize,
        l.status.2 as usize,
    );
    let bg = theme.bar_bg;
    match tr {
        None => {}
        Some(Transient::Confirm(text)) => {
            c.fill(x0, y, w, ' ', &attr(theme.accent_fg, theme.warn));
            c.put(x0 + 1, y, text, &bold(attr(theme.accent_fg, theme.warn)));
            *cursor = None;
        }
        Some(Transient::Prompt(title, input)) => {
            c.fill(x0, y, w, ' ', &attr(theme.text, bg));
            let x = c.put(
                x0 + 1,
                y,
                &format!("{title}: "),
                &bold(attr(theme.accent, bg)),
            );
            let x = c.put(x, y, input, &attr(theme.text, bg));
            *cursor = Some((x, y));
        }
        Some(Transient::Text(spans, wants_cursor)) => {
            let x = if l.bar_vertical {
                c.fill(x0, y, w, ' ', &attr(theme.bar_fg, bg));
                x0 + 1
            } else {
                // After the chip and the mode.
                let (mode, _) = mode_label(app, theme);
                x0 + app.model.session.chars().count() + 2 + mode.len() + 2 + 1
            };
            let end = c.spans(x, y, spans, theme, bg);
            if *wants_cursor {
                *cursor = Some((end, y));
            }
        }
    }
}

/// The tab strip (REQ-UI-020). `app.tab_hits` is refreshed for the mouse.
fn draw_strip(app: &mut App, c: &mut Canvas, theme: &Theme, l: &Layout) {
    app.tab_hits.clear();
    let Some((sx, sy, sw)) = l.strip else { return };
    let (sx, y, end) = (sx as usize, sy as usize, (sx + sw) as usize);
    c.fill(sx, y, sw as usize, ' ', &attr(theme.bar_fg, theme.bar_bg));
    let mut x = sx;
    let tabs = app.model.tabs.clone();
    for (i, t) in tabs.iter().enumerate() {
        let mut label = format!(" {} {}", i + 1, app.model.tab_title(t));
        if app.model.tab_running(t) {
            label.push(' ');
            label.push_str(&app.cfg.running_indicator);
        }
        if app.model.tab_restored(t) {
            label.push_str(" ↺");
        }
        if t.urgent {
            label.push_str(" !");
        }
        if t.zoomed {
            label.push_str(" Z");
        }
        label.push(' ');
        let a = if t.id == app.model.current {
            bold(attr(theme.accent_fg, theme.accent))
        } else if t.urgent {
            bold(attr(theme.warn, theme.bar_bg))
        } else {
            attr(theme.bar_fg, theme.bar_bg)
        };
        let label: String = label.chars().take(end.saturating_sub(x)).collect();
        let x1 = c.put(x, y, &label, &a);
        app.tab_hits.push((x as u16, x1 as u16, t.id.clone()));
        x = x1 + 1;
        if x >= end {
            break;
        }
    }
}

/// Draws the whole client. Hit boxes for the mouse are refreshed.
pub fn render(app: &mut App, theme: &Theme, s: &mut Surface) {
    app.dirty = false;
    let (w, h) = s.dimensions();
    s.add_change(Change::ClearScreen(ColorAttribute::Default));
    let mut c = Canvas { s, w, h };
    let l = app.layout();
    let area = l.panes;
    let o = (area.x as usize, area.y as usize);
    let (vw, vh) = (area.w, area.h);
    let mut cursor: Option<(usize, usize)> = None;

    // Panes.
    let current = app.model.current_tab().cloned();
    if let Some(tab) = &current {
        let tiled: Vec<(String, m::Rect)> = tab
            .tiled()
            .into_iter()
            .filter_map(|id| {
                let r = app.model.panes.get(&id)?.screen.as_ref()?.rect.clone();
                Some((id, r))
            })
            .collect();
        for (id, _) in &tiled {
            draw_pane(app, &mut c, theme, id, o, &mut cursor);
        }
        if !tab.zoomed {
            let restored: Vec<&m::Rect> = tiled
                .iter()
                .filter(|(id, _)| app.model.panes.get(id).is_some_and(|p| p.restored))
                .map(|(_, r)| r)
                .collect();
            for (x, y, glyph, focused) in borders(&tiled, (vw, vh), &tab.focus, app.cfg.borders) {
                let by_restored = restored.iter().any(|r| on_edge(x, y, r));
                let color = match (focused, by_restored) {
                    (true, _) => theme.accent,
                    (false, true) => theme.warn,
                    _ => theme.muted,
                };
                let glyph = if by_restored { dashed(glyph) } else { glyph };
                c.put(
                    x as usize + o.0,
                    y as usize + o.1,
                    glyph,
                    &attr(color, ColorAttribute::Default),
                );
            }
            for f in &tab.floats {
                let Some(r) = app
                    .model
                    .panes
                    .get(&f.id)
                    .and_then(|p| p.screen.as_ref())
                    .map(|s| s.rect.clone())
                else {
                    continue;
                };
                let focused = tab.focus == f.id;
                let restored = app.model.panes.get(&f.id).is_some_and(|p| p.restored);
                let a = attr(
                    match (focused, restored) {
                        (true, _) => theme.accent,
                        (false, true) => theme.warn,
                        _ => theme.muted,
                    },
                    ColorAttribute::Default,
                );
                let g = glyphs(app.cfg.borders);
                let (ox, oy) = (o.0 as i32, o.1 as i32);
                let (x0, y0) = (r.x as i32 - 1 + ox, r.y as i32 - 1 + oy);
                let (x1, y1) = (r.x as i32 + r.w as i32 + ox, r.y as i32 + r.h as i32 + oy);
                for y in y0..=y1 {
                    if y < oy || y >= oy + vh as i32 {
                        continue;
                    }
                    for x in x0..=x1 {
                        if x < ox || x >= ox + vw as i32 {
                            continue;
                        }
                        let glyph = match (x == x0, x == x1, y == y0, y == y1) {
                            (true, _, true, _) => g[7],
                            (_, true, true, _) => g[8],
                            (true, _, _, true) => g[9],
                            (_, true, _, true) => g[10],
                            (true, _, _, _) | (_, true, _, _) => g[0],
                            (_, _, true, _) | (_, _, _, true) => g[1],
                            _ => continue,
                        };
                        let glyph = if restored { dashed(glyph) } else { glyph };
                        c.put(x as usize, y as usize, glyph, &a);
                    }
                }
                let title = app
                    .model
                    .panes
                    .get(&f.id)
                    .map(|p| {
                        if !p.title.is_empty() {
                            p.title.clone()
                        } else {
                            p.fg_cmd.clone()
                        }
                    })
                    .unwrap_or_default();
                if !title.is_empty() && y0 >= oy {
                    c.put(
                        (x0 as usize + 2).min(w),
                        y0 as usize,
                        &format!(" {title} "),
                        &bold(a.clone()),
                    );
                }
                draw_pane(app, &mut c, theme, &f.id, o, &mut cursor);
            }
        }
    }

    draw_strip(app, &mut c, theme, &l);
    crate::nav::draw_sidenav(app, &mut c, theme, &l);
    let tr = transient(app);
    draw_bar(app, &mut c, theme, &l, &tr);
    draw_status(app, &mut c, theme, &l, &tr, &mut cursor);

    // Overlays.
    match app.overlay.clone() {
        Overlay::WhichKey => {
            if let Some(ov) = app.plugin_overlay() {
                draw_panel(
                    &mut c,
                    theme,
                    &ov,
                    area.x as usize,
                    area.w as usize,
                    (area.y + area.h) as usize,
                );
            }
        }
        Overlay::Help => {
            let ov = help_overlay(app);
            draw_box(&mut c, theme, &ov, &mut cursor, None);
        }
        Overlay::Palette(p) => {
            let matches = p.matches();
            let mut lines = Vec::new();
            for (i, item) in matches.iter().enumerate().take(h.saturating_sub(6)) {
                let style = if i == p.selected {
                    Style::Accent
                } else {
                    Style::Text
                };
                lines.push(vec![
                    Span::new(if i == p.selected { "> " } else { "  " }, Style::Accent),
                    Span::new(item.label.clone(), style),
                    Span::new(format!("  {}", item.hint), Style::Muted),
                ]);
            }
            let ov = PluginOverlay {
                title: vec![Span::new("> ", Style::Accent), Span::text(p.input.clone())],
                lines,
                place: femux_plugin::Place::Center,
            };
            draw_box(
                &mut c,
                theme,
                &ov,
                &mut cursor,
                Some(2 + p.input.chars().count()),
            );
        }
        Overlay::Navigator(nav) => {
            crate::nav::draw_navigator(app, &nav, &mut c, theme, &mut cursor);
        }
        Overlay::None | Overlay::Confirm { .. } | Overlay::Prompt { .. } => {}
    }

    c.s.add_change(Change::AllAttributes(CellAttributes::default()));
    match cursor {
        Some((x, y)) if x < w && y < h => {
            c.s.add_change(Change::CursorPosition {
                x: Position::Absolute(x),
                y: Position::Absolute(y),
            });
            c.s.add_change(Change::CursorVisibility(CursorVisibility::Visible));
        }
        _ => {
            c.s.add_change(Change::CursorVisibility(CursorVisibility::Hidden));
        }
    }
}

fn draw_pane(
    app: &App,
    c: &mut Canvas,
    theme: &Theme,
    id: &str,
    o: (usize, usize),
    cursor: &mut Option<(usize, usize)>,
) {
    let Some(pane) = app.model.panes.get(id) else {
        return;
    };
    let Some(screen) = &pane.screen else { return };
    let r = &screen.rect;
    let focused = app.model.focused_id().as_deref() == Some(id);
    if focused
        && app.mode == Mode::Copy
        && let Some(cm) = &app.copy
    {
        draw_copy(cm, c, theme, r, o, cursor);
        return;
    }
    draw_screen(screen, c, o);
    if focused && matches!(app.overlay, Overlay::None | Overlay::WhichKey) && screen.cursor.visible
    {
        *cursor = Some((
            o.0 + r.x as usize + screen.cursor.x as usize,
            o.1 + r.y as usize + screen.cursor.y as usize,
        ));
    }
}

/// Draws a pane's rows as runs of equally styled cells: one cursor move, one attribute
/// change and one text per run rather than per cell.
fn draw_screen(screen: &PaneScreen, c: &mut Canvas, o: (usize, usize)) {
    let r = &screen.rect;
    let rx = o.0 + r.x as usize;
    for (y, row) in screen.rows.iter().enumerate().take(r.h as usize) {
        let sy = o.1 + r.y as usize + y;
        let mut x = 0usize;
        let end = row.len().min(r.w as usize);
        let mut run = String::new();
        let mut run_x = 0usize;
        let mut run_attr: Option<CellAttributes> = None;
        while x < end {
            let (text, attr, width) = match &row[x] {
                Some(cell) if !cell.ch.is_empty() => (
                    cell.ch.as_str(),
                    attributes(cell),
                    cell.width.max(1) as usize,
                ),
                Some(cell) => (" ", attributes(cell), 1),
                None => (" ", CellAttributes::default(), 1),
            };
            if run_attr.as_ref() != Some(&attr) {
                if let Some(a) = run_attr.take() {
                    c.put(rx + run_x, sy, &run, &a);
                }
                run.clear();
                run_x = x;
                run_attr = Some(attr);
            }
            run.push_str(text);
            x += width;
        }
        if let Some(a) = run_attr {
            c.put(rx + run_x, sy, &run, &a);
        }
    }
}

fn draw_copy(
    cm: &crate::copy::CopyMode,
    c: &mut Canvas,
    theme: &Theme,
    r: &m::Rect,
    o: (usize, usize),
    cursor: &mut Option<(usize, usize)>,
) {
    let rx = o.0 + r.x as usize;
    for row in 0..r.h {
        let line = cm.top + row as i64;
        let sy = o.1 + r.y as usize + row as usize;
        let cells = cm.lines.get(&line);
        let mut x = 0u16;
        while x < r.w {
            let cell = cells.and_then(|l| l.get(x as usize));
            let mut a = cell.map(attributes).unwrap_or_default();
            let text = cell
                .map(|cl| {
                    if cl.ch.is_empty() {
                        " ".to_owned()
                    } else {
                        cl.ch.clone()
                    }
                })
                .unwrap_or_else(|| " ".into());
            if cm.is_selected(line, x) {
                a.set_background(theme.selection);
                a.set_foreground(theme.text);
            } else if cm.is_match(line, x) {
                a.set_background(theme.warn);
                a.set_foreground(theme.accent_fg);
            }
            c.put(rx + x as usize, sy, &text, &a);
            x += cell.map(|cl| cl.width.max(1)).unwrap_or(1) as u16;
        }
    }
    if cm.typing.is_none() {
        *cursor = Some((
            rx + cm.cur.1 as usize,
            o.1 + r.y as usize + (cm.cur.0 - cm.top).max(0) as usize,
        ));
    }
}

/// A full-width panel above the status line (which-key).
/// A panel across `x..x + w` ending above row `bottom` (which-key).
fn draw_panel(c: &mut Canvas, theme: &Theme, o: &PluginOverlay, x: usize, w: usize, bottom: usize) {
    let height = (o.lines.len() + 2).min(bottom);
    let top = bottom - height;
    for y in top..bottom {
        c.fill(x, y, w, ' ', &attr(theme.text, theme.overlay_bg));
    }
    c.spans(x + 1, top, &o.title, theme, theme.overlay_bg);
    for (i, line) in o.lines.iter().enumerate().take(height.saturating_sub(2)) {
        c.spans(x + 1, top + 1 + i, line, theme, theme.overlay_bg);
    }
}

/// A centered box (help, palette). `input_cursor` places the cursor on the title line.
fn draw_box(
    c: &mut Canvas,
    theme: &Theme,
    o: &PluginOverlay,
    cursor: &mut Option<(usize, usize)>,
    input_cursor: Option<usize>,
) {
    // Never wider or taller than the terminal, however small it is.
    let bw = (c.w.saturating_sub(4)).clamp(10, 72).min(c.w);
    let bh = (o.lines.len() + 3)
        .clamp(3, c.h.saturating_sub(2).max(3))
        .min(c.h);
    let x0 = c.w.saturating_sub(bw) / 2;
    let y0 = c.h.saturating_sub(bh) / 2;
    let bg = theme.overlay_bg;
    let frame = attr(theme.accent, bg);
    for y in y0..y0 + bh {
        c.fill(x0, y, bw, ' ', &attr(theme.text, bg));
    }
    c.fill(x0, y0, bw, '─', &frame);
    c.fill(x0, y0 + bh - 1, bw, '─', &frame);
    let tx = c.spans(x0 + 1, y0 + 1, &o.title, theme, bg);
    let _ = tx;
    for (i, line) in o.lines.iter().enumerate().take(bh.saturating_sub(3)) {
        c.spans(x0 + 1, y0 + 2 + i, line, theme, bg);
    }
    *cursor = input_cursor.map(|ix| (x0 + 1 + ix, y0 + 1));
}

/// `?`: every table's bindings.
fn help_overlay(app: &App) -> PluginOverlay {
    let mut lines = Vec::new();
    for (name, t) in [
        ("root", Table::Root),
        ("prefix", Table::Prefix),
        ("copy", Table::Copy),
    ] {
        lines.push(vec![Span::new(format!("{name} table"), Style::Title)]);
        for (table, key, _, desc) in app.bindings.list() {
            if table != name {
                continue;
            }
            let _ = t;
            lines.push(vec![
                Span::new(format!("{key:<12}"), Style::Key),
                Span::text(desc),
            ]);
        }
    }
    PluginOverlay {
        title: vec![
            Span::new("help", Style::Title),
            Span::new("  any key closes", Style::Muted),
        ],
        lines,
        place: femux_plugin::Place::Center,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: u16, y: u16, w: u16, h: u16) -> m::Rect {
        m::Rect { x, y, w, h }
    }

    #[test]
    fn runs_render_like_cells_and_boxes_fit_tiny_terminals() {
        let cell = |ch: &str, bold: bool| m::Cell {
            ch: ch.into(),
            attrs: u16::from(bold),
            width: 1,
            ..Default::default()
        };
        let mut screen = PaneScreen::default();
        screen.apply(&m::PaneFrame {
            pane_id: "P".into(),
            rect: r(1, 0, 6, 1),
            cells: vec![m::CellRun {
                x: 0,
                y: 0,
                run: vec![
                    cell("a", false),
                    cell("b", true),
                    cell("c", true),
                    cell("", false),
                ],
            }],
            cursor: m::Cursor::default(),
            scroll_offset: 0,
        });
        let mut s = Surface::new(8, 1);
        let mut c = Canvas {
            s: &mut s,
            w: 8,
            h: 1,
        };
        draw_screen(&screen, &mut c, (0, 0));
        assert_eq!(s.screen_chars_to_string(), " abc    \n");
        assert_eq!(s.screen_cells()[0][2].attrs().intensity(), Intensity::Bold);
        assert_eq!(
            s.screen_cells()[0][1].attrs().intensity(),
            Intensity::Normal
        );
        // A palette box on a 6x3 terminal must not underflow.
        let o = PluginOverlay {
            title: vec![Span::text("> ")],
            lines: vec![vec![Span::text("x")]; 5],
            place: femux_plugin::Place::Center,
        };
        let mut tiny = Surface::new(6, 3);
        let mut c = Canvas {
            s: &mut tiny,
            w: 6,
            h: 3,
        };
        let mut cursor = None;
        draw_box(&mut c, &Theme::dark(), &o, &mut cursor, Some(2));
        assert_eq!(cursor, Some((3, 1)));
    }

    #[test]
    fn clip_keeps_styles_and_stops_at_the_width() {
        let sp = vec![Span::new("abc", Style::Warn), Span::text("defgh")];
        let c = clip(&sp, 5);
        assert_eq!(c, vec![Span::new("abc", Style::Warn), Span::text("de")]);
        assert!(clip(&sp, 0).is_empty());
    }

    #[test]
    fn borders_fill_the_gaps_with_junctions() {
        // a | (b / c) in an 81x25 area (REQ-SESS-002 geometry).
        let rects = vec![
            ("a".to_owned(), r(0, 0, 40, 25)),
            ("b".to_owned(), r(41, 0, 40, 12)),
            ("c".to_owned(), r(41, 13, 40, 12)),
        ];
        let b = borders(&rects, (81, 25), "b", Borders::Single);
        let at = |x: u16, y: u16| {
            b.iter()
                .find(|(bx, by, _, _)| (*bx, *by) == (x, y))
                .map(|(_, _, g, f)| (*g, *f))
        };
        assert_eq!(at(40, 0), Some(("│", true)));
        assert_eq!(at(40, 12), Some(("├", true)));
        assert_eq!(at(60, 12), Some(("─", true)));
        assert_eq!(at(40, 20), Some(("│", false)));
        assert_eq!(at(80, 12), Some(("─", true)));
        assert!(at(81, 0).is_none() && at(0, 25).is_none());
        // Spaces style draws blanks.
        assert_eq!(borders(&rects, (81, 25), "a", Borders::Spaces)[0].2, " ");
    }
}
