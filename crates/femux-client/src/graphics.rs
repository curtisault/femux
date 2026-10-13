//! Images on the host terminal (REQ-GFX-001, REQ-UI-052). The daemon keeps every pane's
//! images and sends their placements in pane cells with each frame. The client reads each
//! image once per generation (`image.read`, in base64 chunks), transmits it to the host under
//! an id of its own, and places it offset to the pane's screen position and clipped to the
//! pane with Kitty `a=p` commands. Placements that disappear are deleted; while a full-screen
//! overlay is open none are shown.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use femux_proto::msg as m;

/// A clipped placement in screen cells plus the part of the image it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub id: u32,
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    /// Cells of the image cut off at the left / top.
    pub skip_cols: u16,
    pub skip_rows: u16,
    /// Cells of the full placement.
    pub total_cols: u16,
    pub total_rows: u16,
}

/// Offsets `p` by the pane's screen rectangle and clips it to that rectangle.
pub fn place(p: &m::ImagePlacement, pane: &m::Rect) -> Option<Placed> {
    let x0 = pane.x as i32 + p.x;
    let y0 = pane.y as i32 + p.y;
    let x1 = x0 + p.cols as i32;
    let y1 = y0 + p.rows as i32;
    let cx0 = x0.max(pane.x as i32);
    let cy0 = y0.max(pane.y as i32);
    let cx1 = x1.min(pane.x as i32 + pane.w as i32);
    let cy1 = y1.min(pane.y as i32 + pane.h as i32);
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    Some(Placed {
        id: p.id,
        x: cx0 as u16,
        y: cy0 as u16,
        cols: (cx1 - cx0) as u16,
        rows: (cy1 - cy0) as u16,
        skip_cols: (cx0 - x0) as u16,
        skip_rows: (cy0 - y0) as u16,
        total_cols: p.cols,
        total_rows: p.rows,
    })
}

/// The Kitty command that shows `p` of host image `host_id`: the cursor is saved, moved and
/// restored around it, the source rectangle is cropped to the visible cells, and `C=1` keeps
/// the cursor still. `q=2` silences the host's answers.
pub fn kitty_put(host_id: u32, p: &Placed, src: &m::ImagePlacement) -> String {
    let cw = src.src_w as f64 / p.total_cols.max(1) as f64;
    let ch = src.src_h as f64 / p.total_rows.max(1) as f64;
    let sx = src.src_x + (p.skip_cols as f64 * cw).round() as u32;
    let sy = src.src_y + (p.skip_rows as f64 * ch).round() as u32;
    let sw = ((p.cols as f64 * cw).round() as u32).min(src.src_w);
    let sh = ((p.rows as f64 * ch).round() as u32).min(src.src_h);
    let xo = if p.skip_cols == 0 {
        src.x_off.unwrap_or(0)
    } else {
        0
    };
    let yo = if p.skip_rows == 0 {
        src.y_off.unwrap_or(0)
    } else {
        0
    };
    format!(
        "\x1b7\x1b[{};{}H\x1b_Ga=p,i={host_id},p={},c={},r={},x={sx},y={sy},w={sw},h={sh},X={xo},Y={yo},z={},C=1,q=2\x1b\\\x1b8",
        p.y + 1,
        p.x + 1,
        host_pid(src.id),
        p.cols,
        p.rows,
        src.z
    )
}

/// Host placement ids must be nonzero to address one placement.
fn host_pid(id: u32) -> u32 {
    id.wrapping_add(1).max(1)
}

/// Deletes one placement, keeping the image on the host.
pub fn kitty_delete(host_id: u32, placement: u32) -> String {
    format!(
        "\x1b_Ga=d,d=i,i={host_id},p={},q=2\x1b\\",
        host_pid(placement)
    )
}

