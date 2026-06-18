use super::{AuthToken, RawResult, fallback_client_version};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::parse::ProcessedResult;
use crate::utils::constants::{USER_AGENT, YTM_URL};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoAuthToken {
    create_time: chrono::DateTime<Utc>,
    visitor_id: String,
}

impl NoAuthToken {
    pub async fn new(client: &Client) -> Result<Self> {
        let initial_headers = [
            // TODO: Confirm if parsing for expired user agent also relevant here.
            ("User-Agent", USER_AGENT.into()),
            ("X-Origin", YTM_URL.into()),
            ("Content-Type", "application/json".into()),
        ];
        let result_text = client.get_query(YTM_URL, initial_headers, &()).await?.text;
        // Extract the parameter from inside the ytcfg.set() call.
        // More resilient than the original split-based approach: handles
        // whitespace, semicolons, and won't break on `})` inside string values.
        // Original implementation: https://github.com/sigma67/ytmusicapi/blob/459bc40e4ce31584f9d87cf75838a1f404aa472d/ytmusicapi/helpers.py#L44
        let ytcfg_raw = extract_ytcfg_json(&result_text)
            .ok_or_else(|| Error::ytcfg(&result_text))?;
        let mut ytcfg: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&format!("{{{ytcfg_raw}}}"))
                .map_err(|_| Error::ytcfg(ytcfg_raw))?;
        let visitor_id = serde_json::from_value(
            ytcfg
                .remove("VISITOR_DATA")
                .ok_or_else(Error::no_visitor_data)?,
        )
        .map_err(|_| Error::no_visitor_data())?;
        Ok(Self {
            create_time: Utc::now(),
            visitor_id,
        })
    }
}

/// Find the JSON payload inside `ytcfg.set(...)` in the YouTube Music HTML page.
/// Returns the raw JSON substring (without outer braces) on success.
fn extract_ytcfg_json(page: &str) -> Option<&str> {
    let start = page.find("ytcfg.set")?;
    let rest = &page[start + 9..].trim_start();
    let rest = rest.strip_prefix('(')?;
    let brace_start = rest.find('{')?;
    let body = &rest[brace_start..];
    let mut depth = 0u32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        if escaped { escaped = false; continue; }
        if c == '\\' { escaped = true; continue; }
        if c == '"' { in_string = !in_string; continue; }
        if !in_string {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &body[1..i];
                        return Some(inner.trim());
                    }
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_ytcfg_json_basic() {
        let page = r#"<html>ytcfg.set({"key": "value"})</html>"#;
        assert_eq!(extract_ytcfg_json(page), Some(r#""key": "value""#));
    }

    #[test]
    fn test_extract_ytcfg_json_with_whitespace() {
        let page = r#"<html>ytcfg.set ( { "key": "value" } ) ;</html>"#;
        assert_eq!(extract_ytcfg_json(page), Some(r#""key": "value""#));
    }

    #[test]
    fn test_extract_ytcfg_json_nested() {
        let page = r#"<html>ytcfg.set({"a": {"b": 1}})</html>"#;
        assert_eq!(extract_ytcfg_json(page), Some(r#""a": {"b": 1}"#));
    }

    #[test]
    fn test_extract_ytcfg_json_brace_in_string() {
        let page = r#"<html>ytcfg.set({"key": "value } with brace"})</html>"#;
        assert_eq!(extract_ytcfg_json(page), Some(r#""key": "value } with brace""#));
    }

    #[test]
    fn test_extract_ytcfg_json_not_found() {
        assert_eq!(extract_ytcfg_json("<html>no ytcfg here</html>"), None);
    }

    #[test]
    fn test_extract_ytcfg_json_empty_object() {
        let page = "<html>ytcfg.set({})</html>";
        assert_eq!(extract_ytcfg_json(page), Some(""));
    }

    #[test]
    fn test_extract_ytcfg_json_escaped_quote() {
        let page = r#"<html>ytcfg.set({"key": "val\"ue"})</html>"#;
        assert_eq!(extract_ytcfg_json(page), Some(r#""key": "val\"ue""#));
    }
}

impl AuthToken for NoAuthToken {
    fn client_version(&self) -> Cow<'_, str> {
        fallback_client_version(&self.create_time).into()
    }
    fn deserialize_response<Q>(
        raw: RawResult<Q, Self>,
    ) -> Result<crate::parse::ProcessedResult<Q>> {
        let processed = ProcessedResult::try_from(raw)?;
        // Guard against error codes in json response.
        // TODO: Add a test for this
        if let Some(error) = processed.get_json().pointer("/error") {
            let Some(code) = error.pointer("/code").and_then(|v| v.as_u64()) else {
                // TODO: Better error.
                return Err(Error::response("API reported an error but no code"));
            };
            let message = error
                .pointer("/message")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            return Err(Error::other_code(code, message));
        }
        Ok(processed)
    }
    fn headers(&self) -> Result<impl IntoIterator<Item = (&str, Cow<'_, str>)>> {
        Ok([
            // TODO: Confirm if parsing for expired user agent also relevant here.
            ("User-Agent", USER_AGENT.into()),
            ("X-Origin", YTM_URL.into()),
            ("X-Goog-Visitor-Id", (&self.visitor_id).into()),
            ("Content-Type", "application/json".into()),
        ])
    }
}
