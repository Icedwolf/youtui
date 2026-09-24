# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 403 youtui bins green (2 ignored). Binary is NOT reinstalled to
`~/.config/cargo/bin/youtui` anymore (user runs it actively) — `target/release/youtui` is the
verification artifact only.
**Last updated:** 2026-09-17

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

## When adding work here

- State the item, the severity/type (bug / complexity / perf), and the evidence.
- One line per item; no prose changelog entries.
- Close an item by deleting it — the "why" goes in the commit message and code comments.
