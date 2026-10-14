# femux

A terminal multiplexer written in Rust. A session daemon owns your processes, a versioned
protocol carries its state, and clients render it. Processes survive a closed terminal, the
client can be restarted or upgraded while panes keep running, and every feature is scriptable
from the `femux` CLI. Agent tracking and the other extras are built-in plugins over that core.

## Status

femux is pre-1.0 and at version 0.10: a client and CLI meant for daily use, locally and over SSH,
with sessions that can survive a daemon restart and tracking of the coding agents running in them.

Working now: tabs, BSP splits with preselection, master and scroll layouts, floating panes, zoom,
marks, copy mode with regex search, a command palette, which-key hints, mouse support, one bar
at the top or bottom with the session, the tabs and cpu, memory, network, battery, git, agent and
notification modules, a panel on the left (`prefix b`, focused with `prefix ctrl+b`) listing
sessions, tabs, panes and every agent, session and pane navigators, themes, the `femux` CLI with `--json` output, session files
with `femux launch`, opt-in restore of sessions after the daemon restarts
(`[restore] enabled = true`), agent state tracking with an inbox, notifications, attaching
to a remote femux over SSH, and images: Kitty graphics and sixel from programs such as
`kitten icat` and `mpv --vo=kitty`, drawn when your terminal speaks the Kitty graphics
protocol (`femux graphics list` shows what each pane holds), git worktree tabs (`femux git tab
<branch>`), typing into every pane of a tab at once (`prefix B`) and asciinema recording of a
pane (`prefix R`, `femux record`). Dragging with the mouse selects text and copies it on release,
and the cursor takes the shape a program asks for (neovim's insert-mode bar).
`[clipboard] read_allow` lets named programs read the clipboard back, and
`[terminal] passthrough = true` passes DCS sequences through to your terminal. The order
they arrive in is the milestone table in [SPEC §13](spec/SPEC.md#13-milestones-and-branches).

## Build

femux builds libghostty-vt from source, so it needs a pinned Zig as well as Rust. With
[mise](https://mise.jdx.dev) installed, one command fetches both and builds:

```sh
mise exec -- cargo build --release
```

The first build downloads the pinned Ghostty source and takes a few minutes. The binary is
`target/release/femux`; `mise run install` builds it and copies it to `~/.local/bin`.
[CONTRIBUTING.md](CONTRIBUTING.md) covers the other tasks and building without mise.

## Quick start

```sh
femux                 # attach to a session, starting the daemon if none runs
femux ls              # list sessions
femux keys list       # every key binding and the action it runs
```

Inside femux, press `ctrl+space` to reach the prefix table. Wait a moment and a panel lists
every chord; `ctrl+space d` detaches and leaves everything running. `ctrl+space :` opens the
command palette, `ctrl+space s` and `ctrl+space w` the session and pane navigators, and
`ctrl+space -` and `ctrl+space |` split the pane. [docs/prefixes.md](docs/prefixes.md) lists
every key, explains how the table is laid out and shows how to change it.

To jump between prompts in copy mode and keep each pane's directory accurate, load the shell
integration from your shell's rc file:

```sh
eval "$(femux shell-integration bash)"    # or zsh
femux shell-integration fish | source     # fish
```

Configuration lives in `~/.config/femux/config.toml`;
[spec/config.example.toml](spec/config.example.toml) lists every key with its default.
`femux config check` validates it, and `femux reload` applies it to a running daemon.

A session file in `~/.config/femux/sessions/<name>.toml` describes a workspace, and
`femux launch <name>` opens it:

```toml
root = "~/writing"
tabs = 3

[[tab]]
title = "editor"
command = "nvim"
restore = true      # re-run after a daemon restart when [restore] is enabled
```

## Agents

femux tracks coding agents per pane as `working`, `blocked`, `done`, `error`, `exited` or
`idle`. Agent hooks are the most reliable source, so install them for the agents you use:

```sh
femux integration install claude-code    # or codex, opencode, cursor
```

Without hooks, femux falls back to OSC progress reports, then to the process state on Linux,
then to output heuristics. `ctrl+space i` opens the inbox of blocked, failed and finished
agents across sessions; `y` or `n` answers the selected one. From scripts:

```sh
femux agent list
femux agent wait %3 --until done --timeout 600
femux agent prompt %3 "run the tests" --wait
femux agent explain %3    # why femux thinks the agent is in that state
```

`[notify]` in the config plays a sound, shows a desktop notification or calls a webhook
when an agent finishes, blocks or fails. `femux notify test` tries the actions out.

## Remote hosts

femux attaches to a daemon on another machine through your own OpenSSH setup: keys, agent
forwarding and `~/.ssh/config` all apply, and femux never opens a port. Install femux on the
remote, then:

```sh
femux hosts add box me@build-box      # once
femux attach --host box               # or: femux attach --host me@build-box
```

The client runs locally and the remote keeps the sessions. If the link drops, the remote
carries on and the client reconnects on its own. Notifications from the remote play on your
machine with your own `[notify]` settings, and `remote:cpu`, `remote:mem`, `remote:net` and
`remote:hostname` in the info bar show the remote machine. `femux hosts add --forward` forwards
the remote socket with `ssh -L` instead of running the protocol over the SSH channel.

## Documentation

- [docs/prefixes.md](docs/prefixes.md) lists every key binding by table, generated from the
  defaults and checked by a test.
- [spec/SPEC.md](spec/SPEC.md) is the single source of truth: requirements, protocol,
  bindings and milestones.
- [spec/ARCHITECTURE.md](spec/ARCHITECTURE.md) explains the design and why it is shaped this way.
- [spec/protocol/](spec/protocol/) holds the JSON Schema for the client protocol.
- [CONTRIBUTING.md](CONTRIBUTING.md) covers building, testing and the toolchain.

## License

MIT.
