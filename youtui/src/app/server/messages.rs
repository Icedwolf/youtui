use super::ArcServer;
use super::api::{
    GetArtistSongsProgressUpdate, GetPlaylistSongsProgressUpdate, resolve_to_audio_track,
};
use super::song_downloader;
use crate::app::structures::ListSongID;
use crate::async_rodio_sink::{
    AllStopped, AutoplayUpdate, PausePlayResponse, Paused, PlayUpdate, ProgressUpdate, Resumed,
    SeekDirection, Stopped, VolumeUpdate,
};
use anyhow::{Error, Result};
use async_callback_manager::{BackendStreamingTask, BackendTask};
use futures::{Future, Stream};
use rodio::Source;
use std::time::Duration;
use tokio_stream::wrappers::ReceiverStream;
use tracing::debug;

use ytmapi_rs::common::{ArtistChannelID, PlaylistID, SearchSuggestion, VideoID, YoutubeID};
use ytmapi_rs::parse::{SearchResultArtist, SearchResultPlaylist, SearchResultSong};

#[derive(PartialEq, Debug)]
pub enum TaskMetadata {
    PlayingSong,
    PlayPause,
}

#[derive(Debug)]
pub struct HandleApiError {
    pub error: Error,
    pub message: String,
}

impl BackendTask<ArcServer> for HandleApiError {
    type Output = ();
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let Self { error, message } = self;
        let backend = backend.clone();
        async move {
            backend.api_error_handler.handle_error(error, message).await;
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct GetSearchSuggestions(pub String);
impl BackendTask<ArcServer> for GetSearchSuggestions {
    type Output = Result<(Vec<SearchSuggestion>, String)>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.api.get_search_suggestions(self.0).await }
    }
}
#[derive(Debug, PartialEq)]
pub struct SearchArtists(pub String);
#[derive(Debug, PartialEq)]
pub struct SearchSongs(pub String);
#[derive(Debug, PartialEq)]
pub struct SearchPlaylists(pub String);
#[derive(Debug, PartialEq)]
pub struct GetArtistSongs(pub ArtistChannelID<'static>);
#[derive(Debug, PartialEq)]
pub struct GetPlaylistSongs {
    pub playlist_id: PlaylistID<'static>,
}

#[derive(Debug)]
pub struct DownloadSong(
    pub VideoID<'static>,
    pub ListSongID,
    pub tokio_util::sync::CancellationToken,
);

impl PartialEq for DownloadSong {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 && self.1 == other.1
    }
}

pub enum DownloadProgressUpdate {
    Downloading,
    Completed(Box<dyn Source<Item = f32> + Send + 'static>),
    Error(String),
}

impl std::fmt::Debug for DownloadProgressUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Downloading => write!(f, "Downloading"),
            Self::Completed(_) => write!(f, "Completed(<source>)"),
            Self::Error(e) => write!(f, "Error({e})"),
        }
    }
}

impl PartialEq for DownloadProgressUpdate {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Downloading, Self::Downloading) => true,
            (Self::Completed(_), Self::Completed(_)) => true,
            (Self::Error(a), Self::Error(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(PartialEq, Debug)]
pub struct IncreaseVolume(pub i8);
#[derive(Debug, PartialEq)]
pub struct SetVolume(pub u8);
#[derive(Debug, PartialEq)]
pub struct Seek {
    pub duration: Duration,
    pub direction: SeekDirection,
}
#[derive(Debug, PartialEq)]
pub struct SeekTo {
    pub position: Duration,
    pub id: ListSongID,
}
#[derive(Debug, PartialEq)]
pub struct Stop(pub ListSongID);
#[derive(Debug, PartialEq)]
pub struct StopAll;
#[derive(Debug, PartialEq)]
pub struct PausePlay(pub ListSongID);
#[derive(Debug, PartialEq)]
pub struct Resume(pub ListSongID);
#[derive(Debug, PartialEq)]
pub struct Pause(pub ListSongID);
pub struct PlaySong {
    pub song: Box<dyn Source<Item = f32> + Send + 'static>,
    pub id: ListSongID,
}
impl std::fmt::Debug for PlaySong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaySong").field("id", &self.id).finish()
    }
}
pub struct AutoplaySong {
    pub song: Box<dyn Source<Item = f32> + Send + 'static>,
    pub id: ListSongID,
}
impl std::fmt::Debug for AutoplaySong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutoplaySong")
            .field("id", &self.id)
            .finish()
    }
}
impl BackendTask<ArcServer> for SearchArtists {
    type Output = Result<Vec<SearchResultArtist>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.api.search_artists(self.0).await }
    }
}
impl BackendTask<ArcServer> for SearchSongs {
    type Output = Result<Vec<SearchResultSong>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move {
            let mut results = backend.api.search_songs(self.0).await?;

            if results.is_empty() {
                return Ok(results);
            }

            // Pre-resolve stream URLs + resolve non-Atv to Atv for top 5.
            // #1 result resolves URL synchronously so URL_CACHE is populated
            // before the user can select it. Others are background tasks.
            let yt_dlp = backend.config.yt_dlp_command.clone();
            let top_count = results.len().min(5);
            let mut atv_replaced: Vec<(usize, VideoID<'static>)> = Vec::new();
            for (idx, song) in results.iter().enumerate().take(top_count) {
                // Resolve non-Atv to Atv first (may change video_id).
                if !song.is_audio_track()
                    && let Ok(concurrent_api) = backend.api.get_api().await
                {
                    let new_id = resolve_to_audio_track(
                        &concurrent_api,
                        &song.title,
                        &song.artist,
                        song.video_id.get_raw(),
                    )
                    .await
                    .unwrap_or_else(|| song.video_id.clone());
                    if new_id.get_raw() != song.video_id.get_raw() {
                        atv_replaced.push((idx, new_id));
                    }
                }
            }
            // Apply Atv replacements BEFORE URL resolve so we resolve the
            // corrected video_id.
            for (idx, new_id) in &atv_replaced {
                results[*idx].video_id = new_id.clone();
            }
            let vid0 = results[0].video_id.get_raw().to_string();
            let yt0 = yt_dlp.clone();
            let _0_r = song_downloader::resolve_url(&vid0, &yt0, backend.po_token.as_deref(), None).await;
            debug!(video_id = %vid0, resolved = _0_r, "Synchronous URL pre-resolve for top result");
            for result in results.iter().skip(1).take(top_count.saturating_sub(1)) {
                let vid = result.video_id.get_raw().to_string();
                let yt = yt_dlp.clone();
                let pt = backend.po_token.clone();
                // fire-and-forget: best-effort URL cache warming for non-top results
                tokio::spawn(async move {
                    song_downloader::resolve_url(&vid, &yt, pt.as_deref(), None).await;
                });
            }

            Ok(results)
        }
    }
}

