//! Copy mode (SPEC §5.1): a cursor over the focused pane's screen and scrollback, vim
//! motions, three selection shapes, regex search and prompt-mark jumps. History lines come
//! from `scrollback.read` on demand (line 0 is the top of the screen, negative lines are
//! history, ADR-015 #12); search runs in the daemon (`scrollback.search`).

use std::collections::BTreeMap;

use femux_proto::msg as m;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelKind {
    Char,
    Line,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub regex: String,
    pub dir: m::SearchDir,
}

/// What copy mode wants fetched or done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    Read {
        from: i64,
        count: u32,
    },
    Search {
        search: Search,
        from: i64,
        limit: u32,
        jump: bool,
    },
    Marks,
    Yank(String),
    Exit,
}

#[derive(Debug, Clone)]
pub struct CopyMode {
    pub pane: String,
    pub cols: u16,
    pub rows: u16,
    /// First visible line.
    pub top: i64,
    /// Oldest readable line, once known.
    pub first: Option<i64>,
    pub cur: (i64, u16),
    pub lines: BTreeMap<i64, Vec<m::Cell>>,
    pub sel: Option<(SelKind, (i64, u16))>,
    /// Search input being typed, with its direction.
    pub typing: Option<Search>,
    pub last_search: Option<Search>,
    pub matches: Vec<m::SearchMatch>,
    pub prompts: Vec<i64>,
    pending_g: bool,
}

fn cell_text(c: &m::Cell) -> &str {
    if c.ch.is_empty() { " " } else { &c.ch }
}

impl CopyMode {
    /// Starts at the bottom of the screen with the cursor where the pane's cursor is.
    pub fn new(pane: &str, cols: u16, rows: u16, cursor: (u16, u16)) -> (CopyMode, Vec<Need>) {
        let cm = CopyMode {
            pane: pane.to_owned(),
            cols: cols.max(1),
            rows: rows.max(1),
            top: 0,
            first: None,
            cur: (cursor.1.min(rows.saturating_sub(1)) as i64, cursor.0),
            lines: BTreeMap::new(),
            sel: None,
            typing: None,
            last_search: None,
            matches: Vec::new(),
            prompts: Vec::new(),
            pending_g: false,
        };
        let needs = vec![
            Need::Read {
                from: 0,
                count: rows.max(1) as u32,
            },
            Need::Marks,
        ];
        (cm, needs)
    }

    pub fn last_line(&self) -> i64 {
        self.rows as i64 - 1
    }

    pub fn on_read(&mut self, from: i64, lines: &[Vec<m::Cell>], first_line: i64) {
        self.first = Some(first_line);
        // Lines before `first_line` were skipped, so the returned ones start at max(from, first).
        let start = from.max(first_line);
        for (i, l) in lines.iter().enumerate() {
            self.lines.insert(start + i as i64, l.clone());
        }
        self.clamp();
    }

    pub fn on_marks(&mut self, prompts: Vec<i64>) {
        self.prompts = prompts;
    }

    pub fn on_search(&mut self, matches: Vec<m::SearchMatch>, jump: bool) {
        if jump {
            if let Some(mt) = matches.first() {
                self.cur = (mt.line, mt.col);
                self.scroll_to_cursor();
            }
        } else {
            self.matches = matches;
        }
    }

    fn clamp(&mut self) {
        let first = self.first.unwrap_or(i64::MIN / 2);
        self.cur.0 = self.cur.0.clamp(first, self.last_line());
        self.cur.1 = self.cur.1.min(self.cols - 1);
        self.top = self
            .top
            .clamp(first, self.last_line() - self.rows as i64 + 1);
    }

    /// Lines of the viewport not yet cached.
    fn missing(&self) -> Option<Need> {
        let first = self.first.unwrap_or(i64::MIN / 2);
        let want: Vec<i64> = (self.top..self.top + self.rows as i64)
            .filter(|l| *l >= first && !self.lines.contains_key(l))
            .collect();
        let (lo, hi) = (*want.first()?, *want.last()?);
        Some(Need::Read {
            from: lo,
            count: (hi - lo + 1) as u32,
        })
    }

    /// Re-clamps after new data and says which viewport lines are still missing.
    pub fn refresh(&mut self) -> Vec<Need> {
        self.clamp();
        self.scroll_to_cursor();
        self.missing().into_iter().collect()
    }

    fn scroll_to_cursor(&mut self) {
        if self.cur.0 < self.top {
            self.top = self.cur.0;
        } else if self.cur.0 >= self.top + self.rows as i64 {
            self.top = self.cur.0 - self.rows as i64 + 1;
        }
        self.clamp();
    }

    fn line_text(&self, line: i64) -> String {
        self.lines
            .get(&line)
            .map(|l| l.iter().map(cell_text).collect::<String>())
            .unwrap_or_default()
    }

