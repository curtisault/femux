//! `hosts` plugin (0.6): remote hosts for `femux attach --host` (SPEC §11, REQ-PROTO-003).
//!
//! - `femux hosts add|list|rm` keep `hosts.toml` next to `config.toml`.
//! - Daemon side: samples the daemon's machine (hostname, cpu, memory, network) every 2 s and
//!   publishes it with `plugin.publish` (ADR-020).
//! - Client side: the `remote:cpu`, `remote:mem`, `remote:net` and `remote:hostname` info bar
//!   modules render those values (REQ-UI-032), so an SSH session shows both machines.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use femux_config::{HostEntry, HostsFile, hosts_path};
use femux_plugin::{
    BarCtx, CliArg, CliCommand, CliCore, CliFlag, CliInput, CliOutput, CliPlugin, ClientPlugin,
    DaemonPlugin, InfoModule, Plugin, Span, Spans, Style,
};
use femux_proto::msg as m;
use femux_proto::{Event, Request};
use serde_json::json;

pub fn plugin() -> Plugin {
    Plugin {
        name: "hosts",
        client: Some(Box::new(Client)),
        daemon: Some(Box::new(Sampler::new(Duration::from_secs(2)))),
        cli: Some(Box::new(Cli)),
    }
}

/// The published keys.
pub const KEYS: &[&str] = &[
    "hostname",
    "cpu",
    "mem_used",
    "mem_total",
    "net_rx",
    "net_tx",
];

/// 1536 -> "1.5k" (as the infobar plugin prints sizes).
pub fn human(bytes: f64) -> String {
    let units = ["", "k", "M", "G", "T"];
    let mut v = bytes;
    let mut i = 0;
    while v >= 1000.0 && i + 1 < units.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 || v >= 100.0 {
        format!("{v:.0}{}", units[i])
    } else {
        format!("{v:.1}{}", units[i])
    }
}

fn hostname() -> String {
    sysinfo::System::host_name()
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_owned()
}

// ---------------------------------------------------------------------------- daemon

/// Samples the daemon's host and publishes it.
pub struct Sampler {
    every: Duration,
    sys: sysinfo::System,
    nets: sysinfo::Networks,
    last: Option<Instant>,
}

impl Sampler {
    pub fn new(every: Duration) -> Sampler {
        Sampler {
            every,
            sys: sysinfo::System::new(),
            nets: sysinfo::Networks::new_with_refreshed_list(),
            last: None,
        }
    }

    /// One sample as published (`cpu` in percent, sizes in bytes, rates in bytes/s).
    pub fn sample(&mut self) -> BTreeMap<String, String> {
        let now = Instant::now();
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.nets.refresh(true);
        let (rx, tx) = self
            .nets
            .iter()
            .filter(|(name, _)| !name.starts_with("lo"))
            .fold((0u64, 0u64), |(r, t), (_, d)| {
                (r + d.received(), t + d.transmitted())
            });
        let secs = self
            .last
            .map_or(1.0, |l| now.duration_since(l).as_secs_f64().max(0.001));
        self.last = Some(now);
        [
            ("hostname", hostname()),
            ("cpu", format!("{:.1}", self.sys.global_cpu_usage())),
            ("mem_used", self.sys.used_memory().to_string()),
            ("mem_total", self.sys.total_memory().to_string()),
            ("net_rx", format!("{:.0}", rx as f64 / secs)),
            ("net_tx", format!("{:.0}", tx as f64 / secs)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    }

    fn publish(&mut self) -> Vec<Request> {
        vec![
            m::PluginPublish {
                plugin: "hosts".into(),
                data: self.sample(),
            }
            .into(),
        ]
    }
}

impl DaemonPlugin for Sampler {
    fn name(&self) -> &'static str {
        "hosts"
    }
    fn subscription(&self) -> m::EventsSubscribe {
        // Nothing to react to; ticks drive it.
        m::EventsSubscribe {
            kinds: Some(Vec::new()),
            ..Default::default()
        }
    }
    fn start(&mut self) -> Vec<Request> {
        self.publish()
    }
    fn on_event(&mut self, _ev: &Event) -> Vec<Request> {
        Vec::new()
    }
    fn tick_every(&self) -> Option<Duration> {
        Some(self.every)
    }
    fn tick(&mut self, _now_ms: u64) -> Vec<Request> {
        self.publish()
    }
}

// ---------------------------------------------------------------------------- client

struct Client;

impl ClientPlugin for Client {
    fn name(&self) -> &'static str {
        "hosts"
    }
    fn modules(&self) -> Vec<Box<dyn InfoModule>> {
        ["cpu", "mem", "net", "hostname"]
            .into_iter()
            .map(|k| Box::new(RemoteModule(format!("remote:{k}"))) as Box<dyn InfoModule>)
            .collect()
    }
}

