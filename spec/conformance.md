# Conformance

Every requirement of a milestone maps to at least one test (REQ-TEST-002). A requirement is done
when its tests are listed here and pass. Test names are `crate` `module::test`.

## 0.1.0

| Requirement | Tests | Status |
| --- | --- | --- |
| REQ-ARCH-001 | femux-test `repo::arch_001_daemon_has_no_ui_dependencies` | pass |
| REQ-ARCH-002 | femux-test `repo::arch_002_clients_and_plugins_use_the_protocol_only` | pass |
| REQ-ARCH-003 | femux-test `repo::arch_003_client_restart_keeps_panes` | pass |
| REQ-ARCH-004 | femux-cli `binary::arch_004_plain_femux_starts_a_daemon_and_attaches`, `binary::arch_004_daemon_subcommand_detaches_and_refuses_a_second` | pass |
| REQ-PTY-001 | femux-test `pty::pty_001_own_session_and_group` (Linux) | pass |
| REQ-PTY-002 | femux-test `pty::pty_002_foreground_process_and_cwd` | pass |
| REQ-PTY-003 | femux-test `pty::pty_003_busy_pane_does_not_block_others` | pass |
| REQ-PTY-004 | femux-test `pty::pty_004_signals` | pass |
| REQ-PTY-005 | femux-test `pty::pty_005_pane_ids_are_ulids`, `store::store_004_sessions_come_back_with_their_ids_layout_and_marks` | pass |
| REQ-PTY-006 | femux-daemon `pty::tests::env_filter_matches_names_and_prefixes`; femux-test `pty::pty_006_live_environment_is_unfiltered`, `store::store_004_sessions_come_back_with_their_ids_layout_and_marks` (persisted env) | pass |
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
| REQ-LIFE-001 | femux-cli `binary::life_001_sigterm_hups_panes_and_exits`, `binary::acceptance_0_3_kill_the_daemon_restart_and_the_layout_returns` (with restore) | pass |
| REQ-LIFE-002 | femux-test `lifecycle::life_002_exit_with_last_session`, `lifecycle::life_002_keep_alive`, `lifecycle::life_002_the_request_that_ends_the_daemon_is_answered`, `lifecycle::life_002_exit_when_the_last_shell_exits`; femux-cli `binary::life_002_kill_of_the_last_session_succeeds` | pass |
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
| REQ-TEST-002 | femux-test `repo::test_002_every_milestone_requirement_is_mapped` | pass |
| Acceptance (0.1.0) | femux-test `acceptance::create_split_ls_detach_reattach`; bare viewer: femux-client `viewer::tests::*`, femux-cli `binary::arch_004_plain_femux_starts_a_daemon_and_attaches` | pass |

## 0.2.0

