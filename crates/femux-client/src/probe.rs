//! Host terminal capability probe, run once before the handshake: OSC 10/11/12 colors
//! (REQ-UI-051, and `hello.term` for REQ-QRY-002), synchronized output (mode 2026,
//! REQ-UI-050), light/dark (mode 2031 via DSR 996), the Kitty keyboard flags, Kitty
//! graphics (a query image, REQ-GFX-001), the cell size in pixels (CSI 16 t), and DA1 last
//! because every terminal answers it, which ends the probe. Bytes that are not answers are
//! kept for the input parser.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
    pub cursor: Option<[u8; 3]>,
    pub sync_output: bool,
    pub dark: Option<bool>,
    pub kitty_keyboard: bool,
    /// The host answered a Kitty graphics query with OK.
    pub kitty_graphics: bool,
    /// The host read an image from shared memory (Kitty `t=s`, ADR-021 #4).
    pub kitty_shm: bool,
    /// One cell in pixels (width, height).
    pub cell: Option<(u16, u16)>,
    pub sixel: bool,
    /// DA1 answered: the probe is complete.
    pub done: bool,
    pub leftover: Vec<u8>,
}

/// The queries, DA1 last.
pub const QUERIES: &str = "\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\\x1b[?2026$p\x1b[?996n\x1b[?u\
     \x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c";

