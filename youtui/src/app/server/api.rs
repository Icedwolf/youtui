use crate::api::{DynamicApiError, DynamicYtMusic};
use crate::app::CALLBACK_CHANNEL_SIZE;
use crate::async_rodio_sink::send_or_error;
use crate::config::ApiKey;
use anyhow::{Error, Result};
use async_cell::sync::AsyncCell;
use futures::stream::FuturesUnordered;
use futures::{Stream, StreamExt};
use reqwest;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, trace, warn};
use ytmapi_rs::auth::BrowserToken;
use ytmapi_rs::common::{
    AlbumID, ArtistChannelID, PlaylistID, VideoID, YoutubeID,
};
use ytmapi_rs::parse::{
    AlbumSong, GetAlbum, GetArtist, GetArtistAlbums, GetArtistAlbumsAlbum, ParsedSongAlbum,
    ParsedSongArtist, PlaylistItem, SearchResultArtist, SearchResultPlaylist, SearchResultSong,
};
use ytmapi_rs::query::{GetAlbumQuery, GetArtistAlbumsQuery, GetArtistQuery, GetPlaylistTracksQuery};

#[derive(Clone)]
/// # Note
/// Since the underlying API is wrapped in an Arc, it's cheap to clone this
/// type.
pub struct Api {
    api: Arc<AsyncCell<Result<ConcurrentApi, DynamicApiError>>>,
}
pub type ConcurrentApi = Arc<RwLock<DynamicYtMusic>>;

impl Api {
    #[must_use]
    pub fn new(api_key: ApiKey, client: reqwest::Client) -> Api {
        let api = AsyncCell::new().into_shared();
        let api_clone = api.clone();
        // fire-and-forget: API cell receives result; callers await via api.get().await
        tokio::spawn(async move {
            let api = DynamicYtMusic::new(api_key, client)
                .await
                .map(|api| Arc::new(RwLock::new(api)));
            api_clone.set(api)
        });
        Api { api }
    }
    // NOTE: Situation where user has tried to create API from an expired OAuth
    // token is not currently handled.
    pub async fn get_api(&self) -> Result<ConcurrentApi, DynamicApiError> {
        // Note that the error, if it exists, is cloned here.
        self.api.get().await
    }
    pub async fn search_playlists(&self, text: String) -> Result<Vec<SearchResultPlaylist>> {
        search_playlists(self.get_api().await?, text).await
    }
    pub async fn search_artists(&self, text: String) -> Result<Vec<SearchResultArtist>> {
        search_artists(self.get_api().await?, text).await
    }
    pub async fn search_songs(&self, text: String) -> Result<Vec<SearchResultSong>> {
        search_songs(self.get_api().await?, text).await
    }
    pub fn get_playlist_songs(
        &self,
        playlist_id: PlaylistID<'static>,
    ) -> impl Stream<Item = GetPlaylistSongsProgressUpdate> + 'static + use<> {
        let api = self.api.clone();
        get_playlist_songs(api, playlist_id)
    }
    pub fn get_artist_songs(
        &self,
        browse_id: ArtistChannelID<'static>,
    ) -> impl Stream<Item = GetArtistSongsProgressUpdate> + 'static + use<> {
        let api = self.api.clone();
        get_artist_songs(api, browse_id)
    }
}

fn resolve_omv_crossref<'a>(
    album_songs: &mut [AlbumSong],
    lookup: impl Fn(&str) -> Option<&'a str>,
    prefix: &str,
) {
    let mut corrected = 0u32;
    let mut searched = 0u32;
    for song in album_songs.iter_mut().filter(|s| !s.is_audio_track()) {
        searched += 1;
        if searched <= 3 {
            debug!(
                "{prefix}: checking title={:?} video_id={:?}",
                song.title,
                song.video_id.get_raw()
            );
        }
        if let Some(new_id) = lookup(&song.title) {
            debug!(
                "{prefix}: CORRECTED title={:?} old_id={:?} new_id={:?}",
                song.title,
                song.video_id.get_raw(),
                new_id
            );
            song.video_id = VideoID::from_raw(new_id.to_string());
            corrected += 1;
        }
    }
    if corrected > 0 {
        debug!("{prefix}: corrected {corrected}/{searched} non-Atv tracks");
    } else if searched > 0 {
        debug!("{prefix}: 0/{searched} corrected — no title matches");
    }
}

fn resolve_omv_with_audio_playlist(album_songs: &mut [AlbumSong], playlist_items: &[PlaylistItem]) {
    let mut atv_map: HashMap<&str, &str> = HashMap::new();
    for item in playlist_items.iter().filter_map(|item| match item {
        PlaylistItem::Song(s) => Some((s.title.as_str(), s.video_id.get_raw())),
        _ => None,
    }) {
        match atv_map.entry(item.0) {
            hash_map::Entry::Occupied(e) => {
                debug!(
                    "Audio playlist duplicate title {:?}: first_video={:?} second_video={:?}",
                    e.key(),
                    e.get(),
                    item.1
                );
            }
            hash_map::Entry::Vacant(e) => {
                e.insert(item.1);
            }
        }
    }
    resolve_omv_crossref(
        album_songs,
        |title| atv_map.get(title).copied(),
        "audio_playlist",
    );
}

