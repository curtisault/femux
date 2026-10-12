# ADR-016: Protocol schema additions for 0.2.0

Date: 2026-10-11 · Status: accepted · Requirements: PROTO-010, UI-011, UI-012, UI-020, UI-021, PLUG-004 (`clipboard`), copy mode (§5.1)

Building the TUI client, the CLI and the `clipboard` plugin needed data the 0.1 schema (ADR-015)
did not carry. Each addition is the smallest field that lets the client stay a pure consumer of
the protocol (REQ-ARCH-002). All are additive; 0.1 clients keep working because unknown fields
are ignored (REQ-PROTO-012).

| # | Change | Why |
| --- | --- | --- |
| 1 | `pane.info` gains `pid` (the pane's own process) and optional `cmd` (what it was spawned with). | UI-020's running indicator is "fg process ≠ the shell", which the daemon identifies by pid (REQ-PTY-004 fix in 0.1.1); UI-012 layout templates serialize `cmd` per pane. |
| 2 | `TabSnapshot` gains optional `custom_title` (the `tab.rename` value). | `title` is derived (UI-021); the client re-derives titles as panes change and must know when a user rename takes precedence. |
| 3 | `scrollback.read` answers with `first_line`, the oldest readable line. | Copy mode's `gg` and scroll bounds need the history size; a read that skips lines outside history cannot tell the client which ones it skipped. |
| 4 | New event `clipboard.changed {text, source: "pane" \| "client", pane_id?}`, emitted for `clipboard.set` and for OSC 52 writes from pane programs. | The daemon owns no clipboard; the `clipboard` plugin in each attached client copies the text to its host terminal (OSC 52). Pane OSC 52 writes are allowed from any pane (REQ-SEC-002); reads are not answered. |
| 5 | `layout.float` on a pane that already floats updates its `rect` instead of failing. | UI-011 mouse drags move and resize floats; `pane.resize` covers size only. |
| 6 | The daemon serves the `clipboard` feature from 0.2.0 (ADR-015 #17). | The behaviour is the event above. |
| 7 | `pane.info` gains `session`. | A CLI running inside a pane (REQ-CLI-002) needs the session for `tab new` without walking every session. |
| 8 | `tab.select.view_id` is optional: without it every attached view of the tab's session switches, one `tab.selected` per view. | `femux tab select` has no view of its own. |
| 9 | `layout.focus.view_id` is optional: then `pane_id` is required and `dir` is relative to that pane. | `femux pane focus l` inside a pane. |
