# femux Architecture

Oct 10, 2026 · @Curtis Ault

## Goals and non-goals

The project is a Rust terminal multiplexer built as three strictly separated layers: a session daemon that owns processes, a versioned protocol, and clients that render. Agent tracking, remote hosts and every other feature are plugins over that core, never special cases inside it.

**Goals**

- Processes survive client exit, SSH drops and (optionally) daemon restarts and reboots
- tmux-level reliability and scriptability with modern defaults: vim modal navigation, tiling layouts, tabs, info bar, Kitty graphics
- Agent state (working, blocked, idle, done) from authoritative signals first, inference second
- SSH-first remote work that reuses the user's OpenSSH setup and never implements its own auth
- Plugins compiled into the binary and enabled in config; no runtime plugin manager
- A protocol stable enough that third-party clients (native terminals, editors, mobile) can attach

**Non-goals for v1**

- A GUI or native-rendering client (the layering keeps the door open)
- A plugin runtime for third-party code (plugins arrive through issues and PRs)
- Windows native support (WSL is fine)
- Byte-for-byte tmux config compatibility

## System overview

&#91;embedded content: architecture · 3 layers, plugins on the event bus\]

Clients, scripts and plugins all speak the same protocol, so the daemon stays small and the UI can be replaced or run from another machine without touching process ownership.

## Layer 1: session daemon

The daemon is the only process that must never crash, so it does the minimum: own PTYs, keep terminal state, hold the session model, persist it. No rendering, no keybindings, no agent logic.

| Component | Responsibility | Notes |
| --- | --- | --- |
| PTY manager | Spawn one PTY per pane, pump output into the VT, forward input, deliver signals, track the foreground process group | Uses `rustix`/`nix` for `openpty`, `forkpty`, `tcgetpgrp`. Exposes fg pid, cwd and process state per pane. Pane IDs are ULIDs, stable across restarts |
| Terminal state | One libghostty-vt instance per pane: screen grid, scrollback, modes, cursor | Rust binding over the C ABI; herdr already vendors one. Emits cell diffs per frame, not raw bytes |
| Query proxy | Answer terminal queries from pane programs (DA1/DA2, OSC 10/11 colors, Kitty keyboard push/pop, mode 2026/2031) | Answers come from the attached client's host-terminal capabilities, cached at attach; a pane never talks to the host terminal directly |
| Session model | Sessions → tabs → layout tree (BSP) → panes; focus, zoom, marks, floating panes | Pure data structure with a command API; every mutation emits an event |
| Client views | Several clients on one session, each with its own current tab and size | A tab is sized to the smallest client viewing it; clients on different tabs do not constrain each other |
| Store | SQLite (WAL mode) for layout snapshots, pane metadata, session files, optional capped scrollback | Written on every layout change, on SIGTERM, and on a timer. Restore is opt-in |
| Event bus | Broadcast channel for every state change plus pane metadata (`key=value` set by any client) | Subscribers get an ordered stream with sequence numbers; slow readers receive `events_lost` rather than stalling the daemon |
| Graphics passthrough | Kitty graphics and sixel sequences are captured per pane and re-emitted by the client with offsets | Image data lives in the daemon so a reattach restores it |

**Threading model.** One async runtime (tokio). Each PTY has a reader task; the VT for each pane is behind a mutex owned by that task. Clients get frames from a per-client sender that coalesces to the newest frame under backpressure, so one slow SSH client never blocks a local one and never receives a backlog of stale frames.

**Restart without losing processes (later).** Panes can be moved to a new daemon binary by passing PTY file descriptors over the socket (`SCM_RIGHTS`), the way herdr's live handoff works. Out of scope for 0.1 but the fd ownership is kept in one place so it stays possible.

## Layer 2: protocol

One protocol, used by the TUI client, the CLI, plugins and anything third-party. It is the surface the spec will define most rigorously, because an agent can build to a schema and a conformance suite.

