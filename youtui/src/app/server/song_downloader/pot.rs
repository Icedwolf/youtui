//! Per-video GVS PO token generation.
//!
//! YouTube Music now requires a GVS PO token on the `web_music` (WEB_REMIX)
//! player client, and currently binds the token to the *video ID* (the
//! "bind GVS PO Token to video ID" experiment). A static token (the old
//! `po_token.txt`) is both rejected by current yt-dlp (it requires the
//! `CLIENT.CONTEXT+TOKEN` format) and not bound to the resolved song, so the
//! CDN 403s it. The token must be minted per `video_id`.
//!
//! Generation is delegated to the bundled `pot-provider/generate.mjs` botguard
//! script under node (`node generate.mjs -c <video_id>` → JSON on stdout). When
//! node and the script are present the app auto-generates; otherwise it falls
//! back to the old no-token behavior. Tokens are cached per `video_id` with a
//! short TTL so a relay retry (throttle wave) does not re-spawn node.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use super::apply_child_env;

const TOKEN_CACHE_TTL: Duration = Duration::from_secs(30 * 60);

pub(crate) static TOKEN_CACHE: LazyLock<Mutex<HashMap<String, (String, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_get(video_id: &str) -> Option<String> {
    let mut cache = TOKEN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((token, ts)) = cache.get(video_id) {
        if ts.elapsed() < TOKEN_CACHE_TTL {
            return Some(token.clone());
        }
        cache.remove(video_id);
    }
    None
}

fn cache_put(video_id: String, token: String) {
    let mut cache = TOKEN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(video_id, (token, Instant::now()));
}

/// Parse the `poToken` field out of the generator's JSON stdout line.
fn parse_po_token(stdout: &str) -> Option<String> {
    // `{"poToken":"...","contentBinding":"...","expiresAt":"..."}`
    let line = stdout.lines().find(|l| l.contains("\"poToken\""))?;
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    let token = value.get("poToken")?.as_str()?.to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Generate (or return the cached) GVS PO token bound to `video_id`.
///
/// `gen_path` is the path to the bundled `generate.mjs` script; `node` is the
/// JS runtime name (`"node"`) only when node was detected at startup. Returns
/// `None` when generation is not configured or fails — the caller then proceeds
/// without a token (the historical no-po_token path).
pub(crate) fn generate_po_token(video_id: &str, gen_path: &Path, node: Option<&str>) -> Option<String> {
    let node = node?;
    if let Some(cached) = cache_get(video_id) {
        return Some(cached);
    }

    let mut cmd = Command::new(node);
    apply_child_env(&mut cmd);
    cmd.arg(gen_path).arg("-c").arg(video_id);
    let output = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            warn!(%video_id, error = %e, "po_token: cannot spawn node generator");
            return None;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(%video_id, stderr = %stderr, "po_token: generator failed");
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let token = match parse_po_token(&stdout) {
        Some(t) => t,
        None => {
            warn!(%video_id, stdout = %stdout, "po_token: no poToken in generator output");
            return None;
        }
    };
    debug!(%video_id, "po_token: generated per-video GVS token");
    cache_put(video_id.to_string(), token.clone());
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::{generate_po_token, parse_po_token};

    #[test]
    fn parse_po_token_extracts_field() {
        let out = r#"{"poToken":"abc.def","contentBinding":"vid","expiresAt":"2026-09-05T00:00:00Z"}"#;
        assert_eq!(parse_po_token(out), Some("abc.def".to_string()));
    }

    #[test]
    fn parse_po_token_rejects_missing_or_empty() {
        assert_eq!(parse_po_token("{}"), None);
        assert_eq!(parse_po_token(r#"{"poToken":""}"#), None);
        assert_eq!(parse_po_token("garbage"), None);
    }

    #[test]
    fn no_node_yields_none() {
        assert_eq!(
            generate_po_token("vid", std::path::Path::new("/nonexistent/generate.mjs"), None),
            None
        );
    }

    #[test]
    fn missing_generator_yields_none() {
        // node present but the script path does not exist -> child spawn fails -> None
        assert_eq!(
            generate_po_token("vid", std::path::Path::new("/nonexistent/generate.mjs"), Some("node")),
            None
        );
    }
}
