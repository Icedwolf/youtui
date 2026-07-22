//! # ytmapi_rs
//! Library into YouTube Music's internal API.
//! ## Examples
//! For additional examples using builder, see [`builder`] module.
//! ### Unauthenticated usage - note, not all queries supported.
//! ```no_run
//! #[tokio::main]
//! pub async fn main() -> Result<(), ytmapi_rs::Error> {
//!     let yt = ytmapi_rs::YtMusic::new_unauthenticated().await?;
//!     yt.get_search_suggestions("Beatles").await?;
//!     let result = yt.get_search_suggestions("Beatles").await?;
//!     println!("{:?}", result);
//!     Ok(())
//! }
//! ```
//! ### Basic authenticated usage with a pre-created cookie file, demonstrating uploading a song.
//! ```no_run
//! #[tokio::main]
//! pub async fn main() -> Result<(), ytmapi_rs::Error> {
//!     let cookie_path = std::path::Path::new("./cookie.txt");
//!     let yt = ytmapi_rs::YtMusic::from_cookie_file(cookie_path).await?;
//!     yt.get_search_suggestions("Beatles").await?;
//!     let result = yt.get_search_suggestions("Beatles").await?;
//!     println!("{:?}", result);
//!     assert_eq!(
//!         yt.upload_song("my_song_to_upload.mp3").await.unwrap(),
//!         ytmapi_rs::common::ApiOutcome::Success
//!     );
//!     Ok(())
//! }
//! ```
//! ## Optional Features
//! ### TLS
//! NOTE: reqwest will prefer to utilise default-tls if multiple features are
//! built when using the standard constructors. Use `YtMusicBuilder` to ensure
//! the preferred choice of TLS is used. See reqwest docs for more information <https://docs.rs/reqwest/latest/reqwest/tls/index.html>.
//! - **default-tls** *(enabled by default)*: Utilises the default TLS from
//!   reqwest - at the time of writing is native-tls.
//! - **native-tls**: This feature allows use of the the native-tls crate,
//!   reliant on vendors tls.
//! - **rustls**: This feature allows use of the rustls crate, written in rust.
//! ### Other
//! - **simplified_queries**: Adds convenience methods to [`YtMusic`].
//! - **serde_json**: Enables some interoperability functions with `serde_json`.
//! - **reqwest**: Enables some interoperability functions with `reqwest`.
// For feature specific documentation.
#![cfg_attr(docsrs, feature(doc_cfg))]
#[cfg(not(any(feature = "rustls", feature = "native-tls", feature = "default-tls")))]
compile_error!("One of the TLS features must be enabled for this crate");
use auth::browser::BrowserToken;
use auth::noauth::NoAuthToken;
use auth::{AuthToken, RawResult};
#[doc(inline)]
pub use builder::YtMusicBuilder;
#[doc(inline)]
pub use client::Client;
use common::ApiOutcome;
use continuations::ParseFromContinuable;
#[doc(inline)]
pub use error::{Error, Result};
use futures::Stream;
use json::Json;
use parse::ParseFrom;
#[doc(inline)]
pub use parse::ProcessedResult;
use query::{PostQuery, Query, QueryMethod};
use std::borrow::Borrow;
use std::path::Path;

#[macro_use]
mod utils;
mod nav_consts;
mod upload_song;
mod youtube_enums;

pub mod auth;
pub mod builder;
pub mod client;
pub mod common;
pub mod continuations;
pub mod error;
pub mod json;
pub mod parse;
pub mod query;

#[cfg(feature = "simplified-queries")]
#[cfg_attr(docsrs, doc(cfg(feature = "simplified-queries")))]
pub mod simplified_queries;

