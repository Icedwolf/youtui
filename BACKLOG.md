# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 267 youtui + 98 ytmapi-rs unit passed
**Last updated:** 2026-07-01

**Session (2026-07-01):** Logging audit (P1.4–1.7), dead code + imports (P2.9, P2.15, P2.19), poisons/unwraps (P2.13), magic numbers → named constants (P2.12), `Arc<[u8]>` for BYTE_CACHE (P3.4), row number cache (P3.6), `NonZero` const + hot path trace (decoder/mod.rs). `cargo check` — 0 errors, 0 warnings, 0 clippy.

**Session (2026-07-01 pt2):** `.into_iter().next()` → `.first()` (P2.18), removed dead `decoder_integration_test.rs` (P2.1), added `#[must_use]` to 26 pure functions (P2.16), merged `handle_play_update` → `handle_autoplay_update` via `From` conversion (P2.10), documented 8 fire-and-forget `tokio::spawn` calls (P2.14). `cargo check` — 0 errors, 0 warnings, 0 clippy.

**Session (2026-07-02):** Process overhaul — restructured AGENTS.md with failure story + compulsory pre-flight, added `opencode.jsonc` with `edit:ask` + `instructions`, created `.opencode/skills/preflight/SKILL.md`. Code: P2.10 (actually done — removed `handle_play_update` method + `HandlePlayUpdate` effect variant), P2.11 (actually done — unified `resolve_to_audio_track` + `ResolveSongToAudio`), P2.17 (done — all 24 `String` error sites in `song_downloader.rs` → `anyhow::{bail, Context}`, removed dead `search_broad` method on `Api`), reverted half-baked po_token retry. `cargo check` — 0 errors, 0 warnings. `cargo test -p youtui` — 255 passed, 2 ignored.

**Live log analysis (debug128.log):** Fixed 3 bugs — (1) prebuffer no longer includes the currently-playing song (`download_upcoming_from_id` filters out `get_cur_playing_id()`), (2) buffering songs marked `Queued` so prebuffer won't re-queue them, (3) yt-dlp ERROR lines on stderr now call `SharedBuffer::fail()` so the 5s buffer deadline aborts early instead of waiting for timeout. `cargo check` — 0 errors, 0 warnings, 0 clippy.

**Priority fix:** Removed `download_upcoming_from_id` from `prepare_playback_id`. Prebuffer now only fires AFTER the selected song starts playing (via `handle_playing` → `regenerate_downloads_for_current`). During download, future songs are NOT queued — 100% bandwidth goes to the selected song. `cargo check` — 0 errors, 0 warnings, 0 clippy.

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
**Status:** ✅ Done

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

## P1 — Logging Audit

### P1.4 — Hot path log demotion (`decoder/mod.rs:209`)

**What:** `info!("current_span_len -> Some(0) (eos)")` fires on every audio `current_span_len()` query during playback (hot path, ~40/sec).

**Fix:** `info!` → `trace!`
**Effort:** XS (1 min)
**Status:** ✅ Done

---

### P1.5 — Per-keystroke log demotion (`async_rodio_sink.rs`)

**What:** ~15 `info!` calls for seek, volume, pause, play, skip — fire on every user keystroke. Also 2 `error!` calls that are normal transitions (autoplay already playing, queue-empty).

**Fix:** All 15 `info!` → `debug!`, 2 `error!` → `debug!`
**Effort:** XS (15 min)
**Status:** ✅ Done

---

### P1.6 — Per-download pipeline noise (`song_downloader.rs`, `playlist.rs`)

**What:** yt-dlp WARNING stderr (`info!`, every download), download scope/queue dump (`info!`, every song transition), normal stream termination messages.

**Fix:**
- `song_downloader.rs:307,383` `info!` → `debug!`
- `song_downloader.rs:310,386,364,404` `info!` → `debug!`
- `song_downloader.rs:142,358,398` `info!` → `warn!` (really errors, fix wrong level)
- `song_downloader.rs:173,448,548` `info!` → `error!` (panics)
- `playlist.rs:840-886` `info!` → `debug!`
**Effort:** S (30 min)
**Status:** ✅ Done

---

### P1.7 — Per-event noise (shutdown, browser routing, task tracking)

**What:**
- `appevent.rs:63-190` `warn!` → `debug!` — channel send errors during normal shutdown
- `browser.rs:114-186` `warn!` → `debug!` — wrong-browser variant key routing
- `app.rs:125,241` `info!` → `debug!` — task spawn/finish (fires per async task)
- `api.rs:342-622` `error!` → `warn!` — recoverable API query failures
- `async_rodio_sink.rs:48,95` `error!` → `debug!` — normal autoplay/queue transitions

