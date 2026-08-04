use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use crate::core::PoisonRecovery;

const URL_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

pub(crate) static URL_CACHE: LazyLock<Mutex<HashMap<String, (String, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn url_cache_get(video_id: &str) -> Option<String> {
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

pub fn apply_ytdlp_auth_args(
    cmd: &mut tokio::process::Command,
    po_token: Option<&str>,
    cookie_path: Option<&Path>,
    cookie_header: Option<&str>,
    js_runtime: Option<&str>,
    video_id: &str,
) {
    // Ignore yt-dlp's own config files (~/.config/yt-dlp/config etc). A
    // zero-byte --cookies entry there hard-fails every download; youtui owns
    // the full argument list and must not inherit broken host config.
    cmd.arg("--ignore-config");
    let skip = "hls,translated_subs";
    let extractor_args = match po_token {
        Some(pt) => format!("youtube:po_token={pt};skip={skip}"),
        None => format!("youtube:skip={skip}"),
    };
    cmd.arg("--extractor-args");
    cmd.arg(&extractor_args);
    // Only pass --cookies when the file actually exists and has content. An
    // empty or missing cookie file makes yt-dlp hard-fail every download, so
    // never let a bad export disable playback — fall back to header/unauth.
    if let Some(cp) = cookie_path.filter(|cp| is_nonempty_cookie_file(cp)) {
        cmd.arg("--cookies");
        cmd.arg(cp);
    } else if let Some(ch) = cookie_header {
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
    po_token: Option<&str>,
    cookie_path: Option<&Path>,
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
    cmd.arg("--print").arg("url");
    cmd.arg("-f").arg("bestaudio[ext=webm]/bestaudio");
    cmd.arg("--no-warnings").arg("--no-playlist");
    apply_ytdlp_auth_args(&mut cmd, po_token, cookie_path, cookie_header, js_runtime, video_id);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // kill_on_drop + child owned by the `resolve` future: when cancellation wins
    // the select!, the future is dropped and the child is killed. Without this,
    // a cancelled resolve left an orphan yt-dlp running for its full duration.
    cmd.kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return ResolveOutcome::Failed,
    };

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
            _ = ct.cancelled() => ResolveOutcome::Failed,
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
        ResolveOutcome, apply_ytdlp_auth_args, is_nonempty_cookie_file, resolve_url, url_cache_get,
        url_cache_put, url_cache_remove,
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
    fn ytdlp_args_always_ignore_global_config() {
        use tokio::process::Command;
        let mut cmd = Command::new("yt-dlp");
        apply_ytdlp_auth_args(
            &mut cmd,
            Some("po"),
            Some(std::path::Path::new("/nonexistent/cookies")),
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
