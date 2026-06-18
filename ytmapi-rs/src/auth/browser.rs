use super::{AuthToken, RawResult};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::parse::ProcessedResult;
use crate::utils;
use crate::utils::constants::{USER_AGENT, YTM_URL};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt::Debug;
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
pub struct BrowserToken {
    sapisid: String,
    client_version: String,
    cookies: String,
}

impl AuthToken for BrowserToken {
    fn client_version(&self) -> Cow<'_, str> {
        (&self.client_version).into()
    }
    fn deserialize_response<Q>(
        raw: RawResult<Q, Self>,
    ) -> Result<crate::parse::ProcessedResult<Q>> {
        let processed = ProcessedResult::try_from(raw)?;
        // Guard against error codes in json response.
        // TODO: Check for a response the reflects an expired Headers token
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
        let hash = utils::hash_sapisid(&self.sapisid);
        Ok([
            ("X-Origin", YTM_URL.into()),
            ("Origin", YTM_URL.into()),
            ("Content-Type", "application/json".into()),
            ("Authorization", format!("SAPISIDHASH {hash}").into()),
            ("Cookie", self.cookies.as_str().into()),
            ("Accept", "*/*".into()),
            ("Accept-Encoding", "gzip, deflate".into()),
        ])
    }
}

impl BrowserToken {
    pub async fn from_str(cookie_str: &str, client: &Client) -> Result<Self> {
        let cookies = cookie_str.trim().to_string();
        let user_agent = USER_AGENT;
        let initial_headers = [
            ("User-Agent", user_agent.into()),
            ("Cookie", cookies.as_str().into()),
        ];
        let response_text = client.get_query(YTM_URL, initial_headers, &()).await?.text;
        // Check for rejected user agent (English message; localized versions fall
        // through to the "missing INNERTUBE_CLIENT_VERSION" Header error below with
        // a descriptive message instead of the old opaque "Error parsing header.").
        if response_text.contains("Sorry, YouTube Music is not optimised for your browser. Check for updates or try Google Chrome.") {
            return Err(Error::invalid_user_agent(user_agent));
        };
        let client_version = response_text
            .split_once("INNERTUBE_CLIENT_VERSION\":\"")
            .ok_or(Error::header("missing INNERTUBE_CLIENT_VERSION in YouTube Music page"))?
            .1
            .split_once('\"')
            .ok_or(Error::header("malformed INNERTUBE_CLIENT_VERSION value"))?
            .0
            .to_string();
        let sapisid = cookies
            .split_once("SAPISID=")
            .ok_or(Error::header("cookie string missing SAPISID"))?
            .1
            .split_once(';')
            .ok_or(Error::header("SAPISID cookie not terminated by ';'"))?
            .0
            .to_string();
        Ok(Self {
            sapisid,
            client_version,
            cookies,
        })
    }
    pub async fn from_cookie_file<P>(path: P, client: &Client) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        let contents = tokio::fs::read_to_string(path).await?;
        BrowserToken::from_str(&contents, client).await
    }
}

// Don't use default Debug implementation for BrowserToken - contents are
// private
impl Debug for BrowserToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Private BrowserToken")
    }
}