| Requirement | Tests | Status |
| --- | --- | --- |
| REQ-UI-001 | femux-client `ui_modes::ui_001_three_modes` | pass |
| REQ-UI-002 | femux-client `ui_modes::ui_002_prefix_is_one_shot_and_configurable`, `keys::tests::*`, `bindings::tests::every_default_binding_of_spec_5_1_exists`, `app::tests::a_bound_prefix_action_sends_the_prefix_itself` | pass |
| REQ-UI-003 | femux-client `ui_overlays::ui_003_which_key_shows_after_the_delay`; femux-plugin-which-key `tests::groups_stay_together_in_columns`, `tests::only_in_prefix_mode` | pass |
| REQ-UI-004 | femux-client `ui_modes::ui_004_prefixless_nav_is_off_by_default` | pass |
| REQ-UI-010 | femux-daemon `layout::tests::master_and_scroll_schemes_follow_pane_order`; femux-test `scheme::ui_010_schemes_change_the_geometry`; femux-client `ui_layout::ui_010_preselection_places_the_next_split` | pass |
| REQ-UI-011 | femux-client `ui_floats::ui_011_floats_render_above_tiles_and_follow_the_mouse`; femux-test `session::sess_002_layout_tree_nodes` (floats beside the tree) | pass |
| REQ-UI-043 | femux-client `ui_overlays::ui_043_palette_fuzzy_searches_actions_and_cli_commands`, `render::tests::runs_render_like_cells_and_boxes_fit_tiny_terminals` | pass |
| REQ-UI-020 | femux-client `ui_tabs::ui_020_tab_strip_shows_running_urgent_and_zoom`, `model::tests::tabs_are_inserted_at_the_daemons_index` | pass |
| REQ-UI-021 | femux-client `ui_tabs::ui_021_title_precedence`; femux-daemon `state::State::tab_title` via femux-test `session::*` | pass |
| REQ-UI-050 | femux-client `tests::paint_writes_only_the_diff_inside_synchronized_output`, `probe::tests::parses_answers_and_keeps_the_rest` | pass |
| REQ-UI-051 | femux-client `theme::tests::picks_by_name_mode_or_background`, `theme::tests::plugin_palettes_win_and_their_errors_fall_back`, `probe::tests::parses_answers_and_keeps_the_rest`; femux-plugin-theme `tests::*` | pass |
| REQ-UI-052 | femux-client `graphics::tests::placements_are_offset_and_clipped`, `graphics::tests::kitty_commands_carry_offsets_and_crops` | pass |
| REQ-UI-053 | femux-client `ui_mouse::ui_053_mouse_focuses_forwards_and_can_be_disabled` | pass |
| Acceptance (0.2.0) | femux-client `acceptance::all_default_bindings_work`, `acceptance::copy_mode_search_selects_and_yanks`; femux-cli `binary::arch_004_plain_femux_starts_a_daemon_and_attaches` (daily driver on a PTY) | pass |
| REQ-UI-012 | femux-cli `cli::ui_012_layout_templates_save_load_list_export`, `layouts::tests::nodes_roundtrip_through_json` | pass |
| REQ-CLI-001 | femux-cli `cli::cli_001_every_subcommand_has_json` | pass |
| REQ-CLI-002 | femux-cli `cli::cli_002_commands_default_to_the_current_pane`; femux-test `session::sess_001_hierarchy_and_unique_names` (`session.tabs`) | pass |
| REQ-CLI-003 | femux-cli `cli::cli_003_completions_come_from_the_definition` | pass |
| REQ-AGT-005 (install/uninstall; detection is 0.5.0) | femux-cli `cli::integration_install_hook_and_uninstall`, `integration::tests::*` | pass |
| Plugins `clipboard`, `shell-integration`, `which-key` | femux-cli `cli::plugin_subcommands_and_config_bindings`; femux-client `acceptance::copy_mode_search_selects_and_yanks` (OSC 52 + clipboard.set), `ui_overlays::ui_003_which_key_shows_after_the_delay` | pass |
| REQ-PLUG-001 | femux-test `repo::plug_001_plugins_are_feature_gated_crates` | pass |
| REQ-PLUG-002 | femux-plugin `daemon_plugin::plug_002_daemon_plugin_subscribes_and_writes_metadata`; femux-client `bindings::tests::plugin_bindings_join_the_tables`, `app::tests::yank_echoes_are_not_replayed_to_the_host`; femux-plugin-which-key `tests::*`; femux-plugin-clipboard `tests::*`; femux-plugin-shell-integration `tests::*` | pass |
| REQ-PLUG-003 | femux-test `repo::plug_003_plugins_depend_on_the_protocol_crates_only`, `repo::arch_002_clients_and_plugins_use_the_protocol_only` | pass |
| REQ-PLUG-004 | femux-test `repo::plug_004_built_in_list_matches_crates_and_milestones` | pass |

## 0.3.0

