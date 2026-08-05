# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 553 youtui/ytmapi-rs passed + 20 ignored
**Last updated:** 2026-08-04

## Completed

### Session 2026-08-04 — Skip-loop fix, API drift tolerance, cookie-export hardening

- **Auth with browser running (18+ skips, closed)** — the export hard-failed on the locked profile (browser open) leaving a 0-byte/stale Netscape file → guest-only `--add-header` → age-restricted songs skipped. Root-caused via live dry-runs: yt-dlp's `--cookies-from-browser` *resolve* flow always yields storyboards (no audio), but the app's Netscape-file → `--add-header Cookie:` mechanism returns `251 opus` with signed-in cookies. Fix (`1154fdc`): `run_cookie_export` falls back to copying `cookies.sqlite`(+WAL) to a temp profile and exporting from the copy when the live DB is locked or empty. Copy is never locked → auth works with Floorp open. End-to-end verified live (Floorp running → 1378-line export with SID → header → `251 opus`). New test `run_cookie_export_falls_back_to_copied_profile_when_locked` (fail-first).
- **Dead stream-URL loop (song skips)** — `resolve_url` caches a stream URL (6h). When a cached URL died, ffmpeg got 0 bytes with nulled stderr (buffer never marked failed) and the `empty pipe after 5s` bail fired **without evicting the cache** → every retry reused the dead URL forever (observed: same song ×4 identical skips). Fixed: empty-pipe bail now evicts via `url_cache_remove`; 3 duplicate eviction sites consolidated onto the helper (mod.rs/resolve.rs, `e75d207`); fast-fail ffmpeg bail also evicts (`4dd54ab`).
- **Long songs skip mid-song** — `spawn_bg_cache_task` hard-capped the *post-playback* download at `DOWNLOAD_TIMEOUT_S = 120s` and `kill_and_reap`'d ffmpeg on expiry. For a long song whose WAV download+transcode exceeds 120s, killing ffmpeg closed the pipe → `writer.finish()` → buffer EOF → decoder truncated mid-playback. Fixed (`997be06`): no absolute deadline — the bg task now kills only when the buffer stops growing for `BG_STALL_TIMEOUT_S = 60s` (`track_download_progress` helper, 3 unit tests). Pre-playback `DOWNLOAD_TIMEOUT_S` sites (init-fallback + M4A) unchanged — those skip before playing, not mid-song.
- **AlbumType drift (`Upcoming Album`)** — YouTube began sending unknown album categories; strict serde made the *whole GetAlbumQuery* fail. Fixed with a split design: `AlbumType` keeps a **strict** custom `Deserialize` (search classification relies on unknown→`None`), plus a new `Other` variant + `album_type_from_text` lenient mapper used only by album/artist paths (common.rs, album.rs, artist.rs). First attempt with `#[serde(other)]` broke 7 search tests — reverted.
- **GetArtistQuery (R2 → closed)** — artist page missing `sectionListRenderer/contents` failed the whole artist fetch. Section list is now optional → artist loads with empty top-releases. (Was confirmed hitting in logs: `ArtistChannelID UCJK0856RNSlCDFNK4XbLIiw`.)
- **Cookie-export hardening** (4 commits):
  - `--ignore-config` — host yt-dlp config with 0-byte `cookies.txt` was hard-failing every download.
  - Export honors `config.yt_dlp_command` (was hardcoded `yt-dlp`) with the same `""`→fallback as downloads.
  - `run_cookie_export` now captures yt-dlp **stderr** — the WARN shows *why* an export failed (locked browser, unsupported DB) instead of a bare "Failed".
  - **Atomic export** — writes to `<dest>.tmp`, publishes via rename only on success/non-empty. A failed re-export (browser running at startup) no longer nukes the last good export to 0 bytes — the historical 0-byte trap is closed for good.
