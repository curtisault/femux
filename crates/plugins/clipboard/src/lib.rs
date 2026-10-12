//! `clipboard` plugin (0.2): copies `clipboard.changed` texts to the host terminal with
//! OSC 52 (client side) and offers `femux clipboard set` (CLI side). The daemon owns no
//! clipboard (ADR-016 #4); this plugin is where text reaches the user.

use femux_plugin::{
    CliArg, CliCommand, CliCore, CliOutput, CliPlugin, ClientPlugin, HostEffect, Plugin,
};
use femux_proto::Event;
use femux_proto::msg as m;

pub fn plugin() -> Plugin {
    Plugin {
        name: "clipboard",
        client: Some(Box::new(Client)),
        daemon: None,
        cli: Some(Box::new(Cli)),
    }
}

struct Client;

impl ClientPlugin for Client {
    fn name(&self) -> &'static str {
        "clipboard"
    }
    fn on_event(&mut self, ev: &Event) -> Vec<HostEffect> {
        match ev {
            Event::ClipboardChanged(c) => vec![HostEffect::Clipboard(c.text.clone())],
            _ => vec![],
        }
    }
}

struct Cli;

impl CliPlugin for Cli {
    fn name(&self) -> &'static str {
        "clipboard"
    }
    fn commands(&self) -> Vec<CliCommand> {
        vec![CliCommand {
            name: "clipboard set",
            about: "Copy text (or stdin) to the clipboard of every attached client",
            args: vec![CliArg {
                name: "text",
                help: "Text to copy; read from stdin when omitted",
                required: false,
            }],
        }]
    }
    fn run(
        &self,
        _cmd: &str,
        args: &[String],
        core: &mut dyn CliCore,
    ) -> Result<CliOutput, String> {
        let text = match args.first() {
            Some(t) => t.clone(),
            None => core.stdin(),
        };
        core.request(m::ClipboardSet { text: text.clone() }.into())
            .map_err(|e| e.message)?;
        Ok(CliOutput {
            text: String::new(),
            json: serde_json::json!({ "bytes": text.len() }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_events_become_osc52_effects() {
        let mut p = Client;
        let ev = Event::ClipboardChanged(m::ClipboardChanged {
            text: "hi".into(),
            source: m::ClipboardSource::Pane,
            ..Default::default()
        });
        assert_eq!(p.on_event(&ev), [HostEffect::Clipboard("hi".into())]);
        assert!(
            p.on_event(&Event::PaneBell(m::PaneBell::default()))
                .is_empty()
        );
    }
}