| Requirement | Tests | Status |
| --- | --- | --- |
| REQ-CFG-003 | femux-config `tests::cfg_003_unknown_keys_are_errors_with_lines`, `tests::example_config_is_exactly_the_defaults`; femux-cli `cli::cfg_003_config_check_rejects_unknown_keys_with_lines` | pass |
| REQ-STORE-001 | femux-daemon `store::tests::store_001_002_snapshot_roundtrip_wal_and_mode` | pass |
| REQ-STORE-002 | femux-daemon `store::tests::store_001_002_snapshot_roundtrip_wal_and_mode`; femux-test `store::store_004_sessions_come_back_with_their_ids_layout_and_marks` | pass |
| REQ-STORE-003 | femux-test `store::store_003_snapshots_follow_layout_changes_and_a_dirty_tick`; femux-daemon `logind::tests::*` (logind: best effort) | pass |
| REQ-STORE-004 | femux-test `store::store_004_sessions_come_back_with_their_ids_layout_and_marks`, `store::store_004_restore_is_off_by_default` | pass |
| REQ-STORE-005 | femux-test `store::store_005_reruns_are_staggered` | pass |
| REQ-STORE-006 | femux-test `store::store_006_scrollback_is_opt_in`; femux-daemon `persist::tests::history_bytes_redraw_text_and_styles`, `store::tests::store_001_002_snapshot_roundtrip_wal_and_mode` (compressed) | pass |
| REQ-STORE-007 | femux-daemon `store::tests::store_007_unloadable_files_are_backed_up_never_overwritten`; femux-test `store::store_007_a_bad_store_is_backed_up_and_the_daemon_starts` | pass |
| REQ-STORE-008 | femux-client `app::tests::store_008_restored_panes_have_a_dashed_border_until_a_command_runs`; femux-test `store::store_004_sessions_come_back_with_their_ids_layout_and_marks` (daemon mark) | pass |
| `femux reload` (ADR-013) | femux-test `store::server_reload_applies_the_config_or_changes_nothing`; femux-cli `cli::reload_applies_the_config_to_the_running_daemon`; femux-client `app::tests::config_reloaded_asks_for_the_config_and_applies_it` | pass |
| REQ-CFG-001 | femux-config `tests::session_files_list_tabs_then_plain_shells`; femux-cli `cli::cfg_001_002_launch_from_a_file_and_from_flags`; femux-plugin-launcher `tests::cfg_002_flags_build_the_same_session_as_a_file` | pass |
| REQ-CFG-002 | femux-cli `cli::cfg_001_002_launch_from_a_file_and_from_flags`; femux-plugin-launcher `tests::*` | pass |
| Plugin `launcher` | femux-cli `cli::cfg_001_002_launch_from_a_file_and_from_flags`; femux-test `repo::plug_003_plugins_depend_on_the_protocol_crates_only` | pass |
| Acceptance (0.3.0) | femux-cli `binary::acceptance_0_3_kill_the_daemon_restart_and_the_layout_returns`, `cli::cfg_001_002_launch_from_a_file_and_from_flags` | pass |

## 0.4.0

| Requirement | Tests | Status |
| --- | --- | --- |
| REQ-UI-030 | femux-client `ui_bar::ui_030_the_bar_sits_on_any_side_and_the_panes_move_over`, `ui_bar::ui_030_a_column_bar_never_draws_over_the_panes`, `layout::tests::ui_030_the_bar_takes_one_side_and_the_panes_the_rest` | pass |
| REQ-UI-031 | femux-client `ui_bar::ui_031_modules_come_from_plugins_in_config_order_on_their_interval` | pass |
| REQ-UI-032 | femux-plugin-infobar `tests::*`; femux-client `ui_bar::acceptance_0_4_bar_on_all_four_sides_with_every_built_in_module` | pass |
| REQ-UI-033 | femux-client `ui_bar::ui_033_notifications_show_while_their_pane_is_hidden`; femux-test `notify::notify_post_is_relayed_to_the_panes_session` | pass |
| Plugin `infobar` | femux-plugin-infobar `tests::*`; femux-client `ui_bar::*` | pass |
| REQ-UI-040 | femux-client `ui_nav::ui_040_session_navigator_lists_attaches_renames_creates_and_kills`, `nav::tests::ui_040_sessions_expand_to_tabs_and_filter`; femux-test `notify::last_activity_follows_output_and_input` | pass |
| REQ-UI-041 | femux-client `ui_nav::ui_041_window_navigator_previews_panes_live_and_goes_to_them`, `nav::tests::ui_041_windows_list_every_pane_and_preview_the_selection` | pass |
| REQ-UI-042 | femux-client `ui_nav::ui_042_side_nav_lists_sessions_and_tabs_and_switches_on_click` | pass |
| Plugin `theme` | femux-plugin-theme `tests::bundled_themes_by_name`, `tests::files_override_a_base_and_reject_unknown_keys`; femux-client `theme::tests::plugin_palettes_win_and_their_errors_fall_back` | pass |
| Acceptance (0.4.0) | femux-client `ui_bar::acceptance_0_4_bar_on_all_four_sides_with_every_built_in_module`, `ui_nav::*` | pass |

## 0.5.0

