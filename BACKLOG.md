# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 361 youtui + 72 ytmapi-rs lib + doctests, +2 ignored (live network tests are flaky)
**Last updated:** 2026-08-13

## Completed

### Session 2026-08-14 — Throttle-relay race hardening; streaming_buffer dedup

- **Site-3 throttle-mark race closed (`c9b706c`).** In the streaming-init-fail → full-download fallback, `stdout_handle` resolves on the writer task's stdout-EOF, which can beat the async ffmpeg stderr handler's 403 mark — so a mid-fallback CDN refusal could be misclassified as a generic exit failure (song skipped) instead of relay-retrying. The fallback-exit path now yields once before `throttled_url_retry`, matching the existing Site-2 (empty-pipe) yield-once + re-check pattern. All three throttle-retry sites now drain the mark race; Site-1 is race-free by construction (the mark is what breaks the wait loop).
- **`total_len`-record guard deduplicated (`b8f06c4`).** The `if Partial && total_len.is_none() { total_len = Some(v.len()) }` block was copy-pasted 4× (`mark_throttled`/`fail`/writer `finish`/`fail`). Extracted `record_len_if_unknown`; fail-first test `failed_or_finished_buffer_records_partial_len_as_total` covers fail/throttle/finish-with-known-total.
- **Stale L5 backlog item closed (`d7d6b84`).** The "misleading comment in streaming_buffer" was from the pre-single-Mutex/MediaSource era; the current file's comments are accurate (full-file audit).
- **Audits (no change needed):** both children (`ffmpeg` + `yt-dlp`) are `kill_on_drop`, so the throttle `continue 'attempt` drop cannot orphan a process; no stray 403-as-auth classification remains outside the committed files (UI `is_auth_error` is message-based and consistent); `empty_pipe_verdict` prioritizes `buffer_failed` so a throttle mark breaks out before the empty-pipe classification.
- **Throttle→relay loop now covered end-to-end (2 tests).** The `'attempt` loop's retry wiring was the session's central behavior but only its decision helpers were unit-tested. Two deterministic fake-binary tests run the full `download_and_decode` pipeline with fake `ffmpeg`/`yt-dlp` on PATH: `throttled_url_retries_via_relay_end_to_end` (URL-`-i` → 403+exit 1; `pipe:0` → `cat` passthrough of `test_alac_fragmented.mp4`; `--print` resolve → fake URL; relay `-o -` → fixture bytes) proves resolve → direct-URL 403 → Site-1 retry → relay → ALAC streaming decode, and asserts the throttled URL is evicted from `url_cache_get`. `throttled_relay_failure_bails_without_retry` proves a throttled relay (yt-dlp `HTTP Error 403`) bails as a generic transient failure — exactly one retry deep, no third attempt. Both serialized via `PIPELINE_TEST_LOCK` (+ semaphore + cache locks). Fail-first verified: neutering `throttled_url_retry` fails test A with `format not available (ffmpeg error)`.
- **Site-2/Site-3 yield-once retained (documented decision).** Awaiting the ffmpeg stderr handler deterministically (instead of `yield_now`) would close the residual mark-race window but requires threading the ffmpeg `JoinHandle` through the pipeline tuple. The window is already tight in practice (50ms empty-pipe poll; ffmpeg writes the 403 before exiting, so the handler is reactor-woken before `child.wait()` returns) — not worth the complexity. Test A exercises the common Site-1 path.
- Verified: cargo check 0, clippy `--workspace --all-targets` 0, 364 youtui tests green (+2).

### Session 2026-08-13 — CDN 403 throttle: browser-shaped ffmpeg fetch + relay retry

- **ffmpeg's direct-URL fetch is now shaped like yt-dlp's** — the resolved googlevideo URL is fetched with a browser `-user_agent`, a `https://music.youtube.com/` `-referer`, and `-headers "Cookie: <header>"` when the app has one (the same header yt-dlp gets via `--add-header`). A bare `Lavf/…` anonymous fetch is a bot signal, intermittently refused with `403 Forbidden (access denied)` even on a fresh URL. `build_ffmpeg_command` (song_downloader/mod.rs) builds the argv; tests assert the exact args for URL/pipe inputs with and without cookies. DECISIONS.md:28.
- **A CDN 403 on the direct-URL path is a throttle, never a skip.** The ffmpeg stderr handler (`spawn_ffmpeg_stderr_handler`) classifies 403 lines (`is_throttle_line`) and `mark_throttled`s the buffer (also failed); the new `'attempt` loop in `ytdlp_pipeline` then evicts the cached URL, sets `stream_url = None`, and retries the song once through the credential-carrying yt-dlp relay. The relay attempt is exempt from retry (`from_url_cache` false), so a throttle wave halts via the transient-failure counter instead of silently draining the queue. `HTTP Error 403` is no longer an auth-error line (`is_auth_error_line`), so a resolve-phase 403 falls through to the relay download instead of failing fast. DECISIONS.md:29-30.
- Verified: 361 youtui + 72 ytmapi-rs + doctests green, clippy 0, `cargo build --release` clean.

### Session 2026-08-13 — Filtered search survives the YTM junk-leak wave

- **Artist and song search aborted on a single junk entry — fixed (`ytmapi-rs`).** YouTube began flooding the filtered Artists/Songs shelves with unrelated content (videos, playlists, non-matching songs); entries with no `navigationEndpoint/browseEndpoint/browseId` (artists) or no album browseEndpoint in the subtitle (songs) killed the whole query via the strict `TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultArtist>/Vec<SearchResultSong>`. `"american football"` — a famous band — returned "nothing found": the filtered query errored, and the basic fallback's new layout (top-result card + itemSections, no Artists `musicShelfRenderer`) gave empty artists. Root-caused from `debug331.log` + live dumps of the exact queries the app runs; both impls now skip unparseable entries (`.filter_map(...ok())`), matching the basic-search artists path. Structurally-valid entries from other artists are kept. Fail-first tests on live-response fixtures: `test_search_artists_drops_junk_entries` (2 artists kept, 9 junk dropped), `test_search_songs_drops_junk_entries` (21 kept incl. one well-formed unrelated entry, 3 malformed dropped) — both red before the fix (reproducing the production `…/contents/20/…browseId not found` error verbatim), green after. DECISIONS.md:31.
- Verified: 72 ytmapi-rs lib + doctests green (+2), 361 youtui green, clippy 0 across the workspace.

