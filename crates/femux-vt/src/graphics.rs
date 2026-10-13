//! Kitty graphics (REQ-GFX-001): libghostty-vt parses and stores images per terminal; this
//! reads its placements and pixels for the daemon. PNG payloads are decoded here (the
//! library asks its embedder), and storage is capped per terminal (REQ-GFX-004).

use std::ffi::c_void;
use std::ptr;
use std::sync::Once;

use crate::{Error, Terminal, ffi};

/// REQ-GFX-004: the image memory one pane may hold (the library evicts beyond it).
pub const IMAGE_STORAGE_LIMIT: u64 = 64 * 1024 * 1024;

/// Pixel layout of stored image data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb,
    Rgba,
    Gray,
    GrayAlpha,
}

impl PixelFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Rgb => 3,
            PixelFormat::Rgba => 4,
            PixelFormat::Gray => 1,
            PixelFormat::GrayAlpha => 2,
        }
    }
}

/// A stored image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Changes whenever the pixels do (replacement, animation frame).
    pub generation: u64,
    /// Bytes of pixel data; 0 while the payload is still arriving.
    pub len: usize,
}

/// A placement on the visible screen, in cells relative to the terminal's viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub image_id: u32,
    pub placement_id: u32,
    pub col: i32,
    pub row: i32,
    pub cols: u32,
    pub rows: u32,
    /// The part of the image shown, in pixels.
    pub src: (u32, u32, u32, u32),
    /// Pixel offset inside the first cell.
    pub offset: (u32, u32),
    pub z: i32,
    pub image_generation: u64,
}

/// Decodes PNG payloads for libghostty-vt into RGBA.
unsafe extern "C" fn decode_png(
    _ud: *mut c_void,
    alloc: *const ffi::GhosttyAllocator,
    data: *const u8,
    len: usize,
    out: *mut ffi::GhosttySysImage,
) -> bool {
    // SAFETY: the library passes a valid buffer for the duration of the call.
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    let Some((w, h, rgba)) = png_to_rgba(bytes) else {
        return false;
    };
    // SAFETY: the library owns the buffer allocated with its allocator.
    unsafe {
        let buf = ffi::ghostty_alloc(alloc, rgba.len());
        if buf.is_null() {
            return false;
        }
        ptr::copy_nonoverlapping(rgba.as_ptr(), buf, rgba.len());
        (*out).width = w;
        (*out).height = h;
        (*out).data = buf;
        (*out).data_len = rgba.len();
    }
    true
}

/// A PNG as width, height and RGBA bytes.
pub fn png_to_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|[r, g, b]| [*r, *g, *b, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|[g, a]| [*g, *g, *g, *a])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((info.width, info.height, rgba))
}

/// Installs the PNG decoder once per process.
pub(crate) fn install_png_decoder() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let f: ffi::GhosttySysDecodePngFn = Some(decode_png);
        // SAFETY: the option takes the function pointer as its value.
        unsafe {
            ffi::ghostty_sys_set(
                ffi::GhosttySysOption::GHOSTTY_SYS_OPT_DECODE_PNG,
                f.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    });
}

impl Terminal {
    /// Turns Kitty graphics on with the storage cap, the temp-file and shared-memory media
    /// (the program runs on the daemon's machine) and the file medium off.
    pub(crate) fn enable_graphics(&mut self) -> Result<(), Error> {
        use ffi::GhosttyTerminalOption as O;
        install_png_decoder();
        let limit = IMAGE_STORAGE_LIMIT;
        let off = false;
        let on = true;
        let tmp = std::env::temp_dir();
        let tmp = tmp.to_string_lossy();
        let tmp = ffi::GhosttyString {
            ptr: tmp.as_ptr(),
            len: tmp.len(),
        };
        // SAFETY: each value has the type its option documents.
        unsafe {
            self.set_opt(
                O::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
                &limit as *const u64 as *const c_void,
            )?;
            self.set_opt(
                O::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE,
                &off as *const bool as *const c_void,
            )?;
            self.set_opt(
                O::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM,
                &on as *const bool as *const c_void,
            )?;
            self.set_opt(
                O::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE,
                &tmp as *const ffi::GhosttyString as *const c_void,
            )?;
        }
        Ok(())
    }

