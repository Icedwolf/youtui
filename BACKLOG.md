# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 259 youtui + 98 ytmapi-rs unit passed; 49 ytmapi-rs live integration failed (pre-existing YT API drift)
**~400 pub functions** across youtui
**Last updated:** 2026-06-17

---

## 🛡️ Regression Guardrail Policy

1. **Compile-time tests preferred** — type-level assertions (`&mc.field`, `fn(_: &Type)`) that fail at compile time if invariants break, zero runtime cost.
2. **Hot-path changes require benchmarks** — any change to `get_field`, `compute_artists_string`, or `get_fields` must include a criterion benchmark proving no regression.
3. **Notification system is protected** — `NotificationController`, `notify_track_change()`, and its call in `update_metadata()` each have a compile-time test. Removing any breaks the build.
4. **Mako only supports `file://` URLs** — remote thumbnail URLs are silently skipped. Do not add thumbnail downloading to the notification path.
5. **Never add `--print` to yt-dlp download args when `-o -` is also used** — combo causes empty stdout (yt-dlp >=2026.04.10), silently producing zero-byte audio. Guarded by `test_build_stream_args_no_print_flag`.
6. **`VL` prefix lives in the query `header()`, not in callers** — `GetPlaylistTracksQuery::header()` and `GetPlaylistDetailsQuery::header()` auto-prepend `VL`. Callers pass raw `OLAK5uy_…` IDs. `PlaylistID` stays clean (no `VL` prefix).

---

## P0 — Must Preserve (Do Not Remove)

| # | File | Issue | Status |
|---|------|-------|--------|
| 1 | `media_controls.rs` | **Notification system** — `NotificationController` + `notify_track_change()` + call in `update_metadata()`. Was removed by `faf21ef`; restored. Has compile-time guard tests. | ✅ Guarded |
| 2 | `structures.rs` | **artists_string / track_no_string cached fields** — were removed by `7d88983` causing per-frame allocation regression; restored and guarded. `get_field(Artists)` must return `Cow::Borrowed`. | ✅ Guarded |
| 3 | `yt_dlp.rs` | **`--js-runtimes` must NOT be auto-detected.** Auto-detection spawns Node.js per yt-dlp instance → massive RAM waste. Only pass when user explicitly configures. | ✅ Fixed |
| 4 | `yt_dlp.rs` | **Video IDs starting with `-` need `--` separator** before URL arg (e.g. `-6FvsKo162U` for Placebo — Special K). yt-dlp interprets leading `-` as flag. | ✅ Fixed |
| 5 | `structures.rs` | **`deduplicate()` must be O(n) via HashSet.** Vec::contains O(n²) caused ~1.7B comparisons on 58k-song autosave. | ✅ Fixed |
| 6 | `structures.rs` | **`push_song_list()` dedup must use `HashSet<String>` (owned).** Borrow-vs-move conflicts make `HashSet<&str>` impossible in `retain` closure. | 🔒 Invariant |
| 7 | `yt_dlp.rs` | **`write_netscape_cookie_file()` must parse both Netscape (tab-separated) and Cookie: header formats.** Users' cookie.txt exports from Floorp/Firefox are often Netscape format. | ✅ Fixed |
| 8 | `yt_dlp.rs` | **`--no-check-formats` removed** — skips yt-dlp's format verification which can cause rodio-incompatible streams. Fallback chain `bestaudio[ext=m4a]/bestaudio/best` is sufficient. | ✅ Fixed |
| 9 | `yt_dlp.rs:267` | **`stream_song` stdout pipeline must NOT be tampered with.** `--print` on download command produces empty stdout → zero-byte audio → decode failure. Guarded by `test_build_stream_args_no_print_flag`. | ✅ Guarded |
| 10 | `api.rs` | **`VL` prefix is added by query `header()`, not by callers.** `GetPlaylistTracksQuery::header()` and `GetPlaylistDetailsQuery::header()` auto-prepend `VL` (with double-VL guard). All callers pass raw IDs without `VL`. | ✅ Fixed |

---

## P0 — Bugs (Previously Fixed This Session)

