# femux — Specification

Version 0.4 (draft) · 2026-10-12 · License: MIT

femux is a terminal multiplexer written in Rust. A session daemon owns processes, a versioned protocol carries state, and clients render. Agent tracking and every other feature are plugins over that core. This document is the single source of truth for an implementing agent. Requirement IDs (`REQ-*`) are stable; tests, commits and PRs reference them.

Conventions: MUST / SHOULD / MAY as in RFC 2119. "Daemon" is `femux daemon`; "client" is the TUI unless stated; "pane" is one PTY plus its terminal state.

---

## 1. Scope

### 1.1 Goals

- Processes survive client exit and SSH drops; optionally survive daemon restart and reboot.
- tmux-level reliability and scriptability with modern defaults: vim modal navigation, BSP tiling, tabs, info bar, Kitty graphics.
- Agent state from authoritative signals first, inference last.
- SSH-first remote work that reuses OpenSSH; femux never authenticates anyone itself.
- Built-in plugins enabled in config; no runtime plugin manager.
- A protocol stable enough for third-party clients by 1.0.

### 1.2 Non-goals for 1.0

- GUI or native-rendering client.
- Third-party plugin runtime.
- Native Windows (WSL is supported as Linux).
- tmux configuration compatibility.

### 1.3 Platforms

Linux (reference) and macOS. CI runs both. Where behavior differs, Linux is normative and macOS is best-effort; the difference MUST be documented at the requirement.

---

## 2. Architecture

```
┌──────────────┐  ┌──────────────┐  ┌──────────────────┐
│ TUI client   │  │ CLI/scripts  │  │ third-party      │
└──────┬───────┘  └──────┬───────┘  └────────┬─────────┘
       └─────────────────┼───────────────────┘
                 protocol (msgpack over unix socket, SSH-tunneled)
                         │
┌────────────────────────┴────────────────────────────┐
│ daemon: PTY manager · terminal state (libghostty-vt) │
│         query proxy · session model · client views   │
│         store (SQLite) · event bus · graphics        │
└──────┬──────────────────────────────┬───────────────┘
       │ spawn/read/write/signal      │ events out, metadata in
┌──────┴──────────┐          ┌────────┴──────────────────┐
│ shell/agent     │          │ built-in plugins          │
│ processes       │          │ agents · infobar · notify │
└─────────────────┘          │ git · clipboard · …       │
                             └───────────────────────────┘
```

Layer rules:

- REQ-ARCH-001: The daemon MUST NOT contain rendering, keybinding, theme or agent-detection code.
- REQ-ARCH-002: Every client, the CLI and every plugin MUST communicate with the daemon only through the protocol in §4.
- REQ-ARCH-003: The client MUST be restartable and upgradable while panes keep running.
- REQ-ARCH-004: A single `femux` binary ships; `femux daemon` runs the server, plain `femux` attaches and starts a daemon if none is running.

---

## 3. Daemon

### 3.1 PTY manager

- REQ-PTY-001: One PTY per pane, created with `openpty`; the child is placed in its own session and process group.
- REQ-PTY-002: The daemon MUST track the foreground process group (`tcgetpgrp`) of each pane and expose fg pid, command name and cwd via `pane.info`.
- REQ-PTY-003: Pane output MUST be read on a dedicated task and fed to that pane's VT without blocking other panes.
- REQ-PTY-004: Signals (`SIGINT`, `SIGTERM`, `SIGKILL`, `SIGHUP`, `SIGWINCH`) MUST be deliverable via `pane.signal`.
  Note: signals go to the pane's foreground process group, as if typed at the terminal.
- REQ-PTY-005: Pane IDs are ULIDs and MUST be stable across daemon restarts when restore is enabled.
  Note: ids are ULIDs from 0.1.0; with restore enabled, sessions, tabs and panes come back with their saved ids (0.3.0).
- REQ-PTY-006: Environment passed to a pane MUST be filtered by `[env] allow` before persistence; the live environment is unfiltered.
- REQ-PTY-007: New panes MUST set `TERM=xterm-256color` (overridable), `TERM_PROGRAM=femux`, `TERM_PROGRAM_VERSION`, `FEMUX_PANE=<ulid>`, `FEMUX_SOCKET=<path>`.

### 3.2 Terminal state

- REQ-VT-001: Each pane has one libghostty-vt instance holding screen grid, scrollback, modes and cursor.
- REQ-VT-002: The daemon MUST emit cell diffs per frame (changed cells, cursor, scroll regions) and MUST NOT send raw escape bytes to clients.
  Note (0.9.0): the frame cursor carries `blink` beside `shape` (ADR-023), both as the pane program set them (DECSCUSR). The client sets the host cursor's shape and blink (DECSCUSR) to the focused pane's whenever they change, so neovim's insert-mode bar shows; prompts and overlays use the host's default cursor, and the client restores it on detach.
- REQ-VT-003: Default scrollback is 10,000 lines per pane, configurable.
  Note: the limit is passed to libghostty-vt, which prunes whole pages, so retained history stays within one page of it (measured: up to ~2,300 lines under at 20 columns, ~600 at 80).
- REQ-VT-004: Frames MUST be coalesced per client: if a client cannot keep up, it receives the newest full state, never a backlog.
- REQ-VT-005: OSC 133 prompt marks MUST be recorded per line so clients can jump between commands.
- REQ-VT-006: OSC 2 (title) and OSC 7 (cwd) MUST be captured into pane info.

### 3.3 Query proxy

- REQ-QRY-001: Terminal queries from pane programs (DA1, DA2, XTVERSION, OSC 10/11/12 color queries, Kitty keyboard query, DECRQM for 2026/2031/2004/1006) MUST be answered by the daemon.
- REQ-QRY-002: Answers derive from the capabilities reported by the attached client at `hello`; with no client attached, conservative defaults apply.
  Note: the session's panes answer with the capabilities of the most recently attached client; when its last view detaches they fall back to the defaults.
- REQ-QRY-003: A pane program MUST never receive bytes from the host terminal directly.

### 3.4 Session model

- REQ-SESS-001: Hierarchy is server → sessions → tabs → layout tree → panes. A session has a name (unique per server), root dir, creation time.
- REQ-SESS-002: Layout tree nodes are `split{dir, ratio, children}`, `pane{id}`, `float{id, rect}`. Ratios are rationals in [0,1].
  Note: floats are listed beside the tiled tree in the tab's `floats`, not as tree nodes; splits are binary with `ratio = {num, den}` (ADR-015 #6, #7).
  Note: a split gives its first child `ratio` of the space left after a one-cell separator, so panes never overlap and clients draw borders in the gaps. A pane closes when its process exits; an empty tab or session closes with it.
- REQ-SESS-003: Every mutation MUST emit exactly one event (§4.6) after the mutation is applied.
  Note: a request that performs several mutations emits one event per mutation (e.g. `layout.split` emits `pane.spawned`, `layout.changed`, `layout.focused`).
- REQ-SESS-004: Marks: a pane may carry up to 26 single-letter marks per session (`a`–`z`), used by `jump_to_mark`.
  Note (0.8.0): `prefix M` opens the marks overlay: each mark of the session with its tab number and what its pane shows. A mark's letter or `enter` jumps there, `j`/`k` move, `x` deletes the selected mark (`pane.mark` with `clear`), `q` closes. A tiled pane shows its marks on the border above it (`'a 'b`).
- REQ-SESS-005: Zoom is a per-tab flag; a zoomed tab renders only its focused pane and the tab strip.

### 3.5 Client views

- REQ-VIEW-001: Multiple clients MAY attach to one session; each has its own current tab, size and mode.
- REQ-VIEW-002: A tab's size is the minimum width and height across clients currently viewing it.
- REQ-VIEW-003: Clients on different tabs MUST NOT constrain each other.
- REQ-VIEW-004: Pane resize from a view change MUST be debounced (default 50 ms) to avoid resize storms.

### 3.6 Event bus

- REQ-EVT-001: All events carry `seq` (monotonic per daemon), `ts` (ms since epoch) and `kind`.
- REQ-EVT-002: A subscriber specifies a filter (kinds, session, pane). A subscriber that falls more than `events.buffer` (default 4096) behind receives `events_lost{from, to}` and continues.
  Note: an event with no session of its own (`clipboard.changed` from `clipboard.set`) passes every session filter.
