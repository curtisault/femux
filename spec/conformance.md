# Conformance

Every requirement of a milestone maps to at least one test (REQ-TEST-002). A requirement is done
when its tests are listed here and pass. Test names are `crate` `module::test`.

## 0.1.0

| Requirement | Tests | Status |
| --- | --- | --- |
| REQ-ARCH-001 | femux-test `repo::arch_001_daemon_has_no_ui_dependencies` | pass |
| REQ-ARCH-002 | femux-test `repo::arch_002_clients_and_plugins_use_the_protocol_only` | pass |
| REQ-ARCH-003 | femux-test `repo::arch_003_client_restart_keeps_panes` | pass |
| REQ-ARCH-004 | – | pending |
| REQ-PTY-001 | femux-test `pty::pty_001_own_session_and_group` (Linux) | pass |
| REQ-PTY-002 | femux-test `pty::pty_002_foreground_process_and_cwd` | pass |
| REQ-PTY-003 | femux-test `pty::pty_003_busy_pane_does_not_block_others` | pass |
| REQ-PTY-004 | femux-test `pty::pty_004_signals` | pass |
| REQ-PTY-005 | femux-test `pty::pty_005_pane_ids_are_ulids`; restart stability with restore (0.3.0) | pass |
| REQ-PTY-006 | femux-daemon `pty::tests::env_filter_matches_names_and_prefixes`; femux-test `pty::pty_006_live_environment_is_unfiltered` | pass |
| REQ-PTY-007 | femux-test `pty::pty_007_pane_environment` | pass |
| REQ-VT-001 | femux-test `vt::vt_001_each_pane_has_its_own_terminal` | pass |
| REQ-VT-002 | femux-vt `tests::only_dirty_rows_are_copied`, `tests::resize_reshapes_grid`; femux-test `vt::vt_002_frames_carry_cell_diffs` | pass |
| REQ-VT-003 | femux-vt `history::tests::scrollback_is_capped`; femux-test `vt::vt_003_scrollback_default_and_configurable` | pass |
| REQ-VT-004 | femux-test `vt::vt_004_slow_client_gets_newest_state_not_backlog`, `protocol::proto_040_one_unacked_frame_coalesces` | pass |
| REQ-VT-005 | femux-vt `history::tests::records_prompt_marks`; femux-test `vt::vt_005_prompt_marks` | pass |
| REQ-QRY-001 | femux-vt `query::tests::*`; femux-test `query::qry_001_daemon_answers_queries` | pass |
| REQ-QRY-002 | femux-vt `query::tests::answers_from_host_caps`; femux-test `query::qry_002_answers_follow_the_attached_client` | pass |
| REQ-QRY-003 | femux-test `query::qry_003_panes_get_encoded_input_only` | pass |
| REQ-SESS-001 | femux-test `session::sess_001_hierarchy_and_unique_names` | pass |
| REQ-SESS-002 | femux-daemon `layout::tests::*`; femux-test `session::sess_002_layout_tree_nodes` | pass |
| REQ-SESS-003 | femux-test `session::sess_003_one_event_per_mutation` | pass |
| REQ-SESS-004 | femux-test `session::sess_004_marks` | pass |
| REQ-SESS-005 | femux-test `session::sess_005_zoom_shows_only_focused_pane` | pass |
| REQ-VIEW-001 | femux-test `view::view_001_each_client_has_its_own_tab_and_size` | pass |
| REQ-VIEW-002 | femux-test `view::view_002_tab_size_is_minimum_of_viewers` | pass |
| REQ-VIEW-003 | femux-test `view::view_003_other_tabs_do_not_constrain` | pass |
| REQ-VIEW-004 | femux-test `view::view_004_resizes_are_debounced` | pass |
| REQ-EVT-001 | femux-daemon `bus::tests::seq_is_monotonic_and_stamped`; femux-test `events::evt_001_seq_ts_kind` | pass |
| REQ-EVT-002 | femux-daemon `bus::tests::filters_by_kind_session_and_pane`, `bus::tests::slow_subscriber_gets_events_lost_and_continues`; femux-test `events::evt_002_filters_and_loss`, `protocol::proto_041_full_frame_after_attach_and_events_lost` | pass |
| REQ-EVT-003 | femux-daemon `bus::tests::late_subscriber_replays_since`; femux-test `events::evt_003_replay_since` | pass |
| REQ-EVT-004 | femux-test `events::evt_004_pane_metadata` | pass |
| REQ-LIFE-001 | – | pending |
| REQ-LIFE-002 | femux-test `lifecycle::life_002_exit_with_last_session`, `lifecycle::life_002_keep_alive` | pass |
| REQ-PROTO-001 | femux-test `protocol::proto_001_socket_and_directory_modes`; femux-proto `tests::socket_dirs` | pass |
| REQ-PROTO-002 | femux-daemon `server::tests::other_uids_are_refused`; femux-test `protocol::proto_002_same_uid_peer_is_served` | pass |
| REQ-PROTO-003 | femux-test `protocol::proto_003_no_tcp_listener` (Linux) | pass |
| REQ-PROTO-004 | femux-proto `tests::frame_roundtrip_and_classification`, `tests::oversized_frames_rejected`; femux-test `protocol::proto_004_oversized_frame_closes_connection` | pass |
| REQ-PROTO-010 | femux-proto `schema::generated_kinds_match_schema` | pass |
| REQ-PROTO-011 | femux-proto `tests::frame_roundtrip_and_classification`; femux-test `protocol::proto_011_ids_and_kinds` | pass |
| REQ-PROTO-012 | femux-proto `tests::unknown_kind_and_fields`; femux-test `protocol::proto_012_unknown_kinds_and_fields` | pass |
| REQ-PROTO-020 | femux-test `protocol::proto_020_missing_feature_loses_action_not_connection` | pass |
| REQ-PROTO-021 | femux-test `protocol::proto_021_major_version_mismatch_closes` | pass |
| REQ-PROTO-030 | femux-vt `input::tests::*`; femux-test `query::qry_003_panes_get_encoded_input_only`, `acceptance::create_split_ls_detach_reattach` | pass |
| REQ-PROTO-040 | femux-test `protocol::proto_040_one_unacked_frame_coalesces` | pass |
| REQ-PROTO-041 | femux-test `protocol::proto_041_full_frame_after_attach_and_events_lost` | pass |
| REQ-BUILD-001 | femux-test `repo::build_001_pinned_zig_build`; CI `cargo build` | pass |
| REQ-BUILD-002 | femux-test `repo::build_002_rust_is_pinned` | pass |
| REQ-BUILD-003 | femux-test `repo::build_003_ci_runs_lint_and_tests_on_both_platforms` | pass |
| REQ-TEST-003 | femux-proto `schema::every_message_matches_its_schema_both_ways`, `schema::invalid_bodies_are_rejected_by_schema_and_decoder` | pass |
| REQ-TEST-001 | femux-test `acceptance::create_split_ls_detach_reattach` (uses `screen` and `screen_text`) | pass |
| Acceptance (0.1.0) | – | pending |