#[derive(Debug, Clone)]
// XXX: Consider wrapping auth in reference counting for cheap cloning.
// XXX: Note that we would then need to use a RwLock if we wanted to use mutability for
// refresh_token().
/// A handle to the YouTube Music API, wrapping a http client.
/// Generic over AuthToken, as different AuthTokens may allow different queries
/// to be executed.
/// It is recommended to re-use these as they internally contain a connection
/// pool.
/// # Documentation note
/// Examples given for methods on this struct will use fake or mock
/// constructors. When using in a real environment, you will need to construct
/// using a real token or cookie.
pub struct YtMusic<A: AuthToken> {
    // TODO: add language
    // TODO: add location
    client: Client,
    token: A,
}
impl YtMusic<NoAuthToken> {
    /// Create a new unauthenticated API handle.
    /// In unauthenticated mode, less queries are supported.
    /// Utilises the default TLS option for the enabled features.
    pub async fn new_unauthenticated() -> Result<Self> {
        let client = Client::new()?;
        let token = NoAuthToken::new(&client).await?;
        Ok(YtMusic { client, token })
    }
}
impl YtMusic<BrowserToken> {
    /// Create a new API handle using a BrowserToken.
    /// Utilises the default TLS option for the enabled features.
    #[deprecated = "Use generic `from_auth_token` instead"]
    pub fn from_browser_token(token: BrowserToken) -> Result<YtMusic<BrowserToken>> {
        let client = Client::new()?;
        Ok(YtMusic { client, token })
    }
    /// Create a new API handle using a real browser authentication cookie saved
    /// to a file on disk.
    /// Utilises the default TLS option for the enabled features.
    pub async fn from_cookie_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let client = Client::new()?;
        let token = BrowserToken::from_cookie_file(path, &client).await?;
        Ok(Self { client, token })
    }
    /// Create a new API handle using a real browser authentication cookie in a
    /// String.
    /// Utilises the default TLS option for the enabled features.
    pub async fn from_cookie<S: AsRef<str>>(cookie: S) -> Result<Self> {
        let client = Client::new()?;
        let token = BrowserToken::from_str(cookie.as_ref(), &client).await?;
        Ok(Self { client, token })
    }
    /// Upload a song to your YouTube Music library. Only available using
    /// Browser auth.
    /// ```no_run
    /// # async {
    /// let yt = ytmapi_rs::YtMusic::from_cookie("FAKE COOKIE").await.unwrap();
    /// yt.upload_song("test_song_to_upload.mp3").await
    /// # };
    pub async fn upload_song(&self, file_path: impl AsRef<Path>) -> Result<ApiOutcome> {
        upload_song::upload_song(file_path, &self.token, &self.client).await
    }
}
impl<A: AuthToken> YtMusic<A> {
    /// Create a new API handle using a AuthToken.
    /// Utilises the default TLS option for the enabled features.
    pub fn from_auth_token(token: A) -> Result<YtMusic<A>> {
        let client = Client::new()?;
        Ok(YtMusic { client, token })
    }
    /// Return the source JSON returned by YouTube music for the query, prior to
    /// deserialization and error processing.
    /// # Usage
    /// ```no_run
    /// use ytmapi_rs::auth::BrowserToken;
    ///
    /// # async {
    /// let yt = ytmapi_rs::YtMusic::from_cookie("FAKE COOKIE").await?;
    /// let query = ytmapi_rs::query::SearchQuery::new_filtered("Beatles", ytmapi_rs::query::search::ArtistsFilter);
    /// let result = yt.raw_json_query(query).await?;
    /// assert!(result.len() != 0);
    /// # Ok::<(), ytmapi_rs::Error>(())
    /// # };
    /// ```
    pub async fn raw_json_query<Q: Query<A>>(&self, query: impl Borrow<Q>) -> Result<String> {
        Q::Method::call(query.borrow(), &self.client, &self.token)
            .await
            .map(|raw| raw.json)
    }
    /// Return a result from YouTube music that has had errors removed and been
    /// deserialized into parsable JSON.
    /// The return type implements Serialize and Deserialize.
    /// # Usage
    /// ```no_run
    /// use ytmapi_rs::auth::BrowserToken;
    /// use ytmapi_rs::parse::ParseFrom;
    ///
    /// # async {
    /// let yt = ytmapi_rs::YtMusic::from_cookie("FAKE COOKIE").await?;
    /// let query = ytmapi_rs::query::SearchQuery::new_filtered("Beatles", ytmapi_rs::query::search::ArtistsFilter);
    /// let result = yt.json_query(query).await?;
    /// println!("{:?}", result);
    /// # Ok::<(), ytmapi_rs::Error>(())
    /// # };
    /// ```
    pub async fn json_query<Q: Query<A>>(&self, query: impl Borrow<Q>) -> Result<Json> {
        Q::Method::call(query.borrow(), &self.client, &self.token)
            .await?
            .process()
            .map(|processed| processed.json)
    }
    /// Run a Query on the API returning its output.
    /// # Usage
    /// ```no_run
    /// # async {
    /// let yt = ytmapi_rs::YtMusic::from_cookie("").await?;
    /// let query = ytmapi_rs::query::SearchQuery::new_filtered("Beatles", ytmapi_rs::query::search::ArtistsFilter);
    /// let result = yt.query(query).await?;
    /// assert_eq!(result[0].artist, "The Beatles");
    /// # Ok::<(), ytmapi_rs::Error>(())
    /// # };
    /// ```
    pub async fn query<Q: Query<A>>(&self, query: impl Borrow<Q>) -> Result<Q::Output> {
        Q::Output::parse_from(
            Q::Method::call(query.borrow(), &self.client, &self.token)
                .await?
                .process()?,
        )
    }
    /// Stream a query that has 'continuations', i.e can continue to stream
    /// results.
    /// # Return type lifetime notes
    /// The returned `impl Stream` is tied to the lifetime of self, since it's
    /// self's client that will emit the results. It's also tied to the
    /// lifetime of query, but ideally it could take either owned or
    /// borrowed query.
    /// # Usage
    /// ```no_run
    /// use futures::stream::TryStreamExt;
    /// # async {
    /// let yt = ytmapi_rs::YtMusic::from_cookie("").await?;
    /// let query = ytmapi_rs::query::GetLibrarySongsQuery::default();
    /// let results = yt.stream(&query).try_collect::<Vec<_>>().await?;
    /// # Ok::<(), ytmapi_rs::Error>(())
    /// # };
    /// ```
    pub fn stream<'a, Q>(&'a self, query: &'a Q) -> impl Stream<Item = Result<Q::Output>> + 'a
    where
        Q: Query<A>,
        Q: PostQuery,
        Q::Output: ParseFromContinuable<Q>,
    {
        continuations::stream(query, &self.client, &self.token)
    }
    /// Return the source JSON from streaming a query that has 'continuations',
    /// i.e can continue to stream results.
    /// Note that the stream will stop if an error is detected (after returning
    /// the source string that produced the error).
    /// # Return type lifetime notes
    /// The returned `impl Stream` is tied to the lifetime of self, since it's
    /// self's client that will emit the results. It's also tied to the
    /// lifetime of query, but ideally it could take either owned or
    /// borrowed query.
    /// # Usage
    /// ```no_run
    /// use futures::stream::TryStreamExt;
    /// # async {
    /// let yt = ytmapi_rs::YtMusic::from_cookie("").await?;
    /// let query = ytmapi_rs::query::GetLibrarySongsQuery::default();
    /// let results = yt
    ///     .raw_json_stream(&query)
    ///     .try_collect::<Vec<String>>()
    ///     .await?;
    /// # Ok::<(), ytmapi_rs::Error>(())
    /// # };
    /// ```
    pub fn raw_json_stream<'a, Q>(&'a self, query: &'a Q) -> impl Stream<Item = Result<String>> + 'a
    where
        Q: Query<A>,
        Q: PostQuery,
        Q::Output: ParseFromContinuable<Q>,
    {
        continuations::raw_json_stream(query, &self.client, &self.token)
    }
}
/// Generates a Browser Token when given a browser cookie.
/// This requires a [`Client`] to run.
/// # Usage
/// ```no_run
/// # async {
/// let client = ytmapi_rs::Client::new().unwrap();
/// let cookie = "FAKE COOKIE";
/// let token = ytmapi_rs::generate_browser_token(&client, cookie).await;
/// assert!(matches!(
///     token.unwrap_err().into_kind(),
///     ytmapi_rs::error::ErrorKind::Header { .. }
/// ));
/// # };
/// ```
pub async fn generate_browser_token<S: AsRef<str>>(
    client: &Client,
    cookie: S,
) -> Result<BrowserToken> {
    let token = BrowserToken::from_str(cookie.as_ref(), client).await?;
    Ok(token)
}
/// Process a string of JSON as if it had been directly received from the
/// api for a query. Note that this is generic across AuthToken, and you may
/// need to provide the AuthToken type using 'turbofish'.
/// # Usage
/// ```
/// let json = r#"{ "test" : true }"#.to_string();
/// let query = ytmapi_rs::query::SearchQuery::new_filtered("Beatles", ytmapi_rs::query::search::ArtistsFilter);
/// let result = ytmapi_rs::process_json::<_, ytmapi_rs::auth::BrowserToken>(json, query);
/// assert!(result.is_err());
/// ```
pub fn process_json<Q: Query<A>, A: AuthToken>(
    json: String,
    query: impl Borrow<Q>,
) -> Result<Q::Output> {
    Q::Output::parse_from(RawResult::<Q, A>::from_raw(json, query.borrow()).process()?)
}
