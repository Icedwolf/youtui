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
  fragmented MP4, decoded incrementally as it arrives. In-RAM footprint varies with song
  duration/source (recent 3–4 minute tracks measured about 45–90 MB of buffer), no disk cache.
  The selected song always gets the initial download bandwidth; after its fill finishes, one
  immediate successor may be filled while it plays. First frames stream in as soon as the URL
  resolves and the first chunk arrives (resolve + first-byte latency are the dominant cost,
  typically 2-5s on a cold start).
- M4A/AAC full-download fallback when ffmpeg is unavailable.
- In-memory audio cache (1 entry by default; each buffer's size varies with song
  duration/source, configurable via `download_cache_size`).
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

YouTube Music requires a GVS PO token for the `web_music` client and binds it to each *video
ID*. A static `po_token.txt` does not work. youtui delegates minting to yt-dlp's POT framework:
download the `bgutil-ytdlp-pot-provider-rs` release zip, extract its `yt_dlp_plugins/` directory
to `~/.config/youtui/yt-dlp-plugins/bgutil-ytdlp-pot-provider/`, and place the Linux
`bgutil-pot` release executable at `~/.config/youtui/bin/bgutil-pot` (`chmod 755`). The plugin
supplies the per-video token to yt-dlp; youtui has no token generator of its own. Node is still
required (if installed) — yt-dlp uses it as its JavaScript runtime to solve the nsig
player-JS challenge. Both assets are required for `web_music` playback.

**Required patch:** `bgutil-pot` keeps a per-video token cache on disk
(`~/.cache/bgutil-ytdlp-pot-provider/cache.json`) that YouTube invalidates *before* the token's
advertised expiry, so a replayed song is 403-refused (and the relay retry replays the same
stale token). In
`~/.config/youtui/yt-dlp-plugins/bgutil-ytdlp-pot-provider/yt_dlp_plugins/extractor/getpot_bgutil_cli.py`,
make `--bypass-cache` unconditional — replace

```python
if request.bypass_cache:
    command_args.append('--bypass-cache')
```

with

```python
command_args.append('--bypass-cache')
```

so a fresh token is minted on every resolve (minting is ~0.6s; the app's own 6-hour URL cache
already deduplicates resolves). Note that `--bypass-cache` only disables *reading* the disk
cache; `bgutil-pot` still *writes* `cache.json` after every mint, so the file keeps growing and
never going stale isn't a sign the patch is missing — it is never consulted again after the
patch.

## Architecture notes

- **Streaming path**: `resolve_url` (yt-dlp) → ffmpeg (`-f mp4 -movflags
  empty_moov+default_base_moof+frag_every_frame -c:a alac`) → non-seekable buffer →
  symphonia isomp4 incremental decode. `empty_moov` puts the moov atom (ALAC sample entry)
  in the first ~700 bytes, so decoding starts from a few KB.
- **No parallel downloads**: a semaphore (1 permit) is held from pipeline start until the
  background cache fill finishes, so a second ffmpeg can't spawn mid-song.
- **Bounded successor fill**: nothing competes with the selected song's initial fill. Once it
  completes, at most the immediately next song may fill while the current song plays; the cache
  (default 1 entry) keeps replay/resume data in memory.
- **CDN 403 → capped retry ladder**: a throttled direct-URL fetch retries via the credential
  relay; a throttled relay gets one more relay attempt (three capped attempts total: URL →
  relay → relay), so a song whose two fresh resolves were both refused by an intermittent CDN
  wave plays on a third instead of skipping first.
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