/// Transmits base64 image data as host image `host_id`, in 4096-byte chunks.
pub fn kitty_transmit(
    host_id: u32,
    format: m::ImageFormat,
    w: u32,
    h: u32,
    zlib: bool,
    b64: &str,
) -> String {
    let f = match format {
        m::ImageFormat::Rgb => 24,
        _ => 32,
    };
    let chunks: Vec<&str> = b64
        .as_bytes()
        .chunks(4096)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();
    let mut out = String::new();
    for (i, c) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        if i == 0 {
            out.push_str(&format!(
                "\x1b_Ga=t,i={host_id},f={f},s={w},v={h}{},q=2,m={more};{c}\x1b\\",
                if zlib { ",o=z" } else { "" }
            ));
        } else {
            out.push_str(&format!("\x1b_Gm={more},q=2;{c}\x1b\\"));
        }
    }
    out
}

/// Transmits host image `host_id` from the shared memory object `name` (Kitty `t=s`); the
/// host reads and unlinks it.
pub fn kitty_transmit_shm(
    host_id: u32,
    format: m::ImageFormat,
    w: u32,
    h: u32,
    size: u64,
    name: &str,
) -> String {
    use base64::Engine;
    let f = match format {
        m::ImageFormat::Rgb => 24,
        _ => 32,
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(name);
    format!("\x1b_Ga=t,t=s,i={host_id},f={f},s={w},v={h},S={size},q=2;{b64}\x1b\\")
}

/// How image bytes travel from the daemon to the host (ADR-021 #4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Transfer {
    /// Base64 chunks through the protocol, sent on as they are.
    #[default]
    Plain,
    /// The client shares the daemon's machine and its host reads shared memory: the daemon
    /// writes the pixels once and the host reads them directly.
    Shm,
    /// A remote client: zlib-compressed chunks the host inflates (`o=z`).
    Zlib,
}

/// Removes every image femux put on the host (when the client exits).
pub const KITTY_CLEAR: &str = "\x1b_Ga=d,d=A,q=2\x1b\\";

/// (pane, image) as the daemon names it.
type ImageKey = (String, u32);
/// (pane, image, placement).
type PlacementKey = (String, u32, u32);

#[derive(Debug, Clone)]
struct HostImage {
    /// The host id currently shown.
    id: u32,
    generation: u64,
    /// The frame it replaced, left placed underneath until the host has drawn the new one
    /// (a host uploads a new image on its next render and shows nothing for it until then).
    under: Option<(u32, Instant)>,
    /// Ids to delete (with their data) once the current puts are out.
    retired: Vec<u32>,
}

/// How long a replaced frame stays under its successor: a few host render frames.
const UNDERLAY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
struct Fetch {
    generation: u64,
    data: String,
}

/// What the host terminal shows of femux's images.
#[derive(Debug, Default)]
pub struct Graphics {
    /// The host speaks the Kitty graphics protocol.
    pub enabled: bool,
    pub transfer: Transfer,
    /// The host has synchronized output (mode 2026).
    pub sync: bool,
    hosts: HashMap<ImageKey, HostImage>,
    next_id: u32,
    fetching: HashMap<ImageKey, Fetch>,
    shown: BTreeMap<PlacementKey, String>,
    out: Vec<u8>,
}

impl Graphics {
    pub fn new(enabled: bool) -> Graphics {
        Graphics {
            enabled,
            next_id: 1,
            ..Default::default()
        }
    }