- **Render-time panic elimination** (P0) — `draw.rs` per-frame `expect(visual_to_actual_index out-of-bounds)` → empty `Cow` row + `debug!`; `shared_components.rs` suggestions `expect` → bounds-checked `.get()`. Regression test `draw_get_items_survives_desynced_shuffle_map`.
- **Notification consolidation** — duplicate `notify_rust` spawn blocks merged into `spawn_notification(summary, body, timeout_ms)`; auth bail now tags `po_token set|no po_token`; `short_type_name` `unwrap` → `unwrap_or`.

### Session 2026-07-31 — Dep pruning, module splits, Phase 8 close

- **Dep feature pruning** — ratatui `all-widgets` dropped; tokio `full` → explicit features; ytmapi-rs `default-features = false` + `simplified-queries`; rodio `symphonia-all` → `{wav, pcm, isomp4, aac}`. Cargo.lock −80 lines; redundant decoder crates (claxon, minimp3, lewton, hound, rtrb, rand_distr) gone. TLS: rustls-only (native-tls/openssl absent from resolved graph, no system ssl linkage).
- **Test fixture swap** — rodio lost `symphonia-mp3`, so `TEST_MP3` → `TEST_WAV` (`ytmapi-rs/test_json/test_silence.wav`, 1 MB, 12s 44100Hz mono); ffmpeg relay test now `-f wav`. Cache-test race fixed with `CACHE_TEST_LOCK` (serializes 3 shared-`BYTE_CACHE` tests).
- **Module splits** — `song_downloader.rs` → `song_downloader/{mod, cache, resolve}.rs`; `playlist.rs` → `playlist/{mod, draw, playback}.rs`; `parse/search.rs` → `parse/search/{mod, types}.rs`.
- **Seek removal** — dead `SeekForward/SeekBack`, `try_seek`, `SeekDirection`, MPRIS seek handlers stripped.
- **Phase 8 — symphonia 0.6.0 investigated → ❌ rejected** — rodio 0.22.2 (latest) pins symphonia 0.5.5; a direct 0.6 bump creates a dual-symphonia tree duplicating the codec stack. 0.6's additions (video/subtitle groundwork, metadata, SIMD) irrelevant to a WAV/AAC-only player. See AGENTS.md + DECISIONS.md:21. All phases now ✅.

### P1 — Should Have (UX / Configurability)

- **P1.1** — Configurable download cache size (`download_cache_size` in config.toml, `AtomicUsize`).
- **P1.2** — Status bar streaming indicator (`status_bar_icon()` on Playlist, 9-state exhaustive match).
- **P1.4** — Hot path log demotion (`info!` → `trace!` on `current_span_len`).
- **P1.5** — Per-keystroke log demotion (`info!` → `debug!` in async_rodio_sink, 15 sites).
- **P1.6** — Per-download pipeline noise (`info!` → `debug!`, `info!` → `error!`, `info!` → `warn!` in song_downloader).
- **P1.7** — Per-event noise (`warn!` → `debug!`, `error!` → `warn!` across appevent, browser, app, api).

### P2 — Code Quality

