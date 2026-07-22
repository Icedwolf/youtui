#![allow(clippy::unwrap_used)]
use std::env;
use std::path::Path;
use ytmapi_rs::auth::BrowserToken;
use ytmapi_rs::{Result, YtMusic};

pub const COOKIE_PATH: &str = "cookie.txt";
// Cookie filled with nonsense values to test this case.
// pub const INVALID_COOKIE: &str = "HSID=abc; SSID=abc; APISID=abc; SAPISID=abc; __Secure-1PAPISID=abc; __Secure-3PAPISID=abc; YSC=abc; LOGIN_INFO=abc; VISITOR_INFO1_LIVE=abc; _gcl_au=abc; PREF=tz=Australia.Perth&f6=40000000&f7=abc; VISITOR_PRIVACY_METADATA=abc; __Secure-1PSIDTS=abc; __Secure-3PSIDTS=abc; SID=abc; __Secure-1PSID=abc; __Secure-3PSID=abc; SIDCC=abc; __Secure-1PSIDCC=abc; __Secure-3PSIDCC=abc";

// It may be possible to put these inside a static, but last time I tried I kept
// getting web errors.
// The cause of the web errors is that each tokio::test has its own runtime.
// To resolve this, we'll need a shared runtime as well as a static containing
// the API.
/// Returns `None` if neither the cookie file nor env var exist — tests
/// skip gracefully instead of panicking with "No such file or directory".
pub async fn maybe_new_standard_api() -> Option<YtMusic<BrowserToken>> {
    if env::var("youtui_test_cookie").is_ok() || Path::new(COOKIE_PATH).exists() {
        Some(new_standard_api().await.unwrap())
    } else {
        None
    }
}

pub async fn new_standard_api() -> Result<YtMusic<BrowserToken>> {
    if let Ok(cookie) = env::var("youtui_test_cookie") {
        YtMusic::from_cookie(cookie).await
    } else {
        YtMusic::from_cookie_file(Path::new(COOKIE_PATH)).await
    }
}

/// Macro to generate browser tests for provided query.
/// Attributes like #[ignore] can be passed as the optional first argument.
macro_rules! generate_query_test_logged_in {
    ($(#[$m:meta])*
    $fname:ident,$query:expr_2021) => {
        paste::paste! {
            $(#[$m])*
            #[tokio::test]
            async fn [<$fname _browser>]() {
                let Some(api) = crate::utils::maybe_new_standard_api().await else {
                    eprintln!("SKIP: browser auth not configured (set youtui_test_cookie or create cookie.txt)");
                    return;
                };
                api.query($query)
                    .await
                    .expect("Expected query to run succesfully under browser auth");
            }
        }
    };
}

/// Macro to generate noauth and browser tests for provided query.
macro_rules! generate_query_test {
    ($(#[$m:meta])*
    $fname:ident,$query:expr_2021) => {
        paste::paste! {
            $(#[$m])*
            #[tokio::test]
            async fn [<$fname _browser>]() {
                let Some(api) = crate::utils::maybe_new_standard_api().await else {
                    eprintln!("SKIP: browser auth not configured (set youtui_test_cookie or create cookie.txt)");
                    return;
                };
                api.query($query)
                    .await
                    .expect("Expected query to run succesfully under browser auth");
            }
            $(#[$m])*
            #[tokio::test]
            async fn [<$fname _noauth>]() {
                let api = YtMusic::new_unauthenticated().await.unwrap();
                api.query($query)
                    .await
                    .expect("Expected query to run succesfully without auth");
            }
        }
    };
}

/// Macro to generate noauth and browser tests for provided stream.
/// Attributes like #[ignore] can be passed as the optional first argument.
macro_rules! generate_stream_test {
    ($(#[$m:meta])*
    $fname:ident,$query:expr_2021) => {
        paste::paste! {
            $(#[$m])*
            #[tokio::test]
            async fn [<$fname _browser>]() {
                use futures::stream::{StreamExt, TryStreamExt};
                let Some(api) = crate::utils::maybe_new_standard_api().await else {
                    eprintln!("SKIP: browser auth not configured (set youtui_test_cookie or create cookie.txt)");
                    return;
                };
                let query = $query;
                let stream = api.stream(&query);
                tokio::pin!(stream);
                stream
                    // limit test to 5 results to avoid overload
                    .take(5)
                    .try_collect::<Vec<_>>()
                    .await
                    .expect("Expected all results from browser stream to suceed");
            }
            $(#[$m])*
            #[tokio::test]
            async fn [<$fname _noauth>]() {
                use futures::stream::{StreamExt, TryStreamExt};
                let api = YtMusic::new_unauthenticated().await.unwrap();
                let query = $query;
                let stream = api.stream(&query);
                tokio::pin!(stream);
                stream
                    // limit test to 5 results to avoid overload
                    .take(5)
                    .try_collect::<Vec<_>>()
                    .await
                    .expect("Expected all results from stream to succeed without auth");
            }
        }
    };
}

/// Macro to generate browser tests for provided stream.
/// Attributes like #[ignore] can be passed as the optional first argument.
macro_rules! generate_stream_test_logged_in {
    ($(#[$m:meta])*
    $fname:ident,$query:expr_2021) => {
        paste::paste! {
            $(#[$m])*
            #[tokio::test]
            async fn [<$fname _browser>]() {
                use futures::stream::{StreamExt, TryStreamExt};
                let Some(api) = crate::utils::maybe_new_standard_api().await else {
                    eprintln!("SKIP: browser auth not configured (set youtui_test_cookie or create cookie.txt)");
                    return;
                };
                let query = $query;
                let stream = api.stream(&query);
                tokio::pin!(stream);
                stream
                    // limit test to 5 results to avoid overload
                    .take(5)
                    .try_collect::<Vec<_>>()
                    .await
                    .expect("Expected all results from browser stream to suceed");
            }
        }
    };
}