fn resolve_omv_album_songs_with_search(
    album_songs: &mut [AlbumSong],
    search_map: &HashMap<String, VideoID<'static>>,
) {
    resolve_omv_crossref(
        album_songs,
        |title| search_map.get(title).map(|v| v.get_raw()),
        "resolve_omv",
    );
}

/// Run a query with a single retry on error.
pub async fn query_api_with_retry<Q, O>(api: &ConcurrentApi, query: impl Borrow<Q>) -> Result<O>
where
    Q: ytmapi_rs::query::Query<BrowserToken, Output = O>,
{
    let res = api
        .read()
        .await
        .query_browser_or_oauth::<Q, O>(query.borrow())
        .await;
    match res {
        Ok(r) => Ok(r),
        Err(e) => {
            trace!("Got error {e} from api");
            match e.downcast::<ytmapi_rs::Error>() {
                Ok(_) => {
                    info!("Retrying once");
                    Ok(api.read().await.query_browser_or_oauth(query).await?)
                }
                Err(e) => Err(e),
            }
        }
    }
}

async fn search_playlists(api: ConcurrentApi, text: String) -> Result<Vec<SearchResultPlaylist>> {
    debug!("Searching playlists for {text}");
    let query = ytmapi_rs::query::SearchQuery::new_filtered(
        text,
        ytmapi_rs::query::search::PlaylistsFilter,
    )
    .with_spelling_mode(ytmapi_rs::query::search::SpellingMode::ExactMatch);
    query_api_with_retry(&api, query).await
}

fn resolve_channel_id_from_search(s: &str) -> Option<ArtistChannelID<'static>> {
    let raw = s.trim();
    if raw.starts_with("UC")
        && raw.len() == 24
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Some(ArtistChannelID::from_raw(raw.to_string()));
    }
    for prefix in &[
        "https://music.youtube.com/channel/",
        "https://www.youtube.com/channel/",
        "music.youtube.com/channel/",
        "youtube.com/channel/",
    ] {
        if let Some(id) = raw.strip_prefix(prefix) {
            let id = id.split('/').next().unwrap_or(id);
            if id.starts_with("UC") && id.len() >= 20 {
                return Some(ArtistChannelID::from_raw(id.to_string()));
            }
        }
    }
    None
}

async fn search_artists(api: ConcurrentApi, text: String) -> Result<Vec<SearchResultArtist>> {
    debug!("Searching artists for {text}");
    if let Some(channel_id) = resolve_channel_id_from_search(&text) {
        match query_api_with_retry::<GetArtistQuery<'_>, GetArtist>(&api, GetArtistQuery::new(channel_id)).await {
            Ok(artist) => {
                debug!("Resolved channel ID to artist: {}", artist.name);
                return Ok(vec![SearchResultArtist::new(
                    artist.name,
                    artist.subscribers,
                    artist.channel_id,
                )]);
            }
            Err(e) => debug!("Channel ID lookup failed: {e}, falling back to text search"),
        }
    }
    let exact_query =
        ytmapi_rs::query::SearchQuery::new_filtered(text.clone(), ytmapi_rs::query::search::ArtistsFilter)
            .with_spelling_mode(ytmapi_rs::query::search::SpellingMode::WithSuggestions);
    let results: Vec<SearchResultArtist> = match query_api_with_retry(&api, exact_query).await {
        Ok(r) => r,
        Err(e) => {
            debug!("Filtered artist search error: {e}, falling back to basic search");
            Vec::new()
        }
    };
    if !results.is_empty() {
        return Ok(results);
    }
    debug!("Filtered artist search returned empty, trying basic search");
    let basic: ytmapi_rs::parse::SearchResults = match query_api_with_retry(
        &api,
        Into::<ytmapi_rs::query::SearchQuery<'_, ytmapi_rs::query::search::BasicSearch>>::into(text),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("Basic artist search also failed: {e}");
            return Ok(Vec::new());
        }
    };
    if !basic.artists.is_empty() {
        debug!("Falling back to {} artist results from basic search", basic.artists.len());
        return Ok(basic.artists);
    }
    debug!("No artist results found in basic search either");
    Ok(Vec::new())
}

async fn search_songs(api: ConcurrentApi, text: String) -> Result<Vec<SearchResultSong>> {
    debug!("Searching songs for {text}");
    let query =
        ytmapi_rs::query::SearchQuery::new_filtered(text, ytmapi_rs::query::search::SongsFilter)
            .with_spelling_mode(ytmapi_rs::query::search::SpellingMode::ExactMatch);
    query_api_with_retry(&api, query).await
}

