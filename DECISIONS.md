# Product Design Decisions

Critical invariants and rationale. **Read before changing playback/download code.**

## Playback Flow (priority order)

1. **Single-song hot path is priority.** User searches → selects → plays. No playlist walking optimization. The download pipeline must optimize for this case.

2. **No parallel downloads.** Semaphore=1, one download at a time. Multiple yt-dlp processes compete for bandwidth and slow down the song the user actually selected. The semaphore permit moves into `spawn_bg_cache_task` at streaming-init success (and is otherwise held through `ytdlp_pipeline`), so it is released only when the fill completes, its token is cancelled (a distant song skip), or the children are killed — a second ffmpeg cannot spawn while the current song's fill is still streaming. Tradeoff: selecting the *next* adjacent song waits for the current fill (transcode/network seconds — ffmpeg transcodes at 20-50× realtime, not playback-time).

3. **No prefetch before playback.** `prepare_playback_id` must NOT call `download_upcoming_from_id`. Prebuffer only fires AFTER `handle_playing` — the selected song gets 100% download bandwidth first.

4. **Start playing ASAP.** Every millisecond between select and first audio frame is valuable. Decoder init latency, ffmpeg latency, buffer thresholds — all fair targets.

5. **Prefetch scope starts AFTER song plays.** When `handle_playing` fires, `regenerate_downloads_for_current` queues only the next song (`SONGS_AHEAD_TO_BUFFER = 1`, no previous-song seek-back — that was removed to save cache RAM). This is the only prebuffer trigger.

## State Machine Invariants

6. **`start_buffering` must NOT set `download_status = Queued`.** Doing so causes `download_song` to bail early — no download starts, song stuck in Buffering forever. The prebuffer already excludes the current song via `get_cur_playing_id()`.

7. **After `play_song`, the download pipeline must be active.** `download_status != None` AND `active_downloads` must contain the song ID. Test: `play_song_advances_download_status`.

8. **Buffering state requires an active download.** `play_status == Buffering(id)` implies there MUST be a download task for that id. If not, song never transitions to Playing.

22. **Dead/deleted tracks are never auto-removed.** The `video unavailable` signal is not reliable (has falsely flagged valid songs). A dead song stays in the queue, marked `Failed`; playback advances past it. `session_dead_videos` (in-memory only, per-process) makes auto-advance and the end-of-queue wrap skip that song so the same refused video is never retried on its own; a dead-only queue stops cleanly. A wrongly-flagged song recovers on restart. Manual re-select still allowed (retries once, fails fast).

23. **The download-failure halt counts only transient/systemic failures.** `consecutive_download_failures` (halt + stop at `HALT_AFTER_CONSECUTIVE_FAILURES`) is incremented only for non-cancellation, non-dead, non-auth errors (rate limits, format loss, spawn errors). A dead video or auth/18+ failure is a definitive per-song/per-session condition, never a sign of a systemic download problem — it advances the queue with its own notification instead of feeding the halt. This keeps a run of deleted (or age-restricted) tracks from spuriously stopping the whole player.

24. **Subprocesses never inherit the parent `envp`.** Every yt-dlp/ffmpeg child runs through `apply_child_env` (`song_downloader/mod.rs`): `env_clear()` + a small allowlist (`PATH, HOME, LANG, LC_*, TMP*/TEMP, XDG_*, proxy, SSL_CERT_*`). An oversized launch environment made `execve` fail with **E2BIG** (`Argument list too long`) — `spawn yt-dlp` / `ffmpeg` refused for every song (the errno behind the older 2376× silent spawn cascade, first surfaced by the resolve spawn logging). Bounding the child env makes the failure structurally impossible regardless of how the parent was launched. `apply_child_env` is generic over a sync/async `ChildCommand` trait, so the spawn-side checks (`check_ffmpeg`, node version, cookie export) run the same bounded env too — a hostile env can no longer silently disable ALAC streaming (M4A fallback) or break the cookie export at startup.

## Audio Format Constraints (symphonia 0.5)

9. **No Opus support.** symphonia 0.5's `all-codecs` = {aac, adpcm, alac, flac, mp1, mp2, mp3, pcm, vorbis}. No `opus` feature exists in any 0.5.x version.

