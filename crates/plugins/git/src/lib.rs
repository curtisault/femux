//! `git` plugin (0.8): worktree tabs. `femux git tab <branch>` gives a branch its own git
//! worktree in `<repo>/.worktrees/<branch>` and a tab there titled with the branch;
//! `femux git tabs` lists them; `femux git rm <branch>` closes the tab and removes the
//! worktree, refusing uncommitted changes unless `--force`. The info bar's `git` module
//! lives in the infobar plugin.

use std::path::{Path, PathBuf};
use std::process::Command;

use femux_plugin::{CliArg, CliCommand, CliCore, CliFlag, CliInput, CliOutput, CliPlugin, Plugin};
use femux_proto::Response;
use femux_proto::msg as m;
use serde_json::json;

pub fn plugin() -> Plugin {
    Plugin {
        name: "git",
        client: None,
        daemon: None,
        cli: Some(Box::new(Cli)),
    }
}

/// Where a branch's worktree lives.
pub fn worktree_path(root: &Path, branch: &str) -> PathBuf {
    root.join(".worktrees").join(branch)
}

/// Runs git in `dir`; stdout trimmed, or stderr as the error.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The worktrees git knows, as (path, branch).
fn worktrees(root: &Path) -> Result<Vec<(PathBuf, Option<String>)>, String> {
    let text = git(root, &["worktree", "list", "--porcelain"])?;
    let mut out = Vec::new();
    let mut path: Option<PathBuf> = None;
    for line in text.lines().chain(std::iter::once("")) {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(p) = &path {
                out.push((
                    p.clone(),
                    Some(b.trim_start_matches("refs/heads/").to_owned()),
                ));
                path = None;
            }
        } else if line.is_empty()
            && let Some(p) = path.take()
        {
            out.push((p, None));
        }
    }
    Ok(out)
}

/// `.worktrees/` stays out of `git status` (in `info/exclude`, not the user's .gitignore).
fn exclude_worktrees(root: &Path) -> Result<(), String> {
    let common = git(root, &["rev-parse", "--git-common-dir"])?;
    let common = if Path::new(&common).is_absolute() {
        PathBuf::from(common)
    } else {
        root.join(common)
    };
    let file = common.join("info").join("exclude");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if text.lines().any(|l| l.trim() == "/.worktrees/") {
        return Ok(());
    }
    std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut text = text;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("/.worktrees/\n");
    std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))
}

struct Ctx {
    /// The repository's top level.
    root: PathBuf,
    session: String,
}

fn pane_info(core: &mut dyn CliCore, pane: &str) -> Result<m::PaneInfoOk, String> {
    match core
        .request(
            m::PaneInfo {
                pane_id: pane.to_owned(),
            }
            .into(),
        )
        .map_err(|e| e.message)?
    {
        Response::PaneInfo(i) => Ok(i),
        other => Err(format!("unexpected answer {other:?}")),
    }
}

