//! `image.read` and `image.list` (REQ-GFX-001, ADR-021): image data stays in each pane's
//! terminal; clients read it in base64 chunks, once per generation.

use std::collections::VecDeque;
use std::ffi::CString;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use femux_proto::Response;
use femux_proto::msg as m;
use femux_vt::Terminal;
use femux_vt::graphics::{Image, PixelFormat};
use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::state::{Fail, not_found};

/// Raw bytes per `image.read` answer: 6 MiB, a multiple of 3 so base64 chunks concatenate,
/// and 8 MiB once encoded, well under the 16 MiB frame limit (REQ-PROTO-004).
pub const CHUNK: usize = 6 * 1024 * 1024;

/// What a client receives for a stored format: Kitty transmits RGB, RGBA or PNG only, so
/// gray images become RGBA.
fn wire_format(f: PixelFormat) -> m::ImageFormat {
    match f {
        PixelFormat::Rgb => m::ImageFormat::Rgb,
        _ => m::ImageFormat::Rgba,
    }
}

fn to_rgba(f: PixelFormat, px: &[u8]) -> Vec<u8> {
    match f {
        PixelFormat::Gray => px.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        PixelFormat::GrayAlpha => px
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|[g, a]| [*g, *g, *g, *a])
            .collect(),
        _ => px.to_vec(),
    }
}

/// The whole image as sent: RGB or RGBA bytes.
fn pixels(vt: &Terminal, id: u32) -> Option<(Image, Vec<u8>)> {
    let img = vt.image(id)?;
    let (_, bytes) = vt.image_bytes(id, 0, img.len)?;
    let bytes = match img.format {
        PixelFormat::Rgb | PixelFormat::Rgba => bytes,
        f => to_rgba(f, &bytes),
    };
    Some((img, bytes))
}

/// What `image.read` keeps between calls.
#[derive(Default)]
pub struct Shared {
    /// The last few compressed images, by (pane, image, generation): chunked reads of one
    /// image compress it once.
    zlib: Mutex<VecDeque<(ZKey, Arc<Vec<u8>>)>>,
    /// Shared memory objects given out, unlinked after a while if the host never did.
    shm: Mutex<VecDeque<(String, Instant)>>,
}

type ZKey = (String, u32, u64);

/// How long a shared memory object may wait for the host to read (and unlink) it.
const SHM_GRACE: Duration = Duration::from_secs(30);

impl Shared {
    fn compressed(&self, key: ZKey, raw: impl FnOnce() -> Option<Vec<u8>>) -> Option<Arc<Vec<u8>>> {
        let mut cache = self.zlib.lock().unwrap();
        if let Some((_, z)) = cache.iter().find(|(k, _)| *k == key) {
            return Some(z.clone());
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&raw()?).ok()?;
        let z = Arc::new(enc.finish().ok()?);
        cache.push_back((key, z.clone()));
        while cache.len() > 4 {
            cache.pop_front();
        }
        Some(z)
    }

    /// Writes `bytes` to a new shared memory object; returns its name.
    fn put_shm(&self, bytes: &[u8]) -> std::io::Result<String> {
        static N: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "/femux-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        write_shm(&name, bytes)?;
        let mut given = self.shm.lock().unwrap();
        let now = Instant::now();
        while given
            .front()
            .is_some_and(|(_, t)| now.duration_since(*t) > SHM_GRACE)
        {
            let (old, _) = given.pop_front().unwrap();
            unlink_shm(&old);
        }
        given.push_back((name.clone(), now));
        Ok(name)
    }
}

impl Drop for Shared {
    /// Objects no host read before the daemon stopped.
    fn drop(&mut self) {
        for (name, _) in self.shm.get_mut().unwrap().drain(..) {
            unlink_shm(&name);
        }
    }
}

fn write_shm(name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let c = CString::new(name).map_err(std::io::Error::other)?;
    // SAFETY: plain libc calls on a fresh object; the mapping is `bytes.len()` long and
    // unmapped before returning. Writing through mmap works on Linux and macOS alike.
    unsafe {
        let fd = libc::shm_open(
            c.as_ptr(),
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
            0o600 as libc::c_uint,
        );
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fail = |fd| {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            libc::shm_unlink(c.as_ptr());
            Err(e)
        };
        if libc::ftruncate(fd, bytes.len() as libc::off_t) < 0 {
            return fail(fd);
        }
        let map = libc::mmap(
            std::ptr::null_mut(),
            bytes.len(),
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        );
        if map == libc::MAP_FAILED {
            return fail(fd);
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), map as *mut u8, bytes.len());
        libc::munmap(map, bytes.len());
        libc::close(fd);
    }
    Ok(())
}

