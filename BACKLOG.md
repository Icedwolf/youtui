# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 687 workspace passed (whole workspace incl. ytmapi-rs + doctests; composition 412 youtui bin + 113 ytmapi lib + 77 doctests + 13 debug_dump + 70 live_integration + 2 json_crawler doctests; −8 from 695 by the client-fallback removal, DECISIONS.md:46). Binary is NOT reinstalled to
`~/.config/cargo/bin/youtui` anymore (user runs it actively) — `target/release/youtui` is the
verification artifact only.
**Last updated:** 2026-09-29

This file is a working backlog only — no changelog, no session archaeology. Past work
and its rationale live in git history and in the code comments / `DECISIONS.md`.

## Open

Part of this file is a working backlog only — no changelog, no session archaeology. The
sort/filter shell duplication item is closed: `SongsPanel` and `SongSearchBrowser` now
share a single canonical body via the `SortFilterTable` trait in `shared_components.rs`
(defaults + 14 accessors), with `go_to_first`/`go_to_last` routing abstracted through
`route_is_list`/`route_is_sort` (preserving the `Search`-arm divergence).

## Current state

The codebase is at a local optimum across the areas this project optimizes:

- **Release perf baseline (2026-09-25, HEAD `1e51888`, `cargo test --release -- --nocapture`,
  all threshold guards green, 700 passed):** `get_field(Artists)` 1.1ns, `get_field(TrackNo)`
  1.0ns (manual loop), `get_fields(4col)` 14.96ns/call (criterion, 100 songs/iter; tight-loop
  harness reads 4.5ns — harness-dependent), `compute_lowercached` 66.0ns, `create_with_metadata`
  ~290ns isolated (287.8–298.9; 393.9 on 09-24 — mid-suite it reads ~768ns due to allocator/
  thermal contamination after the 58k/135k fixture tests, isolate for the true number),
  `push_song_list(58k existing + 58k new)` 77ms, `playlist/get_song_from_idx_last` 1.73ns;
  save serialization (informational, 135k): single-pass 60.7ms / to_string 23.4ms / to_writer
  19.4ms (the abandoned 2-pass variant read 234.25ms pre-removal, 2026-09-25). R39/R40
  additions to the suite: none (api.rs/querybuilder.rs/widgets.rs only; zero bench code).
  The warm release suite measured 1:04.65 and 1:14.65 on 2026-09-25 (network-jitter-dominated
  via the live integration tests) — drift from the R26-era ~46s is the later-added criterion
  benches (3× queue_persistence save + `get_fields/7col` + playlist index, ≈2–3s each), all
  pre-session; the hard `bench` thresholds remain the authoritative regression lock (guards
  pass with ≤2% of their bound at the tightest).

- **Bench-only `into_compact_two_pass` dead path removed (queue_persistence.rs, 2026-09-25)**
  — the 2-pass accumulation and its `save/current_two_pass` criterion bench modeled an approach
  production never uses (`save_queue` inlines single-pass); zero production refs. Before/after:
  the removed bench read 234.25ms vs single-pass 60.7ms (baseline above); the isolated
  save-serialization criterion test now runs 20.92s. 692 debug + 700 release green.

- **Gapless-gate misfire fixed (playback.rs/drawutils.rs, 2026-09-28)** — debug19's 13/17
  unused fills traced to race-free but wrong behavior: streamed ALAC reports `Some(0ns)`
  duration (symphonia inits with `n_frames=0`), so the gate computed `0 − 0 = 0` at the
  *first* progress tick and pre-filled the next song ~0.13s in — a 1-deep cascade burning
  bandwidth/CPU on a song never played. Final fix makes the invariant structural instead of a
  gate-guard patch: the single writer `handle_playing` normalizes `Some(0)` → `None`
  (`None` = "unknown") so `actual_duration` never carries a fake zero-length; the near-end
  gate then needs no filter and `resolve_display_duration` drops its now-dead `secs > 0`
  filter clause + pinned test (net −18 lines). M4A (known duration) gapless behavior
  byte-identical. Test-first: `handle_playing_zero_duration_stores_none` failed on HEAD and
  passes now; `play_progress_zero_duration_streamed_does_not_queue_next` locks the end-to-end
  path; two parity locks (`..._near_end_queues_next`, `..._far_from_end...`) pin the intended
  M4A rows. Expected in the next debug session: fills/play ≈ 1 (was ≈ 2).

- **Zero-sentinel audit (2026-09-28, docs-only)** — after making the duration
  "unknown = None" invariant structural, swept the codebase for the same bug class
  ("numeric `0` or `Some(0)` meaning *unknown* fed into arithmetic"). Traced every
  `Duration`/`Option<Duration>` producer→consumer: `duration_secs` (0 = no metadata)
  is display-only through `resolve_display_duration` (treats 0 honestly, pinned);
  `cur_played_dur` is display + the now-unarmed near-end gate; decoder
  `total_duration` → sink `cur_song_duration` feeds only `handle_playing` (single
  writer, normalized); MPRIS `duration` is forwarded verbatim to souvlaki (µs
  conversion lives in the dep — no youtui arithmetic); `shuffle_seed`'s
  `unwrap_or_default().as_secs()` zero is a valid RNG seed (cosmetic); downloader
  `ttf` sums are test-only instrumentation. Verdict: the misfire class has **no**
  remaining instantiations — no code change warranted.

- **Log review debug20 (2026-09-28, WARN session)** — first session after the gapless
  fix; captured at WARN level only (the app defaults to WARN unless `youtui --debug`).
  Surfaces: 9 403-throttle relays (9 unique songs, each throttled exactly once,
  self-healed on retry attempt 2/3 — no re-throttle loops, zero halts, zero spawn
  failures), 1 stale-cookie auth bail (graceful early abort), 2 video-unavailable
  skips (graceful), and 6 `get_artist_albums` continuation-key misses + 1 album
  `musicResponsiveHeaderRenderer` parse miss — a recurring ytmapi-rs API-response
  shape gap, graceful first-page fallback, kept as honest WARNs (they flag real
  artist-discography truncation; NOT actionable in youtui — upstream). The fills/play
  metric is NOT measurable at WARN level: it needs one `youtui --debug` session on the
  rebuilt binary (next debug.log, ratio ≈ 1 expected); the unit-level proof
  (structural None-invariant + fail-first locks) is the standing guarantee meanwhile.

- **Log review debug21 (2026-09-29) — wild verification of the gapless fix** —
  77-min fixed-binary session (18:42→19:59Z): 41 plays / 50 completed fills =
  fills/play **1.22** (was 4.25 in debug19); `Queuing up song!` = **0** (was 17);
  8 WARNs, all known classes (2 stale-cookie bails, 1 video-unavailable skip,
  1 self-healed 403 throttle, 1 album parse fallback), zero halts/spawn failures.
  The N+2 prefill cascade is gone in the wild; the 1-deep successor prefill is at
  steady state (44 launches / 39 solo plays).