/// The repository (from this pane's directory, else the current one) and the session.
fn context(input: &CliInput, core: &mut dyn CliCore) -> Result<Ctx, String> {
    let here = core.pane().map(|p| pane_info(core, &p)).transpose()?;
    let dir = here
        .as_ref()
        .map(|i| PathBuf::from(&i.cwd))
        .filter(|d| !d.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .ok_or("no directory")?;
    let root = PathBuf::from(git(&dir, &["rev-parse", "--show-toplevel"])?);
    let session = match (input.value("session"), here) {
        (Some(s), _) => s.to_owned(),
        (None, Some(i)) => i.session,
        (None, None) => match core
            .request(m::SessionList {}.into())
            .map_err(|e| e.message)?
        {
            Response::SessionList(l) if l.sessions.len() == 1 => l.sessions[0].name.clone(),
            Response::SessionList(l) if l.sessions.is_empty() => return Err("no sessions".into()),
            _ => return Err("several sessions: pass --session".into()),
        },
    };
    Ok(Ctx { root, session })
}

/// The session's tabs as (id, custom title).
fn tabs(core: &mut dyn CliCore, session: &str) -> Result<Vec<(String, Option<String>)>, String> {
    match core
        .request(
            m::SessionTabs {
                name: session.to_owned(),
            }
            .into(),
        )
        .map_err(|e| e.message)?
    {
        Response::SessionTabs(t) => {
            Ok(t.tabs.into_iter().map(|t| (t.id, t.custom_title)).collect())
        }
        other => Err(format!("unexpected answer {other:?}")),
    }
}

struct Cli;

fn branch_arg() -> CliArg {
    CliArg {
        name: "branch",
        help: "The branch",
        required: true,
    }
}

fn session_flag() -> CliFlag {
    CliFlag {
        name: "session",
        help: "The session (default: this pane's, or the only one)",
        value: true,
    }
}

impl CliPlugin for Cli {
    fn name(&self) -> &'static str {
        "git"
    }

    fn commands(&self) -> Vec<CliCommand> {
        vec![
            CliCommand {
                name: "git tab",
                about: "Open a tab in a worktree of the branch (created when missing)",
                args: vec![branch_arg()],
                flags: vec![session_flag()],
                ..Default::default()
            },
            CliCommand {
                name: "git tabs",
                about: "List the repository's worktree tabs",
                flags: vec![session_flag()],
                ..Default::default()
            },
            CliCommand {
                name: "git rm",
                about: "Close a branch's tab and remove its worktree",
                args: vec![branch_arg()],
                flags: vec![
                    session_flag(),
                    CliFlag {
                        name: "force",
                        help: "Remove it even with uncommitted changes",
                        value: false,
                    },
                ],
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
        let ctx = context(input, core)?;
        match cmd {
            "git tab" => {
                let branch = &input.args[0];
                let path = worktree_path(&ctx.root, branch);
                let known = worktrees(&ctx.root)?.into_iter().any(|(p, _)| p == path);
                if !known {
                    exclude_worktrees(&ctx.root)?;
                    let p = path.to_string_lossy().into_owned();
                    let exists = git(
                        &ctx.root,
                        &[
                            "rev-parse",
                            "--verify",
                            "--quiet",
                            &format!("refs/heads/{branch}"),
                        ],
                    )
                    .is_ok();
                    if exists {
                        git(&ctx.root, &["worktree", "add", &p, branch])?;
                    } else {
                        git(&ctx.root, &["worktree", "add", "-b", branch, &p])?;
                    }
                }
                // A tab already titled with the branch is the branch's tab.
                let existing = tabs(core, &ctx.session)?
                    .into_iter()
                    .find(|(_, t)| t.as_deref() == Some(branch.as_str()));
                let tab_id = match existing {
                    Some((id, _)) => id,
                    None => match core
                        .request(
                            m::TabCreate {
                                session: ctx.session.clone(),
                                cwd: Some(path.to_string_lossy().into_owned()),
                                cmd: None,
                                title: Some(branch.clone()),
                                index: None,
                                restore: None,
                            }
                            .into(),
                        )
                        .map_err(|e| e.message)?
                    {
                        Response::TabCreate(t) => t.tab_id,
                        other => return Err(format!("unexpected answer {other:?}")),
                    },
                };
                Ok(CliOutput {
                    text: path.display().to_string(),
                    json: json!({
                        "branch": branch,
                        "worktree": path,
                        "tab_id": tab_id,
                        "session": ctx.session,
                        "created": !known,
                    }),
                    attach: None,
                })
            }
            "git tabs" => {
                let tabs = tabs(core, &ctx.session)?;
                let dir = ctx.root.join(".worktrees");
                let rows: Vec<serde_json::Value> = worktrees(&ctx.root)?
                    .into_iter()
                    .filter(|(p, _)| p.starts_with(&dir))
                    .map(|(p, b)| {
                        let branch = b.unwrap_or_default();
                        let tab = tabs
                            .iter()
                            .find(|(_, t)| t.as_deref() == Some(branch.as_str()))
                            .map(|(id, _)| id.clone());
                        json!({ "branch": branch, "worktree": p, "tab_id": tab })
                    })
                    .collect();
                let text = rows
                    .iter()
                    .map(|r| {
                        format!(
                            "{:<24} {}{}",
                            r["branch"].as_str().unwrap_or(""),
                            r["worktree"].as_str().unwrap_or(""),
                            if r["tab_id"].is_null() {
                                "  (no tab)"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(CliOutput {
                    text,
                    json: json!({ "worktrees": rows }),
                    attach: None,
                })
            }
            "git rm" => {
                let branch = &input.args[0];
                let path = worktree_path(&ctx.root, branch);
                if !worktrees(&ctx.root)?.into_iter().any(|(p, _)| p == path) {
                    return Err(format!("no worktree for {branch} at {}", path.display()));
                }
                if !input.flag("force") && !git(&path, &["status", "--porcelain"])?.is_empty() {
                    return Err(format!(
                        "{} has uncommitted changes; commit them or pass --force",
                        path.display()
                    ));
                }
                let mut closed = Vec::new();
                for (id, title) in tabs(core, &ctx.session)? {
                    if title.as_deref() == Some(branch.as_str()) {
                        core.request(m::TabClose { tab_id: id.clone() }.into())
                            .map_err(|e| e.message)?;
                        closed.push(id);
                    }
                }
                let p = path.to_string_lossy().into_owned();
                let mut args = vec!["worktree", "remove"];
                if input.flag("force") {
                    args.push("--force");
                }
                args.push(&p);
                git(&ctx.root, &args)?;
                Ok(CliOutput {
                    text: String::new(),
                    json: json!({ "branch": branch, "worktree": path, "closed_tabs": closed }),
                    attach: None,
                })
            }
            other => Err(format!("unknown command {other}")),
        }
    }
}
