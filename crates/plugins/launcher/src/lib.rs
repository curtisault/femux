//! `launcher` plugin (0.3): `femux launch <name|file>` creates a session from a session file
//! (REQ-CFG-001/002), or from flags: `femux launch writing --tabs 4 --dir ~/writing -- nvim
//! 'git status'`, which `--save` writes as `sessions/writing.toml`. An existing session is
//! attached to rather than created again.

use std::path::{Path, PathBuf};

use femux_config::{SessionFile, TabSpec};
use femux_plugin::{CliArg, CliCommand, CliCore, CliFlag, CliInput, CliOutput, CliPlugin, Plugin};
use femux_proto::Response;
use femux_proto::msg as m;
use serde_json::json;

pub fn plugin() -> Plugin {
    Plugin {
        name: "launcher",
        client: None,
        daemon: None,
        cli: Some(Box::new(Launcher)),
    }
}

/// What to launch: the session description and the file it came from, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub name: String,
    pub spec: SessionFile,
    pub file: Option<PathBuf>,
    /// Where `--save` writes the flag form.
    pub save: Option<PathBuf>,
}

/// Builds the plan from what was typed. With any of `--tabs`, `--dir` or `-- commands` it is
/// the flag form named `arg`; otherwise `arg` names a session file (REQ-CFG-002).
pub fn plan(input: &CliInput) -> Result<Plan, String> {
    let arg = input
        .args
        .first()
        .ok_or("launch needs a session name or file")?
        .clone();
    let flag_form =
        input.value("tabs").is_some() || input.value("dir").is_some() || !input.trailing.is_empty();
    if flag_form {
        let tabs = input
            .value("tabs")
            .map(|t| {
                t.parse::<u32>()
                    .map_err(|_| format!("--tabs {t:?} is not a number"))
            })
            .transpose()?;
        let spec = SessionFile {
            name: Some(arg.clone()),
            root: input.value("dir").map(str::to_owned),
            tabs,
            tab: input
                .trailing
                .iter()
                .map(|c| TabSpec {
                    command: Some(c.clone()),
                    ..Default::default()
                })
                .collect(),
        };
        let save = input
            .flag("save")
            .then(|| femux_config::sessions_dir().join(format!("{arg}.toml")));
        return Ok(Plan {
            name: arg,
            spec,
            file: None,
            save,
        });
    }
    if input.flag("save") {
        return Err("--save writes the flag form; give --tabs, --dir or -- commands".into());
    }
    let path = femux_config::session_file_path(&arg);
    let spec = SessionFile::load(&path)?;
    let name = spec.name.clone().unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(arg)
    });
    Ok(Plan {
        name,
        spec,
        file: Some(path),
        save: None,
    })
}

fn call(core: &mut dyn CliCore, req: impl Into<femux_proto::Request>) -> Result<Response, String> {
    core.request(req.into())
        .map_err(|e| format!("{}: {}", e.code.as_str(), e.message))
}

fn expand(p: &str) -> String {
    femux_config::expand(p).to_string_lossy().into_owned()
}

/// Creates the session of `plan` unless it exists; returns whether it was created and its
/// tab count.
pub fn launch(plan: &Plan, core: &mut dyn CliCore) -> Result<(bool, usize), String> {
    let Response::SessionList(list) = call(core, m::SessionList {})? else {
        return Err("unexpected answer to session.list".into());
    };
    if let Some(s) = list.sessions.iter().find(|s| s.name == plan.name) {
        return Ok((false, s.tabs as usize));
    }
    let root = plan.spec.root.as_deref().map(expand);
    call(
        core,
        m::SessionCreate {
            name: plan.name.clone(),
            root: root.clone(),
            file: plan.file.as_ref().map(|f| f.to_string_lossy().into_owned()),
        },
    )?;
    // session.create opens one plain tab; the file's tabs replace it.
    let Response::SessionTabs(first) = call(
        core,
        m::SessionTabs {
            name: plan.name.clone(),
        },
    )?
    else {
        return Err("unexpected answer to session.tabs".into());
    };
    let tabs = plan.spec.all_tabs();
    for t in &tabs {
        call(
            core,
            m::TabCreate {
                session: plan.name.clone(),
                cwd: t.cwd.as_deref().map(expand),
                cmd: t.command.clone(),
                title: t.title.clone(),
                index: None,
                restore: t.restore.then_some(true),
            },
        )?;
    }
    if let Some(t) = first.tabs.first() {
        call(
            core,
            m::TabClose {
                tab_id: t.id.clone(),
            },
        )?;
    }
    Ok((true, tabs.len()))
}