    fn line_len(&self, line: i64) -> u16 {
        let t = self.line_text(line);
        t.trim_end().chars().count().max(1) as u16
    }

    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    fn chars(&self, line: i64) -> Vec<char> {
        let mut v: Vec<char> = self.line_text(line).chars().collect();
        v.resize(self.cols as usize, ' ');
        v
    }

    fn word_next(&mut self) {
        let chars = self.chars(self.cur.0);
        let mut x = self.cur.1 as usize;
        let start_word = chars.get(x).copied().is_some_and(Self::is_word);
        while x < chars.len() && Self::is_word(chars[x]) == start_word && chars[x] != ' ' {
            x += 1;
        }
        while x < chars.len() && chars[x] == ' ' {
            x += 1;
        }
        if x >= self.line_len(self.cur.0) as usize && self.cur.0 < self.last_line() {
            self.cur.0 += 1;
            let next = self.chars(self.cur.0);
            self.cur.1 = next.iter().position(|c| *c != ' ').unwrap_or(0) as u16;
        } else {
            self.cur.1 = x.min(self.cols as usize - 1) as u16;
        }
    }

    fn word_prev(&mut self) {
        if self.cur.1 == 0 {
            let first = self.first.unwrap_or(0);
            if self.cur.0 > first {
                self.cur.0 -= 1;
                self.cur.1 = self.line_len(self.cur.0).saturating_sub(1);
            }
        }
        let chars = self.chars(self.cur.0);
        let mut x = self.cur.1 as usize;
        while x > 0 && chars[x - 1] == ' ' {
            x -= 1;
        }
        if x == 0 {
            self.cur.1 = 0;
            return;
        }
        let kind = Self::is_word(chars[x - 1]);
        while x > 0 && chars[x - 1] != ' ' && Self::is_word(chars[x - 1]) == kind {
            x -= 1;
        }
        self.cur.1 = x as u16;
    }

    fn word_end(&mut self) {
        let chars = self.chars(self.cur.0);
        let mut x = self.cur.1 as usize + 1;
        while x < chars.len() && chars[x] == ' ' {
            x += 1;
        }
        if x >= chars.len() {
            self.cur.1 = self.line_len(self.cur.0).saturating_sub(1);
            return;
        }
        let kind = Self::is_word(chars[x]);
        while x + 1 < chars.len() && chars[x + 1] != ' ' && Self::is_word(chars[x + 1]) == kind {
            x += 1;
        }
        self.cur.1 = x as u16;
    }

    /// Handles a key while a search pattern is being typed; `Some(needs)` if consumed.
    pub fn on_search_key(&mut self, key: &m::KeyEvent) -> Option<Vec<Need>> {
        let typing = self.typing.as_mut()?;
        match key.key.as_str() {
            "escape" => {
                self.typing = None;
                Some(vec![])
            }
            "enter" => {
                let s = self.typing.take().unwrap();
                if s.regex.is_empty() {
                    return Some(vec![]);
                }
                self.last_search = Some(s);
                Some(self.search_needs(false))
            }
            "backspace" => {
                typing.regex.pop();
                Some(vec![])
            }
            _ => {
                if let Some(t) = &key.text
                    && !key.mods.contains(&m::Mod::Ctrl)
                {
                    typing.regex.push_str(t);
                }
                Some(vec![])
            }
        }
    }

    /// Requests for the last search: a jump from the cursor, then highlights for the view.
    fn search_needs(&self, reverse: bool) -> Vec<Need> {
        let Some(s) = &self.last_search else {
            return vec![];
        };
        let mut dir = s.dir;
        if reverse {
            dir = match dir {
                m::SearchDir::Up => m::SearchDir::Down,
                m::SearchDir::Down => m::SearchDir::Up,
            };
        }
        let from = match dir {
            m::SearchDir::Down => self.cur.0 + 1,
            m::SearchDir::Up => self.cur.0 - 1,
        };
        let jump = Search {
            regex: s.regex.clone(),
            dir,
        };
        vec![
            Need::Search {
                search: jump,
                from,
                limit: 1,
                jump: true,
            },
            Need::Search {
                search: Search {
                    regex: s.regex.clone(),
                    dir: m::SearchDir::Down,
                },
                from: self.first.unwrap_or(-10_000),
                limit: 500,
                jump: false,
            },
        ]
    }

