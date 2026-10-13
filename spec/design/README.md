# Design mocks

Visual targets for the TUI client. Each image is one artboard from the femux UI canvas;
the requirement IDs say which parts of SPEC.md it illustrates. Mocks show intent, not
pixel-exact output: fonts, colors and glyphs follow the user's terminal and theme.

| Image | Shows | Requirements |
| --- | --- | --- |
| `main.png` | Tab strip with running/urgency/zoom markers, BSP layout (one wide pane, two stacked), per-pane header with agent name, cwd, state, source, bottom info bar with session, mode, agent counts, git, cpu, mem, net, clock | UI-020, UI-021, UI-030, UI-032, AGT-001–004, SESS-002 |
| `which-key.png` | Overlay after `ctrl+space`: every prefix-table chord grouped by purpose, info bar showing PREFIX mode. The keys predate the 0.8.3 re-key; §5.1 is authoritative (`s`/`w` navigators, `-`/`\|` splits, `e` swap, `n`/`N` tabs, `p` paste, `space` sticky normal) | UI-002, UI-003, §5.1 default bindings |
| `inbox.png` | Agent inbox overlay: rows for blocked, error and done panes across sessions and machines, one-key answer for approvals, source shown per row, state counts in footer | UI-044, AGT-001–004, NOTIFY-002 |
| `session-nav.png` | Full-screen session navigator: tree of sessions and tabs with agent summaries, remote and restored sessions marked, live preview of the selected pane | UI-040, UI-041, STORE-008, §6 restore |
| `sidebar.png` | Info bar on the left (vertical stacking, `remote:` module, notification queue), side nav open, zoomed pane with Z marker | UI-030, UI-032, UI-033, UI-042, SESS-005 |

## Conventions used in the mocks

- Agent states: `◐` working (blue), `■` blocked (amber), `✓` done (green), `✗` error (red). Glyphs and colors are theme tokens, not fixed.
- Pane header right side shows the state's `source` (`hook`, `osc`, `process`, `heuristic`) so a user can tell authoritative from inferred.
- Tab strip: `●` running process, `!` needs attention, `Z` zoomed.
- Active pane border uses the accent color; blocked panes use the warning color; idle panes use the muted border.

## Source

The editable canvas lives in claude.ai (not in this repo). Re-export PNGs here when it changes
and update this table.
