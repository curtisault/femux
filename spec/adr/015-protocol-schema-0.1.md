# ADR-015: Protocol schema 0.1 refinements to the §4.4 catalog

Date: 2026-10-11 · Status: accepted · Requirements: PROTO-010, PROTO-011, PROTO-020, SESS-002, VIEW-002, QRY-002

`spec/protocol/*.json` is the first machine-readable version of the §4.4 catalog. Writing it
forced the decisions below. Each is the simplest reading that keeps the catalog's intent.

| # | Change | Why |
| --- | --- | --- |
| 1 | A response's `kind` is its request's kind; `hello` answers with `hello_ok`; failures use kind `error`. | PROTO-011 fixes the id but not the kind; reusing the request kind lets a client decode a response without remembering what it sent. |
| 2 | Responses the catalog shows as arrays are wrapped in a map (`session.list` → `{sessions}`). | PROTO-011 requires `body` to be a map. |
| 3 | New request `view.resize {view_id, cols, rows}`; `session.attach` takes optional `cols`, `rows`. | VIEW-002 sizes tabs from each client's view, and the catalog had no way to report a view's size after `hello`. |
| 4 | `tab.create` also returns the new tab's `pane_id`. | Every caller needs it next and would otherwise have to `layout.get`. |
| 5 | `hello.term` is optional; `TermCaps` gains optional `fg`, `bg`, `cursor`, `name`. | The CLI and plugins have no terminal. QRY-002 answers OSC 10/11/12 and XTVERSION from the attached client's host terminal, which needs these values. |
| 6 | Floating panes are listed in a tab's `floats` beside the tiled `tree`; the tree's nodes are `split` and `pane`. `layout.get`, `layout.apply` and `layout.changed` carry `floats`. | A float has no position in the split tree; keeping it out of the tree keeps BSP operations total. SESS-002's node kinds are all still represented. |
| 7 | `split.ratio` is `{num, den}`, the first child's share; splits have exactly two children. | SESS-002 asks for rationals; BSP splits are binary. |
| 8 | `dir: "h"` puts children side by side (new pane right), `"v"` stacks them (new pane below); `preselect` is a direction overriding that side. | Matches tmux's `-h`/`-v`; UI-010 preselection picks a side. |
| 9 | Every event body carries `seq` and `ts`. | EVT-001; the envelope is fixed at `{id, kind, body}`. |
| 10 | Features are message families (the kind's prefix). A request whose family is not in both the client's and the daemon's `features` gets `feature_missing`; the connection stays open. | PROTO-020. |
| 11 | `Cell.width` is 1 or 2; the column after a wide character is not sent, and runs advance `x` by each cell's width. | Keeps runs free of placeholder cells. |
| 12 | Scrollback line 0 is the top of the screen; negative lines are history (-1 newest). | Stable as history is trimmed from the top. |
| 13 | `pane.resize` applies to floating panes; tiled panes are sized by the layout. | Tiled sizes are derived (VIEW-002); a direct resize would fight the layout. |
| 14 | `ImagePlacement` is `{id, pane_id, x, y, cols, rows}`; always empty before 0.7.0. | Placeholder until GFX defines it. |
| 15 | `pane.set_meta` `set` and `unset` are optional. | Either alone is a valid change. |
