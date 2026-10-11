//! Reading scrollback and OSC 133 prompt marks.
//!
//! Lines are numbered relative to the active screen: 0 is its top row, -1 the newest
//! scrollback line, `rows - 1` its bottom row.

use std::ffi::c_void;

use crate::{Cell, Error, Terminal, cell, check, ffi};

impl Terminal {
    /// Lines of history currently retained (REQ-VT-003).
    pub fn scrollback_rows(&self) -> usize {
        let mut n: usize = 0;
        // SAFETY: SCROLLBACK_ROWS outputs a size_t.
        let _ = unsafe {
            self.get(
                ffi::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS,
                &mut n,
            )
        };
        n
    }

    /// Lowest valid line number (the oldest history line).
    pub fn first_line(&self) -> i64 {
        -(self.scrollback_rows() as i64)
    }

    fn screen_y(&self, line: i64) -> Option<u32> {
        let y = line + self.scrollback_rows() as i64;
        let last = self.scrollback_rows() as i64 + self.size().1 as i64;
        (0..last).contains(&y).then_some(y as u32)
    }

    fn grid_ref(&self, x: u16, screen_y: u32) -> Result<ffi::GhosttyGridRef, Error> {
        let mut r = ffi::GhosttyGridRef {
            size: std::mem::size_of::<ffi::GhosttyGridRef>(),
            node: std::ptr::null_mut(),
            x: 0,
            y: 0,
        };
        let point = ffi::GhosttyPoint {
            tag: ffi::GhosttyPointTag::GHOSTTY_POINT_TAG_SCREEN,
            value: ffi::GhosttyPointValue {
                coordinate: ffi::GhosttyPointCoordinate { x, y: screen_y },
            },
        };
        // SAFETY: `raw` is live; `r` is a sized out struct.
        check(unsafe { ffi::ghostty_terminal_grid_ref(self.raw, point, &mut r) })?;
        Ok(r)
    }

    /// Reads up to `count` lines starting at `from`. Lines outside history and screen are skipped.
    pub fn read_lines(&mut self, from: i64, count: u32) -> Result<Vec<Vec<Cell>>, Error> {
        let cols = self.size().0;
        let mut out = Vec::new();
        for line in from..from.saturating_add(count as i64) {
            let Some(y) = self.screen_y(line) else {
                continue;
            };
            let mut row = Vec::with_capacity(cols as usize);
            for x in 0..cols {
                let r = self.grid_ref(x, y)?;
                let mut c = Cell::default();
                let mut raw: ffi::GhosttyCell = 0;
                // SAFETY: `r` is a fresh grid ref; outputs are correctly typed.
                unsafe {
                    check(ffi::ghostty_grid_ref_cell(&r, &mut raw))?;
                    let styled = cell::decode_raw(raw, &mut c)?;
                    let mut cps = [0u32; 16];
                    let mut n = 0usize;
                    check(ffi::ghostty_grid_ref_graphemes(
                        &r,
                        cps.as_mut_ptr(),
                        cps.len(),
                        &mut n,
                    ))?;
                    c.text = cps[..n.min(cps.len())]
                        .iter()
                        .filter_map(|&cp| char::from_u32(cp))
                        .collect();
                    if styled {
                        let mut st = cell::new_style();
                        check(ffi::ghostty_grid_ref_style(&r, &mut st))?;
                        cell::apply_style(&st, &mut c);
                    }
                    cell::apply_bg_content(raw, &mut c)?;
                }
                row.push(c);
            }
            out.push(row);
        }
        Ok(out)
    }

    /// Lines whose row starts with an OSC 133 prompt (REQ-VT-005), oldest first.
    pub fn prompt_lines(&self) -> Result<Vec<i64>, Error> {
        let history = self.scrollback_rows() as i64;
        let total = history + self.size().1 as i64;
        let mut out = Vec::new();
        for y in 0..total {
            let r = self.grid_ref(0, y as u32)?;
            let mut row: ffi::GhosttyRow = 0;
            let mut prompt = ffi::GhosttyRowSemanticPrompt(0);
            // SAFETY: `r` is a fresh grid ref; outputs are correctly typed.
            unsafe {
                check(ffi::ghostty_grid_ref_row(&r, &mut row))?;
                check(ffi::ghostty_row_get(
                    row,
                    ffi::GhosttyRowData::GHOSTTY_ROW_DATA_SEMANTIC_PROMPT,
                    &mut prompt as *mut _ as *mut c_void,
                ))?;
            }
            if prompt == ffi::GhosttyRowSemanticPrompt::GHOSTTY_ROW_SEMANTIC_PROMPT {
                out.push(y - history);
            }
        }
        Ok(out)
    }

    /// Drops all scrollback (CSI 3 J); the screen is untouched.
    pub fn clear_scrollback(&mut self) {
        self.write(b"\x1b[3J");
    }
}

#[cfg(test)]
mod tests {
    use crate::{Terminal, line_text};

    #[test]
    fn scrollback_is_capped() {
        let mut t = Terminal::new(20, 5, 10_000).unwrap();
        for i in 0..30_000 {
            t.write(format!("line {i}\r\n").as_bytes());
        }
        // libghostty-vt prunes whole pages, so retained history sits within a page of the cap
        // (about 2,300 lines at 20 columns, fewer when wider).
        let n = t.scrollback_rows();
        assert!((7_500..=10_000).contains(&n), "{n}");
        let first = t.read_lines(-1, 1).unwrap();
        assert_eq!(line_text(&first[0]), "line 29995");
    }

    #[test]
    fn reads_history_and_screen() {
        let mut t = Terminal::new(10, 3, 100).unwrap();
        t.write(b"one\r\ntwo\r\nthree\r\nfour\r\n\x1b[32mfive");
        let lines: Vec<String> = t
            .read_lines(-2, 5)
            .unwrap()
            .iter()
            .map(|l| line_text(l))
            .collect();
        assert_eq!(lines, ["one", "two", "three", "four", "five"]);
        let five = &t.read_lines(2, 1).unwrap()[0];
        assert_eq!(five[0].fg, crate::Color::Indexed(2));
        assert!(t.read_lines(-50, 2).unwrap().is_empty());
        t.clear_scrollback();
        assert_eq!(t.scrollback_rows(), 0);
    }

    #[test]
    fn records_prompt_marks() {
        let mut t = Terminal::new(20, 4, 100).unwrap();
        // Two prompts with output between them; OSC 133;A marks a prompt start.
        t.write(b"\x1b]133;A\x07$ ls\r\n\x1b]133;C\x07a\r\nb\r\nc\r\n\x1b]133;A\x07$ ");
        let marks = t.prompt_lines().unwrap();
        let rows: Vec<String> = marks
            .iter()
            .map(|&l| line_text(&t.read_lines(l, 1).unwrap()[0]))
            .collect();
        assert_eq!(rows, ["$ ls", "$"]);
    }
}