- **P2.1** — Remove dead `decoder_integration_test.rs` (coverage subsumed by song_downloader tests).
- **P2.2** — Deduplicate code generators: deleted `gen_output.rs` + `gen_output/`, kept `gen_expected.rs`.
- **P2.3** — English-only browser UA check: removed `contains` on localized YouTube error message. INNERTUBE_CLIENT_VERSION fallback works for all locales. Removed dead `InvalidUserAgent` error variant.
- **P2.4** — `type_name::<A>()` → short type names via `rsplit("::")`. Added `short_type_name` helper in `error.rs`.
- **P2.6** — `resolve_omv_crossref` extraction: shared loop + closure-based lookup, both callers delegate.
- **P2.7** — ⏭ Extracted as part of P2.6/P2.8 (partial: shared pattern extraction + named future extraction). Full monolith split deferred.
- **P2.8** — `get_artist_songs` async nest: extracted `fetch_and_resolve_album` named fn, moved `PerAlbumResult` to module level.
- **P2.9** — Dead code removal: `QueueSong` variant/impl/method, 3 `map_to_*` functions, `#[allow(dead_code)]` items, `handle_queue_update`, `re_enqueue_downloaded_song`, unused fields.
- **P2.10** — Merged `handle_play_update` → `handle_autoplay_update` via `From<PlayUpdate> for AutoplayUpdate`. Removed `HandlePlayUpdate` effect variant.
- **P2.11** — Unified `resolve_to_audio_track` + `ResolveSongToAudio`. Removed `search_broad` method from `Api`.
- **P2.12** — Magic numbers → named constants (`READ_BUF_SIZE`, `DOWNLOAD_TIMEOUT_S`, `STREAM_INIT_THRESHOLD`, `DECODER_INIT_DEADLINE_S`, `M4A_TOTAL_LEN_TIMEOUT_S`).
- **P2.13** — Poisons & unwraps: `unwrap_or_else(|e| e.into_inner())` on all mutex locks.
- **P2.14** — Fire-and-forget `tokio::spawn` documented (8 sites across messages, song_downloader, api).
- **P2.15** — `needless_range_loop` → `.iter().enumerate().take()` in messages.rs.
- **P2.16** — `#[must_use]` on ~26 pure functions across 8 files.
- **P2.17** — All `String` error types → `anyhow::{bail, Context}` in song_downloader (24 sites).
- **P2.18** — `.into_iter().next()` → `.first()` in querybuilder (4 sites).
- **P2.19** — Unused test imports: `PlayMode`, `.nth(0)`, dead `COOKIE_PATH`/`API`.
- **P2.20** — Merged `BYTE_CACHE` + `CACHE_ORDER` behind single `AudioCache` struct.
- **P2.5** — Triplicated song-clone-and-callback pattern: 4 standalone generic helpers in `shared_components.rs`, 12 thin wrapper methods. Net -92 lines.

### P3 — Performance

- **P3.1** — Compact autosave: `serde_json::to_string_pretty` → `to_string` (~50% smaller files).
- **P3.3** — Server RwLock: moved `query_api_with_retry` to free fn taking `&ConcurrentApi` instead of `&Server`.
- **P3.4** — `Arc<[u8]>` for Byte Cache: cache hits avoid cloning 50MB+ buffers.
- **P3.6** — Row number formatting cache: `cached_row_numbers: Vec<String>` rebuilt on playlist mutation.
- **P3.7** — `RwLock` for `SharedBuffer.data` (single writer + N readers pattern).
- **P3.8** — Playlist title cached (`RefCell<Option<String>>`, invalidated on mutation).
- **P3.9** — Artist string cached on `ListSong::artists_string` (media controls use cached field).
- **P3.10** — Removed dead `_audio_quality` parameter from `download_and_decode`.
- **P3.12** — `DEFAULT_STREAM_CHANNEL_SIZE`: 20 → 256.

### P4 — Testing / Infrastructure

- **P4.1** — ❌ Cancelled: single-user project, no CI needed.
- **P4.2** — Criterion benchmarks for `get_field` hot-path (6 benchmarks in `structures.rs::criterion_benches`, `cargo test --release`).
- **P4.3** — Edge case tests for `resolve_omv_*` and `build_search_map`: all-Atv untouched, non-Atv filtered, duplicate Atv title.
- **P4.4** — Artist album pagination: `ParseFromContinuable` for `Vec<GetArtistAlbumsAlbum>` tested with continuation mock + output. `get_artist_songs` already paginates with `usize::MAX`.

### Fixes During Development (Session 2026-07-08)

- URL pre-resolution architecture: `download_and_decode` calls `resolve_url` first → feeds URL directly to ffmpeg, bypassing rate-limited `-o -` yt-dlp path. Resolve runs inside semaphore.
- yt-dlp ERROR on stderr → `SharedBuffer::fail()` (early abort instead of 5s timeout).
- Prebuffer excludes currently-playing song from scope.
- `download_upcoming_from_id` excludes `Failed` songs from queue rebuild.
- `download_song` pops `Failed` songs from queue and advances to next.
- `cached_title` invalidation in `deduplicate()` and `clear()`.

