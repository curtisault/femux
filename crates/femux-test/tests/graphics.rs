//! REQ-GFX-001/003/004 (daemon side): a program's Kitty images are stored per pane, frames
//! carry their placements relative to the pane, `image.read` returns the pixels, and a new
//! attach gets the placements again.

use std::time::Duration;

use femux_proto::msg::*;
use femux_test::{Client, Harness};

/// A 2x2 red image shown over 4x2 cells, below the text.
const ICAT: &str = "printf '\\033_Ga=T,f=24,s=2,v=2,i=7,c=4,r=2,z=-1;/wAA/wAA/wAA/wAA\\033\\\\'; echo; echo shown\n";

fn images(c: &Client) -> Vec<ImagePlacement> {
    c.frames
        .last()
        .map(|f| f.images.clone())
        .unwrap_or_default()
}

async fn wait_images(c: &mut Client, what: &str, f: impl Fn(&[ImagePlacement]) -> bool) {
    c.wait_for(what, Duration::from_secs(10), |c| {
        c.frames.last().is_some_and(|fr| f(&fr.images))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gfx_001_003_placements_reach_clients_and_survive_reattach() {
    let h = Harness::start();
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    let att = c.attach("s", 80, 24).await;
    c.wait_text(&pane, "$").await;
    c.type_text(&pane, ICAT).await;
    c.wait_text(&pane, "shown").await;
    wait_images(&mut c, "a placement", |i| !i.is_empty()).await;
    let p = images(&c)[0].clone();
    assert_eq!(p.pane_id, pane);
    assert_eq!((p.image_id, p.cols, p.rows, p.z), (7, 4, 2, -1));
    assert_eq!((p.src_w, p.src_h), (2, 2));
    assert_eq!(p.x, 0);
    assert!(p.y >= 0 && p.y < 24, "{p:?}");
    // The pixels.
    let r = c
        .call(ImageRead {
            pane_id: pane.clone(),
            image_id: 7,
            offset: 0,
            shm: None,
            zlib: None,
        })
        .await
        .unwrap();
    assert_eq!(
        (r.format, r.width, r.height, r.size),
        (ImageFormat::Rgb, 2, 2, 12)
    );
    assert_eq!(r.data, "/wAA/wAA/wAA/wAA");
    assert_eq!(r.generation, p.generation);
    assert!(
        c.call(ImageRead {
            pane_id: pane.clone(),
            image_id: 99,
            offset: 0,
            shm: None,
            zlib: None,
        })
        .await
        .is_err()
    );
    // image.list.
    let list = c.call(ImageList { pane_id: None }).await.unwrap().images;
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].image_id, list[0].bytes, list[0].visible),
        (7, 12, true)
    );
    // REQ-GFX-003: detach and attach again; the placement comes back with the first frame.
    c.call(SessionDetach {
        view_id: att.view_id,
    })
    .await
    .unwrap();
    let mut again = h.client().await;
    again.attach("s", 80, 24).await;
    wait_images(&mut again, "the placement after reattach", |i| {
        i.iter().any(|q| q.image_id == 7 && q.y == p.y)
    })
    .await;
    // Scrolled out of view: no placement, the image still stored.
    again
        .type_text(
            &pane,
            "clear; for i in $(seq 40); do echo; done; echo bottom\n",
        )
        .await;
    again.wait_text(&pane, "bottom").await;
    wait_images(&mut again, "no placement", |i| i.is_empty()).await;
    let list = again
        .call(ImageList {
            pane_id: Some(pane.clone()),
        })
        .await
        .unwrap()
        .images;
    assert!(list.iter().all(|i| !i.visible));
}

#[tokio::test(flavor = "multi_thread")]
async fn gfx_001_the_pty_reports_the_clients_cell_size_in_pixels() {
    let h = Harness::start();
    let mut hello = Client::hello(80, 24);
    if let Some(t) = &mut hello.term {
        t.cell_w = Some(9);
        t.cell_h = Some(18);
    }
    let mut c = Client::connect(&h.socket, hello).await.unwrap();
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 80, 24).await;
    c.wait_text(&pane, "$").await;
    // XTWINOPS 14t answers the text area in pixels from the cell size.
    c.type_text(&pane, "printf '\\033[14t'; read -r x; echo got-$x\n")
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    c.type_text(&pane, "\n").await;
    let rows = c.panes[&pane].rect.h as u32;
    let cols = c.panes[&pane].rect.w as u32;
    c.wait_text(&pane, &format!("4;{};{}t", rows * 18, cols * 9))
        .await;
}