    /// Sets the image memory cap in bytes (REQ-GFX-004; 0 turns Kitty graphics off).
    pub fn set_image_limit(&mut self, bytes: u64) -> Result<(), Error> {
        // SAFETY: the option takes a u64 pointer.
        unsafe {
            self.set_opt(
                ffi::GhosttyTerminalOption::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
                &bytes as *const u64 as *const c_void,
            )
        }
    }

    pub fn cell_px(&self) -> (u32, u32) {
        self.effects.cell_px
    }

    fn graphics(&self) -> Option<ffi::GhosttyKittyGraphics> {
        let mut g: ffi::GhosttyKittyGraphics = ptr::null_mut();
        // SAFETY: the handle is borrowed until the next mutating call; callers finish first.
        unsafe {
            self.get(
                ffi::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
                &mut g,
            )
            .ok()?;
        }
        (!g.is_null()).then_some(g)
    }

    /// Changes whenever any image or placement does (0: never used).
    pub fn graphics_generation(&self) -> u64 {
        let Some(g) = self.graphics() else { return 0 };
        let mut generation = 0u64;
        // SAFETY: `g` is live; the output is a u64.
        unsafe {
            ffi::ghostty_kitty_graphics_get(
                g,
                ffi::GhosttyKittyGraphicsData::GHOSTTY_KITTY_GRAPHICS_DATA_GENERATION,
                &mut generation as *mut u64 as *mut c_void,
            );
        }
        generation
    }

    fn image_of(img: ffi::GhosttyKittyGraphicsImage) -> Option<Image> {
        use ffi::GhosttyKittyGraphicsImageData as D;
        let (mut id, mut w, mut h, mut generation, mut len) = (0u32, 0u32, 0u32, 0u64, 0usize);
        let mut format = ffi::GhosttyKittyImageFormat::GHOSTTY_KITTY_IMAGE_FORMAT_RGBA;
        // SAFETY: `img` is live; each output has the documented type.
        unsafe {
            let get = |k: D, out: *mut c_void| ffi::ghostty_kitty_graphics_image_get(img, k, out);
            get(
                D::GHOSTTY_KITTY_IMAGE_DATA_ID,
                &mut id as *mut _ as *mut c_void,
            );
            get(
                D::GHOSTTY_KITTY_IMAGE_DATA_WIDTH,
                &mut w as *mut _ as *mut c_void,
            );
            get(
                D::GHOSTTY_KITTY_IMAGE_DATA_HEIGHT,
                &mut h as *mut _ as *mut c_void,
            );
            get(
                D::GHOSTTY_KITTY_IMAGE_DATA_FORMAT,
                &mut format as *mut _ as *mut c_void,
            );
            get(
                D::GHOSTTY_KITTY_IMAGE_DATA_GENERATION,
                &mut generation as *mut _ as *mut c_void,
            );
            if get(
                D::GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN,
                &mut len as *mut _ as *mut c_void,
            ) != ffi::GhosttyResult::GHOSTTY_SUCCESS
            {
                len = 0;
            }
        }
        use ffi::GhosttyKittyImageFormat as F;
        let format = match format {
            F::GHOSTTY_KITTY_IMAGE_FORMAT_RGB => PixelFormat::Rgb,
            F::GHOSTTY_KITTY_IMAGE_FORMAT_GRAY => PixelFormat::Gray,
            F::GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA => PixelFormat::GrayAlpha,
            _ => PixelFormat::Rgba,
        };
        Some(Image {
            id,
            width: w,
            height: h,
            format,
            generation,
            len,
        })
    }

    /// The image with `id`, if stored.
    pub fn image(&self, id: u32) -> Option<Image> {
        let g = self.graphics()?;
        // SAFETY: `g` is live.
        let img = unsafe { ffi::ghostty_kitty_graphics_image(g, id) };
        if img.is_null() {
            return None;
        }
        Self::image_of(img)
    }

