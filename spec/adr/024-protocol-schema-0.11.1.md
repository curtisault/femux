# ADR-024: Protocol schema additions for 0.11.1

Date: 2026-10-14 · Status: accepted · Requirements: PROTO-010, LOG-003

0.11.1 adds a log and a debug mode (SPEC §3.9). A running daemon must be switched into debug mode without a restart, and the clients attached to it must follow. All changes are additive (REQ-PROTO-012). Both live under the existing `server` feature, so the handshake does not change.

| # | Change | Why |
| --- | --- | --- |
| 1 | New request `server.debug {level?: "warn"\|"debug"\|"trace"}` answered by `{level, path, since}`. Without `level` it only reports. | `femux debug on\|off\|status` and a client attached with `--debug` need to set and read the daemon's level; the log's path lets `femux debug path` answer for the daemon's server rather than guessing it. |
| 2 | New event `server.debug_changed {seq, ts, level}`, emitted when `server.debug` changes the level. | REQ-LOG-003 has every attached client log at the daemon's level, and the bar shows `DEBUG` while it is on. An event reaches clients that did not make the change. |
| 3 | New def `LogLevel`, shared by both. `error` and `info` are not settable: `warn` is the floor every process always writes, and `info` is part of debug mode rather than a mode of its own. | Keeps the user-facing choice to the three levels `[debug] level` accepts. |