**Effort:** S (30 min)
**Status:** ✅ Done

---

## P2 — Code Quality

### P2.1 — Remove dead decoder integration test file

**What:** Delete `decoder_integration_test.rs`. Its coverage is subsumed by `song_downloader.rs` tests.

**Why:** Dead code. Confuses navigation.

**AC:** File deleted. `cargo test` passes. Nothing references it.

**Dep:** None
**Effort:** XS (5 min)
**Status:** ✅ Done

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

### P2.7 — `download_and_decode()` 536-line monolith extraction

**Files:** `song_downloader.rs:88-624`

**What:** Function has 3 async sub-pipelines (URL-cache fast path, ffmpeg relay, M4A direct), 6+ nesting levels, and ~80% duplicated structure between paths.

**Why:** Maintainability — any change to the download pipeline risks missing one of the duplicated paths.

**AC:**
- Extract stdout writer pattern → `spawn_stdout_writer()`
- Extract stderr monitor pattern → `spawn_stderr_monitor()`
- Extract "wait for completion + cache" postlude → helper
- All paths share the same helpers
- Tests pass, no behavior change

**Dep:** None
**Effort:** M (1-2 days)
**Status:** 🔜

---

### P2.8 — `get_artist_songs()` 241-line async nest

**File:** `api.rs:330-571`

**What:** Nested closures, 5+ match levels, inline `PerAlbumResult` enum, `FuturesUnordered` — undocumented async logic with zero tests for the orchestration.

**Why:** Maintainability — undocumented, untested.

**AC:**
- Extract per-album async future into named fn
- Add unit tests for the extracted helper

**Dep:** None
**Effort:** S (1 day)
**Status:** 🔜

---

### P2.9 — Remove dead `#[allow(dead_code)]` items (18 annotations)

**Files:** `async_rodio_sink.rs`, `messages.rs`, `player.rs`, `streaming_buffer.rs`, `decoder/mod.rs`, `core.rs`, `drawutils.rs`, `widgets/`, `actionhandler.rs`, `footer.rs`

**What:** `queue_song` feature is fully wired but never called. `map_to_play_update`, `map_to_queue_update`, `map_to_autoplay_update` duplicate inline match logic. `wait_for_total_len` is dead and dangerous (sync Condvar in async context). Various unused fields/functions.

**Why:** Dead code confuses navigation, litters warnings.

**AC:**
- Remove `QueueSong` variant/impl/method from async_rodio_sink, messages, player
- Remove `map_to_*` functions (3)
- Remove `wait_for_total_len` (dead, uses sync Condvar — dangerous)
- Remove unused fields/widgets/helpers
- Verify no compilation errors

**Dep:** None
**Effort:** S (1 day)
**Status:** ✅ Done

---

### P2.10 — Merge `handle_play_update` / `handle_autoplay_update`

**File:** `playlist.rs:1671-1689`, `effect_handlers.rs:66-71,135-138`

**What:** `handle_play_update` was a 1-line delegating wrapper (`self.handle_autoplay_update(update.into())`). `handle_autoplay_update` contained the real implementation with all match branches. The separate `PlaylistEffect::HandlePlayUpdate` enum variant and its dispatch case duplicated the routing logic.

**Why:** Reduces maintenance surface — eliminates dead delegation and enum variants.

**AC:**
- `handle_play_update` method removed
- `PlaylistEffect::HandlePlayUpdate` variant removed
- `PlayUpdate` → `AutoplayUpdate` conversion moved into the effect handler closure for `HandlePlayUpdateOk`
- Tests pass

**Dep:** None
**Effort:** XS (15 min)
**Status:** ✅ Done

---

### P2.11 — Unify duplicate OMV resolution

**Files:** `api.rs:647-689`, `messages.rs:233-274`

**What:** Both `resolve_to_audio_track()` in api.rs and `ResolveSongToAudio::into_future()` in messages.rs implemented the same algorithm: `search_songs` → check is_audio_track + title + artist → `search_broad` fallback → return.

**Why:** DRY — duplicated code diverges silently.

**AC:**
- `resolve_to_audio_track` made the canonical implementation (takes title/artist/raw_id, returns `Option<VideoID>`)
- `ResolveSongToAudio::into_future` now delegates to `resolve_to_audio_track`
- `search_broad` method removed from `Api` struct (dead code — callers use free function directly)
- Old `search_songs` + `search_broad` free functions remain behind the shared helper
- Tests pass

