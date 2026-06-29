# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 263 youtui + 98 ytmapi-rs unit passed
**Last updated:** 2026-06-25

---

## Guardrails (Do Not Break)

| # | Guardrail | Rationale |
|---|-----------|-----------|
| G1 | **Notifications** — `NotificationController` + `notify_track_change()` + call in `update_metadata()`. Has compile-time guard tests. | Active D-Bus system. Was removed once (`faf21ef`). |
| G2 | **artists_string / track_no_string** cached on `ListSong` — `get_field(Artists)` returns `Cow::Borrowed`, not allocate. | Per-frame rendering perf. Was removed once (`7d88983`). |
| G3 | **`--js-runtimes` must NOT be auto-detected.** Only pass when user explicitly configures. Auto-detection spawns Node.js per yt-dlp. | RAM waste |
| G4 | **Video IDs starting with `-` need `--` separator** before URL arg (e.g. `-6FvsKo162U`). yt-dlp interprets leading `-` as flag. | Correct playback |
| G5 | **`deduplicate()` must be O(n) via HashSet.** Vec::contains O(n²) caused ~1.7B comparisons on 58k-song autosave. | Startup perf |
| G6 | **Never add `--print` to yt-dlp download args when `-o -` is also used** — combo causes empty stdout (yt-dlp >=2026.04.10). Guarded by test. | Zero-byte audio |
| G7 | **`VL` prefix lives in query `header()`, not in callers.** `GetPlaylistTracksQuery`/`GetPlaylistDetailsQuery` auto-prepend `VL`. | Playlist loading |
| G8 | **Mako only supports `file://` URLs** for notification icons. Remote URLs silently ignored. | Notification perf |
| G9 | **Effect chain order: `stop_song_id` saved BEFORE `start_buffering`.** Old song stops immediately on skip/next. | Correct skip |
| G10 | **`handle_playing` must transition `Buffering → Playing`**, not just `Paused → Playing`. | Status bar + media keys |

---

## P0 — Must Have (Playback UX)

### P0.1 — Emit streaming decoder ASAP from download pipeline

**What:** `download_and_decode` currently returns the streaming decoder as its final result — the caller (`DownloadSong` stream) gets it only when the function returns. Split the function so the decoder is sent via `oneshot` channel the moment it's created from the streaming buffer (after first yt-dlp progress line), BEFORE the function returns. This eliminates the 0-2s gap when `play_song_id` is called for a song whose download hasn't completed yet.

**Why:** The existing pre-buffer infra (`download_upcoming_from_id`, `preloaded_sources`) already downloads songs 2-ahead and stores decoders for instant play. The remaining gap is when user skips to a song whose download is still in progress (status = `Queued`). `play_song_id` finds nothing in `preloaded_sources` and falls through — no `PlaySong` is sent until `download_and_decode` returns. With this fix, the decoder arrives immediately after the first yt-dlp progress line (~1s), not after the full download cycle.

**AC:**
- `download_and_decode` refactored: sends streaming decoder via `oneshot` immediately after creation
- `DownloadSong.into_stream` awaits the `oneshot` and sends `Completed(decoder)` earlier
- Background download + cache population continues independently
- When `play_song_id` is called for a `Queued` song, the existing download stream emits `Completed(decoder)` within ~1s instead of waiting for download completion
- All existing tests pass
- No regression in cache population (cache still populated after full download)

**Dep:** None
**Effort:** M (2-3 days)
**Status:** ✅ Done

**What was done:**
- Discovered the actual bottleneck: `tokio::time::timeout(15s, stderr_handle).await` waited for the **entire stderr stream to end** (yt-dlp to finish), not just the first progress line. Decoder creation was delayed by 8-60s.
- Fix: replaced with `buffer.wait_for_total_len(15s)` which returns immediately after the first progress line is parsed (~1-2s), reducing decoder creation from 8-60s down to ~1-2s.
- Added `wait_for_total_len(&self, timeout: Duration) -> Option<u64>` to `SharedBuffer` (streaming_buffer.rs).
- `stderr_handle` is no longer awaited — it continues running in the background to log ERROR/WARNING lines.
- 3 new tests for `wait_for_total_len`.

