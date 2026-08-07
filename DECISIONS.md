# Product Design Decisions

Critical invariants and rationale. **Read before changing playback/download code.**

## Playback Flow (priority order)

1. **Single-song hot path is priority.** User searches → selects → plays. No playlist walking optimization. The download pipeline must optimize for this case.

2. **No parallel downloads.** Semaphore=1, one download at a time. Multiple yt-dlp processes compete for bandwidth and slow down the song the user actually selected.

3. **No prefetch before playback.** `prepare_playback_id` must NOT call `download_upcoming_from_id`. Prebuffer only fires AFTER `handle_playing` — the selected song gets 100% download bandwidth first.

4. **Start playing ASAP.** Every millisecond between select and first audio frame is valuable. Decoder init latency, ffmpeg latency, buffer thresholds — all fair targets.

5. **Prefetch scope starts AFTER song plays.** When `handle_playing` fires, `regenerate_downloads_for_current` queues next ±1 songs for download. This is the only prebuffer trigger.

## State Machine Invariants

6. **`start_buffering` must NOT set `download_status = Queued`.** Doing so causes `download_song` to bail early — no download starts, song stuck in Buffering forever. The prebuffer already excludes the current song via `get_cur_playing_id()`.

7. **After `play_song`, the download pipeline must be active.** `download_status != None` AND `active_downloads` must contain the song ID. Test: `play_song_advances_download_status`.

8. **Buffering state requires an active download.** `play_status == Buffering(id)` implies there MUST be a download task for that id. If not, song never transitions to Playing.

22. **Dead/deleted tracks are never auto-removed.** The `video unavailable` signal is not reliable (has falsely flagged valid songs). A dead song stays in the queue, marked `Failed`; playback advances past it. `session_dead_videos` (in-memory only, per-process) makes auto-advance and the end-of-queue wrap skip that song so the same refused video is never retried on its own; a dead-only queue stops cleanly. A wrongly-flagged song recovers on restart. Manual re-select still allowed (retries once, fails fast).

23. **The download-failure halt counts only transient/systemic failures.** `consecutive_download_failures` (halt + stop at `HALT_AFTER_CONSECUTIVE_FAILURES`) is incremented only for non-cancellation, non-dead, non-auth errors (rate limits, format loss, spawn errors). A dead video or auth/18+ failure is a definitive per-song/per-session condition, never a sign of a systemic download problem — it advances the queue with its own notification instead of feeding the halt. This keeps a run of deleted (or age-restricted) tracks from spuriously stopping the whole player.

## Audio Format Constraints (symphonia 0.5)

9. **No Opus support.** symphonia 0.5's `all-codecs` = {aac, adpcm, alac, flac, mp1, mp2, mp3, pcm, vorbis}. No `opus` feature exists in any 0.5.x version.

10. **WebM/Opus → ffmpeg pipe to fragmented MP4+ALAC.** Default path: `bestaudio[ext=webm]` piped through ffmpeg to lossless ALAC in a **fragmented MP4** container (`-f mp4 -movflags empty_moov+default_base_moof+frag_every_frame -c:a alac`). `frag_every_frame` is the ONLY flag that streams incrementally — `frag_keyframe`/`frag_duration` buffer the whole file until the pipe closes (measured). ALAC is lossless (no quality regression) at ~16MB/song vs 42MB WAV. `empty_moov` puts ftyp+moov (with the ALAC sample entry) in the first ~700 bytes, so the decoder inits from a partial buffer exactly like WAV did. (Changed from WAV 2026-08-06 — see item 12.)

11. **M4A/AAC works but cannot stream.** isomp4 format reader + aac codec enabled via rodio's `symphonia-isomp4`/`symphonia-aac`. `byte_len` MUST be actual total file size because isomp4 seeks to end for moov atom. Full download-then-decode only. This is the no-ffmpeg fallback.

12. **Fragmented MP4 streams from partial buffer via a NON-SEEKABLE source.** isomp4's seekable branch seeks past mdat during decode (`seek error during decode`) and the growing `SharedBuffer` can't serve those seeks. A non-seekable MediaSource (`NonSeekableReadSource`) forces isomp4's incremental branch (demuxer.rs:388-398). The ALAC streamed path uses `try_streaming_init_nonseekable`; the full-file fallbacks keep the seekable `ReadSeekSource`. `byte_len` is `None` for the streamed path.

## Cache Design

13. **`Arc<[u8]>` for BYTE_CACHE.** Cache hits avoid cloning large buffers — refcount bump ~8 bytes vs full Vec clone. `cache_get()` returns `Arc<[u8]>`.

14. **LRU eviction with 3 entries.** `AudioCache` struct with `HashMap<String, Arc<[u8]>>` + `VecDeque<String>` order behind single `Mutex`. Oldest entry evicted when full.

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