### Session 2026-08-13 — Upstream-release and issue-routing declutter

- **justfile `release-youtui-to-aur` deleted (`2eed8c8`).** The recipe committed as `nick42d-bot` and pushed to `ssh://aur@aur.archlinux.org/youtui.git` — the **upstream** AUR package. On the fork it would have published a fork build into upstream's Arch package; with no release track (P4.1) it was both dead and a live footgun. -25 lines. Remaining recipes (`test`, `integration-test`, `doc`) verified against the actual workspace: `cargo test --bins --lib` covers 354 youtui + 100 ytmapi-rs lib tests, `--test live_integration_tests` builds from workspace root, `doc` works on nightly. `ytmapi-rs/tests/debug_dump.rs` is a live raw-JSON utility — kept, not stale.
- **`json-crawler/Cargo.toml` homepage/repository → `Icedwolf/youtui`** (`2eed8c8`), closing the last nick42d metadata holdout left by the README refresh (youtui + ytmapi-rs were realigned then). `authors` stays `nick42d` — factual lineage, consistent with the README fork-note.
- **GitHub issue templates no longer auto-assign `nick42d`** (`2eed8c8`). Fork issues would have routed to the upstream maintainer. All three templates' `assignees:` lines dropped.
- **dependabot.yml repaired** (`2eed8c8`). The file had three top-level `updates:` keys — invalid YAML, so the last (github-actions) block silently won, pointing at a workflows dir that doesn't exist (P4.1: no CI). Rewrote as one list tracking cargo for the three real package dirs `/youtui`, `/ytmapi-rs`, `/json-crawler` (the old `/` was the dep-free workspace manifest; `/youtui` — the app with the real dep surface — was never covered).
- Verified: 354 youtui + 100 ytmapi-rs tests green, clippy 0, `cargo check --workspace` clean. Pushed to `origin/main`.

### Session 2026-08-13 — README fact-fix pass (scrutiny follow-up on the fork-docs refresh)

- **Four factual errors in the just-rewritten README corrected (`d5cecf6`).** (1) "first audio frame well under a second" overstated — the startup-latency deep dive dominant cost is resolve + first-byte network fetch (2-4s+; the ~120ms floor is only ffmpeg init). Now "no pre-download buffering; first frames stream when the URL resolves and the first chunk arrives". (2) "detects a Chrome-family browser" wrong — `detect_browser_source` is Floorp-first, then Firefox, then Chromium. (3) "No prefetch: only the selected song downloads" contradicted DECISIONS.md:3/5 and code (`handle_playing` → `regenerate_downloads_for_current` → `download_upcoming_from_id`, `SONGS_AHEAD_TO_BUFFER = 1`) — prefetch is deferred until playback starts, not absent. (4) "keeps a playing buffer available for seek-back" — seek was removed; the cache's purpose is re-select/replay (`playback.rs:187` returns the cached decoder at `play_song`). Plus the awkward "lyrics-free song fetch" tucked into the auth blurb and a stale `±1` in DECISIONS.md:5 (N-1 was removed 2026-07-17 to save cache RAM). Megacomment sweep for other stale-era terms (OAuth, gapless, seek, WAV labels, rusty_ytdl) found nothing: remaining `TODO`s are genuine.
- Verified: 354 youtui tests green, clippy 0, `cargo check` clean. Pushed to `origin/main`. AGENTS.md "seek-back" phrase updated to "re-select/replay" (gitignored, local).

### Session 2026-08-13 — README + docs reflect fork reality; DECISIONS/cache-max hygiene

- **README.md rewritten (`e36feb5`).** The top-level README was 100% upstream nick42d content — dead badges (release-plz/deps.rs point at the upstream CI), an "Artist→Albums workflow" pitch that isn't this app, `download_type = "YtDlp"` and OAuth setup sections for removed features, and a roadmap listing rejected/deleted items (seeking, OAuth, gapless, lyrics, theming, offline) as open. Now describes the fork on its own terms: suckless "search → queue → play" pitch, lossless streaming pipeline (yt-dlp → ffmpeg → fragmented-MP4 ALAC, ~16MB/song), real config table (`auth_type`, `yt_dlp_command`, `volume`, `notifications_enabled`, `download_cache_size`, `keybinds`), real auth (browser cookie export + manual `cookie.txt`, po_token), the M4A fallback, E2BIG-safe subprocess note, and a **Scope** section listing what's explicitly NOT coming.
- **Downstream references realigned to the fork.** `main.rs` README anchors (`BROWSER_AUTH_SETUP_STEPS_URL`, `POTOKEN_INFORMATION_URL`, `RUNNING_YOUTUI_GUIDE_URL`) + both Cargo.toml `homepage`/`repository` → `github.com/Icedwolf/youtui`; anchor names kept so the error-message links still resolve to the new README. Stale `expired OAuth token` comment in `server/api.rs` → stale-browser-cookie framing. `CODEBASE_GUIDE.md` fixed: `playlist.rs` → `playlist/{mod,draw,playback}.rs`, `rusty_ytdl`/`rodio` dep table → yt-dlp+ffmpeg external tools, `serde_skip` → real compact-persistence pattern, removed the stale "test infrastructure issues" note. Fork notes added to all three CHANGELOGs.
- **Scrutiny follow-up on the cache-default change (`5245157`).** Two findings from the review: DECISIONS.md:14 still said "LRU eviction with 3 entries" (contradicting the just-shipped default-1 fix and the product decision) — updated to "configurable, default 1" with the 32MB rationale; and the two cache eviction/LRU tests that `CACHE_MAX_ENTRIES.store(3, ...)` never restored the global (a latent trap where a later default-assuming cache test runs under max=3) — now wrapped in a `RestoreCacheMax` RAII guard. Fail-safe, no behavior change.
- Verified: 354 youtui tests green, clippy 0, `cargo check`/release clean. Pushed to `origin/main`.

### Session 2026-08-13 — MediaSource dedup; cache default 3→1 (config audit)