---

### P0.2 — Graceful streaming error surfacing

**What:** When yt-dlp fails mid-stream (timeout, 403, empty output, non-zero exit), the UI currently freezes silently. Surface the error so user sees feedback.

**Why:** Silent failure is the #1 confusing UX for new users.

**AC:**
- yt-dlp non-zero exit → status bar shows "Download failed: yt-dlp exited with code N" for 5s
- yt-dlp timeout (120s) → status bar shows "Download timed out" for 5s
- yt-dlp empty output → status bar shows "Download produced no audio data" for 5s
- Error clears on next song / manual action
- No panic, no endless spinner
- Existing `PlayUpdate` variants suffice, or add `PlayUpdate::DownloadError(String)`

**Dep:** None (self-contained in `playlist.rs`)
**Effort:** S (1 day)
**Status:** ✅ Done

**What was done:**
- When `DownloadProgressUpdate::Error(e)` arrives and the song is in `Buffering` state, transition to `PlayState::Error(id)` and call `play_next_or_stop(id)` to skip.
- Status bar shows `''` (warning icon) + song title for the failed song, matching existing `PlayState::Error` UI handling.
- Error message is logged at `error!` level; the `handle_set_to_error` path auto-skips to the next available song.

---

## P1 — Should Have (UX / Configurability)

### P1.1 — Configurable download cache size

**What:** Move `CACHE_MAX_ENTRIES = 3` from compile-time constant to `config.toml` (`download_cache_size = 10`).

**Why:** Heavy users replaying albums want 10+ entries. 3 is too small for album replay.

**AC:**
- `Config` + `ConfigIR` gains `download_cache_size: Option<usize>` (default: 3)
- `song_downloader.rs` reads value at init
- Range validated: 0 = disabled, 1-100 allowed
- `cache_put` / `CACHE_ORDER` use runtime value instead of compile-time const

**Dep:** None
**Effort:** S (half day)
**Status:** 🔜

---

### P1.2 — Status bar streaming indicator

**What:** Show distinct icon in status bar footer when download is still in progress vs fully cached.

**Why:** User has no feedback whether the song is still streaming or fully cached.

**AC:**
- Status bar shows `⬇ Song Title` while download incomplete, `▶ Song Title` when fully cached
- Transition happens when `SharedBuffer::is_finished()` becomes true
- Does NOT add latency to the playback pipeline (purely cosmetic UI state)
- Works correctly with P0.1 pre-buffering (pre-buffered songs are always fully cached by play time)

**Dep:** P0.1 (design must account for pre-buffer — a pre-buffered song is always fully cached at swap time, so the streaming state is only relevant for the first song or cache misses)
**Effort:** S (half day)
**Status:** 🔜

---

### P1.3 — Configurable pre-buffer count

**What:** Extend P0.1 to pre-buffer N upcoming songs (configurable, default 1).

**Why:** Albums/podcasts: pre-buffering the next 3-5 songs eliminates all download latency for the entire listening session.

**AC:**
- `prebuffer_count: usize` in `config.toml` (default: 1, max: 10)
- N SharedBuffers + decoders allocated in background
- FIFO rotation: oldest pre-buffered decoder is the next to play; new download fills the vacated slot
- Respects memory: N * (peak song size) should not exhaust RAM (document tradeoff)

**Dep:** P0.1
**Effort:** M (3 days)
**Status:** 🔜

---

## P2 — Code Quality

### P2.1 — Remove dead decoder integration test file

**What:** Delete `decoder_integration_test.rs`. Its coverage is subsumed by `song_downloader.rs` tests.

**Why:** Dead code. Confuses navigation.

**AC:** File deleted. `cargo test` passes. Nothing references it.

**Dep:** None
**Effort:** XS (5 min)
**Status:** 🔜

---

### P2.2 — Deduplicate code generators

**What:** `gen_expected.rs` (standalone binary for generating expected outputs) vs `gen_output.rs` + `gen_output/src/main.rs` (test helper, same logic).

