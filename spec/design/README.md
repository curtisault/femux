# Design mocks

Visual targets for the TUI client. Each image is one artboard from the femux UI canvas;
the requirement IDs say which parts of SPEC.md it illustrates. Mocks show intent, not
pixel-exact output: fonts, colors and glyphs follow the user's terminal and theme.

## 0.10.0: one bar and the panel

0.10.0 (SPEC §13) redraws the chrome around the panes: the tab strip and the info bar
become one row at the top or bottom (REQ-UI-020, REQ-UI-030–033, `[bar]`), and the left
side becomes a collapsible, focusable panel of stacked sections, `workspace` and `agents`
first (REQ-UI-042, `[panel]`). Drawn 2026-10-14 at 5120×3200:

| Image | Shows | Requirements |
| --- | --- | --- |
| `bar-bottom.png` | The one bar at the bottom: session chip and mode, tab strip with `●`/`!`/`Z` markers, modules on the right; panel hidden; three panes (working agent, blocked agent, shell) | UI-020, UI-030–033 |
| `bar-top.png` | The same scene with `[bar] position = "top"` | UI-030 |
| `panel.png` | Panel open, unfocused: `workspace` expanded (sessions → tabs → panes; tab rows carry `●`/`!` and the branch with its ahead count, pane rows the agent glyph, program and the pane's title, `$ fish` for a shell; other sessions collapsed, a remote one as `host:name`), `agents` collapsed to its header with its state counts, the drag handle on the right edge, panes narrower beside it | UI-042, AGT-001–004 |
| `narrow.png` | A 100-column terminal: modules in short form, one dropped, a long tab title truncated, tab numbers intact | UI-020, UI-030 |

## Before 0.10.0: `pre-0.10.0/`

The mocks the client was built to through 0.8.x. They still describe the panes, overlays
and navigators; their chrome (a separate tab row, an info bar on any of four sides, the
side nav) is what 0.10.0 replaces, so read them for everything inside the pane area and
for the overlays, not for the bar or the left column.

| Image | Shows | Requirements |
| --- | --- | --- |
| `pre-0.10.0/main.png` | Tab strip with running/urgency/zoom markers, BSP layout (one wide pane, two stacked), per-pane header with agent name, cwd, state, source, bottom info bar with session, mode, agent counts, git, cpu, mem, net, clock | UI-020, UI-021, UI-030 (pre-0.10.0), UI-032, AGT-001–004, SESS-002 |
| `pre-0.10.0/which-key.png` | Overlay after `ctrl+space`: every prefix-table chord grouped by purpose, info bar showing PREFIX mode. The keys predate the 0.8.3 re-key; §5.1 is authoritative (`s`/`w` navigators, `S` new session, `-`/`\|` splits, `e` swap, `space` sticky normal) | UI-002, UI-003, §5.1 default bindings |
| `pre-0.10.0/inbox.png` | Agent inbox overlay: rows for blocked, error and done panes across sessions and machines, one-key answer for approvals, source shown per row, state counts in footer | UI-044, AGT-001–004, NOTIFY-002 |
| `pre-0.10.0/session-nav.png` | Full-screen session navigator: tree of sessions and tabs with agent summaries, remote and restored sessions marked, live preview of the selected pane | UI-040, UI-041, STORE-008, §6 restore |
| `pre-0.10.0/sidebar.png` | Info bar on the left (vertical stacking, `remote:` module, notification queue), side nav open, zoomed pane with Z marker. Superseded by the panel (REQ-UI-042, 0.10.0) | UI-030 (pre-0.10.0), UI-032, UI-033, UI-042 (pre-0.10.0), SESS-005 |

## Conventions used in the mocks

- Agent states: `◐` working (blue), `■` blocked (amber), `✓` done (green), `✗` error (red). Glyphs and colors are theme tokens, not fixed.
- Pane header right side shows the state's `source` (`hook`, `osc`, `process`, `heuristic`) so a user can tell authoritative from inferred.
- Tab strip: `●` running process, `!` needs attention, `Z` zoomed.
- Active pane border uses the accent color; blocked panes use the warning color; idle panes use the muted border.

## Source

The editable canvas lives in claude.ai (not in this repo). Re-export PNGs here when it changes
and update this table.