fn unlink_shm(name: &str) {
    if let Ok(c) = CString::new(name) {
        // SAFETY: a plain libc call; a missing object (the host unlinked it) is fine.
        unsafe { libc::shm_unlink(c.as_ptr()) };
    }
}

pub fn read(vt: &Terminal, r: &m::ImageRead, shared: &Shared) -> Result<Response, Fail> {
    let img = vt.image(r.image_id).ok_or_else(|| not_found("image"))?;
    let ok = |size: usize, data: &[u8], shm: Option<String>, zlib: Option<bool>| {
        Response::ImageRead(m::ImageReadOk {
            image_id: img.id,
            format: wire_format(img.format),
            width: img.width,
            height: img.height,
            generation: img.generation,
            size: size as u64,
            offset: r.offset,
            data: base64::engine::general_purpose::STANDARD.encode(data),
            shm,
            zlib,
        })
    };
    if r.shm == Some(true) && img.len > 0 {
        let (_, px) = pixels(vt, r.image_id).ok_or_else(|| not_found("image data"))?;
        let name = shared
            .put_shm(&px)
            .map_err(|e| (m::ErrorCode::Internal, format!("shared memory: {e}")))?;
        return Ok(ok(px.len(), &[], Some(name), None));
    }
    if r.zlib == Some(true) {
        let key = (r.pane_id.clone(), img.id, img.generation);
        let z = shared
            .compressed(key, || pixels(vt, r.image_id).map(|(_, px)| px))
            .ok_or_else(|| not_found("image data"))?;
        let start = (r.offset as usize).min(z.len());
        let end = start.saturating_add(CHUNK).min(z.len());
        return Ok(ok(z.len(), &z[start..end], None, Some(true)));
    }
    let offset = r.offset as usize;
    let (size, chunk) = match img.format {
        PixelFormat::Rgb | PixelFormat::Rgba => {
            let (_, bytes) = vt
                .image_bytes(r.image_id, offset, CHUNK)
                .ok_or_else(|| not_found("image data"))?;
            (img.len, bytes)
        }
        _ => {
            // Small in practice; converted whole so offsets refer to the RGBA bytes.
            let (_, rgba) = pixels(vt, r.image_id).ok_or_else(|| not_found("image data"))?;
            let end = offset.saturating_add(CHUNK).min(rgba.len());
            (rgba.len(), rgba[offset.min(end)..end].to_vec())
        }
    };
    Ok(ok(size, &chunk, None, None))
}

/// The images placed in `vt` (an image without placements is not listed).
pub fn list(pane: &str, vt: &Terminal) -> Vec<m::ImageInfo> {
    let all = vt.placements(false);
    let visible = vt.placements(true);
    let mut ids: Vec<u32> = all.iter().map(|p| p.image_id).collect();
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .filter_map(|id| {
            let img = vt.image(id)?;
            Some(m::ImageInfo {
                pane_id: pane.to_owned(),
                image_id: id,
                format: wire_format(img.format),
                width: img.width,
                height: img.height,
                bytes: img.len as u64,
                placements: all.iter().filter(|p| p.image_id == id).count() as u32,
                visible: visible.iter().any(|p| p.image_id == id),
            })
        })
        .collect()
}

/// The visible placements of a pane as frame entries (REQ-UI-052: the client clips).
pub fn placements(pane: &str, vt: &Terminal) -> Vec<m::ImagePlacement> {
    vt.placements(true)
        .into_iter()
        .map(|p| m::ImagePlacement {
            id: p.placement_id,
            pane_id: pane.to_owned(),
            x: p.col,
            y: p.row,
            cols: p.cols.min(u16::MAX as u32) as u16,
            rows: p.rows.min(u16::MAX as u32) as u16,
            image_id: p.image_id,
            generation: p.image_generation,
            src_x: p.src.0,
            src_y: p.src.1,
            src_w: p.src.2,
            src_h: p.src.3,
            x_off: Some(p.offset.0),
            y_off: Some(p.offset.1),
            z: p.z,
        })
        .collect()
}