    /// `len` bytes of image `id`'s pixels from `offset` (fewer at the end).
    pub fn image_bytes(&self, id: u32, offset: usize, len: usize) -> Option<(Image, Vec<u8>)> {
        let g = self.graphics()?;
        // SAFETY: `g` is live; the data pointer stays valid until the next mutation, and
        // the bytes are copied before returning.
        unsafe {
            let img = ffi::ghostty_kitty_graphics_image(g, id);
            if img.is_null() {
                return None;
            }
            let info = Self::image_of(img)?;
            let mut data: *const u8 = ptr::null();
            let r = ffi::ghostty_kitty_graphics_image_get(
                img,
                ffi::GhosttyKittyGraphicsImageData::GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR,
                &mut data as *mut *const u8 as *mut c_void,
            );
            if r != ffi::GhosttyResult::GHOSTTY_SUCCESS || data.is_null() {
                return None;
            }
            let all = std::slice::from_raw_parts(data, info.len);
            let start = offset.min(all.len());
            let end = offset.saturating_add(len).min(all.len());
            Some((info, all[start..end].to_vec()))
        }
    }

    /// Every placement of every stored image (virtual ones left out), with its position on
    /// the visible screen; `visible_only` drops those scrolled out of view.
    pub fn placements(&self, visible_only: bool) -> Vec<Placement> {
        let Some(g) = self.graphics() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // Virtual placements (Unicode placeholders): (image, placement) -> (cols, rows, z).
        let mut virtuals: Vec<(u32, u32, u32, u32, i32)> = Vec::new();
        // SAFETY: the iterator is created and freed here; every handle stays valid while no
        // mutating call happens (we hold `&self`).
        unsafe {
            let mut it: ffi::GhosttyKittyGraphicsPlacementIterator = ptr::null_mut();
            if ffi::ghostty_kitty_graphics_placement_iterator_new(ptr::null(), &mut it)
                != ffi::GhosttyResult::GHOSTTY_SUCCESS
            {
                return out;
            }
            if ffi::ghostty_kitty_graphics_get(
                g,
                ffi::GhosttyKittyGraphicsData::GHOSTTY_KITTY_GRAPHICS_DATA_PLACEMENT_ITERATOR,
                &mut it as *mut _ as *mut c_void,
            ) == ffi::GhosttyResult::GHOSTTY_SUCCESS
            {
                use ffi::GhosttyKittyGraphicsPlacementData as P;
                while ffi::ghostty_kitty_graphics_placement_next(it) {
                    let (mut image_id, mut pid, mut virt) = (0u32, 0u32, false);
                    let (mut xo, mut yo, mut z) = (0u32, 0u32, 0i32);
                    let get =
                        |k: P, o: *mut c_void| ffi::ghostty_kitty_graphics_placement_get(it, k, o);
                    get(
                        P::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IMAGE_ID,
                        &mut image_id as *mut _ as *mut c_void,
                    );
                    get(
                        P::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_PLACEMENT_ID,
                        &mut pid as *mut _ as *mut c_void,
                    );
                    get(
                        P::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IS_VIRTUAL,
                        &mut virt as *mut _ as *mut c_void,
                    );
                    get(
                        P::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_X_OFFSET,
                        &mut xo as *mut _ as *mut c_void,
                    );
                    get(
                        P::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Y_OFFSET,
                        &mut yo as *mut _ as *mut c_void,
                    );
                    get(
                        P::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Z,
                        &mut z as *mut _ as *mut c_void,
                    );
                    if virt {
                        let (mut cols, mut rows) = (0u32, 0u32);
                        get(
                            P::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_COLUMNS,
                            &mut cols as *mut _ as *mut c_void,
                        );
                        get(
                            P::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_ROWS,
                            &mut rows as *mut _ as *mut c_void,
                        );
                        virtuals.push((image_id, pid, cols, rows, z));
                        continue;
                    }
                    let img = ffi::ghostty_kitty_graphics_image(g, image_id);
                    if img.is_null() {
                        continue;
                    }
                    let mut info: ffi::GhosttyKittyGraphicsPlacementRenderInfo = std::mem::zeroed();
                    info.size = std::mem::size_of::<ffi::GhosttyKittyGraphicsPlacementRenderInfo>();
                    if ffi::ghostty_kitty_graphics_placement_render_info(
                        it, img, self.raw, &mut info,
                    ) != ffi::GhosttyResult::GHOSTTY_SUCCESS
                    {
                        continue;
                    }
                    if visible_only && !info.viewport_visible {
                        continue;
                    }
                    let generation = Self::image_of(img).map_or(0, |i| i.generation);
                    out.push(Placement {
                        image_id,
                        placement_id: pid,
                        col: info.viewport_col,
                        row: info.viewport_row,
                        cols: info.grid_cols,
                        rows: info.grid_rows,
                        src: (
                            info.source_x,
                            info.source_y,
                            info.source_width,
                            info.source_height,
                        ),
                        offset: (xo, yo),
                        z,
                        image_generation: generation,
                    });
                }
            }
            ffi::ghostty_kitty_graphics_placement_iterator_free(it);
        }
        out.extend(self.placeholder_placements(&virtuals));
        out
    }