| # | File | Line | Issue | Fix |
|---|------|------|-------|-----|
| 1 | `shared_components.rs` | 85 | `SortAction::context()` returned `"Filter"` instead of `"Sort"` | `"Filter"` → `"Sort"` |
| 2 | `playlistsearch/songs_panel.rs` | 59 | `BrowserPlaylistSongsAction::context()` returned `"Artist Songs Panel"` | `"Artist Songs Panel"` → `"Playlist Songs Panel"` |
| 3 | `ui.rs` | 428-442 | `ListAction::First/Last` for `Logs` navigated browser through it | `self.browser.go_to_first/last()` → `()` |
| 4 | `api.rs` | 407-409 | `search_songs` failure logged at `debug!` only | `debug!` → `warn!` |
| 5 | `api.rs` | 473-478 | Audio-playlist fetch failure logged at `debug!` only | `debug!` → `warn!` |
| 6 | `yt_dlp.rs` | 354 | `stream_song` had no timeout — yt-dlp hang would leak forever | Added `timeout(Duration::from_secs(30), ...)` around spawn |
| 7 | `artistsearch/songs_panel.rs:98-109`, `playlistsearch/songs_panel.rs:94-106` | `.expect()` in `apply_all_sort_commands` would panic on mismatch | `.expect("...")` → `.ok_or_else(|| anyhow!(...))?` |
| 8 | `playlistsearch.rs:380` | Error message said "sorting album songs panel" | `"album"` → `"playlist"` |
| 9 | `artistsearch/songs_panel.rs:420-434`, `playlistsearch/songs_panel.rs:419-433`, `songsearch.rs:342-356` | `Loaded` state with 0 songs showed `"Songs - 0 results"` | Special-case `len == 0` → `"Songs - no songs found"` |

---

## P1 — Silent Failures / UX Gaps

| # | File | Line | Issue | Status |
|---|------|------|-------|--------|
| 1 | `yt_dlp.rs` | 376-386 | **yt-dlp non-zero exit logged at `warn!`.** Exit code surfaced via wait-task. Prevents zombie processes. | ✅ Fixed |
| 2 | `native.rs` | 48-56 | **No retry logic for transient network failures.** Single failure aborts download. | 🔜 |
| 3 | `config/keymap.rs` | 300+ | **Modal keybindings not shown in help.** Users can't discover modal binds without reading source. | ✅ Fixed |
| 4 | `config/keymap.rs` | 300+ | **Error messages use `type_name::<A>()`** producing unreadable names for complex generics. | 🔜 |
| 5 | `yt_dlp.rs` | 282 | **`--ignore-config` bypasses user yt-dlp config** (rate limits, extractors, proxy). Tradeoff: prevents user config from breaking stdout streaming. | 🔒 Invariant |
| 6 | `yt_dlp.rs` | 298-300 | **`BROWSER_SOURCE_CACHE` caches browser profile path.** yt-dlp reads fresh cookies at runtime from the cached profile. Low impact — profile changes mid-session are unlikely. | 🔒 Invariant |
| 7 | `media_controls.rs` | — | **Notifications always fire with no config to disable.** Noisy on headless/WSL. | ✅ Fixed — added `notifications_enabled = true/false` to config |
| 8 | `song_downloader.rs` | — | **Retry mismatch:** native=3 retries, yt-dlp=0. | ❌ False positive — both paths go through `download_song_using_downloader` with `MAX_RETRIES=5` |
| 9 | `api.rs`:260-267 | — | **Broad song-search limited to 20 results.** Falls back to audio-playlist for tracks not in top 20. | 🔜 |
| 10 | `api.rs:100` | — | **Dedup by title string, not video ID.** Two songs with same title could collide. | ✅ Fixed — both `build_search_map` and `resolve_omv_with_audio_playlist` now use `Entry::Vacant` with `warn!` on collision |

---

## P2 — Code Quality / Maintainability

| # | File | Issue | Status |
|---|------|-------|--------|
| 1 | `gen_output.rs` + `gen_output/src/main.rs` | **Functional duplicate** — two code generators for test output files; identical logic. | 🔜 |
| 2 | `ytmapi-rs/src/auth/noauth.rs:27-43` | **Fragile ytcfg parsing.** Uses `.split_once("ytcfg.set({")` — YouTube JS format changes will break silently. | ✅ Fixed |
| 3 | `ytmapi-rs/src/auth/browser.rs:69-71` | **English-only browser version check.** Localized versions break silently. | 🔜 |
| 4 | `PanickingReceiverStream` | **Panics on channel close** — `resume_unwind` aborts current task, too harsh for recoverable errors. | ✅ Fixed |
| 5 | `core.rs:32` + `async_rodio_sink.rs:847` | **`blocking_send_or_error` defined twice** (identical signatures). | ✅ Fixed — removed duplicate, import from core |
| 6 | `songsearch.rs` / `artistsearch.rs` / `playlistsearch.rs` | **Triplicated song-clone-and-callback pattern.** Previous extraction failed (RPITIT + self in macro). | ⏳ Postponed |
| 7 | `api.rs:118-161, 75-116` | **Both `resolve_omv_*` functions** share the same HashMap-based pattern. Extract common helper. | 🔜 |
| 8 | `ytmapi-rs/src/error.rs:38` | **`ErrorKind::Header` has no context string** — impossible to diagnose which header failed. | ✅ Fixed |
| 9 | `structures.rs:56-57` | **`ListSongID` has different layout in test vs release** (`#[cfg(test)] pub usize`). | 🔜 |
| 10 | `async-callback-manager/src/adapt.rs:22-30` | **Blanket impl gated on `not(task-equality)`** — enabling either feature breaks compilation. | 🔜 |