- **Transport:** Unix domain socket at `$XDG_RUNTIME_DIR/mux/<server>.sock`, mode 0600. Remote use tunnels this socket through the user's own `ssh` (`-L` or `RemoteCommand`), never a listening TCP port
- **Framing:** length-prefixed MessagePack. JSON Schema in `spec/protocol/` is the source of truth; the Rust types are generated from it so the schema can never drift
- **Versioning:** a `hello` handshake exchanges a protocol version and a feature list. A client missing a feature loses that action, not the connection
- **Message families:** `session.*` (list, create, attach), `layout.*` (split, close, move, resize, zoom), `pane.*` (input, resize, signal, `set_meta`, `get_meta`), `frame.*` (cell diffs, cursor, graphics), scrollback.\* (read a line range, regex search, OSC 133 prompt marks, all run daemon-side so copy mode never ships whole histories), `events.subscribe`
- **Ordering:** every event carries a monotonic sequence number per daemon. A client subscribes before taking its first snapshot, so nothing is missed
- **Frames, not bytes:** the daemon sends rendered cell diffs for visible regions. Clients never parse escape sequences, which removes double emulation and makes a non-terminal client possible. Input flows the other way as structured key, mouse and paste events; the daemon encodes them for each pane according to the modes that pane requested (Kitty keyboard, bracketed paste, mouse reporting)
- **Scripting:** the CLI (`mux pane split`, `mux pane set working_dir=…`, `mux session launch writing`) is a thin wrapper, so every feature is scriptable from day one

A tmux control-mode shim can be added later as a separate adapter that translates to this protocol.

## Layer 3: clients

The TUI client is the only client in v1. It attaches to a daemon, receives frames, composes them into the host terminal, and turns keystrokes into protocol commands. Every UI feature in the wish list lives here.

- **Modal input:** three modes, *terminal* (keys go to the pane), *normal* (vim motions move focus, resize, swap, mark), and *copy* (scrollback with `hjkl`, `/` regex search, `v`/`y` selection, OSC 133 jump between prompts). A prefix key (default `ctrl+space`, several allowed) or `esc` chord enters normal mode; a which-key popup shows chords after a short delay
- **Layout engine:** BSP tiling with preselection, equalize, swap, rotate, zoom, and floating panes; master-stack and scrolling layouts as alternative schemes. Layouts are serializable so templates and restore reuse the same type
- **Tabs:** a tab strip with a running-process indicator (fg process is not the shell), an urgency hint on bell or agent state change, and a title taken from OSC 2 or the fg command
- **Info bar:** one bar, placed top, bottom, left or right, built from modules declared in config with an ordered list and a format string. Modules are plugins: clock, cpu, memory, network, battery, git branch, agent states, notification queue. A module reads the machine the client runs on; a module prefixed remote: reads the daemon's machine, so an SSH session can show both. A vertical bar stacks modules; the client reserves the space and the daemon never knows
- **Navigators:** fullscreen session navigator and window navigator (tree view with previews), a command palette (`ctrl+p`) over every bound action, and an optional side nav listing sessions for mouse users
- **Mouse:** click focus, drag resize, wheel scrollback, drag select, all optional
- **Rendering:** diff-based output with synchronized updates (mode 2026); Kitty graphics re-placed with pane offsets; theme from the host terminal's palette query with a light/dark override

The client renders only what the daemon sends plus its own chrome, so it can be restarted or upgraded while panes keep running.

## Plugin model

Plugins are Rust crates compiled into the binary and switched on in config. There is no runtime loader and no TPM-style manager: a new plugin arrives as a PR, ships in the next release, and is off until the user enables it.

```toml
[plugins]
enabled = ["agents", "infobar", "notify", "git", "clipboard"]

[plugins.notify]
chime = "~/.config/mux/done.wav"
```

A plugin implements one or more of three small traits, one per surface:

| Surface | What a plugin can do | Example |
| --- | --- | --- |
| Daemon side | Subscribe to events, read pane state and fg process info, set pane metadata | Agent tracker, git worktree watcher, session recorder |
| Client side | Add info-bar modules, palette actions, keybindings, overlays | CPU module, which-key, inbox overlay |
| CLI side | Add subcommands | `mux agent wait`, `mux notify test` |

Plugins talk to the core only through the protocol and the event bus, so a plugin can also run as a separate process (useful for one that needs root or heavy dependencies). The planned built-ins are listed in the release plan below.

