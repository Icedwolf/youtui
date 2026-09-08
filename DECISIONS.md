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

10. **WebM/Opus → ffmpeg pipe to fragmented MP4+ALAC.** Default path: `bestaudio[ext=webm]` piped through ffmpeg to lossless ALAC in a **fragmented MP4** container (`-f mp4 -movflags empty_moov+default_base_moof+frag_every_frame -c:a alac`). `frag_every_frame` is the ONLY flag that streams incrementally — `frag_keyframe`/`frag_duration` buffer the whole file until the pipe closes (measured). ALAC is lossless (no quality regression); its temporary buffer varies with duration/source, with recent 3–4 minute tracks measuring 45–90MB. `empty_moov` puts ftyp+moov (with the ALAC sample entry) in the first ~700 bytes, so the decoder inits from a partial buffer exactly like WAV did. (Changed from WAV 2026-08-06 — see item 12.)

11. **M4A/AAC works but cannot stream.** isomp4 format reader + aac codec enabled via rodio's `symphonia-isomp4`/`symphonia-aac`. `byte_len` MUST be actual total file size because isomp4 seeks to end for moov atom. Full download-then-decode only. This is the no-ffmpeg fallback.

12. **Fragmented MP4 streams from partial buffer via a NON-SEEKABLE source.** isomp4's seekable branch seeks past mdat during decode (`seek error during decode`) and the growing `SharedBuffer` can't serve those seeks. A non-seekable MediaSource (`NonSeekableReadSource`) forces isomp4's incremental branch (demuxer.rs:388-398). The ALAC streamed path uses `try_streaming_init_nonseekable`; the full-file fallbacks keep the seekable `ReadSeekSource`. `byte_len` is `None` for the streamed path.

## Cache Design

13. **`Arc<[u8]>` for BYTE_CACHE.** Cache hits avoid cloning large buffers — refcount bump ~8 bytes vs full Vec clone. `cache_get()` returns `Arc<[u8]>`.

14. **LRU eviction, size configurable, default 1.** `AudioCache` struct with `HashMap<String, Arc<[u8]>>` + `VecDeque<String>` order behind single `Mutex`. Oldest entry evicted when full. Capacity from `config.download_cache_size` (`CACHE_MAX_ENTRIES`), default **1** — one ALAC buffer playing plus one cached entry; memory is duration/source-dependent (recent 3–4 minute tracks measured 45–90MB each). Do not raise the default; multi-entry caches are a user opt-in.

15. **Single Mutex for cache.** `BYTE_CACHE` and `CACHE_ORDER` are merged into `AudioCache` behind one `Mutex` — no deadlock risk from nested lock acquisition.

16. **NEVER implement offline/disk cache.** This is a streaming YouTube Music client, not an offline jukebox. Serializing multi-MB in-memory buffers to disk on shutdown and reloading them on restart wastes I/O bandwidth and flash write endurance for zero user-facing benefit (the song will be re-downloaded faster than disk can read it). Every prior session that explored this direction reached the same conclusion. This file exists to prevent re-proposing it. (Rejected: F4)

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

31. **Every list parse tolerates leaked junk entries — no single item aborts a search.** YouTube floods search shelves and artist pages with unrelated content (videos, playlists, non-matching songs), some of which lack the `navigationEndpoint/browseEndpoint/browseId` (artists), an album browseEndpoint in the subtitle (songs), or a `musicResponsiveListItemRenderer` (shelves) / `musicTwoRowItemRenderer` (artist carousels + albums grid). A strict parse failed the **whole query** on one such item. All list parses now skip unparseable entries (`.filter_map(...ok())`, `...transpose()` on the card/album-track paths), so one junk item never aborts a search or artist fetch. Fixed: basic search Albums/FeaturedPlaylists/Songs, filtered search Albums/Profiles/Episodes/Podcasts/Playlists/CommunityPlaylists/FeaturedPlaylists, artist songs shelf, Singles/Albums carousels, and the artist albums grid. Already tolerant (verified, unchanged): basic TopResult/Artists, the top-results card, filtered Artists/Songs/Videos, and album tracks. Structurally-valid entries from unrelated artists are kept — the filter is structural, YTM decides relevance. Do not restore strictness. (Session 2026-08-13; sweep extended to all 14 abort sites 2026-08-14.)

## Stream-URL Fetch / Throttling

28. **ffmpeg's direct-URL fetch is shaped like yt-dlp's, not like a bare `Lavf/…` fetcher.** The resolved googlevideo URL is fetched by ffmpeg with a browser `-user_agent`, a `https://music.youtube.com/` `-referer`, and `-headers "Cookie: <cookie_header>"` when the app has one — the same header yt-dlp gets via `--add-header`. An anonymous fetch is a bot signal and is intermittently refused with `403 Forbidden (access denied)` even on a fresh URL. (Session 2026-08-13 — fixed the 10×-skip `debug330` window.)