/// Resolve a queue song (by title + artist + original video_id) to its
/// audio (Atv) version via the YTM API.  Returns `Some(resolved_video_id)`
/// if a different (Atv) track was found, or `None` if already Atv / no match.
#[derive(Debug, PartialEq)]
pub struct ResolveSongToAudio {
    pub video_id: VideoID<'static>,
    pub id: ListSongID,
    pub title: String,
    pub artist: String,
}
impl BackendTask<ArcServer> for ResolveSongToAudio {
    type Output = Option<VideoID<'static>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move {
            if let Ok(api) = backend.api.get_api().await {
                resolve_to_audio_track(&api, &self.title, &self.artist, self.video_id.get_raw())
                    .await
            } else {
                None
            }
        }
    }
}

impl BackendTask<ArcServer> for SearchPlaylists {
    type Output = Result<Vec<SearchResultPlaylist>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.api.search_playlists(self.0).await }
    }
}
impl BackendStreamingTask<ArcServer> for GetArtistSongs {
    type Output = GetArtistSongsProgressUpdate;
    type MetadataType = TaskMetadata;
    fn into_stream(
        self,
        backend: &ArcServer,
    ) -> impl futures::Stream<Item = Self::Output> + Send + Unpin + 'static {
        let backend = backend.clone();
        backend.api.get_artist_songs(self.0)
    }
}
impl BackendStreamingTask<ArcServer> for GetPlaylistSongs {
    type Output = GetPlaylistSongsProgressUpdate;
    type MetadataType = TaskMetadata;
    fn into_stream(
        self,
        backend: &ArcServer,
    ) -> impl futures::Stream<Item = Self::Output> + Send + Unpin + 'static {
        let backend = backend.clone();
        backend
            .api
            .get_playlist_songs(self.playlist_id)
    }
}