---

## P3 — Performance / Memory

| # | File | Issue | Status |
|---|------|-------|--------|
| 1 | `server.rs` | **`RwLock<DynamicYtMusic>` write-held across entire HTTP request** — serializes concurrent queries. | 🔜 |
| 2 | `api.rs:415-427` | **`FuturesOrdered` adds sequential overhead** — audio-playlist pass 2 for album N delays pass 2 for album N+1. | ✅ Fixed — switched to `FuturesUnordered` with index-based reordering |
| 3 | `api.rs:128-132` | **HashMap rebuilt per album** in cross-ref — build once before loop. | ✅ Fixed — `build_search_map` returns owned `HashMap<String, VideoID>` for `Arc`-sharing across concurrent futures |
| 4 | `ytmapi-rs/src/auth.rs:30-44` | **`RawResult` holds entire response as `String`** — clones via `from_str`. | 🔜 |
| 5 | `ytmapi-rs/src/client.rs:47` | **`response.text().await?` collects entire body** — memory-heavy. | 🔜 |
| 6 | `queue_persistence.rs:88` | **`to_string_pretty` for 20MB autosave** — pretty-printing wastes space. | ⏳ Future |
| 7 | `manager.rs:176` | **`DEFAULT_STREAM_CHANNEL_SIZE=20` hardcoded.** | ⏳ Future |

---

## P4 — Infrastructure / Testing Gaps

| # | File | Issue | Status |
|---|------|-------|--------|
| 1 | `.github/` | **No CI pipeline** — only `dependabot.yml`. No automated builds, tests, or linting. | 🔜 |
| 2 | `youtui/src/tests.rs:1-67` | **Only 1 integration test** (download, `#[ignore]`d). | 🔜 |
| 3 | `api.rs` | **`resolve_omv_*` with zero-Atv results** untested. | 🔜 |
| 4 | `api.rs` | **`GetAlbumsSongsError`** never tested. | 🔜 |
| 5 | `api.rs:390-393` | **Batch empty-list guard** untested. | 🔜 |
| 6 | `ytmapi-rs/tests/` | **54 live integration test failures** (YT API drift). | 🔒 External |

---

## Legend

| Icon | Meaning |
|------|---------|
| ✅ Done / Fixed | Resolved in this or prior session |
| ✅ Guarded | Compile-time test prevents regression |
| 🔜 Planned | Picked for upcoming work |
| ⏳ Postponed | Deferred — blocked or lower priority |
| ⏳ Future | Far future / integration-only |
| 🔒 Invariant | Verified correct, do not "fix" |
| 🔒 External | Blocked on external change |

---

## Session Summary (2026-06-17)

**Goal:** Bug hunt and systematic fix of P0/P1 issues across the codebase.

**10 true bugs fixed:**
| Area | Files Changed |
|------|--------------|
| Context labels | `shared_components.rs:85`, `playlistsearch/songs_panel.rs:59` |
| Wrong handler dispatch | `ui.rs:428-442` |
| Silent failures → visible | `api.rs:407-409,473-478` |
| yt-dlp spawn timeout | `yt_dlp.rs:354` |
| yt-dlp exit code propagation + zombie prevention | `yt_dlp.rs:376-406` |
| `.expect()` → proper errors | `artistsearch/songs_panel.rs`, `playlistsearch/songs_panel.rs` |
| Wrong error message | `playlistsearch.rs:380` |
| "0 results" → "no songs found" | 3 songs panel `get_title()` impls |

**20 false positives eliminated** — verified correct as-is after reading source:
- `#[should_panic]` test correctly panics (r1 is out of bounds)
- OAuth hash is within-process only → SipHash fine
- `blocking_send` called from `spawn_blocking` threads → correct
- `BrowserSongsList` has no `cur_selected` → not duplicated
- `--ignore-config` prevents user config from breaking stdout streaming → intentional
- `config.rs` already has `deny_unknown_fields`
- `"reqwest"` feature exists in ytmapi-rs
- Column headings use `"Song"` consistently

