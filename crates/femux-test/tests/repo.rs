//! REQ-ARCH-*, REQ-BUILD-*: checks on the workspace itself, plus client restarts.

use std::path::PathBuf;
use std::time::Duration;

use femux_proto::msg::*;
use femux_test::Harness;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(p: &str) -> String {
    std::fs::read_to_string(root().join(p)).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// Dependency names in a manifest's [dependencies] table.
fn deps(manifest: &str) -> Vec<String> {
    let text = read(manifest);
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[dependencies]";
            continue;
        }
        if inside
            && let Some((k, _)) = line.split_once('=')
            && !k.trim().starts_with('#')
        {
            out.push(k.trim().to_owned());
        }
    }
    out
}

const UI_LIBS: &[&str] = &[
    "termwiz",
    "crossterm",
    "ratatui",
    "termion",
    "tui",
    "vt100",
    "notify-rust",
    "cursive",
];

#[test]
fn arch_001_daemon_has_no_ui_dependencies() {
    let d = deps("crates/femux-daemon/Cargo.toml");
    assert!(d.contains(&"femux-proto".to_owned()));
    for lib in UI_LIBS {
        assert!(!d.iter().any(|x| x == lib), "femux-daemon depends on {lib}");
    }
    // Code only: the docs say what the daemon does not contain.
    let src: String = std::fs::read_dir(root().join("crates/femux-daemon/src"))
        .unwrap()
        .flat_map(|e| {
            std::fs::read_to_string(e.unwrap().path())
                .unwrap()
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .map(|l| format!("{l}\n"))
                .collect::<Vec<_>>()
        })
        .collect();
    for word in ["keybind", "theme", "agent.state"] {
        assert!(!src.contains(word), "daemon source mentions {word}");
    }
}

#[test]
fn arch_002_clients_and_plugins_use_the_protocol_only() {
    let mut manifests = vec!["crates/femux-client/Cargo.toml".to_owned()];
    for e in std::fs::read_dir(root().join("crates/plugins")).unwrap() {
        manifests.push(format!(
            "crates/plugins/{}/Cargo.toml",
            e.unwrap().file_name().to_string_lossy()
        ));
    }
    for m in manifests {
        let d = deps(&m);
        for forbidden in ["femux-daemon", "femux-vt"] {
            assert!(
                !d.iter().any(|x| x == forbidden),
                "{m} depends on {forbidden}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn arch_003_client_restart_keeps_panes() {
    let h = Harness::start();
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let pane = c.attach("s", 80, 24).await.snapshot.tabs[0].focus.clone();
    c.wait_text(&pane, "$").await;
    c.type_text(&pane, "sleep 300\n").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = c
        .call(PaneInfo {
            pane_id: pane.clone(),
        })
        .await
        .unwrap();
    assert_eq!(before.fg_cmd, "sleep");
    drop(c); // the client goes away without detaching
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut c = h.client().await;
    let after = c
        .call(PaneInfo {
            pane_id: pane.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        (after.fg_pid, after.fg_cmd.as_str()),
        (before.fg_pid, "sleep")
    );
    assert_eq!(c.call(SessionList {}).await.unwrap().sessions[0].clients, 0);
}

#[test]
fn build_001_pinned_zig_build() {
    let pin = read("zig-version");
    let zon = read("crates/femux-vt/zig/build.zig.zon");
    assert!(zon.contains(&format!(".minimum_zig_version = \"{}\"", pin.trim())));
    assert!(
        zon.contains("ghostty/archive/") && zon.contains(".hash = "),
        "ghostty is pinned by commit and hash"
    );
    assert!(read("mise.toml").contains(&format!("zig = \"{}\"", pin.trim())));
    assert!(
        read("crates/femux-vt/build.rs").contains("ghostty-vt-static"),
        "libghostty-vt is linked statically"
    );
}

#[test]
fn build_002_rust_is_pinned() {
    let channel = read("rust-toolchain.toml")
        .lines()
        .find_map(|l| {
            l.strip_prefix("channel = ")
                .map(|v| v.trim_matches('"').to_owned())
        })
        .unwrap();
    assert!(
        channel.split('.').count() == 3 && channel.chars().all(|c| c.is_ascii_digit() || c == '.'),
        "{channel}"
    );
    assert!(read("mise.toml").contains(&format!("rust = \"{channel}\"")));
}

#[test]
fn build_003_ci_runs_lint_and_tests_on_both_platforms() {
    let ci = read(".github/workflows/ci.yml");
    for needle in [
        "ubuntu-latest",
        "macos-latest",
        "cargo clippy --workspace --all-targets -- -D warnings",
        "cargo test --workspace",
    ] {
        assert!(ci.contains(needle), "ci.yml lacks {needle}");
    }
}

/// REQ-TEST-002: every 0.1.0 requirement in SPEC.md has a passing test in conformance.md.
#[test]
fn test_002_every_milestone_requirement_is_mapped() {
    let spec = read("spec/SPEC.md");
    // §13, 0.1.0: ARCH-*, PTY-*, VT-001–005, QRY-*, SESS-*, VIEW-*, EVT-*, LIFE-001–002, PROTO-*, BUILD-*.
    let whole = [
        "ARCH", "PTY", "QRY", "SESS", "VIEW", "EVT", "PROTO", "BUILD",
    ];
    let ranged = [("VT", 1..=5), ("LIFE", 1..=2)];
    let mut want = std::collections::BTreeSet::new();
    for line in spec.lines() {
        let Some(rest) = line.strip_prefix("- REQ-") else {
            continue;
        };
        let id = rest.split(':').next().unwrap();
        let Some((family, num)) = id.split_once('-') else {
            continue;
        };
        let n: u32 = num.split_whitespace().next().unwrap().parse().unwrap();
        if whole.contains(&family) || ranged.iter().any(|(f, r)| *f == family && r.contains(&n)) {
            // Deferred requirements are not part of the milestone.
            if !line.contains("(Deferred") {
                want.insert(format!("REQ-{family}-{num}"));
            }
        }
    }
    assert!(want.len() > 40, "{want:?}");
    let conf = read("spec/conformance.md");
    let mut bad = Vec::new();
    for req in &want {
        let row = conf.lines().find(|l| l.starts_with(&format!("| {req} |")));
        match row.map(|r| r.split('|').map(str::trim).collect::<Vec<_>>()) {
            Some(cols) if cols[2] != "–" && !cols[2].is_empty() && cols[3] == "pass" => {}
            other => bad.push(format!("{req}: {other:?}")),
        }
    }
    assert!(bad.is_empty(), "unmapped or failing: {bad:#?}");
}
