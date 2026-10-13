# ADR-020: Protocol schema additions for 0.6.0

Date: 2026-10-12 · Status: accepted · Requirements: PROTO-010, PROTO-003, UI-032, ARCH-001

0.6.0 attaches over SSH. The transport needs no protocol change: `femux attach --stdio` copies the same frames between the SSH channel and the daemon's socket (REQ-PROTO-003). One addition is needed for `remote:` info bar modules, which show the daemon host's state. It is additive (REQ-PROTO-012).

| # | Change | Why |
| --- | --- | --- |
| 1 | New family `plugin`: request `plugin.publish {plugin, data}` and event `plugin.data {plugin, data}`, `data` a string map. The daemon relays it to every subscriber without reading it. | REQ-UI-032: a `remote:` module renders on the daemon host. The `hosts` plugin samples that host and publishes the values; the client renders them. A generic relay keeps host statistics out of the daemon (REQ-ARCH-001) and lets other plugins publish later. |
