//! Integration tests for key components, to allow for automated checking of 3rd party API changes.
use std::env;
use std::path::Path;
use tokio::sync::OnceCell;
use ytmapi_rs::auth::BrowserToken;
use ytmapi_rs::common::YoutubeID;
use ytmapi_rs::{YtMusic, YtMusicBuilder};

const COOKIE_PATH: &str = "../ytmapi-rs/cookie.txt";

static API: OnceCell<YtMusic<BrowserToken>> = OnceCell::const_new();

async fn get_api() -> &'static YtMusic<BrowserToken> {
    API.get_or_init(|| async {
        if let Ok(cookie) = env::var("youtui_test_cookie") {
            YtMusicBuilder::new_rustls_tls()
                .with_browser_token_cookie(cookie)
                .build()
                .await
                .unwrap()
        } else {
            YtMusicBuilder::new_rustls_tls()
                .with_browser_token_cookie_file(Path::new(COOKIE_PATH))
                .build()
                .await
                .unwrap()
        }
    })
    .await
}
