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

fn plugin_dirs() -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root().join("crates/plugins"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn arch_002_clients_and_plugins_use_the_protocol_only() {
    let mut manifests = vec![
        "crates/femux-client/Cargo.toml".to_owned(),
        "crates/femux-plugin/Cargo.toml".to_owned(),
    ];
    for p in plugin_dirs() {
        manifests.push(format!("crates/plugins/{p}/Cargo.toml"));
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

/// REQ-PLUG-001: one crate per plugin under crates/plugins, one Cargo feature each in the
/// `femux` binary, all on by default.
#[test]
fn plug_001_plugins_are_feature_gated_crates() {
    let cli = read("crates/femux-cli/Cargo.toml");
    let plugins = plugin_dirs();
    assert!(!plugins.is_empty());
    let default_line = cli
        .lines()
        .find(|l| l.starts_with("default = ["))
        .expect("femux-cli has a default feature list");
    for p in &plugins {
        assert!(
            cli.contains(&format!("{p} = [\"dep:femux-plugin-{p}\"]")),
            "no feature for plugin {p}"
        );
        assert!(
            default_line.contains(&format!("\"{p}\"")),
            "{p} is off by default"
        );
        assert!(
            cli.contains(&format!(
                "femux-plugin-{p} = {{ path = \"../plugins/{p}\", optional = true }}"
            )),
            "{p} is not an optional dependency"
        );
        assert!(
            read(&format!("crates/plugins/{p}/src/lib.rs")).contains("pub fn plugin() -> "),
            "{p} exports plugin()"
        );
    }
    // Runtime activation is by name from `[plugins] enabled`.
    let example = read("spec/config.example.toml");
    assert!(example.contains("[plugins]") && example.contains("enabled = ["));
}

/// REQ-PLUG-003: plugin crates reach the core through femux-plugin / femux-proto only. The
/// femux-config parser is allowed: it reads files and talks to nothing.
#[test]
fn plug_003_plugins_depend_on_the_protocol_crates_only() {
    for p in plugin_dirs() {
        let d = deps(&format!("crates/plugins/{p}/Cargo.toml"));
        for dep in d.iter().filter(|d| d.starts_with("femux-")) {
            assert!(
                matches!(
                    dep.as_str(),
                    "femux-plugin" | "femux-proto" | "femux-config"
                ),
                "plugin {p} depends on {dep}"
            );
        }
    }
    let config = deps("crates/femux-config/Cargo.toml");
    assert!(
        config.iter().all(|d| !d.starts_with("femux-")),
        "femux-config depends on nothing of femux: {config:?}"
    );
}

/// REQ-PLUG-004: the built-in list, and each crate's version is its milestone.
#[test]
fn plug_004_built_in_list_matches_crates_and_milestones() {
    let mut listed: Vec<String> = femux_plugin::BUILT_IN
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();
    listed.sort();
    assert_eq!(listed, plugin_dirs());
    for (name, milestone) in femux_plugin::BUILT_IN {
        let manifest = read(&format!("crates/plugins/{name}/Cargo.toml"));
        assert!(
            manifest.contains(&format!("version = \"{milestone}.0\"")),
            "{name} is versioned {milestone}.0"
        );
    }
    let spec = read("spec/SPEC.md");
    for (name, milestone) in femux_plugin::BUILT_IN {
        assert!(
            spec.contains(&format!("`{name}` ({milestone})")),
            "SPEC §8 lists {name} ({milestone})"
        );
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
    let mise = read("mise.toml");
    assert!(
        mise.contains(&format!("rust = {{ version = \"{channel}\"")),
        "mise.toml pins rust {channel}"
    );
    assert!(mise.contains("clippy"), "mise installs clippy (CI runs it)");
}

/// CI runs the `ci` task from mise.toml, which must hold clippy and the tests.
#[test]
fn build_003_ci_runs_lint_and_tests_on_both_platforms() {
    let ci = read(".github/workflows/ci.yml");
    for needle in ["ubuntu-latest", "macos-latest", "run: mise run ci"] {
        assert!(ci.contains(needle), "ci.yml lacks {needle}");
    }
    let mise = read("mise.toml");
    let task = mise
        .split("[tasks.ci]")
        .nth(1)
        .and_then(|t| t.split("\n[").next())
        .expect("mise.toml has a [tasks.ci]");
    for needle in [
        "cargo clippy --workspace --all-targets -- -D warnings",
        "cargo test --workspace",
    ] {
        assert!(task.contains(needle), "the ci task lacks {needle}");
    }
}

/// REQ-TEST-002: every requirement of a shipped milestone has a passing test in conformance.md.
#[test]
fn test_002_every_milestone_requirement_is_mapped() {
    let spec = read("spec/SPEC.md");
    // §13, 0.1.0: ARCH-*, PTY-*, VT-001–005, QRY-*, SESS-*, VIEW-*, EVT-*, LIFE-001–002, PROTO-*, BUILD-*.
    // §13, 0.2.0: UI-001–012, UI-020–021, UI-043, UI-050–053, CLI-*, plus PLUG-* (built for its plugins).
    // §13, 0.3.0: STORE-*, CFG-*.
    let whole = [
        "ARCH", "PTY", "QRY", "SESS", "VIEW", "EVT", "PROTO", "BUILD", "CLI", "PLUG", "STORE",
        "CFG",
    ];
    let ranged = [
        ("VT", 1..=5),
        ("LIFE", 1..=2),
        ("UI", 1..=12),
        ("UI", 20..=21),
        ("UI", 43..=43),
        ("UI", 50..=53),
    ];
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
