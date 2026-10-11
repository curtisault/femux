//! Generates the protocol types from `spec/protocol/*.json` (REQ-PROTO-010).
//!
//! Supported JSON Schema subset; anything else fails the build:
//! - `$ref` to `#/$defs/X` or `file.json#/$defs/X`. Def names are one global namespace.
//! - Primitive defs (`integer` with `format` uint8..int64, `string`, `boolean`) and ref-only
//!   defs become type aliases.
//! - `object` with `properties` becomes a struct; properties not in `required` are `Option`.
//!   `object` with only `additionalProperties` becomes a `BTreeMap<String, T>`.
//! - `string` + `enum` becomes a unit enum; `const` string becomes a unit struct.
//! - `oneOf` of object refs that all have a required `const` property becomes an internally
//!   tagged enum on that property; any other `oneOf` becomes an untagged enum (variant name
//!   from `title`, else the ref name).
//! - `array` becomes `Vec<T>`, or `[T; N]` when `minItems == maxItems`.
//! - `{}` (any) becomes `rmpv::Value`.
//!
//! Top-level `x-messages` (`kind`, `request`, `response`, optional `response_kind`),
//! `x-events` and `x-pushes` (`kind`, `body`) generate the dispatch enums.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = manifest.join("../../spec/protocol");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("spec/protocol missing")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();

    let mut g = Gen::default();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let doc: Value = serde_json::from_str(&fs::read_to_string(f).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        g.load(&doc, f.file_name().unwrap().to_str().unwrap());
    }
    let code = g.generate();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("protocol.rs");
    fs::write(out, code).unwrap();
}

/// A tagged union: tag property and `(variant, def, tag value)` per alternative.
type Union = (String, Vec<(String, String, String)>);

struct Msg {
    kind: String,
    request: String,
    response: String,
    response_kind: String,
}

struct Push {
    kind: String,
    body: String,
}

#[derive(Default)]
struct Gen {
    defs: BTreeMap<String, (String, Value)>,
    order: Vec<String>,
    messages: Vec<Msg>,
    events: Vec<Push>,
    pushes: Vec<Push>,
    /// Struct defs used as tagged-union variants, with the tag property to omit.
    stripped: BTreeMap<String, String>,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

impl Gen {
    fn load(&mut self, doc: &Value, file: &str) {
        let defs = doc["$defs"]
            .as_object()
            .unwrap_or_else(|| panic!("{file}: no $defs"));
        for (name, schema) in defs {
            if let Some((other, _)) = self.defs.get(name) {
                panic!("def {name} defined in both {other} and {file}");
            }
            self.defs
                .insert(name.clone(), (file.to_owned(), schema.clone()));
            self.order.push(name.clone());
        }
        for m in doc
            .get("x-messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let kind = s(m, "kind").unwrap();
            self.messages.push(Msg {
                response_kind: s(m, "response_kind").unwrap_or_else(|| kind.clone()),
                kind,
                request: s(m, "request").unwrap(),
                response: s(m, "response").unwrap(),
            });
        }
        for (key, list) in [
            ("x-events", &mut self.events),
            ("x-pushes", &mut self.pushes),
        ] {
            for p in doc.get(key).and_then(Value::as_array).into_iter().flatten() {
                list.push(Push {
                    kind: s(p, "kind").unwrap(),
                    body: s(p, "body").unwrap(),
                });
            }
        }
    }

    fn ref_name(r: &str) -> String {
        r.rsplit_once("#/$defs/")
            .unwrap_or_else(|| panic!("unsupported $ref {r}"))
            .1
            .to_owned()
    }

    fn def(&self, name: &str) -> &Value {
        &self
            .defs
            .get(name)
            .unwrap_or_else(|| panic!("unknown def {name}"))
            .1
    }

