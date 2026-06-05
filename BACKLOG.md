# Youtui Backlog

**Build:** 0 errors, 1 warning (pre-existing `parse_simple_time_to_secs` dead code) | **Tests:** 97 passed, 5 ignored | **Last updated:** 2026-06-05

---

## 🛡️ Regression Guardrail Policy

1. **Compile-time tests preferred** — use type-level assertions (`&mc.field`, `fn(_: &Type)`) that fail at compile time if invariants break, with zero runtime cost.
2. **Hot-path changes require benchmarks** — any change to `get_field`, `compute_artists_string`, or `get_fields` must include a criterion benchmark proving no regression.
3. **Notification system is protected** — `NotificationController`, `notify_track_change()`, and its call in `update_metadata()` each have a compile-time test. Removing any of them breaks the build.
4. **Mako only supports `file://` URLs** for notification icons. Remote URLs are silently skipped. Do not add thumbnail downloading to the notification path.

---

## P0 — Must Preserve (Do Not Remove)

| # | File | Issue | Status |
|---|------|-------|--------|
| 1 | `media_controls.rs` | **Notification system** — `NotificationController` + `notify_track_change()` + call in `update_metadata()`. Was removed by `faf21ef`; restored. Has compile-time guard tests. | ✅ Guarded |
| 2 | `structures.rs` | **artists_string / track_no_string cached fields** — were removed by `7d88983` causing per-frame allocation regression; restored and guarded. `get_field(Artists)` must return `Cow::Borrowed`. | ✅ Guarded |

## P1 — Performance / Allocation

| # | File | Line | Issue | Status |
|---|------|------|-------|--------|
| 7 | `playlist.rs` | 78 | `std::sync::Mutex` in async (invariant documented) | 🔒 Invariant OK |
| 11 | `draw.rs` | 101–163 | `draw_help` two-pass (lifetime constraint) | ⏳ Postponed |
| — | `shared_components.rs` | — | Sort popup allocation per open | ⏳ TBD |
| — | `structures.rs` | — | `get_field` benchmark baseline established via criterion | 🔜 Planned |

## P2 — Code Quality / Maintainability

| # | File | Issue | Status |
|---|------|-------|--------|
| 21 | `shared_components.rs` | FilterManager/SortManager XXX refactor | ✅ Fixed |
| 22 | `actionhandler.rs` | Library/binary API split | ⏳ Pending |
| 23 | `playlist.rs` 68 | `pub cur_played_dur` → getter | ⏳ Pending |
| 24 | `playlist.rs` 889–891 | `HashMap<ListSongID, usize>` for O(1) index lookup | ⏳ Pending |

## P3 — Architecture / Design

| # | File | Issue | Status |
|---|------|-------|--------|
| — | `youtui/` | Notification system was removed as "dead code" once. Add check: no removal of `notify-rust` or `notification_controller` field without explicit approval. | ✅ Guarded |

## P4 — Test Debt / Infrastructure

| # | File | Issue | Status |
|---|------|-------|--------|
| 26 | `view.rs` 97 | TODO: more tests | ⏳ Pending |
| 27 | `ytmapi-rs/` | 54 live integration test failures (pre-existing) | 🔒 External |
| — | `youtui/` | ✅ Add criterion benchmark for `get_field` hot-path (perf regression detection) | 🔜 Planned |
| — | `youtui/` | ✅ Add `TestBackend` render snapshot tests for playlist + browser views | 🔜 Planned |
| — | `youtui/` | ✅ Add `profile-render` feature flag with per-draw timing (warn if >8ms) | 🔜 Planned |
| — | `youtui/` | ✅ Add state-model integration tests (keypress → expected state) | 🔜 Planned |
| — | `media_controls.rs` | ✅ 5 regression tests for notification system (3 compile-time, 2 ignored D-Bus) | ✅ Done |

## P5 — Dead Code (preserved for reuse)

| # | File | Line | Function | Status |
|---|------|------|---------|--------|
| 33 | `core.rs` | 110–142 | `create_or_clean_directory` | `#[allow(dead_code)]` |
| 34 | `core.rs` | 147–170 | `touch_file_with_timestamp` | `#[allow(dead_code)]` |
| 35 | `core.rs` | 174–192 | `get_dir_file_paths` | `#[allow(dead_code)]` |
