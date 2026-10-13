//! `graphics` plugin (0.7): `femux graphics list` shows the images each pane holds (REQ-GFX-001,
//! REQ-GFX-004). Images themselves live in the daemon and are drawn by the client.

use femux_plugin::{CliArg, CliCommand, CliCore, CliInput, CliOutput, CliPlugin, Plugin};
use femux_proto::Response;
use femux_proto::msg as m;
use serde_json::json;

pub fn plugin() -> Plugin {
    Plugin {
        name: "graphics",
        client: None,
        daemon: None,
        cli: Some(Box::new(Cli)),
    }
}

/// 1536 -> "1.5k".
pub fn human(bytes: u64) -> String {
    let units = ["B", "k", "M", "G"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < units.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes}B")
    } else {
        format!("{v:.1}{}", units[i])
    }
}

struct Cli;

impl CliPlugin for Cli {
    fn name(&self) -> &'static str {
        "graphics"
    }

    fn commands(&self) -> Vec<CliCommand> {
        vec![CliCommand {
            name: "graphics list",
            about: "List the images panes hold",
            args: vec![CliArg {
                name: "pane",
                help: "Only this pane",
                required: false,
            }],
            ..Default::default()
        }]
    }

    fn run(
        &self,
        _cmd: &str,
        input: &CliInput,
        core: &mut dyn CliCore,
    ) -> Result<CliOutput, String> {
        let images = match core
            .request(
                m::ImageList {
                    pane_id: input.args.first().cloned(),
                }
                .into(),
            )
            .map_err(|e| e.message)?
        {
            Response::ImageList(l) => l.images,
            other => return Err(format!("unexpected answer {other:?}")),
        };
        let mut text = Vec::new();
        let mut panes: Vec<&str> = images.iter().map(|i| i.pane_id.as_str()).collect();
        panes.dedup();
        for pane in panes {
            let of: Vec<&m::ImageInfo> = images.iter().filter(|i| i.pane_id == pane).collect();
            let total: u64 = of.iter().map(|i| i.bytes).sum();
            text.push(format!("{pane}: {} images, {}", of.len(), human(total)));
            for i in of {
                text.push(format!(
                    "  {:>10}  {}x{} {}  {}  {} placement{}{}",
                    i.image_id,
                    i.width,
                    i.height,
                    i.format.as_str(),
                    human(i.bytes),
                    i.placements,
                    if i.placements == 1 { "" } else { "s" },
                    if i.visible { ", visible" } else { "" }
                ));
            }
        }
        Ok(CliOutput {
            text: text.join("\n"),
            json: json!({ "images": images }),
            attach: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_well() {
        assert_eq!(human(12), "12B");
        assert_eq!(human(1536), "1.5k");
        assert_eq!(human(64 * 1024 * 1024), "64.0M");
    }
}