    /// The Rust primitive a def aliases, if it is an alias.
    fn alias_target(&self, schema: &Value) -> Option<String> {
        if let Some(r) = s(schema, "$ref") {
            return Some(
                self.ty(&Value::Object(
                    [("$ref".to_owned(), Value::String(r))]
                        .into_iter()
                        .collect(),
                )),
            );
        }
        match schema.get("type").and_then(Value::as_str) {
            Some("integer") | Some("boolean") => Some(self.ty(schema)),
            Some("string") if schema.get("enum").is_none() => Some("String".into()),
            Some("array") => Some(self.ty(schema)),
            Some("object")
                if schema.get("properties").is_none()
                    && schema.get("additionalProperties").is_some() =>
            {
                Some(self.ty(schema))
            }
            _ => None,
        }
    }

    /// Rust type for an inline schema (a property, array item or ref).
    fn ty(&self, schema: &Value) -> String {
        if let Some(r) = s(schema, "$ref") {
            let name = Self::ref_name(&r);
            let target = self.def(&name);
            // Primitive aliases are inlined so docs show the real type.
            return match target.get("type").and_then(Value::as_str) {
                Some("integer") | Some("boolean") => self.ty(target),
                _ => name,
            };
        }
        let obj = schema.as_object().unwrap();
        let meaningful = obj.keys().any(|k| k != "description" && k != "title");
        if !meaningful {
            return "rmpv::Value".into();
        }
        match s(schema, "type").as_deref() {
            Some("integer") => match s(schema, "format").as_deref() {
                Some("uint8") => "u8",
                Some("uint16") => "u16",
                Some("uint32") => "u32",
                Some("uint64") => "u64",
                Some("int32") => "i32",
                Some("int64") => "i64",
                f => panic!("integer needs a format, got {f:?}"),
            }
            .into(),
            Some("boolean") => "bool".into(),
            Some("string") if schema.get("enum").is_none() && schema.get("const").is_none() => {
                "String".into()
            }
            Some("array") => {
                let item = self.ty(&schema["items"]);
                match (schema.get("minItems"), schema.get("maxItems")) {
                    (Some(a), Some(b))
                        if a == b && a.as_u64().unwrap() <= 32 && is_primitive(&item) =>
                    {
                        format!("[{item}; {}]", a.as_u64().unwrap())
                    }
                    _ => format!("Vec<{item}>"),
                }
            }
            Some("object")
                if schema.get("properties").is_none()
                    && schema
                        .get("additionalProperties")
                        .is_some_and(Value::is_object) =>
            {
                format!(
                    "std::collections::BTreeMap<String, {}>",
                    self.ty(&schema["additionalProperties"])
                )
            }
            _ => panic!("inline schema must be a $ref to a named def: {schema}"),
        }
    }

    fn generate(&mut self) -> String {
        // Find tagged unions first so their variant structs omit the tag.
        let mut tagged: BTreeMap<String, Union> = BTreeMap::new();
        for name in &self.order {
            if let Some(t) = self.tagged_union(self.def(name)) {
                for (_, variant_def, _) in &t.1 {
                    if let Some(prev) = self.stripped.insert(variant_def.clone(), t.0.clone())
                        && prev != t.0
                    {
                        panic!("{variant_def} is a variant of unions with different tags");
                    }
                }
                tagged.insert(name.clone(), t);
            }
        }

        // A stripped variant struct cannot also appear on its own: it would lose its tag.
        for (name, (_, schema)) in &self.defs {
            if tagged.contains_key(name) {
                continue;
            }
            let text = schema.to_string();
            for v in self.stripped.keys() {
                if text.contains(&format!("#/$defs/{v}\"")) {
                    panic!("{v} is a tagged-union variant and is also used by {name}");
                }
            }
        }

        let mut out = String::from(
            "// @generated by femux-proto/build.rs from spec/protocol. Do not edit.\n\n",
        );
        for name in self.order.clone() {
            let schema = self.def(&name).clone();
            doc(&mut out, &schema, "");
            if let Some((tag, variants)) = tagged.get(&name) {
                self.emit_tagged(&mut out, &name, tag, variants);
            } else if let Some(t) = self.alias_target(&schema) {
                writeln!(out, "pub type {name} = {t};\n").unwrap();
            } else if let Some(c) = s(&schema, "const") {
                emit_const(&mut out, &name, &c);
            } else if let Some(vals) = schema.get("enum").and_then(Value::as_array) {
                emit_enum(&mut out, &name, vals);
            } else if let Some(alts) = schema.get("oneOf").and_then(Value::as_array) {
                self.emit_untagged(&mut out, &name, alts);
            } else if s(&schema, "type").as_deref() == Some("object") {
                self.emit_struct(&mut out, &name, &schema);
            } else {
                panic!("unsupported def {name}: {schema}");
            }
        }
        self.emit_dispatch(&mut out);
        out
    }

