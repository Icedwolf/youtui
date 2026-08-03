use crate::config::Config;
use self::song_downloader::resolve::is_nonempty_cookie_file;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;

pub mod api;
pub mod player;
pub mod song_downloader;
pub mod streaming_buffer;

pub type ArcServer = Arc<Server>;

pub struct Server {
    pub api: api::Api,
    pub player: player::Player,
    pub config: Arc<Config>,
    pub po_token: Option<String>,
    pub cookie_path: Option<PathBuf>,
    pub cookie_header: Option<String>,
    pub js_runtime: Option<String>,
}

impl Server {
    pub fn new(
        api_key: crate::config::ApiKey,
        po_token: Option<String>,
        config: &Config,
        cookie_path: Option<PathBuf>,
        js_runtime: Option<String>,
    ) -> anyhow::Result<Server> {
        let cookie_header = resolve_cookie_header(cookie_path.as_deref(), &api_key);
        let downloader_client = {
            use reqwest::header::{COOKIE, HeaderMap, HeaderValue};
            if let Some(ref cookie) = cookie_header {
                match HeaderValue::from_str(cookie) {
                    Ok(cookie_val) => {
                        let mut headers = HeaderMap::new();
                        headers.insert(COOKIE, cookie_val);
                        new_reqwest_client_builder()
                            .default_headers(headers)
                            .build()?
                    }
                    Err(e) => {
                        warn!("Invalid cookie header, falling back to unauthenticated client: {e}");
                        new_reqwest_client_builder().build()?
                    }
                }
            } else {
                new_reqwest_client_builder().build()?
            }
        };
        let api_client = downloader_client.clone();
        let api = api::Api::new(api_key, api_client);
        let player = player::Player::new()?;
        Ok(Server {
            api,
            player,
            config: Arc::new(config.clone()),
            po_token,
            cookie_path,
            cookie_header,
            js_runtime,
        })
    }
}

fn new_reqwest_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .use_rustls_tls()
        .pool_max_idle_per_host(8)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(30))
}

fn extract_cookie_header(api_key: &crate::config::ApiKey) -> Option<String> {
    match api_key {
        crate::config::ApiKey::BrowserToken(cookie_str) => extract_cookie_header_str(cookie_str),
        _ => None,
    }
}

/// Cookie names that mark a genuinely signed-in YouTube session. Guest
/// exports carry only `VISITOR_INFO1_LIVE`/`__Secure-YEC` etc., which are not
/// enough for authenticated API calls — an export without one of these must
/// not shadow the manual login header.
const AUTH_COOKIE_NAMES: [&str; 6] = [
    "SID",
    "__Secure-1PSID",
    "__Secure-3PSID",
    "SAPISID",
    "APISID",
    "__Secure-3PAPISID",
];

fn has_auth_cookie(header: &str) -> bool {
    header.split(';').any(|seg| {
        let name = seg.trim().split('=').next().unwrap_or_default();
        AUTH_COOKIE_NAMES.contains(&name)
    })
}

/// Resolve the API's `Cookie` header from a single unified auth source. A
/// non-empty exported browser cookie file wins only if it carries a signed-in
/// session (`has_auth_cookie`); any other case falls back to the manual
/// `api_key` header so auth degrades gracefully instead of vanishing when the
/// export is stale, empty, or holds only guest cookies.
fn resolve_cookie_header(
    cookie_path: Option<&Path>,
    api_key: &crate::config::ApiKey,
) -> Option<String> {
    if let Some(cp) = cookie_path.filter(|cp| is_nonempty_cookie_file(cp))
        && let Ok(content) = std::fs::read_to_string(cp)
        && let Some(header) = extract_cookie_header_str(&content)
        && has_auth_cookie(&header)
    {
        return Some(header);
    }
    extract_cookie_header(api_key)
}

