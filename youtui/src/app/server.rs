use crate::config::Config;
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
    pub config: Config,
    pub po_token: Option<String>,
}

impl Server {
    pub fn new(
        api_key: crate::config::ApiKey,
        po_token: Option<String>,
        config: &Config,
    ) -> anyhow::Result<Server> {
        let downloader_client = {
            use reqwest::header::{COOKIE, HeaderMap, HeaderValue};
            if let Some(cookie) = extract_cookie_header(&api_key) {
                match HeaderValue::from_str(&cookie) {
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
            config: config.clone(),
            po_token,
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
        crate::config::ApiKey::BrowserToken(cookie_str) => {
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
        _ => None,
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
}