    /// `(tag, [(variant, def, tag value)])` if `schema` is a oneOf of tagged object refs.
    fn tagged_union(&self, schema: &Value) -> Option<Union> {
        let alts = schema.get("oneOf")?.as_array()?;
        let mut tag: Option<String> = None;
        let mut variants = vec![];
        for alt in alts {
            let def_name = Self::ref_name(&s(alt, "$ref")?);
            let def = self.def(&def_name);
            let props = def.get("properties")?.as_object()?;
            let required: BTreeSet<&str> = def
                .get("required")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let (k, v) = props.iter().find(|(k, v)| {
                v.get("const").is_some()
                    && required.contains(k.as_str())
                    && tag.as_deref().is_none_or(|t| t == k.as_str())
            })?;
            tag = Some(k.clone());
            let variant = s(alt, "title").unwrap_or_else(|| def_name.clone());
            variants.push((variant, def_name, s(v, "const")?));
        }
        Some((tag?, variants))
    }

    fn emit_struct(&self, out: &mut String, name: &str, schema: &Value) {
        let empty = Map::new();
        let props = schema
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let required: BTreeSet<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let skip = self.stripped.get(name);
        // rmpv::Value has no Default; structs that require one don't derive it.
        let default = !props.iter().any(|(f, fs)| {
            Some(f) != skip && required.contains(f.as_str()) && self.ty(fs) == "rmpv::Value"
        });
        let derive_default = if default { "Default, " } else { "" };
        writeln!(out, "#[derive(Debug, Clone, PartialEq, {derive_default}serde::Serialize, serde::Deserialize)]").unwrap();
        writeln!(out, "pub struct {name} {{").unwrap();
        for (field, fs) in props {
            if Some(field) == skip {
                continue;
            }
            doc(out, fs, "    ");
            let ident = rust_ident(field);
            let ty = self.ty(fs);
            if required.contains(field.as_str()) {
                writeln!(out, "    pub {ident}: {ty},").unwrap();
            } else {
                writeln!(
                    out,
                    "    #[serde(default, skip_serializing_if = \"Option::is_none\")]"
                )
                .unwrap();
                writeln!(out, "    pub {ident}: Option<{ty}>,").unwrap();
            }
        }
        writeln!(out, "}}\n").unwrap();
    }

    fn emit_tagged(
        &self,
        out: &mut String,
        name: &str,
        tag: &str,
        variants: &[(String, String, String)],
    ) {
        writeln!(
            out,
            "#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]"
        )
        .unwrap();
        writeln!(out, "#[serde(tag = \"{tag}\")]").unwrap();
        writeln!(out, "pub enum {name} {{").unwrap();
        for (variant, def, value) in variants {
            writeln!(
                out,
                "    #[serde(rename = \"{value}\")]\n    {variant}({def}),"
            )
            .unwrap();
        }
        writeln!(out, "}}\n").unwrap();
        emit_union_default(out, name, &variants[0].0);
    }

    fn emit_untagged(&self, out: &mut String, name: &str, alts: &[Value]) {
        writeln!(
            out,
            "#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]"
        )
        .unwrap();
        writeln!(out, "#[serde(untagged)]").unwrap();
        writeln!(out, "pub enum {name} {{").unwrap();
        for alt in alts {
            let r = s(alt, "$ref").unwrap_or_else(|| panic!("{name}: oneOf entries must be $refs"));
            let variant = s(alt, "title").unwrap_or_else(|| Self::ref_name(&r));
            writeln!(out, "    {variant}({}),", self.ty(alt)).unwrap();
        }
        writeln!(out, "}}\n").unwrap();
        let first =
            s(&alts[0], "title").unwrap_or_else(|| Self::ref_name(&s(&alts[0], "$ref").unwrap()));
        emit_union_default(out, name, &first);
    }