/// 0.7.0 acceptance, with a real program when it is installed: `mpv --vo=kitty` draws video
/// frames into a pane (skipped where mpv is missing, as on CI).
#[tokio::test(flavor = "multi_thread")]
async fn acceptance_mpv_plays_into_a_pane() {
    if std::process::Command::new("mpv")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("mpv not installed: skipped");
        return;
    }
    let h = Harness::start();
    let mut hello = Client::hello(80, 24);
    if let Some(t) = &mut hello.term {
        t.kitty_graphics = true;
        t.cell_w = Some(10);
        t.cell_h = Some(20);
    }
    let mut c = Client::connect(&h.socket, hello).await.unwrap();
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 80, 24).await;
    c.wait_text(&pane, "$").await;
    c.type_text(
        &pane,
        "mpv --vo=kitty --really-quiet --no-audio --frames=40 'av://lavfi:testsrc=size=160x120:rate=10'; echo mpv-done\n",
    )
    .await;
    let mut seen = std::collections::BTreeSet::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        c.recv(Duration::from_millis(100)).await;
        if let Some(f) = c.frames.last() {
            seen.extend(f.images.iter().map(|p| p.generation));
        }
        if seen.len() >= 3 {
            break;
        }
    }
    assert!(
        seen.len() >= 3,
        "frames with images: {seen:?}; screen: {}",
        c.screen_text(&pane)
    );
    let p = c
        .frames
        .iter()
        .rev()
        .find_map(|f| f.images.first().cloned())
        .unwrap();
    let r = c
        .call(ImageRead {
            pane_id: pane.clone(),
            image_id: p.image_id,
            offset: 0,
            shm: None,
            zlib: None,
        })
        .await;
    // The image may have been replaced meanwhile; when it is there it is whole video frames.
    if let Ok(r) = r {
        assert!(r.width >= 100 && r.height >= 60, "{}x{}", r.width, r.height);
    }
}

/// yazi previews images in a pane (it draws with Kitty Unicode placeholders); skipped where
/// yazi is not installed.
#[tokio::test(flavor = "multi_thread")]
async fn yazi_previews_show_up_as_placements() {
    if std::process::Command::new("yazi")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("yazi not installed: skipped");
        return;
    }
    let design =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/design/pre-0.10.0");
    let h = Harness::start();
    let mut hello = Client::hello(120, 30);
    if let Some(t) = &mut hello.term {
        t.kitty_graphics = true;
        t.cell_w = Some(10);
        t.cell_h = Some(20);
    }
    let mut c = Client::connect(&h.socket, hello).await.unwrap();
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 120, 30).await;
    c.wait_text(&pane, "$").await;
    c.type_text(&pane, &format!("yazi {}/inbox.png\n", design.display()))
        .await;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline
        && c.frames.last().is_none_or(|f| f.images.is_empty())
    {
        c.recv(Duration::from_millis(100)).await;
    }
    let images = c
        .frames
        .last()
        .map(|f| f.images.clone())
        .unwrap_or_default();
    c.type_text(&pane, "q").await;
    assert!(
        !images.is_empty(),
        "no preview; screen:\n{}",
        c.screen_text(&pane)
    );
}

/// ADR-021 #4: a local client takes images through shared memory, a remote one compressed.
#[tokio::test(flavor = "multi_thread")]
async fn image_reads_through_shared_memory_and_zlib() {
    use base64::Engine;
    use std::io::Read;
    let h = Harness::start();
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 80, 24).await;
    c.wait_text(&pane, "$").await;
    c.type_text(&pane, ICAT).await;
    c.wait_text(&pane, "shown").await;
    // Shared memory: the bytes are in the named object, `data` is empty.
    let r = c
        .call(ImageRead {
            pane_id: pane.clone(),
            image_id: 7,
            offset: 0,
            shm: Some(true),
            zlib: None,
        })
        .await
        .unwrap();
    let name = r.shm.clone().expect("a shared memory name");
    assert!(r.data.is_empty());
    assert_eq!(r.size, 12);
    let bytes = unsafe {
        let c = std::ffi::CString::new(name.clone()).unwrap();
        let fd = libc::shm_open(c.as_ptr(), libc::O_RDONLY, 0);
        assert!(fd >= 0, "shm_open {name}");
        let map = libc::mmap(
            std::ptr::null_mut(),
            12,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        );
        assert_ne!(map, libc::MAP_FAILED);
        let v = std::slice::from_raw_parts(map as *const u8, 12).to_vec();
        libc::munmap(map, 12);
        libc::close(fd);
        // What the host terminal does after reading.
        libc::shm_unlink(c.as_ptr());
        v
    };
    assert_eq!(bytes, [255, 0, 0].repeat(4));
    // zlib: inflating `data` gives the pixels.
    let r = c
        .call(ImageRead {
            pane_id: pane.clone(),
            image_id: 7,
            offset: 0,
            shm: None,
            zlib: Some(true),
        })
        .await
        .unwrap();
    assert_eq!(r.zlib, Some(true));
    let z = base64::engine::general_purpose::STANDARD
        .decode(&r.data)
        .unwrap();
    assert_eq!(z.len() as u64, r.size);
    let mut px = Vec::new();
    flate2::read::ZlibDecoder::new(&z[..])
        .read_to_end(&mut px)
        .unwrap();
    assert_eq!(px, [255, 0, 0].repeat(4));
}