- **Decoder debug-log line narrowing (decoder/mod.rs, 2026-09-29)** — the two
  consecutive init-time `debug!` blocks (`SymphoniaDecoder created` +
  `SymphoniaDecoder codec params`) merged into one emitting all 8 fields
  (codec_sample_rate, decoder_sample_rate, decoder_channels, duration_s, n_frames,
  time_base_num, time_base_den, codec). debug21 showed 68×2 = 136
  lines/session for this family (~9% of a 1599-line log); the merged line halves
  it with zero diagnostic loss — `n_frames=0` (the streamed-ALAC "unknown
  duration" marker) stays on the line. No test pins the strings; net −1 source
  line. Measurable after: next `--debug` session should show ~68 decoder lines
  for comparable volume.

- **Prefill debug-log narrowing (playback.rs, 2026-09-29)** — removed the two
  queue-state `debug!` lines from `download_upcoming_from_id`: `queue BEFORE
  clear` (always `[]` — the queue is unconditionally cleared immediately after)
  and `queue AFTER filtering` (derivable from the per-song skip logs + the
  STARTING/no-download outcome line). debug21 showed 44×3 = 132 lines/session
  for the trio; each prefill pass now emits only its breadcrumbs (START for /
  scope_song / per-song skips / outcome). −10 source lines; no test pins the
  strings. Measurable after: next `--debug` session shows ~44 prefill lines for
  comparable volume (with the decoder merge, ~180 fewer log lines/session total).

- **Log review debug23/24/25 (2026-09-29) — measured-after for both merges** —
  debug23/24 are pre-merge builds (codec_params + queue-state lines present);
  **debug25 is the merged binary in the wild**: `SymphoniaDecoder created`
  renders all 8 fields on one line (codec_sample_rate=48000 … codec=0x2003,
  n_frames=0), `queue BEFORE/AFTER clear` = 0, 0 WARNs, `Queuing up song!` = 0.
  Session-scale verification stands (debug21: fills/play 1.22, gate misfire 0).

- **Audio-path audit (2026-09-29) — "Playback channel closed" characterized
  benign** — the `play_song` task (async_rodio_sink.rs:232) exits without
  `DonePlaying` whenever the responder sender drops before `StoppedPlaying` fires.
  Trace of every such close:
  - *Supersede* (play next / hot path): new song's `handle_play_song` calls
    `sink.stop()` (:39-41), dropping the old `on_done` source → old channel closes.
    Terminal state comes from the new song's `Playing` mutation — never depends
    on `DonePlaying`.
  - *Explicit stop*: `Playlist::stop()` sets `play_status = NotPlaying` directly
    (playback.rs:883) and `handle_stopped` (:1560) is reached via `stop_song_id`;
    both bypass `DonePlaying`.
  - *App quit*: request sender drops → audio loop exits → responder closes;
    process is exiting, no state to preserve.
  `StartedPlaying`'s `try_send` (:53) always succeeds — the response channel is
  empty at that instant (no progress updates before playback begins) — so
  `audio_output_started` always fires with the start. No reachable stall; no test
  added (nothing to reverse), docs record closes the query.

- **Play-start log-line narrowing (async_rodio_sink.rs, 2026-09-29)** — dropped
  `debug!("Inside PlaySong")` (:33, bare function-entry marker; the kept
  `Received request to play … duration` line already proves entry) and
  `debug!("Now playing …")` (:52, duplicates `audio_output_started` one tick
  later on the same `StartedPlaying`; the response channel is empty at that
  instant so `audio_output_started` always fires). −2 log lines per play
  (~−82/session at debug21 volume), zero diagnostic loss in any reachable branch;
  no test pins the strings. Net −2 source lines.

- **Log review debug26 (2026-09-29) — measured-after for the sink trim** —
  sink trim binary in the wild: `Inside PlaySong`=0, `Now playing`=0,
  `Received request to play`=8 = plays 8 (kept line 1:1). Decoder lines 10
  (1/init, was 2), prefill queue lines 0, `Queuing up song!`=0, fills/play 1.63
  (debug25: 1.18 — small-sample scatter around the debug21 baseline 1.22),
  `ERROR`=0, 0 halts. Single WARN = the known 60s ffmpeg no-progress watchdog:
  `16gZm9oeLtI` stalled after its first chunk (12:41:39 → 12:42:40), killed clean,
  successor download spawned instantly, user superseded — one song lost to a stall,
  no loop, no throttle wave. All three narrowings stand in production.

- **Startup latency** — cookie export is conditional (fresh-file skip); the `ffmpeg -version`
  probe is warmed on the blocking pool so it overlaps the rest of startup; the autosave
  deserialize overlaps startup on the blocking pool and the load moves `CompactSongRef`
  fields (no clone) + skips the redundant post-load dedup pass — a 135k-song restore
  measures ~171ms in-app (was ~300ms); the vestigial `node --version` spawn is gone.
- **Song-start latency** — measured headless 1:1: ~2.0–2.6s is yt-dlp bootstrap + resolve +
  CDN first byte (external); the in-app cost after the first byte is ~6ms (ffmpeg transcode)
  plus sub-ms decoder init. No in-app lever remains without daemonizing yt-dlp (rejected).
- **Save-queue latency** — `save_queue` converts in one pass (no intermediate `Vec<ListSong>`
  clone) and serializes with `to_writer` into a capacity-hinted buffer; 135k-song save
  serialization measured ~218ms → ~89ms (`criterion_save_queue_serialization`). `sync_all`
  kept (save is user-triggered, infrequent).
- **Filtered selection resolves correctly** — play/add on a filtered SongsPanel/SongSearchBrowser
  now maps the visible (filtered) row to the real song. The shadowing inherent `get_song_from_idx`
  (unfiltered) that preempted the `SongListComponent` filtered mapping is gone; the trait method
  is the single definition and underlying access is explicit via `list.get_song_from_idx`.
- **Sort reindexes the filtered list** — `push_sort_command`/`apply_all_sort_commands` rebuild
  `filtered_indices` after reordering the underlying list, so filter-then-sort keeps the visible
  rows and play-on-selected correct (previously the stale mapping pointed at wrong songs).
- **No inherent/trait method-name collisions remain** — scripted audit (all 57 `.rs` files under
  `youtui/src`) compares inherent methods against (a) methods in concrete trait impl blocks and
  (b) required/default methods of every trait a type implements (incl. external `Widget`/
  `StatefulWidget` render surfaces). Both reports: **0 collisions**. The `6863e05` shadow class
  is closed tree-wide; `Playlist::get_song_from_idx` is trait-only (the "dual method" was a
  misread — its 3 call sites in playback.rs all resolve to the trait impl).
- **Playlist lookups use the canonical accessor everywhere** — the trait impl and
  `get_song_from_id`/`get_id_from_index`/buffer-scope lookups now call
  `BrowserSongsList::get_song_from_idx` instead of re-deriving via `get_list_iter().nth(idx)`
  (4 sites). Measured cost-neutral (`criterion_playlist_get_song_from_idx`: 1.50 → 1.10ns,
  p=0.78) — `slice::Iter::nth` is already O(1) via pointer arithmetic, so this is a pure
  duplication cut, locked by a parity test (`get_song_from_idx_matches_underlying_list`).
- **`id_to_index_cache` is the single source of truth for id→index** — the `.position()`
  fallback in `get_index_from_id` is gone (landing 2026-09-16). Production mutations are
  exactly three (`clear`, `push_song_list`, `delete_selected`) and all refresh the cache;
  the fallback existed only to mask test helpers writing `p.list` directly. Red-first test
  `direct_list_mutation_does_not_populate_cache` locks the invariant: tests must push via
  `Playlist::push_song_list`. Dead O(n) fallback removed, same correctness.
- **Test-side `get_dummy_playlist`/`get_dummy_album`/`DUMMY_ALBUM` scaffolding deleted**
  (2026-09-16) — ~40 lines + `include_str` album JSON fixture + 6 imports, all for one test
  that only asserted `play_status` and never needed album data. Its call site now uses the
  inline warm-cache construction (373 tests at the time; now 379, no behavior change). The last
  direct-write test helper is gone; `append_raw_album_songs` remains a browser-panel-only
  path.
- **`get_song_from_id`/`get_mut_song_from_id` are the only song lookups** — 7 two-step
  `get_index_from_id` + `nth`/`get_song_from_idx` sites collapsed onto the existing
  accessors (`play_song` ×2, `download_upcoming` ×2, download-progress handler ×3).
  Remaining `get_index_from_id` uses are genuine index consumers (visual-mapping, scope
  windows, OOB diagnostics). Net −15 lines, behavior-neutral.
- **Per-frame `nth` sweep complete (2026-09-16)** — `download_song`'s dead OOB arm
  removed via an invariant `.expect()` (see below note), 5 test two-step lookups collapsed
  onto `p.get_mut_song_from_id`, and the dead cancelled-entry cleanup (`position` +
  `swap_remove`) cut: every cancel site removes its `active_downloads` entry in the same
  locked scope, so the `:619` guard already covers every reachable state.
- **Shuffle refocus blocks collapsed** — `enable_shuffle`, `push_song_list`, and
  `toggle_shuffle` all called `generate_shuffle_indices()` then searched the shuffle
  order for the playing song. After the pin at position 0 (`indices.swap(0, pos)`
  in generate), both the `position()` lookup and the `0.min(max)` fallback resolve
  to 0; the `was_playing` pre-push binding and the `get_cur_playing_id()` call in
  the tuple were dead. Three 8–10-line blocks → one-liners. 6 parity-lock tests
  pin the invariant (would go red if the pin contract ever regressed).
- **`handle_playing` promotion arms merged** — the `Paused` and `Buffering` arms of the
  `play_status` match did identical work (both promote the same id to `Playing`); now a
  single or-pattern with a bound guard. 2 parity-lock tests cover all state rows.
- **`play_next_inner` dead else branch** — inside the `Paused|Playing|Buffering|Error`
  arm, `current_id` is `Some` (the match only fires for the variants
  `get_cur_playing_id()` maps to `Some`), so the `let Some(id) = current_id else
  { return Effects::none(); }` else is unreachable. Replaced with invariant
  `.expect()` (same class as the `download_song` OOB arm).
- **`play_prev` duplicate block extracted** — the "jump to last song" block (compute last
  visual, map to actual, look up, select + play) was identical in the `NotPlaying` and
  wrap arms. Extracted into a private `play_last` helper — both arms now call it. The
  `let cur = &self.play_status;` intermediate binding is gone too (`match &self.play_status`
  directly). Net −5. Locked by the existing play_prev tests across all play states.
- **`refresh_search_view` extraction** — the identical trio `update_search_indices()` +
  `cached_title.take()` + `cur_selected.min(max)` was repeated at 4 production sites
  (Ctrl-W/Char/Backspace arms in `handle_text_event_impl` and `clear_search`). A single
  `pub(super) fn refresh_search_view()` in playback.rs replaces all four. 6 parity-lock
  tests cover the arms and `clear_search`. (Net −9 lines in production, +6 tests.)
- **`regenerate_downloads_debounced` token-cancel hoist** — the
  `shuffle_regen_token.take() + cancel()` block was duplicated in both the idle
  early-return branch and the normal continue path; since it is unconditional in both,
  hoisting it above the `is_none` check is behavior-preserving. Existing regen-token
  tests cover both rows (stale token cancelled on supersede, idle toggle schedules
  nothing).
- **`TextHandler::get_text`/`clear_text` dead chain removed** — the flagged `clear_text`
  gap (Playlist skipped `cached_title` invalidation + clamp) is **resolved as dead code**,
  not a bug: `YoutuiWindow::get_text`/`clear_text` (ui.rs) have zero callers anywhere, so
  the whole container chain (`Browser` → `SongSearchBrowser`/macro composite browsers →
  `Playlist`) was transitively dead. Both methods removed from the `TextHandler` trait;
  the 4 live leaf helpers (`SearchBlock::get_text`/`clear_text`,
  `FilterManager::get_text`, `SearchPanel::clear_text` — called from AddSong search
  submit + `apply_filter`) demoted to inherent methods. 4 new parity locks
  (search_block get/clear, filter_manager get, search_panel clear). (Net −43 lines.)
- **`TextEntryAction` Playlist no-op audited — reachable, deliberate.** Full chain traced
  (`handle_crossterm_event` → `try_handle_text` → playlist `handle_text_event_impl` → fall
  through of unhandled keys → `text_entry` keybind map → `handle_text_entry_action` ui.rs:379).
  The `WindowContext::Playlist => Effects::none()` arm is reachable (Left/Right always, plus
  Backspace/Ctrl+W on empty text) and intentionally swallows those actions so they cannot leak
  into the playlist list keymap — playlist search is search-as-you-type. Enter/Esc never reach
  the keybind map: the text handler closes the search directly (`KeyCode::Esc | KeyCode::Enter`
  arm, mod.rs:343). No dead code; added a documenting comment and 2 parity locks
  (`text_enter_closes_search_and_is_consumed`, `text_esc_closes_search_and_is_consumed`) closing
  the untested Enter/Esc row of the state table.
- **`is_text_handling` leaf impls audited — vestigial, never consulted.** Call-site map of all
  12 `is_text_handling()` sites: none reach `FilterManager` (shared_components.rs) or
  `SearchBlock` (shared_components.rs); every gating path routes through the route checks in
  `SearchPanel` (route == Search) / `SongsPanel` (route == Filter) / custom and macro composite
  browsers. The two always-`true` leaf impls are trait-required but never consulted — documented
  as vestigial. A trait-default swap was rejected (adds an internal default to delete 2 lines,
  net-zero, and would silently flip those leaves to `false` for any future caller). No behavior
  change. Custom `SongSearchBrowser` guard (songsearch.rs:319) confirmed identical to the macro
  composite (Filter route excluded from Submit via the extra `matches!`).
- **`Browser::get_active_keybinds` duplicate dominator gate removed.** The inner
  `if self.dominant_keybinds_active()` early return (browser.rs) was unreachable from the
  dispatch path: `YoutuiWindow`'s DominantKeyRouter consults the same
  `browser.dominant_keybinds_active()` in the same immutable frame and early-returns before
  `Browser::get_active_keybinds` is ever chained (ui.rs:136 vs :146). The three existing direct
  callers (tests) were all non-dominant too. Dominance handling is now consolidated in one place
  (the window); the `DominantKeyRouter` impl on Browser remains for the window's delegation.
  Test-first: `direct_get_active_keybinds_chains_browser_map_when_filter_shown` — failed on the
  old gate (keymap blocked), passes after; the direct-call contract (dominant still yields
  variant + `browser` map) is locked. Net −16 lines. (395 tests.)
- **`SongsPanel`/`SongSearchBrowser` `get_all_keybinds` help gap fixed.** Both returned only
  their list/search maps, omitting the `filter` and `sort` maps that `get_active_keybinds`
  routes to — so the help menu (`YoutuiWindow::get_help_list_items` → `flatten_keybinds_as_readable`
  over `get_all_keybinds`) was missing the 7 Global-visibility sort/filter shortcuts
  (sort Enter asc / Alt+Enter desc / Alt+o clear / o close; filter f close / Enter apply /
  Alt+f clear) plus the list-navigation keys that `get_sort_keybinds` = `[sort, list]` carries
  (PageUp/PageDown/g/G were absent from help entirely — window `get_all_keybinds` never chains
  `list`). Both impls now return the union of every map the active router can select
  (`keybinds_key` + `filter` + `get_sort_keybinds`; and `browser_songs` + `browser_search` +
  `filter` + `get_sort_keybinds`). Test-first: 2 new tests assert the filter map (`f` closes)
  and sort map (Enter asc) are present — failed on current code, pass after. `Playlist` and
  `SearchPanel` `get_all_keybinds` verified complete (state-flat/2-map). (397 tests.)
- **`init_tracing` dead `logging` branch removed.** The param was hardcoded `true` at the only
  call site (app.rs:94); the `false` branch built a subscriber with a Targets filter and no
  layer — unreachable and would silently swallow every event if ever taken. Function now always
  file-logs (matching the standing "File logging is always enabled" note). Net −7 lines.
- **`flatten_tree` dedup onto `from_keybind_and_action_tree`.** `flatten_tree`'s Key row and Mode
  trigger row duplicated `DisplayableKeyAction::from_keybind_and_action_tree` inline (the Mode arm
  was byte-identical). Both arms now delegate to the shared constructor, which stays live at its
  two other call sites (ui.rs:536 mode popup, actionhandler.rs:114 global header). Parity lock
  `flatten_shows_visible_keys_and_mode_subkeys_and_drops_hidden` (keyaction.rs) added first — green
  before and after: visible keys, mode trigger (named by mode), and visible sub-keys appear
  (prefix `Enter → Space`), Hidden rows and sub-keys dropped. Net −12 lines.
- **`NotificationController.cover_url` dead field removed + `icon_path` chain collapse.**
  `cover_url` was written on every successful `notify_track_change` but the dedup gate keys on
  `last_notification` (title+body) only — the stored field's only readers were the type-level
  tests (dropped with it). The 7-line `icon_path` if-let chain collapses to `cover_url.filter`
  (`file://` prefix gate unchanged). Behavior-preserving; full suite parity before/after. Net −12
  lines. `effect.rs`, `server.rs`, `view.rs`, and `ui/header.rs` scans were clean (no changes).
  (398 tests.)
- **`ScrollingList` dead `style` + `highlight_symbol` members removed.** No builder exists for
  either (only `new`, `highlight_style`, `ticker_gap`, `max_times_to_scroll`), so `style` was
  always `Style::default()` passed straight through (no-op) and `highlight_symbol` was always
  `None` — render's `if let Some` branch was structurally dead. Removing both dropped the field
  init/destructure and collapsed `List::new(...).style(..).highlight_style(..)` +
  conditional to a flat `List::new(...).highlight_style(..).render(..)`; the unused struct
  lifetime param moved from the struct to the `new`/`StatefulWidget` impls (method-level, bounds
  unchanged). Parity: the three render-output tests
  (`test_basic_scrolling_list`, `test_max_times_to_scroll`, `test_scrolling_graphemes`) lock exact
  cell output and never touch either member — green before/after. Net −14 lines.
  `queue_persistence.rs` scan clean (`auto_load` is still the synchronous fallback; criterion
  benches are old-vs-new baselines, deliberate).
- **`TabGrid` dead `style` + duplicated render arms collapsed.** `style` had no builder (always
  `Style::default()`, a transparent no-op in `Line::from(title).style(style)`). The render body's
  `MaxCols`/`MaxRows` match arms were 30 identical verbatim lines — `longest_title`/`rows` are
  already computed by constraint-dispatching helpers before the match, so the match was pure
  duplication; the single body now follows. Parity: `test_basic_tab_grid`/`_max_cols`/`_max_rows`
  pin exact cell output, green before/after. Net −41 lines.
- **Draw-layer duplication cuts: reuse `get_cur_playing_id`/`get_cur_playing_song`.**
  `draw_footer` (footer.rs:121) and `draw_app_media_controls` (draw_media_controls.rs:18) each
  re-implemented the `play_status → active song` match inline even though
  `Playlist::get_cur_playing_id()`/`get_cur_playing_song()` (playback.rs:737/747, 14 live call
  sites) are byte-identical to those arms. Both now call the accessors; the duplicated
  `draw_help` doc comment (ui/draw.rs) is gone. Behavior-preserving literal substitution — suite
  parity before/after. Net −14 lines across 3 files. `ui/footer.rs` cache, `action.rs`
  (`NoOp` = keybind-migration sentinel, live), `draw_media_controls.rs`, `ui/draw.rs`, and
  `scrolling_table.rs` otherwise clean.
- **Playlist search silent failure fixed + artist-search album actions deduped.**
  `PlaylistSearchBrowser::execute_search` never set `ListStatus::Loading` pre-fetch nor
  `ListStatus::Error` on failure (songsearch/artistsearch both do) — a failed playlist search
  left the panel on a stale status. Now mirrors the siblings: title shows "Playlists - loading"
  then "Playlists - Error received" on failure. Locked by `search_sets_loading_state_before_fetch`
  (red before, green after; the error arm is sibling-parity + already-covered panel render).
  `add_album_to_playlist`/`play_album` shared ~20 identical lines (selected song → album →
  filter list by album id); both now call `selected_album_songs()` with `error!` preserved.
  3 parity tests lock callback contents. Net −34 production lines (+4 tests = 402 bins green).
  Also repaired a **pre-existing `cargo +nightly fmt` drift** (app.rs, keyaction.rs,
  songs_panel.rs, songsearch.rs) that earlier verification rounds had masked behind a `tail`
  pipeline — the fmt gate now uses the bare formatter exit status (`style:` commit `013efa1`).
- **Dead `browser_search` suggestion keybinds dropped from defaults.**
  `default_browser_search_keybinds()` shipped Up/Down → `Next/PrevSearchSuggestion` (retained
  no-ops from the removed suggestions dropdown) — active no-ops in the search route and dead
  help entries. Now returns an empty map; the category + `BrowserSearchAction` variants stay
  for config-parse compat (user bindings parse and dispatch as no-ops; parse-time NoOp strip
  unaffected). Search-route Up/Down: no-op key → unmapped (`NoMap`) — observably identical
  (both clear the key stack). Example `config.toml` drops the suggestion section; two
  browser.rs tests flipped to assert no dead suggestion bindings in the search-route active
  chain; new red→green test asserts empty defaults. (keymap.rs sweep otherwise clean.)
- **Player-pause effect deduped.** `pauseplay`/`resume`/`pause` (playback.rs) each carried a
  byte-identical 7-line `server.player.pause()` effect block (only the `play_status` guard
  differs). Extracted private assoc fn `player_pause_effect()`; all three call it. `stop()`
  untouched (its block carries the `handle_all_stopped` mutation). Parity locked by the
  existing pause/resume state-transition tests; net −5 lines.
- **Dead videos now flagged at ANY download failure.** Log review (debug15/16) showed the
  same pattern for every permanently-unavailable video: `download_error` twice for one song —
  once as a background prefetch/queued download, again after a re-resolve while buffering —
  because `session_dead_videos` was only populated in the buffering branch. Every dead
  successor thus cost one wasted yt-dlp resolve cycle (~2–4s) at auto-advance. The flag +
  Song Unavailable notify now happen on any non-cancellation dead-video failure; the
  buffering branch keeps skip/auth/halt duties with the same `is_dead`/`is_auth` reads.
  Red→green: `dead_failure_flags_session_dead_even_when_not_buffering`. Full `playback.rs`
  sweep now complete and clean.
- **`SortManager::new()`/`FilterManager::new()` dropped for `Default`.** Both hand-rolled
  constructors re-implemented `Default` field-by-field (SortManager derives it; FilterManager
  has a manual impl), sole callers were `SongsPanel::new` (songs_panel.rs:58-59). Net −19 lines;
  the vestigial `is_text_handling() -> true` impls were checked and kept — `TextHandler` has no
  trait default, so they're mandatory overrides.
- **`handle_sort_cur_asc`/`desc` merged.** The two `SortFilterTable` default methods were 18-line
  twins differing only in the `SortDirection` literal. Both implementors (`SongsPanel`,
  `SongSearchBrowser`) use the defaults; the four dispatch sites (macro songs_panel routing +
  songsearch.rs self-routing) are unchanged. Shared `handle_sort_cur(direction)` body, asc/desc
  become one-line delegators. Net −11 lines.
- **`ListSong` cached-field derivation consolidated (R11 flag).** All four constructors
  (`create_with_metadata`, `add_raw_album_song`, `add_raw_search_result_song`,
  `add_raw_playlist_item`) recomputed the same caches: `compute_artists_string` + `track_no_string`
  + the lowercase search triple; `ensure_cached_fields` was a fifth partial copy filling only the
  first two lazily. All now call one `compute_cached_fields` helper with a documented
  `Option<track_no>` contract. Behavior-identical (state-table verified), locked by the suite.
  Net +22 lines — one-time helper scaffolding replacing 4× full derivations.
- **Artist/playlist split-browser draw merged (draw.rs).** `draw_artist_search_browser` +
  `draw_playlist_search_browser` were 71-line near-twins. Both are now thin wrappers over
  `draw_split_search_browser_core`; the real differences are explicit params: left layout
  (`[Max(30), Min(0)]` vs `[Percentage(30), Percentage(70)]`), search-box label, and a
  `loadable_left` flag preserving the playlist's historical lack of the `draw_loadable`
  overlay. The risky part — render code with zero tests — is locked by two new TestBackend
  render-parity tests (snapshot fixture `draw_parity.txt`, captured from the pre-refactor
  render; 6 states: loaded/popped/loading × artist/playlist). Production net −~60 lines;
  +2 parity tests + fixture. 682 tests.
- **Five per-browser `ActionHandler` impls generated from one macro (browser.rs).**
  `BrowserArtistSongsAction`/`BrowserArtistsAction`/`BrowserSongsAction`/`BrowserPlaylistsAction`/
  `BrowserPlaylistSongsAction` were identical-shape impls (match variant → `apply_action_mapped`,
  else `debug!` + noop), ~82 lines differing only in action type/variant/field/message. Now five
  `impl_browser_sub_action_handler!` invocations; new wrong-variant test locks the mismatch arm.
  Net −26 lines. 683 tests.
- **Dead `DownloadStatus::Downloading` payload dropped (youtui).** The tuple variant
  (`Downloading(Percentage)`) was constructed with literals (`Percentage(0)`/`Percentage(50)`) and
  only ever pattern-matched as `Downloading(_)` — the percentage was never read. Narrowed to a unit
  `Downloading`; `DownloadStatus` never hits serde (queue persistence serializes `CompactSongRef`),
  so no persisted shape changes. Existing icon-semantics test locks the change. 692 tests (unchanged).
- **Live `_stream`/`_stream_source` renamed (youtui).** Both-token streaming
  variants on `DynamicYtMusic` were underscore-prefixed like dead code, but
  cli/querybuilder.rs calls both (`get_string_output_of_streaming_query`). The
  lie invites the false-dead-code trap — a sweep nearly deleted them this
  session. Renamed to `stream`/`stream_source` (both names free — no conflict
  with the `stream_browser_or_oauth` siblings).
- **Live API drift 2026-09-25: anonymous featured-playlists continuation caps
  at 2 pages with an empty terminal page** (no `continuationContents/
  musicShelfContinuation`), erroring the parsed stream on page 3. Raw dump
  measured: page 1 full shelf, page 2 full continuation, page 3 = 531 B
  `responseContext`-only doc. Browser auth still serves 5 full shelves, so the
  browser variant stays live; only the noauth variant is `#[ignore]`d — the
  shared macro would have ignored both, losing browser coverage. App path
  unaffected (browser auth; `FeaturedPlaylistsFilter` is CLI-diagnostic-only).
- **`continuations.rs` stream vs `raw_json_stream` verdict (ytmapi-rs): keep
  separate.** Parallel unfold skeletons (first query then continuation chain)
  but different yields (parsed `Q::Output` vs cloned raw JSON `String`) and
  different continuation-extraction error policies (propagate vs swallow-and-
  stop), and both back live, separately-called APIs (api.rs:83/104 vs 139/164).
  A merge needs a yield-mapper closure + error-mode branch — more complex than
  the ~15-line shared skeleton saves. Same judgment as the watch-playlist pair.
- **`server/api.rs` structural audit (youtui): clean.** `resolve_omv_crossref`
  is the single shared crossref core (two thin map-building wrappers —
  already factorized); `search_artists`/`fuse_artist_search`/`wait_artist_results`
  concurrency is deliberate and documented; `get_artist_songs` vs
  `get_playlist_songs` share only a ~15-line channel/spawn/Loading preamble
  while their bodies diverge 240 vs 50 lines — below the twin threshold, kept
  inline. Marginal-kept: `send_or_error`'s `S: Borrow<mpsc::Sender<T>>` bound
  absorbs 5 owned-tx vs 9 borrowed-tx call forms, but owned-vs-borrowed is
  semantically meaningless (the channel closes at task-end either way) and a
  normalization has zero behavior delta → no fail-first test possible (rule 6),
  zero lines saved — rejected at preflight like the MRLIR prologue.
- **`DEFAULT_TICKER_GAP` canonicalized to widgets.rs (R40).** Was duplicated
  1:1 (`pub const ... = 6;`) across scrolling_list.rs and scrolling_table.rs —
  drift risk (editing one widget's gap leaves the other stale). Both widgets
  already import `get_scrolled_line` from the widgets root; the const now lives
  beside it and both import it. Player.rs also closed: clean 34-line delegating
  facade over `AsyncRodio`. List-vs-table render strategies are genuinely
  different (List delegates to ratatui; Table manages windowing manually) —
  the const was the only cross-file drift point.
- **Whole-crate mechanical sweep (consts + fn liveness): clean.** Const scan:
  no duplicate same-name consts remain anywhere (R40 was the last). Liveness
  scan of every `pub(crate) fn`/`fn`: every low-caller lead resolved live —
  note two scanner blind spots to re-check by hand on future sweeps:
  turbofish calls (`fn::<T>(`) and serde string paths
  (`deserialize_with = "crate::core::string_or_struct"`). Send-helper trio
  (`send_or_error` async-tokio / `blocking_send_or_error` sync-tokio /
  `std_send_or_error` std-mpsc) kept — distinct channel types and execution
  contexts; merging needs a send-strategy flag, worse than three clear fns.
- **Log review 2026-09-24/25: no new in-app pattern.** debug17 is 100% the
  known 403-throttle→relay-retry wave (every case resolved on attempt 2);
  debug16 shows one attempt-3 halt (eDrGiP1UVfk, the external per-video
  PO-token gap) plus a correctly-classified `Video unavailable` graceful skip
  (bdf_ll68Z8o). debug18 (today) empty — app running, not disturbed.
- **Start-fast buffer threshold: no lever (already optimal).**
  `STREAM_INIT_THRESHOLD = 512` measured against ffmpeg's first atomic flush
  (the ~700 B empty_moov header write) — the gate sits below the first flush by
  design, so init can't start earlier than the flush exists; the threshold is a
  wake-up gate, not a latency control. Closed with measurement.
- **Re-checked and rejected: MRLIR-prepare prologue extraction (ytmapi-rs).** history/library/
  playlist share a 5-line borrow-MRLIR + flex-title prologue (history/library byte-identical,
  playlist 1-sentinel-diff "Song deleted"). Extracting `borrow_mrlir_title<'a, C: JsonCrawler>`
  returning `Option<(C::BorrowTo<'a>, String)>` would save only 2 lines/site and add a lifetime-
  generic helper (net +lines, more complex surface). Previous "kept inline" judgment confirmed.
- **TextHandler::is_text_handling defaulted; two vestigial overrides cut (youtui).**
  FilterManager and SearchBlock each returned a tombstoned literal-`true` `is_text_handling` that
  no caller consults (input-ownership gates live in SongsPanel/SearchPanel route checks), but the
  trait required the dead bodies. The trait method now defaults to `true` (value-identical to both
  removed bodies at every call site — provably behavior-neutral); the two overrides and their
  tombstone comments are gone. New lock test `leaf_text_widgets_own_input_by_default`. 692 → 693.
- **Audit records (kept intentionally, not bugs):** the ytmapi-rs search-suggestion API
  (`SearchSuggestion`/`SuggestionType`/`TextRun`/`GetSearchSuggestionsQuery`, ~150 lines) is NOT
  dead — youtui's CLI diagnostic command `GetSearchSuggestions` (main.rs:106, querybuilder.rs:58)
  backs onto it, so an attempted removal was reverted at pre-flight (enumerated call sites with a
  head-truncated grep missed it). Dependency audit across both crates: every dependency has a
  live, wired use (no unused deps to cut).
- **Two-column header description parse shared (ytmapi-rs).** The album and playlist detail
  pages each carried the same nine-line description-shelf block (mirrored author notes
  "NOTE: Similar code to get_album_2024"/"get_playlist_2024"). `take_description` in parse.rs is
  now the single home; both sites route through it and the orphaned `CrawlerResult`/
  `DESCRIPTION_SHELF_RUNS` imports are gone. This closes the parse-tree byte-twin hunt: every
  remaining cross-site sequence (MRLIR-prepare prologue, feedback-token/duration/video_id menu
  chains, item-prologue filler) is 2–5 lines with differing sentinels/contracts and stays
  intentionally inline.
- **Play-button video id canonicalized (ytmapi-rs).** `PLAYLIST_ITEM_VIDEO_ID` already existed at
  nav_consts.rs:81, but twelve sites in five modules re-expanded it as
  `concatcp!(PLAY_BUTTON, "/playNavigationEndpoint", WATCH_VIDEO_ID)` or hand-rolled the literal
  `"/playNavigationEndpoint/watchEndpoint/videoId"` (a string-identical const split). All now use
  the const; unused `PLAY_BUTTON`/`WATCH_VIDEO_ID` imports dropped where the expansion was their
  last user. −44 net lines; album/history/library/playlist/upload fixtures pass.
- **Library tab list parsers share one skeleton (ytmapi-rs).** The seven `parse_library_*` /
  `parse_content_list_*` list fns each reimplemented the same continuation-params + iterate +
  collect shape. `parse_library_list<R, T>` (Fn-parameterized over the item parser) is now the
  single home; pointer, skip, and Option-vs-plain item contracts remain per-call-site. −21 net
  lines; all library fixture suites (playlists/artists/albums/songs + continuations) pass.
- **Grey-out display-policy check canonicalized (ytmapi-rs parse).** Ten sites in five parse
  modules read `musicItemRendererDisplayPolicy` two ways (7× `is_available = != GREY_OUT`,
  3× `if let Ok(GREY_OUT)` skip), most with the raw path literal instead of the `DISPLAY_POLICY`
  const. `fn is_greyed_out` in parse.rs is now the one truth table (absent → false, GREY_OUT →
  true, other → false), locked by a tri-state unit test; the const drift is gone.
- **Playlist shelf parsers share one skeleton (ytmapi-rs).** The featured and community
  playlist parsers in `parse/search/mod.rs` were byte-identical except the (1,2) column name
  (songs/views) and output type. `take_playlist_shelf_fields` holds the single copy of the
  MRLIR extraction (title/author/count/browse id) so a layout-drift fix lands once. Covered by
  the 6 search fixture tests + 2 drop-junk cases.
- **Stale sorting TODOs pruned from the CLI enum (main.rs).** The 9 `//TODO: Allow sorting` /
  `// TODO: Sorting` comments above the `GetLibrary*`/`GetLibraryUpload*` variants aspired to
  CLI sort flags — rejected by the product vision, and misleading since interactive sort
  exists. The `CreatePlaylist`/`GetWatchPlaylist` capability notes remain (genuine gaps on
  existing debug surfaces).
- **Criterion benchmark windows capped (structures.rs, queue_persistence.rs, playlist tests).**
  All three criterion-in-`#[test]` wrappers ran bare `Criterion::default()` — 3s warm-up + 5s
  measurement per bench function; ten benches cost 64.5s of every `cargo test --release`. Now
  0.5s/1s: same 10 bench_functions, baseline-visibility stats, release suite 76.9s → 46.2s
  (−40%). Perf baseline recorded in "Current state" above.
- **Prefer-audio-track dedupe loop extracted (structures.rs).** `append_raw_album_songs` and
  `append_raw_search_result_songs` each carried an inline HashMap dedupe (one song per key,
  audio track displaces a non-audio occupant) identical modulo the key type (title vs
  (title, artist)). Now one generic `dedupe_preferring_audio(key, is_audio)` the displacing
  semantics exist in. 3 new lock tests via `serde_json::from_value` fixtures (bypasses
  `#[non_exhaustive]`, same recipe as the api.rs `resolve_omv` tests). 691 tests.
- **`Config::default` route through `default_notifications_enabled` (config.rs).** The IR's
  serde default used the fn while `Config::default` (and the test's Config literal) inlined
  `true` — two sources of truth for the same policy. Now all three use the fn, matching
  `volume`/`download_cache_size`. New alignment lock
  `default_notifications_matches_fn`. 688 tests.
- **Three song-list default keybind tables collapsed into one macro (config/keymap.rs).**
  `default_browser_songs_keybinds` and `default_browser_playlist_songs_keybinds` were byte-identical
  tables (same f/o/Enter-"Play" skeleton, differing only in the action-type path);
  `default_browser_artist_songs_keybinds` was the same table + exactly two bindings
  ('a'→PlayAlbum, 'A'→AddAlbumToPlaylist). Now one `default_song_list_keybinds!($category,
  $action_ty [, extra-pairs])` macro: every key, action, visibility (Global on f/o, Standard on
  mode subs) and the "Play" mode name preserved. New parity-lock test
  `song_list_default_tables_share_one_skeleton`. Prod tables 160 → ~84 lines. 687 tests.
- **TabGrid `MaxCols` constraint variant stripped (widgets/tab_grid.rs).** The only production
  caller (header.rs `TAB_ROWS`) uses the row-constrained path; `MaxCols` and its `#[cfg(test)]`-only
  `new_with_max_cols` existed solely for two unit tests. `TabGridConstraint` (enum) collapsed to a
  plain `rows: u16` field, `new_with_max_rows` → `new`. The two max_cols snapshot tests ported to
  row form with byte-identical expected strings (layout parity: `MaxCols(2)≡MaxRows(2)`,
  `MaxCols(3)≡MaxRows(2)`); new zero-rows guard lock. Net −28 lines. 686 tests.
- **Three duplicated volume-apply effect bodies merged (ui.rs).** `YoutuiWindow::new`'s startup
  volume init, `handle_increase_volume` and `handle_set_volume` each carried the same ~15-line
  async shape (`Effects::new` → Arc clone → player op → `handle_volume_update`). All three now
  route through `apply_volume_effect(mutate_visual, op)`; the op closure takes the cloned `Arc`
  by value and returns a boxed `+ Send + 'static` future (the `Effects::new` `'static` constraint
  drove the shape). Two lock tests assert the synchronous visual volume + clamp through both
  public wrappers. 685 tests.
- **Vestigial `ResolveAudioTracks` stub reduced to a retained no-op.**
  The `PlaylistAction::ResolveAudioTracks` arm marked `ListSong::resolution_checked` (a field
  nothing read), spawned N no-op effect closures, and set/cleared
  `Playlist::resolving_audio`/`resolve_remaining` synchronously — the `[RESOLVING]` title
  indicator could never render. All unobservable. The variant + describe + Global `r` keybind
  stay as a documented no-op (config-parse compat, same as the retained `BrowserSearchAction`);
  removed the dead `resolution_checked` field (4 ctor inits), the two flags (+inits), the
  no-op spawn, and the dead indicator. Locked by `resolve_audio_tracks_is_retained_noop`
  (red before: spawned 3 no-op closures; green after). Net ~−40 production lines,
  +1 test (403 bins green).
- **`apply_ytdlp_auth_args` duplicate skip-only branch collapsed** — the inner
  `else` (fallback requested but no pot provider) and the outer `else` (no
  fallback) emitted the byte-identical
  `--extractor-args youtube:skip=hls,translated_subs` line. Nested if/else
  pair → single `if web_music_fallback && let Some(pp) = pot_provider` guard;
  all four fallback/provider rows emit identical args (state table verified).
  New lock `fallback_without_provider_degrades_to_default_clients` covers the
  previously-untested `(true, None)` row. (Net −4 lines in production.)
- **`song_downloader/mod.rs` production block fully re-swept** (lines 1–1255:
  classifier/`bail_failed_buffer`/retry ladder/`spawn_bg_cache_task`/
  `ytdlp_pipeline`/`await_full_download`/`download_and_decode`) — clean, no
  dead branches; `try_streaming_init` vs `_nonseekable` (seekable vs
  non-seekable source) and `decoder_from_buffer` (sync) are genuinely
  distinct init paths, kept.
- **RAM** — in-memory ALAC buffers are duration/source-dependent (5.5–77MB, median ~45MB);
  cache max = 1 by design.
- **Render/CPU** — per-frame caches (title, row numbers, artist string, lowercased fields)
  are in place; no per-frame allocation on the hot path. Landing 2026-09-15: footer
  `bar`/`vol` strings and all four `HasTitle` impls cached (footer + playlist + songs
  panel + songsearch + search panel); `create_with_metadata` avoids the double artists-join
  (bench 535 → ~310ns/song, ~2× faster 135k-song autosave apply).

## Rejected / not planned

These are deliberately out of scope. Rationale in `AGENTS.md` scope and `DECISIONS.md`.

| Item | Why rejected |
|------|--------------|
| Lyrics, themes, stats, mouse, offline cache, gapless, search suggestions | no new features (suckless "search → queue → play") |
| Async-callback-manager → native Tokio rewrite | ~2000-line rewrite, no measured payoff |
| symphonia 0.6 upgrade | rodio pins 0.5.5; a bump would duplicate the codec stack |
| Daemonizing yt-dlp to shave ~2s song start | complexity vs a single external cost |
| `cargo fmt` tree-wide | older-rustfmt drift (272 hunks), cosmetic, not a correctness gate |

## Known issues (not currently actionable)

External-root-cause items tracked so future sessions don't re-diagnose them. Evidence in
`DECISIONS.md` (line refs) and the debug-log reviews in this file.

| Issue | Root cause | Status |
|-------|-----------|--------|
| Intermittent CDN 403 on the relay's first attempt (debug18: 2, debug19: 2, debug20: 9, debug21: 1, debug27: 7) — mostly self-heals on the retry; a throttle wave can still halt via the transient-failure counter | nsig/GVS-token CDN throttling wave — fresh external churn, NOT stale cookies/dead video (DECISIONS.md:32,40) | External fix lives upstream (yt-dlp client/token matrix); the recovery is item 40's fresh-resolve relay retry (9/9 observed self-heals, all `attempt 2/3`). The POT-provider slice (incl. the `--bypass-cache` patch) was **removed** 2026-09-29 (DECISIONS.md:46) — it never fired and is not involved in the 403 path |
| Missing artist-albums continuation (6×/session in debug21, graceful first-page fallback) | Upstream ytmapi-rs response-shape gap; not fixable inside youtui | Flagged upstream; youtui side is already correct (optional section list, no R2 crash) |

## Log reviews (2026-09-29)

- **debug26 (complete, 21 min, 12:35:27→12:56:26Z)** — all narrowings hold (`codec
  params`=0, prefill queue lines=0, `Inside PlaySong`/`Now playing`=0). Real
  fills/play = **15/15 = 1.00** — the earlier "23" count double-counts
  `finishing buffer`, which fires twice per download; the load-bearing anchors are
  `ffmpeg completed successfully` + `download_done` (each fired once). 6 WARNs, all
  known classes: the 60s watchdog kill (`16gZm9oeLtI`, stall after first chunk,
  self-healed), a **self-healed 403 throttle** (`d8JXbgjILys`: relay first-attempt 403 at
  12:47:20 → relay attempt 2/3 → first chunk 6.4s, streamed, cached 79MB, later
  reused from cache), and a `Video unavailable` graceful skip (`sM3Pc9hDLJI`).
  11 `download cancelled` = normal supersede churn (each prefilled successor is
  cancelled when the user picks elsewhere). `ERROR`=0, 0 halts, 0 loops.
- **debug27 (complete, 2h, 12:56:30→14:57:01Z, 1902 lines)** — 51 plays / 58 real
  fills (fills/play 1.14 incl. top-up churn; the appended tail 13:26→14:57 alone
  adds 36 plays / 39 fills ≈ 1.08), 31 cache reuses, 18 cancels. **20 WARNs /
  0 `ERROR`**, all known classes: **PO-token CDN 403 churn** (known-issue row
  below) — 7 relay first-attempt 403s spread across the tail (13:39, 13:51, 14:03, 14:18,
  14:41, 14:51, ~1 per 10 min), every one self-healed via the relay/fresh-resolve
  (48-111MB fill completing 4-6s after the 403; e.g. 6Ge21BOyips 84MB, Dc1aVHdz5uY
  111MB); one adjacent auth bot-check (`fG047b1uE9I`: "Sign in to confirm you're
  not a bot" → stale-cookies bail → notified + skipped, separate cookie-staleness
  class; its retry was superseded-cancelled — the only song that didn't recover,
  not via the 403 path); one dead video (`MG80av83nxA`, flagged session-dead).
  Removed branch wild-confirmed dead: `DEBUG download failed while buffering` = 0
  in this pre-cut binary, and `song id not found` = 0 → the new `expect()` is
  safe. 0 halts, 0 loops.
- **debug28 (complete — final figures; ran the PRE-CUT binary, 2273 lines,
  session ended 13:27 local) — heavy rapid-skip session**: 216 download starts /
  365 cancels across ~165 distinct songs — but coalesced by the settle window
  exactly as designed: 25 real fills / 25 `download_done` (1:1), 7 cache
  reuses, semaphore=1 held, no parallel yt-dlp, 18 `audio_output_started`
  (+7 cache-reuse plays ≈ 25 plays → fills/play ≈ 1.00). **5 WARNs / 0
  `ERROR`, 0 halts**: one dead-video incident (`9REO_JI0exY` — 3 WARNs across
  distinct layers: yt-dlp stderr classifier → UI `download_error` → the live
  `:1421` "download failed while buffering" skip, confirming the removed
  `debug!` branch was a distinct unreachable site) + one throttle incident
  (`bkXsZqmXxZU` — 2 WARNs: classifier + `relay_throttle_retry` note; the
  **pre-cut** 2-WARN-per-throttle pattern — the both-cuts land in the next
  build, baseline for commits 74ba3f6 + a6cf39b).
- **Throttle-WARN consolidation (2026-09-29, `74ba3f6`)** — the
  `Relay throttled (403) — retrying with a fresh resolve (attempt n/3)` warn
  (`relay_throttle_retry`) was redundant with the stderr classifier's
  `yt-dlp 403 (throttled)` warn (`:509`, the deliberate wave detector): both
  fired per incident in debug27 (14 of its 20 WARNs = 2 sites echoing 1 event,
  7 incidents). The retry outcome is logged downstream either way. Downgraded
  to `debug!` (attempt n/3 + elapsed evidence preserved at review level);
  WARN now means definitive failure only (exhausted retry/halt, dead video,
  auth). Expected: ~7 WARNs/session for the same churn. Field note: the
  `--bypass-cache` plugin patch is since REMOVED with the POT-provider slice
  (DECISIONS.md:46) — the churn was genuine first-fetch GVS rejection, and the
  recovery is the fresh-resolve relay retry alone (all 7 recovered in ~2s; all
  prefills, user impact ≈ 0).
- **AGENTS.md staleness fix (2026-09-29)** — AGENTS.md still documented the
  **removed** direct-URL architecture (`build_ffmpeg_command(FfmpegInput::Url)`,
  `throttled_url_retry`, URL-cache eviction, `player_client=web_music` forced on
  the primary, `bestaudio[ext=webm]`, "URL pre-resolution outside semaphore"
  invariant) — despite DECISIONS.md:39 removing the entire path on 2026-09-09.
  The drift cost a full wrong-model session today (analysis built on the stale
  direct-URL theory before the relay-only code was re-verified). Rewritten to
  the verified relay-only reality: `ba/bestaudio` relay → ffmpeg `pipe:0` ALAC,
  auth via yt-dlp `--ignore-config` + `--add-header Cookie:`, primary = token-free
  default clients with `web_music`+POT only as the bounded fallback (item 42,
  itself since removed — item 46),
  throttle = in-place relay retry capped at 3 (items 39-40). Also appended
  DECISIONS.md:45 (retry-note WARN→debug policy, amends item 36) and corrected
  the `direct-URL 403` mislabels in this file (the 403s always hit the relay's
  first attempt). AGENTS.md is untracked (local-only); DECISIONS.md + BACKLOG
  are committed.
- **Client-fallback + POT-provider slice removed (2026-09-29, DECISIONS.md:46)**
  — the whole `web_music`+POT escape hatch is gone: `PotProvider`,
  `load_pot_provider`, the `web_music_fallback`/`pot_provider` threading,
  `relay_client_fallback_retry`, `is_format_unavailable_line`, the
  `format_unavailable` buffer flag, the `--bypass-cache` plugin patch, and
  their tests (resolve.rs ×4 + mod.rs ×4 incl. 2 E2E). Evidence: never fired
  once across debug23–28 (~2 weeks / hundreds of songs; the only failure
  classes ever observed were the throttle — recovered by item 40's
  fresh-resolve retry, 9/9 on attempt 2 — dead video, auth, and stalls), and
  the net depended on fragile external assets (plugin + CLI + patch all
  current). `apply_ytdlp_auth_args` is now `(cmd, cookie_header, video_id)`
  with no client branching; `try_pipeline_retry` is throttle-only. The
  `Requested format is not available` line now bails as a generic transient
  (WARN + skip), matching what a doubly-refused fallback did anyway. Expected
  next session: byte-identical fills, throttle recovery unchanged, no
  format-unavailable path ever warming (6-session-old baseline says none).

## Cancel-class audit (2026-09-29) — characterized benign, no gap

**All 8 cancel bail sites** in `song_downloader/mod.rs` (`before start` :1206,
`during settle` :1227, `before semaphore` :1235, `after semaphore` :1244, `during
buffering` :966, `empty-pipe wait` :1015, `M4A total_len wait` :1132, `fallback
wait` :826) are gated on the per-download `cancel_token`, and every consumer
branch falls through `is_cancellation_error` (test-pinned: never increments the
halt ladder, never notifies, never dead-video-flags). The **selected song's token
is unreachable by supersede**: `download_upcoming_from_id` rebuilds the scope as
inclusive `{current, successor}` (:450-461, :1250-1254), the P0 fix — so the
buffering-skip guard (`Buffering(target)==id`, :1419/:1450) fires only on genuine
errors, never on a cancel of the current song. The remaining two triggers are
safe by ordering: `stop()` sets `NotPlaying` before `cancel_all_downloads`
(:883-886), and `cancel_song_download` only runs when the user deletes a song that
was already stopped (:928-936). The stale-cancel race (an old cancelled task's
`Error` landing after a replay of the same id with a fresh token live) is
impossible: the cancel `Error` and any replay keypress drain the same mutation
channel in order. Observed cancels (debug26: 11, debug27: 3) are all supersede
churn on prefilled successors — never the current target. No test added (nothing
to reverse); docs record closes the query.

- **Dead cancel-branch removed (playback.rs, 2026-09-29)** — the
  `if is_cancellation_error(&e) { debug!("download failed while buffering,
  skipping") }` inside the `Buffering(target)==id` guard (:1451-1453) was dead:
  every token-cancel sink either sets `play_status` away from `Buffering(id)`
  before cancelling (`stop()` :883, delete :928-933) or never touches the current
  target's token (inclusive `{current, successor}` scope; the `prepare_playback_id`
  window also contains `id` itself). The stale-cancel/replay race is impossible
  (same mutation channel, ordered). −3 source lines; `is_cancellation_error` still
  used at :1395; 695 tests green, clippy 0, release build clean.

- **Cancel-mechanism dual audit (2026-09-29) — benign, no consolidation** — the
  two cancel predicates are *not* a duplication. `drop_unscoped_from_id` (:682)
  is a positional window `{idx, idx+1}` on the list, run once per `play_song`
  (:174→:149); `cancel_out_of_scope_downloads` (:1215) is an explicit set
  `{current} ∪ {successors}` where successors come from a shuffle-aware walk
  (:416-426), run per prefill/regen. In shuffle mode the successor lives at
  `shuffle_indices[visual+1]` and is *rarely* list-adjacent — a window-based drop
  would cancel an in-flight shuffle-successor prebuffer (wasted yt-dlp per song
  start), exactly the regression the :1251-1256 comment warns the regen path
  avoids. The set-based path also drains `download_queue` + resets status.
  Consolidating = regression; keep both. Log families re-audited in the same
  pass: `finishing buffer` is a single legit writer-EOF site (fires per spawned
  writer incl. killed/cancelled — that's the debug26 23-vs-15 inflation; anchor
  `ffmpeg completed successfully` already corrected), `Stream task finished` is
  the load-bearing stream-drain breadcrumb (stall/hang detection). Keep.
  Dead-message sweep (same pass): playback.rs/app.rs/sink/song_downloader
  `debug!`/`warn!` families all sit on live branches — the dead-message class
  had exactly one member (the cancel branch removed above).

- **Dead download_song not-found arm narrowed to expect() (playback.rs,
  2026-09-29)** — the `id not found` arm (`debug!` + `play_status = NotPlaying`
  + return, :511-514) was provably unreachable: every caller validates its id
  against the live list before `download_song` runs (`play_song` :176, scope
  walk :406/:420/:429, single-source `download_queue` :470 with delete-retain
  :937 and reset/stop/cancel clears; no test passes an absent id). Same
  cache-invariant argument as the sibling OOB arm already converted at
  :531-537 (test-pinned by `direct_list_mutation_does_not_populate_cache`).
  Converted to `expect()`; −3 lines, one dead `debug!` removed. 695 tests
  green, clippy 0, release build clean.

## When adding work here

- State the item, the severity/type (bug / complexity / perf), and the evidence.
- One line per item; no prose changelog entries.
- Close an item by deleting it — the "why" goes in the commit message and code comments.