10. **WebM/Opus → ffmpeg pipe to fragmented MP4+ALAC.** Default path: `bestaudio[ext=webm]` piped through ffmpeg to lossless ALAC in a **fragmented MP4** container (`-f mp4 -movflags empty_moov+default_base_moof+frag_every_frame -c:a alac`). `frag_every_frame` is the ONLY flag that streams incrementally — `frag_keyframe`/`frag_duration` buffer the whole file until the pipe closes (measured). ALAC is lossless (no quality regression) at ~16MB/song vs 42MB WAV. `empty_moov` puts ftyp+moov (with the ALAC sample entry) in the first ~700 bytes, so the decoder inits from a partial buffer exactly like WAV did. (Changed from WAV 2026-08-06 — see item 12.)

11. **M4A/AAC works but cannot stream.** isomp4 format reader + aac codec enabled via rodio's `symphonia-isomp4`/`symphonia-aac`. `byte_len` MUST be actual total file size because isomp4 seeks to end for moov atom. Full download-then-decode only. This is the no-ffmpeg fallback.

12. **Fragmented MP4 streams from partial buffer via a NON-SEEKABLE source.** isomp4's seekable branch seeks past mdat during decode (`seek error during decode`) and the growing `SharedBuffer` can't serve those seeks. A non-seekable MediaSource (`NonSeekableReadSource`) forces isomp4's incremental branch (demuxer.rs:388-398). The ALAC streamed path uses `try_streaming_init_nonseekable`; the full-file fallbacks keep the seekable `ReadSeekSource`. `byte_len` is `None` for the streamed path.

## Cache Design

13. **`Arc<[u8]>` for BYTE_CACHE.** Cache hits avoid cloning large buffers — refcount bump ~8 bytes vs full Vec clone. `cache_get()` returns `Arc<[u8]>`.

14. **LRU eviction, size configurable, default 1.** `AudioCache` struct with `HashMap<String, Arc<[u8]>>` + `VecDeque<String>` order behind single `Mutex`. Oldest entry evicted when full. Capacity from `config.download_cache_size` (`CACHE_MAX_ENTRIES`), default **1** — one ALAC buffer playing + one cached ≈ 32MB. Do not raise the default; multi-entry caches are a user opt-in.

15. **Single Mutex for cache.** `BYTE_CACHE` and `CACHE_ORDER` are merged into `AudioCache` behind one `Mutex` — no deadlock risk from nested lock acquisition.

16. **NEVER implement offline/disk cache.** This is a streaming YouTube Music client, not an offline jukebox. Serializing multi-MB in-memory buffers (WAV ~42MB, ALAC ~16MB) to disk on shutdown and reloading them on restart wastes I/O bandwidth and flash write endurance for zero user-facing benefit (the song will be re-downloaded faster than disk can read it). Every prior session that explored this direction reached the same conclusion. This file exists to prevent re-proposing it. (Rejected: F4)

## Rendering / UI

16. **Playlist title is cached.** `get_title()` uses `RefCell<Option<String>>`, invalidated on `push_song_list`, `toggle_shuffle`, `toggle_search`, `cycle_audio_quality`. Avoids `format!()` allocation every frame.

17. **Row numbers are pre-formatted.** `cached_row_numbers: Vec<String>` rebuilt when playlist changes. Avoids 60k allocs/sec at 60fps.

18. **Artist string is cached on song creation.** `ListSong::artists_string` computed once at creation time. Media controls use this instead of rebuilding from artists Vec every 100ms.

## Testing Guardrails

19. **`play_song_advances_download_status`** test verifies that `play_song` creates an active download entry. Would catch any regression where `download_song` bails without starting a download.

20. **Defensive WARN in `download_song`.** If a song's status is `Queued` but no active download exists, logs a warning and falls through instead of silently returning no-op.

21. **Stay on symphonia 0.5 until rodio supports 0.6.** symphonia 0.6.0 (2026-05-15) is a full rewrite; rodio 0.22.2 (latest) pins symphonia 0.5.5. A direct symphonia 0.6 bump would create a dual-version tree (0.6 direct + 0.5.5 via rodio), duplicating the entire codec stack in the binary. 0.6's additions (video/subtitle groundwork, metadata formats, SIMD) are irrelevant to a WAV/AAC/ALAC-only streaming player. (Phase 8 closed 2026-07-31)

