//! Builds the pinned libghostty-vt (see zig/build.zig.zon) with Zig and links it statically.
//! Zig is taken from `$ZIG`, else `zig` on PATH; the required version is in /zig-version.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let zig_dir = manifest.join("zig");
    let prefix = out.join("ghostty-vt");

    println!("cargo:rerun-if-changed=zig/build.zig");
    println!("cargo:rerun-if-changed=zig/build.zig.zon");
    println!("cargo:rerun-if-env-changed=ZIG");

    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".into());
    check_zig_version(&zig, &manifest.join("../../zig-version"));
    let mut cmd = Command::new(&zig);
    cmd.current_dir(&zig_dir)
        .arg("build")
        .arg("-Doptimize=ReleaseFast")
        .arg("--prefix")
        .arg(&prefix)
        .arg("--cache-dir")
        .arg(out.join("zig-cache"));
    // Always pass an explicit target so the library is built for a baseline CPU,
    // not the build host's native one.
    if let Some(t) = zig_target(&env::var("TARGET").unwrap()) {
        cmd.arg(format!("-Dtarget={t}"));
    }

    let status = cmd.status().unwrap_or_else(|e| {
        panic!("failed to run `{zig}` ({e}); install Zig as described in CONTRIBUTING.md")
    });
    assert!(status.success(), "zig build of libghostty-vt failed");

    let lib_dir = prefix.join("lib");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        repack_for_apple_ld(&zig, &lib_dir);
    }

    // libghostty-vt first, then the SIMD archives it depends on.
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    for entry in std::fs::read_dir(&lib_dir).expect("zig installed no lib/ dir") {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if let Some(stem) = name.strip_prefix("lib").and_then(|n| n.strip_suffix(".a"))
            && stem != "ghostty-vt-static"
        {
            println!("cargo:rustc-link-lib=static={stem}");
        }
    }
    println!("cargo:include={}", prefix.join("include").display());
}

/// Zig's archiver does not 8-byte align Mach-O members, which Apple's ld rejects, and Apple's
/// libtool drops members it cannot read from such archives. So each archive is unpacked with
/// `zig ar` (LLVM reads it fine) and repacked from its object files with Apple's libtool.
fn repack_for_apple_ld(zig: &str, lib_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(lib_dir).expect("zig installed no lib/ dir") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "a") {
            continue;
        }
        let dir = path.with_extension("objects");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new(zig)
            .args(["ar", "x"])
            .arg(&path)
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success(), "zig ar x failed on {}", path.display());
        let mut objects: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        objects.sort();
        for o in &objects {
            // Zig records members with mode 0.
            std::fs::set_permissions(o, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let tmp = path.with_extension("repacked.a");
        let status = Command::new("libtool")
            .args(["-static", "-o"])
            .arg(&tmp)
            .args(&objects)
            .status()
            .expect("failed to run Apple libtool (install the Xcode command line tools)");
        assert!(status.success(), "libtool failed on {}", path.display());
        std::fs::rename(&tmp, &path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Fails early with an actionable message when Zig is missing or the wrong version.
fn check_zig_version(zig: &str, pin_file: &std::path::Path) {
    println!("cargo:rerun-if-changed={}", pin_file.display());
    let want = std::fs::read_to_string(pin_file).expect("missing zig-version file");
    let want = want.trim();
    let have = Command::new(zig).arg("version").output().unwrap_or_else(|e| {
        panic!("`{zig}` not found ({e}). femux needs Zig {want}: run `mise install` (see CONTRIBUTING.md)")
    });
    let have = String::from_utf8_lossy(&have.stdout);
    let have = have.trim();
    // Ghostty requires an exact minor version; patch releases are compatible.
    let minor = |v: &str| v.splitn(3, '.').take(2).collect::<Vec<_>>().join(".");
    assert!(
        minor(have) == minor(want),
        "femux needs Zig {want}, found {have} at `{zig}`. Run `mise install` or set ZIG=/path/to/zig-{want}"
    );
}

/// Maps a Rust target triple to a Zig target triple.
fn zig_target(rust: &str) -> Option<String> {
    let mut parts = rust.split('-');
    let arch = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    let os = if rest.contains(&"linux") {
        "linux"
    } else if rest.contains(&"darwin") {
        "macos"
    } else {
        return None;
    };
    let abi = match rest.last() {
        Some(&"musl") => "-musl",
        Some(&"gnu") => "-gnu",
        _ => "",
    };
    Some(format!("{arch}-{os}{abi}"))
}