- **`NonSeekableReadSource` merged into `ReadSeekSource` (`bf17112`).** The two byte-identical `Read`/`Seek` impls (only `is_seekable()`/`byte_len()` differed) collapsed into one struct with a `seekable: bool` + `length: Option<u64>`; `new(inner, length)` (seekable) and `nonseekable(inner)` constructors. −25 net lines. Streaming-init sites in `song_downloader/mod.rs` (`try_streaming_init_nonseekable` + 2 relay/streaming tests) switched to `ReadSeekSource::nonseekable`. Also **audited the decoder/streaming-init path (Option B) — no further change warranted**: all `SymphoniaDecoder` fields used; `current_span_len` eos debug fires ~1-2×/song-end (rodio calls it once per `bootstrap`, uniform.rs:55, then drops the source — not per-frame); seekable-M4A / nonseekable-ALAC split verified correct.
- **Cache default `download_cache_size` 3→1 (`58e035d`).** Config audit (P1.1 verification) found the documented "cache default 3→1" (BACKLOG memory 2026-07-08, AGENTS.md "cache max=1", DECISIONS.md:13/16) had never reached the code: `default_cache_size()` still returned 3 → a fresh install (no config.toml) defaulted to 3 ALAC entries ≈ 48MB instead of the product's ~32MB. One-line revert to 1; affects only fresh installs (live config already pins 1). Fail-first test `default_cache_size_is_one` (red before, green after). Rest of config surface verified live: `download_cache_size`, `auth_type`, `notifications_enabled`, `volume`, `yt_dlp_command`, `keybinds` all wired — no dead options.
- Verified: 354 youtui tests green (+1), clippy 0, `cargo build --release` clean. Pushed to `origin/main`.

### Session 2026-08-11 — Cache decoder single-fetch; log moved into cache impl

- **The dedup (`741a2d1`) introduced a double `cache_get`** — `cached_decoder` delegated to `create_decoder_from_cache` (one fetch) then fetched again to harvest `len` for its debug log, so every cache re-check grabbed the Mutex-locked `Arc<[u8]>` twice. Fixed (`f00c86c`): the `"Reusing cached buffer"` log now lives inside `create_decoder_from_cache` (cache.rs), where the `Arc<[u8]>` is already in hand; the `cached_decoder` wrapper is deleted and `download_and_decode`'s two re-checks call `create_decoder_from_cache` directly. Single fetch, single impl. Also pruned the now-unused `cache_get` re-export (test module imports it explicitly). Verified: 353 youtui tests green, clippy 0, `cargo build --release` clean. Pushed to `origin/main`.

### Session 2026-08-11 — Dedup cached_decoder on single cache impl

- **`cached_decoder` (mod.rs) now delegates to `create_decoder_from_cache` (cache.rs) instead of duplicating the cursor/ReadSeekSource/MediaStreamSource/SymphoniaDecoder construction.** Two byte-identical 5-line copies (the `download_and_decode` re-check pair and the `play_song` hot path) collapsed to one; `cached_decoder` keeps its `"Reusing cached buffer"` debug log on top of the shared impl. Zero behavior change, −7 net lines. Verified: 353 youtui tests green, clippy 0, `cargo build` clean. Pushed to `origin/main`.

### Session 2026-08-11 — Resolve diagnosability; stale ALAC docs; held-next audit closed

- **Resolve spawn/cancel now logged (`4047657`).** `resolve_url` emits `debug!("resolve: spawned")` on success and `debug!("resolve: cancelled")` when cancellation wins the biased `select!` — matching the relay path's existing `"yt-dlp spawned"` line. The resolve spawn was the silent gap: concurrent-resolve churn (the held-key/rapid-next class this session eliminated) is now countable from logs, so a recurrence is self-diagnosing rather than an invisible cascade.
- **Held next/prev resolve-stacking audited → closed, no change.** Rapid next-presses cancel the prior resolve before/alongside a new one: `download_and_decode` re-checks cancellation at three points (pre-start, pre-semaphore, post-semaphore) and `resolve_url` is cancellation-aware (`select!` biased + `kill_on_drop`); the `DOWNLOAD_SEMAPHORE` serializes the actual downloads. Contained by design — no speculative hot-path change.
- **Stale WAV→ALAC doc labels corrected (`369530b`).** Two production comments (`init_decoder_from`, pipeline ffmpeg gate) still described the pre-2026-08-06 WAV pipeline; now describe ALAC-in-fragmented-MP4 (`empty_moov` → moov in first ~700B) with M4A as the no-ffmpeg fallback. A test-only mp3 relay label ("WAV transcode") also realigned. Docs only, zero behavior change.
- Verified: 353 youtui tests green, clippy 0, `cargo build --release` clean. Pushed to `origin/main`.

### Session 2026-08-11 — Superseded fired regen skips scope rebuild; strict pending-only

- **A fired debounce whose token was superseded no longer rebuilds the download scope.** `apply_fired_shuffle_regen` previously ran the regen unconditionally and cleared the field conditionally — so an *older* callback finding a newer token in the field still rebuilt the scope, then the newer debounce rebuilt it again ~100ms later (two regens per burst). Now the guard gates the regen itself: only the field's live token regenerates; a superseded callback returns `Effects::none()` and leaves the newer token untouched. "Finally toggle wins" is now true of the *regeneration*, not just the token. Also `pub(crate)` → `pub(super)` (method is only used by the sibling test module). Tests (fail-first): `superseded_fired_callback_skips_scope_regen` red before the guard (returned a non-empty effect), green after. No regen/shuffle/debounce test regressed.
- Verified: cargo check 0, clippy 0, 353 youtui tests green, `cargo build --release` clean. Pushed to `origin/main`.

### Session 2026-08-11 — Fired-debounce token cleared; strict pending-only invariant

- **A fired debounce no longer leaves a stale `shuffle_regen_token` in the field.** After the sleep branch ran its regen, the recovered token stayed `Some(fired)` until the next toggle/play — the field lied about pending state. New `apply_fired_shuffle_regen` calls the regen then clears the field *only if it still holds the fired token* (`CancellationToken` `PartialEq` = `Arc::ptr_eq`), so a newer toggle scheduled between sleep-win and callback is never stomped. Test (fail-first): `debounce_fire_clears_own_token_not_a_newer_one`. Invariant is now strict: `Some` ⟺ genuinely pending.
- Verified: cargo check 0, clippy 0, 352 youtui tests green, `cargo build --release` clean. Pushed to `origin/main`.

### Session 2026-08-11 — Debounce lifecycle completed; P1 log-noise items closed

