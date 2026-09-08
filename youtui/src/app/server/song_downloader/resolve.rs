use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use crate::core::PoisonRecovery;
use tracing::{debug, warn};

const URL_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

pub(crate) static URL_CACHE: LazyLock<Mutex<HashMap<String, (String, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn url_cache_get(video_id: &str) -> Option<String> {
    let mut cache = URL_CACHE.lock().unwrap_or_warn();
    if let Some((url, ts)) = cache.get(video_id) {
        if ts.elapsed() < URL_CACHE_TTL {
            return Some(url.clone());
        }
        cache.remove(video_id);
    }
    None
}

fn url_cache_put(video_id: String, url: String) {
    let mut cache = URL_CACHE.lock().unwrap_or_warn();
    cache.insert(video_id, (url, Instant::now()));
}

/// Drop a cached stream URL. Called when the download fails after using a
/// cached URL (e.g. empty pipe / timeout): the URL may have died server-side,
/// and keeping it would make every retry reuse the same dead URL forever.
pub(crate) fn url_cache_remove(video_id: &str) {
    URL_CACHE.lock().unwrap_or_warn().remove(video_id);
}

/// Outcome of a URL pre-resolution. Distinct from a shared Option so the
/// caller can fail fast (skip the redundant download) and surface a
/// diagnosable message when yt-dlp reports an authentication/cookie error.
#[derive(Debug, PartialEq)]
pub enum ResolveOutcome {
    Url(String),
    /// Authentication/cookie failure; carries the offending yt-dlp stderr
    /// line for a user-facing notification.
    AuthError(String),
    /// Any other failure: cancellation, dead video, network, unavailable format.
    Failed,
}

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
/// framework mints the per-video GVS token and attaches it to the `web_music`
/// URL — the app carries no token code of its own.
#[derive(Clone, Debug)]
pub struct PotProvider {
    /// Passed to `--plugin-dirs`; contains
    /// `bgutil-ytdlp-pot-provider/yt_dlp_plugins/`.
    pub plugin_dir: PathBuf,
    /// The `bgutil-pot` executable, passed via
    /// `--extractor-args youtubepot-bgutilcli:cli_path=`.
    pub cli: PathBuf,
}

pub fn apply_ytdlp_auth_args(
    cmd: &mut tokio::process::Command,
    pot_provider: Option<&PotProvider>,
    cookie_header: Option<&str>,
    js_runtime: Option<&str>,
    video_id: &str,
) {
// Ignore yt-dlp's own config files (~/.config/yt-dlp/config etc). A
    // zero-byte `--cookies` entry there would hard-fail every download; youtui
    // owns the full argument list and must not inherit broken host config.
    cmd.arg("--ignore-config");
    let skip = "hls,translated_subs";
    // YouTube Music requires a per-video GVS PO token; the provider plugin +
    // `bgutil-pot` binary let yt-dlp mint and attach it itself. Force
    // `web_music` only when the provider is present — without a token that
    // client errors out (the default no-pot clients are 403-refused at fetch).
    if let Some(pp) = pot_provider {
        cmd.arg("--plugin-dirs").arg(&pp.plugin_dir);
        cmd.arg("--extractor-args").arg(format!(
            "youtubepot-bgutilcli:cli_path={}",
            pp.cli.display()
        ));
        cmd.arg("--extractor-args")
            .arg(format!("youtube:player_client=web_music;skip={skip}"));
    } else {
        cmd.arg("--extractor-args").arg(format!("youtube:skip={skip}"));
    }
    // Always use --add-header Cookie: instead of --cookies <file>.
    // --cookies triggers yt-dlp's "tv downgraded" player API which returns only
    // m3u8 HLS streams (no separate audio-only DASH formats), making bestaudio
    // unavailable. --add-header returns the normal DASH format set.
    if let Some(ch) = cookie_header {
        cmd.arg("--add-header");
        cmd.arg(format!("Cookie: {ch}"));
    }
    if let Some(jr) = js_runtime {
        cmd.arg("--js-runtimes");
        cmd.arg(jr);
    }
    cmd.arg(format!("https://music.youtube.com/watch?v={video_id}"));
}

pub async fn resolve_url(
    video_id: &str,
    yt_dlp_cmd: &str,
    pot_provider: Option<&PotProvider>,
    cookie_header: Option<&str>,
    js_runtime: Option<&str>,
    cancel_token: Option<&tokio_util::sync::CancellationToken>,
) -> ResolveOutcome {
    if let Some(url) = url_cache_get(video_id) {
        return ResolveOutcome::Url(url);
    }

    if let Some(ct) = cancel_token && ct.is_cancelled() {
        return ResolveOutcome::Failed;
    }

    let cmd_name = if yt_dlp_cmd.is_empty() { "yt-dlp" } else { yt_dlp_cmd };
    let mut cmd = tokio::process::Command::new(cmd_name);
    super::apply_child_env(&mut cmd);
    cmd.arg("--print").arg("url");
    cmd.arg("-f").arg("bestaudio[ext=webm]/bestaudio");
    cmd.arg("--no-warnings").arg("--no-playlist");
    apply_ytdlp_auth_args(&mut cmd, pot_provider, cookie_header, js_runtime, video_id);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // kill_on_drop + child owned by the `resolve` future: when cancellation wins
    // the select!, the future is dropped and the child is killed. Without this,
    // a cancelled resolve left an orphan yt-dlp running for its full duration.
    cmd.kill_on_drop(true);
    debug!(%video_id, ?js_runtime, "resolve: spawning yt-dlp with --print url -f bestaudio[ext=webm]/bestaudio");
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            warn!(%video_id, error = %e, "resolve: cannot spawn {cmd_name}");
            return ResolveOutcome::Failed;
        }
    };

    // Concurrent resolves are the churn class this session eliminated (held-key
    // shuffle toggles, rapid next-press). Log the spawn so a recurrence is
    // countable from logs (see the "yt-dlp spawned" line in the relay path).
    debug!(%video_id, "resolve: spawned {cmd_name}");

    let resolve = async {
        let output = child.wait_with_output().await.ok()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if let Some(line) = stderr.lines().find(|l| super::is_auth_error_line(l)) {
                return Some(ResolveOutcome::AuthError(line.trim().to_string()));
            }
            return Some(ResolveOutcome::Failed);
        }
        let url = String::from_utf8(output.stdout).ok()?;
        let url = url.trim().to_string();
        if url.is_empty() {
            return Some(ResolveOutcome::Failed);
        }
        Some(ResolveOutcome::Url(url))
    };

    let outcome = if let Some(ct) = cancel_token {
        tokio::select! {
            biased;
            _ = ct.cancelled() => {
                // Cancellation aborts the resolve (and kill_on_drop reaps the
                // child). Log it so a burst-orphan or scope-change storm is
                // distinguishable from a genuine failure.
                debug!(%video_id, "resolve: cancelled");
                ResolveOutcome::Failed
            }
            outcome = resolve => outcome.unwrap_or(ResolveOutcome::Failed),
        }
    } else {
        resolve.await.unwrap_or(ResolveOutcome::Failed)
    };

    if let ResolveOutcome::Url(ref url) = outcome {
        url_cache_put(video_id.to_string(), url.clone());
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::{
        PotProvider, ResolveOutcome, apply_ytdlp_auth_args, is_nonempty_cookie_file, resolve_url,
        url_cache_get, url_cache_put, url_cache_remove,
    };
    use std::time::Duration;

    fn fake_ytdlp_script(pidfile: &std::path::Path, script: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(script, format!("#!/bin/sh\necho $$ > '{}'\nsleep 30\n", pidfile.display()))
            .expect("write fake yt-dlp script");
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake yt-dlp script");
    }

    fn fake_fail_script(script: &std::path::Path, stderr_line: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            script,
            format!(
                "#!/bin/sh\ncat >&2 <<'YOUTUIEOF'\n{stderr_line}\nYOUTUIEOF\nexit 1\n"
            ),
        )
        .expect("write failing fake yt-dlp script");
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake yt-dlp script");
    }

    fn unique_tmp(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{name}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn resolve_url_cancelled_kills_child() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let pidfile = unique_tmp("resolve_pid");
            let script = unique_tmp("fake_ytdlp");
            fake_ytdlp_script(&pidfile, &script);

            let ct = tokio_util::sync::CancellationToken::new();
            let cancel = ct.clone();
            let canceller = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                cancel.cancel();
            });

            let start = std::time::Instant::now();
            let url = resolve_url(
                &format!("fake-video-{}", std::process::id()),
                script.to_str().expect("script path"),
                None,
                None,
                None,
                Some(&ct),
            )
            .await;
            let elapsed = start.elapsed();
            let _ = canceller.await;

            assert!(matches!(url, ResolveOutcome::Failed), "cancelled resolve must be Failed");
            assert!(
                elapsed < Duration::from_secs(5),
                "cancel must return promptly, took {elapsed:?}"
            );

            let pid_str = std::fs::read_to_string(&pidfile).unwrap_or_default();
            let pid: i32 = pid_str.trim().parse().unwrap_or(-1);
            assert!(pid > 0, "pidfile should contain a pid, got {pid_str:?}");

            let mut dead = false;
            for _ in 0..40 {
                let alive = std::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .stderr(std::process::Stdio::null())
                    .status()
                    .expect("kill -0");
                if !alive.success() {
                    dead = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(dead, "cancelled resolve child {pid} must be dead");

            std::fs::remove_file(&script).ok();
            std::fs::remove_file(&pidfile).ok();
        });
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
            None,
            "dQw4w9WgXcQ",
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
            None,
            "dQw4w9WgXcQ",
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
            None,
            "dQw4w9WgXcQ",
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
    fn pot_provider_configures_plugin_cli_and_web_music() {
        use tokio::process::Command;
        let provider = PotProvider {
            plugin_dir: std::path::PathBuf::from("/plugins"),
            cli: std::path::PathBuf::from("/bin/bgutil-pot"),
        };

        let mut cmd = Command::new("yt-dlp");
        apply_ytdlp_auth_args(
            &mut cmd,
            Some(&provider),
            None,
            None,
            "web-music-vid",
        );
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2).any(|w| w[0] == "--plugin-dirs" && w[1] == "/plugins"),
            "must configure the plugin directory: {args:?}"
        );
        assert!(
            args.windows(2).any(|w| {
                w[0] == "--extractor-args"
                    && w[1] == "youtubepot-bgutilcli:cli_path=/bin/bgutil-pot"
            }),
            "must configure the bgutil CLI: {args:?}"
        );
        assert!(
            args.windows(2).any(|w| {
                w[0] == "--extractor-args"
                    && w[1].contains("player_client=web_music")
                    && !w[1].contains("po_token=")
            }),
            "yt-dlp must mint the token itself for web_music: {args:?}"
        );
    }

    #[test]
    fn no_pot_provider_skips_web_music() {
        use tokio::process::Command;
        // Without both installed provider assets, do not force web_music,
        // which requires a GVS token.
        let mut cmd = Command::new("yt-dlp");
        apply_ytdlp_auth_args(&mut cmd, None, None, None, "dQw4w9WgXcQ");
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
    fn resolve_auth_error_reported() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let script = unique_tmp("fake_ytdlp_auth");
            fake_fail_script(
                &script,
                "ERROR: [youtube] X: Sign in to confirm you're not a bot",
            );
            let outcome = resolve_url(
                "auth-video",
                script.to_str().expect("script path"),
                None,
                None,
                None,
                None,
            )
            .await;
            let ResolveOutcome::AuthError(line) = outcome else {
                panic!("expected AuthError, got {outcome:?}");
            };
            assert!(
                line.contains("not a bot"),
                "must carry the offending stderr line, got: {line}"
            );
            std::fs::remove_file(&script).ok();
        });
    }

    #[test]
    fn resolve_non_auth_failure_is_failed() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let script = unique_tmp("youtui_resolve_failed");
            fake_fail_script(
                &script,
                "ERROR: [youtube] X: HTTP Error 429: Too Many Requests",
            );
            let outcome = resolve_url(
                "fail-video",
                script.to_str().expect("script path"),
                None,
                None,
                None,
                None,
            )
            .await;
            assert!(
                matches!(outcome, ResolveOutcome::Failed),
                "non-auth failure must be Failed, got {outcome:?}"
            );
            std::fs::remove_file(&script).ok();
        });
    }

    #[test]
    fn resolve_success_returns_url() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let script = unique_tmp("youtui_resolve_ok");
            std::fs::write(
                &script,
                "#!/bin/sh\necho 'https://example.com/stream'\nexit 0\n",
            )
            .expect("write succeeding fake yt-dlp script");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake yt-dlp script");
            let outcome = resolve_url(
                "ok-video",
                script.to_str().expect("script path"),
                None,
                None,
                None,
                None,
            )
            .await;
            assert_eq!(
                outcome,
                ResolveOutcome::Url("https://example.com/stream".to_string())
            );
            std::fs::remove_file(&script).ok();
        });
    }

    #[test]
    fn url_cache_remove_evicts_entry() {
        // Regression for the empty-pipe loop: a dead cached stream URL must be
        // evictable so the next retry re-resolves instead of reusing it.
        url_cache_put("evict-me".to_string(), "https://example.com/stream".to_string());
        assert!(url_cache_get("evict-me").is_some(), "precondition: entry cached");
        url_cache_remove("evict-me");
        assert_eq!(
            url_cache_get("evict-me"),
            None,
            "entry must be gone after eviction"
        );
    }
}
