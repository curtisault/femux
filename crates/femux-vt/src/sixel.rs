//! Sixel images (REQ-GFX-002), and one repair to Kitty commands on the way through: shared
//! memory names without the leading `/` (mpv's) get it, as Kitty accepts them.
//!
//! Sixel images: The pane program's `DCS … q … ST` never reaches
//! libghostty-vt: it is decoded to RGBA here and handed over as a Kitty image displayed at
//! the cursor, so sixels scroll, clip, survive reattach and are drawn like Kitty images.

/// The most a sixel sequence may hold before it is dropped (64 MiB, REQ-GFX-004).
const MAX_SEQUENCE: usize = 64 * 1024 * 1024;
/// The largest decoded image side.
const MAX_SIDE: usize = 10_000;

/// The VT340's default 16 color registers, as RGB.
const VT340: [[u8; 3]; 16] = [
    [0, 0, 0],
    [51, 51, 204],
    [204, 36, 36],
    [51, 204, 51],
    [204, 51, 204],
    [51, 204, 204],
    [204, 204, 51],
    [120, 120, 120],
    [69, 69, 69],
    [87, 87, 153],
    [153, 69, 69],
    [87, 153, 87],
    [153, 87, 153],
    [87, 153, 153],
    [153, 153, 87],
    [204, 204, 204],
];

/// A decoded sixel image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sixel {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

fn pct(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}