- **`handle_playing` cancels the pending shuffle regen.** Toggling shuffle then getting a song playing within the ~100ms window left both the immediate regen (`handle_playing`) and the trailing debounce to run `drop_unscoped_from_id` + `download_upcoming_from_id` on the same scope — structurally redundant. `handle_playing` now `take()`s + cancels any pending `shuffle_regen_token` before its own immediate regen, so at most one scope regeneration survives. The idle guard also clears a stale pending token instead of leaving it to fire a dead regen. Tests (fail-first): `handle_playing_cancels_pending_shuffle_regen`, `idle_toggle_clears_pending_token`.
- **Backlog staleness pruned:** P1.4–P1.7 (log-noise demotions) were already done but still listed as open — verified no `info!` remains in `async_rodio_sink`/`decoder`/`song_downloader`; remaining `info!` sites are the allowed startup/auth/retry/event class. Re-labeled ✅ DONE with rationale.
- Verified: cargo check 0, clippy 0, 351 youtui tests green, `cargo build --release` clean. Pushed to `origin/main`.

## Completed

### Session 2026-08-11 — Strict single-download invariant; shuffle-regen debounce

- **Strict single-download invariant (permit lives in the bg fill).** `ytdlp_pipeline` dropped `_permit` at all 4 streaming-init sites (ALAC+M4A, success+fail): on streaming success the decoder returned immediately while `spawn_bg_cache_task` kept ffmpeg alive downloading the rest, so `handle_playing`'s next prebuffer found the semaphore free and spawned a **second concurrent ffmpeg** — 2 ffmpegs were the steady state on every song start. `spawn_bg_cache_task` now takes an owned `SemaphorePermit<'static>`; streaming-success sites **move the permit into the bg task** (held until fill completes/cancels/kills), streaming-fail sites hold it to pipeline return. Tradeoff documented in DECISIONS.md:2 (adjacent-song select waits for the current fill; distant skip still cancels instantly). Named tests: `bg_cache_task_holds_permit_until_complete`, `permit_released_after_cancel_not_before`. `SEMAPHORE_TEST_LOCK` serializes semaphore tests against parallel-runtime permit stealing.
- **Post-acquire cache re-check (replay hardening).** A re-select of the current song during its own fill blocked on `acquire()` then re-downloaded a now-cached song. New `cached_decoder(video_id)` helper; `download_and_decode` re-checks cache after acquiring the permit + cancel check. `cached_decoder` returns `Arc<[u8]>` for zero-copy cache hits.
- **Held-shuffle-key download churn (P1).** Resolve runs outside the semaphore by design, so every `toggle_shuffle` → `regenerate_downloads_for_current` spawned a fresh yt-dlp before the previous was cancelled — a key repeat burst ran several resolves at once. `toggle_shuffle` now schedules a **trailing-debounced** regeneration (`SHUFFLE_REGEN_DEBOUNCE_MS = 100`): shuffle *order* applies synchronously (UI stays instant); only the network scope-regeneration coalesces to the final toggle of the burst, each previous pending regen cancelled by its own token (`tokio::select!` biased on `token.cancelled()`). `handle_playing` prebuffer stays immediate. An **idle guard** skips the debounce timer entirely when nothing is playing (regen would no-op anyway). Tests: `held_shuffle_key_burst_keeps_only_latest_regen_token`, `idle_shuffle_toggle_does_not_schedule_regen`.
- Verified: cargo check 0, clippy 0, 349 youtui tests green, `cargo build --release` clean. Pushed to `origin/main`.

### Session 2026-08-11 — Footer duration frozen at 00:00; startup volume never applied

- **Footer duration stuck at `00:00` / gauge frozen (P1).** `draw_footer` resolved a non-zero `duration` only inside the `Playing|Paused` branch, but cached `duration_str` keyed on **song-id change** alone. The id first transitioned during `Buffering` — when duration was still literally `0` — so `00:00` was committed to the cache; when `handle_playing` later flipped to `Playing` (setting `actual_duration`), the id was unchanged and the cache never refreshed → `00:00` + ratio `progress/0` = frozen gauge for the whole song. Fix: extracted pure `refresh_footer_cache` (footer.rs) — duration string keyed on its own resolved value (`last_duration`), song/album strings still keyed on id (no per-frame alloc). `draw_footer` now resolves duration for any active id (Buffering included) and renders the metadata total the moment playback starts. 5 new tests, including `duration_string_refreshes_when_playing_starts_same_song_id` (the exact Buffering→Playing transition that regressed). (`draw_media_controls.rs` was already per-frame; the MPRIS pause state-change bug is fixed below.)
- **Configured volume never reached the audio device (P2).** At startup `rodio::Player` defaults to `1.0` regardless of `config.volume`, so the footer number and the audible level disagreed until the first `+`/`-` press (footer 50 / audio 100% when no `volume` key → `default_volume()` 50). `YoutuiWindow::new` now pushes a one-shot startup effect: `server.player.set_volume(config.volume)` and applies the returned `VolumeUpdate` to the playlist. Footer and device agree from the first frame; adding `volume = 100` to config.toml now actually takes effect.
- **MPRIS stuck on Playing after pause (P1).** `update_progress` diffed on position delta alone, so a pause a few seconds after the last progress update never pushed to the platform — and since position stops moving while paused, it stayed stuck on Playing forever. `update_progress` now takes an explicit `playing` gate: a Playing↔Paused state change always pushes; `POSITION_DIFFERENCE_REDRAW_THRESHOLD` throttling applies only to progress updates within the same state. 4 tests (`pause_state_change_pushes_even_within_progress_throttle`, `resume_state_change_pushes_even_within_progress_throttle`, `progress_update_still_throttled_within_same_state`, `start_playback_from_stopped_pushes`).
- Note: tree-wide `cargo fmt -- --check` fails under every installed rustfmt (1.90/1.91/1.94/nightly/stable) — 272 hunks across ~25 files predate this work (older-rustfmt formatting). fmt is not a required gate; migration deferred.
- Verified: cargo check 0, full test suite 345 passed (+2 ignored), clippy 0, `cargo build --release` clean.

### Session 2026-08-10 — Artist/song search status feedback; concurrent fallback fusion

- **Search headers now tell the truth** — the `ListStatus`-driven titles (`... - loading` / `- N results` / `- no songs found` / `- Error received`) were dead plumbing: song search never set any state, and artist search masked genuine failures as `nothing found`. Now `search()` sets `Loading`, the success path `Loaded`, a failed query `Error` — on both `SongSearchBrowser` (songsearch.rs:530/584/547) and `SearchPanel` (already wired for artists). Spinner + title render from the existing `draw_loadable`; zero draw changes.
- **Artist fallback fusion (latency + truth)** — filtered (artists-only) query runs concurrently with a basic-search fallback; filtered non-empty wins (fallback aborted, common path pays one round trip not two). Fusion is a pure `fuse_artist_search` (api.rs): a success that returned empty still counts as "nothing found"; only when **every** query fails is the panel put in the error state (was: `Ok(empty)` on any failure — misleading). Filtered/basic JoinError + API errors are `warn!`ed with the real cause before flattening (was: swallowed). 8 new tests (6 fusion matrix + 2 song-search state), fail-first.

### Session 2026-08-07 — Release-bench repair; duration-const naming; env invariant docs

- **Release-profile benchmark module repaired.** `structures.rs:1101` `bench_create_with_metadata` still passed a stale 6th arg to the 5-arg `create_with_metadata` (E0061), and `criterion_benches` used deprecated `criterion::black_box` (6 sites). Both broke `cargo test --release` / `cargo clippy --release --tests` compile of the `#[cfg(all(test, not(debug_assertions)))]` block — the P4.2 criterion workflow and the "0 clippy" full-profile claim silently died. Dropped the stale arg; `criterion::black_box` → `std::hint::black_box`. Now `cargo test --release -p youtui bench_` runs (5 passed).
- **Magic number named.** `7200` in `resolve_display_duration` → `MAX_PLAUSIBLE_DURATION_S` (`drawutils.rs`), documented.
- **Env-isolation invariant recorded.** DECISIONS.md:24 + AGENTS.md architecture invariants: children never inherit the parent `envp` (apply_child_env allowlist, E2BIG impossible); also fixed the stale "WebM→WAV / 42MB WAV" invariant lines to ALAC/fragmented-MP4.