| Requirement | Tests | Status |
| --- | --- | --- |
| REQ-AGT-001 | femux-plugin-agents `detect::agt_001_a_hook_state_is_tier_one_and_gets_source_and_since`, `detect::agt_001_osc_progress_and_private_reports_are_tier_one` | pass |
| REQ-AGT-002 | (Linux; macOS is hook-only) femux-plugin-agents `detect::agt_002_a_process_waiting_on_the_tty_is_blocked_then_exited_then_idle`, `procfs::tests::*` | pass |
| REQ-AGT-003 | femux-plugin-agents `detect::agt_003_heuristics_judge_silence_after_output_by_the_last_line`, `detect::agt_003_heuristics_stay_quiet_once_tier_one_spoke`, `detect::tests::prompts_look_like_questions` | pass |
| REQ-AGT-004 | femux-plugin-agents `tier::tests::agt_004_higher_tiers_override_and_lower_ones_wait_out_the_hold`, `detect::agt_004_a_lower_tier_waits_out_the_hold` | pass |
| REQ-AGT-005 | femux-cli `cli::integration_install_hook_and_uninstall`, `agents::agent_wait_returns_when_the_claude_code_hook_reports_done` | pass |
| REQ-TEST-006 | femux-plugin-agents `detect::*` | pass |
| SPEC §9.3 `femux agent …` | femux-cli `agents::agent_wait_returns_when_the_claude_code_hook_reports_done`, `agents::agent_list_and_explain_show_state_source_and_evidence`, `agents::agent_prompt_types_text_and_enter`, `cli::cli_001_every_subcommand_has_json` | pass |
| REQ-UI-044 | femux-client `ui_inbox::ui_044_inbox_jumps_and_answers_across_sessions`, `inbox::tests::ui_044_lists_blocked_error_done_across_sessions` | pass |
| REQ-NOTIFY-001 | femux-plugin-notify `tests::notify_001_sound_bell_and_desktop_become_host_effects`, `notify::notify_001_agent_states_and_exits_become_notifications`, `notify::notify_001_webhooks_post_json`; femux-client `host::tests::*`; femux-cli `notify::notify_test_posts_a_notification` | pass |
| REQ-NOTIFY-002 | femux-plugin-notify `tests::notify_002_only_when_hidden_skips_shown_panes`, `notify::notify_002_only_when_hidden_webhooks_skip_shown_panes`; femux-client `notify::notify_002_003_the_client_acts_on_posted_events_unless_shown` | pass |
| REQ-NOTIFY-003 | femux-client `notify::notify_002_003_the_client_acts_on_posted_events_unless_shown` | pass |
| 0.5.0 acceptance | femux-cli `agents::agent_wait_returns_when_the_claude_code_hook_reports_done`; femux-client `notify::chime_plays_when_the_hook_reports_done` | pass |
| REQ-PLUG-002 (plugin ticks, responses, CLI events) | femux-plugin-agents `detect::*`; femux-cli `agents::*` | pass |

## 0.6.0

| Requirement | Tests | Status |
| --- | --- | --- |
| REQ-PROTO-010 (ADR-020 plugin.publish/plugin.data) | femux-test `plugin::plugin_data_reaches_every_subscriber`, `protocol::*` | pass |
| REQ-PROTO-003 | femux-cli `remote::attach_over_ssh_runs_the_remote_session_and_detaches`, `remote::forwarded_socket_attach`, `remote::a_dropped_link_reconnects_and_the_session_survives`; femux-client `remote::tests::ssh_commands_use_openssh_with_keepalives` | pass |
| REQ-SEC-007 | femux-cli `remote::remote_notifications_play_on_the_client` | pass |
| REQ-UI-032 (`remote:` modules) | femux-client `ui_bar::ui_032_remote_modules_show_the_daemon_hosts_values`; femux-plugin-hosts `tests::*` | pass |
| `femux hosts add\|list\|rm` | femux-cli `remote::hosts_add_list_rm`; femux-config `tests::hosts_file_roundtrips_and_resolves_raw_destinations` | pass |
| 0.6.0 acceptance | femux-cli `remote::attach_over_ssh_runs_the_remote_session_and_detaches` (attach over SSH), `remote::a_dropped_link_reconnects_and_the_session_survives` (detach survives link drop), `remote::remote_notifications_play_on_the_client` (remote notifications play locally) | pass |

## 0.6.1

| Requirement | Tests | Status |
| --- | --- | --- |
| REQ-UI-010 (`[layout] scheme`, `gaps`) | femux-daemon `layout::tests::gaps_widen_every_separator_and_give_way_when_space_is_short`; femux-test `scheme::layout_config_sets_the_scheme_and_gaps`; femux-client `render::tests::borders_run_down_the_middle_of_gaps` | pass |