struct Launcher;

impl CliPlugin for Launcher {
    fn name(&self) -> &'static str {
        "launcher"
    }

    fn commands(&self) -> Vec<CliCommand> {
        vec![CliCommand {
            name: "launch",
            about: "Create a session from a session file, or from --tabs/--dir/-- commands",
            args: vec![CliArg {
                name: "session",
                help: "Session name (sessions/<name>.toml) or path to a session file",
                required: true,
            }],
            flags: vec![
                CliFlag {
                    name: "tabs",
                    help: "Total tabs; those without a command are shells in --dir",
                    value: true,
                },
                CliFlag {
                    name: "dir",
                    help: "Root directory of the session",
                    value: true,
                },
                CliFlag {
                    name: "save",
                    help: "Write the flag form to sessions/<name>.toml",
                    value: false,
                },
                CliFlag {
                    name: "detach",
                    help: "Do not attach afterwards",
                    value: false,
                },
            ],
            trailing: Some("commands"),
        }]
    }

    fn run(
        &self,
        _cmd: &str,
        input: &CliInput,
        core: &mut dyn CliCore,
    ) -> Result<CliOutput, String> {
        let plan = plan(input)?;
        if let Some(path) = &plan.save {
            save(path, &plan.spec)?;
        }
        core.start_daemon()?;
        let (created, tabs) = launch(&plan, core)?;
        Ok(CliOutput {
            text: plan.name.clone(),
            json: json!({
                "name": plan.name,
                "created": created,
                "tabs": tabs,
                "file": plan.file.as_ref().or(plan.save.as_ref()),
            }),
            attach: (!input.flag("detach")).then(|| plan.name.clone()),
        })
    }
}

fn save(path: &Path, spec: &SessionFile) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(path, spec.to_toml()).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(args: &[&str], flags: &[(&str, Option<&str>)], trailing: &[&str]) -> CliInput {
        CliInput {
            args: args.iter().map(|s| s.to_string()).collect(),
            flags: flags
                .iter()
                .map(|(k, v)| (k.to_string(), v.map(str::to_owned)))
                .collect(),
            trailing: trailing.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn cfg_002_flags_build_the_same_session_as_a_file() {
        let p = plan(&input(
            &["writing"],
            &[("tabs", Some("4")), ("dir", Some("~/writing"))],
            &["nvim", "git status"],
        ))
        .unwrap();
        assert_eq!(p.name, "writing");
        assert!(p.file.is_none() && p.save.is_none());
        let from_file = SessionFile::parse(
            "name = \"writing\"\nroot = \"~/writing\"\ntabs = 4\n[[tab]]\ncommand = \"nvim\"\n[[tab]]\ncommand = \"git status\"\n",
        )
        .unwrap();
        assert_eq!(p.spec, from_file);
        // CFG-001: the tabs beyond the listed ones are plain shells.
        let tabs = p.spec.all_tabs();
        assert_eq!(tabs.len(), 4);
        assert!(tabs[2].command.is_none() && tabs[3].command.is_none());
        assert!(plan(&input(&["x"], &[("tabs", Some("many"))], &[])).is_err());
        assert!(plan(&input(&["x"], &[("save", None)], &[])).is_err());
        assert!(plan(&input(&[], &[], &[])).is_err());
    }

    #[test]
    fn a_file_names_the_session_or_its_stem_does() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("proj.toml");
        std::fs::write(&f, "root = \"/tmp\"\n").unwrap();
        let p = plan(&input(&[f.to_str().unwrap()], &[], &[])).unwrap();
        assert_eq!(p.name, "proj");
        assert_eq!(p.file.as_deref(), Some(f.as_path()));
        std::fs::write(&f, "name = \"other\"\n").unwrap();
        assert_eq!(
            plan(&input(&[f.to_str().unwrap()], &[], &[])).unwrap().name,
            "other"
        );
        std::fs::write(&f, "name = \"x\"\nwindows = 2\n").unwrap();
        let e = plan(&input(&[f.to_str().unwrap()], &[], &[])).unwrap_err();
        assert!(e.contains("line 2") && e.contains("windows"), "{e}");
    }
}