**20 typo fixes:** `recieved`/`receieved` → `received` (16 sites), `ConstraitType` → `ConstraintType` (4 sites).

**P3.3 HashMap optimization:** `resolve_omv_album_songs_with_search` takes pre-built `&HashMap` instead of rebuilding per album. Extracted `build_search_map()` helper outside `#[cfg(test)]` for shared use.

## Session Summary (2026-06-17 cont.)

**P2.2 — Fragile ytcfg parsing:** Replaced `split_once`-based matching with `extract_ytcfg_json()` using brace-depth tracking. Handles whitespace, nested braces, escaped quotes, and `}` inside string values. 7 unit tests.

**P2.8 — `ErrorKind::Header` no context:** Added `message: String` field. `Error::header()` now takes a string; 4 call sites in `browser.rs` provide descriptive context (e.g. "missing INNERTUBE_CLIENT_VERSION in YouTube Music page").

## Session Summary (2026-06-18)

**Spinner:** `draw_loadable` now shows animated braille spinner (`⠋⠙⠹...`) instead of static "Loading" text. `cur_tick` parameter added.

**P1.3 — Modal keybindings shown in help:** Added `flatten_keybinds_as_readable()` + `flatten_tree()` in `keyaction.rs` that recursively expand `KeyActionTree::Mode` entries into individual rows with `{trigger} → {sub_key}` keybinds. `get_help_list_items()` in `ui.rs` now uses the new flattener.

**P1.7 — Notifications config:** Added `notifications_enabled: bool` to `Config` and `ConfigIR`. `MediaController::new()` takes the flag; `update_metadata` skips `notify_track_change` when disabled. Example `config.toml` updated.

**Spinner fixes (artist song loading):**
- Speed: divisor `/8` → `/1` — now animates 1 frame/sec (was 1 frame/8sec) matching the 1s tick rate.
- Centered: uses `centered_rect` + `Paragraph::alignment(Alignment::Center)` instead of top-left corner.

**P3.2 — Concurrent album fetching:** Switched `FuturesOrdered` → `FuturesUnordered` with index-based reordering. Album queries now run in parallel; an artist with N albums fetches in ~max(N latency) instead of ~sum(N latency).

**P3.2 follow-up — Audio-playlist fold:** Audio-playlist fetch (pass 2 of Omv→Atv correction) folded into each per-album `FuturesUnordered` future so all N playlist fetches run concurrently instead of sequentially after album fetch.

**P3.3 — Owned HashMap for Arc-sharing:** `build_search_map` returns `HashMap<String, VideoID<'static>>` instead of `HashMap<&str, &SearchResultSong>`. Wrapped in `Arc` and shared across concurrent per-album futures. `resolve_omv_album_songs_with_search` updated accordingly.

**P2.4 — PanickingReceiverStream logs instead of panics:** `resume_unwind` replaced with `tracing::error!` log + `Poll::Ready(None)`. Background task panics no longer crash the consuming task.

**P2.5 — Duplicate `blocking_send_or_error` removed:** Defined in both `core.rs` and `async_rodio_sink.rs` (identical). Removed the `async_rodio_sink.rs` copy, import from `core` instead.

**search_songs parallelized with album ID collection:** Spawned via `tokio::spawn` before collecting album browse IDs, awaited after. Search query runs concurrently with paginated album-list fetches.

**HashSet dedup:** Album ID dedup uses `HashSet::insert` (O(n)) instead of `Vec::contains` (O(n²)).

**try_send for Loading signals:** Both `GetArtistSongsProgressUpdate::Loading` and `GetPlaylistSongsProgressUpdate::Loading` use `tx.try_send` (non-blocking) instead of `send_or_error` (blocking).

**AlbumProgress variant:** New `GetArtistSongsProgressUpdate::AlbumProgress { current, total }` sent incrementally as each album finishes processing. Handled with `debug!` log in the UI layer.

### Totals
| Metric | Count |
|--------|-------|
| True bugs fixed | 10 |
| Code quality fixes | 8 |
| New features | 4 (spinner, help mode expansion, notification config, AlbumProgress) |
| Performance improvements | 5 (concurrent album fetch, audio-playlist fold, owned HashMap, HashSet dedup, parallelized search_songs) |
| Code quality | 2 (blocking_send_or_error dedup, PanickingReceiverStream) |
| False positives eliminated | 21 |
| Pending | 20 items in P1–P4 remain |

**Next highest-impact picks:**
- P1.4 — `type_name::<A>()` in error messages
- P2.4 — `PanickingReceiverStream` panics on channel close
- P2.1 — Duplicate code generators
- P2.9 — `ListSongID` cfg-gated field