### Session 2026-08-07 — Subprocess env isolation; streamed-ALAC duration display

- **E2BIG spawn failures root-caused and fixed (`e9c90f6`).** A live report — every song "instantly" failing with 5 consecutive download errors — surfaced `execve` errno **E2BIG** (`Argument list too long`) via the resolve-spawn logging: the app forwarded its *entire inherited `envp`* to every child, so an oversized launch environment blew past `ARG_MAX`/`MAX_ARG_STRLEN` and the OS refused every `spawn`. Reconstructed argv was ~2.4KB max (the `--add-header Cookie:` header); injecting one 200KB env var reproduces the exact error. Fix: `apply_child_env(&mut Command)` (`song_downloader/mod.rs`) **env_clears** children and re-adds a bounded allowlist (`PATH, HOME, LANG, LC_*, TMP*/TEMP, XDG_*, proxy, SSL_CERT_*`) — child env is now provably small, so E2BIG is structurally impossible regardless of parent env. Applied to yt-dlp resolve, yt-dlp relay/M4A, and ffmpeg. This is also the errno behind the older 2376× silent `spawn yt-dlp` cascade. Init-only checks (node version, `check_ffmpeg`, cookie export) intentionally left inheriting env: they degrade gracefully (js-runtime off, M4A fallback, no auto export) rather than blocking playback. Tests (fail-first): oversized-env E2BIG, env-clear rescue, PATH/proxy preservation.
- **Streamed-ALAC duration display fixed (`80a981c`).** Bottom bar showed no progress and `0:00` total for ALAC playback. Fragmented-MP4 with `empty_moov` writes `mdhd.duration = 0`; symphonia's isomp4 sets `n_frames = mdhd.duration` (demuxer.rs:48), so the decoder reported `duration = Some(0)` — which the footer's `secs < 7200` filter accepted, never falling back to metadata. New shared `resolve_display_duration(actual, meta_secs)` (`drawutils.rs`) rejects zero *and* bogus-huge durations and falls back to `duration_secs`; replaces the two divergent inline copies (footer.rs + draw_media_controls.rs, the latter was also missing the huge-value clamp). 5 tests.

### Session 2026-08-07 — Spawn consolidation + upstream review (nothing to merge)

