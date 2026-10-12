//! REQ-TEST-003: every message validates against its JSON Schema in both directions.
//!
//! For each example in `spec/protocol/examples/messages.json`: the JSON body validates against
//! the schema def for its kind; it is encoded as MessagePack, decoded into the generated Rust
//! type, re-encoded by femux-proto, and the result again validates (envelope and body) and
//! equals the original. Every kind must have at least one example.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use femux_proto::{Event, Push, Request, Response, ServerMsg, decode_envelope, encode_frame};
use jsonschema::{Registry, Validator};
use serde_json::{Value, json};

const BASE: &str = "https://femux.dev/protocol/";

fn protocol_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/protocol")
}

fn schema_files() -> Vec<(String, Value)> {
    let mut files: Vec<_> = fs::read_dir(protocol_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_str().unwrap().to_owned();
            (
                name,
                serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap(),
            )
        })
        .collect()
}

/// `(dir, kind)` -> `(file, def)`.
fn kind_defs(files: &[(String, Value)]) -> BTreeMap<(String, String), (String, String)> {
    let mut defs = BTreeMap::new();
    let def_file = |def: &str| -> String {
        files
            .iter()
            .find(|(_, d)| d["$defs"].get(def).is_some())
            .unwrap()
            .0
            .clone()
    };
    for (_, doc) in files {
        for m in doc["x-messages"].as_array().into_iter().flatten() {
            let kind = m["kind"].as_str().unwrap().to_owned();
            let resp_kind = m
                .get("response_kind")
                .and_then(Value::as_str)
                .unwrap_or(&kind)
                .to_owned();
            let (req, resp) = (
                m["request"].as_str().unwrap(),
                m["response"].as_str().unwrap(),
            );
            defs.insert(("request".into(), kind), (def_file(req), req.to_owned()));
            defs.insert(
                ("response".into(), resp_kind),
                (def_file(resp), resp.to_owned()),
            );
        }
        for (key, dir) in [("x-events", "event"), ("x-pushes", "push")] {
            for e in doc[key].as_array().into_iter().flatten() {
                let body = e["body"].as_str().unwrap();
                defs.insert(
                    (dir.into(), e["kind"].as_str().unwrap().into()),
                    (def_file(body), body.into()),
                );
            }
        }
    }
    defs.insert(
        ("error".into(), "error".into()),
        ("envelope.json".into(), "ErrorBody".into()),
    );
    defs
}

struct Schemas {
    validators: BTreeMap<String, Validator>,
}

impl Schemas {
    fn new(files: &[(String, Value)], defs: impl Iterator<Item = (String, String)>) -> Self {
        let mut registry = Registry::new();
        for (name, doc) in files {
            registry = registry.add(format!("{BASE}{name}"), doc.clone()).unwrap();
        }
        let registry = registry.prepare().unwrap();
        let mut validators = BTreeMap::new();
        for (file, def) in defs.chain([("envelope.json".to_owned(), "Envelope".to_owned())]) {
            let schema = json!({"$ref": format!("{BASE}{file}#/$defs/{def}")});
            let v = jsonschema::options()
                .with_registry(&registry)
                .build(&schema)
                .unwrap();
            validators.insert(def, v);
        }
        Schemas { validators }
    }

