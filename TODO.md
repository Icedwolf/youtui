# Future TODOs

## Bug Investigation (Blocked)
- **54 integration tests fail** — YT API format drift (missing JSON keys like `gridRenderer/items`, `musicShelfRenderer/contents`). Needs API response reverse-engineering. Blocked on network captures.
- **Artist album pagination** — only first page returned. Needs `ParseFromContinuable` impl for `GetArtistAlbumsQuery`. Significant feature.

## Constraints (Do Not Remove)
- **Notifications** (`notify-rust` via D-Bus) — `NotificationController` + `notify_track_change()` + its call in `update_metadata()` are active, working code. The entire system was removed once by mistake (`faf21ef`). Any rename/refactor must preserve the notification call chain.
- **artists_string / track_no_string** — these cached fields on `ListSong` are critical for per-frame rendering perf. `get_field(Artists)` must return `Cow::Borrowed`, not allocate. Do not remove.
- **Mako only supports `file://` URLs** for notification icons. Remote URLs (YouTube thumbnails, http/https) are silently ignored to keep notifications instant.

## Dep Tracking
- Upstream removed `AudioQuality` from structures.rs — if they finalize removal, adapt our fork's re-exports.