**Why:** Two independent implementations of the same thing. Changes to one silently rot the other.

**AC:** One generator serves both purposes. The other is deleted. `gen_output/` tests still pass.

**Dep:** Need to understand what each does and whether they're truly equivalent.
**Effort:** S (1 day)
**Status:** 🔜

**Sub-tasks:**
1. Read both files, document what each does
2. If truly equivalent: delete one, alias the other
3. If different: extract shared logic, delete duplicate
4. Verify tests pass

---

### P2.3 — English-only browser version check

**File:** `ytmapi-rs/src/auth/browser.rs:69-71`

**What:** Version detection string-matches `" "` (English "Version" substring). Localized browsers (e.g. Firefox in German "Version" → "Version" still works, but some languages differ) break silently.

**Why:** Fragile. Will break for non-English browser users.

**AC:** Use numeric `Int` parsing from `"ver {int}"` pattern or regex `Version\s+(\d+)` instead of English-specific string match.

**Dep:** None
**Effort:** S (half day)
**Status:** 🔜

---

### P2.4 — Error messages with `type_name::<A>()` produce unreadable text

**File:** `config/keymap.rs:300+`

**What:** Error messages use `std::any::type_name::<A>()` which for complex generic types produces strings like `youtui::app::ui::playlist::Playlist<(youtui::app::ui::playlist::QueueState, alloc::sync::Arc<tokio::sync::Mutex<...>>)>`.

**Why:** Useless to users. They can't act on "type_name garbled."

**AC:** Replace with hand-written display strings or a `type_name_short` helper that strips module paths and generic params. Verify all error paths render readable text.

**Dep:** None
**Effort:** S (half day)
**Status:** 🔜

---

### P2.5 — Triplicated song-clone-and-callback pattern

**Files:** `songsearch.rs` / `artistsearch.rs` / `playlistsearch.rs`

**What:** ~50 lines of the "clone song, send callback with progress" pattern duplicated 3x with minor variations. Previous extraction failed due to RPITIT + self-in-macro limitations.

**Why:** DRY. Reduces maintenance surface.

**AC:** Single shared implementation. All 3 call sites use it. Tests pass. No new trait constraints leaked into callers.

**Dep:** None (but previous attempt failed — may be dead end)
**Effort:** M (2-3 days, risk of failure again)
**Status:** 🔜

---

### P2.6 — `resolve_omv_*` shared pattern extraction

**Files:** `api.rs:118-161`, `api.rs:75-116`

**What:** Both `resolve_omv_with_audio_playlist` and `resolve_omv_album_songs_with_search` use the same HashMap cross-ref logic.

**Why:** Duplicated code. Low risk, mechanical extraction.

**AC:** Single `resolve_omv_crossref()` helper. Both callers use it. Tests pass. Coverage for edge cases (zero results, all results matched, partial match).

**Dep:** None
**Effort:** S (half day)
**Status:** 🔜

---

## P3 — Performance

### P3.1 — Compact autosave: `to_string_pretty` → `to_string`

**File:** `queue_persistence.rs:88`

**What:** Autosave JSON uses `serde_json::to_string_pretty` producing 20MB files for 58k songs. Switch to compact `to_string()`.

**Why:** Smaller file (~10MB), faster writes, faster reads.

**AC:** Autosave file is ~50% smaller. Loading still works (JSON is valid either way). No data loss.

**Dep:** None
**Effort:** XS (15 min)
**Status:** 🔜

---

### P3.2 — `DEFAULT_STREAM_CHANNEL_SIZE` increase

**File:** `manager.rs:176`

**What:** `DEFAULT_STREAM_CHANNEL_SIZE = 20` hardcoded. 20 is small for high-throughput audio events.

**Why:** Potential backpressure under heavy seeking / rapid song switches.

**AC:** Increase to `256` (consistent with rodio queue sizes) or make configurable. Verify no adverse memory impact (256 * message_size ≈ negligible).