fn rgb(spec: &str) -> Option<[u8; 3]> {
    // rgb:RRRR/GGGG/BBBB (any width 1-4 per channel), or #RRGGBB.
    if let Some(rest) = spec.strip_prefix("rgb:") {
        let mut out = [0u8; 3];
        for (i, part) in rest.split('/').take(3).enumerate() {
            let v = u16::from_str_radix(part, 16).ok()?;
            out[i] = match part.len() {
                1 => (v * 17) as u8,
                2 => v as u8,
                3 => (v >> 4) as u8,
                _ => (v >> 8) as u8,
            };
        }
        return Some(out);
    }
    let hex = spec.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Parses everything in `buf`; incomplete trailing sequences stay in `leftover`.
pub fn parse(buf: &[u8]) -> Probe {
    let mut p = Probe::default();
    let mut i = 0;
    while i < buf.len() {
        if buf[i] != 0x1b || i + 1 >= buf.len() {
            p.leftover.push(buf[i]);
            i += 1;
            continue;
        }
        match buf[i + 1] {
            b']' => {
                // OSC ... (BEL | ESC \)
                let start = i + 2;
                let mut j = start;
                let mut end = None;
                while j < buf.len() {
                    if buf[j] == 0x07 {
                        end = Some((j, j + 1));
                        break;
                    }
                    if buf[j] == 0x1b && buf.get(j + 1) == Some(&b'\\') {
                        end = Some((j, j + 2));
                        break;
                    }
                    j += 1;
                }
                let Some((e, next)) = end else {
                    p.leftover.extend_from_slice(&buf[i..]);
                    return p;
                };
                let body = String::from_utf8_lossy(&buf[start..e]).into_owned();
                let mut handled = false;
                for (code, slot) in [("10;", 0), ("11;", 1), ("12;", 2)] {
                    if let Some(spec) = body.strip_prefix(code) {
                        let c = rgb(spec.trim());
                        match slot {
                            0 => p.fg = c,
                            1 => p.bg = c,
                            _ => p.cursor = c,
                        }
                        handled = true;
                    }
                }
                if !handled {
                    p.leftover.extend_from_slice(&buf[i..next]);
                }
                i = next;
            }
            b'[' => {
                let start = i + 2;
                let mut j = start;
                while j < buf.len() && !(0x40..=0x7e).contains(&buf[j]) {
                    j += 1;
                }
                if j >= buf.len() {
                    p.leftover.extend_from_slice(&buf[i..]);
                    return p;
                }
                let params = String::from_utf8_lossy(&buf[start..j]).into_owned();
                let fin = buf[j];
                let next = j + 1;
                let handled = match (params.as_str(), fin) {
                    (q, b'y') if q.starts_with("?2026;") && q.ends_with('$') => {
                        let v = q[6..q.len() - 1].parse::<u8>().unwrap_or(0);
                        p.sync_output = v == 1 || v == 2;
                        true
                    }
                    (q, b'n') if q.starts_with("?997;") => {
                        p.dark = match &q[5..] {
                            "1" => Some(true),
                            "2" => Some(false),
                            _ => None,
                        };
                        true
                    }
                    (q, b'u') if q.starts_with('?') => {
                        p.kitty_keyboard = true;
                        true
                    }
                    (q, b't') if q.starts_with("6;") => {
                        let v: Vec<u16> =
                            q[2..].split(';').filter_map(|x| x.parse().ok()).collect();
                        if let [h, w] = v[..]
                            && w > 0
                            && h > 0
                        {
                            p.cell = Some((w, h));
                        }
                        true
                    }
                    (q, b'c') if q.starts_with('?') => {
                        p.sixel = q[1..].split(';').any(|x| x == "4");
                        p.done = true;
                        true
                    }
                    _ => false,
                };
                if !handled {
                    p.leftover.extend_from_slice(&buf[i..next]);
                }
                i = next;
            }
            b'_' => {
                // APC ... ESC \ : the Kitty graphics answer `Gi=31;OK`.
                let start = i + 2;
                let mut j = start;
                while j + 1 < buf.len() && !(buf[j] == 0x1b && buf[j + 1] == b'\\') {
                    j += 1;
                }
                if j + 1 >= buf.len() {
                    p.leftover.extend_from_slice(&buf[i..]);
                    return p;
                }
                let body = String::from_utf8_lossy(&buf[start..j]).into_owned();
                if body.starts_with("Gi=31") {
                    p.kitty_graphics |= body.ends_with(";OK");
                } else if body.starts_with("Gi=32") {
                    p.kitty_shm |= body.ends_with(";OK");
                } else {
                    p.leftover.extend_from_slice(&buf[i..j + 2]);
                }
                i = j + 2;
            }
            _ => {
                p.leftover.push(buf[i]);
                i += 1;
            }
        }
    }
    p
}

/// Writes the queries and reads answers until DA1 or `timeout`.
/// The queries with a shared memory query for `shm` (an object holding one black pixel),
/// DA1 still last.
pub fn queries(shm: Option<&str>) -> String {
    let Some(name) = shm else {
        return QUERIES.to_owned();
    };
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(name);
    let (head, da1) = QUERIES.split_at(QUERIES.len() - "\x1b[c".len());
    format!("{head}\x1b_Gi=32,s=1,v=1,a=q,t=s,f=24;{b64}\x1b\\{da1}")
}

pub fn run(term: &mut crate::term::Term, timeout: std::time::Duration) -> Probe {
    // A pixel in shared memory asks whether the host reads that medium; the host unlinks it
    // when it does, and so do we afterwards.
    let name = format!("/femux-probe-{}", std::process::id());
    let shm = crate::shm::write(&name, &[0, 0, 0])
        .is_ok()
        .then_some(name.as_str());
    let written = term.write(queries(shm).as_bytes());
    let probe = if written.is_err() {
        Probe::default()
    } else {
        read_answers(term, timeout)
    };
    if let Some(n) = shm {
        crate::shm::unlink(n);
    }
    probe
}

fn read_answers(term: &mut crate::term::Term, timeout: std::time::Duration) -> Probe {
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match term.read_raw(left) {
            Ok(Some(bytes)) => buf.extend_from_slice(&bytes),
            Ok(None) | Err(_) => break,
        }
        if parse(&buf).done {
            break;
        }
    }
    parse(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_answers_and_keeps_the_rest() {
        let input = b"\x1b]10;rgb:dcdc/dcdc/dcdc\x1b\\\x1b]11;rgb:1414/1414/1414\x07\x1b[?2026;2$y\x1b[?997;1n\x1b[?1u\x1b[?62;4;22cxy\x1b[A";
        let p = parse(input);
        assert_eq!(p.fg, Some([0xdc, 0xdc, 0xdc]));
        assert_eq!(p.bg, Some([0x14, 0x14, 0x14]));
        assert!(p.sync_output && p.kitty_keyboard && p.sixel && p.done);
        assert_eq!(p.dark, Some(true));
        assert_eq!(p.leftover, b"xy\x1b[A");
        // No answers at all: everything is input.
        let none = parse(b"hello\r");
        assert!(!none.done && none.bg.is_none());
        assert_eq!(none.leftover, b"hello\r");
        // Kitty graphics and the cell size.
        let k = parse(b"\x1b_Gi=31;OK\x1b\\\x1b[6;18;9t\x1b[?62c");
        assert!(k.kitty_graphics && k.done);
        assert_eq!(k.cell, Some((9, 18)));
        assert!(k.leftover.is_empty());
        assert!(!parse(b"\x1b_Gi=31;ENOTSUPPORTED:x\x1b\\\x1b[?62c").kitty_graphics);
        assert!(parse(b"\x1b_Gi=32;OK\x1b\\\x1b[?62c").kitty_shm);
        let q = queries(Some("/femux-probe-1"));
        assert!(
            q.ends_with("\x1b_Gi=32,s=1,v=1,a=q,t=s,f=24;L2ZlbXV4LXByb2JlLTE=\x1b\\\x1b[c"),
            "{q:?}"
        );
        // An unfinished answer waits.
        assert_eq!(parse(b"ab\x1b[?2026").leftover, b"ab\x1b[?2026");
        assert_eq!(rgb("#1a2b3c"), Some([0x1a, 0x2b, 0x3c]));
        assert_eq!(rgb("rgb:f/0/8"), Some([255, 0, 136]));
    }
}
