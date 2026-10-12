# femux

A terminal multiplexer written in Rust. A session daemon owns your processes, a versioned
protocol carries its state, and clients render it. Processes survive a closed terminal, the
client can be restarted or upgraded while panes keep running, and every feature is scriptable
from the `femux` CLI. Agent tracking and the other extras are built-in plugins over that core.

## Status

femux is pre-1.0 and at version 0.4: a client and CLI meant for daily local use, with sessions
that can survive a daemon restart.

Working now: tabs, BSP splits with preselection, master and scroll layouts, floating panes, zoom,
marks, copy mode with regex search, a command palette, which-key hints, mouse support, an info
bar on any side with cpu, memory, network, battery, git, agent and notification modules, session
and window navigators, a side nav, themes, the `femux` CLI with `--json` output, session files
with `femux launch`, and opt-in restore of sessions after the daemon restarts
(`[restore] enabled = true`).

Not yet: agent state tracking and notifications, SSH attach and image passthrough. The order
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
every chord; `ctrl+space d` detaches and leaves everything running. `ctrl+p` opens the command
palette, `ctrl+space S` and `ctrl+space W` the session and window navigators, and
`ctrl+space b` the side nav.

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

## Documentation

- [spec/SPEC.md](spec/SPEC.md) is the single source of truth: requirements, protocol,
  bindings and milestones.
- [spec/ARCHITECTURE.md](spec/ARCHITECTURE.md) explains the design and why it is shaped this way.
- [spec/protocol/](spec/protocol/) holds the JSON Schema for the client protocol.
- [CONTRIBUTING.md](CONTRIBUTING.md) covers building, testing and the toolchain.

## License

MIT.