#[derive(Debug, Clone)]
pub struct AlbumSongsData {
    pub song_list: Vec<AlbumSong>,
    pub album: ParsedSongAlbum,
    pub year: String,
    pub artists: Vec<ParsedSongArtist>,
}

enum PerAlbumResult {
    Success(usize, AlbumSongsData),
    Error {
        album_id: AlbumID<'static>,
        error: Error,
    },
}

async fn fetch_and_resolve_album(
    api: ConcurrentApi,
    idx: usize,
    a_id: AlbumID<'static>,
    search_map: Option<Arc<HashMap<String, VideoID<'static>>>>,
) -> PerAlbumResult {
    let query = GetAlbumQuery::new(&a_id);
    let album = query_api_with_retry(&api, query).await;
    let album = match album {
        Ok(a) => a,
        Err(e) => {
            return PerAlbumResult::Error {
                album_id: a_id,
                error: e,
            };
        }
    };
    let GetAlbum {
        title,
        artists,
        year,
        mut tracks,
        audio_playlist_id,
        ..
    } = album;
    if let Some(ref search_map) = search_map
        && !search_map.is_empty()
    {
        debug!(
            "artist_songs: cross-referencing {} tracks for album {:?}",
            tracks.len(),
            title
        );
        resolve_omv_album_songs_with_search(&mut tracks, search_map);
    }
    if let Some(ref ap_id) = audio_playlist_id {
        let query = ytmapi_rs::query::GetPlaylistTracksQuery::new(ap_id.clone());
        match query_api_with_retry::<_, Vec<PlaylistItem>>(&api, query).await {
            Ok(items) => {
                resolve_omv_with_audio_playlist(&mut tracks, &items);
            }
            Err(e) => {
                warn!(
                    "artist_songs: audio_playlist fetch failed for {:?}: {e}",
                    title
                );
            }
        }
    }
    PerAlbumResult::Success(
        idx,
        AlbumSongsData {
            song_list: tracks,
            album: ParsedSongAlbum {
                name: title,
                id: a_id,
            },
            year,
            artists,
        },
    )
}

pub enum GetArtistSongsProgressUpdate {
    Loading,
    // Caller should know the ArtistChannelID already, as they provided it.
    // Stream closes here.
    GetArtistAlbumsError(Error),
    // Stream doesn't close here - maybe some of the other albums were succesfully able to send
    // songs.
    GetAlbumsSongsError {
        album_id: AlbumID<'static>,
        error: Error,
    },
    /// Sent incrementally as each album finishes processing.
    /// UI uses this to show progress (e.g. "Loading album 3/15").
    AlbumProgress {
        current: usize,
        total: usize,
    },
    // Stream closes here.
    AllAlbumsSongs(Vec<AlbumSongsData>),
    // Stream closes here.
    AllSongsSent,
    // Stream closes here.
    NoSongsFound,
}