## Search Feedback / Fallback

25. **Artist search runs the filtered (artists-only) query concurrently with a basic-search fallback and fuses them.** Filtered results win when non-empty (fallback aborted — the common path pays one round trip, never two; the aborted task leaks nothing, its request is dropped). Otherwise the fallback decides: a *successful* empty result is a genuine "nothing found"; only when **every** query errors (API failure or task join/panic) is the panel set to the error state. A failed filtered query falls back with its real cause `warn!`ed, never silently swallowed.

26. **Artist results are deduplicated on channel ID and (case/whitespace-insensitive) name, keeping first occurrence and search order.** The same artist returned under a duplicate channel — the "split discography" annoyance — shows once without an extra UI pass.

27. **Song and artist search drive their header state (`ListStatus`) to `Loading` → `Loaded`/`Error`.** The status-driven titles (`- loading` / `- N results` / `- no songs found` / `- Error received`) and spinner render from that state via `draw_loadable`; every search path must set it — a search that never sets status produces a silent title and turns a failed query into a misleading `Songs`/`Artists` header.

31. **Filtered search shelves tolerate leaked junk entries.** YouTube began flooding the filtered Artists/Songs shelves with unrelated content — videos, playlists, and non-matching songs — some of which lack `navigationEndpoint/browseEndpoint/browseId` (artists) or an album browseEndpoint in the subtitle (songs). The strict `TryFrom` parses failed the **whole query** on a single junk item, so every artist/song search errored out and fell back to a basic-search result that (in the new basic layout, a top-result card + itemSections with no Artists `musicShelfRenderer`) was empty — a famous band returned "nothing found" while its songs were right there in the shelf. Both `TryFrom<FilteredSearchMusicShelfContents>` impls (`Vec<SearchResultArtist>`, `Vec<SearchResultSong>`) now skip unparseable entries (`.filter_map(...ok())`), matching the already-lenient basic-search artists path: one junk item never aborts a search. Structurally-valid entries from unrelated artists are kept — the filter is structural, YTM decides relevance. Do not restore strictness. (Session 2026-08-13.)

## Stream-URL Fetch / Throttling

28. **ffmpeg's direct-URL fetch is shaped like yt-dlp's, not like a bare `Lavf/…` fetcher.** The resolved googlevideo URL is fetched by ffmpeg with a browser `-user_agent`, a `https://music.youtube.com/` `-referer`, and `-headers "Cookie: <cookie_header>"` when the app has one — the same header yt-dlp gets via `--add-header`. An anonymous fetch is a bot signal and is intermittently refused with `403 Forbidden (access denied)` even on a fresh URL. (Session 2026-08-13 — fixed the 10×-skip `debug330` window.)

29. **A CDN 403 on the direct-URL path is a throttle, never a skip.** It is neither a dead video nor stale cookies: it's the nsig/po_token throttling wave, and the same song fetched through the credential-carrying relay (or after a re-resolve) usually plays. On such a 403 the ffmpeg stderr handler marks the buffer `throttled` (also failed), the pipeline evicts the cached URL, sets `stream_url = None`, and runs the `'attempt` loop once more as a relay. The relay failure is exempt from retry (from_url_cache false), so a throttle wave halts via the transient-failure counter instead of silently draining the queue. (Session 2026-08-13.)

30. **The `n`-signed-URL throttle and the po_token.** The only definitive fix for the intermittent direct-URL 403 is a fresh po_token (`po_token.txt`); the app-side mitigation above keeps throttled songs playing without one. Do not "fix" the throttle by retrying the URL (it'll 403 again) or by flagging the song dead/auth. Consistently, `HTTP Error 403` is **not** an auth-error line (`is_auth_error_line`): a signed-in 403 is the CDN throttle, a guest has no cookies to be stale — so a relay/resolve-phase 403 is a *transient* failure that feeds the halt counter (DECISIONS.md:23), and a resolve-phase 403 falls through to the relay download instead of failing fast.