- REQ-EVT-003: The last `events.replay` (default 1,000) events are kept so a late subscriber can request `since: seq`.
- REQ-EVT-004: Pane metadata is a map `string → string` per pane; `pane.set_meta` emits `pane.meta_changed`. Metadata is untrusted display data.

### 3.7 Graphics

- REQ-GFX-001: Kitty graphics protocol commands MUST be parsed per pane; image payloads are stored in the daemon and placements sent to clients with pane-relative coordinates.
  Note (0.7.0): libghostty-vt parses and stores the images; femux-vt decodes PNG payloads for it. The direct, temp-file and shared-memory media work (programs run on the daemon's machine); the file medium is off. Every frame carries the visible placements of its panes (ADR-021), with the image's generation, the shown source rectangle, pixel offsets and z. A client reads each image once per generation with `image.read` (6 MiB of base64 per answer), transmits it to its host under an id of its own, places it with `a=p` and `C=1` at the pane's screen position, and deletes placements that go away. Images are hidden while the navigators, inbox, palette or help cover the panes, and cleared from the host when the client exits. The client probes the host with a Kitty query and CSI 16 t; `hello.term` carries the cell size, which sets each pane terminal's cell size and the pty's pixel size (`TIOCGWINSZ`, needed by `kitten icat`). A host without Kitty graphics, or `[terminal] kitty_graphics = false`, shows no images. Kitty Unicode placeholders (virtual placements shown by U+10EEEE cells, as yazi and programs under tmux draw) are found in the screen's cells by femux-vt, following Ghostty's rules, and sent as one ordinary placement per run of cells; the client draws placeholder cells blank. How pixels reach the host (ADR-021 #4): a client on the daemon's machine whose host answers a Kitty `t=s` probe gets each image through a POSIX shared memory object it hands to the host; a remote client gets zlib-compressed chunks it forwards with `o=z`; otherwise base64 chunks. Kitty shared memory commands are taken over as they arrive: femux-vt opens the program's object, removes its name at once and hands libghostty-vt a private copy, so a program that reuses one name for every frame (mpv) never has a frame torn or lost; names without the leading `/` are accepted and `m=1` is ignored, as Kitty does. Video from `mpv --vo=kitty` reaches the host at about its frame rate with `--vo-kitty-use-shm=yes`; mpv's default base64 output is limited by mpv itself. Each new generation of an image gets a new, higher id on the host (Ghostty draws images of equal z in id order, so the newest must have the highest) and is placed over the shown one, which stays underneath for 50 ms or until the next generation (a host uploads a new image on its next render and draws nothing for it until then), and each batch of image commands is wrapped in synchronized output (mode 2026) when the host has it, so video does not flicker.
- REQ-GFX-002: Sixel passthrough is supported as a per-pane blob with clipping to pane bounds (pixel-level clipping MAY be deferred).
  Note (0.7.0): instead of a blob, femux-vt decodes each sixel (`DCS … q … ST`, raster attributes, RGB and HLS registers, repeats; undrawn pixels transparent) into RGBA and hands it to libghostty-vt as a Kitty image displayed at the cursor, followed by a new line, as a sixel leaves the cursor below the image. Sixels therefore scroll, clip to the pixel, survive reattach, count against the memory cap and are drawn like Kitty images, which needs Kitty graphics on the client's host: a host that speaks sixel but not Kitty shows none. DA1 offers sixel (feature 4) only while the session's client can show images.
- REQ-GFX-003: Reattach MUST restore visible image placements.
  Note (0.7.0): images and placements are terminal state in the daemon, so a new attach's first frame carries them. They are not part of the store's snapshots: after a daemon restart, restored panes come back without images.
- REQ-GFX-004: Image memory per pane is capped (default 64 MiB, LRU eviction).
  Note (0.7.0): the cap is libghostty-vt's per-terminal image storage limit, which evicts the oldest images first; it is not configurable yet.

### 3.8 Lifecycle

- REQ-LIFE-001: `SIGTERM` to the daemon MUST snapshot state, then exit without killing panes if restore is enabled, or kill panes with `SIGHUP` if not.
  Note: the snapshot is taken first, before anything else can change. With restore enabled femux sends no SIGHUP, but the kernel still hangs up each PTY when the daemon exits, so processes survive only as restored panes; keeping them alive needs the live handoff of REQ-LIFE-003. `server.stop` SIGHUPs when `kill_panes` is true or restore is off.
- REQ-LIFE-002: The daemon MUST exit when the last session is closed unless `daemon.keep_alive = true`.
  Note: when a request closes the last session (or is `server.stop`), its reply is written before the daemon begins to exit.
- REQ-LIFE-003: (Deferred, post-1.0) Live handoff of PTY fds to a new daemon via `SCM_RIGHTS`. Fd ownership MUST remain in the PTY manager so this stays possible.

---

## 4. Protocol

### 4.1 Transport

- REQ-PROTO-001: Unix domain socket at `$XDG_RUNTIME_DIR/femux/<server>.sock`; on macOS, `$TMPDIR/femux-<uid>/<server>.sock`. Directory mode 0700, socket mode 0600.
- REQ-PROTO-002: Peer UID MUST be checked (`SO_PEERCRED` / `LOCAL_PEERCRED`) and connections from other UIDs refused before any message is read.
  Note: refusal of other UIDs is unit-tested; an integration test would need a second user.
- REQ-PROTO-003: No TCP listener. Remote attach uses `ssh <host> femux attach --stdio` with the protocol on stdin/stdout, or a socket forwarded by OpenSSH.
  Note (0.6.0): before it takes over the terminal, `femux attach --host <h>` runs `ssh <dest> true` with `ControlMaster=auto`, `ControlPersist=600` and `ControlPath=/tmp/femux-ssh-<uid>/%C`, so host-key and password prompts appear as they do for plain `ssh`. Every later command uses the same options and so the same master connection: `ssh -T -o ServerAliveInterval=5 -o ServerAliveCountMax=3 <dest> femux attach --stdio` (`$FEMUX_SSH` overrides `ssh`). `<h>` is a name from `hosts.toml` (next to `config.toml`, kept by `femux hosts add|list|rm`) or a raw ssh destination. `--stdio` starts the remote daemon if none runs and copies bytes between stdin/stdout and its socket. A host saved with `--forward` instead runs `femux daemon --ensure` over ssh (start if needed, print the socket), forwards that socket with `ssh -N -L` to a local socket in `$TMPDIR` and connects there. When the link drops, the remote daemon sees an ordinary disconnect and keeps the session; the client says the link is lost and reconnects in the background (1 s, doubling to 30 s) with `BatchMode=yes`, since it owns the terminal (the master connection, while it lasts, spares the prompts), then reattaches the same session. `prefix d` quits meanwhile. The session chip, the side nav and the session navigator name the host.
- REQ-PROTO-004: Framing is 4-byte big-endian length prefix followed by one MessagePack map. Max frame 16 MiB.

### 4.2 Schema

- REQ-PROTO-010: Message schemas live in `spec/protocol/*.json` (JSON Schema draft 2020-12). Rust types in `femux-proto` are generated from them at build time; hand-edited types are a build failure.
  Note: types are generated into `OUT_DIR` by `crates/femux-proto/build.rs`, so there is no checked-in type to hand-edit; the supported JSON Schema subset is documented there and anything outside it fails the build.
- REQ-PROTO-011: Every message is `{id: u64, kind: string, body: map}`. Requests get a response with the same `id`; events have `id = 0`.
  Note: a response's kind is its request's kind (`hello` → `hello_ok`); failures use kind `error`. See ADR-015 for the other catalog refinements.
- REQ-PROTO-012: Unknown fields MUST be ignored; unknown `kind` MUST return `error{code: "unknown_kind"}`.

### 4.3 Handshake

```
client → hello {proto: 1, client: "femux-tui/0.2.0", features: [...], term: {cols, rows, colors: "truecolor", kitty_graphics: bool, kitty_keyboard: bool, sixel: bool, sync_output: bool}}
daemon → hello_ok {proto: 1, server: "name", version: "0.2.0", features: [...], seq: <current>}
```

- REQ-PROTO-020: A client missing a feature loses that action; the connection is kept.
  Note: features are message families (kind prefixes); a request outside the intersection of client and daemon features gets `feature_missing` (ADR-015 #10).
- REQ-PROTO-021: Protocol major version mismatch returns `error{code: "proto_version"}` and closes.

### 4.4 Message catalog

| kind | dir | body (request) | response |
| --- | --- | --- | --- |
| `session.list` | c→d | – | `[{name, root, tabs, clients, created}]` |
| `session.create` | c→d | `{name, root?, file?}` | `{name}` |
| `session.attach` | c→d | `{name, tab?}` | `{view_id, snapshot}` |
| `session.detach` | c→d | `{view_id}` | – |
| `session.rename` | c→d | `{name, new}` | – |
| `session.kill` | c→d | `{name}` | – |
| `tab.create` | c→d | `{session, cwd?, cmd?, title?, index?}` | `{tab_id}` |
| `tab.select` | c→d | `{view_id, tab_id}` | – |
| `tab.close` | c→d | `{tab_id}` | – |
| `tab.move` | c→d | `{tab_id, index}` | – |
| `tab.rename` | c→d | `{tab_id, title}` | – |
| `layout.split` | c→d | `{pane_id, dir: "h"\|"v", ratio?, cwd?, cmd?, preselect?}` | `{pane_id}` |
| `layout.close` | c→d | `{pane_id}` | – |
| `layout.focus` | c→d | `{view_id, pane_id \| dir: "h"\|"j"\|"k"\|"l"}` | `{pane_id}` |
| `layout.resize` | c→d | `{pane_id, dir, delta_cells}` | – |
| `layout.swap` | c→d | `{a, b}` | – |
| `layout.equalize` | c→d | `{tab_id}` | – |
| `layout.rotate` | c→d | `{pane_id}` | – |
| `layout.zoom` | c→d | `{tab_id, on?: bool}` | `{zoomed}` |
| `layout.float` | c→d | `{pane_id, rect?}` | – |
| `layout.tile` | c→d | `{pane_id}` | – |
| `layout.set_scheme` | c→d | `{tab_id, scheme: "bsp"\|"master"\|"scroll"}` | – |
| `layout.get` | c→d | `{tab_id}` | `{tree}` |
| `layout.apply` | c→d | `{tab_id, tree}` | – |
| `pane.input` | c→d | `{pane_id, events: [KeyEvent\|MouseEvent\|Paste]}` | – |
| `pane.resize` | c→d | `{pane_id, cols, rows}` (views only) | – |
| `pane.signal` | c→d | `{pane_id, sig}` | – |
| `pane.info` | c→d | `{pane_id}` | `{id, tab, cwd, fg_pid, fg_cmd, title, exit?, meta}` |
| `pane.set_meta` | c→d | `{pane_id, set: {k: v}, unset: [k]}` | – |
| `pane.mark` | c→d | `{pane_id, mark: "a"-"z"}` | – |
| `pane.respawn` | c→d | `{pane_id, cmd?}` | – |
| `frame.subscribe` | c→d | `{view_id}` | – |
| `frame.ack` | c→d | `{view_id, frame_seq}` | – |
| `scrollback.read` | c→d | `{pane_id, from_line, count}` | `{lines: [[Cell]]}` |
| `scrollback.search` | c→d | `{pane_id, regex, from_line, dir, limit}` | `{matches: [{line, col, len}]}` |
| `scrollback.marks` | c→d | `{pane_id}` | `{prompts: [line]}` |
| `scrollback.clear` | c→d | `{pane_id}` | – |
| `clipboard.set` | c→d | `{text}` | – |
| `events.subscribe` | c→d | `{kinds?, session?, pane?, since?}` | – |
| `events.unsubscribe` | c→d | – | – |
| `store.snapshot` | c→d | – | `{ts}` |
| `server.info` | c→d | – | `{version, uptime, sessions, panes, clients}` |
| `server.stop` | c→d | `{kill_panes: bool}` | – |

Input event shapes:

```
KeyEvent   {t:"key", key: string, mods: ["ctrl","alt","shift","super"], text?: string, event: "press"|"repeat"|"release"}
MouseEvent {t:"mouse", x, y, button?, mods, event: "press"|"release"|"move"|"wheel_up"|"wheel_down"}
Paste      {t:"paste", text}
Cell       {ch: string, fg: Color, bg: Color, ul?: Color, attrs: u16, width: 1|2}
Color      "default" | u8 | [r,g,b]
```

- REQ-PROTO-030: Input is structured; the daemon encodes keys for each pane according to the modes the pane has requested (Kitty keyboard flags, bracketed paste, mouse modes).

### 4.5 Frames (d→c)

```
frame {view_id, frame_seq, full: bool, panes: [{pane_id, rect, cells: [ {x,y,run:[Cell]} ], cursor: {x,y,shape,visible,blink}, mouse, scroll_offset}], tab: {id, title, index, count}, images: [{id, pane_id, placement}]}
```

- REQ-PROTO-040: A client acks frames with `frame.ack`; the daemon keeps at most one unacked frame per view and coalesces newer state into it.
- REQ-PROTO-041: After `session.attach` and after any `events_lost`, the next frame MUST be `full: true`.

### 4.6 Events (d→c, `id = 0`)

`session.created|renamed|closed`, `tab.created|selected|closed|moved|renamed`, `layout.changed{tab_id, tree}`, `pane.spawned|exited{code}|fg_changed{pid, cmd}|cwd_changed|title_changed|meta_changed{set,unset}|bell`, `view.attached|detached|resized`, `store.snapshot`, `events_lost{from,to}`.
  Note (0.9.0): `pane.passthrough{pane_id, data}` carries one DCS sequence a pane program emitted, base64, only while `[terminal] passthrough = true` (REQ-SEC-004); like `pane.output` it is never retained for `since`.

### 4.7 Errors

`error {id, code, message, data?}`. Codes: `unknown_kind`, `proto_version`, `not_found`, `conflict`, `invalid`, `denied`, `feature_missing`, `internal`.

---

## 5. Client (TUI)

### 5.1 Modes

- REQ-UI-001: Three modes: `terminal` (default; keys go to the focused pane), `normal` (vim motions act on layout), `copy` (scrollback).
- REQ-UI-002: Prefix (default `ctrl+space`, list allowed) enters a one-shot `prefix` table; `prefix prefix` sends the prefix to the pane; `prefix space` enters sticky `normal`; `prefix esc` cancels.
  Note: sticky `normal` uses the prefix table directly; `esc`, `space` or the prefix key returns to `terminal`. An unbound key after the prefix is swallowed and named in the status line; `esc` is swallowed silently.
  Note (0.8.3): `prefix esc` used to enter sticky `normal`. It now cancels, since `esc` is the reflex after an accidental prefix, and `space` is the easy key to reach after `ctrl+space`.
- REQ-UI-003: A which-key overlay lists available chords after `ui.which_key_delay_ms` (default 400).
  Note: the panel is the `which-key` plugin's; with the plugin disabled the delay passes silently and the table still works.
  Note: the panel splits the pane area's width into up to four even columns (at least 30 cells each). Groups stay whole and in order, cut so the tallest column is shortest; text is clipped to its column with `…`, and descriptions drop parenthetical asides (help keeps them).
- REQ-UI-004: Prefix-less bindings (`alt+h/j/k/l`) exist but are off by default.

Default bindings (prefix table unless noted):

| key | action |
| --- | --- |
| `h j k l` | focus left/down/up/right |
| `H J K L` | resize in direction |
| `-` / `\|` (also `\`) | split below / right |
| `x` | close pane (confirm if fg process ≠ shell) |
| `z` | zoom toggle |
| `f` | float/tile toggle |
| `=` | equalize |
| `r` | rotate split |
| `e` | swap (exchange) with next |
| `t` / `T` | new tab / rename tab |
| `n` / `p` (also `N`) / `1-9` | next / previous / nth tab |
| `m<a-z>` / `'<a-z>` | set / jump to mark |
| `o` / `O` | next / previous float |
| `[` | copy mode |
| `]` | paste |
| `s` | session navigator |
| `S` | new session (asks for a name) |
| `w` | pane navigator |
| `:` | command palette |
| `b` / `ctrl+b` | toggle the panel / focus it (0.10.0) |
| `d` | detach |
| `?` | help |
| `space` | sticky normal mode |

Copy mode: `hjkl`, `w b e`, `0 $`, `gg G`, `ctrl+u/d`, `/ ? n N` (regex), `v V ctrl+v` select, `y` yank (OSC 52 + clipboard.set), `[ ]` previous/next prompt mark, `q` exit.

Note (0.8.3): the table was re-keyed so that one rule covers it: a lowercase letter acts or opens the list it names (`s` sessions, `w` panes, `b` panel, `i` inbox), and shift on a letter means the same object the other way, its overview or a new one (`n`/`N` tabs, `o`/`O` floats, `m`/`M` marks, `t`/`T` tabs, `f`/`F` floats, `s`/`S` sessions, `h`/`H` focus and resize). `p` stays previous tab beside `N`, and `]` stays paste beside `[` copy mode, as in tmux. Splits moved to `-` and `|` (with `\` for those who would rather not hold shift), swap to `e` for exchange, and float cycling to `o`/`O`; `W`, `v`, `g` and `G` are unbound. Until 0.8.1 the table had `s`/`v` splits, `w` swap, `g`/`G` floats and `S`/`W` navigators.
Note (0.8.3): `S` opens a `new session` prompt holding the focused pane's directory name (empty in the home directory). `enter` creates the session rooted in that directory and switches to it; `esc` cancels; a name already taken is refused and named in the status line.
Note (0.8.3): `docs/prefixes.md` is the user-facing key reference. Its tables are generated from the default bindings and every built-in plugin's keys; femux-cli `plugins::tests::prefixes_doc_matches_the_default_bindings` fails when they drift, and `UPDATE_DOCS=1` rewrites them.
Note: `]` pastes the newest server clipboard text the client has seen (its own yank or any `clipboard.changed`). `s`, `w` and `b` open the navigators and the panel (the side nav until 0.10.0) from 0.4.0 (they were inert in 0.2.0). `x` confirms when the pane's foreground process is not its shell (`pane.info.pid`). Action names for `[keys.*]` are listed by `femux keys list`.

### 5.2 Layout

- REQ-UI-010: BSP with preselection (`prefix` then `shift+h/j/k/l` chooses where the next split goes), master-stack and scrolling schemes selectable per tab.
  Note: `shift+h/j/k/l` are the resize keys in the §5.1 table, so preselection is `prefix ctrl+h/j/k/l` (actions `preselect left|down|up|right`); the choice shows in the status line and applies to the next split. Schemes are palette actions `scheme bsp|master|scroll` (`layout.set_scheme`).
  Note (0.6.1): `[layout] scheme` is the scheme new tabs start in. `[layout] gaps = n` widens every separator between tiled panes to `1 + 2n` cells with the border down the middle, falling back to one cell where a split is too small for it; the outer edge gets no margin. `femux reload` re-applies gaps to open tabs; the scheme applies to tabs created afterwards.
- REQ-UI-011: Floating panes render above tiles; mouse drag moves and resizes them.
  Note: dragging a float's top border moves it, dragging its bottom-right corner resizes it; both send `layout.float` with the new rect (ADR-016 #5).
  Note (0.8.0): `prefix F` opens a scratch float (a new shell in the tab's directory, floated and focused) when the tab has none, and otherwise hides or shows the tab's floats (`tab.set floats_hidden`): hidden floats keep running, leave the screen and give up focus. `prefix o`/`O` focus the next/previous float, showing hidden ones (`g`/`G` until 0.8.1, when `o` was read as "other float" to match the `n`/`N` shape). A float's border carries its pane's title.
- REQ-UI-012: Layout templates can be saved and loaded by name (`femux layout save|load`); they serialize `layout.get` output plus cwd and cmd per pane.
  Note: one JSON file per name under `$XDG_STATE_HOME/femux/layouts/` (macOS: `~/Library/Application Support/femux/layouts/`); `load` opens a new tab and rebuilds the tree by splitting, re-running each pane's `cmd`; `list` names the files and `export` prints a template, or the current tab when no name is given.

### 5.3 Tabs

- REQ-UI-020: The tab strip shows index, title, a running indicator when the fg process is not the shell, an urgency marker on bell or agent state change in an unfocused tab, and a zoom marker.
  Note (0.10.0): the strip is the middle of the one bar (REQ-UI-030), not a row of its own: `1 shell  2 api ●●  3 docs ●  4 infra !  5 scratch`, the current tab in the accent style. When the row is short, titles truncate to fit before any tab is dropped, and tab numbers always show. The strip's legend (`● running · ! needs you · Z zoomed`) is gone; the help overlay explains the markers.
  Note: "not the shell" is `pane.info.fg_pid != pid` (ADR-016 #1); urgency (`!`) comes from `pane.bell` or a `pane.meta_changed` setting `agent.state` and clears when the tab is selected; zoom shows `Z`.
- REQ-UI-021: Title precedence: user rename > OSC 2 > fg command > cwd basename.
  Note (0.6.1): while the shell itself is in the foreground (the pane runs no other program), the title is `<shell> <dir>` (`~` for home, else the last path component) instead of the shell's OSC 2 title, which is usually that same information spelled `~ - fish`. A program's OSC 2 title still beats its command name. The session navigator and window navigator show the same titles as the tab strip.

### 5.4 Info bar

- REQ-UI-030: One bar, one row, at `[bar] position = top|bottom`: the session chip and mode on the left, the tab strip in the middle, the modules on the right. Nothing else frames the panes: no separate tab row, no vertical bar.
  Note (0.10.0): this replaces the 0.4.0 `[infobar]` (`top|bottom|left|right`, `left`/`right` module lists) and `[tabs]` (`position`, `running_indicator`) sections with `[bar]` (`position`, `modules`, `interval_ms`, `running_indicator`); `config check` names the replacement for the old keys. When the row is short, the right side gives first: modules render their short form (the value without its label: `31%`, `9.2G`, `main`; `agents` drops zero counts and joins glyph and count, `◐1`; `clock` drops the day); then tab titles shrink to 8 columns; then modules drop from the right end; then titles shrink further (REQ-UI-020), as `narrow.png` draws it. Full forms show only while every title fits whole. Modules are two cells apart. Prompts, confirmations and messages use the bar row while they are shown. The left column that `[infobar] position = left` provided is now the panel (REQ-UI-042). `femux reload` applies `[bar]`.
  Note (0.8.1): the chip reads `session <name>` (`session <host>:<name>` over `--host`), so a session named by a number never reads as a tab index. `femux` with no session to attach to creates one named `main`.
  Note (0.10.0): with the tabs on the same row the chip is the bare name again (`api`, `build-box:deploy`), as the mocks draw it; only a name that is all digits keeps the `session ` prefix.
  Note (0.6.1): the horizontal bar keeps one cell of padding at each end. It shows no mode label while typing into panes, only `PREFIX`, `NORMAL`, `COPY` and the navigators' names, and no permanent prefix hint (the which-key panel shows the prefix table).
- REQ-UI-031: Modules are listed in config in order; each is a plugin implementing `InfoModule { fn render(&self, w: usize) -> Spans; fn interval(&self) -> Duration }`.
  Note: modules also get `sample(&mut self, ctx)`, called every `max(interval(), [bar] interval_ms)` and only for modules the config shows, and `render` reads a `BarCtx` (session, tabs, agent counts, the focused pane's cwd, notifications of hidden panes, orientation). Unknown names are skipped. `session` is the client's own chip and mode, so with the `infobar` plugin disabled the bar still shows them.
  Note (0.10.0): `[bar] modules` is the one list, rendered on the right in order; `session` and `tabs` are no longer modules because the chip and the strip are fixed parts of the bar. `render` gets the width the bar can spare, and a module that returns nothing takes no room.
- REQ-UI-032: Built-in modules: `clock`, `cpu`, `mem`, `net`, `battery`, `git`, `agents`, `notifications`, `hostname`. A module name prefixed `remote:` renders on the daemon host (data arrives as pane-independent events).
  Note (0.6.0): the `hosts` plugin's daemon side samples hostname, cpu, memory and network on the daemon's machine every 2 s and publishes them (`plugin.publish`, ADR-020); its client side provides `remote:cpu`, `remote:mem`, `remote:net` and `remote:hostname`, labelled `<host>:<module>`. Other `remote:` names still render the local module.
  Note (0.4.0): `remote:<module>` renders the local module until SSH attach (0.6.0) gives the daemon a different host. `agents` counts `agent.state` metadata as it stands; `notifications` shows `notify.posted` (ADR-018 #2). `cpu`, `mem` and `net` use the `sysinfo` crate; `battery` reads `/sys/class/power_supply` on Linux and `pmset` on macOS; `git` runs `git status` off the client's thread. `battery` and `git` show nothing without a battery or a repository.
- REQ-UI-033: `notifications` shows the newest N notification titles when the originating pane is hidden (useful in zoom).
  Note: hidden means not in this client's current tab, or zoomed away; a notification about no pane always shows. The bar shows the newest.

### 5.5 Navigators and overlays

- REQ-UI-040: Session navigator (`s`; `S` starts a new one): full-screen list of sessions with tab counts, agent summary, last activity; `enter` attaches, `x` kills, `r` renames, `/` filters.
  Note: each session row shows its root, agent summary and tab count (`attached` for this client's own); `tab` expands a session to its tabs; created, last activity (ADR-018 #1), tab, pane and client counts sit under the preview. `n` creates a session; `x` asks first, and killing the attached session first moves this client to another one. `enter` attaches by detaching this client's view and attaching to the chosen session.
  Note (0.8.1): the list shows sessions only until `tab` expands one; every row names its kind (`session api`, `tab 1 fish ~`), the session row's right side reads `5 tabs · attached`, and the footer counts tabs, panes and clients. A session's tabs never appear as sessions.
- REQ-UI-041: Window navigator (`w`): tree of sessions → tabs → panes with a live preview of the selected pane.
  Note (0.8.1): named the pane navigator (`PANES` on the bar, action `panes`): femux has no window object, a tab owns its layout of panes, and the tree's leaves are panes. Pane rows are titled like the tab strip (`● fish ~`, or the program and its directory).
  Note: the preview reads the selected pane's screen with `scrollback.read` every 500 ms; `enter` goes to the pane, attaching to its session when it is another one.
- REQ-UI-042: Panel (`b` toggles, `ctrl+b` focuses): a collapsible left column of stacked sections, each from a plugin, listed in `[panel] sections`; open state and width are the client's, the daemon resizes the view to what is left.
  Note (0.10.0): replaces the 0.4.0 side nav. Width is `[panel] width` (default 28 columns) or whatever the mouse drags its right edge to; `[panel] open` says whether a new client starts with it open. Sections stack top to bottom with a header row each (`▾ workspace`, `▸ agents`); a header click or `h`/`l` on it collapses or expands the section, and a collapsed section keeps its header with a one-line summary on the right (`agents` shows its state counts, `◐3 ■1 ✓1`). Focused (`prefix ctrl+b`; `esc` or `q` give focus back to the pane, `prefix b` hides it), `j`/`k` move over every row of every section, `tab` jumps to the next section header, `enter` acts on the row. Mouse clicks act without focusing. Overlays (`s`, `w`, `i`) stay as they are: the panel is the persistent view, they are the quick one.
  Note (0.10.0): built-in sections: `workspace`, sessions → tabs → panes with the attached session expanded and the others collapsed (a remote session reads `host:name`); a tab row carries its `●`/`!`/`Z` markers and, right-aligned, its git branch with the ahead count (`main +3`) when its focused pane is in a repository; a pane row shows the agent glyph, the program and the pane's title (`◐ claude  paginate.rs`, `$ fish` for a shell at its prompt); `enter` or a click attaches, selects or focuses. `agents`, every agent across sessions (name, state glyph, elapsed, last line of its pane), blocked rows answer `y`/`n` in place like the inbox; `enter` jumps to the pane. Sessions and agents refresh every 2 s while the panel is open. Other sections (commands, git, notifications, system) are later milestones or plugins.
  Note (0.10.0): `workspace` and `agents` are built into the client; a plugin hook for further sections arrives with the first plugin that needs one, and unknown names in `sections` are skipped. `agents` orders blocked, error, working, done, oldest first within a state, with the time since the state changed (`4m12s`); collapsed it reads `◐3 ■1 ✓1`. A tab's branch is `git status` of its focused pane's directory, run off the UI thread at most every 5 s, with `+N` when ahead of upstream. The attached session is listed first. A drag keeps the width between 16 columns and half the terminal; a new client starts at `[panel] width`. The bar shows `PANEL` while the panel has the keys.
- REQ-UI-043: Command palette (`prefix :`): fuzzy search over all bound actions and CLI subcommands.
  Note (0.8.3): no root key by default. `ctrl+p` was bound at the root until 0.8.1, but it is readline's and fish's history key and the usual fuzzy-finder key inside editors, which ADR-008 set out to avoid; `[keys.root] "ctrl+p" = "palette"` restores it.
  Note: actions are listed by name (`femux keys list`) with their description and key; choosing a CLI subcommand types `femux <subcommand> ` into the focused pane. Matching is subsequence-based.
- REQ-UI-044: Inbox overlay (agents plugin): list of panes in `blocked|done|error` with one-key jump and one-key respond.
  Note: `prefix i` (bound by the agents plugin to the client action `inbox`). Rows sort blocked, error, done, newest first, across every session; each shows what the pane's last screen line says. `1`-`9` and `enter` jump to the pane, `j/k` move, `y`/`n`/`a` type that letter plus Enter into a blocked pane, `x` dismisses (sets `agent.state=idle`, `source=hook`), `/` filters, `q`/`escape` close.

### 5.6 Rendering

- REQ-UI-050: Output is diff-based; each frame is wrapped in synchronized output (mode 2026) when the host supports it.
  Note: support is probed at startup with `DECRQM 2026`, together with OSC 10/11/12, `DSR 996` (light/dark), the Kitty keyboard query and DA1.
- REQ-UI-051: Theme from `[theme]` or, when `theme = "terminal"`, from OSC 10/11 queries at startup; light/dark follows mode 2031 when available.
  Note: `terminal` picks the built-in dark or light theme from the mode 2031 answer, else from the OSC 11 background's luminance. Other names come from the `theme` plugin (0.4.0): `dark`, `light`, `high-contrast`, or a path to a JSON file of `#rrggbb` colors over a `base`; unknown keys are errors. A theme the plugin cannot load falls back to `terminal` and says why in the status row. `femux reload` re-applies it.
  Note (0.6.1): mode 2031 reports the desktop's light/dark preference in some terminals (Ghostty), which can differ from the terminal's own colors, so the OSC 11 background's luminance decides and mode 2031 only applies when no background is reported. With a reported background, `terminal` colors the chrome from the terminal itself: text and accent-foreground are its foreground and background, bars and overlays its background tinted 5–8% toward the foreground, muted text halfway between, and accent, warn, ok and error its own palette's blue, yellow, green and red (ANSI 4, 3, 2, 1). Without one, the bundled dark or light theme applies.
- REQ-UI-052: Kitty graphics placements are re-emitted with pane offsets and clipped at pane bounds.
  Note: 0.2.0 implements the geometry and the `a=p` re-emission for `frame.images`; the daemon sends placements from 0.7.0 (GFX-*).
- REQ-UI-053: Mouse support is on by default and fully disableable.
  Note: `[input] mouse = false` disables reporting entirely. A click focuses the pane under it and is forwarded; wheel events are forwarded to the pane (in copy mode they scroll the view); clicking the tab strip selects a tab.
  Note (0.9.0): dragging with the left button selects text in the pane under it and releasing copies the selection the way `y` does in copy mode (OSC 52 on the host plus `clipboard.set`). The selection is linear from the press to the release in reading order, trailing blanks trimmed per line, lines joined with newlines, and stays highlighted until the next key or click. A pane whose program tracks the mouse (frame `mouse`, ADR-023) gets the drag instead, unless `shift` is held, which always selects. With `mouse = false` nothing is selected.

---

## 6. Persistence

- REQ-STORE-001: SQLite in WAL mode at `$XDG_STATE_HOME/femux/<server>.db` (macOS: `~/Library/Application Support/femux/`), file mode 0600, schema versioned with forward migrations.
  Note: the directory is created 0700; the file and its WAL files are 0600. The schema version is `PRAGMA user_version`; a file from a newer femux is treated as unloadable (REQ-STORE-007). Rows are kept per server name, so servers may share a file.
- REQ-STORE-002: Tables: `servers`, `sessions`, `tabs(layout_json)`, `panes(cwd, cmd, env_json, restore, cols, rows, marks)`, `pane_meta`, `scrollback(opt-in)`, `events(ring)`.
  Note: `tabs.layout_json` holds the tree and floats; `panes.marks` the mark letters; `panes.env_json` the environment filtered by `[env] allow`; `scrollback` one deflated chunk per pane; `events` the bus's retained events, so `seq` continues across a restart and `since` still reaches them. A snapshot replaces every row in one transaction.
- REQ-STORE-003: Snapshot on every layout change (debounced 500 ms), every 30 s while dirty, on `SIGTERM`, on `logind PrepareForShutdown` (Linux) and on `SIGTERM` from launchd (macOS).
  Note: "layout change" is any session, tab, layout, spawn, exit or mark event; cwd and metadata changes, and pane output when scrollback is persisted, are saved on the 30 s tick. Snapshots are written whether or not restore is enabled. The logind trigger is best effort: it needs a system bus and logind, takes a delay inhibitor lock when granted, and is otherwise silent. On macOS, launchd's SIGTERM is handled as any SIGTERM. `store.snapshot` forces one (ADR-017 #4).
- REQ-STORE-004: `restore.enabled` defaults to `false`. When true, saved sessions reopen with panes marked `restored` (shell in cwd, no command) unless the pane's `restore` flag is true, in which case `cmd` is re-run.
  Note: a pane whose command is not re-run comes back as a shell and is a shell from then on (`cmd` cleared). Restored panes get their persisted environment on top of the daemon's. The `restored` mark (ADR-017 #2) clears when a process other than the shell runs.
- REQ-STORE-005: Re-run commands start staggered by `restore.stagger_ms` (default 100).
  Note: the n-th re-run command starts n × `stagger_ms` after the daemon opens its store; the pane exists from the start.
- REQ-STORE-006: Persisted scrollback is opt-in and capped at `restore.scrollback_lines` (default 2,000) per pane, stored compressed.
  Note: the pane's last `scrollback_lines` lines of history and screen are stored as text with SGR styles, deflated, and replayed into the pane before its shell starts.
- REQ-STORE-007: A snapshot that fails to load is moved to `session-backups/<ts>.db` and never overwritten.
  Note: the WAL and shared-memory files move with it; a name already taken gets a `-1`, `-2`, … suffix.
- REQ-STORE-008: Restored panes render with a distinct border style until a process runs in them.
  Note: borders next to a restored pane are dashed and drawn in the warning color, and its tab shows `↺`, since a lone pane has no border to restyle. The style clears when a process other than the shell runs (ADR-017 #2).

---

## 7. Security

- REQ-SEC-001: No authentication code in femux. Local trust = same UID. Remote trust = OpenSSH.
- REQ-SEC-002: `clipboard.set` is allowed from any pane; clipboard read requests from panes are denied unless the fg command is in `[clipboard] read_allow`.
  Note (0.9.0): a read (OSC 52 `?`, OSC 5522) is decided when its bytes arrive, from the process in the pane's foreground at that moment, by command name (`read_allow = ["nvim"]`). An allowed read gets the newest clipboard text the daemon has seen (a `clipboard.set` or any pane's write; the daemon holds no clipboard of its own); a denied one gets an empty clipboard (EPERM for OSC 5522). `femux reload` applies the list.
- REQ-SEC-003: Metadata and titles are sanitized for control characters before rendering.
- REQ-SEC-004: DCS passthrough to the host terminal is off by default (`terminal.passthrough = false`).
  Note (0.9.0): off, DCS sequences a pane program emits are dropped inside the VT (sixel is decoded into an image regardless, REQ-GFX-002). On, the daemon forwards each one as `pane.passthrough` and a client showing the pane writes it to its host terminal verbatim, so programs can reach the outer terminal (`DCS tmux; … ST` style wrappers included). `femux reload` applies the setting. OSC 52 writes, Kitty graphics and synchronized output are not passthrough: they are handled by their own requirements.
- REQ-SEC-005: Environment persisted to the store is limited to `[env] allow` (default: `PATH HOME SHELL LANG LC_* TERM EDITOR`).
- REQ-SEC-006: `femux update` verifies a signed checksum before replacing the binary and never restarts a running daemon.
- REQ-SEC-007: Over SSH, notifications are forwarded as events; sound files are never transferred, only names resolved on the client.
  Note (0.6.0): `notify.posted` carries the `[notify]` entry name (`event`), never an action; the client runs its own config's actions for it, so a remote's sound paths are never used.

---

## 8. Plugins

- REQ-PLUG-001: Plugins are workspace crates under `crates/plugins/*`, each behind a Cargo feature, all enabled in release builds, activated at runtime by `[plugins] enabled`.
- REQ-PLUG-002: Three traits: `DaemonPlugin` (event subscriber + metadata writer), `ClientPlugin` (info modules, palette actions, keybindings, overlays), `CliPlugin` (subcommands). A plugin implements any subset.
  Note: the traits live in `crates/femux-plugin` (added to §12), which depends only on `femux-proto`; a daemon plugin runs as a protocol client, in-process or not.
- REQ-PLUG-003: Plugins use only `femux-proto` to talk to the core; a plugin MAY run out-of-process.
- REQ-PLUG-004: Built-in plugin list and their versions: `agents` (0.5), `infobar` (0.4), `notify` (0.5), `git` (0.8), `clipboard` (0.2), `graphics` (0.7), `shell-integration` (0.2), `broadcast` (0.8), `which-key` (0.2), `recorder` (0.8), `hosts` (0.6), `theme` (0.4), `launcher` (0.3).
  Note (0.8.0): `git` worktree tabs: `femux git tab <branch>` makes (or reuses) a worktree at `<repo root>/.worktrees/<branch>`, creating the branch when it does not exist, keeps `.worktrees/` out of `git status` through `.git/info/exclude`, and opens a tab there titled with the branch (or selects the tab already titled so). `femux git tabs` lists the worktrees and their tabs; `femux git rm <branch> [--force]` refuses a worktree with uncommitted changes, closes its tabs and removes it. The repository is the one the command runs in.
  Note (0.8.0): `broadcast`: `prefix B` (or the `broadcast` action) toggles the tab's `broadcast` flag (`tab.set`); while it is on, keys and pastes typed into any pane go to every tiled and floating pane of the tab, mouse input does not, and borders take the warning color. `femux broadcast on|off [tab]` sets it, defaulting to the tab of `$FEMUX_PANE`.
  Note (0.8.0): `recorder`: `prefix R` (or the `record` action) toggles the pane metadata `recorder.on`; the daemon-side plugin then subscribes to `pane.output` for that pane and writes an asciinema v2 cast to `<state dir>/recordings/femux-<unix time>-<pane>.cast`, naming it in `recorder.file` while recording and `recorder.last` after. The cast's size is the pane's at the start. `femux record [pane] [file] [--duration s]` records in the foreground until interrupted or the duration ends.

---

## 9. Agents plugin

### 9.1 State model

`agent.state ∈ {idle, working, blocked, done, error, exited}`, stored in pane metadata as `agent.state`, `agent.source ∈ {hook, osc, process, heuristic}`, `agent.since`, `agent.name`.

### 9.2 Detection tiers

- REQ-AGT-001 (tier 1, authoritative): `femux pane set agent.state=<s>` from agent hooks, and OSC `9;4` progress (0 → idle, 1–3 → working) plus private `OSC 7777;state=<s>`.
- REQ-AGT-002 (tier 2): from `pane.fg_changed` and process state. Linux: `/proc/<pid>/stat` state and `wchan`; a fg process sleeping in a tty read with no recent output ≥ `agents.blocked_after_ms` (default 1500) → `blocked`. Shell returning to foreground → `exited` then `idle`. macOS: `proc_pidinfo` thread states, best-effort, marked `source=process` with lower confidence.
  Note: macOS is hook-only: tier 2 is not implemented there (spike §16-3 could not be run; decided 2026-10-11). macOS panes get tier 1 and tier 3 only.
- REQ-AGT-003 (tier 3): output heuristics (silence after burst, prompt-like last line) only when tiers 1–2 give nothing; always `source=heuristic`.
  Note: the agents plugin watches panes whose foreground command is a known agent (`claude`, `codex`, `opencode`, `cursor-agent`, `aider`, `gemini`, `goose`, `amp`, `crush`, `qwen`) or that carry `agent.name`. Tier 3 runs only where tier 2 does not (macOS, or tier 2 off); silence means no output for `agents.blocked_after_ms`, and a last screen line ending in `?` or `:`, a lone `>`, or containing `(y/n)`, `[y/N]`, `(yes/no)` or `press enter` reads as `blocked`, any other as `done`. A bare `agent.state` written by someone else counts as tier 1 with `source=hook`; the plugin adds `agent.source` and `agent.since` and ignores values that are not states. Every state the plugin accepts also writes `agent.evidence`, the one-line reason `femux agent explain` prints. After `exited`, `idle` follows 1 s later.
- REQ-AGT-004: Higher tiers override lower tiers; a lower tier MUST NOT override a higher-tier state younger than `agents.hold_ms` (default 5000).
- REQ-AGT-005: `femux integration install <claude-code|codex|opencode|cursor>` writes the agent's hook config to call tier-1 commands; `uninstall` reverts it.
  Note: shipped in 0.2.0 with the CLI. Hooks call `femux integration hook <agent> [state]`, which sets `agent.state|source|name|since`. Claude Code (`~/.claude/settings.json`) and Codex (`notify` in `~/.codex/config.toml`) follow their documented formats; the OpenCode plugin file and Cursor `hooks.json` are best effort until 0.5.0 tests them end to end.
  Note (0.5.0): the Claude Code hook is tested end to end against the daemon (`femux agent wait` returns on `Stop`). OpenCode and Cursor stay best effort: their hook formats are written as documented, but no test runs those agents.

### 9.3 Commands

`femux agent list`, `femux agent wait <pane> --until <state> [--timeout]`, `femux agent prompt <pane> "text" [--wait]`, `femux pane cat <pane> [--lines N]`, `femux agent explain <pane>` (prints the evidence behind the current state).

Note: `<pane>` defaults to `$FEMUX_PANE` for `wait` and `explain`. `list` shows panes with an `agent.state` (`--all`: every pane). `--until` takes a state or a comma-separated list (default `done,blocked,error`); `--timeout` is in seconds and exits 1 when it runs out. `prompt` sends the text as a paste followed by Enter; with `--wait` it first waits for the agent to start working when it was `done|blocked|error`, then for `done|blocked|error|exited`.

### 9.4 Notifications (`notify` plugin)

```toml
[notify]
agent_done    = { sound = "~/.config/femux/done.wav", desktop = true, only_when_hidden = true }
agent_blocked = { sound = "bell" }
agent_error   = { desktop = true, webhook = "https://ntfy.sh/mytopic" }
pane_exited   = { desktop = false }
```

- REQ-NOTIFY-001: Actions: `sound` (file path or `"bell"`), `desktop` (OSC 777 / OSC 99 / native fallback), `webhook` (POST JSON).
- REQ-NOTIFY-002: `only_when_hidden` fires only when no attached view shows the pane.
- REQ-NOTIFY-003: Notifications originating on a remote daemon are delivered as events and executed on the client.
  Note: the notify plugin's daemon side posts `notify.post` with `event` set to the `[notify]` entry (`agent_done|agent_blocked|agent_error` when `agent.state` changes to that state, `pane_exited` when a pane's process exits), and only for entries the config has. A pane exit is posted without `pane_id`, since the pane is gone. `sound` and `desktop` run on the client from `notify.posted`, using the client machine's `[notify]`. `sound` is `"bell"` (BEL to the host terminal) or a file played with `afplay`, `paplay`, `pw-play` or `aplay`, falling back to the bell. `desktop` uses OSC 99 on kitty, OSC 777 on ghostty, WezTerm, foot, contour and rxvt, and otherwise `notify-send` (`osascript` on macOS). `webhook` runs on the daemon's side and POSTs `{event, title, level, pane_id, ts, shown}` as JSON. `only_when_hidden` reads `shown` from `notify.posted`, which the daemon sets when some attached view shows the pane.

---

## 10. Configuration

`~/.config/femux/config.toml`, validated on load with line-numbered errors. Reload with `femux reload`.

Note: `femux reload` validates the file, then the daemon applies `keep_alive`, `[terminal] scrollback_lines` (for new panes), `[restore]`, `[env] allow` and `[notify]` (both sides of the notify plugin re-read it), and attached clients re-read their keys, prefix, mouse, borders, tab strip, info bar, which-key delay and theme, resizing their view when the tab strip or bar moving changes the pane area. `[daemon] name` and `[plugins] enabled` take effect on the next start. An invalid file changes nothing (ADR-017 #3).

```toml
[daemon]
name = "default"
keep_alive = false

[input]
prefix = ["ctrl+space"]
mode_keys = "vi"
prefixless_nav = false        # alt+h/j/k/l

[layout]
scheme = "bsp"                # bsp | master | scroll
gaps = 0
borders = "single"            # single | heavy | double | spaces

[bar]
position = "bottom"           # top | bottom
modules = ["agents", "git", "cpu", "mem", "net", "clock"]
interval_ms = 1000
running_indicator = "●"

[panel]
open = false
width = 28
sections = ["workspace", "agents"]

[terminal]
scrollback_lines = 10000
kitty_graphics = true
sixel = true
passthrough = false

[restore]
enabled = false
scrollback = false
scrollback_lines = 2000
stagger_ms = 100

[env]
allow = ["PATH", "HOME", "SHELL", "LANG", "LC_*", "TERM", "EDITOR"]

[clipboard]
read_allow = []

[theme]
name = "terminal"             # terminal | <bundled name> | path to json

[agents]
enabled = true
blocked_after_ms = 1500
hold_ms = 5000

[notify]
agent_done = { sound = "bell", desktop = true, only_when_hidden = true }

[plugins]
enabled = ["agents", "infobar", "notify", "clipboard", "shell-integration", "which-key", "launcher"]

[keys.prefix]
"s" = "split down"
"v" = "split right"
# any action name from `femux keys list`
```

Session files: `~/.config/femux/sessions/<name>.toml`.

```toml
name = "writing"
root = "~/writing"
tabs = 4

[[tab]]
title = "editor"
command = "nvim"
restore = true

[[tab]]
command = "git status"
cwd = "~/writing/notes"
```

- REQ-CFG-001: Tabs beyond the listed `[[tab]]` entries open as plain shells in `root`.
  Note: `tabs` counts every tab; with fewer `[[tab]]` entries the rest are shells in `root`, and with none at all the session still opens one. Paths in `root` and `cwd` may start with `~`.
- REQ-CFG-002: `femux launch writing` creates the session from its file; `femux launch writing --tabs 4 --dir ~/writing -- nvim 'git status'` is equivalent and MAY write the file with `--save`.
  Note: `launch` is the `launcher` plugin's subcommand. `<arg>` is a path when it contains `/`, ends in `.toml` or names a file, else `sessions/<arg>.toml`; the session is named by the file's `name`, else its stem. The flag form applies when `--tabs`, `--dir` or `-- commands` is given; each command after `--` is one tab. A session that already exists is attached to, not recreated. `launch` starts the daemon if needed and attaches afterwards when run from a terminal outside femux; `--detach` and `--json` do not attach.
- REQ-CFG-003: Unknown keys are errors, not warnings.
  Note: every table of `config.toml` and of session files rejects unknown keys and names the line; enum-like values (`scheme`, `borders`, positions, `mode_keys`) reject unknown values the same way. A bad config stops every command. `femux config check` validates the config and every session file; `femux config edit` seeds a missing config with the defaults from `spec/config.example.toml`.

---

## 11. CLI

`femux` (attach or start), `femux daemon [--foreground]`, `femux attach [session] [--host h] [--stdio]`, `femux ls`, `femux new <name>`, `femux kill <name>`, `femux launch <name|file>`, `femux tab new|close|rename|select`, `femux pane split|close|focus|resize|info|set|cat|signal|respawn`, `femux layout save|load|list|export`, `femux agent …`, `femux integration install|uninstall`, `femux keys list|explain`, `femux config check|edit`, `femux reload`, `femux reload`, `femux update [--check]`, `femux hosts add|list|rm`, `femux notify test`, `femux graphics list [pane]`, `femux git tab|tabs|rm`, `femux broadcast on|off`, `femux record`.

- REQ-CLI-001: Every subcommand has `--json` output.
- REQ-CLI-002: Inside a pane, commands default to that pane (`$FEMUX_PANE`).
  Note: the pane's tab and session default the same way (`pane.info`); outside femux, `tab new` needs `--session` unless exactly one session exists. `tab select` switches every attached client of the session (ADR-016 #8); tab indexes are resolved with the read-only `session.tabs` (ADR-016 #11).
- REQ-CLI-003: Shell completions for bash, zsh, fish are generated from the CLI definition.
  Note: `femux completions <shell>`, including the subcommands plugins add.

Note (0.6.0): `hosts` is enabled by default. `femux daemon --ensure` starts a daemon only if none runs and prints its socket (what `--forward` hosts ask the remote for).

Note (§11): `launch`, `config check|edit` and `reload` arrive in 0.3.0; `agent`, `update`, `hosts` and `notify` with their milestones (0.5.0, 1.0, 0.6.0, 0.5.0). `integration install` ships early (0.2.0) because its hook target, `femux pane set`, exists.

---

## 12. Crate layout

```
femux/
  Cargo.toml                workspace
  spec/                     this file, protocol/*.json, config.example.toml, adr/, conformance.md
  crates/
    femux-proto/            generated types, framing, handshake
    femux-vt/               libghostty-vt binding (build.rs invokes zig), cell diff
    femux-daemon/           pty, vt, query proxy, session model, views, store, bus, graphics, socket
    femux-client/           modes, layout engine, tab strip, infobar host, navigators, renderer (termwiz)
    femux-cli/              the `femux` binary; embeds client and daemon
    femux-plugin/           plugin traits and the daemon-plugin runner (REQ-PLUG-002)
    femux-config/           config.toml and session-file parsing (SPEC §10)
    femux-test/             headless harness
    plugins/
      agents/ infobar/ notify/ git/ clipboard/ graphics/ shell-integration/ broadcast/ which-key/ recorder/ hosts/ theme/ launcher/
```

- REQ-BUILD-001: `cargo build --release` on Linux and macOS with a pinned Zig (`rust-toolchain.toml` + `zig-version`) produces one binary.
- REQ-BUILD-002: MSRV is the current stable at the time of 0.1.0 and is pinned.
- REQ-BUILD-003: CI runs `cargo clippy -D warnings`, `cargo test --workspace`, and the conformance suite on both platforms.

---

## 13. Milestones and branches

Each minor version is one feature set and one GitButler branch named `v0.N.0`. Patch branches `v0.N.1`, `v0.N.2`, … stack on it and hold only review fixes and hotfixes. A branch is done when every requirement it lists has a passing test in `spec/conformance.md`.

| Version | Requirements | Acceptance |
| --- | --- | --- |
| 0.1.0 | ARCH-*, PTY-*, VT-001–005, QRY-*, SESS-*, VIEW-*, EVT-*, LIFE-001–002, PROTO-*, BUILD-*, plus a bare viewer client (attach to one session, show one pane, type into it, `prefix d` to detach) | Harness can create a session, split, run `ls`, assert on the cell grid, detach and reattach with identical screen |
| 0.2.0 | UI-001–012, UI-020–021, UI-043, UI-050–053, CLI-*, plugins `clipboard`, `shell-integration`, `which-key` | Daily-driver for local use; all default bindings work; copy mode search passes tests |
| 0.3.0 | STORE-*, CFG-*, plugin `launcher` | Kill daemon, restart, layout returns; `femux launch` from file and flags |
| 0.4.0 | UI-030–033, UI-040–042, plugins `infobar`, `theme` | Bar on all four sides with every built-in module; navigators and side nav |
| 0.5.0 | AGT-*, NOTIFY-*, UI-044, plugins `agents`, `notify` | Claude Code hook reports states; chime plays on done; `femux agent wait` returns |
| 0.6.0 | SEC-007, plugin `hosts`, `attach --stdio`, socket forwarding | Attach over SSH; detach survives link drop; remote notifications play locally |
| 0.7.0 | GFX-*, plugin `graphics` | `kitten icat` and `mpv --vo=kitty` work inside a pane; images survive reattach |
| 0.8.0 | SESS-004 marks UI, float UI, plugins `git`, `broadcast`, `recorder` | Worktree tabs, broadcast typing, asciinema recording |
| 0.9.0 | SEC-002 (`read_allow`), SEC-004 (`passthrough`), UI-053 drag selection, VT-002 cursor shapes (ADR-023) | A pane in `read_allow` reads the clipboard back and one outside it gets nothing; `passthrough = true` lets a DCS reach the host; drag-and-release copies text; neovim's insert cursor is a bar |
| 0.10.0 | UI-020, UI-030–033 one bar; UI-042 panel with `workspace` and `agents`; `[bar]` and `[panel]` config | One row carries session, mode, tabs and modules at top or bottom and degrades right side first; `prefix b` opens a left panel whose sections collapse, `ctrl+b` focuses it, its width follows config or a drag; mocks in `spec/design/` |
| 1.0.0 | Scoped in a release spec (`spec/RELEASE.md`), written when a release is ready; the protocol is not frozen before then | Set by the release spec |

---

## 14. Testing

- REQ-TEST-001: `femux-test` starts a daemon on a temp socket, connects as a client, and exposes `screen(pane) -> Grid` (JSON cell grid) plus `screen_text(pane) -> String` for debugging.
- REQ-TEST-002: Every REQ ID in a milestone appears in `spec/conformance.md` with at least one test name; CI fails on an unmapped requirement.
- REQ-TEST-003: Protocol tests validate every message against its JSON Schema in both directions.
- REQ-TEST-004: Golden tests for the VT use recorded byte streams and expected grids; the same fixtures run against libghostty-vt in CI.
- REQ-TEST-005: Client rendering tests run against a virtual terminal (vt100 crate) and compare rendered output.
- REQ-TEST-006: Agent detection tests feed scripted process states and output into the daemon and assert on `agent.state` and `agent.source`.

---

## 15. Decisions (ADRs)

| ADR | Decision | Rationale |
| --- | --- | --- |
| 001 | Rust; libghostty-vt via Zig at build time | Ghostty-identical emulation; strong binding ecosystem; pure-Rust VT as later fallback |
| 002 | Frames over bytes | Removes double emulation; enables non-terminal clients; scrollback served by daemon |
| 003 | OpenSSH only for remote | No auth code to get wrong; users keep keys, MFA, agent forwarding |
| 004 | SQLite for state | Transactional snapshots, migrations, inspectable with standard tools |
| 005 | Compiled-in plugins, config-enabled | No TPM, no supply-chain surface, no ABI; PRs via issues |
| 006 | Tiered agent detection, hooks first | Screen scraping is the last resort, not the design |
| 007 | `termwiz` for the client | Kitty keyboard and graphics support; drives multiplexing in WezTerm |
| 008 | `ctrl+space` prefix, prefix-less nav off, no root keys | Avoids Neovim, shell and agent conflicts by default |
| 009 | Restore off by default; opt-in per tab | Re-running commands is a security decision the user makes explicitly |
| 010 | MIT license | Maximum adoption, no copyleft friction for native clients |
| 011 | Linux reference, macOS supported | `/proc` is the only reliable fg-process state source |
| 012 | JSON cell grid for test assertions | Agents can diff structured output; text dumps for humans |
| 013 | `femux reload` over file watching | Explicit, scriptable, no half-applied configs |
| 014 | 0.1.0 ships a bare viewer | A human can sanity-check the daemon before the real client exists |

Note (ADR-007): termwiz has no Kitty graphics output. `Change::Image` emits only iTerm2 images and panics on cropped or raw RGBA images; `escape::apc::KittyImage` omits the `ESC \` terminator and does not chunk payloads. The client writes Kitty APCs itself (termwiz encodes the keys; the client appends ST and chunks at 4096 bytes). Spike §16-2 confirmed clipping on resize this way.

---

## 16. Risks and spikes (0.1.0)

| Risk | Spike | Exit criterion |
| --- | --- | --- |
| libghostty-vt C ABI may not expose per-frame dirty cells | Build `femux-vt` first, benchmark diff extraction on a 200×60 grid under `yes` | ≤ 2 ms per frame on a laptop |
| `termwiz` Kitty graphics placement API gaps | Render one image at a pane offset in a throwaway client | Image clipped correctly on resize |
| macOS fg process wait-state detection | Prototype tier 2 on macOS with Claude Code | Blocked state detected within 2 s, else document hook-only on macOS |
| Zig toolchain friction for contributors | Document install in `CONTRIBUTING.md`; cache in CI | Fresh clone builds in one documented command |

---

## 17. Instructions for the implementing agent

1. Work one branch at a time (`v0.1.0` first). Do not start `v0.N+1.0` until every requirement listed for `v0.N.0` has a passing test.
2. One commit series per requirement ID; commit messages start with the ID (`REQ-PTY-002: track foreground process group`).
3. Generate `femux-proto` from `spec/protocol/*.json`; if a message needs a field the schema lacks, change the schema first and record why in `spec/adr/`.
4. Never add a dependency on a terminal, UI or agent library inside `femux-daemon`.
5. When a requirement is ambiguous, choose the simplest reading, implement it, and add a one-line note under the requirement in this file prefixed `Note:`.
6. Update `spec/conformance.md` in the same commit as the test.
7. Keep `cargo clippy -D warnings` clean and `cargo test --workspace` green on every commit.
8. Review fixes go on the current patch branch (`v0.N.x`), never on the feature branch.
