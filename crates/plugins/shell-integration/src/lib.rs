//! `shell-integration` plugin (0.2): `femux shell-integration <bash|zsh|fish>` prints a
//! snippet that makes the shell emit OSC 133 prompt marks (REQ-VT-005, copy mode `[`/`]`)
//! and OSC 7 working-directory reports (REQ-VT-006). Source it from the shell's rc file:
//! `eval "$(femux shell-integration bash)"`.

use femux_plugin::{CliArg, CliCommand, CliCore, CliOutput, CliPlugin, Plugin};

pub fn plugin() -> Plugin {
    Plugin {
        name: "shell-integration",
        client: None,
        daemon: None,
        cli: Some(Box::new(Cli)),
    }
}

pub const SHELLS: &[&str] = &["bash", "zsh", "fish"];

/// The integration snippet for `shell`, or `None` for an unknown shell.
pub fn script(shell: &str) -> Option<&'static str> {
    Some(match shell {
        "bash" => BASH,
        "zsh" => ZSH,
        "fish" => FISH,
        _ => return None,
    })
}

const BASH: &str = r#"# femux shell integration for bash: OSC 133 prompt marks and OSC 7 cwd.
if [ -n "$FEMUX_PANE" ] && [ -z "$FEMUX_SHELL_INTEGRATION" ]; then
  FEMUX_SHELL_INTEGRATION=bash
  # Runs first in PROMPT_COMMAND so $? is the user's command's status.
  __femux_precmd() {
    local code=$?
    if [ -n "$__femux_ran" ]; then
      printf '\e]133;D;%s\a' "$code"
      unset __femux_ran
    fi
    printf '\e]7;file://%s%s\a' "$HOSTNAME" "$PWD"
    printf '\e]133;A\a'
    __femux_at_prompt=1
  }
  # DEBUG fires for every simple command, including PROMPT_COMMAND's own: only the first
  # command after a prompt, and never one that is part of PROMPT_COMMAND, starts a run.
  __femux_preexec() {
    [ -n "$COMP_LINE" ] && return
    [ -z "$__femux_at_prompt" ] && return
    case ";${PROMPT_COMMAND[*]};" in *"$BASH_COMMAND"*) return ;; esac
    unset __femux_at_prompt
    __femux_ran=1
    printf '\e]133;C\a'
  }
  if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    PROMPT_COMMAND=(__femux_precmd "${PROMPT_COMMAND[@]}")
  else
    PROMPT_COMMAND="__femux_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  fi
  PS1="${PS1}\[\e]133;B\a\]"
  trap '__femux_preexec' DEBUG
fi
"#;

const ZSH: &str = r#"# femux shell integration for zsh: OSC 133 prompt marks and OSC 7 cwd.
if [[ -n "$FEMUX_PANE" && -z "$FEMUX_SHELL_INTEGRATION" ]]; then
  FEMUX_SHELL_INTEGRATION=zsh
  autoload -Uz add-zsh-hook
  __femux_precmd() {
    local code=$?
    if [[ -n "$__femux_ran" ]]; then
      printf '\e]133;D;%s\a' "$code"
      unset __femux_ran
    fi
    printf '\e]7;file://%s%s\a' "$HOST" "$PWD"
    printf '\e]133;A\a'
  }
  __femux_preexec() {
    __femux_ran=1
    printf '\e]133;C\a'
  }
  add-zsh-hook precmd __femux_precmd
  add-zsh-hook preexec __femux_preexec
  PS1="${PS1}%{$(printf '\e]133;B\a')%}"
fi
"#;

const FISH: &str = r#"# femux shell integration for fish: OSC 133 prompt marks and OSC 7 cwd.
if test -n "$FEMUX_PANE"; and not set -q FEMUX_SHELL_INTEGRATION
  set -g FEMUX_SHELL_INTEGRATION fish
  function __femux_prompt --on-event fish_prompt
    printf '\e]7;file://%s%s\a' (hostname) "$PWD"
    printf '\e]133;A\a'
  end
  function __femux_preexec --on-event fish_preexec
    printf '\e]133;C\a'
  end
  function __femux_postexec --on-event fish_postexec
    printf '\e]133;D;%s\a' $status
  end
end
"#;

struct Cli;

impl CliPlugin for Cli {
    fn name(&self) -> &'static str {
        "shell-integration"
    }
    fn commands(&self) -> Vec<CliCommand> {
        vec![CliCommand {
            name: "shell-integration",
            about: "Print the shell snippet that reports prompts and cwd to femux",
            args: vec![CliArg {
                name: "shell",
                help: "bash, zsh or fish",
                required: true,
            }],
        }]
    }
    fn run(
        &self,
        _cmd: &str,
        args: &[String],
        _core: &mut dyn CliCore,
    ) -> Result<CliOutput, String> {
        let shell = args.first().map(String::as_str).unwrap_or("");
        let script = script(shell)
            .ok_or_else(|| format!("unknown shell {shell:?}; one of {}", SHELLS.join(", ")))?;
        Ok(CliOutput {
            text: script.to_owned(),
            json: serde_json::json!({ "shell": shell, "script": script }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_marks_prompts_and_cwd() {
        for s in SHELLS {
            let sc = script(s).unwrap();
            assert!(sc.contains("133;A"), "{s} lacks prompt start");
            assert!(sc.contains("133;C"), "{s} lacks command start");
            assert!(sc.contains("133;D"), "{s} lacks command end");
            assert!(sc.contains("]7;file://"), "{s} lacks OSC 7");
            assert!(sc.contains("FEMUX_PANE"), "{s} runs outside femux");
        }
        assert!(script("tcsh").is_none());
    }
}