### Phase 1 — Critical Bugs (2026-07-08)

- **C1** — Empty playlist panic in `get_items`: computes count directly instead of `get_max_visual_index().saturating_add(1)`.
- **C2/B1** — Empty search results show first queue track: `play_selected` early-returns when search active with 0 results.
- **C3** — `cancel_all_downloads` never calls `cancel()`: iterates and calls `token.cancel()` before clearing.

### Phase 2 — Medium Issues (2026-07-08)

- **M1** — `u16` overflow in progress bar calc (`view.rs`): cast to `u32`.
- **M2** — Broken `context("wait {pipe_name}")` (`song_downloader.rs`): `with_context(|| format!(...))` (2 sites).
- **M3** — Stale `active_downloads` entries: `cancel_out_of_scope_downloads` uses `retain` + cancels before removing.
- **M4** — No-audio-device panic: `AsyncRodio::new()` returns `Result`, oneshot channel signals init from `spawn_blocking`. Propagates through `Player::new()` → `Server::new()` → `Youtui::new()`.

### Phase 3 — Low Issues (2026-07-08)

- **L1** — Removed 7 dead code items + 10 tests: `send_or_error`, `create_or_clean_directory`, `touch_file_with_timestamp`, `get_dir_file_paths` in `core.rs`; `middle_of_rect` in `drawutils.rs`; `get_visible_keybinds_as_readable_iter` in `actionhandler.rs`.
- **L2** — Poison context logging: `PoisonRecovery` trait with `.unwrap_or_warn()` in `core.rs`. Replaced 42 `unwrap_or_else(|e| e.into_inner())` across 4 files.
- **L3** — Send failure logging (`messages.rs`): `.ok()` → `if let Err(e) = ... { warn! }`.
- **L4** — Autosave failure logging (`playlist.rs`): `let _ = ...` → `if let Err(e) = ... { warn! }`.
- **L5** — `Ordering::Relaxed` → `Acquire`/`Release` on `CACHE_MAX_ENTRIES`.
- **L6** — URL_CACHE TTL eviction: `HashMap<String, String>` → `HashMap<String, (String, Instant)>` with 6-hour TTL + helper functions.
- **L7** — Reqwest builder `.expect()` → `?` in `server.rs` (3 sites).
- **L8** — ❌ Cancelled: multi-byte CJK scroll ticker (cosmetic, documented TODO).

### Memory (from session 2026-07-08)

- Cache default 3 → 1 entry (WAV ~42MB each).
- LRU fix in `AudioCache::get` (touch on hit).
- `cache_clear()` on stop/clear/all-stopped events.
- Preloaded decoders cleared on stop.
- Cache-backed decoders preferred at `play_song`.
- `create_decoder_from_cache()` helper.
- `draw_text_box` dedup into `drawutils.rs`.

### Code Quality (from session 2026-07-08)

- 24 clippy warnings eliminated (collapsible `if` 7×, `double_must_use` 5×, `is_none_or`/`is_some_and` 2×, etc.).
- `check_ffmpeg()` → `LazyLock<bool>` inside function body — saves subprocess spawn per song.
- Footer truncate → `Cow<'_, str>` — saves heap alloc per frame when text fits.
- Footer uses pre-cached `artists_string` — eliminates per-frame artist iteration.
- `get_field_lower()` on `ListSong` — returns pre-lowercased `Song`/`Artists`/`Album` directly. Eliminates ~300 `to_ascii_lowercase()` allocs/frame.
- Filter string early-return — skips `", ".to_string()` + `.collect()` when no filter active.
- `Vec::with_capacity` in `push_song_list` filter + dedup — avoids reallocs on 500+ song lists.
- Channel capacity 1→3 in messages — eliminates background task blocking.
- `ytmapi-rs` doctest fixed: `ErrorKind::Header` → `ErrorKind::Header { .. }`.
- 4 `#[allow(dead_code)]` → `#[cfg(test)]` for test-only helpers.

