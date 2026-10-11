# Conformance

Every requirement of a milestone maps to at least one test (REQ-TEST-002). A requirement is done
when its tests are listed here and pass. Test names are `crate` `module::test`.

## 0.1.0

| Requirement | Tests | Status |
| --- | --- | --- |
| REQ-ARCH-001 | – | pending |
| REQ-ARCH-002 | – | pending |
| REQ-ARCH-003 | – | pending |
| REQ-ARCH-004 | – | pending |
| REQ-PTY-001 | – | pending |
| REQ-PTY-002 | – | pending |
| REQ-PTY-003 | – | pending |
| REQ-PTY-004 | – | pending |
| REQ-PTY-005 | – | pending |
| REQ-PTY-006 | – | pending |
| REQ-PTY-007 | – | pending |
| REQ-VT-001 | – | pending |
| REQ-VT-002 | femux-vt `tests::only_dirty_rows_are_copied`, `tests::resize_reshapes_grid`; daemon frames pending | partial |
| REQ-VT-003 | femux-vt `history::tests::scrollback_is_capped`; per-pane config pending | partial |
| REQ-VT-004 | – | pending |
| REQ-VT-005 | femux-vt `history::tests::records_prompt_marks`; `scrollback.marks` pending | partial |
| REQ-QRY-001 | femux-vt `query::tests::answers_identity_queries`, `query::tests::answers_mode_and_keyboard_queries`; daemon pending | partial |
| REQ-QRY-002 | femux-vt `query::tests::answers_from_host_caps`; wiring to `hello` pending | partial |
| REQ-QRY-003 | – | pending |
| REQ-SESS-001 | – | pending |
| REQ-SESS-002 | – | pending |
| REQ-SESS-003 | – | pending |
| REQ-SESS-004 | – | pending |
| REQ-SESS-005 | – | pending |
| REQ-VIEW-001 | – | pending |
| REQ-VIEW-002 | – | pending |
| REQ-VIEW-003 | – | pending |
| REQ-VIEW-004 | – | pending |
| REQ-EVT-001 | – | pending |
| REQ-EVT-002 | – | pending |
| REQ-EVT-003 | – | pending |
| REQ-EVT-004 | – | pending |
| REQ-LIFE-001 | – | pending |
| REQ-LIFE-002 | – | pending |
| REQ-PROTO-001 | – | pending |
| REQ-PROTO-002 | – | pending |
| REQ-PROTO-003 | – | pending |
| REQ-PROTO-004 | femux-proto `tests::frame_roundtrip_and_classification`, `tests::oversized_frames_rejected` | pass |
| REQ-PROTO-010 | femux-proto `schema::generated_kinds_match_schema` | pass |
| REQ-PROTO-011 | femux-proto `tests::frame_roundtrip_and_classification` | pass |
| REQ-PROTO-012 | femux-proto `tests::unknown_kind_and_fields` (decoder); daemon `error{unknown_kind}` reply pending | partial |
| REQ-PROTO-020 | – | pending |
| REQ-PROTO-021 | – | pending |
| REQ-PROTO-030 | femux-vt `input::tests::*`; `pane.input` pending | partial |
| REQ-PROTO-040 | – | pending |
| REQ-PROTO-041 | – | pending |
| REQ-BUILD-001 | – | pending |
| REQ-BUILD-002 | – | pending |
| REQ-BUILD-003 | – | pending |
| REQ-TEST-003 | femux-proto `schema::every_message_matches_its_schema_both_ways`, `schema::invalid_bodies_are_rejected_by_schema_and_decoder` | pass |
| Acceptance (0.1.0) | – | pending |