29. **A CDN 403 on the direct-URL path is a throttle, never a skip.** It is neither a dead video nor stale cookies: it's the nsig/GVS-token throttling wave, and the same song fetched through the credential-carrying relay (or after a re-resolve) usually plays. On such a 403 the ffmpeg stderr handler marks the buffer `throttled` (also failed), the pipeline evicts the cached URL, sets `stream_url = None`, and runs the `'attempt` loop once more as a relay. A throttled relay is itself retried **exactly once** (`relay_throttle_retry`, 403 classified on the yt-dlp stderr handler; three capped attempts total: direct URL → relay → relay), because the CDN refuses two consecutive fresh resolves occasionally and a third (fresh mint, possibly different edge) plays — the observed "failed, then plays on re-select" case. Two throttled relays are definitive: the song bails as a generic transient failure and the waveform halts via the transient-failure counter instead of silently draining the queue or looping forever. Dead/auth failures never throttle, so a truly unplayable song still fails fast on the first attempt. (Session 2026-08-13; relay cap amendment 2026-09-08.)

30. **YouTube Music requires a per-video GVS PO token on `web_music`.** Static tokens are obsolete: yt-dlp rejects a bare value (it needs `CLIENT.CONTEXT+TOKEN`), and a token bound to another video is CDN-403'd even when attached. youtui delegates token generation to yt-dlp's POT framework: the externally installed `bgutil-ytdlp-pot-provider-rs` plugin invokes the externally installed `bgutil-pot` binary and attaches the token to the WEB_REMIX itag-251 URL. nsig solving alone does **not** fix this — a no-pot WEB/ANDROID_VR URL 403s. (Session 2026-09-04.)
31. **The plugin and binary are a hard pair.** Startup enables `web_music` only when `~/.config/youtui/yt-dlp-plugins/bgutil-ytdlp-pot-provider/yt_dlp_plugins/` and executable `~/.config/youtui/bin/bgutil-pot` both exist. It then passes `--plugin-dirs`, `youtubepot-bgutilcli:cli_path=…`, and `youtube:player_client=web_music;skip=…`. A missing binary makes yt-dlp skip all `web_music` formats, so youtui intentionally does not enable the client for an incomplete installation; there is no hollow Node or static-token fallback.
32. **Do not "fix" the throttle by retrying the URL or flagging the song dead/auth.** HTTP Error 403 is **not** an auth-error line (`is_auth_error_line`): a signed-in 403 is the CDN throttle, a guest has no cookies to be stale — so a relay/resolve-phase 403 is a *transient* failure that feeds the halt counter (DECISIONS.md:23), and a resolve-phase 403 falls through to the relay download instead of failing fast.

33. **`bgutil-pot`'s on-disk token cache is poison — always bypass it.** `bgutil-pot` caches minted tokens in `~/.cache/bgutil-ytdlp-pot-provider/cache.json` (script mode hardcodes `Settings::default()`; there is no TTL knob). YouTube invalidates GVS tokens *before* their advertised ~6h expiry, so a cached token for a replayed song is 403-refused — on **both** the direct fetch and the relay (which replays the same token), turning "some songs skip" into a deterministic per-song failure. The fix is a one-line patch to the installed plugin (`getpot_bgutil_cli.py`): make `--bypass-cache` unconditional so every resolve mints fresh (~0.6s). yt-dlp hardcodes `bypass_cache=False` and never sets it True, so its own retry path never helps; the plugin patch is the only lever. **Verified against the provider source** (`jim60105/bgutil-ytdlp-pot-provider-rs`): `--bypass-cache` only skips the *read* (`SessionManager::generate_pot_token` gates the cached-token hit on `!bypass_cache`); the disk cache is still *written* unconditionally after every mint, so a growing `cache.json` is harmless write-only garbage that is never consulted again — its presence is not a sign the patch failed, and it does not need deleting. (Session 2026-09-08.)

34. **ffmpeg's HTTP options must precede `-i`, and the cookie header must be domain-filtered.** The direct-URL fetch is shaped with `-user_agent`/`-referer`/`-headers "Cookie: …"`. Placed **after** `-i` (the original code) ffmpeg treats them as output options and silently drops them — the fetch actually went out anonymous (`Lavf/…`), a bot signal. Moved before `-i`, and `extract_cookie_header_str` now keeps only `*.youtube.com`/`*.google.com` cookies and dedups by name: an all-domain browser export carried unrelated ad cookies and a 9 KB header that exceeded ffmpeg's request-buffer limit (`overlong headers`, client-side EINVAL), while duplicate names (`SID` on both `.youtube.com` and `.google.com`) confuse the CDN's session binding. (Session 2026-09-08.)