fn get_artist_songs(
    api: Arc<AsyncCell<Result<ConcurrentApi, DynamicApiError>>>,
    browse_id: ArtistChannelID<'static>,
) -> impl Stream<Item = GetArtistSongsProgressUpdate> + 'static {
    let (tx, rx) = tokio::sync::mpsc::channel(CALLBACK_CHANNEL_SIZE);
    let _handle = tokio::spawn(async move {
        debug!("Running songs query");
        if tx.try_send(GetArtistSongsProgressUpdate::Loading).is_err() {
            debug!("artist_songs: Loading signal dropped (channel full or closed)");
        }
        let api = match api.get().await {
            Err(e) => {
                warn!("Error getting API");
                send_or_error(
                    tx,
                    GetArtistSongsProgressUpdate::GetArtistAlbumsError(e.into()),
                )
                .await;
                return;
            }
            Ok(api) => api,
        };
        let query = ytmapi_rs::query::GetArtistQuery::new(&browse_id);
        let artist = query_api_with_retry(&api, query).await;
        let artist = match artist {
            Ok(a) => a,
            Err(e) => {
                warn!(error = %e, "GetArtistQuery failed");
                send_or_error(tx, GetArtistSongsProgressUpdate::GetArtistAlbumsError(e)).await;
                return;
            }
        };
        let artist_name = artist.name.clone();
        let mut browse_id_list: Vec<AlbumID<'static>> = Vec::new();
        let mut seen_ids: HashSet<AlbumID<'static>> = HashSet::new();
        // Spawn search_songs early — it only needs artist_name and can run
        // concurrently with the paginated album ID collection below.
        let search_fut = {
            let api = api.clone();
            tokio::spawn(async move { search_songs(api, artist_name).await })
        };

        for albums in artist
            .top_releases
            .albums
            .into_iter()
            .chain(artist.top_releases.singles.into_iter())
        {
            let GetArtistAlbums {
                browse_id,
                params,
                results,
                ..
            } = albums;
            let ids: Vec<AlbumID<'static>> = match (browse_id, params) {
                (None, None) if !results.is_empty() => {
                    results.into_iter().map(|r| r.album_id).collect()
                }
                (None, _) | (_, None) => Vec::new(),
                (Some(browse_id), Some(params)) => {
                    let query = GetArtistAlbumsQuery::new(browse_id, params);
                    let album_pages_result = {
                        let api_locked = api.read().await;
                        api_locked
                            .stream_browser_or_oauth::<
                                GetArtistAlbumsQuery<'_>,
                                Vec<GetArtistAlbumsAlbum>,
                            >(&query, usize::MAX)
                            .await
                    };
                    match album_pages_result {
                        Ok(r) => r.into_iter().flatten().map(|a| a.browse_id).collect(),
                        Err(e) => {
                            warn!(
                                "get_artist_albums continuation failed (\"{}\"), falling back to first page only",
                                e
                            );
                            send_or_error(
                                &tx,
                                GetArtistSongsProgressUpdate::GetArtistAlbumsError(e),
                            )
                            .await;
                            results.into_iter().map(|r| r.album_id).collect()
                        }
                    }
                }
            };
            for id in ids {
                if seen_ids.insert(id.clone()) {
                    browse_id_list.push(id);
                }
            }
        }

        let top_songs_data: Option<AlbumSongsData> = match artist.top_releases.songs {
            None => None,
            Some(songs) => {
                let first_page = songs.results;
                let browse_id = songs.browse_id;
                let tracks: Vec<AlbumSong> = if browse_id.get_raw().is_empty() {
                    first_page.into_iter().map(AlbumSong::from).collect()
                } else {
                    let query = GetPlaylistTracksQuery::new(browse_id);
                    let items = {
                        let api_locked = api.read().await;
                        api_locked
                            .stream_browser_or_oauth::<GetPlaylistTracksQuery<'_>, Vec<PlaylistItem>>(
                                &query, usize::MAX,
                            )
                            .await
                    };
                    match items {
                        Ok(all_pages) => all_pages
                            .into_iter()
                            .flatten()
                            .filter_map(|item| match item {
                                PlaylistItem::Song(s) => Some(AlbumSong::from(s)),
                                _ => None,
                            })
                            .collect(),
                        Err(e) => {
                            warn!("top songs pagination failed: {e}, falling back to first page");
                            first_page.into_iter().map(AlbumSong::from).collect()
                        }
                    }
                };
                Some(AlbumSongsData {
                    song_list: tracks,
                    album: ParsedSongAlbum {
                        name: "Top Songs".to_string(),
                        id: AlbumID::from_raw(""),
                    },
                    year: String::new(),
                    artists: vec![ParsedSongArtist {
                        name: artist.name.clone(),
                        id: None,
                    }],
                })
            }
        };

        if browse_id_list.is_empty() && top_songs_data.is_none() {
            info!("No songs found for artist");
            send_or_error(&tx, GetArtistSongsProgressUpdate::NoSongsFound).await;
            return;
        }
        // Await the search results that have been running concurrently with
        // the album ID collection above.
        let search_results = match search_fut.await {
            Ok(Ok(results)) => {
                debug!(
                    "artist_songs: search_songs returned {} results ({} Atv)",
                    results.len(),
                    results.iter().filter(|s| s.is_audio_track()).count()
                );
                Some(results)
            }
            Ok(Err(e)) => {
                warn!("artist_songs: search_songs failed: {e}");
                None
            }
            Err(join_err) => {
                warn!("artist_songs: search_songs task panicked: {join_err}");
                None
            }
        };
        // Build the Atv search map once before the album loop, not once per album.
        let search_map: Option<Arc<HashMap<String, VideoID<'static>>>> = search_results
            .as_ref()
            .map(|r| Arc::new(build_search_map(r)));
        let total_albums = browse_id_list.len();
        // Request all albums concurrently, running each album's audio-playlist
        // fetch inside the same future so all N playlist fetches overlap.
        let mut stream: FuturesUnordered<_> = browse_id_list
            .into_iter()
            .enumerate()
            .inspect(|(_, a_id)| {
                debug!("Spawning request for caller tracks for album ID {:?}", a_id)
            })
            .map(move |(idx, a_id)| {
                let api = api.clone();
                let search_map = search_map.clone();
                fetch_and_resolve_album(api, idx, a_id, search_map)
            })
            .collect();
        // Batch: collect all albums and send once when ready.
        let mut album_results: Vec<(usize, AlbumSongsData)> = Vec::new();
        let mut processed = 0usize;
        while let Some(result) = stream.next().await {
            match result {
                PerAlbumResult::Success(idx, data) => album_results.push((idx, data)),
                PerAlbumResult::Error {
                    album_id, error, ..
                } => {
                    warn!("Error with GetAlbumQuery, album {:?}", album_id);
                    send_or_error(
                        &tx,
                        GetArtistSongsProgressUpdate::GetAlbumsSongsError { album_id, error },
                    )
                    .await;
                }
            }
            processed += 1;
            if tx
                .try_send(GetArtistSongsProgressUpdate::AlbumProgress {
                    current: processed,
                    total: total_albums,
                })
                .is_err()
            {
                // Channel full or closed — progress hint is best-effort.
            }
        }
        debug!(
            "Sending {} albums for artist {:?}",
            album_results.len(),
            browse_id
        );
        // Reorder by original index so albums appear in the same order as
        // browse_id_list, regardless of completion order.
        album_results.sort_by_key(|(idx, _)| *idx);
        let mut all_albums: Vec<AlbumSongsData> = album_results.into_iter().map(|(_, a)| a).collect();
        if let Some(top) = top_songs_data {
            all_albums.insert(0, top);
        }
        if all_albums.is_empty() {
            // All album fetches failed — send NoSongsFound so the UI shows
            // an error/empty state instead of a blank loaded list.
            send_or_error(tx, GetArtistSongsProgressUpdate::NoSongsFound).await;
            return;
        }
        send_or_error(
            &tx,
            GetArtistSongsProgressUpdate::AllAlbumsSongs(all_albums),
        )
        .await;
        send_or_error(tx, GetArtistSongsProgressUpdate::AllSongsSent).await;
    });
    ReceiverStream::new(rx)
}

