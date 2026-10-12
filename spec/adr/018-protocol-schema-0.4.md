# ADR-018: Protocol schema additions for 0.4.0

Date: 2026-10-12 · Status: accepted · Requirements: PROTO-010, UI-033, UI-040, NOTIFY-001 (groundwork)

The navigators and the info bar needed two additions. Both are additive (REQ-PROTO-012).

| # | Change | Why |
| --- | --- | --- |
| 1 | `session.list` entries carry `last_activity` (ms since epoch): the newest output from or input to any pane of the session, else its creation time. | UI-040's session navigator shows each session's last activity; nothing else in the protocol records it. |
| 2 | New family `notify`: request `notify.post {pane_id?, title, level?}` and event `notify.posted {pane_id?, title, level}`, `level` one of `info`, `done`, `warn`, `error`. | UI-033's `notifications` module shows notification titles, and the notify plugin that will decide what to post arrives in 0.5.0. Defining the event now lets the module ship and be tested (approved 0.4.0 default); the 0.5.0 plugin posts through the same request. The daemon only relays: it validates the pane and emits the event to every subscriber of the pane's session. |
