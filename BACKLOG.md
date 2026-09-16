# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 381 youtui bins green (2 ignored)
**Last updated:** 2026-09-16

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