**Dep:** None
**Effort:** XS (30 min)
**Status:** ✅ Done

---

### P2.12 — Magic numbers → named constants

**Files:** `song_downloader.rs`

**What:** `64 * 1024` (read buffer, 12+ occurrences), `120` (download timeout, 6+), `1024` (stream init threshold, 2), `270` (WAV header size, comments only), `5` (decoder init deadline, 3+).

**Why:** Maintainability — magic numbers are undocumented.

**AC:**
- `const READ_BUF_SIZE: usize = 64 * 1024`
- `const DOWNLOAD_TIMEOUT_S: u64 = 120`
- `const STREAM_INIT_THRESHOLD: usize = 1024`
- `const DECODER_INIT_DEADLINE_S: u64 = 5`
- `const M4A_TOTAL_LEN_TIMEOUT_S: u64 = 15`
- Replace all occurrences

**Dep:** None
**Effort:** XS (30 min)
**Status:** ✅ Done

---

### P2.13 — Poisons & unwraps (crash safety)

**Files:** `song_downloader.rs:83`, `streaming_buffer.rs:65,174`, `decoder/mod.rs:218,223`

**What:** `URL_CACHE.lock().unwrap()` panic risk on poisoned mutex. `cvar.wait_timeout().unwrap()` — waits indefinitely if poisoned. `inner.total_len.unwrap()` — panics if total_len not set. `NonZero::new(2u16).unwrap()` — always safe but non-idiomatic.

**Why:** Crash safety — poisoned mutexes cause cascade panics.

**AC:**
- `song_downloader.rs:83` → `unwrap_or_else(|e| e.into_inner())`
- `streaming_buffer.rs:65` → proper error handling
- `streaming_buffer.rs:174` → `.expect("total_len must be set before seek")`
- `decoder/mod.rs:218,223` → `const` `NonZero` or `NonZero::MIN`

**Dep:** None
**Effort:** XS (15 min)
**Status:** ✅ Done

---

### P2.14 — Fire-and-forget `tokio::spawn` (lost panics)

**Files:** `messages.rs:216,335,476,501,526`, `song_downloader.rs:544`

**What:** Spawned tasks are not bound to variables — panics are silently swallowed.

**Why:** Debuggability — silent failures hide bugs.

**AC:**
- Bind `JoinHandle` and log `JoinError` on panic
- At minimum, add `// fire-and-forget: panics here are benign because …` comment documenting intent

**Dep:** None
**Effort:** S (half day)
**Status:** ✅ Done (added fire-and-forget comments to 8 unbound spawns: messages.rs ×4, song_downloader.rs ×3, api.rs ×1)

---

### P2.15 — `needless_range_loop` in messages.rs

**Files:** `messages.rs:192,213`

**What:** `for idx in 0..top_count { results[idx] }` → use `.iter().enumerate().take(top_count)`.

**Why:** Idiomatic Rust.

**AC:** Replace with iterator chain
**Effort:** XS (5 min)
**Status:** ✅ Done

---

### P2.16 — Missing `#[must_use]` on pure functions (26 functions across 8 files)

**Files:** `streaming_buffer.rs`, `scrolling_list.rs`, `tab_grid.rs`, `scrolling_table.rs`, `player.rs`, `view.rs`, `api.rs`, `server/api.rs`

**What:** Pure `fn(&self) -> T` and constructor/public functions returning values without side effects are missing `#[must_use]`.

**Why:** Prevents callers from accidentally discarding return values.

**AC:** Add `#[must_use]` to all identified functions
**Effort:** XS (15 min)
**Status:** ✅ Done

**Files:** `streaming_buffer.rs`, `decoder/mod.rs`, `widgets/`, `player.rs`, `view.rs`, `api.rs`

**What:** ~15 pure `fn(&self) -> T` functions that return a value without side effects are missing `#[must_use]`.

**Why:** Best practice — prevents callers from accidentally discarding return values.

**AC:** Add `#[must_use]` to all identified functions
**Effort:** XS (15 min)
**Status:** ✅ Done (player.rs: 9 async functions annotated)

---

### P2.17 — `String` error types → `anyhow`

**Files:** `song_downloader.rs:426,488,492,499,586,590,597,607`