    /// Runs a copy-mode action; returns what to fetch or do.
    pub fn act(&mut self, a: crate::bindings::Action) -> Vec<Need> {
        use crate::bindings::Action::*;
        let half = (self.rows / 2).max(1) as i64;
        let mut needs = Vec::new();
        let was_g = std::mem::take(&mut self.pending_g);
        match a {
            CopyLeft => self.cur.1 = self.cur.1.saturating_sub(1),
            CopyRight => self.cur.1 = (self.cur.1 + 1).min(self.cols - 1),
            CopyUp => self.cur.0 -= 1,
            CopyDown => self.cur.0 += 1,
            CopyWordNext => self.word_next(),
            CopyWordPrev => self.word_prev(),
            CopyWordEnd => self.word_end(),
            CopyLineStart => self.cur.1 = 0,
            CopyLineEnd => self.cur.1 = self.line_len(self.cur.0).saturating_sub(1),
            CopyTop => {
                if was_g {
                    // gg: the oldest line. Before the history size is known, go as far back
                    // as the daemon will tell us about.
                    self.cur.0 = self.first.unwrap_or(i64::MIN / 2);
                    self.cur.1 = 0;
                    if self.first.is_none() {
                        self.top = -(10_000 - self.rows as i64);
                        self.cur.0 = self.top;
                    }
                } else {
                    self.pending_g = true;
                    return needs;
                }
            }
            CopyBottom => {
                self.cur.0 = self.last_line();
                self.cur.1 = 0;
            }
            CopyHalfUp => {
                self.cur.0 -= half;
                self.top -= half;
            }
            CopyHalfDown => {
                self.cur.0 += half;
                self.top += half;
            }
            CopySearchDown => {
                self.typing = Some(Search {
                    regex: String::new(),
                    dir: m::SearchDir::Down,
                });
            }
            CopySearchUp => {
                self.typing = Some(Search {
                    regex: String::new(),
                    dir: m::SearchDir::Up,
                });
            }
            CopyNext => return self.search_needs(false),
            CopyPrev => return self.search_needs(true),
            CopySelectChar => self.toggle_sel(SelKind::Char),
            CopySelectLine => self.toggle_sel(SelKind::Line),
            CopySelectBlock => self.toggle_sel(SelKind::Block),
            CopyYank => {
                if let Some(text) = self.selection_text() {
                    needs.push(Need::Yank(text));
                }
                needs.push(Need::Exit);
                return needs;
            }
            CopyPromptPrev => {
                if let Some(l) = self.prompts.iter().rev().find(|l| **l < self.cur.0) {
                    self.cur = (*l, 0);
                }
            }
            CopyPromptNext => {
                if let Some(l) = self.prompts.iter().find(|l| **l > self.cur.0) {
                    self.cur = (*l, 0);
                }
            }
            CopyExit => return vec![Need::Exit],
            CopyClear => {
                if self.sel.take().is_none() && self.matches.is_empty() {
                    return vec![Need::Exit];
                }
                self.matches.clear();
            }
            _ => {}
        }
        self.clamp();
        self.scroll_to_cursor();
        needs.extend(self.missing());
        needs
    }

    fn toggle_sel(&mut self, kind: SelKind) {
        match self.sel {
            Some((k, _)) if k == kind => self.sel = None,
            Some((_, anchor)) => self.sel = Some((kind, anchor)),
            None => self.sel = Some((kind, self.cur)),
        }
    }

    /// The selected cells as `(line, x0, x1)` inclusive ranges per line, oldest first.
    pub fn selection(&self) -> Vec<(i64, u16, u16)> {
        let Some((kind, anchor)) = self.sel else {
            return vec![];
        };
        let (a, b) = if anchor <= self.cur {
            (anchor, self.cur)
        } else {
            (self.cur, anchor)
        };
        let mut out = Vec::new();
        match kind {
            SelKind::Line => {
                for l in a.0..=b.0 {
                    out.push((l, 0, self.cols - 1));
                }
            }
            SelKind::Char => {
                for l in a.0..=b.0 {
                    let x0 = if l == a.0 { a.1 } else { 0 };
                    let x1 = if l == b.0 { b.1 } else { self.cols - 1 };
                    out.push((l, x0, x1));
                }
            }
            SelKind::Block => {
                let (x0, x1) = (anchor.1.min(self.cur.1), anchor.1.max(self.cur.1));
                for l in a.0..=b.0 {
                    out.push((l, x0, x1));
                }
            }
        }
        out
    }

    /// Text of the selection, lines joined by `\n`; `None` without a selection.
    pub fn selection_text(&self) -> Option<String> {
        let sel = self.selection();
        if sel.is_empty() {
            return None;
        }
        let kind = self.sel.map(|(k, _)| k).unwrap();
        let n = sel.len();
        let mut lines = Vec::new();
        for (i, (l, x0, x1)) in sel.into_iter().enumerate() {
            let chars = self.chars(l);
            let mut s: String = chars[x0 as usize..=(x1 as usize).min(chars.len() - 1)]
                .iter()
                .collect();
            // Lines that run to the right edge lose their padding; a character selection
            // keeps what was chosen on its last line, a block keeps its shape.
            let to_edge = x1 as usize + 1 >= chars.len();
            if kind == SelKind::Line || (kind == SelKind::Char && to_edge && i + 1 < n) {
                s = s.trim_end().to_owned();
            }
            lines.push(s);
        }
        Some(lines.join("\n"))
    }

