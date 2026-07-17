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

## Audio Format Constraints (symphonia 0.5)

9. **No Opus support.** symphonia 0.5's `all-codecs` = {aac, adpcm, alac, flac, mp1, mp2, mp3, pcm, vorbis}. No `opus` feature exists in any 0.5.x version.

10. **WebM/Opus → ffmpeg pipe to WAV.** Default path: `bestaudio[ext=webm]` piped through ffmpeg to raw PCM in RIFF/WAV container. No MP3 compression step saves ~300ms per song.

11. **M4A/AAC works but cannot stream.** isomp4 format reader + aac codec enabled via rodio's `symphonia-all`. `byte_len` MUST be actual total file size because isomp4 seeks to end for moov atom. Full download-then-decode only.

12. **WAV streams from partial buffer.** RIFF/WAV header (~78 bytes) is at the start, so symphonia's WAV reader can probe and init from the first few KB. `byte_len` can be `None` for WAV.

## Cache Design

13. **`Arc<[u8]>` for BYTE_CACHE.** Cache hits avoid cloning 50MB+ buffers — refcount bump ~8 bytes vs full Vec clone. `cache_get()` returns `Arc<[u8]>`.

14. **LRU eviction with 3 entries.** `AudioCache` struct with `HashMap<String, Arc<[u8]>>` + `VecDeque<String>` order behind single `Mutex`. Oldest entry evicted when full.

15. **Single Mutex for cache.** `BYTE_CACHE` and `CACHE_ORDER` are merged into `AudioCache` behind one `Mutex` — no deadlock risk from nested lock acquisition.

16. **NEVER implement offline/disk cache.** This is a streaming YouTube Music client, not an offline jukebox. Serializing ~42MB WAV buffers to disk on shutdown and reloading them on restart wastes I/O bandwidth and flash write endurance for zero user-facing benefit (the song will be re-downloaded faster than disk can read 42MB). Every prior session that explored this direction reached the same conclusion. This file exists to prevent re-proposing it. (Rejected: F4)

## Rendering / UI

16. **Playlist title is cached.** `get_title()` uses `RefCell<Option<String>>`, invalidated on `push_song_list`, `toggle_shuffle`, `toggle_search`, `cycle_audio_quality`. Avoids `format!()` allocation every frame.

17. **Row numbers are pre-formatted.** `cached_row_numbers: Vec<String>` rebuilt when playlist changes. Avoids 60k allocs/sec at 60fps.

18. **Artist string is cached on song creation.** `ListSong::artists_string` computed once at creation time. Media controls use this instead of rebuilding from artists Vec every 100ms.

## Testing Guardrails

19. **`play_song_advances_download_status`** test verifies that `play_song` creates an active download entry. Would catch any regression where `download_song` bails without starting a download.

20. **Defensive WARN in `download_song`.** If a song's status is `Queued` but no active download exists, logs a warning and falls through instead of silently returning no-op.
