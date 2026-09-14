# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 366 youtui bins green (2 ignored)
**Last updated:** 2026-09-14

This file is a working backlog only — no changelog, no session archaeology. Past work
and its rationale live in git history and in the code comments / `DECISIONS.md`.

## Open

_None._

The codebase is at a local optimum across the areas this project optimizes:

- **Startup latency** — cookie export is conditional (fresh-file skip); the `ffmpeg -version`
  probe is warmed on the blocking pool so it overlaps the rest of startup; the vestigial
  `node --version` spawn (leftover from the removed custom PO-token generator) is gone.
- **Song-start latency** — measured headless 1:1: ~2.0–2.6s is yt-dlp bootstrap + resolve +
  CDN first byte (external); the in-app cost after the first byte is ~6ms (ffmpeg transcode)
  plus sub-ms decoder init. No in-app lever remains without daemonizing yt-dlp (rejected).
- **RAM** — in-memory ALAC buffers are duration/source-dependent (5.5–77MB, median ~45MB);
  cache max = 1 by design.
- **Render/CPU** — per-frame caches (title, row numbers, artist string, lowercased fields)
  are in place; no per-frame allocation on the hot path.

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
