//! Drawing the client into a termwiz [`Surface`]: tab strip (REQ-UI-020), tiled panes with
//! borders in the daemon's gaps, floats on top (REQ-UI-011), copy mode, the status line and
//! overlays. The host diffs successive surfaces for the terminal (REQ-UI-050).

use femux_plugin::{Overlay as PluginOverlay, Span, Style};
use femux_proto::msg as m;
use termwiz::cell::{Blink, CellAttributes, Intensity, Underline};
use termwiz::color::{ColorAttribute, SrgbaTuple};
use termwiz::surface::{Change, CursorVisibility, Position, Surface};

use crate::app::{App, Mode, Overlay};
use crate::bindings::Table;
use crate::config::{Borders, TabsPosition};
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

fn attr(fg: ColorAttribute, bg: ColorAttribute) -> CellAttributes {
    let mut a = CellAttributes::default();
    a.set_foreground(fg);
    a.set_background(bg);
    a
}

fn bold(mut a: CellAttributes) -> CellAttributes {
    a.set_intensity(Intensity::Bold);
    a
}

struct Canvas<'a> {
    s: &'a mut Surface,
    w: usize,
    h: usize,
}

impl Canvas<'_> {
    fn put(&mut self, x: usize, y: usize, text: &str, a: &CellAttributes) -> usize {
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

    fn fill(&mut self, x: usize, y: usize, n: usize, ch: char, a: &CellAttributes) {
        if n > 0 {
            let s: String = std::iter::repeat_n(ch, n).collect();
            self.put(x, y, &s, a);
        }
    }

    fn spans(
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
    let touches = |x: u16, y: u16| {
        f.is_some_and(|r| {
            let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
            let in_x = x + 1 >= x0 && x <= x1;
            let in_y = y + 1 >= y0 && y <= y1;
            ((x + 1 == x0 || x == x1) && in_y) || ((y + 1 == y0 || y == y1) && in_x)
        })
    };
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

/// Draws the whole client. `app.tab_hits` is refreshed for mouse hit testing.
pub fn render(app: &mut App, theme: &Theme, s: &mut Surface) {
    app.dirty = false;
    let (w, h) = s.dimensions();
    s.add_change(Change::ClearScreen(ColorAttribute::Default));
    let mut c = Canvas { s, w, h };
    let oy = app.top_chrome() as usize;
    let (vw, vh) = app.view_size();
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
            draw_pane(app, &mut c, theme, id, oy, &mut cursor);
        }
        if !tab.zoomed {
            for (x, y, glyph, focused) in borders(&tiled, (vw, vh), &tab.focus, app.cfg.borders) {
                let a = attr(
                    if focused { theme.accent } else { theme.muted },
                    ColorAttribute::Default,
                );
                c.put(x as usize, y as usize + oy, glyph, &a);
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
                let a = attr(
                    if focused { theme.accent } else { theme.muted },
                    ColorAttribute::Default,
                );
                let g = glyphs(app.cfg.borders);
                let (x0, y0) = (r.x as i32 - 1, r.y as i32 - 1 + oy as i32);
                let (x1, y1) = (r.x as i32 + r.w as i32, r.y as i32 + r.h as i32 + oy as i32);
                let limit_y = oy as i32 + vh as i32;
                for y in y0..=y1 {
                    if y < oy as i32 || y >= limit_y {
                        continue;
                    }
                    for x in x0..=x1 {
                        if x < 0 || x >= vw as i32 {
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
                if !title.is_empty() && y0 >= oy as i32 {
                    c.put(
                        (r.x as usize + 1).min(w),
                        y0 as usize,
                        &format!(" {title} "),
                        &bold(a.clone()),
                    );
                }
                draw_pane(app, &mut c, theme, &f.id, oy, &mut cursor);
            }
        }
    }

    // Tab strip.
    app.tab_hits.clear();
    let strip_row = match app.cfg.tabs {
        TabsPosition::Top => Some(0usize),
        TabsPosition::Bottom => Some(h.saturating_sub(2)),
        TabsPosition::Hidden => None,
    };
    if let Some(y) = strip_row {
        c.fill(0, y, w, ' ', &attr(theme.bar_fg, theme.bar_bg));
        let mut x = 0usize;
        let tabs = app.model.tabs.clone();
        for (i, t) in tabs.iter().enumerate() {
            let mut label = format!(" {} {}", i + 1, app.model.tab_title(t));
            if app.model.tab_running(t) {
                label.push(' ');
                label.push_str(&app.cfg.running_indicator);
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
            let x1 = c.put(x, y, &label, &a);
            app.tab_hits.push((x as u16, x1 as u16, t.id.clone()));
            x = x1 + 1;
            if x >= w {
                break;
            }
        }
    }

    // Status line.
    let sy = h.saturating_sub(1);
    c.fill(0, sy, w, ' ', &attr(theme.bar_fg, theme.bar_bg));
    let mut x = c.put(
        0,
        sy,
        &format!(" {} ", app.model.session),
        &bold(attr(theme.accent_fg, theme.accent)),
    );
    let (label, a) = match (app.prefix_armed(), app.mode) {
        (true, _) => (" PREFIX ", bold(attr(theme.accent_fg, theme.warn))),
        (false, Mode::Terminal) => (" TERMINAL ", attr(theme.muted, theme.bar_bg)),
        (false, Mode::Normal) => (" NORMAL ", bold(attr(theme.accent_fg, theme.ok))),
        (false, Mode::Copy) => (" COPY ", bold(attr(theme.accent_fg, theme.accent))),
    };
    x = c.put(x, sy, label, &a);
    if let Some(d) = app.preselect {
        x = c.put(
            x + 1,
            sy,
            &format!(
                "next split: {}",
                crate::bindings::Action::Preselect(d).describe()
            ),
            &attr(theme.warn, theme.bar_bg),
        );
    }
    if let Some(cm) = &app.copy
        && let Some(t) = &cm.typing
    {
        let lead = if t.dir == m::SearchDir::Down {
            "/"
        } else {
            "?"
        };
        x = c.put(
            x + 1,
            sy,
            &format!("{lead}{}", t.regex),
            &attr(theme.text, theme.bar_bg),
        );
        cursor = Some((x, sy));
    } else if let Some(msg) = &app.message {
        x = c.put(x + 1, sy, msg, &attr(theme.warn, theme.bar_bg));
    }
    let _ = x;
    let right = match (&app.copy, app.mode) {
        (Some(cm), Mode::Copy) => format!("line {} col {}  ", cm.cur.0, cm.cur.1),
        _ => format!("prefix {}  ", app.prefix_label()),
    };
    let rx = w.saturating_sub(right.chars().count());
    c.put(rx, sy, &right, &attr(theme.muted, theme.bar_bg));

    // Overlays.
    match app.overlay.clone() {
        Overlay::None => {}
        Overlay::WhichKey => {
            if let Some(o) = app.plugin_overlay() {
                draw_panel(&mut c, theme, &o, sy);
            }
        }
        Overlay::Help => {
            let o = help_overlay(app);
            draw_box(&mut c, theme, &o, &mut cursor, None);
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
            let o = PluginOverlay {
                title: vec![Span::new("> ", Style::Accent), Span::text(p.input.clone())],
                lines,
                place: femux_plugin::Place::Center,
            };
            draw_box(
                &mut c,
                theme,
                &o,
                &mut cursor,
                Some(2 + p.input.chars().count()),
            );
        }
        Overlay::Confirm { text, .. } => {
            c.fill(0, sy, w, ' ', &attr(theme.accent_fg, theme.warn));
            c.put(1, sy, &text, &bold(attr(theme.accent_fg, theme.warn)));
            cursor = None;
        }
        Overlay::Prompt { title, input, .. } => {
            c.fill(0, sy, w, ' ', &attr(theme.text, theme.bar_bg));
            let x = c.put(
                1,
                sy,
                &format!("{title}: "),
                &bold(attr(theme.accent, theme.bar_bg)),
            );
            let x = c.put(x, sy, &input, &attr(theme.text, theme.bar_bg));
            cursor = Some((x, sy));
        }
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
    oy: usize,
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
        draw_copy(cm, c, theme, r, oy, cursor);
        return;
    }
    draw_screen(screen, c, oy);
    if focused && matches!(app.overlay, Overlay::None | Overlay::WhichKey) && screen.cursor.visible
    {
        *cursor = Some((
            r.x as usize + screen.cursor.x as usize,
            oy + r.y as usize + screen.cursor.y as usize,
        ));
    }
}

/// Draws a pane's rows as runs of equally styled cells: one cursor move, one attribute
/// change and one text per run rather than per cell.
fn draw_screen(screen: &PaneScreen, c: &mut Canvas, oy: usize) {
    let r = &screen.rect;
    for (y, row) in screen.rows.iter().enumerate().take(r.h as usize) {
        let sy = oy + r.y as usize + y;
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
                    c.put(r.x as usize + run_x, sy, &run, &a);
                }
                run.clear();
                run_x = x;
                run_attr = Some(attr);
            }
            run.push_str(text);
            x += width;
        }
        if let Some(a) = run_attr {
            c.put(r.x as usize + run_x, sy, &run, &a);
        }
    }
}

fn draw_copy(
    cm: &crate::copy::CopyMode,
    c: &mut Canvas,
    theme: &Theme,
    r: &m::Rect,
    oy: usize,
    cursor: &mut Option<(usize, usize)>,
) {
    for row in 0..r.h {
        let line = cm.top + row as i64;
        let sy = oy + r.y as usize + row as usize;
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
            c.put(r.x as usize + x as usize, sy, &text, &a);
            x += cell.map(|cl| cl.width.max(1)).unwrap_or(1) as u16;
        }
    }
    if cm.typing.is_none() {
        *cursor = Some((
            r.x as usize + cm.cur.1 as usize,
            oy + r.y as usize + (cm.cur.0 - cm.top).max(0) as usize,
        ));
    }
}

/// A full-width panel above the status line (which-key).
fn draw_panel(c: &mut Canvas, theme: &Theme, o: &PluginOverlay, status_row: usize) {
    let height = (o.lines.len() + 2).min(status_row);
    let top = status_row - height;
    for y in top..status_row {
        c.fill(0, y, c.w, ' ', &attr(theme.text, theme.overlay_bg));
    }
    c.spans(1, top, &o.title, theme, theme.overlay_bg);
    for (i, line) in o.lines.iter().enumerate().take(height.saturating_sub(2)) {
        c.spans(1, top + 1 + i, line, theme, theme.overlay_bg);
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
        draw_screen(&screen, &mut c, 0);
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
