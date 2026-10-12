# ADR-019: Protocol schema additions for 0.5.0

Date: 2026-10-12 · Status: accepted · Requirements: PROTO-010, ARCH-001, AGT-001, AGT-002, NOTIFY-001, NOTIFY-002

Agent tracking and notifications live in the `agents` and `notify` plugins (REQ-ARCH-001: no agent code in the daemon). The plugins needed four things only the daemon sees. All are additive (REQ-PROTO-012).

| # | Change | Why |
| --- | --- | --- |
| 1 | New event `pane.progress {pane_id, state, value?}` for OSC 9;4 (state 0 remove, 1 set, 2 error, 3 indeterminate, 4 pause). | AGT-001 tier 1 reads progress reports (0 → idle, 1–3 → working). The daemon's terminal parser is the only thing that sees them; it reports them neutrally and the agents plugin interprets them. |
| 2 | New event `pane.report {pane_id, data}` for the private OSC 7777 (`data` is what follows `7777;`). | AGT-001's `OSC 7777;state=<s>`, reported the same way. libghostty-vt does not parse 7777, so femux-vt scans for it. |
| 3 | `pane.info` reports `last_output`, ms since epoch of the pane's newest output. | AGT-002 tier 2 marks a pane `blocked` only after `agents.blocked_after_ms` without output; tier 3's "silence after a burst" needs the same. |
| 4 | `notify.post` and `notify.posted` take an optional `event` (the `[notify]` entry, e.g. `agent_done`); `notify.posted` reports `shown`: whether some attached view showed the pane when it was posted. | Clients run the actions configured for `event` (NOTIFY-001/003). NOTIFY-002's `only_when_hidden` depends on every attached view, which only the daemon knows. |