/// `remote:<module>`: the daemon host's value from [`BarCtx::remote`].
pub struct RemoteModule(pub String);

impl InfoModule for RemoteModule {
    fn name(&self) -> &str {
        &self.0
    }
    fn render(&self, ctx: &BarCtx, _w: usize) -> Spans {
        render(self.0.trim_start_matches("remote:"), ctx)
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(1)
    }
}

/// What `remote:<what>` shows, labelled with the daemon's hostname.
pub fn render(what: &str, ctx: &BarCtx) -> Spans {
    let r = &ctx.remote;
    let num = |k: &str| r.get(k).and_then(|v| v.parse::<f64>().ok());
    let host = r.get("hostname").cloned().unwrap_or_default();
    let label = |name: &str| {
        (!ctx.vertical).then(|| {
            Span::new(
                if host.is_empty() {
                    format!("{name} ")
                } else {
                    format!("{host}:{name} ")
                },
                Style::Muted,
            )
        })
    };
    let value = match what {
        "hostname" => {
            return if host.is_empty() {
                Vec::new()
            } else {
                vec![Span::text(host)]
            };
        }
        "cpu" => num("cpu").map(|c| format!("{c:.0}%")),
        "mem" => match (num("mem_used"), num("mem_total")) {
            (Some(u), Some(t)) if t > 0.0 && ctx.vertical => {
                Some(format!("{} / {}", human(u), human(t)))
            }
            (Some(u), Some(t)) if t > 0.0 => Some(human(u)),
            _ => None,
        },
        "net" => match (num("net_rx"), num("net_tx")) {
            (Some(d), Some(u)) => {
                let sep = if ctx.vertical { "\n" } else { " " };
                Some(format!("↓{}{sep}↑{}", human(d), human(u)))
            }
            _ => None,
        },
        _ => None,
    };
    let Some(v) = value else {
        return Vec::new();
    };
    let mut out: Spans = label(what).into_iter().collect();
    out.push(Span::text(v));
    out
}

// ---------------------------------------------------------------------------- cli

struct Cli;

fn entry_json(name: &str, e: &HostEntry) -> serde_json::Value {
    json!({
        "name": name,
        "ssh": e.ssh,
        "femux": e.femux,
        "forward": e.forward,
    })
}

