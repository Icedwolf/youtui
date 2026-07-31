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

pub fn apply_ytdlp_auth_args(
    cmd: &mut tokio::process::Command,
    po_token: Option<&str>,
    cookie_path: Option<&Path>,
    cookie_header: Option<&str>,
    js_runtime: Option<&str>,
    video_id: &str,
) {
    let skip = "hls,translated_subs";
    let extractor_args = match po_token {
        Some(pt) => format!("youtube:po_token={pt};skip={skip}"),
        None => format!("youtube:skip={skip}"),
    };
    cmd.arg("--extractor-args");
    cmd.arg(&extractor_args);
    if let Some(cp) = cookie_path {
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
) -> Option<String> {
    if let Some(url) = url_cache_get(video_id) {
        return Some(url);
    }

    if let Some(ct) = cancel_token && ct.is_cancelled() {
        return None;
    }

    let cmd_name = if yt_dlp_cmd.is_empty() { "yt-dlp" } else { yt_dlp_cmd };
    let mut cmd = tokio::process::Command::new(cmd_name);
    cmd.arg("--print").arg("url");
    cmd.arg("-f").arg("bestaudio[ext=webm]/bestaudio");
    cmd.arg("--no-warnings").arg("--no-playlist");
    apply_ytdlp_auth_args(&mut cmd, po_token, cookie_path, cookie_header, js_runtime, video_id);

    let resolve = async {
        let output = cmd.output().await.ok()?;
        if !output.status.success() {
            return None;
        }
        let url = String::from_utf8(output.stdout).ok()?;
        let url = url.trim().to_string();
        if url.is_empty() {
            return None;
        }
        Some(url)
    };

    let url = if let Some(ct) = cancel_token {
        tokio::select! {
            biased;
            _ = ct.cancelled() => None,
            url = resolve => url,
        }
    } else {
        resolve.await
    };

    if let Some(ref url) = url {
        url_cache_put(video_id.to_string(), url.clone());
    }
    url
}