**What:** Multiple `Err(format!(...))` and `.map_err(|e| format!(...))` calls use `String` as error type, losing original error context (`.source()`). The project standardizes on `anyhow` everywhere else.

**Why:** Best practice — `String` errors lose chain context.

**AC:**
- Replace `Result<T, String>` with `anyhow::Result<T>` in `download_and_decode`, `try_streaming_init`, `create_decoder_from`
- Use `anyhow::{bail, Context}` (`.context("msg")` / `bail!(...)`)
- `search_broad` method removed from `Api` struct (dead code)
- Call site in `messages.rs` converts anyhow error to `String` with `.to_string()`

**Dep:** None
**Effort:** S (half day)
**Status:** ✅ Done

---

### P2.18 — `.into_iter().next()` → `.first()` in querybuilder

**File:** `cli/querybuilder.rs:663,670,704,711`

**What:** `sources.into_iter().next().unwrap_or_default()` constructs an iterator just to get index 0. Use `sources.first().cloned().unwrap_or_default()`.

**Why:** Idiomatic — avoids iterator allocation.

**AC:** 4 replacements
**Effort:** XS (5 min)
**Status:** ✅ Done

---

### P2.19 — Unused test imports & dead test code

**Files:** `playlist/tests.rs:6`, `tests.rs:9,11,13`

**What:** Unused `PlayMode` import, `.nth(0)` → `.next()`, dead `COOKIE_PATH`/`API`/`get_api` in integration tests.

**Why:** Cleanliness — warnings from dead test code.

**AC:** 5 changes
**Effort:** XS (5 min)
**Status:** ✅ Done

---

### P2.20 — Merge `cache_put`/`CACHE_ORDER` behind single Mutex

**Files:** `song_downloader.rs:28-38`

**What:** `cache_put()` acquires `BYTE_CACHE` then `CACHE_ORDER` sequentially (2 lock round-trips). Risk of deadlock if future code reverses order.

**Why:** Safety + perf — single lock is faster and deadlock-free.

**AC:**
- `struct AudioCache { data: HashMap<String, Vec<u8>>, order: VecDeque<String> }` behind single `Mutex`
- `cache_put` / `cache_get` operate on the same lock
- All tests pass

**Dep:** None
**Effort:** S (half day)
**Status:** ✅ Done

---

## P3

### P3.1 — Compact autosave: `to_string_pretty` → `to_string`

**File:** `queue_persistence.rs:88`

**What:** Autosave JSON uses `serde_json::to_string_pretty` producing 20MB files for 58k songs. Switch to compact `to_string()`.

**Why:** Smaller file (~10MB), faster writes, faster reads.

**AC:** Autosave file is ~50% smaller. Loading still works (JSON is valid either way). No data loss.

**Dep:** None
**Effort:** XS (15 min)
**Status:** ✅ Done

**Files:** `song_downloader.rs`, `streaming_buffer.rs`, `decoder/mod.rs`

**What:** `cache_get()` clones the entire audio buffer (up to 50+ MB for WAV) on every cache-hit decoder init. Store `Arc<Vec<u8>>` in the cache so clones are refcount bumps (~8 bytes). `SharedBuffer::data()` similarly returns `Arc<Vec<u8>>` instead of `Vec<u8>`.

**Why:** P0 perf — eliminates repeated 50MB heap allocations during song replay.

**AC:**
- `BYTE_CACHE` stores `Arc<Vec<u8>>`, `cache_get()` returns `Arc<Vec<u8>>`
- `SharedBuffer::data()` returns `Arc<Vec<u8>>` (wraps existing `data` field into `Arc`)
- `SharedBuffer::data()` callers updated
- `Arc::make_mut()` used for writer paths to avoid copy-on-write on every write
- All tests pass

**Dep:** None
**Effort:** S (half day)
**Status:** ✅ Done

---

### P3.5 — `SampleBuffer` reuse in decoder (40 fewer allocs/sec)

**File:** `decoder/mod.rs:182-183`

**What:** `SampleBuffer::new(num_frames, spec)` allocates a new heap buffer on every decoded packet (~38-43/sec). Use `resize()` to reuse the existing buffer when capacity suffices.

**Why:** P0 perf — reduces heap alloc churn in the audio decode hot path.

**AC:**
- Replace `self.buffer = SampleBuffer::new(num_frames, self.spec)` with `self.buffer.resize(num_frames, self.spec)` when buffer already exists
- Arm for the initial creation (no `resize` on `SampleBuffer` before any allocation — only `new` for the first time)
- `copy_interleaved_ref` still works identically on resized buffer
- Audio output is bit-exact identical (test by ear or by checksum)

