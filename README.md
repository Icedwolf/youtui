# youtui

A suckless music TUI player for Linux. Search → queue → play. DBus notifications. That's it.

A heavily diverged fork of [nick42d/youtui](https://github.com/nick42d/youtui). The
playback/download pipeline was rebuilt around lossless streaming (yt-dlp → ffmpeg →
fragmented-MP4 ALAC), the OAuth/native-downloader paths were removed, and the feature
surface was narrowed to the minimum. If a feature isn't in this README, it's not coming.

This project is not supported or endorsed by Google.

## Features

- Search songs, artists, and playlists (songs only)
- Queue management, shuffle, filter, sort
- **Lossless streaming playback** — `bestaudio[ext=webm]` piped through ffmpeg to ALAC in a
  fragmented MP4, decoded incrementally as it arrives. ~16 MB/song in RAM, no disk cache.
  No pre-download buffering: the first frames stream in as soon as the URL resolves and the
  first chunk arrives (resolve + first-byte latency are the dominant cost, typically 2-5s on
  a cold start).
- M4A/AAC full-download fallback when ffmpeg is unavailable.
- In-memory audio cache (1 entry ≈ 32 MB total, configurable via `download_cache_size`).
- One download at a time — no parallel yt-dlp/ffmpeg processes competing for bandwidth.
- DBus MPRIS media keys, desktop notifications, configurable keybinds.

## Installing

### Build from source

Requires Rust (edition 2024, MSRV 1.91+) and ALSA development headers
(`libasound2-dev` Debian/Ubuntu, `alsa-lib-devel` Fedora, `alsa-lib` Arch).

```sh
cargo build --release
./target/release/youtui
```

### Dependencies (runtime)

- [yt-dlp](https://github.com/yt-dlp/yt-dlp) — resolves stream URLs and drives downloads.
- [ffmpeg](https://ffmpeg.org/) — **recommended**: transcodes WebM/Opus to lossless ALAC for
  true streaming. Without it, playback falls back to full-download M4A (slower start, needs
  the complete file before audio begins).
- A font that can render FontAwesome symbols for the UI icons.

## Running youtui

```sh
youtui
```

Options:

| Flag | Meaning |
|------|---------|
| `-d, --debug` | extra logging to the debug log file |
| `--disable-media-controls` | disable DBus MPRIS media keys |
| `-a, --auth-type <auth-type>` | override `auth_type` from the config (`Browser`, `Unauthenticated`) |
| `-g, --generate-completions <shell>` | print shell completions and exit |
| `search <query>`, `get-artist <id>`, `get-album <browse-id>`, … | headless API queries (see `youtui --help`) |

### Running youtui — config

Configuration lives in `~/.config/youtui/config.toml` (or `$XDG_CONFIG_HOME/youtui/config.toml`).
An example with all defaults is shipped in [`youtui/config/config.toml`](youtui/config/config.toml).

| Key | Default | Meaning |
|-----|---------|---------|
| `auth_type` | `"Browser"` | `"Browser"` (use your YouTube cookies) or `"Unauthenticated"` |
| `yt_dlp_command` | `"yt-dlp"` | the `yt-dlp` executable to invoke |
| `volume` | `50` | initial volume, applied at startup |
| `notifications_enabled` | `true` | desktop notifications for song changes and errors |
| `download_cache_size` | `1` | in-memory cached songs (1 ≈ 32 MB: one playing + one cached) |
| `keybinds` | see example | keybind overrides per context; also `mode_names` |

Unknown config keys are rejected. The format is stable for the current version.

### Browser Auth Setup Steps

`auth_type = "Browser"` authenticates API requests (search, song/artist pages, playback)
using your YouTube cookies. Two ways to provide them:

1. **Automatic** *(preferred)*: on startup youtui detects a Floorp/Firefox profile (falling
   back to Chromium) with YouTube cookies and exports them to `cookies_netscape.txt` via
   `--cookies-from-browser`, which also authenticates yt-dlp downloads.
2. **Manual `cookie.txt`**: copy the `Cookie` request header (from a `music.youtube.com`
   network request) into `~/.config/youtui/cookie.txt`.

If cookies are stale, refresh them (re-export or re-copy) and restart. Signed-in access is
required for age-restricted (`18+`) content, which yt-dlp would otherwise refuse.

### PO token information

If yt-dlp downloads always fail with a sign-in/`po_token` error, you can supply a PO Token by
saving it to `po_token.txt` in the config directory. For more information on PO Tokens and how
to obtain them, see [the yt-dlp PO Token guide](https://github.com/yt-dlp/yt-dlp/wiki/Po-Token-Guide).

## Architecture notes

- **Streaming path**: `resolve_url` (yt-dlp) → ffmpeg (`-f mp4 -movflags
  empty_moov+default_base_moof+frag_every_frame -c:a alac`) → non-seekable buffer →
  symphonia isomp4 incremental decode. `empty_moov` puts the moov atom (ALAC sample entry)
  in the first ~700 bytes, so decoding starts from a few KB.
- **No parallel downloads**: a semaphore (1 permit) is held from pipeline start until the
  background cache fill finishes, so a second ffmpeg can't spawn mid-song.
- **No prefetch before playback**: nothing downloads until the selected song is actually
  playing; then the next song is queued for download (1 ahead). The cache (default 1 entry)
  keeps the current song's buffer so a re-select/replay is instant.
- Subprocesses run with a bounded environment (`env_clear()` + allowlist) — children never
  inherit the parent's oversized `envp` (E2BIG-safe by construction).

See [`DECISIONS.md`](DECISIONS.md) for the full design rationale and [`BACKLOG.md`](BACKLOG.md)
for the changelog of this fork.

## Scope

In: music search (songs/artists/playlists), queue management, shuffle/filter, MPRIS, audio
cache. Out: podcasts, video clips, live concerts, OAuth/account management, audio quality
toggles, disk cache, gapless playback, lyrics, theming, mouse support, stats, offline mode,
Windows/macOS support. None of these will be added.

## Components

- **youtui/** — the TUI application itself.
- **ytmapi-rs/** — asynchronous YouTube Music API client (Tokio + Reqwest, rustls) used by the
  app and the CLI queries.
- **json-crawler/** — serde_json wrapper with better errors for large JSON crawling.

## Acknowledgements

Inspired by [ytermusic](https://github.com/ccgauche/ytermusic/) and cmus; the API client is
inspired by [ytmusicapi](https://github.com/sigma67/ytmusicapi/).