pub enum GetPlaylistSongsProgressUpdate {
    Loading,
    Songs(Vec<PlaylistItem>),
    // PlaylistID is returned to allow caller to reuse allocation if required.
    // May occur before or after sending some songs, ie api could fail straight away or stream
    // some songs then fail. Stream closes here.
    GetPlaylistSongsError {
        playlist_id: PlaylistID<'static>,
        error: Error,
    },
    // Stream closes here.
    AllSongsSent,
}

fn get_playlist_songs(
    api: Arc<AsyncCell<Result<ConcurrentApi, DynamicApiError>>>,
    playlist_id: PlaylistID<'static>,
) -> impl Stream<Item = GetPlaylistSongsProgressUpdate> + 'static {
    let (tx, rx) = tokio::sync::mpsc::channel(CALLBACK_CHANNEL_SIZE);
    let _handle = tokio::spawn(async move {
        debug!("Running songs query");
        if tx
            .try_send(GetPlaylistSongsProgressUpdate::Loading)
            .is_err()
        {
            debug!("playlist_songs: Loading signal dropped (channel full or closed)");
        }
        let api = match api.get().await {
            Err(e) => {
                warn!("Error getting API");
                send_or_error(
                    tx,
                    GetPlaylistSongsProgressUpdate::GetPlaylistSongsError {
                        playlist_id,
                        error: e.into(),
                    },
                )
                .await;
                return;
            }
            Ok(api) => api,
        };
        let query = ytmapi_rs::query::GetPlaylistTracksQuery::new((&playlist_id).into());
        // TODO: Streaming
        let first_tracks = query_api_with_retry(&api, query).await;
        match first_tracks {
            Ok(t) => {
                debug!("Sending caller tracks for {:?}", playlist_id);
                send_or_error(&tx, GetPlaylistSongsProgressUpdate::Songs(t)).await;
            }
            Err(error) => {
                warn!("Error with GetPlaylistTracksQuery");
                send_or_error(
                    &tx,
                    GetPlaylistSongsProgressUpdate::GetPlaylistSongsError { playlist_id, error },
                )
                .await;
                return;
            }
        }
        send_or_error(tx, GetPlaylistSongsProgressUpdate::AllSongsSent).await;
    });
    ReceiverStream::new(rx)
}