**Dep:** symphonia 0.5 `SampleBuffer` has no `resize()` method — only `new()`.
**Effort:** XS (15 min)
**Status:** ❌ Wontfix (symphonia 0.5 lacks `resize()`)

---

### P3.6 — Row number formatting cache (60k allocs/sec at 60fps)

**File:** `playlist.rs:400`

**What:** `get_items()` is called every render frame. For every non-playing row, `(visual_i + 1).to_string()` allocates a `String` on the heap. With 1000 songs at 60fps = 60,000 allocs/sec.

**Why:** P0 perf — reduces GC pressure and frame render time.

**AC:**
- Pre-format row numbers into a `Vec` cache when playlist changes
- Cache invalidated on: song push, delete, shuffle, search filter change
- `get_items` uses cached `Cow::Borrowed` values
- No measurable increase in song-add latency

**Dep:** None
**Effort:** S (half day)
**Status:** ✅ Done

---

### P3.7 — `RwLock` for `SharedBuffer` (reduce audio-thread lock contention)

**File:** `streaming_buffer.rs`

**What:** `Mutex` is locked on every audio `read()` call in the playback thread (hundreds of times/sec), contending with the ffmpeg writer. A single writer + N readers pattern favors `RwLock` for the data field.

**Why:** P1 perf — reduces lock contention between audio decode thread and download pipeline.

**AC:**
- `SharedBufferInner.data` protected by `RwLock` instead of `Mutex`
- `finished`/`failed`/`total_len` fields can stay under `Mutex` (rarely read)
- Reader path acquires read lock (no contention with other readers)
- Writer path acquires write lock
- All tests pass

**Dep:** None
**Effort:** S (half day)
**Status:** ✅ Done

---

### P3.8 — Cache playlist title string

**File:** `playlist.rs:461-468`

**What:** `get_title()` calls `format!("Local playlist - N songs[Q:...][SHUFFLE][SEARCH: ...]")` every render frame. Cache and invalidate on mutation.

**Why:** P1 perf — eliminates format! alloc every render cycle.

**AC:**
- `cached_title: Option<String>` field on Playlist
- Invalidated on: `push_song_list`, `toggle_shuffle`, `toggle_search`, `cycle_audio_quality`
- `get_title()` returns `Cow::Borrowed` from cache

**Dep:** None
**Effort:** XS (15 min)
**Status:** ✅ Done

---

### P3.9 — Artist string

**File:** `draw_media_controls.rs:44`

**What:** `Itertools::intersperse(...).collect::<String>()` rebuilds the full artist string from `Vec<ListSongArtist>` every 100ms for desktop integration.

**Why:** P2 perf — medium CPU in the hot path.

**AC:**
- Use cached `ListSong::artists_string` instead of rebuilding
- Verify `artists_string` is populated at song creation time

**Dep:** None
**Effort:** XS (5 min)
**Status:** ✅ Done

---

### P3.10 — `_audio_quality`

**File:** `song_downloader.rs:91`

**What:** `_audio_quality: AudioQuality` parameter is threaded through the entire download pipeline but never used — format selection is hardcoded in format strings.

**Why:** P2 — dead code parameter, makes API misleading.

**AC:**
- Remove parameter from `download_and_decode` and all callers
- If future quality-based format selection is planned, add TODO with reference

**Dep:** None
**Effort:** XS (15 min)
**Status:** ✅ Done

---

**File:** `manager.rs:176`

**What:** `DEFAULT_STREAM_CHANNEL_SIZE = 20` hardcoded. 20 is small for high-throughput audio events.

**Why:** Potential backpressure under heavy seeking / rapid song switches.

**AC:** Increase to `256` (consistent with rodio queue sizes) or make configurable. Verify no adverse memory impact (256 * message_size ≈ negligible).

**Dep:** None
**Effort:** XS (15 min)
**Status:** ✅ Done

---

### P3.3

**File:** `server.rs`

**What:** `RwLock` write lock held across entire HTTP request duration serializes concurrent API queries.

**Why:** Album fetch + search run in parallel today but the RwLock serializes them.

**AC:** Measure current contention (add metrics or log). If significant, use `Arc<DynamicYtMusic>` + clone-on-write or split reader/writer. If negligible, close as wontfix.

**Dep:** None
**Effort:** S (1 day to investigate + fix if needed)
**Status:** ✅ Done

---

## P4

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