impl CliPlugin for Cli {
    fn name(&self) -> &'static str {
        "hosts"
    }

    fn commands(&self) -> Vec<CliCommand> {
        vec![
            CliCommand {
                name: "hosts add",
                about: "Save a remote for `femux attach --host <name>`",
                args: vec![
                    CliArg {
                        name: "name",
                        help: "What to call it",
                        required: true,
                    },
                    CliArg {
                        name: "ssh",
                        help: "The ssh destination: host, user@host or an ~/.ssh/config alias",
                        required: true,
                    },
                ],
                flags: vec![
                    CliFlag {
                        name: "femux",
                        help: "The femux command on the remote (default: femux)",
                        value: true,
                    },
                    CliFlag {
                        name: "forward",
                        help: "Forward the remote daemon's socket (ssh -L) instead of --stdio",
                        value: false,
                    },
                ],
                ..Default::default()
            },
            CliCommand {
                name: "hosts list",
                about: "List saved remotes",
                ..Default::default()
            },
            CliCommand {
                name: "hosts rm",
                about: "Forget a saved remote",
                args: vec![CliArg {
                    name: "name",
                    help: "The remote's name",
                    required: true,
                }],
                ..Default::default()
            },
        ]
    }

    fn run(
        &self,
        cmd: &str,
        input: &CliInput,
        _core: &mut dyn CliCore,
    ) -> Result<CliOutput, String> {
        let path = hosts_path();
        let mut hosts = HostsFile::load(&path)?;
        match cmd {
            "hosts add" => {
                let name = input.args[0].clone();
                if name.is_empty() || name.contains(char::is_whitespace) {
                    return Err(format!("{name:?} is not a host name"));
                }
                let e = HostEntry {
                    ssh: input.args[1].clone(),
                    femux: input.value("femux").map(str::to_owned),
                    forward: input.flag("forward"),
                };
                let j = entry_json(&name, &e);
                hosts.host.insert(name, e);
                hosts.save(&path)?;
                Ok(CliOutput {
                    text: String::new(),
                    json: j,
                    attach: None,
                })
            }
            "hosts list" => {
                let rows: Vec<_> = hosts.host.iter().map(|(n, e)| entry_json(n, e)).collect();
                let text = hosts
                    .host
                    .iter()
                    .map(|(n, e)| {
                        let mut l = format!("{n:<12} {}", e.ssh);
                        if let Some(f) = &e.femux {
                            l.push_str(&format!("  femux={f}"));
                        }
                        if e.forward {
                            l.push_str("  forward");
                        }
                        l
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(CliOutput {
                    text,
                    json: json!({ "hosts": rows }),
                    attach: None,
                })
            }
            "hosts rm" => {
                let name = &input.args[0];
                if hosts.host.remove(name).is_none() {
                    return Err(format!("no host {name:?}"));
                }
                hosts.save(&path)?;
                Ok(CliOutput {
                    text: String::new(),
                    json: json!({ "name": name }),
                    attach: None,
                })
            }
            other => Err(format!("unknown command {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(pairs: &[(&str, &str)], vertical: bool) -> BarCtx {
        BarCtx {
            remote: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            vertical,
            ..Default::default()
        }
    }

    fn text(s: &Spans) -> String {
        s.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn ui_032_remote_modules_render_the_daemon_hosts_values() {
        let c = ctx(
            &[
                ("hostname", "build-box"),
                ("cpu", "12.6"),
                ("mem_used", "1610612736"),
                ("mem_total", "17179869184"),
                ("net_rx", "1536"),
                ("net_tx", "0"),
            ],
            false,
        );
        assert_eq!(text(&render("cpu", &c)), "build-box:cpu 13%");
        assert_eq!(text(&render("mem", &c)), "build-box:mem 1.5G");
        assert_eq!(text(&render("net", &c)), "build-box:net ↓1.5k ↑0");
        assert_eq!(text(&render("hostname", &c)), "build-box");
        let v = ctx(&[("mem_used", "1024"), ("mem_total", "2048")], true);
        assert_eq!(text(&render("mem", &v)), "1.0k / 2.0k");
        // Nothing published yet: nothing shown.
        assert!(render("cpu", &BarCtx::default()).is_empty());
    }

    #[test]
    fn the_sampler_publishes_every_key() {
        let mut s = Sampler::new(Duration::from_secs(2));
        let d = s.sample();
        for k in KEYS {
            assert!(d.contains_key(*k), "{k}");
        }
        assert!(d["mem_total"].parse::<u64>().unwrap() > 0);
    }
}
