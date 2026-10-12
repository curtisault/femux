//! Layout templates (REQ-UI-012): `femux layout save|load|list|export`. A template is one
//! JSON file per name under the state directory holding `layout.get` output plus cwd and cmd
//! per pane; `load` rebuilds it in a new tab by splitting.

use std::path::PathBuf;

use femux_proto::msg as m;
use serde_json::{Value, json};

use crate::conn::Conn;

pub fn dir() -> PathBuf {
    crate::config::state_dir().join("layouts")
}

fn path(name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(format!("bad layout name {name:?}"));
    }
    Ok(dir().join(format!("{name}.json")))
}

fn node_json(n: &m::LayoutNode) -> Value {
    match n {
        m::LayoutNode::PaneNode(p) => json!({"t": "pane", "id": p.id}),
        m::LayoutNode::SplitNode(s) => json!({
            "t": "split",
            "dir": match s.dir { m::SplitDir::H => "h", m::SplitDir::V => "v" },
            "ratio": {"num": s.ratio.num, "den": s.ratio.den},
            "children": s.children.iter().map(node_json).collect::<Vec<_>>(),
        }),
    }
}

fn node_from(v: &Value) -> Result<m::LayoutNode, String> {
    match v.get("t").and_then(Value::as_str) {
        Some("pane") => Ok(m::LayoutNode::PaneNode(m::PaneNode {
            id: v["id"].as_str().unwrap_or_default().to_owned(),
        })),
        Some("split") => {
            let children = v["children"]
                .as_array()
                .ok_or("split without children")?
                .iter()
                .map(node_from)
                .collect::<Result<Vec<_>, _>>()?;
            if children.len() != 2 {
                return Err("a split needs two children".into());
            }
            Ok(m::LayoutNode::SplitNode(m::SplitNode {
                dir: match v["dir"].as_str() {
                    Some("h") => m::SplitDir::H,
                    _ => m::SplitDir::V,
                },
                ratio: m::Ratio {
                    num: v["ratio"]["num"].as_u64().unwrap_or(1) as u32,
                    den: v["ratio"]["den"].as_u64().unwrap_or(2) as u32,
                },
                children,
            }))
        }
        _ => Err("node without t".into()),
    }
}

fn panes_of(n: &m::LayoutNode, out: &mut Vec<String>) {
    match n {
        m::LayoutNode::PaneNode(p) => out.push(p.id.clone()),
        m::LayoutNode::SplitNode(s) => s.children.iter().for_each(|c| panes_of(c, out)),
    }
}

/// A pane's saved cwd and cmd.
type Spec = (Option<String>, Option<String>);

/// Serializes `tab`: the tree, floats, and cwd/cmd per pane.
pub fn snapshot(conn: &mut Conn, tab: &str) -> Result<Value, String> {
    let got = conn
        .call(m::LayoutGet {
            tab_id: tab.to_owned(),
        })
        .map_err(|e| e.message)?;
    let mut ids = Vec::new();
    panes_of(&got.tree, &mut ids);
    ids.extend(got.floats.iter().map(|f| f.id.clone()));
    let mut panes = serde_json::Map::new();
    for id in ids {
        let info = conn
            .call(m::PaneInfo {
                pane_id: id.clone(),
            })
            .map_err(|e| e.message)?;
        panes.insert(id, json!({"cwd": info.cwd, "cmd": info.cmd}));
    }
    Ok(json!({
        "version": 1,
        "tree": node_json(&got.tree),
        "floats": got.floats.iter().map(|f| json!({"id": f.id, "rect": {"x": f.rect.x, "y": f.rect.y, "w": f.rect.w, "h": f.rect.h}})).collect::<Vec<_>>(),
        "panes": panes,
    }))
}

pub fn save(conn: &mut Conn, name: &str, tab: &str) -> Result<PathBuf, String> {
    let p = path(name)?;
    let v = snapshot(conn, tab)?;
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).map_err(|e| e.to_string())?;
    Ok(p)
}