    /// Placements drawn with Unicode placeholders on the visible screen, one per run of
    /// cells. Their ids are synthetic (high bit set, the run's screen cell) so each run is
    /// its own placement on the client's host.
    fn placeholder_placements(&self, virtuals: &[(u32, u32, u32, u32, i32)]) -> Vec<Placement> {
        if virtuals.is_empty() {
            return Vec::new();
        }
        let cell = self.effects.cell_px;
        crate::placeholder::runs(&self.grid)
            .into_iter()
            .filter_map(|run| {
                let target = virtuals.iter().find(|v| {
                    v.0 == run.image_id && (run.placement_id == 0 || v.1 == run.placement_id)
                })?;
                let img = self.image(run.image_id)?;
                let cols = if target.2 > 0 {
                    target.2
                } else {
                    img.width.div_ceil(cell.0.max(1))
                };
                let rows = if target.3 > 0 {
                    target.3
                } else {
                    img.height.div_ceil(cell.1.max(1))
                };
                let (src, offset) =
                    crate::placeholder::render(&run, (img.width, img.height), (cols, rows), cell)?;
                Some(Placement {
                    image_id: run.image_id,
                    placement_id: 0x8000_0000 | (run.y as u32) << 16 | run.x as u32,
                    col: run.x as i32,
                    row: run.y as i32,
                    cols: run.width,
                    rows: 1,
                    src,
                    offset,
                    z: target.4,
                    image_generation: img.generation,
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let n = (c[0] as u32) << 16
                | (*c.get(1).unwrap_or(&0) as u32) << 8
                | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn gfx_001_images_and_placements_are_parsed_per_terminal() {
        let mut t = Terminal::new(40, 10, 100).unwrap();
        t.write(b"\r\n\r\n  ");
        // A 2x2 red RGB image, shown at the cursor over 4x2 cells.
        let px = [255u8, 0, 0].repeat(4);
        t.write(format!("\x1b_Ga=T,f=24,s=2,v=2,i=7,c=4,r=2,z=-1;{}\x1b\\", b64(&px)).as_bytes());
        let img = t.image(7).expect("stored");
        assert_eq!(
            (img.width, img.height, img.format, img.len),
            (2, 2, PixelFormat::Rgb, 12)
        );
        let p = t.placements(true);
        assert_eq!(p.len(), 1, "{p:?}");
        assert_eq!(
            (p[0].image_id, p[0].col, p[0].row, p[0].cols, p[0].rows),
            (7, 2, 2, 4, 2)
        );
        assert_eq!((p[0].src, p[0].z), ((0, 0, 2, 2), -1));
        let (_, bytes) = t.image_bytes(7, 3, 6).unwrap();
        assert_eq!(bytes, [255, 0, 0, 255, 0, 0]);
        let g = t.graphics_generation();
        assert!(g > 0);
        // Scrolled out of view: still stored, no longer visible.
        t.write(&b"\r\n".repeat(20));
        assert!(t.placements(true).is_empty());
        assert_eq!(t.placements(false).len(), 1);
        // Deleting it removes it.
        t.write(b"\x1b_Ga=d,d=I,i=7\x1b\\");
        assert!(t.image(7).is_none());
        assert!(t.graphics_generation() > g);
    }

    #[test]
    fn gfx_004_image_memory_is_capped_and_old_images_go_first() {
        let mut t = Terminal::new(40, 10, 100).unwrap();
        // Room for two 10x10 RGBA images (400 bytes each), not three.
        t.set_image_limit(1000).unwrap();
        let px = b64(&[9u8; 400]);
        for id in 1..=3 {
            t.write(format!("\x1b_Ga=t,f=32,s=10,v=10,i={id};{px}\x1b\\").as_bytes());
        }
        let kept: Vec<u32> = (1..=3).filter(|id| t.image(*id).is_some()).collect();
        assert_eq!(kept, [2, 3]);
        assert_eq!(IMAGE_STORAGE_LIMIT, 64 * 1024 * 1024);
    }

    #[test]
    fn gfx_002_sixels_become_images_at_the_cursor() {
        let mut t = Terminal::new(40, 10, 100).unwrap();
        // 10x20 px cells (the default): a 20x40 sixel covers 2x2 cells.
        t.write(b"\r\n   ");
        t.write(b"\x1bPq\"1;1;20;40#1;2;100;0;0#1!20~-!20~-!20~-!20~-!20~-!20~-!20~\x1b\\after");
        let p = t.placements(true);
        assert_eq!(p.len(), 1, "{p:?}");
        assert_eq!((p[0].col, p[0].row, p[0].cols, p[0].rows), (3, 1, 2, 2));
        let img = t.image(p[0].image_id).unwrap();
        assert_eq!(
            (img.width, img.height, img.format),
            (20, 40, PixelFormat::Rgba)
        );
        // Text continues below the image.
        t.sync().unwrap();
        let row = crate::line_text(&t.grid().rows[3].cells);
        assert!(row.starts_with("after"), "{row:?}");
        // DA1 offers sixel only to a client that shows images.
        t.write(b"\x1b[c");
        assert_eq!(t.take_pty_output(), b"\x1b[?62;22c");
        t.set_host(&crate::HostCaps {
            images: true,
            ..Default::default()
        })
        .unwrap();
        t.write(b"\x1b[c");
        assert_eq!(t.take_pty_output(), b"\x1b[?62;22;4c");
    }

    #[test]
    fn icat_style_query_and_temp_file_transfer_work() {
        let mut t = Terminal::new(40, 10, 100).unwrap();
        // kitten icat first asks whether graphics work.
        t.write(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
        let answer = String::from_utf8(t.take_pty_output()).unwrap();
        assert!(answer.contains("i=31;OK"), "{answer:?}");
        // Then it sends the pixels through a temp file, which the terminal reads and deletes.
        let path = std::env::temp_dir().join(format!(
            "tty-graphics-protocol-femux-{}.rgb",
            std::process::id()
        ));
        std::fs::write(&path, [0u8, 0, 255].repeat(4)).unwrap();
        let b64path = b64(path.to_string_lossy().as_bytes());
        t.write(format!("\x1b_Ga=T,t=t,f=24,s=2,v=2,i=5;{b64path}\x1b\\").as_bytes());
        let img = t.image(5).expect("read from the temp file");
        assert_eq!((img.width, img.height, img.len), (2, 2, 12));
        assert!(!path.exists(), "the temp file is removed");
        assert_eq!(t.placements(true).len(), 1);
    }

    #[test]
    fn unicode_placeholders_become_placements() {
        let mut t = Terminal::new(40, 10, 100).unwrap();
        // yazi's way: a virtual placement over 2x2 cells, then placeholder cells whose
        // foreground is the image id and whose diacritics give image row and column.
        let px = [0u8, 255, 0].repeat(4 * 4);
        t.write(
            format!(
                "\x1b_Ga=T,U=1,f=24,s=4,v=4,i=5,c=2,r=2,q=2;{}\x1b\\",
                b64(&px)
            )
            .as_bytes(),
        );
        assert!(t.placements(true).is_empty(), "nothing on screen yet");
        let (r0, r1, c0, ph) = ('\u{0305}', '\u{030D}', '\u{0305}', '\u{10EEEE}');
        t.write(format!("\x1b[38;5;5m  {ph}{r0}{c0}{ph}\r\n  {ph}{r1}{c0}{ph}\x1b[0m").as_bytes());
        t.sync().unwrap();
        let mut p = t.placements(true);
        p.sort_by_key(|p| p.row);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(
            (p[0].image_id, p[0].col, p[0].row, p[0].cols, p[0].rows),
            (5, 2, 0, 2, 1)
        );
        assert_eq!(p[1].row, 1);
        // Each row shows its half of the image (4x4 px fitted into 2x2 cells of 10x20).
        assert_eq!(p[0].src.1, 0);
        assert!(
            p[1].src.1 > 0 && p[1].src.1 + p[1].src.3 <= 4,
            "{:?}",
            p[1].src
        );
        assert_ne!(p[0].placement_id, p[1].placement_id);
    }

    #[test]
    fn mpv_style_shared_memory_frames_load_and_free_the_name() {
        // mpv: no leading slash, `m=1`, one name reused for every frame.
        let name = format!("femux-mpv-test-{}", std::process::id());
        let path = std::ffi::CString::new(format!("/{name}")).unwrap();
        let mut t = Terminal::new(40, 10, 100).unwrap();
        for frame in 0..3u8 {
            let px = [frame, 0, 0].repeat(4);
            unsafe {
                let fd = libc::shm_open(path.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600);
                assert!(fd >= 0);
                assert_eq!(libc::ftruncate(fd, 12), 0);
                let m = libc::mmap(
                    std::ptr::null_mut(),
                    12,
                    libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                );
                std::ptr::copy_nonoverlapping(px.as_ptr(), m as *mut u8, 12);
                libc::munmap(m, 12);
                libc::close(fd);
            }
            t.write(
                format!(
                    "\x1b[1;1H\x1b_Ga=T,t=s,f=24,s=2,v=2,C=1,q=2,m=1;{}\x1b\\",
                    b64(name.as_bytes())
                )
                .as_bytes(),
            );
            // The program's name is free again at once, ready for its next frame.
            let gone = unsafe { libc::shm_open(path.as_ptr(), libc::O_RDONLY, 0) } < 0;
            assert!(gone, "frame {frame}: the name is released");
            let p = t.placements(true);
            let newest = p.iter().max_by_key(|p| p.image_id).expect("placed");
            let (_, bytes) = t.image_bytes(newest.image_id, 0, 3).unwrap();
            assert_eq!(bytes, [frame, 0, 0], "frame {frame} loaded");
        }
    }

    #[test]
    fn png_payloads_are_decoded_to_rgba() {
        let mut png_bytes = Vec::new();
        {
            let mut e = png::Encoder::new(&mut png_bytes, 1, 2);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().unwrap();
            w.write_image_data(&[0, 255, 0, 0, 0, 255]).unwrap();
        }
        let mut t = Terminal::new(20, 5, 10).unwrap();
        t.write(format!("\x1b_Ga=t,f=100,i=3;{}\x1b\\", b64(&png_bytes)).as_bytes());
        let img = t.image(3).expect("png stored");
        assert_eq!(
            (img.width, img.height, img.format),
            (1, 2, PixelFormat::Rgba)
        );
        let (_, px) = t.image_bytes(3, 0, 8).unwrap();
        assert_eq!(px, [0, 255, 0, 255, 0, 0, 255, 255]);
    }
}
