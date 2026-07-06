# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 268 youtui + 97 ytmapi-rs unit passed
**Last updated:** 2026-07-06

## Completed

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
- **P4.3** — Edge case tests for `resolve_omv_*` and `build_search_map`: all-Atv untouched, non-Atv filtered, duplicate Atv title.

### Fixes During Development

- URL pre-resolution architecture: `download_and_decode` calls `resolve_url` first → feeds URL directly to ffmpeg, bypassing rate-limited `-o -` yt-dlp path. Resolve runs inside semaphore.
- yt-dlp ERROR on stderr → `SharedBuffer::fail()` (early abort instead of 5s timeout).
- Prebuffer excludes currently-playing song from scope.
- `download_upcoming_from_id` excludes `Failed` songs from queue rebuild.
- `download_song` pops `Failed` songs from queue and advances to next.
- `cached_title` invalidation in `deduplicate()` and `clear()`.

## Backlog

### P0 — Must Have (Playback UX)

None remaining.

### P1 — Should Have

None remaining.

### P2 — Code Quality

#### P2.5 — Triplicated song-clone-and-callback pattern

**Files:** `songsearch.rs` / `artistsearch.rs` / `playlistsearch.rs`

**What:** ~50 lines of the "clone song, send callback with progress" pattern duplicated 3× with minor variations. Previous extraction failed due to RPITIT + self-in-macro limitations.

**Why:** DRY. Reduces maintenance surface.

**AC:** Single shared implementation. All 3 call sites use it. Tests pass. No new trait constraints leaked into callers.

**Effort:** M (2-3 days, risk of failure again)
**Status:** 🔜

### P4 — Testing / Infrastructure

#### P4.2 — Criterion benchmarks for `get_field` hot-path

**File:** `benches/` (new)

**What:** Perf regression detection for `get_field` which is called per frame.

**Why:** Any change to `get_field` that causes allocation degrades frame rendering.

**AC:**
- `cargo bench` runs and produces stable measurements
- Benchmarks cover: `Artists` (Cow::Borrowed), `Title`, `Album`, `Duration`
- Baseline stored in repo or compared against previous run

**Effort:** S (1 day)
**Status:** 🔜

#### P4.4 — Artist album pagination

**What:** `GetArtistAlbumsQuery` only returns first page. Needs `ParseFromContinuable` impl for the album response type.

**Why:** Artists with many albums show incomplete discography.

**AC:**
- `GetArtistAlbumsQuery` implements `ParseFromContinuable`
- Pagination wired in browser: user scrolls past last album → fetches next page
- Existing continuation token from API response is used
- All pages loaded before reaching end of list

**Effort:** M (3-5 days, needs API reverse-engineering)
**Status:** 🔜

### P5 — Far Future (Not Ready)

| # | Item | Why blocked |
|---|------|-------------|
| F1 | Replace async-callback-manager with native Tokio | 29 usages, ~2000 line rewrite, unclear payoff. Needs dependency analysis. |
| F2 | Gapless playback | Blocked on symphonia AAC gapless support (upstream). Not actionable. |
| F3 | Mouse support | Needs ratatui MouseEvent impl across key stack. Pure scope. |
| F4 | Offline disk cache | Serialize `BYTE_CACHE` to disk on shutdown. Needs shutdown hook. |
| F5 | Display lyrics | Requires GetLyrics integration + new UI component. Pure scope. |
| F6 | Theming | Color scheme config. Pure scope. Low demand. |
| F7 | Stats Tab | CPU/memory/cache metrics in TUI. Pure scope. |

## Legend

| Icon | Meaning |
|------|---------|
| 🔜 Pending | Ready to work on |
| ✅ Done | Completed |
| ❌ Cancelled | Declined |

**Effort:** XS (< 1h), S (< 1 day), M (3-5 days), L (1-2 weeks)
