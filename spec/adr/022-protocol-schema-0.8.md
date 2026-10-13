# ADR-022: Protocol schema additions for 0.8.0

Date: 2026-10-13 · Status: accepted · Requirements: PROTO-010, SESS-004, UI-011, ARCH-001

0.8.0 adds a marks overlay, floating-pane controls and the `broadcast` and `recorder` plugins. All changes are additive (REQ-PROTO-012).

| # | Change | Why |
| --- | --- | --- |
| 1 | `pane.mark` takes `clear`; `pane.marked` reports `cleared`. | The marks overlay deletes marks (REQ-SESS-004). |
| 2 | New request `tab.set {tab_id, broadcast?, floats_hidden?}`, event `tab.flags {tab_id, broadcast, floats_hidden}`, and both flags in `TabSnapshot`. | Broadcast and hidden floats are tab state every client and the CLI must agree on. The daemon applies broadcast to `pane.input` (each pane gets the input encoded for its own modes), and hidden floats leave `visible_rects`, so frames and focus follow without client logic. |
| 3 | New event `pane.output {pane_id, data}`: the bytes a pane's program wrote, base64. It reaches only subscribers that name it in `kinds` and is never retained for `since`. | The recorder writes asciinema casts from the raw stream; ordinary subscribers and the replay ring would drown in it. The daemon only encodes output while someone subscribes to it. |
| 4 | `pane.info` reports the pane's `cols` and `rows`. | A recording's header states the terminal size. |
