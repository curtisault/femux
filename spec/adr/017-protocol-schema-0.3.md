# ADR-017: Protocol schema additions for 0.3.0

Date: 2026-10-12 · Status: accepted · Requirements: PROTO-010, STORE-003, STORE-004, STORE-008, CFG-002, ADR-013

Persistence, restore and `femux reload` needed four additions. All are additive (REQ-PROTO-012).

| # | Change | Why |
| --- | --- | --- |
| 1 | `tab.create` takes optional `restore` (default false). | STORE-004 re-runs a pane's `cmd` on restore only when its `restore` flag is set; session files set it per tab (CFG-002), and ADR-009 makes re-running a command an explicit choice. |
| 2 | `pane.info` reports `restore` and `restored`. | `restored` marks a pane that came back from a snapshot as a plain shell, until a command runs in it; the client draws it with a distinct border (STORE-008). Clients clear the mark themselves on `pane.fg_changed` to a pid other than the pane's own. |
| 3 | New request `server.reload {}` and event `config.reloaded {}`. | ADR-013: config changes apply on `femux reload`. The daemon re-reads its file and applies what can change while running; clients re-read their own part when they see the event. An invalid file changes nothing. |
| 4 | The daemon serves the `store` feature (ADR-015 #17): `store.snapshot` writes a snapshot now and answers its time. | STORE-003's triggers are internal; the request lets tools and tests force one. |