/// HLS (hue 0-360 with blue at 0, lightness and saturation 0-100) to RGB, as DEC defines it.
fn hls(h: u32, l: u32, s: u32) -> [u8; 3] {
    let (l, s) = (l.min(100) as f64 / 100.0, s.min(100) as f64 / 100.0);
    // DEC hue 0 is blue; shift so 0 is red.
    let h = ((h as f64 + 240.0) % 360.0) / 360.0;
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |t: f64| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

/// Reads decimal parameters separated by `;` starting at `i`; returns them and the next index.
fn params(data: &[u8], mut i: usize) -> (Vec<u32>, usize) {
    let mut out = vec![0u32];
    while i < data.len() {
        match data[i] {
            b'0'..=b'9' => {
                let last = out.last_mut().unwrap();
                *last = last
                    .saturating_mul(10)
                    .saturating_add((data[i] - b'0') as u32);
            }
            b';' => out.push(0),
            _ => break,
        }
        i += 1;
    }
    (out, i)
}

/// Decodes the sixel data that follows the `q` of a DCS (without the terminator). Undrawn
/// pixels are transparent.
pub fn decode(data: &[u8]) -> Option<Sixel> {
    let mut palette: Vec<[u8; 3]> = VT340.to_vec();
    palette.resize(256, [0, 0, 0]);
    let mut color = 0usize;
    // Pixels as (x, y) -> palette index + 1 (0: undrawn), grown as needed.
    let mut width = 0usize;
    let mut declared: Option<(usize, usize)> = None;
    let mut rows: Vec<Vec<u16>> = Vec::new();
    let (mut x, mut band) = (0usize, 0usize);
    let mut i = 0;
    let put =
        |x: usize, band: usize, bits: u8, color: usize, n: usize, rows: &mut Vec<Vec<u16>>| {
            for b in 0..6 {
                if bits & (1 << b) == 0 {
                    continue;
                }
                let y = band * 6 + b;
                if y >= MAX_SIDE || x + n > MAX_SIDE {
                    return;
                }
                if rows.len() <= y {
                    rows.resize(y + 1, Vec::new());
                }
                let row = &mut rows[y];
                if row.len() < x + n {
                    row.resize(x + n, 0);
                }
                for px in &mut row[x..x + n] {
                    *px = color as u16 + 1;
                }
            }
        };
    while i < data.len() {
        let c = data[i];
        match c {
            b'"' => {
                let (p, next) = params(data, i + 1);
                if let [_, _, w, h, ..] = p[..] {
                    declared = Some((w as usize, h as usize));
                }
                i = next;
                continue;
            }
            b'#' => {
                let (p, next) = params(data, i + 1);
                color = (p[0] as usize).min(255);
                match p[..] {
                    [_, 2, r, g, b] => palette[color] = [pct(r), pct(g), pct(b)],
                    [_, 1, h, l, s] => palette[color] = hls(h, l, s),
                    _ => {}
                }
                i = next;
                continue;
            }
            b'!' => {
                let (p, next) = params(data, i + 1);
                let n = (p[0] as usize).max(1);
                if let Some(&ch) = data.get(next)
                    && (0x3f..=0x7e).contains(&ch)
                {
                    put(x, band, ch - 0x3f, color, n, &mut rows);
                    x += n;
                    width = width.max(x);
                    i = next + 1;
                } else {
                    i = next;
                }
                continue;
            }
            b'$' => x = 0,
            b'-' => {
                x = 0;
                band += 1;
            }
            0x3f..=0x7e => {
                put(x, band, c - 0x3f, color, 1, &mut rows);
                x += 1;
                width = width.max(x);
            }
            _ => {}
        }
        i += 1;
    }
    let height = rows.len();
    let (width, height) = match declared {
        Some((w, h)) if w > 0 && h > 0 => (w.min(MAX_SIDE), h.min(MAX_SIDE)),
        _ => (width, height),
    };
    if width == 0 || height == 0 {
        return None;
    }
    let mut rgba = vec![0u8; width * height * 4];
    for (y, row) in rows.iter().enumerate().take(height) {
        for (x, &v) in row.iter().enumerate().take(width) {
            if v == 0 {
                continue;
            }
            let [r, g, b] = palette[v as usize - 1];
            let o = (y * width + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    Some(Sixel {
        width,
        height,
        rgba,
    })
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(B64[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The Kitty commands that display `img` as image `id` at the cursor, then a new line, as a
/// sixel leaves the cursor below the image.
pub fn as_kitty(img: &Sixel, id: u32) -> Vec<u8> {
    let b64 = base64(&img.rgba);
    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
    let mut out = Vec::new();
    for (k, c) in chunks.iter().enumerate() {
        let more = u8::from(k + 1 < chunks.len());
        let head = if k == 0 {
            format!(
                "\x1b_Ga=T,f=32,s={},v={},i={id},q=2,m={more};",
                img.width, img.height
            )
        } else {
            format!("\x1b_Gm={more};")
        };
        out.extend_from_slice(head.as_bytes());
        out.extend_from_slice(c);
        out.extend_from_slice(b"\x1b\\");
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// Where the scanner is in the byte stream.
#[derive(Debug, Default)]
enum State {
    #[default]
    Text,
    /// ESC seen at the end of a chunk.
    Esc,
    /// `ESC P` and parameters so far.
    Params(Vec<u8>),
    /// Inside a sixel's data; the flag is a pending ESC (maybe the ST).
    Data(Vec<u8>, bool),
    /// `ESC _ G` and the Kitty keys so far (up to the `;`).
    ApcKeys(Vec<u8>),
    /// A Kitty shared memory command (`t=s`): its payload is held so its name can be fixed;
    /// the flag is a pending ESC.
    ApcShm(Vec<u8>, Vec<u8>, bool),
}

/// The longest Kitty key list looked at, and the longest shared memory name.
const APC_KEYS: usize = 256;

/// Kitty shared memory keys without `m`: chunking belongs to direct transmission, and a
/// shared memory command marked `m=1` (as mpv sends them) would otherwise wait for a final
/// chunk that only the next command ends, a frame late.
fn fix_shm_keys(keys: &[u8]) -> Vec<u8> {
    // `ESC _ G` + keys + `;`
    let inner = &keys[3..keys.len() - 1];
    let kept: Vec<&[u8]> = inner
        .split(|b| *b == b',')
        .filter(|kv| !kv.starts_with(b"m="))
        .collect();
    let mut out = b"\x1b_G".to_vec();
    out.extend(kept.join(&b","[..]));
    out.push(b';');
    out
}

/// A Kitty shared memory name as libghostty-vt opens it: POSIX names start with `/`, and
/// programs such as mpv leave it out (Kitty itself adds it).
fn fix_shm_name(payload: &[u8]) -> Vec<u8> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let Ok(mut name) = b64.decode(payload) else {
        return payload.to_vec();
    };
    if !name.starts_with(b"/") {
        name.insert(0, b'/');
    }
    let name = String::from_utf8_lossy(&name).into_owned();
    let name = relay_shm(&name).unwrap_or(name);
    b64.encode(name).into_bytes()
}

/// Takes the program's shared memory object at once: opens it, removes its name and copies
/// it to a private object for libghostty-vt. A program that reuses one name for every frame
/// (mpv) then gets a fresh object for its next frame instead of one the terminal is still
/// reading and is about to unlink, which tore frames and lost the next one.
fn relay_shm(name: &str) -> Option<String> {
    use std::ffi::CString;
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let src = CString::new(name).ok()?;
    let private = format!(
        "/femux-vt-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    let dst = CString::new(private.clone()).ok()?;
    // SAFETY: plain libc calls; every mapping is unmapped and every fd closed before return.
    unsafe {
        let fd = libc::shm_open(src.as_ptr(), libc::O_RDONLY, 0);
        if fd < 0 {
            return None;
        }
        libc::shm_unlink(src.as_ptr());
        let mut st: libc::stat = std::mem::zeroed();
        let len = if libc::fstat(fd, &mut st) == 0 {
            st.st_size as usize
        } else {
            0
        };
        if len == 0 || len > MAX_SEQUENCE {
            libc::close(fd);
            return None;
        }
        let from = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        );
        libc::close(fd);
        if from == libc::MAP_FAILED {
            return None;
        }
        let out = libc::shm_open(
            dst.as_ptr(),
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
            0o600 as libc::c_uint,
        );
        let mut ok = out >= 0 && libc::ftruncate(out, len as libc::off_t) == 0;
        if ok {
            let to = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                out,
                0,
            );
            if to == libc::MAP_FAILED {
                ok = false;
            } else {
                std::ptr::copy_nonoverlapping(from as *const u8, to as *mut u8, len);
                libc::munmap(to, len);
            }
        }
        libc::munmap(from, len);
        if out >= 0 {
            libc::close(out);
        }
        if !ok {
            libc::shm_unlink(dst.as_ptr());
            return None;
        }
    }
    Some(private)
}

/// Pulls sixel sequences out of the PTY stream (they may span reads).
#[derive(Debug, Default)]
pub struct Scanner {
    state: State,
    /// Ids for the images it creates, from the top of the id space.
    next_id: u32,
}

impl Scanner {
    /// Between sequences: bytes without ESC pass straight through.
    pub fn idle(&self) -> bool {
        matches!(self.state, State::Text)
    }

    /// `bytes` with every complete sixel replaced by `replace(decoded)`; partial sequences
    /// wait for the next call.
    pub fn feed(
        &mut self,
        bytes: &[u8],
        mut replace: impl FnMut(&Sixel, u32) -> Vec<u8>,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            self.state = match std::mem::take(&mut self.state) {
                State::Text if b == 0x1b => State::Esc,
                State::Text => {
                    out.push(b);
                    State::Text
                }
                State::Esc if b == b'P' => State::Params(vec![0x1b, b'P']),
                State::Esc if b == b'_' => State::ApcKeys(vec![0x1b, b'_']),
                State::ApcKeys(mut k) => {
                    if b == b';' {
                        let keys = String::from_utf8_lossy(&k[2..]).into_owned();
                        if keys.starts_with('G') && keys[1..].split(',').any(|kv| kv == "t=s") {
                            k.push(b);
                            State::ApcShm(k, Vec::new(), false)
                        } else {
                            // Not shared memory: everything else streams through.
                            out.extend_from_slice(&k);
                            out.push(b);
                            State::Text
                        }
                    } else if b == 0x1b || k.len() > APC_KEYS {
                        out.extend_from_slice(&k);
                        if b == 0x1b {
                            State::Esc
                        } else {
                            out.push(b);
                            State::Text
                        }
                    } else {
                        k.push(b);
                        State::ApcKeys(k)
                    }
                }
                State::ApcShm(k, mut p, esc) => {
                    if esc && b == b'\\' {
                        out.extend(fix_shm_keys(&k));
                        out.extend(fix_shm_name(&p));
                        out.extend_from_slice(b"\x1b\\");
                        State::Text
                    } else if b == 0x1b {
                        State::ApcShm(k, p, true)
                    } else if p.len() > APC_KEYS * 4 {
                        out.extend_from_slice(&k);
                        out.extend_from_slice(&p);
                        out.push(b);
                        State::Text
                    } else {
                        p.push(b);
                        State::ApcShm(k, p, false)
                    }
                }
                State::Esc => {
                    out.extend_from_slice(&[0x1b, b]);
                    if b == 0x1b {
                        out.pop();
                        State::Esc
                    } else {
                        State::Text
                    }
                }
                State::Params(mut p) => match b {
                    b'0'..=b'9' | b';' if p.len() < 32 => {
                        p.push(b);
                        State::Params(p)
                    }
                    b'q' => State::Data(Vec::new(), false),
                    _ => {
                        // Another DCS: hand it on untouched.
                        out.extend_from_slice(&p);
                        out.push(b);
                        State::Text
                    }
                },
                State::Data(mut d, esc) => {
                    if esc && b == b'\\' {
                        if let Some(img) = decode(&d) {
                            self.next_id = self.next_id.wrapping_add(1);
                            let id = u32::MAX - 0x00ff_ffff + (self.next_id & 0x00ff_ffff);
                            out.extend(replace(&img, id));
                        }
                        State::Text
                    } else if b == 0x1b {
                        if esc {
                            d.push(0x1b);
                        }
                        State::Data(d, true)
                    } else if b == 0x18 || b == 0x1a {
                        // CAN / SUB abort the sequence.
                        State::Text
                    } else {
                        if esc {
                            d.push(0x1b);
                        }
                        d.push(b);
                        if d.len() > MAX_SEQUENCE {
                            State::Text
                        } else {
                            State::Data(d, false)
                        }
                    }
                }
            };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bands_repeats_colors_and_raster_size() {
        // Two colors: register 1 red, register 2 green; 3 columns of red over 6 rows, then a
        // new band with two green pixels in the top row.
        let img = decode(b"\"1;1;3;8#1;2;100;0;0#2;2;0;100;0#1!3~-#2??").unwrap();
        assert_eq!((img.width, img.height), (3, 8));
        let px = |x: usize, y: usize| &img.rgba[(y * 3 + x) * 4..(y * 3 + x) * 4 + 4];
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
        assert_eq!(px(2, 5), [255, 0, 0, 255]);
        // `?` draws nothing: transparent.
        assert_eq!(px(0, 6), [0, 0, 0, 0]);
        // Without raster attributes the size comes from the data.
        let small = decode(b"#3~~").unwrap();
        assert_eq!((small.width, small.height), (2, 6));
        assert_eq!(&small.rgba[..4], [51, 204, 51, 255]);
        assert!(decode(b"").is_none());
        assert_eq!(hls(120, 50, 100), [255, 0, 0]);
    }

    #[test]
    fn the_scanner_replaces_sixels_even_across_reads() {
        let mut s = Scanner::default();
        let mut seen = Vec::new();
        let mut rep = |i: &Sixel, id: u32| {
            seen.push((i.width, i.height, id));
            b"<IMG>".to_vec()
        };
        let mut out = s.feed(b"ab\x1bP0;1q#1~", &mut rep);
        out.extend(s.feed(b"~\x1b", &mut rep));
        out.extend(s.feed(b"\\cd\x1b[1mx\x1bPother\x1b\\", &mut rep));
        assert_eq!(out, b"ab<IMG>cd\x1b[1mx\x1bPother\x1b\\");
        assert_eq!(seen.len(), 1);
        assert_eq!((seen[0].0, seen[0].1), (2, 6));
        assert_eq!(base64(&[255, 0, 0]), "/wAA");
        // Kitty shared memory names get their leading slash; other Kitty commands pass.
        let mut s = Scanner::default();
        let mpv = b"\x1b_Ga=T,t=s,f=24,s=2,v=2,C=1,q=2,m=1;bXB2LWtpdHR5LTE=\x1b\\";
        let out = s.feed(mpv, |_, _| Vec::new());
        assert_eq!(
            out,
            b"\x1b_Ga=T,t=s,f=24,s=2,v=2,C=1,q=2;L21wdi1raXR0eS0x\x1b\\".to_vec(),
            "slash added, m dropped"
        );
        let direct = b"\x1b_Ga=T,f=24,s=1,v=1;/wAA\x1b\\";
        assert_eq!(s.feed(direct, |_, _| Vec::new()), direct.to_vec());
        let slashed = b"\x1b_Ga=T,t=s;L2E=\x1b\\";
        assert_eq!(s.feed(slashed, |_, _| Vec::new()), slashed.to_vec());
    }
}