## Agent tracking

Agent state comes from signals ranked by trust, and every state carries its source so the UI can show how sure it is. herdr relies mostly on screen scraping, which is why its changelog is full of per-agent detection fixes; this design makes scraping the last resort.

| Tier | Signal | Trust | How |
| --- | --- | --- | --- |
| 1 | Explicit report | Authoritative | Agent hooks call `mux pane set agent.state=blocked` or emit an OSC sequence the daemon understands (`OSC 9;4` progress, plus a documented private OSC for state). Ship hook snippets for Claude Code, Codex, OpenCode, Cursor |
| 2 | Process state | High | The daemon knows each pane's foreground process. Linux `/proc/<pid>/stat` and macOS `proc_pidinfo` show running, sleeping on a tty read (waiting for input) or blocked on network. Shell back in the foreground means the agent exited. macOS exposes less (thread wait state only), so tier 2 is marked best-effort there and the spec treats Linux as the reference |
| 3 | Output heuristics | Low | Silence after a burst, cursor parked after a prompt-like line. Only used when tiers 1 and 2 give nothing |

**State model:** `idle`, `working`, `blocked` (needs input), `done`, `error`, `exited`, each with `source` and `since`. Transitions are events on the bus, so the notifier, the info bar, the inbox and `mux agent wait` all consume the same stream.

**Notifications:** a `[notify]` table maps events to actions: a sound file or terminal bell, a desktop notification (OSC 777/99 or native), and a webhook. A `only_when_hidden` flag fires only for panes not on screen. Over SSH the event is forwarded to the client so the chime plays locally.

**Agent-to-agent:** `mux agent prompt <pane> "text"`, `mux agent wait <pane> --until blocked`, and `mux pane cat` give agents a way to drive each other without screen scraping.

## Security model

The daemon trusts only the local user and never implements authentication of its own.

- **Socket:** created with umask 0177 inside a user-owned directory; every connection is checked with `SO_PEERCRED` (Linux) or `LOCAL_PEERCRED` (macOS) and refused if the UID differs. No TCP listener exists. On macOS, where XDG\_RUNTIME\_DIR is unset, the socket lives in a 0700 directory under TMPDIR named for the uid. Pane environments are filtered through an allowlist before persistence so tokens in the environment never reach the store
- **SSH:** remote attach is `ssh host mux attach --stdio`, with the protocol flowing over the SSH channel, or a forwarded socket through `ControlMaster`. Keys, agent forwarding, MFA and host verification are OpenSSH's job. The client passes a host label, never credentials
- **Restore:** the store holds commands and working directories, so restore re-runs commands the user chose earlier. Restore is off by default, and when on it re-runs only commands marked `restore = true` in a session file; everything else comes back as an empty shell in the right directory. Scrollback persistence is a separate opt-in and the file is 0600
- **Clipboard:** OSC 52 writes are allowed, reads require an allowlist of pane commands
- **Pane metadata:** any local process can set it, so clients treat it as untrusted display data. Agent prompts delivered through `mux agent prompt` require the caller to be the same user; no cross-user control
- **Escape sequences:** the daemon's VT is the only parser, so a hostile program can corrupt its own pane and nothing else. Passthrough (DCS tmux-style) is off by default
- **Updates:** release binaries are signed; `mux update` verifies the checksum before replacing anything and never restarts a running daemon on its own

## Persistence and restore

SQLite is the single store; the schema is versioned with migrations so an upgrade never loses a layout.

| Table | Holds | Written when |
| --- | --- | --- |
| `servers` | server name, protocol version, last snapshot time | daemon start and stop |
| `sessions` | name, root dir, source session file if any | create, rename, close |
| `tabs` | session, index, title, layout tree as JSON | any layout change |
| `panes` | tab, cwd, command, env allowlist, restore flag, size | spawn, cwd change, exit |
| `pane_meta` | pane, key, value, updated\_at | any `set_meta` |
| `scrollback` | pane, chunk index, compressed cells (opt-in, capped) | on detach and on a timer |
| `events` | ring buffer of the last N events for late subscribers | continuously |

