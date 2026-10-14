# ADR-023: Protocol schema additions for 0.9.0

Date: 2026-10-14 · Status: accepted · Requirements: PROTO-010, VT-002, UI-053, SEC-004

0.9.0 gives clients the pane's cursor style, its mouse mode and, when the user opts in, the DCS sequences its program writes. All changes are additive (REQ-PROTO-012): older clients ignore the new fields and never subscribe to anything new by name.

| # | Change | Why |
| --- | --- | --- |
| 1 | `Cursor` gains optional `blink`. | `shape` alone loses DECSCUSR's blinking variants; a client sets its host cursor to the focused pane's shape and blink (REQ-VT-002), so neovim's insert-mode bar shows outside femux. |
| 2 | `PaneFrame` gains optional `mouse`: the pane's program has a mouse tracking mode on. A pane frame is sent when it flips even if no cell changed. | A drag over a pane either selects text or goes to the program (REQ-UI-053). Only the daemon's terminal knows the mode, and the client decides per press, so the state rides with the pane's frames. |
| 3 | New event `pane.passthrough {pane_id, data}`: one DCS sequence the pane's program wrote, base64, emitted only while `[terminal] passthrough = true`, never retained for `since`. | REQ-SEC-004 makes passthrough opt-in; REQ-VT-002 keeps raw bytes out of frames, so the sequences travel as an event. Retaining them would replay stale terminal commands to a reattaching client. |
