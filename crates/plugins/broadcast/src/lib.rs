//! `broadcast` plugin (0.8): typing into every pane of a tab (ADR-022). The daemon does the
//! sending (a tab's `broadcast` flag); this binds `prefix B` to the client's `broadcast`
//! action and adds `femux broadcast on|off`.

use femux_plugin::{
    Binding, CliArg, CliCommand, CliCore, CliInput, CliOutput, CliPlugin, ClientPlugin, Plugin,
    Table,
};
use femux_proto::Response;
use femux_proto::msg as m;
use serde_json::json;

pub fn plugin() -> Plugin {
    Plugin {
        name: "broadcast",
        client: Some(Box::new(Client)),
        daemon: None,
        cli: Some(Box::new(Cli)),
    }
}

/// The client side alone: the `prefix B` binding.
pub fn client() -> Box<dyn ClientPlugin> {
    Box::new(Client)
}

struct Client;

impl ClientPlugin for Client {
    fn name(&self) -> &'static str {
        "broadcast"
    }
    fn bindings(&self) -> Vec<Binding> {
        vec![Binding {
            table: Table::Prefix,
            key: "B".into(),
            action: "broadcast".into(),
        }]
    }
}

struct Cli;

impl CliPlugin for Cli {
    fn name(&self) -> &'static str {
        "broadcast"
    }

    fn commands(&self) -> Vec<CliCommand> {
        let tab = || CliArg {
            name: "tab",
            help: "Tab id (default: this pane's tab)",
            required: false,
        };
        vec![
            CliCommand {
                name: "broadcast on",
                about: "Type into every pane of a tab",
                args: vec![tab()],
                ..Default::default()
            },
            CliCommand {
                name: "broadcast off",
                about: "Type into the focused pane only",
                args: vec![tab()],
                ..Default::default()
            },
        ]
    }

    fn run(
        &self,
        cmd: &str,
        input: &CliInput,
        core: &mut dyn CliCore,
    ) -> Result<CliOutput, String> {
        let on = cmd == "broadcast on";
        let tab = match input.args.first() {
            Some(t) => t.clone(),
            None => {
                let pane = core.pane().ok_or("no tab: run inside femux or name one")?;
                match core
                    .request(m::PaneInfo { pane_id: pane }.into())
                    .map_err(|e| e.message)?
                {
                    Response::PaneInfo(i) => i.tab,
                    other => return Err(format!("unexpected answer {other:?}")),
                }
            }
        };
        core.request(
            m::TabSet {
                tab_id: tab.clone(),
                broadcast: Some(on),
                floats_hidden: None,
            }
            .into(),
        )
        .map_err(|e| e.message)?;
        Ok(CliOutput {
            text: String::new(),
            json: json!({ "tab_id": tab, "broadcast": on }),
            attach: None,
        })
    }
}