**Lifecycle**

1. Daemon starts, opens the store, and if `restore.enabled` is on offers each saved session as *restored* (panes marked "process not running")
2. Panes with `restore = true` re-run their command in their cwd, staggered to avoid a thundering herd
3. Snapshots are taken on every layout change, every 30 s while dirty, on `SIGTERM`, and on `logind` `PrepareForShutdown` where available
4. A snapshot that fails to load is kept as `session-backups/<ts>.db`, never overwritten

The client shows restored panes with a distinct border until a process runs in them, so nobody mistakes a restored shell for the agent that was there.

## Configuration

One TOML file at `~/.config/mux/config.toml`, validated on load with line-numbered errors, plus optional session files in `~/.config/mux/sessions/`.

```toml
[input]
prefix = ["ctrl+space"]
mode_keys = "vi"

[layout]
scheme = "bsp"            # bsp | master-stack | scrolling
gaps = 0

[infobar]
position = "bottom"       # top | bottom | left | right
left = ["session", "tabs"]
right = ["agents", "git", "cpu", "mem", "net", "clock"]

[restore]
enabled = true
scrollback = false

[notify]
agent_done = { sound = "~/.config/mux/done.wav", desktop = true, only_when_hidden = true }
agent_blocked = { sound = "bell" }

[plugins]
enabled = ["agents", "infobar", "notify", "git"]
```

A session file describes a launchable workspace. The CLI form `mux launch writing --tabs 4 --dir ~/writing -- nvim 'git status'` generates the same structure.

```toml
name = "writing"
root = "~/writing"
tabs = 4

[[tab]]
command = "nvim"
restore = true

[[tab]]
command = "git status"
```

Tabs without a `[[tab]]` entry open as plain shells in `root`. Keybindings, themes and plugin settings follow the same file; the full key is in the spec.

## Crate layout

A Cargo workspace with one crate per boundary, so the protocol and daemon can be tested without a terminal.

| Crate | Purpose | Depends on |
| --- | --- | --- |
| `mux-proto` | Message types generated from `spec/protocol/*.json`, framing, version negotiation | serde, rmp |
| `mux-vt` | Safe binding to libghostty-vt, cell diff extraction | ghostty C ABI (vendored, built with Zig at build time) |
| `mux-daemon` | PTY manager, session model, store, event bus, socket server | mux-proto, mux-vt, tokio, rusqlite |
| `mux-client` | TUI: modal input, layout engine, info bar, navigators, renderer | mux-proto, crossterm or termwiz |
| `mux-cli` | `mux` binary: subcommands and the launcher; embeds client and daemon | mux-proto, mux-client, mux-daemon |
| `mux-plugins/*` | One crate per built-in plugin behind a feature flag | mux-proto |
| `mux-test` | Headless harness: spawns a daemon, drives it over the protocol, asserts on screen state | mux-proto, mux-daemon |

A single `mux` binary ships; `mux daemon` runs the server and plain `mux` attaches, starting one if needed.

## Release plan and branch stacking

Each minor version is one feature set and one GitButler branch; patch versions stack on top of it for review feedback and hotfixes. The order is chosen so every version is usable on its own and the next one only adds.

| Version | Feature set | Why this order |
| --- | --- | --- |
| 0.1.0 | Protocol schema, `mux-proto`, `mux-vt` binding, daemon with PTYs, sessions, tabs, BSP splits, detach/attach, headless test harness | Everything else depends on a daemon that can be driven and tested without a UI; a bare viewer client (attach, watch, type into one pane) ships here too so a human can sanity-check it |
| 0.2.0 | TUI client: modal input, layout engine, tab strip with running indicator, copy mode, command palette, which-key | First usable release; replaces tmux for local work |
| 0.3.0 | Persistence: SQLite store, snapshots, opt-in restore, session files and `mux launch` | Needs a stable layout type from 0.2 |
| 0.4.0 | Info bar with modules, session and window navigators, side nav, themes | Pure client work, no daemon changes |
| 0.5.0 | Agent plugin: tiered state, hooks for Claude Code/Codex/OpenCode, notifier with chime, inbox overlay, `mux agent` CLI | Needs the event bus and metadata from 0.1 and the overlay system from 0.4 |
| 0.6.0 | SSH: `mux attach host`, socket tunneling, remote hosts list, notification forwarding | Protocol is stable by now; this is transport only |
| 0.7.0 | Graphics: Kitty and sixel passthrough, image restore on reattach | Isolated; can slip without blocking anything |
| 0.8.0 | Git worktrees, broadcast, scratchpad, marks, floating panes, session recording | Window-manager extras once the core is proven |
| 1.0.0 | Protocol frozen, conformance suite complete, docs | Promise of compatibility for third-party clients |