    fn check(&self, def: &str, value: &Value, what: &str) {
        let errors: Vec<String> = self.validators[def]
            .iter_errors(value)
            .map(|e| format!("{e} at {}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{what} fails {def}: {errors:?}\n{value:#}"
        );
    }
}

fn examples() -> Vec<Value> {
    let path = protocol_dir().join("examples/messages.json");
    let doc: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    doc["examples"].as_array().unwrap().clone()
}

/// Re-encodes one example through the typed Rust API; returns the frame femux-proto produced.
fn roundtrip(dir: &str, kind: &str, id: u64, wire: &[u8]) -> Vec<u8> {
    let env = decode_envelope(&wire[4..]).unwrap();
    assert_eq!(env.kind, kind);
    match dir {
        "request" => Request::decode(kind, env.body).unwrap().encode(id).unwrap(),
        "response" => match ServerMsg::decode(env).unwrap() {
            ServerMsg::Response(_, r) => {
                assert_eq!(r.kind(), kind);
                r.encode(id).unwrap()
            }
            m => panic!("{kind}: decoded as {m:?}"),
        },
        "event" => match ServerMsg::decode(env).unwrap() {
            ServerMsg::Event(e) => e.encode(0).unwrap(),
            m => panic!("{kind}: decoded as {m:?}"),
        },
        "push" => match ServerMsg::decode(env).unwrap() {
            ServerMsg::Push(p) => p.encode(0).unwrap(),
            m => panic!("{kind}: decoded as {m:?}"),
        },
        "error" => match ServerMsg::decode(env).unwrap() {
            ServerMsg::Error(i, e) => encode_frame(i, "error", &e).unwrap(),
            m => panic!("{kind}: decoded as {m:?}"),
        },
        _ => panic!("unknown dir {dir}"),
    }
}

#[test]
fn every_message_matches_its_schema_both_ways() {
    let files = schema_files();
    let defs = kind_defs(&files);
    let schemas = Schemas::new(
        &files,
        defs.values().cloned().collect::<BTreeSet<_>>().into_iter(),
    );

    let mut seen = BTreeSet::new();
    for ex in examples() {
        let (dir, kind) = (ex["dir"].as_str().unwrap(), ex["kind"].as_str().unwrap());
        let (_, def) = defs
            .get(&(dir.to_owned(), kind.to_owned()))
            .unwrap_or_else(|| panic!("no schema for {dir} {kind}"));
        let body = &ex["body"];
        schemas.check(def, body, &format!("example {dir} {kind}"));

        let id = if matches!(dir, "event" | "push") {
            0
        } else {
            5
        };
        let wire = encode_frame(id, kind, body).unwrap();
        let back = roundtrip(dir, kind, id, &wire);

        let env: Value = rmp_serde::from_slice(&back[4..]).unwrap();
        schemas.check(
            "Envelope",
            &env,
            &format!("re-encoded {dir} {kind} envelope"),
        );
        schemas.check(def, &env["body"], &format!("re-encoded {dir} {kind}"));
        assert_eq!(&env["body"], body, "{dir} {kind} changed in the round trip");
        assert_eq!(env["id"], json!(id));
        seen.insert((dir.to_owned(), kind.to_owned()));
    }

    let missing: Vec<_> = defs.keys().filter(|k| !seen.contains(*k)).collect();
    assert!(missing.is_empty(), "kinds without an example: {missing:?}");
}

#[test]
fn generated_kinds_match_schema() {
    let files = schema_files();
    let defs = kind_defs(&files);
    let of = |dir: &str| -> BTreeSet<&str> {
        defs.keys()
            .filter(|(d, _)| d == dir)
            .map(|(_, k)| k.as_str())
            .collect()
    };
    assert_eq!(of("request"), Request::KINDS.iter().copied().collect());
    assert_eq!(of("response"), Response::KINDS.iter().copied().collect());
    assert_eq!(of("event"), Event::KINDS.iter().copied().collect());
    assert_eq!(of("push"), Push::KINDS.iter().copied().collect());
}

#[test]
fn invalid_bodies_are_rejected_by_schema_and_decoder() {
    let files = schema_files();
    let defs = kind_defs(&files);
    let schemas = Schemas::new(
        &files,
        defs.values().cloned().collect::<BTreeSet<_>>().into_iter(),
    );
    for (kind, def, body) in [
        (
            "pane.signal",
            "PaneSignal",
            json!({"pane_id": "01J9Z3K8M2N4P6Q8R0S2T4V6W8", "sig": "SIGSTOP"}),
        ),
        ("session.create", "SessionCreate", json!({"root": "/tmp"})),
        (
            "view.resize",
            "ViewResize",
            json!({"view_id": "01J9Z3K8M2N4P6Q8R0S2T4V6WB", "cols": -1, "rows": 2}),
        ),
    ] {
        assert!(
            !schemas.validators[def].is_valid(&body),
            "{kind} should fail the schema"
        );
        let wire = encode_frame(1, kind, &body).unwrap();
        let env = decode_envelope(&wire[4..]).unwrap();
        assert!(
            Request::decode(kind, env.body).is_err(),
            "{kind} should fail to decode"
        );
    }
}