/// Build a title → video_id map from audio-only search results.
/// Used to cross-reference Omv album tracks with their Atv equivalents.
/// Owned keys/values so the map can be `Arc`-shared across concurrent futures.
/// Warns on duplicate titles — the first entry wins but we surface the
/// collision for debugging.
fn build_search_map(search_results: &[SearchResultSong]) -> HashMap<String, VideoID<'static>> {
    let mut map: HashMap<String, VideoID<'static>> = HashMap::new();
    for s in search_results.iter().filter(|s| s.is_audio_track()) {
        match map.entry(s.title.clone()) {
            hash_map::Entry::Occupied(e) => {
                debug!(
                    "Atv search duplicate title {:?}: first={:?} second={:?}",
                    e.key(),
                    e.get().get_raw(),
                    s.video_id.get_raw()
                );
            }
            hash_map::Entry::Vacant(e) => {
                e.insert(VideoID::from_raw(s.video_id.get_raw().to_string()));
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytmapi_rs::auth::BrowserToken;
    use ytmapi_rs::auth::noauth::NoAuthToken;
    use ytmapi_rs::parse::GetAlbum;
    use ytmapi_rs::query::GetAlbumQuery;
    use ytmapi_rs::query::search::{SearchQuery, SongsFilter};

    fn make_playlist_item(title: &str, video_id: &str) -> PlaylistItem {
        serde_json::from_value(serde_json::json!({
            "Song": {
                "video_id": video_id,
                "track_no": 1,
                "album": { "name": "", "id": "" },
                "duration": "3:00",
                "library_management": null,
                "title": title,
                "artists": [],
                "like_status": "INDIFFERENT",
                "thumbnails": [],
                "explicit": "NotExplicit",
                "is_available": true,
                "playlist_id": "",
            }
        }))
        .expect("make_playlist_item should produce a valid PlaylistItem")
    }

    fn parse_album_fixture() -> Vec<AlbumSong> {
        let json =
            include_str!("../../../../ytmapi-rs/test_json/get_album_not_signed_in_20250611.json");
        let GetAlbum { tracks, .. } = ytmapi_rs::process_json::<_, NoAuthToken>(
            json.to_owned(),
            GetAlbumQuery::new(AlbumID::from_raw("")),
        )
        .expect("album fixture should parse");
        tracks
    }

    fn parse_search_fixture() -> Vec<SearchResultSong> {
        let json = include_str!("../../../../ytmapi-rs/test_json/search_songs_20231226.json");
        ytmapi_rs::process_json::<_, BrowserToken>(
            json.to_owned(),
            SearchQuery::new_filtered("", SongsFilter),
        )
        .expect("search fixture should parse")
    }

    #[test]
    fn test_resolve_omv_replaces_matching_atv_video_id() {
        let mut album_songs = parse_album_fixture();
        let search_songs = parse_search_fixture();

        let original_ids: Vec<String> = album_songs
            .iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect();

        // Simulate Yaeji scenario: set all to non-Atv (function treats
        // Omv/Ugc/None identically — anything that isn't Atv is a candidate)
        for song in &mut album_songs {
            // music_video_type is pub on AlbumSong; YoutubeMusicVideoType
            // is not re-exported, but we only need the song to not be
            // `is_audio_track()` — setting None achieves the same effect.
            song.music_video_type = None;
        }

        let atv_song = search_songs
            .iter()
            .find(|s| s.is_audio_track())
            .expect("search fixture should contain Atv track");
        let expected_id = atv_song.video_id.get_raw().to_string();
        album_songs[0].title.clone_from(&atv_song.title);

        let search_map = build_search_map(&search_songs);
        resolve_omv_album_songs_with_search(&mut album_songs, &search_map);

        assert_eq!(
            album_songs[0].video_id.get_raw(),
            expected_id,
            "matching Atv search result should replace video_id"
        );
        for (i, song) in album_songs.iter().enumerate().skip(1) {
            assert_eq!(
                song.video_id.get_raw(),
                original_ids[i].as_str(),
                "index {i}: no match, video_id unchanged"
            );
        }
    }

    #[test]
    fn test_resolve_omv_empty_search_noop() {
        // Clone from parsed data to avoid constructing AlbumSong directly
        let source = parse_album_fixture();
        let mut song = source[0].clone();
        song.title = "Solo Test Track".into();
        let original = song.video_id.get_raw().to_string();
        let empty_map: HashMap<String, VideoID<'static>> = HashMap::new();
        resolve_omv_album_songs_with_search(std::slice::from_mut(&mut song), &empty_map);
        assert_eq!(
            song.video_id.get_raw(),
            original,
            "empty search results = no change"
        );
    }

    #[test]
    fn test_resolve_omv_empty_album_noop() {
        let mut songs: Vec<AlbumSong> = vec![];
        let empty_map: HashMap<String, VideoID<'static>> = HashMap::new();
        resolve_omv_album_songs_with_search(&mut songs, &empty_map);
        assert!(songs.is_empty());
    }

    #[test]
    fn test_resolve_omv_album_tracks_without_matching_songs_untouched() {
        let mut album_songs = parse_album_fixture();
        let search_songs = parse_search_fixture();

        let original_ids: Vec<String> = album_songs
            .iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect();

        let search_map = build_search_map(&search_songs);
        resolve_omv_album_songs_with_search(&mut album_songs, &search_map);

        // No title overlap between the two fixtures → no video_id changes
        for (i, song) in album_songs.iter().enumerate() {
            assert_eq!(
                song.video_id.get_raw(),
                original_ids[i].as_str(),
                "index {i}: no title overlap, video_id unchanged"
            );
        }
    }

    // --- resolve_omv_with_audio_playlist ---

    #[test]
    fn test_audio_playlist_replaces_matching_atv_video_id() {
        let mut album_songs = parse_album_fixture();
        let original_ids: Vec<String> = album_songs
            .iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect();

        // Simulate Yaeji scenario: set all to non-Atv
        for song in &mut album_songs {
            song.music_video_type = None;
        }

        // Build an audio playlist that matches the first song title with a new ID
        let corrected_id = "NEW_ATV_ID";
        let playlist_items = vec![make_playlist_item(
            album_songs[0].title.as_str(),
            corrected_id,
        )];

        resolve_omv_with_audio_playlist(&mut album_songs, &playlist_items);

        assert_eq!(
            album_songs[0].video_id.get_raw(),
            corrected_id,
            "matching playlist title should replace video_id"
        );
        for (i, song) in album_songs.iter().enumerate().skip(1) {
            assert_eq!(
                song.video_id.get_raw(),
                original_ids[i].as_str(),
                "index {i}: no match, video_id unchanged"
            );
        }
    }

    #[test]
    fn test_audio_playlist_multiple_matches() {
        let mut album_songs = parse_album_fixture();
        let original_ids: Vec<String> = album_songs
            .iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect();

        for song in &mut album_songs {
            song.music_video_type = None;
        }

        // Match first two songs with new Atv IDs
        let playlist_items = vec![
            make_playlist_item(album_songs[0].title.as_str(), "ATV_ID_0"),
            make_playlist_item(album_songs[1].title.as_str(), "ATV_ID_1"),
        ];

        resolve_omv_with_audio_playlist(&mut album_songs, &playlist_items);

        assert_eq!(album_songs[0].video_id.get_raw(), "ATV_ID_0");
        assert_eq!(album_songs[1].video_id.get_raw(), "ATV_ID_1");
        for (i, song) in album_songs.iter().enumerate().skip(2) {
            assert_eq!(
                song.video_id.get_raw(),
                original_ids[i].as_str(),
                "index {i}: no match, video_id unchanged"
            );
        }
    }

    #[test]
    fn test_audio_playlist_no_matches_noop() {
        let mut album_songs = parse_album_fixture();
        let original_ids: Vec<String> = album_songs
            .iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect();

        for song in &mut album_songs {
            song.music_video_type = None;
        }

        // Playlist has items but titles don't overlap
        let playlist_items = vec![make_playlist_item("Completely Different Title", "DIFF_ID")];

        resolve_omv_with_audio_playlist(&mut album_songs, &playlist_items);

        for (i, song) in album_songs.iter().enumerate() {
            assert_eq!(
                song.video_id.get_raw(),
                original_ids[i].as_str(),
                "index {i}: no title match, unchanged"
            );
        }
    }

    #[test]
    fn test_audio_playlist_empty_playlist_noop() {
        let mut album_songs = parse_album_fixture();
        let original_ids: Vec<String> = album_songs
            .iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect();

        for song in &mut album_songs {
            song.music_video_type = None;
        }

        resolve_omv_with_audio_playlist(&mut album_songs, &[]);

        for (i, song) in album_songs.iter().enumerate() {
            assert_eq!(
                song.video_id.get_raw(),
                original_ids[i].as_str(),
                "index {i}: empty playlist, unchanged"
            );
        }
    }

    #[test]
    fn test_audio_playlist_empty_album_noop() {
        let mut songs: Vec<AlbumSong> = vec![];
        resolve_omv_with_audio_playlist(&mut songs, &[]);
        assert!(songs.is_empty());
    }

    #[test]
    fn test_audio_playlist_already_atv_untouched() {
        let mut album_songs = parse_album_fixture();
        let original_ids: Vec<String> = album_songs
            .iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect();

        // Don't set music_video_type to None — keep original Atv/Omv as-is.
        // Only non-Atv tracks are candidates for correction (filter in the function).
        let playlist_items = vec![make_playlist_item(
            album_songs[0].title.as_str(),
            "WOULD_BE_CORRECTED",
        )];

        resolve_omv_with_audio_playlist(&mut album_songs, &playlist_items);

        // If track was already Atv it should be skipped even if title matches
        if album_songs[0].is_audio_track() {
            assert_eq!(
                album_songs[0].video_id.get_raw(),
                original_ids[0].as_str(),
                "already Atv: no correction"
            );
        }
    }

    #[test]
    fn test_audio_playlist_unicode_title_matches() {
        let mut album_songs = parse_album_fixture();
        for song in &mut album_songs {
            song.music_video_type = None;
        }
        let original_ids: Vec<String> = album_songs
            .iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect();

        // Assign a Korean title to the first track
        let korean_title = "WHAT WE DREW 우리가 그려왔던";
        album_songs[0].title = korean_title.to_string();

        let playlist_items = vec![make_playlist_item(korean_title, "KOREAN_ATV_ID")];
        resolve_omv_with_audio_playlist(&mut album_songs, &playlist_items);

        assert_eq!(
            album_songs[0].video_id.get_raw(),
            "KOREAN_ATV_ID",
            "Korean title should match via exact HashMap lookup"
        );
        for (i, song) in album_songs.iter().enumerate().skip(1) {
            assert_eq!(
                song.video_id.get_raw(),
                original_ids[i].as_str(),
                "index {i}: no match, video_id unchanged"
            );
        }
    }

    #[test]
    fn test_audio_playlist_duplicate_title_first_wins() {
        let mut album_songs = parse_album_fixture();
        for song in &mut album_songs {
            song.music_video_type = None;
        }

        // Two playlist items with the same title but different video IDs.
        // We use Entry::Vacant so the first item wins — the second logs a
        // warning and is ignored.
        let shared_title = album_songs[0].title.clone();
        let playlist_items = vec![
            make_playlist_item(&shared_title, "FIRST_ID"),
            make_playlist_item(&shared_title, "SECOND_ID"),
        ];

        resolve_omv_with_audio_playlist(&mut album_songs, &playlist_items);

        assert_eq!(
            album_songs[0].video_id.get_raw(),
            "FIRST_ID",
            "duplicate title: first playlist item should win"
        );
    }

    #[test]
    fn test_audio_playlist_empty_title_in_playlist() {
        let mut album_songs = parse_album_fixture();
        for song in &mut album_songs {
            song.music_video_type = None;
            song.title.clear();
        }

        // Playlist item with empty title
        let playlist_items = vec![make_playlist_item("", "EMPTY_TITLE_ID")];
        resolve_omv_with_audio_playlist(&mut album_songs, &playlist_items);

        // All album songs have empty titles → all should match the empty-title playlist item
        for song in &album_songs {
            assert_eq!(
                song.video_id.get_raw(),
                "EMPTY_TITLE_ID",
                "empty title should match empty-title playlist item"
            );
        }
    }

    #[test]
    fn test_resolve_omv_all_atv_untouched() {
        let src = parse_album_fixture();
        let mut json = serde_json::to_value(&src).unwrap();
        for song in json.as_array_mut().unwrap() {
            song["music_video_type"] = serde_json::Value::String("MUSIC_VIDEO_TYPE_ATV".into());
        }
        let mut album_songs: Vec<AlbumSong> = serde_json::from_value(json).unwrap();
        let original_ids: Vec<String> = album_songs
            .iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect();

        // All songs are now Atv → filter in resolve_omv_crossref skips everything
        let empty_map: HashMap<String, VideoID<'static>> = HashMap::new();
        resolve_omv_album_songs_with_search(&mut album_songs, &empty_map);

        for (i, song) in album_songs.iter().enumerate() {
            assert_eq!(
                song.video_id.get_raw(),
                original_ids[i].as_str(),
                "index {i}: all Atv, video_id unchanged"
            );
        }
    }

    #[test]
    fn test_build_search_map_non_atv_filtered() {
        let songs: Vec<SearchResultSong> = vec![
            serde_json::from_value(serde_json::json!({
                "title": "Ugc Title",
                "artist": "Test",
                "album": null,
                "duration": "3:00",
                "plays": "100",
                "explicit": "NotExplicit",
                "video_id": "ugc_id",
                "thumbnails": [],
                "music_video_type": "MUSIC_VIDEO_TYPE_UGC",
            }))
            .unwrap(),
            serde_json::from_value(serde_json::json!({
                "title": "Omv Title",
                "artist": "Test",
                "album": null,
                "duration": "3:00",
                "plays": "200",
                "explicit": "NotExplicit",
                "video_id": "omv_id",
                "thumbnails": [],
                "music_video_type": "MUSIC_VIDEO_TYPE_OMV",
            }))
            .unwrap(),
        ];

        let map = build_search_map(&songs);
        assert!(map.is_empty(), "non-Atv results should be filtered out");
    }

    #[test]
    fn test_build_search_map_duplicate_atv_title() {
        let songs: Vec<SearchResultSong> = vec![
            serde_json::from_value(serde_json::json!({
                "title": "Same Title",
                "artist": "Artist A",
                "album": null,
                "duration": "3:00",
                "plays": "100",
                "explicit": "NotExplicit",
                "video_id": "first_id",
                "thumbnails": [],
                "music_video_type": "MUSIC_VIDEO_TYPE_ATV",
            }))
            .unwrap(),
            serde_json::from_value(serde_json::json!({
                "title": "Same Title",
                "artist": "Artist B",
                "album": null,
                "duration": "4:00",
                "plays": "200",
                "explicit": "NotExplicit",
                "video_id": "second_id",
                "thumbnails": [],
                "music_video_type": "MUSIC_VIDEO_TYPE_ATV",
            }))
            .unwrap(),
        ];

        let map = build_search_map(&songs);
        assert_eq!(map.len(), 1, "duplicate Atv title: only one entry");
        assert_eq!(
            map.get("Same Title").unwrap().get_raw(),
            "first_id",
            "duplicate title: first Atv result should win"
        );
    }
}