### Phase 2 — Additional Fixes (2026-07-09)

- **C1** — Empty search results crash in `SearchSongs::into_future`: `results[0]` panics when results are empty. Added `if results.is_empty()` early return before index.
- **C2** — `handle_all_stopped` race overwrites new playback state: `handle_all_stopped` unconditionally cleared cache and set `play_status = Stopped` even if a new song started playing since `stop()` was called. Added guard: only clear if status is still `Stopped` or `NotPlaying`.
- **B1** — Gapless mechanism inverts condition: checked `DownloadStatus::Downloaded` then called `download_song` which re-downloaded. Changed guard to `!Downloaded && !Failed` so gapless only pre-downloads songs that aren't ready yet.
- **B2** — `Downloading` progress update sets `Queued` not `Downloading`: `DownloadStatus::Downloading(Percentage)` enum variant was dead code. Changed to `Downloading(Percentage(0))`.
- **M2-M3** — `cancel_out_of_scope_downloads` left stale `DownloadStatus` + `preloaded_sources` entries after cancelling tasks. Now resets status to `None` and removes from both `preloaded_sources` and `download_queue`.
- **M4** — Redundant `check_ffmpeg()` call: called at line 188 (warming) and line 195 (pipeline decision). Merged into single call stored in `ffmpeg_avail`.
- **M6** — Dead `_current_id` parameter in `get_next_song_id`/`get_prev_song_id`: both functions ignored the parameter and used `get_cur_playing_index()`. Removed parameter and updated 3 call sites.
- **M7** — Current song added then removed from scope in `download_upcoming_from_id`: started scope with `vec![id]` then immediately removed via `retain`. Changed to start with empty vec.
- **B5** — 6× `Client::new().expect()` in ytmapi-rs → `?`: changed return types of `from_browser_token`, `from_oauth_token`, `from_auth_token` to `Result`; switched `new_unauthenticated`, `from_cookie_file`, `from_cookie` to `?`. All 6 panic sites eliminated.
- **B6** — `play_song` silent no-op on missing ID: added `warn!("play_song called with unknown id {id:?}")` to the else branch.

## Session 2026-07-16 — Final Code Quality Sweep

### Log Noise Reduction (Cross-Codebase)

- **N3 — api.rs: 11× `info!` → `debug!`** — ✅ Done
  Per-request noise (search calls, album/playlist sends, OMV→Atv resolution). Remaining `info!` calls: auth ops, retry, error events only.

- **N4 — async_rodio_sink.rs: 2× `info!` → `debug!`** — ✅ Done
  `audio_output_started` per-song events.

- **N5 — decoder/mod.rs: 2× `info!` → `debug!`** — ✅ Done
  `SymphoniaDecoder created` + codec params (per-song noise).

- **N6 — messages.rs: 1× `info!` → `debug!`** — ✅ Done
  URL pre-resolve for top result (per-search noise).

- **N7 — effect_handlers.rs: 1× `info!` → `debug!`** — ✅ Done
  Queue song→Atv resolution event (per-song noise).

- **N8 — ui.rs: 2× `info!` → `debug!`** — ✅ Done
  Unhandled media control events.

Net: 19 `info!` → `debug!` demotions, 3 `info` imports removed. Remaining 10 `info!` calls are startup/auth/actionable events only.

### Cleanup

- **C3 — Fix `cached_items` dead_code warning** — ✅ Done
  Added `#[allow(dead_code)]` to planned search suggestions cache field in `shared_components.rs`.

### Verification

- **V1 — `cargo check` after each step; `cargo test` after all phases** — ✅ Done
  0 errors, 0 warnings, 0 clippy. 282 youtui + 98 ytmapi + 8 ytmapi doctests passed.

### Previously Completed (detected stale backlog)

