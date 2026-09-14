use std::path::{Path, PathBuf};

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

/// Locations of the yt-dlp POT provider plugin and the `bgutil-pot` binary.
/// When both are present (detected once at startup), yt-dlp's own PO-token
/// framework can mint a per-video GVS token and attach it to a `web_music`
/// URL — used only by the client-fallback retry, never the primary attempt.
/// The app carries no token code of its own.
#[derive(Clone, Debug)]
pub struct PotProvider {
    /// Passed to `--plugin-dirs`; contains
    /// `bgutil-ytdlp-pot-provider/yt_dlp_plugins/`.
    pub plugin_dir: PathBuf,
    /// The `bgutil-pot` executable, passed via
    /// `--extractor-args youtubepot-bgutilcli:cli_path=`.
    pub cli: PathBuf,
}

/// Apply the auth/client argument set a yt-dlp streaming child needs.
///
/// The **primary** attempt (`web_music_fallback = false`) uses yt-dlp's default
/// playback clients (token-free, ~2.5s to first byte on the 2026-08-30 nightly)
/// and ignores the POT provider entirely — the provider is only wired in on the
/// **fallback** retry, when the default clients report no playable formats.
/// yt-dlp itself tracks the (moving) token-free client target (android_vr was
/// removed in 2026-08-19/#17461; web_embedded refuses embedded-only content),
/// so the fast common path needs no client pinning at all.
pub fn apply_ytdlp_auth_args(
    cmd: &mut tokio::process::Command,
    pot_provider: Option<&PotProvider>,
    cookie_header: Option<&str>,
    video_id: &str,
    web_music_fallback: bool,
) {
    // Ignore yt-dlp's own config files (~/.config/yt-dlp/config etc). A
    // zero-byte `--cookies` entry there would hard-fail every download; youtui
    // owns the full argument list and must not inherit broken host config.
    cmd.arg("--ignore-config");
    let skip = "hls,translated_subs";
    if web_music_fallback {
        // Client-fallback attempt: the default clients had no playable formats
        // (SABR experiment / abandoned client), so retry through `web_music`
        // with the GVS PO-token provider — slow (~9.4s first byte) but the most
        // reliably playable client. Only meaningful with a provider present;
        // without one there is nothing to escape to, so degrade to the default
        // clients (the retry guard never requests this combination).
        if let Some(pp) = pot_provider {
            cmd.arg("--plugin-dirs").arg(&pp.plugin_dir);
            cmd.arg("--extractor-args").arg(format!(
                "youtubepot-bgutilcli:cli_path={}",
                pp.cli.display()
            ));
            cmd.arg("--extractor-args")
                .arg(format!("youtube:player_client=web_music;skip={skip}"));
        } else {
            cmd.arg("--extractor-args")
                .arg(format!("youtube:skip={skip}"));
        }
    } else {
        cmd.arg("--extractor-args").arg(format!("youtube:skip={skip}"));
    }
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
    use super::{PotProvider, apply_ytdlp_auth_args, is_nonempty_cookie_file};

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

        assert!(!is_nonempty_cookie_file(&empty), "empty file must be rejected");
        assert!(!is_nonempty_cookie_file(&missing), "missing file must be rejected");
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
        apply_ytdlp_auth_args(
            &mut cmd,
            None,
            Some("SID=manual-signed-in; x=y"),
            "dQw4w9WgXcQ",
            false,
        );
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
        apply_ytdlp_auth_args(
            &mut cmd,
            None,
            Some("SID=manual-header"),
            "dQw4w9WgXcQ",
            false,
        );
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
        apply_ytdlp_auth_args(
            &mut cmd,
            None,
            Some("cookie=header"),
            "dQw4w9WgXcQ",
            false,
        );
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

    #[test]
    fn pot_provider_keeps_primary_on_default_clients() {
        use tokio::process::Command;
        // The provider's presence must NOT force web_music on the primary
        // attempt: the common path stays on yt-dlp's default (token-free)
        // clients — measured ~2.5s to first byte vs ~9.4s for a web_music
        // GVS mint. The provider is only wired in by the fallback retry.
        let provider = PotProvider {
            plugin_dir: std::path::PathBuf::from("/plugins"),
            cli: std::path::PathBuf::from("/bin/bgutil-pot"),
        };

        let mut cmd = Command::new("yt-dlp");
        apply_ytdlp_auth_args(
            &mut cmd,
            Some(&provider),
            None,
            "web-music-vid",
            false,
        );
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.windows(2).any(|w| w[0] == "--plugin-dirs"),
            "primary must not pass --plugin-dirs (stays token-free): {args:?}"
        );
        assert!(
            !args.windows(2).any(|w| {
                w[0] == "--extractor-args" && w[1].contains("player_client=web_music")
            }),
            "primary must not force web_music: {args:?}"
        );
        assert!(
            args.windows(2).any(|w| {
                w[0] == "--extractor-args" && w[1] == "youtube:skip=hls,translated_subs"
            }),
            "primary must use the default clients (skip-only): {args:?}"
        );
    }

    #[test]
    fn no_pot_provider_skips_web_music() {
        use tokio::process::Command;
        // Without both installed provider assets, do not force web_music,
        // which requires a GVS token.
        let mut cmd = Command::new("yt-dlp");
        apply_ytdlp_auth_args(&mut cmd, None, None, "dQw4w9WgXcQ", false);
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let extractor = args
            .windows(2)
            .find(|w| w[0] == "--extractor-args")
            .map(|w| w[1].clone())
            .expect("extractor args present");
        assert!(
            !extractor.contains("player_client=web_music"),
            "must NOT force web_music without a token: {extractor}"
        );
        assert!(
            !extractor.contains("po_token="),
            "must NOT pass a PO token without a provider: {extractor}"
        );
    }

    #[test]
    fn web_music_fallback_configures_plugin_cli_and_web_music() {
        use tokio::process::Command;
        // The client-fallback retry must wire up the POT provider and force
        // `web_music`: the default clients had no playable formats, so yt-dlp
        // mints a fresh GVS token for the most reliably playable client.
        // Only when a provider is present (the pair is the token's source).
        let provider = PotProvider {
            plugin_dir: std::path::PathBuf::from("/plugins"),
            cli: std::path::PathBuf::from("/bin/bgutil-pot"),
        };

        let mut cmd = Command::new("yt-dlp");
        apply_ytdlp_auth_args(
            &mut cmd,
            Some(&provider),
            Some("SID=manual-signed-in"),
            "fb-vid",
            true,
        );
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let extractors: Vec<&String> = args
            .windows(2)
            .filter(|w| w[0] == "--extractor-args")
            .map(|w| &w[1])
            .collect();
        assert!(
            args.windows(2).any(|w| w[0] == "--plugin-dirs" && w[1] == "/plugins"),
            "client fallback must configure the plugin directory: {args:?}"
        );
        assert!(
            extractors.iter().any(|e| e.contains("player_client=web_music")),
            "client fallback must force web_music: {args:?}"
        );
        assert!(
            extractors
                .iter()
                .any(|e| e.contains("youtubepot-bgutilcli:cli_path=")),
            "client fallback must configure the bgutil CLI (token source): {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--add-header" && w[1].contains("SID=manual-signed-in")),
            "client fallback must keep the auth header: {args:?}"
        );
    }
}