    /// Brings the host up to date with `wanted` (placements with their pane's screen
    /// rectangle); returns the image reads to start. Images still loading keep showing their
    /// previous generation, if any.
    pub fn update(&mut self, wanted: &[(m::ImagePlacement, m::Rect)]) -> Vec<m::ImageRead> {
        if !self.enabled {
            return Vec::new();
        }
        let mut reads = Vec::new();
        let mut desired: BTreeMap<PlacementKey, String> = BTreeMap::new();
        for (p, rect) in wanted {
            let key = (p.pane_id.clone(), p.image_id);
            let current = self.hosts.get(&key).map(|h| h.generation);
            if current != Some(p.generation)
                && self
                    .fetching
                    .get(&key)
                    .is_none_or(|f| f.generation != p.generation)
            {
                self.fetching.insert(
                    key.clone(),
                    Fetch {
                        generation: p.generation,
                        data: String::new(),
                    },
                );
                reads.push(self.request(&p.pane_id, p.image_id, 0));
            }
            let (Some(host), Some(placed)) = (self.hosts.get(&key), place(p, rect)) else {
                continue;
            };
            desired.insert(
                (p.pane_id.clone(), p.image_id, p.id),
                kitty_put(host.id, &placed, p),
            );
        }
        let gone: Vec<PlacementKey> = self
            .shown
            .keys()
            .filter(|k| !desired.contains_key(*k))
            .cloned()
            .collect();
        for k in gone {
            self.shown.remove(&k);
            if let Some(h) = self.hosts.get(&(k.0.clone(), k.1)) {
                self.out
                    .extend_from_slice(kitty_delete(h.id, k.2).as_bytes());
            }
        }
        for (k, cmd) in desired {
            if self.shown.get(&k) != Some(&cmd) {
                self.out.extend_from_slice(cmd.as_bytes());
                self.shown.insert(k, cmd);
            }
        }
        // Replaced frames go after the new puts: those two frames old at once, the one just
        // replaced once the host has had time to draw its successor, or when the image is no
        // longer shown at all. Their data goes with them (`d=I`).
        let now = Instant::now();
        let placed: HashSet<ImageKey> = self.shown.keys().map(|k| (k.0.clone(), k.1)).collect();
        for (key, h) in self.hosts.iter_mut() {
            if let Some((old, at)) = h.under
                && (now.duration_since(at) >= UNDERLAY || !placed.contains(key))
            {
                h.retired.push(old);
                h.under = None;
            }
            for old in h.retired.drain(..) {
                self.out
                    .extend_from_slice(format!("\x1b_Ga=d,d=I,i={old},q=2\x1b\\").as_bytes());
            }
        }
        reads
    }

    fn request(&self, pane: &str, image: u32, offset: u64) -> m::ImageRead {
        m::ImageRead {
            pane_id: pane.to_owned(),
            image_id: image,
            offset,
            shm: (self.transfer == Transfer::Shm).then_some(true),
            zlib: (self.transfer == Transfer::Zlib).then_some(true),
        }
    }

    /// A chunk of an image arrived; returns the read for the next chunk, if any. A complete
    /// image is transmitted to the host and its placements are put again on the next update.
    pub fn on_read(&mut self, pane: &str, r: &m::ImageReadOk) -> Option<m::ImageRead> {
        let key = (pane.to_owned(), r.image_id);
        let request = self.request(pane, r.image_id, 0);
        if r.shm.is_some() {
            // A whole image per answer. Reads overlap when frames come fast: show any answer
            // newer than what is shown, and stop waiting for the ones it supersedes.
            if self
                .fetching
                .get(&key)
                .is_some_and(|f| f.generation <= r.generation)
            {
                self.fetching.remove(&key);
            }
            if self
                .hosts
                .get(&key)
                .is_some_and(|h| h.generation >= r.generation)
            {
                return None;
            }
            self.fetching.insert(
                key.clone(),
                Fetch {
                    generation: r.generation,
                    data: String::new(),
                },
            );
        }
        let f = self.fetching.get_mut(&key)?;
        if r.shm.is_none() && f.generation != r.generation {
            // The image changed while it was read: start over.
            f.generation = r.generation;
            f.data.clear();
            if r.offset != 0 {
                return Some(request);
            }
        }
        f.data.push_str(&r.data);
        let have = r.offset + base64_len(&r.data) as u64;
        if r.shm.is_none() && have < r.size {
            return Some(self.request(pane, r.image_id, have));
        }
        let f = self.fetching.remove(&key)?;
        // A new generation gets a new, higher id and is placed over the shown one, which
        // stays underneath until the host has drawn the new one; the frame before that goes
        // now. Ids only grow: hosts draw images of equal z in id order (Ghostty), so the
        // newest frame must have the highest id.
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let (under, retired) = match self.hosts.remove(&key) {
            Some(mut h) => {
                let mut retired = h.retired;
                if let Some((old, _)) = h.under.take() {
                    retired.push(old);
                }
                (Some((h.id, Instant::now())), retired)
            }
            None => (None, Vec::new()),
        };
        let cmd = match &r.shm {
            Some(name) => kitty_transmit_shm(id, r.format, r.width, r.height, r.size, name),
            None => kitty_transmit(
                id,
                r.format,
                r.width,
                r.height,
                r.zlib == Some(true),
                &f.data,
            ),
        };
        self.out.extend_from_slice(cmd.as_bytes());
        self.hosts.insert(
            key.clone(),
            HostImage {
                id,
                generation: r.generation,
                under,
                retired,
            },
        );
        // The placements move to the new id on the next update.
        self.shown
            .retain(|k, _| (k.0.as_str(), k.1) != (pane, r.image_id));
        None
    }

