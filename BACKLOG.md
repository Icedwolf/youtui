# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 360 youtui bins green (2 ignored)
**Last updated:** 2026-09-15

This file is a working backlog only — no changelog, no session archaeology. Past work
and its rationale live in git history and in the code comments / `DECISIONS.md`.

## Open

- **Sort/filter shell duplication** (complexity) — `SongsPanel` and `SongSearchBrowser`
  each carry ~15 near-identical sort/filter methods (~270 duplicated lines total) plus
  ~53 lines of duplicated `AdvancedTableView` one-liners. The bodies differ only by field
  name (`list` vs `song_list`), the route enum (`SongsInputRouting` vs `InputRouting`,
  the latter adds a `Search` arm), and two error-message strings. Moving the shared
  bodies into `AdvancedTableView` default impls (~10 new one-line accessors/struct:
  `get_songs`/`get_mut_songs`, `set_route_list/sort/filter`, `route_is_list`,
  `set_cur_selected`, sort/filter-manager getters, static `get_subcolumns`) nets ~-90
  lines and removes the second copy of every method. No perf delta; covered by the
  existing filter/sort-route tests (`songs_panel_sort_route_navigation_targets_sort_cursor`).

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