- **Download-pipeline spawn layer flattened.** `spawn_ffmpeg(FfmpegInput::Url|Pipe, writer, label)` — both ALAC ffmpeg sites (resolved-URL + yt-dlp relay) share one builder (`ALAC_FFMPEG_ARGS` + piped stdio + kill_on_drop), returning `FfmpegSpawn { stderr-logger, buffer-writer, child, stdin }`. `spawn_ytdlp(cfg, format, buffer, t0, log_cancellation)` — relay + M4A fallback share build/spawn/stderr-classifier, returning `YtDlpSpawn { stderr_handle, stdout, child }`. The relay's kill_on_drop orphan rationale moved into the struct doc.
- **Dead/auth error-message contract deduped.** `DEAD_VIDEO_ERR` + `AUTH_ERR` consts in song_downloader; the 4 plain bail sites use them (the resolve path keeps its po_token-suffixed variant — the UI's `starts_with` classifiers tolerate it). Removes the string-drift fragility class that caused the earlier auth-classification split.
- **Micro-cleanup:** dropped the pointless `ffmpeg_streaming = ffmpeg_avail` rebind; `_t0`→`t0`, `_total_len`→`total_len` (both were used); removed 3 dead `Some(_current_id)` bindings in playback.rs.
- **Upstream review — all 4 commits N/A (nothing to merge).** `47598e7` release v0.0.38 = version chore (divergent fork, no release track). `245a7dc` yt-dlp hyphen-ID fix (#380) = structurally impossible for us — we pass a full `https://music.youtube.com/watch?v={id}` URL (resolve.rs:117), never a bare positional ID, so a `-`-prefixed ID can't be misparsed as a flag (`--default-search ytsearch` moot — host forces the extractor). `eacae62` actions/checkout 6→7 = CI-only, we have no CI. `a2d6870` openssl bump = dormant optional chain — the release binary links no `libssl`/`libcrypto` (`ldd` verified); openssl/native-tls/hyper-tls in Cargo.lock are reqwest's *optional* default-tls closure (normal Cargo.lock v3 behavior), never enabled (rustls-only via `ytmapi-rs/rustls`). `cargo tree -i openssl` confirms it's not in the active graph. Lock left as-is (pruning would re-resolve and risk unrelated bumps).
- Verified: clippy 0, 570 workspace tests green, release build clean. 55 commits pushed to `origin/main`.

### Session 2026-08-07 — Review: halt counter excludes dead/auth; code polish

- **False halt on dead/auth songs (review finding).** `consecutive_download_failures` incremented on *every* non-cancellation error, so 5 consecutive dead-video (`video unavailable`) or auth (403/sign-in / 18+) failures fired `halt_on_download_failures` — stopping playback and cancelling all downloads on a perfectly healthy system. Dead videos and auth failures are definitive per-song/per-session conditions (each already notifies + skips + advances; dead-video additionally session-remembers), not signs of a systemic failure. The halt now counts only transient/systemic errors (429, format loss, spawn errors) — the debug304 `spawn yt-dlp` cascade it was built for. Tests (fail-first): `repeated_dead_videos_do_not_halt` + `repeated_auth_errors_do_not_halt` (312 passed). DECISIONS.md:23.
- **Post-ALAC refactors (DRY, zero behavior change).** `ALAC_FFMPEG_ARGS` const — the 12-arg fragmented-mp4/ALAC mux tail lives in one place, both ffmpeg sites (stream_url + relay) just prepend `-i`. `is_session_dead_video(id)` helper shared by `get_next_song_id` + `first_live_song_id`. Dead-error branch extracts `(video_id, title)` in one song lookup.
- **ffmpeg stderr piped to a read-only logger** (both sites) so a `source exited, empty pipe` bail is self-diagnosing (ffmpeg runs `-loglevel error`; the line logged right before the bail names the real cause). Handler never touches the buffer — pure diagnosability, cannot change playback.
- Verified: 312 youtui tests green, clippy 0, `cargo build --release` clean (huge else block).

### Session 2026-08-06 — Deleted/stale tracks: session-only skip, no auto-removal

Fix for "tracks I already played keep coming back / removed from queue but still replay". The dead-video signal (`video unavailable`) is not reliable — it has falsely flagged valid songs — so auto-removal was the wrong lever. Per user: failsafe first.

- **No song is ever auto-removed for a dead video.** Deleted `remove_unavailable_song` + its list-drain index maintenance. A dead song stays in the queue, marked `Failed`; playback advances past it as before (`handle_set_to_error` → `play_next_or_stop`).
- **Session-only memory (`session_dead_videos: HashSet<String>` on `BrowserSongsList`).** Filled on any dead-video error, never persisted, cleared on process exit — a wrongly-flagged valid song recovers next restart. Auto-advance (`get_next_song_id`) and the end-of-queue wrap (`first_live_song_id`) skip session-dead songs, so the same refused song is never retried on its own and a dead-only queue stops cleanly instead of looping. Manual re-select still allowed (retries once, fails fast).
- **Wrap-stop made synchronous** (set `NotPlaying` + `NotQueued` + clear preloaded inline) so a dead-only single-song queue stops immediately rather than dangle in `Error` pending an effect — matches `halt_on_download_failures`.
- **Tests (fail-first):** `permanently_unavailable_song_is_skipped_but_kept_in_queue` (kept + session-remembered + advances), `single_dead_song_stops_cleanly` (kept, playback stops), `session_dead_song_is_skipped_on_auto_advance` (jumps over dead to song 2), `session_dead_is_not_persisted_across_playlists`. Test count 308 → 310.
- Verified: 310 youtui tests green, clippy 0, fmt clean, `cargo build --release` clean.

### Session 2026-08-06 — Primary stream format: WAV → fragmented-MP4 ALAC

**RAM cut 42MB → ~16MB per song (2.6×), lossless, true streaming preserved — all measured, not assumed.**

- **Gate proven first (no gambling):** dry-ran the ffmpeg muxer on a real stream. `-movflags empty_moov+frag_keyframe` (and `frag_duration`) **buffer the whole file until the pipe closes** — they do NOT stream (observed with real-time `-re` input: output stayed at 28B `ftyp` until the end). **`+frag_every_frame` is the only flag that streams incrementally** — moof/mdat pairs appear progressively (t=3.2/6.6/10.1s). Atom order `ftyp(28B)→moov(683B, with the `alac` sample entry)→moof→mdat`, so the decoder inits from the first ~700B — the same partial-buffer streaming WAV had.
- **symphonia 0.5.5 decodes ALAC-in-fragmented-mp4** — added `symphonia-alac` to rodio's features (one line); unification enables `alac` on the shared `symphonia` crate, so BOTH rodio and the direct `decoder::register_enabled_codecs` get it. Single 0.5.5 tree verified.
- **Non-seekable MediaSource required for streaming** — isomp4's *seekable* branch `seek error during decode` on a growing buffer (seekable + `byte_len=None` → reader's `SeekFrom::End` blocks until `finish()`). Added `NonSeekableReadSource` (read-only, `is_seekable()=false`); isomp4 then takes its incremental branch (demuxer.rs:388-398). The full-file fallbacks still use seekable `ReadSeekSource`.
- **Production change:** both ffmpeg arg sites (stream_url + relay) → `-f mp4 -movflags empty_moov+default_base_moof+frag_every_frame -c:a alac`; streamed init uses `try_streaming_init_nonseekable`; `is_wav` → `ffmpeg_streaming`; `"wav"` labels → `"alac-mp4"`/`"mp4-fallback"`. M4A no-ffmpeg fallback unchanged. `set_total_len` left in place (reserves the opus size, harmless).
- **Tests (fail-first):** `alac_fragmented_decodes_from_full_buffer` + `alac_fragmented_streams_from_partial_buffer` (asserts streaming TTF < full-write time) via new fixture `test_alac_fragmented.mp4` (19743B, 130 fragments). Both red until the non-seekable fix.
- **RAM inventory (point 2-mandate):** UI side already lean (`get_title` guarded draw.rs:85 — allocs only on invalidation; rows/artists pre-cached). The stream buffer was the only fat lever → now ~16MB. Cache max=1: ~16MB playing + ~16MB cached ≈ 32MB (vs 84MB WAV).
- Docs updated: DECISIONS.md (9-12, 13, 16, 21), AGENTS.md (constraints + code decisions + preference #5 now "Prefer lossless streaming"), Cargo.toml comments.

### Session 2026-08-06 — Halt on download-failure cascade, resolve spawn logging

Diagnosis of a live "looped skipped/removed all the songs" report (debug304.log, an older build) surfaced two gaps:

- **Systemic failure drained the whole queue** — 2376 × `error=spawn yt-dlp` across one session: the OS refused to exec new processes (fd/pid/mem limits all healthy afterward — a transient system-wide exec failure). Every buffering failure called `handle_set_to_error` → `play_next_or_stop` → next download → fail, walking the entire playlist off with no visible reason. Fixed (`69cef25`): a `consecutive_download_failures` counter — incremented on every non-cancellation failure of the currently-buffering song, reset on any successful `Completed`. At `HALT_AFTER_CONSECUTIVE_FAILURES = 5` the player halts (`NotPlaying`), clears the queue, and fires one "Download Failures" notification instead of draining the list. Tests written fail-first: `repeated_transient_failures_halt_instead_of_draining` (failed before — advanced to `Buffering(5)`), plus `transient_errors_below_threshold_still_advance` and `successful_download_resets_failure_counter` guards. Test count 561 → 564.
- **Resolve spawn errors were silent** — `resolve.rs` swallowed the `io::Error` from `cmd.spawn()` (`Err(_) => Failed`), so a systemic spawn failure surfaced only as the pipeline's cryptic `spawn yt-dlp` fallback. Now `warn!`s the actual io error (`2ebbf55`), making the next occurrence self-diagnosing. Next time the OS exec fails, the log says *which* error (EMFILE/EAGAIN/ENOMEM/ENOENT).
- **Pipeline dedup + audit** (`bbabbd8`) — three duplicated patterns collapsed into helpers: `evict_cached_url` (7× `if from_url_cache { url_cache_remove }`), `bail_failed_buffer` (2× dead/auth/generic failure-classify blocks), `build_ytdlp_command` (2× yt-dlp `Command` + auth-args). Streaming confirmed: WAV is true streaming (decoder init at 512B of a growing buffer); the M4A full-download path is the no-ffmpeg fallback only. Startup-latency deep dive: ffmpeg `-probesize`/`-analyzeduration`/`-vn` measured **no gain** (~120ms floor regardless — `-fflags nobuffer` already neutralizes probing, dominant cost is resolve + first-chunk network fetch), so **no flag change added**. No per-chunk allocation/noise (bg stall poll = 1s; no `Downloading(%)` spam). Tests remaining 564-green.

### Session 2026-08-05 — Review follow-up: empty-pipe classification, cookie-dir window

Scrutiny of the day's commits surfaced and closed two issues:

- **Classification dropped on zero-byte failure** — the empty-pipe patience loop checked `is_finished()` before `buffer.is_failed()`, so a source that *failed* with zero bytes (auth-blocked 18+, or permanently-dead video at download time) had its writer exit → generic `format not available (empty pipe)` bail. The buffer's dead-video/auth classification (set by the stderr handler) never surfaced: no auth notification, no auto-removal. Extracted `empty_pipe_verdict` (pure, priority: failed beats exited) + reordered via `1be3a2e`; regression test `empty_pipe_verdict_prioritizes_failed_over_exited`. Also dropped the misleading `"empty pipe after {DECODER_INIT_DEADLINE_S}s"` label (that branch isn't a timeout — the source just exited). Test count 560 → 561.
- **Cookie-dir created world-readable pre-lockdown** — `copy_cookie_db_for_export` still created `youtui-cookies-{pid}` at 0755 then locked it down *after* copying, leaving a (microsecond) window where the SID copy sits in world-readable `/tmp`. Now the dirs are built at 0700 up-front (`DirBuilder::mode(0o700)`), and a chmod failure logs a warning instead of being swallowed (`1e13efe`).

## Completed

### Session 2026-08-05 — Download-task panic net, cookie temp-dir perms

- **Stranded-Buffering bug (P4)** — `download_song`'s fire-and-forget task sent `Completed`/`Error` via channel on Ok/Err, but a *panic* inside `download_and_decode` skipped both → song stuck in `Buffering` forever with a leaked `active_downloads` entry. Fixed (`d32fa3e`): the await is wrapped in `std::panic::AssertUnwindSafe(...).catch_unwind()` (same convention as `effect.rs:239`); a panic now emits `Error("download panicked: …")` → song marked Failed + skipped.
- **Latent downcast bug caught by test-first** — a `&(dyn Any + Send)` reference fails to `downcast_ref` (probe-verified) while auto-deref through `&Box<dyn Any + Send>` works. The new `panic_message` helper (core.rs) takes `&Box` — the same reason `effect.rs` downcasts on the Box. Dedup: `effect.rs` now reuses `panic_message` (removed its inline copy). Unit test `panic_message_normalizes_panic_payload` covers `&str`, `String`, and opaque payloads. Test count 559 → 560.
- **Cookie temp-dir perms** — the copied-profile fallback (`copy_cookie_db_for_export`) created `youtui-cookies-{pid}` with 0755 in world-readable `/tmp` and copied `cookies.sqlite` (SID tokens) at 0644. `lock_down_export_perms` now sets dirs 0700 + copied files 0600 (`fd6ec13`). The existing fallback test's fake yt-dlp now `stat -c %a`-enforces 0700 on the copied profile dir, so the test discriminates pre/post fix (umask 022 → 0755 fails).

## Completed

### Session 2026-08-05 — Review follow-up: auth consistency, patience ordering, M4A reachability

Follow-up review of the day's fixes surfaced and closed four issues:

- **yt-dlp auth shadowing (18+ skips when the export is guest)** — `apply_ytdlp_auth_args` passed `--cookies <file>` on non-empty alone, so a guest/stale Netscape file shadowed the `--add-header` fallback. The API client (`server::resolve_cookie_header`) was already auth-aware; yt-dlp is now too: `file_has_auth_cookie` (shares the `AUTH_COOKIE_NAMES` set with server.rs) gates `--cookies`, falling back to the header. Two new tests (`guest_cookie_file_falls_back_to_header`, `signed_in_cookie_file_uses_cookies_arg`).
- **Patience-loop ordering** — the empty-pipe loop checked `is_finished()` before `buffer.len() > 0`, so a source that wrote bytes then exited was mislabeled "empty pipe". Produced-bytes now wins. Also fixed a stale `buf_len = current` (0) in the init debug line.
- **M4A fallback unreachable without ffmpeg** — `is_wav = ffmpeg_avail || from_url_cache` plus an unguarded ffmpeg-spawn branch meant a no-ffmpeg system with a resolved (webm) URL tried `ffmpeg.spawn()` and failed; the documented M4A fallback only ran when resolve failed. `is_wav = ffmpeg_avail` + the URL branch is now gated on ffmpeg.
- **Startup auth-state log** — `auth: signed-in|guest-only|no browser` line after cookie resolution, so 18+ skips are diagnosable from logs.

Verified live: export → SID-bearing file → `file_has_auth_cookie` true → full app-args resolve returns `itag=251 audio/webm`.

## Completed

### Session 2026-08-05 — Auth was silently broken; slow-start songs falsely skipped

- **Cookie export never worked (root cause of every 18+/age-restricted skip)** — `run_one_cookie_export` invoked `yt-dlp --cookies-from-browser <browser> --cookies <out>` with **no URL**, so yt-dlp always errored `You must provide at least one URL` and the export left a 0-byte file. Auth therefore never reached yt-dlp in-app; the copy-profile fallback (`1154fdc`) was unreachable because the direct attempt always failed first. Fixed (`023578a`): point the export at a fast-failing probe URL `https://cookie-export.invalid/` (RFC-2606 `.invalid` never resolves; yt-dlp dumps cookies at startup *before* URL validation), and change success from `exit code == 0` to **non-empty cookie file** (the probe URL's own extraction can fail — storyboards only — while the cookies were already written). Verified live: 1398-line Netscape export with SID from the running Floorp profile, and from a copied `cookies.sqlite` fallback. (The original `is_nonempty_cookie_file` + `--add-header` auth was fine all along — the export feeding it was the broken half.)
- **Playable songs skipped (slow start, not dead pipe)** — log `debug302.log`: `F_Utndr52QA ... format not available (empty pipe after 5s)`, then played fine on the press. A fresh resolve is valid but can take >5s to deliver its first 512 bytes (cold TLS, throttled start); the old empty-pipe bail at `DECODER_INIT_DEADLINE_S = 5s` treated that as a dead pipe. Fixed (`8c089e7`): the bail now checks `stdout_handle.is_finished()` — a source that **already exited** is dead/unavailable (bail + evict), while one **still running** is given `EMPTY_PIPE_PATIENCE_S = 20s` more for the first byte before being called empty. A song that needs a slow first byte is now waited on, not skipped.

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
- **P1.4** — Hot path log demotion (`info!` → `trace!` on `current_span_len`). ✅ DONE (currently `debug!` in decoder/mod.rs:185).
- **P1.5** — Per-keystroke log demotion (`info!` → `debug!` in async_rodio_sink, 15 sites). ✅ DONE.
- **P1.6** — Per-download pipeline noise (`info!` → `debug!`, `info!` → `error!`, `info!` → `warn!` in song_downloader). ✅ DONE.
- **P1.7** — Per-event noise (`warn!` → `debug!`, `error!` → `warn!` across appevent, browser, app, api). ✅ DONE (session 2026-07-16 N3–N8 + backlog item). Remaining `info!`/`warn!` are startup/auth/retry/error-event class only.

### P2 — Code Quality

- **P2.1** — Remove dead `decoder_integration_test.rs` ✅ DONE (file already absent; backlog entry stale)
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
| B3 | `_permit` dropped while background cache still streaming → parallel download | **WASTE** | ✅ FIXED — the original "false alarm" rationale was wrong: the old bg task's ffmpeg *does* keep a live network/transcode stream until the fill completes, so dropping the permit re-enabled parallel downloads. The `DOWNLOAD_SEMAPHORE` permit now lives in the bg cache task and is held until the fill completes/cancels/kills — one download truly at a time. |

## Completed

### Session 2026-08-11 — Strict single-download invariant (permit lives in the bg fill)

- **Early permit release was wrong; now structurally impossible (P1).** `ytdlp_pipeline` dropped `_permit` at all 4 streaming-init sites (`mod.rs`, ALAC+M4A, success+fail). On streaming success the decoder was returned immediately and the permit freed, while `spawn_bg_cache_task` kept ffmpeg/yt-dlp alive downloading the *rest* of the song — so the next prebuffer download (started by `handle_playing` → `download_upcoming_from_id`) found the semaphore free and spawned a **second concurrent ffmpeg** on top of the still-still streaming one. 2 ffmpegs were the steady state on every song start. B3's "old bg reads already-downloaded data, no new HTTP request" was wrong: ffmpeg (URL path) / yt-dlp (relay path) keeps downloading for the whole fill.
- **Fix:** `spawn_bg_cache_task` gained an owned `SemaphorePermit<'static>` param; the two streaming-success sites (`mod.rs:762` ALAC, `:863` M4A) **move the permit into the bg task** instead of dropping it — the permit is now held until the fill completes, the task is cancelled, or the child is killed. The two streaming-fail → inline-full-wait sites no longer drop it either; it's held until `ytdlp_pipeline` returns. `ytdlp_pipeline`'s param is now explicitly `SemaphorePermit<'static>` (the semaphore is a `'static` static, so this is sound).
- **Tradeoff (documented in DECISIONS.md:2):** selecting the *next* adjacent song waits for the current song's fill to finish (transcode/network seconds, not playback time). Skipping to a distant song still cancels the old fill instantly (`drop_unscoped_from_id`). No deadlock: the permit is only released on fill-complete/cancel/kill.
- **Tests (fail-first):** `bg_cache_task_holds_permit_until_complete` (permit held while running, released after join), `permit_released_after_cancel_not_before` (freed only after the bg task future resolves), both red until the signature change. Existing `bg_cache_task_cancel_kills_all_children` updated to the 9-arg signature. Added `SEMAPHORE_TEST_LOCK` — parallel runtimes racing the global `DOWNLOAD_SEMAPHORE` would steal each other's permit (3 passed; the old 2 were silently serialized by luck).
- Verified: `cargo check` 0, 347 youtui tests green, clippy 0, `cargo build --release` clean.
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
| L5 | Misleading comment in streaming_buffer | ✅ DONE — no misleading comment remains; file reworked since (WAV→ALAC, single-Mutex, MediaSource dedup) and audited 2026-08-14 |
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