    /// A read failed (the image is gone): stop waiting for it.
    pub fn read_failed(&mut self, pane: &str, image: u32) {
        self.fetching.remove(&(pane.to_owned(), image));
    }

    /// The host screen was cleared, which removes its placements: put them all again.
    pub fn invalidate(&mut self) {
        self.shown.clear();
    }

    /// Bytes for the host terminal.
    /// Wrapped in synchronized output when the host has it, so a frame's transmit, puts and
    /// deletes appear at once.
    pub fn take_output(&mut self) -> Vec<u8> {
        let out = std::mem::take(&mut self.out);
        if out.is_empty() || !self.sync {
            return out;
        }
        let mut wrapped = b"\x1b[?2026h".to_vec();
        wrapped.extend(out);
        wrapped.extend_from_slice(b"\x1b[?2026l");
        wrapped
    }
}

/// Decoded length of base64 text.
fn base64_len(s: &str) -> usize {
    let pad = s.bytes().rev().take_while(|b| *b == b'=').count();
    (s.len() / 4) * 3 - pad.min(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pl(x: i32, y: i32, cols: u16, rows: u16) -> m::ImagePlacement {
        m::ImagePlacement {
            id: 0,
            pane_id: "P".into(),
            x,
            y,
            cols,
            rows,
            image_id: 7,
            generation: 1,
            src_x: 0,
            src_y: 0,
            src_w: cols as u32 * 10,
            src_h: rows as u32 * 20,
            x_off: None,
            y_off: None,
            z: -1,
        }
    }

    const PANE: m::Rect = m::Rect {
        x: 10,
        y: 2,
        w: 20,
        h: 5,
    };

    #[test]
    fn ui_052_placements_are_offset_and_clipped() {
        let inside = place(&pl(1, 1, 4, 2), &PANE).unwrap();
        assert_eq!(
            (inside.x, inside.y, inside.cols, inside.rows),
            (11, 3, 4, 2)
        );
        assert_eq!((inside.skip_cols, inside.skip_rows), (0, 0));
        // Overhanging right and bottom edges are cut.
        let edge = place(&pl(15, 3, 10, 10), &PANE).unwrap();
        assert_eq!((edge.x, edge.y, edge.cols, edge.rows), (25, 5, 5, 2));
        // Scrolled partly above the pane: the top rows are skipped.
        let above = place(&pl(0, -2, 6, 4), &PANE).unwrap();
        assert_eq!((above.y, above.rows, above.skip_rows), (2, 2, 2));
        assert_eq!(place(&pl(0, -9, 6, 4), &PANE), None);
        assert_eq!(place(&pl(20, 0, 6, 4), &PANE), None);
        assert_eq!(place(&pl(18, 0, 6, 4), &PANE).map(|p| p.cols), Some(2));
    }

    #[test]
    fn put_commands_crop_the_source_to_the_visible_cells() {
        let p = pl(0, -2, 6, 4);
        let placed = place(&p, &PANE).unwrap();
        assert_eq!(
            kitty_put(9, &placed, &p),
            "\x1b7\x1b[3;11H\x1b_Ga=p,i=9,p=1,c=6,r=2,x=0,y=40,w=60,h=40,X=0,Y=0,z=-1,C=1,q=2\x1b\\\x1b8"
        );
        assert_eq!(kitty_delete(9, 0), "\x1b_Ga=d,d=i,i=9,p=1,q=2\x1b\\");
        let big = "A".repeat(5000);
        let t = kitty_transmit(3, m::ImageFormat::Rgba, 50, 25, false, &big);
        assert!(t.starts_with("\x1b_Ga=t,i=3,f=32,s=50,v=25,q=2,m=1;"));
        assert!(t.contains("\x1b\\\x1b_Gm=0,q=2;"));
        assert_eq!(base64_len("/wAA/wAA"), 6);
        assert_eq!(base64_len("/wA="), 2);
    }

    fn read(offset: u64, size: u64, data: &str, generation: u64) -> m::ImageReadOk {
        m::ImageReadOk {
            image_id: 7,
            format: m::ImageFormat::Rgb,
            width: 2,
            height: 2,
            generation,
            size,
            offset,
            data: data.into(),
            shm: None,
            zlib: None,
        }
    }

    #[test]
    fn a_new_frame_goes_over_the_old_one_which_leaves_later() {
        let mut g = Graphics::new(true);
        let p = pl(1, 1, 4, 2);
        let frame = |g: &mut Graphics, generation: u64| {
            let mut q = p.clone();
            q.generation = generation;
            g.update(&[(q.clone(), PANE)]);
            g.on_read("P", &read(0, 12, "/wAA/wAA/wAA/wAA", generation));
            g.update(&[(q, PANE)]);
            String::from_utf8(g.take_output()).unwrap()
        };
        frame(&mut g, 1);
        // Frame 2 goes to id 2 and is placed; frame 1 stays underneath for now.
        let out = frame(&mut g, 2);
        assert!(
            out.contains("a=t,i=2,") && out.contains("a=p,i=2,"),
            "{out:?}"
        );
        assert!(!out.contains("a=d"), "nothing taken down yet: {out:?}");
        // Frame 3: id 3, and frame 1 (two frames old) goes, after the put.
        let out = frame(&mut g, 3);
        let put = out.find("a=p,i=3,").expect("put");
        let del = out
            .find("a=d,d=I,i=1,")
            .expect("frame 1 removed with its data");
        assert!(put < del, "{out:?}");
        assert!(
            !out.contains("d=I,i=2"),
            "frame 2 stays under frame 3: {out:?}"
        );
        // Frame 4 gets a higher id still, so hosts that order equal z by id draw it on top.
        let out = frame(&mut g, 4);
        assert!(
            out.contains("a=t,i=4,") && out.contains("a=d,d=I,i=2,"),
            "{out:?}"
        );
        // With no new frame, the one underneath goes once the host has drawn its successor.
        std::thread::sleep(UNDERLAY + Duration::from_millis(10));
        let mut q = p.clone();
        q.generation = 4;
        g.update(&[(q, PANE)]);
        assert_eq!(
            String::from_utf8(g.take_output()).unwrap(),
            "\x1b_Ga=d,d=I,i=3,q=2\x1b\\"
        );
        // Synchronized output wraps a batch.
        g.sync = true;
        g.invalidate();
        g.update(&[(pl(1, 1, 4, 2), PANE)]);
        let out = String::from_utf8(g.take_output()).unwrap();
        assert!(
            out.starts_with("\x1b[?2026h") && out.ends_with("\x1b[?2026l"),
            "{out:?}"
        );
    }

    #[test]
    fn overlapping_shared_memory_reads_drop_no_frame() {
        let mut g = Graphics::new(true);
        g.transfer = Transfer::Shm;
        let p = pl(1, 1, 4, 2);
        let shm = |generation: u64| {
            let mut r = read(0, 12, "", generation);
            r.shm = Some(format!("/f-{generation}"));
            r
        };
        g.update(&[(p.clone(), PANE)]);
        // Frame 2 arrives while frame 1 is being read: both reads are out.
        let mut p2 = p.clone();
        p2.generation = 2;
        assert_eq!(g.update(&[(p2.clone(), PANE)]).len(), 1);
        g.on_read("P", &shm(1));
        g.on_read("P", &shm(2));
        let out = String::from_utf8(g.take_output()).unwrap();
        assert!(
            out.contains("L2YtMQ==") && out.contains("L2YtMg=="),
            "both shown: {out:?}"
        );
        // An answer older than what is shown is ignored.
        g.on_read("P", &shm(1));
        assert!(g.take_output().is_empty());
    }

    #[test]
    fn shared_memory_and_zlib_transfers() {
        // Local with a shm-capable host: one read, the host is told the object's name.
        let mut g = Graphics::new(true);
        g.transfer = Transfer::Shm;
        let p = pl(1, 1, 4, 2);
        let reads = g.update(&[(p.clone(), PANE)]);
        assert_eq!((reads[0].shm, reads[0].zlib), (Some(true), None));
        let mut r = read(0, 12, "", 1);
        r.shm = Some("/femux-1-0".into());
        assert!(g.on_read("P", &r).is_none());
        assert_eq!(
            String::from_utf8(g.take_output()).unwrap(),
            "\x1b_Ga=t,t=s,i=1,f=24,s=2,v=2,S=12,q=2;L2ZlbXV4LTEtMA==\x1b\\"
        );
        // Remote: compressed chunks, flagged `o=z` for the host.
        let mut g = Graphics::new(true);
        g.transfer = Transfer::Zlib;
        let reads = g.update(&[(p, PANE)]);
        assert_eq!((reads[0].shm, reads[0].zlib), (None, Some(true)));
        let mut r = read(0, 13, "eJz7z8DwH4YAHe4D/Q==", 1);
        r.zlib = Some(true);
        assert!(g.on_read("P", &r).is_none());
        let out = String::from_utf8(g.take_output()).unwrap();
        assert!(
            out.starts_with("\x1b_Ga=t,i=1,f=24,s=2,v=2,o=z,q=2,m=0;eJz7"),
            "{out:?}"
        );
    }

    #[test]
    fn gfx_001_images_are_read_once_transmitted_placed_and_deleted() {
        let mut g = Graphics::new(true);
        let p = pl(1, 1, 4, 2);
        // First sight: read it; nothing to place yet.
        let reads = g.update(&[(p.clone(), PANE)]);
        assert_eq!(reads.len(), 1);
        assert_eq!((reads[0].image_id, reads[0].offset), (7, 0));
        assert!(g.take_output().is_empty());
        // Asking again while it loads does not read twice.
        assert!(g.update(&[(p.clone(), PANE)]).is_empty());
        // Two chunks of 6 then 6 bytes.
        let next = g.on_read("P", &read(0, 12, "/wAA/wAA", 1)).unwrap();
        assert_eq!(next.offset, 6);
        assert!(g.on_read("P", &read(6, 12, "/wAA/wAA", 1)).is_none());
        let out = String::from_utf8(g.take_output()).unwrap();
        assert!(
            out.starts_with("\x1b_Ga=t,i=1,f=24,s=2,v=2,q=2,m=0;/wAA/wAA/wAA/wAA"),
            "{out}"
        );
        // Now it is placed, once.
        assert!(g.update(&[(p.clone(), PANE)]).is_empty());
        let out = String::from_utf8(g.take_output()).unwrap();
        assert!(out.contains("a=p,i=1,p=1,c=4,r=2"), "{out}");
        g.update(&[(p.clone(), PANE)]);
        assert!(g.take_output().is_empty());
        // A new generation is read while the old one stays up.
        let mut p2 = p.clone();
        p2.generation = 2;
        assert_eq!(g.update(&[(p2.clone(), PANE)]).len(), 1);
        assert!(g.take_output().is_empty());
        // Hidden (an overlay) or gone: deleted from the host.
        g.update(&[]);
        assert_eq!(
            String::from_utf8(g.take_output()).unwrap(),
            "\x1b_Ga=d,d=i,i=1,p=1,q=2\x1b\\"
        );
        // A cleared host screen puts it again.
        g.update(&[(p.clone(), PANE)]);
        g.take_output();
        g.invalidate();
        g.update(&[(p, PANE)]);
        assert!(!g.take_output().is_empty());
        // A host without Kitty graphics gets nothing.
        let mut off = Graphics::new(false);
        assert!(off.update(&[(pl(0, 0, 2, 2), PANE)]).is_empty());
        assert!(off.take_output().is_empty());
    }
}