**Stacking rules**

1. `0.N.0` branches off the previous `0.(N-1).x` head and holds only that version's feature set, one commit series per requirement ID
2. `0.N.1`, `0.N.2`, ... stack on `0.N.0` and hold review changes or hotfixes; nothing new lands there
3. When `0.N.x` is accepted the whole stack merges to `main` in order and is tagged; the next feature branch starts from that tag
4. The agent works one branch at a time and opens the next only when the test suite for the current one is green
5. A fix needed by a later version in an earlier one goes in that earlier version's patch branch, so the stack stays rebase-clean

This keeps each branch reviewable against a single section of the spec and lets you drop a whole feature set by removing one branch from the stack.

## Review round 2

A second pass found six gaps, all now folded into the sections above.

| Gap | Risk if left | Fix applied |
| --- | --- | --- |
| Copy mode had no way to reach scrollback once frames replaced bytes | Client would need the whole history or a second parser | `scrollback.*` message family; search and prompt marks run in the daemon |
| Pane programs query the terminal (colors, DA, Kitty keyboard) and nothing answered | Fish, Neovim and agents misdetect capabilities, the most common tmux complaint | Query proxy in the daemon, seeded from the attached client's host terminal |
| Input was unspecified; keys would be forwarded as the host encodes them | Kitty keyboard and paste modes break per pane | Structured input events; the daemon encodes per pane mode |
| Multiple clients on one session had no size policy | Resize storms and clipped panes | Per-client views; smallest viewer of a tab sets its size |
| Slow clients could accumulate stale frames | Memory growth and lag after an SSH stall | Per-client sender coalesces to the newest frame |
| Tier 2 agent detection assumed Linux `/proc` everywhere | macOS would silently report wrong states | Marked best-effort on macOS; Linux is the reference platform |

Two risks remain open and belong in the spec's risk list: the Zig toolchain dependency for libghostty-vt, and the fact that restore re-runs user-chosen commands, which the default-off setting and the per-tab flag mitigate but do not remove.

**Name candidates** (iron + multiplex): femux (Fe + mux; binary `femux`), ferrox, fermux, oxmux, ferroplex, ironmux. Chosen: femux.

## Decisions

All decisions below were settled on 2026-10-11 and carry into the spec as ADRs.

| Decision | Choice |
| --- | --- |
| Name and binary | femux (Fe + mux); every `mux` in this doc reads as `femux` |
| License | MIT |
| Platforms for 0.1.0 | Linux and macOS; Linux is the reference for process-state detection |
| Terminal core | libghostty-vt, Zig toolchain required at build time; a pure-Rust fallback is a later feature flag |
| Default prefix | `ctrl+space`; prefix-less `alt+h/j/k/l` focus keys are off by default to stay out of Neovim's way |
| Protocol output | Rendered frames only through 1.0; a raw-bytes stream is deferred |
| Restore default | Off; when on, only tabs marked `restore = true` re-run commands |
| Client terminal backend | `termwiz` |
| Config changes | Applied with `femux reload`, no file watcher |
| Scrollback defaults | 10,000 lines per pane in memory; persisted scrollback capped at 2,000 lines per pane |
| Test assertions | JSON cell grid from the headless harness; plain-text dump available for debugging |
| Hook snippets | Installed with `femux integration install <agent>`; 0.5.0 ships Claude Code, Codex, OpenCode and Cursor |
| 0.1.0 scope | Includes a bare viewer client alongside the daemon and harness |
