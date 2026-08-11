# Youtui Backlog

**Build:** 0 errors, 0 warnings, 0 clippy
**Tests:** 345 youtui passed + 2 ignored (live network tests are flaky)
**Last updated:** 2026-08-11

## Completed

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
- **P1.4** — Hot path log demotion (`info!` → `trace!` on `current_span_len`).
- **P1.5** — Per-keystroke log demotion (`info!` → `debug!` in async_rodio_sink, 15 sites).
- **P1.6** — Per-download pipeline noise (`info!` → `debug!`, `info!` → `error!`, `info!` → `warn!` in song_downloader).
- **P1.7** — Per-event noise (`warn!` → `debug!`, `error!` → `warn!` across appevent, browser, app, api).

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