impl BackendStreamingTask<ArcServer> for DownloadSong {
    type Output = DownloadProgressUpdate;
    type MetadataType = TaskMetadata;
    fn into_stream(
        self,
        backend: &ArcServer,
    ) -> impl futures::Stream<Item = Self::Output> + Send + Unpin + 'static {
        let backend = backend.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(3);
        // fire-and-forget: result sent via tx channel; panics surface as rx channel close
        tokio::spawn(async move {
            tx.try_send(DownloadProgressUpdate::Downloading).ok();
            let result = song_downloader::download_and_decode(
                &backend.config.yt_dlp_command,
                self.0.get_raw(),
                backend.po_token.as_deref(),
                None,
                None,
                self.2,
            )
            .await;
            match result {
                Ok(decoder) => {
                    if let Err(e) = tx
                        .send(DownloadProgressUpdate::Completed(Box::new(decoder)))
                        .await
                    {
                        debug!("Failed to send download completion: {e}");
                    }
                }
                Err(e) => {
                    if let Err(send_err) =
                        tx.send(DownloadProgressUpdate::Error(e.to_string())).await
                    {
                        debug!("Failed to send download error: {send_err}");
                    }
                }
            }
        });
        ReceiverStream::new(rx)
    }
}
impl BackendTask<ArcServer> for Seek {
    type Output = Option<ProgressUpdate<ListSongID>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.player.seek(self.duration, self.direction).await }
    }
}
impl BackendTask<ArcServer> for SeekTo {
    type Output = Option<ProgressUpdate<ListSongID>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.player.seek_to(self.position, self.id).await }
    }
}
impl BackendTask<ArcServer> for IncreaseVolume {
    type Output = Option<VolumeUpdate>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.player.increase_volume(self.0).await }
    }
}
impl BackendTask<ArcServer> for SetVolume {
    type Output = Option<VolumeUpdate>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.player.set_volume(self.0).await }
    }
}
impl BackendTask<ArcServer> for Stop {
    type Output = Option<Stopped<ListSongID>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.player.stop(self.0).await }
    }
}
impl BackendTask<ArcServer> for StopAll {
    type Output = Option<AllStopped>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.player.stop_all().await }
    }
}
impl BackendTask<ArcServer> for PausePlay {
    type Output = Option<PausePlayResponse<ListSongID>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.player.pause_play(self.0).await }
    }
    fn metadata() -> Vec<Self::MetadataType> {
        vec![TaskMetadata::PlayPause]
    }
}
impl BackendTask<ArcServer> for Resume {
    type Output = Option<Resumed<ListSongID>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.player.resume(self.0).await }
    }
    fn metadata() -> Vec<Self::MetadataType> {
        vec![TaskMetadata::PlayPause]
    }
}
impl BackendTask<ArcServer> for Pause {
    type Output = Option<Paused<ListSongID>>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let backend = backend.clone();
        async move { backend.player.pause(self.0).await }
    }
    fn metadata() -> Vec<Self::MetadataType> {
        vec![TaskMetadata::PlayPause]
    }
}

impl BackendStreamingTask<ArcServer> for PlaySong {
    type Output = PlayUpdate<ListSongID>;
    type MetadataType = TaskMetadata;
    fn into_stream(
        self,
        backend: &ArcServer,
    ) -> impl Stream<Item = Self::Output> + Send + Unpin + 'static {
        let backend = backend.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let stream = backend.player.play_song(self.song, self.id);
        // fire-and-forget: stream bridged via tx/rx; rx close on drop terminates loop
        tokio::spawn(async move {
            use futures::StreamExt;
            tokio::pin!(stream);
            while let Some(item) = stream.next().await {
                if tx.send(item).await.is_err() {
                    break;
                }
            }
        });
        tokio_stream::wrappers::ReceiverStream::new(rx)
    }
    fn metadata() -> Vec<Self::MetadataType> {
        vec![TaskMetadata::PlayingSong]
    }
}
impl BackendStreamingTask<ArcServer> for AutoplaySong {
    type Output = AutoplayUpdate<ListSongID>;
    type MetadataType = TaskMetadata;
    fn into_stream(
        self,
        backend: &ArcServer,
    ) -> impl Stream<Item = Self::Output> + Send + Unpin + 'static {
        let backend = backend.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let stream = backend.player.autoplay_song(self.song, self.id);
        // fire-and-forget: stream bridged via tx/rx; rx close on drop terminates loop
        tokio::spawn(async move {
            use futures::StreamExt;
            tokio::pin!(stream);
            while let Some(item) = stream.next().await {
                if tx.send(item).await.is_err() {
                    break;
                }
            }
        });
        tokio_stream::wrappers::ReceiverStream::new(rx)
    }
    fn metadata() -> Vec<Self::MetadataType> {
        vec![TaskMetadata::PlayingSong]
    }
}
impl PartialEq for HandleApiError {
    fn eq(&self, other: &Self) -> bool {
        self.error.to_string() == other.error.to_string() && self.message == other.message
    }
}
impl PartialEq for PlaySong {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl PartialEq for AutoplaySong {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ytmapi_rs::common::YoutubeID;

    fn make_download_song() -> DownloadSong {
        DownloadSong(VideoID::from_raw("test"), ListSongID(1), tokio_util::sync::CancellationToken::new())
    }

    #[test]
    fn handle_api_error_partial_eq() {
        let a = HandleApiError {
            error: Error::msg("disk full"),
            message: "write failed".into(),
        };
        let b = HandleApiError {
            error: Error::msg("disk full"),
            message: "write failed".into(),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn handle_api_error_partial_eq_differs_on_message() {
        let a = HandleApiError {
            error: Error::msg("disk full"),
            message: "write failed".into(),
        };
        let b = HandleApiError {
            error: Error::msg("disk full"),
            message: "different".into(),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn download_song_partial_eq_eq() {
        let a = make_download_song();
        let b = make_download_song();
        assert_eq!(a, b);
    }

    #[test]
    fn download_song_partial_eq_differs_on_video_id() {
        let a = make_download_song();
        let b = DownloadSong(VideoID::from_raw("other"), ListSongID(1), tokio_util::sync::CancellationToken::new());
        assert_ne!(a, b);
    }
}