    pub fn is_selected(&self, line: i64, x: u16) -> bool {
        self.selection()
            .iter()
            .any(|(l, x0, x1)| *l == line && (*x0..=*x1).contains(&x))
    }

    pub fn is_match(&self, line: i64, x: u16) -> bool {
        self.matches
            .iter()
            .any(|mt| mt.line == line && x >= mt.col && x < mt.col + mt.len.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::{CopyMode, Need, m};
    use crate::bindings::Action::*;

    fn cells(s: &str) -> Vec<m::Cell> {
        s.chars()
            .map(|c| m::Cell {
                ch: c.to_string(),
                width: 1,
                ..Default::default()
            })
            .collect()
    }

    fn mode() -> CopyMode {
        let (mut cm, needs) = CopyMode::new("P", 10, 3, (2, 2));
        assert_eq!(needs, [Need::Read { from: 0, count: 3 }, Need::Marks]);
        cm.on_read(
            0,
            &[cells("one two"), cells("$ ls -la"), cells("hi there")],
            -2,
        );
        cm.on_read(-2, &[cells("older"), cells("old_1 x")], -2);
        cm
    }

    #[test]
    fn motions_and_scrolling() {
        let mut cm = mode();
        assert_eq!(cm.cur, (2, 2));
        cm.act(CopyLineEnd);
        assert_eq!(cm.cur, (2, 7));
        cm.act(CopyLineStart);
        cm.act(CopyWordNext);
        assert_eq!(cm.cur, (2, 3));
        cm.act(CopyWordEnd);
        assert_eq!(cm.cur, (2, 7));
        cm.act(CopyWordPrev);
        assert_eq!(cm.cur, (2, 3));
        cm.act(CopyUp);
        cm.act(CopyUp);
        cm.act(CopyUp);
        assert_eq!((cm.cur.0, cm.top), (-1, -1));
        cm.act(CopyTop);
        cm.act(CopyTop);
        assert_eq!((cm.cur, cm.top), ((-2, 0), -2));
        cm.act(CopyBottom);
        assert_eq!((cm.cur.0, cm.top), (2, 0));
        cm.act(CopyHalfUp);
        assert_eq!(cm.cur.0, 1);
    }

    #[test]
    fn selections_yank_text() {
        let mut cm = mode();
        cm.act(CopyLineStart);
        cm.act(CopySelectChar);
        cm.act(CopyRight);
        cm.act(CopyRight);
        assert_eq!(cm.selection_text().as_deref(), Some("hi "));
        cm.act(CopySelectLine);
        cm.act(CopyUp);
        assert_eq!(cm.selection_text().as_deref(), Some("$ ls -la\nhi there"));
        cm.act(CopySelectBlock);
        assert_eq!(cm.selection_text().as_deref(), Some("$ l\nhi "));
        let needs = cm.act(CopyYank);
        assert_eq!(needs, [Need::Yank("$ l\nhi ".into()), Need::Exit]);
    }

    #[test]
    fn search_and_prompt_marks() {
        let mut cm = mode();
        cm.on_marks(vec![-2, 1]);
        cm.act(CopyPromptPrev);
        assert_eq!(cm.cur, (1, 0));
        cm.act(CopyPromptPrev);
        assert_eq!(cm.cur, (-2, 0));
        cm.act(CopyPromptNext);
        assert_eq!(cm.cur, (1, 0));
        cm.act(CopySearchUp);
        for ch in "old".chars() {
            let k = m::KeyEvent {
                key: ch.to_string(),
                mods: vec![],
                text: Some(ch.to_string()),
                event: m::KeyEventKind::Press,
            };
            assert!(cm.on_search_key(&k).is_some());
        }
        let needs = cm
            .on_search_key(&m::KeyEvent {
                key: "enter".into(),
                mods: vec![],
                text: None,
                event: m::KeyEventKind::Press,
            })
            .unwrap();
        assert!(
            matches!(&needs[0], Need::Search { search, from: 0, limit: 1, jump: true } if search.regex == "old" && search.dir == m::SearchDir::Up)
        );
        cm.on_search(
            vec![m::SearchMatch {
                line: -1,
                col: 0,
                len: 5,
            }],
            true,
        );
        assert_eq!(cm.cur, (-1, 0));
        // n repeats, N reverses.
        let n = cm.act(CopyNext);
        assert!(matches!(&n[0], Need::Search { from: -2, .. }));
        let p = cm.act(CopyPrev);
        assert!(
            matches!(&p[0], Need::Search { from: 0, search, .. } if search.dir == m::SearchDir::Down)
        );
        assert_eq!(cm.act(CopyExit), [Need::Exit]);
    }
}