    fn emit_dispatch(&self, out: &mut String) {
        for m in &self.messages {
            for n in [&m.request, &m.response] {
                self.def(n);
            }
        }
        // Request -> response pairing.
        for m in &self.messages {
            writeln!(
                out,
                "impl Call for {req} {{\n    const KIND: &'static str = \"{k}\";\n    const RESPONSE_KIND: &'static str = \"{rk}\";\n    type Response = {resp};\n}}\n",
                req = m.request, k = m.kind, rk = m.response_kind, resp = m.response
            )
            .unwrap();
        }
        let reqs: Vec<(String, &str, &str)> = self
            .messages
            .iter()
            .map(|m| (variant_name(&m.kind), m.kind.as_str(), m.request.as_str()))
            .collect();
        let resps: Vec<(String, &str, &str)> = self
            .messages
            .iter()
            .map(|m| {
                (
                    variant_name(&m.kind),
                    m.response_kind.as_str(),
                    m.response.as_str(),
                )
            })
            .collect();
        let evs: Vec<(String, &str, &str)> = self
            .events
            .iter()
            .map(|e| (variant_name(&e.kind), e.kind.as_str(), e.body.as_str()))
            .collect();
        let pushes: Vec<(String, &str, &str)> = self
            .pushes
            .iter()
            .map(|e| (variant_name(&e.kind), e.kind.as_str(), e.body.as_str()))
            .collect();
        emit_kind_enum(
            out,
            "Request",
            "A client-to-daemon request body, by kind.",
            &reqs,
        );
        emit_kind_enum(
            out,
            "Response",
            "A successful response body, by kind.",
            &resps,
        );
        emit_kind_enum(
            out,
            "Event",
            "An event body (envelope id 0), by kind.",
            &evs,
        );
        emit_kind_enum(
            out,
            "Push",
            "A non-event daemon push (envelope id 0), by kind.",
            &pushes,
        );
        let feats: Vec<&str> = self
            .messages
            .iter()
            .map(|m| m.kind.split('.').next().unwrap())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        writeln!(
            out,
            "/// Message families; a request's feature is its kind's prefix."
        )
        .unwrap();
        writeln!(out, "pub const FEATURES: &[&str] = &{feats:?};\n").unwrap();
    }
}

fn emit_kind_enum(out: &mut String, name: &str, docs: &str, items: &[(String, &str, &str)]) {
    writeln!(out, "/// {docs}").unwrap();
    writeln!(
        out,
        "#[derive(Debug, Clone, PartialEq)]\npub enum {name} {{"
    )
    .unwrap();
    for (v, _, body) in items {
        writeln!(out, "    {v}({body}),").unwrap();
    }
    writeln!(out, "}}\n").unwrap();
    writeln!(out, "impl {name} {{").unwrap();
    writeln!(out, "    /// Every kind of this message class.").unwrap();
    let kinds: Vec<&str> = items.iter().map(|(_, k, _)| *k).collect();
    writeln!(
        out,
        "    pub const KINDS: &'static [&'static str] = &{kinds:?};\n"
    )
    .unwrap();
    writeln!(
        out,
        "    pub fn kind(&self) -> &'static str {{\n        match self {{"
    )
    .unwrap();
    for (v, k, _) in items {
        writeln!(out, "            Self::{v}(_) => \"{k}\",").unwrap();
    }
    writeln!(out, "        }}\n    }}\n").unwrap();
    writeln!(out, "    /// Decodes a body of the given kind.").unwrap();
    writeln!(out, "    pub fn decode(kind: &str, body: rmpv::Value) -> Result<Self, DecodeError> {{\n        match kind {{").unwrap();
    for (v, k, _) in items {
        writeln!(
            out,
            "            \"{k}\" => Ok(Self::{v}(from_body(body)?)),"
        )
        .unwrap();
    }
    writeln!(
        out,
        "            _ => Err(DecodeError::UnknownKind(kind.to_owned())),\n        }}\n    }}\n"
    )
    .unwrap();
    writeln!(out, "    /// Encodes as one framed envelope.").unwrap();
    writeln!(out, "    pub fn encode(&self, id: u64) -> Result<Vec<u8>, EncodeError> {{\n        match self {{").unwrap();
    for (v, k, _) in items {
        writeln!(
            out,
            "            Self::{v}(b) => encode_frame(id, \"{k}\", b),"
        )
        .unwrap();
    }
    writeln!(out, "        }}\n    }}\n}}\n").unwrap();
    // `From` only when each body type maps to one variant.
    let bodies: BTreeSet<&str> = items.iter().map(|(_, _, b)| *b).collect();
    for (v, _, body) in items.iter().filter(|_| bodies.len() == items.len()) {
        writeln!(out, "impl From<{body}> for {name} {{\n    fn from(b: {body}) -> Self {{\n        Self::{v}(b)\n    }}\n}}\n").unwrap();
    }
}

/// A union defaults to its first variant.
fn emit_union_default(out: &mut String, name: &str, first: &str) {
    writeln!(out, "impl Default for {name} {{\n    fn default() -> Self {{\n        Self::{first}(Default::default())\n    }}\n}}\n").unwrap();
}

fn is_primitive(ty: &str) -> bool {
    matches!(ty, "u8" | "u16" | "u32" | "u64" | "i32" | "i64" | "bool")
}

fn emit_const(out: &mut String, name: &str, value: &str) {
    writeln!(
        out,
        r#"#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct {name};

impl serde::Serialize for {name} {{
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {{
        s.serialize_str("{value}")
    }}
}}

impl<'de> serde::Deserialize<'de> for {name} {{
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {{
        let v = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        if v == "{value}" {{
            Ok({name})
        }} else {{
            Err(serde::de::Error::custom(format!("expected \"{value}\", got {{v:?}}")))
        }}
    }}
}}
"#
    )
    .unwrap();
}

fn emit_enum(out: &mut String, name: &str, vals: &[Value]) {
    writeln!(out, "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]").unwrap();
    writeln!(out, "pub enum {name} {{").unwrap();
    for (i, v) in vals.iter().enumerate() {
        let v = v
            .as_str()
            .unwrap_or_else(|| panic!("{name}: enum values must be strings"));
        if i == 0 {
            writeln!(out, "    #[default]").unwrap();
        }
        writeln!(
            out,
            "    #[serde(rename = \"{v}\")]\n    {},",
            variant_name(v)
        )
        .unwrap();
    }
    writeln!(out, "}}\n").unwrap();
    writeln!(
        out,
        "impl {name} {{\n    pub fn as_str(&self) -> &'static str {{\n        match self {{"
    )
    .unwrap();
    for v in vals {
        let v = v.as_str().unwrap();
        writeln!(out, "            Self::{} => \"{v}\",", variant_name(v)).unwrap();
    }
    writeln!(out, "        }}\n    }}\n}}\n").unwrap();
}

fn doc(out: &mut String, schema: &Value, indent: &str) {
    if let Some(d) = s(schema, "description") {
        for line in d.lines() {
            writeln!(out, "{indent}/// {line}").unwrap();
        }
    }
}

/// `session.fg_changed` -> `SessionFgChanged`; `256` -> `V256`.
fn variant_name(s: &str) -> String {
    let mut out = String::new();
    for part in s
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
    {
        let mut cs = part.chars();
        out.extend(cs.next().unwrap().to_uppercase());
        out.push_str(&cs.as_str().to_lowercase());
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'V');
    }
    out
}

fn rust_ident(s: &str) -> String {
    const KW: &[&str] = &[
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
        "while", "async", "await", "dyn", "gen", "box", "try", "yield",
    ];
    if KW.contains(&s) {
        format!("r#{s}")
    } else {
        s.to_owned()
    }
}