- **N1/N2** — Already done: no `info!` calls remain in song_downloader.rs or playlist.rs.
- **C1** — Already done: no dead `handle` field in `AsyncRodio` struct.
- **C2** — Already done: `exit_code_string()` helper exists at line 24-26.
- **R1** — Already done: both sites already use `debug!` not `warn!`.

## Session 2026-07-17 — Cancellation Fixes, Latency, RAM, Log Noise

### P0 Fix

- **P0** — Cancel scope excludes current song: `download_upcoming_from_id`→`cancel_out_of_scope_downloads` was killing the current song's download token when `handle_playing` fired. Added current ID to cancel scope.

### Playback Latency

- **Semaphore contention** — Moved `resolve_url()` outside `DOWNLOAD_SEMAPHORE`. Previously a cancelled prebuffer holding the semaphore during 2-4s URL resolution blocked the user's song from starting. Now resolution runs in parallel.
- **Cancellation-aware resolve** — `resolve_url()` accepts `CancellationToken`; aborts mid-resolution via `tokio::select!` when cancelled, instead of running yt-dlp to completion.

### RAM

- **Removed N-1 from download scope** — Prebuffer no longer downloads the previous song for seek-back. Saves ~42MB per preloaded song. Seek-back requires re-download (was already mostly broken — cache evicts prev song).

### Log Noise

- **Cancellation vs real errors** — `is_cancellation_error()` helper distinguishes expected scope-change cancellations from genuine download failures. Cancellation = `debug!`, real = `warn!`.
- **UI guard warns demoted** — ~20 sites across songs_panel, songsearch, playlistsearch, artistsearch, tab_grid, messages: expected state transitions (wrong-mode keypress, send-failure on dropped receiver, notification failure) → `debug!`.

### Code Quality

- **`is_cancellation_error()` helper** — Consolidates fragile string match in one place.
- **Block cleanup** — Removed unnecessary block expression in `cancel_scope` construction.
- **Stale comment** — Updated scope comment (N-1 removed).
- **Unused `warn` imports** — Removed from 4 files.

### Open

- **R2 — `GetArtistQuery` JSON path error** — ✅ FIXED 2026-08-04 — missing `sectionListRenderer/contents` now tolerated (optional section list); closed in session 2026-08-04.

## Backlog

### P0 — Crash

| # | Item | Severity | Why not done |
|---|------|----------|-------------|
| C1 | `messages.rs:199` — `results[0]` on empty search results | **CRASH** | ✅ FIXED |
| C2 | `playlist.rs:1874` — `handle_all_stopped` race | **INCORRECT** | ✅ FIXED |

### P1 — Must Have (Incorrect Behavior)

| # | Item | Severity | Why not done |
|---|------|----------|-------------|
| B1 | Gapless re-downloads downloaded songs | **WASTE** | ✅ FIXED |
| B2 | `Downloading` status sets `Queued`, not `Downloading` | **WASTE** | ✅ FIXED |
| B3 | `_permit` dropped while background cache still streaming → parallel download | **WASTE** | ✅ FALSE ALARM — early release is intentional. Old bg task reads pipe (already-downloaded data), no new HTTP request. Next download starts immediately. |
| B4 | Stale `URL_CACHE` on pipeline failure wastes 2-4s retry | **WASTE** | ✅ FALSE ALARM — URL cache already invalidated on fast-path fallback (song_downloader.rs:287,291,299). Warming re-resolves if removed. |
| B5 | 6× `Client::new().expect()` in ytmapi-rs | **CRASH** | ✅ FIXED |
| B6 | `play_song` silent no-op on missing ID | **INCORRECT** | ✅ FIXED |

### P2 — Performance / Waste

