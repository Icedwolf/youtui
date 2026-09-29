use std::path::Path;

/// True only if `path` points to an existing, non-empty regular file. Passing a
/// zero-byte (or absent) cookie file to yt-dlp aborts the whole download, so we
/// treat such a file as "no auth" rather than as a hard failure.
pub(crate) fn is_nonempty_cookie_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(md) => md.is_file() && md.len() > 0,
        Err(_) => false,
    }
}

/// True when a Netscape cookies file actually carries a signed-in YouTube
/// session (one of `AUTH_COOKIE_NAMES` in its name column). A non-empty guest
/// export (`VISITOR_INFO1_LIVE`, `__Secure-YEC`, ...) adds nothing over an
/// anonymous run — and must not shadow a manual auth header, mirroring
/// `server::resolve_cookie_header`'s auth-aware decision for the API client.
pub(crate) fn file_has_auth_cookie(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let auth = crate::app::server::AUTH_COOKIE_NAMES;
    for line in content.lines() {
        // Netscape: domain<TAB>flag<TAB>path<TAB>secure<TAB>expiry<TAB>name<TAB>value
        if let Some(name) = line.split('\t').nth(5)
            && auth.contains(&name)
        {
            return true;
        }
    }
    false
}

/// Apply the auth/client argument set a yt-dlp streaming child needs:
/// `--ignore-config` (never inherit broken host config), yt-dlp's default
/// (token-free) playback clients, and the `--add-header "Cookie: …"` header.
/// yt-dlp itself tracks the moving token-free client target, so youtui pins no
/// client and needs no POT provider (the `web_music` client-fallback slice was
/// removed — DECISIONS.md:46; it never fired in observed sessions).
pub fn apply_ytdlp_auth_args(
    cmd: &mut tokio::process::Command,
    cookie_header: Option<&str>,
    video_id: &str,
) {
    // Ignore yt-dlp's own config files (~/.config/yt-dlp/config etc). A
    // zero-byte `--cookies` entry there would hard-fail every download; youtui
    // owns the full argument list and must not inherit broken host config.
    cmd.arg("--ignore-config");
    cmd.arg("--extractor-args")
        .arg("youtube:skip=hls,translated_subs");
    // Always use --add-header Cookie: instead of --cookies <file>. The
    // historical --cookies "tv downgraded" m3u8-only regression (yt-dlp reading
    // a signed-in Netscape file probed the downgraded player API) is fixed on
    // the 2026-08-30 nightly (itag-251/proto=https re-test), but a --cookies
    // run still resolves ~2x slower (signed-in client probing, 6.5s vs 2.9s),
    // so the header stays for the fast single-song hot path.
    if let Some(ch) = cookie_header {
        cmd.arg("--add-header");
        cmd.arg(format!("Cookie: {ch}"));
    }
    cmd.arg(format!("https://music.youtube.com/watch?v={video_id}"));
}

#[cfg(test)]
mod tests {
    use super::{apply_ytdlp_auth_args, is_nonempty_cookie_file};

    fn unique_tmp(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{name}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn empty_or_missing_cookie_file_omits_cookies_arg() {
        let empty = unique_tmp("empty_cookies");
        let full = unique_tmp("full_cookies");
        let missing = unique_tmp("missing_cookies");
        std::fs::write(&empty, b"").expect("write empty cookie file");
        std::fs::write(&full, b"# Netscape HTTP Cookie File\n").expect("write cookie file");

        assert!(
            !is_nonempty_cookie_file(&empty),
            "empty file must be rejected"
        );
        assert!(
            !is_nonempty_cookie_file(&missing),
            "missing file must be rejected"
        );
        assert!(is_nonempty_cookie_file(&full), "non-empty file must pass");

        std::fs::remove_file(&empty).ok();
        std::fs::remove_file(&full).ok();
    }

    #[test]
    fn guest_cookie_file_falls_back_to_header() {
        use tokio::process::Command;
        // A non-empty but guest-only export (no SID family) must NOT produce
        // `--cookies`: it shadows a manual auth header for zero benefit and
        // would silently drop 18+ playback. The header must win instead.
        let guest = unique_tmp("guest_cookies");
        std::fs::write(
            &guest,
            ".youtube.com\tTRUE\t/\tTRUE\t1801487197\tVISITOR_INFO1_LIVE\togWC2WQgc2A\n",
        )
        .expect("write guest cookie file");

        let mut cmd = Command::new("yt-dlp");
        apply_ytdlp_auth_args(&mut cmd, Some("SID=manual-signed-in; x=y"), "dQw4w9WgXcQ");
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.windows(2).any(|w| w[0] == "--cookies"),
            "guest-only file must not be passed as --cookies: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--add-header" && w[1].contains("SID=manual-signed-in")),
            "manual auth header must be used when the file is guest-only: {args:?}"
        );
        std::fs::remove_file(&guest).ok();
    }

    #[test]
    fn signed_in_cookie_file_uses_header() {
        use tokio::process::Command;
        // A file carrying a signed-in session: always use --add-header Cookie:
        // instead of --cookies, because --cookies triggers yt-dlp's "tv
        // downgraded" player API which returns only m3u8 HLS streams (no DASH
        // audio-only formats), making bestaudio unavailable.
        let signed = unique_tmp("signed_cookies");
        std::fs::write(
            &signed,
            ".youtube.com\tTRUE\t/\tTRUE\t1820496362\t__Secure-1PSID\tabc.signed\n",
        )
        .expect("write signed-in cookie file");

        let mut cmd = Command::new("yt-dlp");
        apply_ytdlp_auth_args(&mut cmd, Some("SID=manual-header"), "dQw4w9WgXcQ");
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.windows(2).any(|w| w[0] == "--cookies"),
            "must not use --cookies (triggers tv downgraded API): {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--add-header" && w[1] == "Cookie: SID=manual-header"),
            "must use --add-header Cookie: for auth: {args:?}"
        );
        std::fs::remove_file(&signed).ok();
    }

    #[test]
    fn ytdlp_args_always_ignore_global_config() {
        use tokio::process::Command;
        let mut cmd = Command::new("yt-dlp");
        apply_ytdlp_auth_args(&mut cmd, Some("cookie=header"), "dQw4w9WgXcQ");
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // A stale/empty --cookies line in yt-dlp's own ~/.config must never
        // cascade into our invocation.
        assert!(
            args.windows(2).any(|w| w[0] == "--ignore-config"),
            "expected --ignore-config in args: {args:?}"
        );
    }
}