**Dep:** None
**Effort:** XS (15 min)
**Status:** 🔜

---

### P3.3 — `RwLock<DynamicYtMusic>` write contention

**File:** `server.rs`

**What:** `RwLock` write lock held across entire HTTP request duration serializes concurrent API queries.

**Why:** Album fetch + search run in parallel today but the RwLock serializes them.

**AC:** Measure current contention (add metrics or log). If significant, use `Arc<DynamicYtMusic>` + clone-on-write or split reader/writer. If negligible, close as wontfix.

**Dep:** None
**Effort:** S (1 day to investigate + fix if needed)
**Status:** 🔜

---

## P4 — Testing / CI

### P4.1 — GitHub Actions CI pipeline

**Files:** `.github/`

**What:** Add workflow for `cargo check` + `cargo test` + `cargo clippy` on PR/push.

**Why:** Currently zero automation. Regressions go undetected between sessions.

**AC:**
- Workflow triggers on `push` to main + `pull_request` to main
- `cargo check` fails the build on warnings (or just compiles)
- `cargo test` runs all non-live-integration tests
- `cargo clippy` runs with `--deny warnings`
- Total CI time < 5 min (use caching)
- Badge in README

**Dep:** None
**Effort:** S (1 day)
**Status:** 🔜

---

### P4.2 — Criterion benchmarks for `get_field` hot-path

**File:** `benches/` (new)

**What:** Perf regression detection for `get_field` which is called per frame.

**Why:** Any change to `get_field` that causes allocation degrades frame rendering.

**AC:** 
- `cargo bench` runs and produces stable measurements
- Benchmarks cover: `Artists` (Cow::Borrowed), `Title`, `Album`, `Duration`
- Baseline stored in repo or compared against previous run

**Dep:** None
**Effort:** S (1 day)
**Status:** 🔜

---

### P4.3 — Coverage for `resolve_omv_*` edge cases

**File:** `api.rs` tests

**What:** Untested error paths: zero-Atv results, batch empty-list guard, `GetAlbumsSongsError`.

**Why:** These branches silently catch errors — if the logic is wrong, we never know.

**AC:** Unit tests with mock JSON responses for each edge case. Tests fail if the error path is removed/changed.

**Dep:** P2.6 (refactoring first makes testing easier)
**Effort:** S (half day)
**Status:** 🔜

---

### P4.4 — Artist album pagination

**What:** `GetArtistAlbumsQuery` only returns first page. Needs `ParseFromContinuable` impl for the album response type.

**Why:** Artists with many albums show incomplete discography.

**AC:**
- `GetArtistAlbumsQuery` implements `ParseFromContinuable` 
- Pagination wired in browser: user scrolls past last album → fetches next page
- Existing continuation token from API response is used
- All pages loaded before reaching end of list

**Dep:** Requires understanding the YT API album response continuation format
**Effort:** M (3-5 days, needs API reverse-engineering)
**Status:** 🔜

---

## P5 — Far Future (Not Ready)

| # | Item | Why blocked |
|---|------|-------------|
| F1 | Replace async-callback-manager with native Tokio | 29 usages, ~2000 line rewrite, unclear payoff. Needs dependency analysis. |
| F2 | Gapless playback | Blocked on symphonia AAC gapless support (upstream). Not actionable. |
| F3 | Mouse support | Needs ratatui MouseEvent impl across key stack. Pure scope. |
| F4 | Offline disk cache | Serialize `BYTE_CACHE` to disk on shutdown. Needs shutdown hook. |
| F5 | Display lyrics | Requires GetLyrics integration + new UI component. Pure scope. |
| F6 | Theming | Color scheme config. Pure scope. Low demand. |
| F7 | Stats Tab | CPU/memory/cache metrics in TUI. Pure scope. |

---

## Legend

| Icon | Meaning |
|------|---------|
| 🔜 Pending | Ready to work on |
| ✅ Done | Completed |
| 🔒 External | Blocked on upstream / needs investigation |

**Effort:** XS (< 1h), S (< 1 day), M (3-5 days), L (1-2 weeks)