| # | Item | Why deferred |
|---|------|-------------|
| M1 | Cache population race → redundant download | Race window is small, auto-resolves |
| M2-M3 | `cancel_*_downloads` leaves stale `Queued` status | ✅ FIXED — `cancel_out_of_scope_downloads` now resets `DownloadStatus`, clears `preloaded_sources` and `download_queue` for removed IDs |
| M4 | Redundant `check_ffmpeg()` call | ✅ FIXED |
| M5 | No iteration guard in decoder packet loop | ✅ FALSE ALARM — existing guards handle DecodeError skip, track_id mismatch, fatal error → EOS |
| M6 | Dead `_current_id` parameter | ✅ FIXED |
| M7 | Current song added/removed from scope | ✅ FIXED |
| M8 | 5ms blind sleep in seek | ✅ INTENTIONAL — gives audio device time to apply seek before `get_pos()` |
| M9 | Full-download fallback blocks semaphore 120s | Edge case (streaming fallback only) |

### P3 — Cleanup / Suckless

- **P3.1** — Removed logs view from TUI (`logger.rs`, `WindowContext::Logs`, `AppAction::ViewLogs`/`Log`, `tui-logger` dep, all 190 lines).
- **P3.2** — Removed dead `Drawable` trait (only used by Logger). Removed `logger.rs` file.
- **P3.3** — Demoted `SymphoniaDecoder: stream ended (EOF)` from `info!` to `debug!` (fires on every WAV song end).
- **P3.4** — Header format: `F1 (Toggle Help)` → `? Toggle Help` (no parens).
- **P3.5** — Default keybinds: all F-keys → home row (`q`=quit, `?`=help, `Tab`=toggle, `/`=search, `Esc`=close, `f`=filter, `o`=sort, `h/j/k/l`=navigation).
- **P3.6** — Esc closes search in playlist queue view.
- **P3.7** — Updated example `config/config.toml` to match new keybinds.

### P5 — Deferred (No Change)

| # | Item | Why deferred |
|---|------|-------------|
| L1 | Download waste after stop during Buffering | Cosmetic, single download |
| L2 | `resolve_url()` return discarded | Warming call is best-effort |
| L3 | ProgressSource on audio thread blocks | 50-slot buffer gives 5s slack |
| L4 | Audio device disconnect no recovery | Requires jack/pipewire supervision |
| L5 | Misleading comment in streaming_buffer | Cosmetic |
| L6 | `SystemTime::now().duration_since(UNIX_EPOCH)` expect in ytmapi-rs | Pre-1970 clock, not realistic |

### P5 — Far Future (Not Ready)

| # | Item | Why blocked |
|---|------|-------------|
| F1 | Replace async-callback-manager with native Tokio | 29 usages, ~2000 line rewrite, unclear payoff. Needs dependency analysis. |
| F2 | Gapless playback | Blocked on symphonia AAC gapless support (upstream). Not actionable. |
| F3 | Mouse support | Needs ratatui MouseEvent impl across key stack. Pure scope. |
| F4 | Offline disk cache | ❌ **REJECTED** — streaming client, not offline jukebox. Serializing 42MB WAV buffers to disk on every shutdown wastes I/O and flash endurance. Song re-downloads faster than disk read. See DECISIONS.md:16. |
| F8 | Auth when browser is running | ✅ **RESOLVED** 2026-08-04 — yt-dlp's `--cookies-from-browser` *resolve* flow (and a direct locked-DB export) yield only storyboards (no audio). The app's real mechanism — Netscape file → `--add-header Cookie:` — **does** return `251 opus` with signed-in cookies. Fix: `run_cookie_export` now falls back to exporting from a **copy** of the profile's `cookies.sqlite`(+WAL) when the live DB is locked/empty, so auth works with the browser open. End-to-end verified live. (`1154fdc`.) |
| F5 | Display lyrics | ❌ **REJECTED** — no new features. Suckless music player. See AGENTS.md. |
| F6 | Theming | ❌ **REJECTED** — no new features. Suckless music player. See AGENTS.md. |
| F7 | Stats Tab | ❌ **REJECTED** — no new features. Suckless music player. See AGENTS.md. |

## Legend

| Icon | Meaning |
|------|---------|
| 🔜 Pending | Ready to work on |
| ✅ Done | Completed |
| ❌ Cancelled | Declined |

**Effort:** XS (< 1h), S (< 1 day), M (3-5 days), L (1-2 weeks)