pub fn read(name: &str) -> Result<Value, String> {
    let p = path(name)?;
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn list() -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_suffix(".json").map(str::to_owned)
        })
        .collect();
    out.sort();
    out
}

/// Rebuilds `template` in a new tab of `session`; returns the tab id.
pub fn load(conn: &mut Conn, session: &str, template: &Value) -> Result<String, String> {
    let tree = node_from(&template["tree"])?;
    let panes = &template["panes"];
    let spec = |id: &str| -> Spec {
        let p = &panes[id];
        (
            p["cwd"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map(str::to_owned),
            p["cmd"].as_str().map(str::to_owned),
        )
    };
    let mut order = Vec::new();
    panes_of(&tree, &mut order);
    let first = order.first().ok_or("template has no panes")?.clone();
    let (cwd, cmd) = spec(&first);
    let created = conn
        .call(m::TabCreate {
            session: session.to_owned(),
            cwd,
            cmd,
            title: None,
            index: None,
        })
        .map_err(|e| e.message)?;
    build(conn, &tree, &created.pane_id, &spec)?;
    // Floats: split the first pane, then float the new one at the saved rect.
    for f in template["floats"].as_array().into_iter().flatten() {
        let id = f["id"].as_str().unwrap_or_default();
        let (cwd, cmd) = spec(id);
        let new = conn
            .call(m::LayoutSplit {
                pane_id: created.pane_id.clone(),
                dir: m::SplitDir::H,
                ratio: None,
                cwd,
                cmd,
                preselect: None,
            })
            .map_err(|e| e.message)?
            .pane_id;
        let r = &f["rect"];
        conn.call(m::LayoutFloat {
            pane_id: new,
            rect: Some(m::Rect {
                x: r["x"].as_u64().unwrap_or(0) as u16,
                y: r["y"].as_u64().unwrap_or(0) as u16,
                w: r["w"].as_u64().unwrap_or(40) as u16,
                h: r["h"].as_u64().unwrap_or(10) as u16,
            }),
        })
        .map_err(|e| e.message)?;
    }
    Ok(created.tab_id)
}

/// Recursively splits `at` so that it takes the shape of `node`.
fn build(
    conn: &mut Conn,
    node: &m::LayoutNode,
    at: &str,
    spec: &dyn Fn(&str) -> Spec,
) -> Result<(), String> {
    let m::LayoutNode::SplitNode(s) = node else {
        return Ok(());
    };
    let mut second = Vec::new();
    panes_of(&s.children[1], &mut second);
    let (cwd, cmd) = spec(second.first().map(String::as_str).unwrap_or_default());
    let new = conn
        .call(m::LayoutSplit {
            pane_id: at.to_owned(),
            dir: s.dir,
            ratio: Some(s.ratio.clone()),
            cwd,
            cmd,
            preselect: None,
        })
        .map_err(|e| e.message)?
        .pane_id;
    build(conn, &s.children[0], at, spec)?;
    build(conn, &s.children[1], &new, spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_roundtrip_through_json() {
        let tree = m::LayoutNode::SplitNode(m::SplitNode {
            dir: m::SplitDir::H,
            ratio: m::Ratio { num: 2, den: 3 },
            children: vec![
                m::LayoutNode::PaneNode(m::PaneNode { id: "a".into() }),
                m::LayoutNode::SplitNode(m::SplitNode {
                    dir: m::SplitDir::V,
                    ratio: m::Ratio { num: 1, den: 2 },
                    children: vec![
                        m::LayoutNode::PaneNode(m::PaneNode { id: "b".into() }),
                        m::LayoutNode::PaneNode(m::PaneNode { id: "c".into() }),
                    ],
                }),
            ],
        });
        assert_eq!(node_from(&node_json(&tree)).unwrap(), tree);
        assert!(path("../x").is_err());
    }
}