fn extract_cookie_header_str(cookie_str: &str) -> Option<String> {
    let mut cookies: Vec<String> = Vec::new();
    for line in cookie_str.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 7 {
            cookies.push(format!("{}={}", fields[5], fields[6]));
        } else {
            let header = line.strip_prefix("Cookie:").unwrap_or(line).trim();
            for kv in header.split(';') {
                let kv = kv.trim();
                if let Some((name, value)) = kv.split_once('=') {
                    let name = name.trim();
                    let value = value.trim();
                    if !name.is_empty() && !value.is_empty() {
                        cookies.push(format!("{name}={value}"));
                    }
                }
            }
        }
    }
    if cookies.is_empty() {
        None
    } else {
        Some(cookies.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApiKey;

    #[test]
    fn test_extract_cookie_header_empty() {
        let api_key = ApiKey::BrowserToken(String::new());
        assert_eq!(extract_cookie_header(&api_key), None);
    }

    #[test]
    fn test_extract_cookie_header_comments_only() {
        let api_key = ApiKey::BrowserToken(
            "# Netscape HTTP Cookie File\n# https://curl.se/docs/http-cookies.html\n".into(),
        );
        assert_eq!(extract_cookie_header(&api_key), None);
    }

    #[test]
    fn test_extract_cookie_header_valid() {
        let cookie_file = "\
.youtube.com\tTRUE\t/\tTRUE\t1735689600\tSAPISID\tabc123\n\
.youtube.com\tTRUE\t/\tTRUE\t1735689600\t__Secure-3PSAPISID\tdef456\n\
music.youtube.com\tTRUE\t/\tTRUE\t1735689600\tVISITOR_INFO1_LIVE\txyz789\n";
        let api_key = ApiKey::BrowserToken(cookie_file.into());
        let result = extract_cookie_header(&api_key);
        assert!(result.is_some());
        let header = result.unwrap();
        assert!(header.contains("SAPISID=abc123"));
        assert!(header.contains("__Secure-3PSAPISID=def456"));
        assert!(header.contains("VISITOR_INFO1_LIVE=xyz789"));
    }

    #[test]
    fn test_extract_cookie_header_noauth_returns_none() {
        let api_key = ApiKey::None;
        assert_eq!(extract_cookie_header(&api_key), None);
    }

    #[test]
    fn test_extract_cookie_header_skips_malformed_lines() {
        let cookie_file = "\
.youtube.com\tTRUE\t/\tTRUE\tSAPISID\tabc123\n\
.youtube.com\tTRUE\t/\tTRUE\t1735689600\tSAPISID\tvalid_value\n";
        let api_key = ApiKey::BrowserToken(cookie_file.into());
        let result = extract_cookie_header(&api_key);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "SAPISID=valid_value");
    }

    #[test]
    fn test_extract_cookie_header_cookie_format() {
        let cookie_file =
            "Cookie: SAPISID=abc123; __Secure-3PSAPISID=def456; VISITOR_INFO1_LIVE=xyz789";
        let api_key = ApiKey::BrowserToken(cookie_file.into());
        let result = extract_cookie_header(&api_key);
        assert!(result.is_some());
        let header = result.unwrap();
        assert!(header.contains("SAPISID=abc123"));
        assert!(header.contains("__Secure-3PSAPISID=def456"));
        assert!(header.contains("VISITOR_INFO1_LIVE=xyz789"));
        assert_eq!(header.matches("; ").count(), 2);
    }

    #[test]
    fn test_extract_cookie_header_cookie_format_no_prefix() {
        let cookie_file = "SAPISID=abc123; VISITOR_INFO1_LIVE=xyz789";
        let api_key = ApiKey::BrowserToken(cookie_file.into());
        let result = extract_cookie_header(&api_key);
        assert!(result.is_some());
        let header = result.unwrap();
        assert!(header.contains("SAPISID=abc123"));
        assert!(header.contains("VISITOR_INFO1_LIVE=xyz789"));
    }

    #[test]
    fn test_extract_cookie_header_cookie_format_single() {
        let cookie_file = "SAPISID=abc123";
        let api_key = ApiKey::BrowserToken(cookie_file.into());
        let result = extract_cookie_header(&api_key);
        assert_eq!(result, Some("SAPISID=abc123".into()));
    }

    #[test]
    fn test_extract_cookie_header_empty_value_skipped() {
        let cookie_file = "SAPISID=abc123; EMPTY=";
        let api_key = ApiKey::BrowserToken(cookie_file.into());
        let result = extract_cookie_header(&api_key);
        assert_eq!(result, Some("SAPISID=abc123".into()));
    }

    #[test]
    fn test_extract_cookie_header_netscape_preferred_over_cookie() {
        let cookie_file = "\
.youtube.com\tTRUE\t/\tTRUE\t1735689600\tSAPISID\tfrom_netscape\n\
Cookie: SAPISID=from_header";
        let api_key = ApiKey::BrowserToken(cookie_file.into());
        let result = extract_cookie_header(&api_key);
        assert!(result.is_some());
        let header = result.unwrap();
        assert!(header.contains("SAPISID=from_netscape"));
        assert!(header.contains("SAPISID=from_header"));
    }

    #[test]
    fn exported_cookie_file_preferred_over_api_key() {
        let path = std::env::temp_dir().join(format!("youtui_cookie_hdr_{}.txt", std::process::id()));
        std::fs::write(&path, ".youtube.com\tTRUE\t/\tTRUE\t1735689600\tSAPISID\tfrom_export\n")
            .expect("write exported cookies");
        let api_key = ApiKey::BrowserToken("SAPISID=from_manual".into());

        // Non-empty exported file must win over the manual api_key header.
        let header = resolve_cookie_header(Some(&path), &api_key).unwrap();
        assert!(header.contains("SAPISID=from_export"), "exported cookies must win");

        // Empty export -> fall back to the manual header.
        std::fs::write(&path, b"").expect("write empty cookies");
        let header = resolve_cookie_header(Some(&path), &api_key).unwrap();
        assert!(header.contains("SAPISID=from_manual"), "empty export must fall back");

        // Missing export -> fall back to the manual header.
        let missing = std::env::temp_dir().join("youtui_missing_cookie_hdr.txt");
        let _ = std::fs::remove_file(&missing);
        let header = resolve_cookie_header(Some(&missing), &api_key).unwrap();
        assert!(header.contains("SAPISID=from_manual"), "missing export must fall back");

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&missing).ok();
    }

    #[test]
    fn exported_guest_cookies_fall_back_to_manual() {
        let path = std::env::temp_dir().join(format!("youtui_cookie_guest_{}.txt", std::process::id()));
        // Guest-only cookies: no SID/APISID family -> not a real login.
        std::fs::write(
            &path,
            ".youtube.com\tTRUE\t/\tTRUE\t1735689600\tVISITOR_INFO1_LIVE\txyz\n",
        )
        .expect("write guest cookies");
        let api_key = ApiKey::BrowserToken("SAPISID=from_manual".into());
        let header = resolve_cookie_header(Some(&path), &api_key).unwrap();
        assert!(
            header.contains("from_manual"),
            "guest-only export must fall back to the manual login header"
        );
        assert!(
            !header.contains("VISITOR_INFO1_LIVE"),
            "guest cookies must not win over the manual login"
        );
        std::fs::remove_file(&path).ok();
    }
}
