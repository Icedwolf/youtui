use super::api::{GetArtistSongsProgressUpdate, GetPlaylistSongsProgressUpdate, resolve_to_audio_track};
use super::ArcServer;
use super::song_downloader;
use tracing::info;
use crate::app::AudioQuality;
use crate::app::structures::ListSongID;
use crate::async_rodio_sink::{
    AllStopped, AutoplayUpdate, PausePlayResponse, Paused, PlayUpdate, ProgressUpdate, QueueUpdate,
    Resumed, SeekDirection, Stopped, VolumeUpdate,
};
use anyhow::{Error, Result};
use async_callback_manager::{BackendStreamingTask, BackendTask};
use futures::{Future, Stream};
use rodio::Source;
use std::time::Duration;
use tokio_stream::wrappers::ReceiverStream;

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
    pub max_songs: usize,
}

#[derive(Debug)]
pub struct DownloadSong(pub VideoID<'static>, pub ListSongID, pub AudioQuality);

impl PartialEq for DownloadSong {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 && self.1 == other.1 && self.2 == other.2
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
        f.debug_struct("AutoplaySong").field("id", &self.id).finish()
    }
}
#[allow(dead_code)]
pub struct QueueSong {
    pub song: Box<dyn Source<Item = f32> + Send + 'static>,
    pub id: ListSongID,
}
impl std::fmt::Debug for QueueSong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueueSong").field("id", &self.id).finish()
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

            // Pre-resolve stream URLs + resolve non-Atv to Atv for top 5.
            let yt_dlp = backend.config.yt_dlp_command.clone();
            let top_count = results.len().min(5);
            let mut atv_replaced: Vec<(usize, VideoID<'static>)> = Vec::new();
            for idx in 0..top_count {
                let song = &results[idx];
                // Start URL pre-resolution in background.
                let vid = song.video_id.get_raw().to_string();
                let yt = yt_dlp.clone();
                tokio::spawn(async move {
                    song_downloader::resolve_url(&vid, &yt).await;
                });
                // Resolve non-Atv to Atv.
                if !song.is_audio_track() {
                    if let Ok(concurrent_api) = backend.api.get_api().await {
                        let resolved = resolve_to_audio_track(&concurrent_api, song).await;
                        if resolved.get_raw() != song.video_id.get_raw() {
                            atv_replaced.push((idx, resolved));
                        }
                    }
                }
            }
            for (idx, new_id) in atv_replaced {
                results[idx].video_id = new_id;
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
            let search_query = format!("{} {}", self.title, self.artist);
            // First try the filtered songs search (deduplicates by title+artist).
            if let Ok(results) = backend.api.search_songs(search_query.clone()).await {
                for result in &results {
                    if result.is_audio_track()
                        && result.title == self.title
                        && result.artist == self.artist
                        && result.video_id.get_raw() != self.video_id.get_raw()
                    {
                        info!(
                            original = self.video_id.get_raw(),
                            resolved = result.video_id.get_raw(),
                            title = self.title.as_str(),
                            artist = self.artist.as_str(),
                            "Resolved queue song to Atv track"
                        );
                        return Some(result.video_id.clone());
                    }
                }
            }
            // Fallback: broad search (may return multiple results per title+artist).
            if let Ok(results) = backend.api.search_broad(search_query).await {
                for result in &results.songs {
                    if result.is_audio_track()
                        && result.title == self.title
                        && result.artist == self.artist
                        && result.video_id.get_raw() != self.video_id.get_raw()
                    {
                        info!(
                            original = self.video_id.get_raw(),
                            resolved = result.video_id.get_raw(),
                            title = self.title.as_str(),
                            artist = self.artist.as_str(),
                            "Resolved queue song to Atv track (broad search)"
                        );
                        return Some(result.video_id.clone());
                    }
                }
            }
            None
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
            .get_playlist_songs(self.playlist_id, self.max_songs)
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
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            tx.try_send(DownloadProgressUpdate::Downloading).ok();
            let result = song_downloader::download_and_decode(
                &backend.config.yt_dlp_command,
                self.0.get_raw(),
                self.2,
                backend.po_token.as_deref(),
                None,
                None,
            ).await;
            match result {
                Ok(decoder) => {
                    tx.send(DownloadProgressUpdate::Completed(Box::new(decoder))).await.ok();
                }
                Err(e) => {
                    tx.send(DownloadProgressUpdate::Error(e)).await.ok();
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
impl BackendStreamingTask<ArcServer> for QueueSong {
    type Output = QueueUpdate<ListSongID>;
    type MetadataType = TaskMetadata;
    fn into_stream(
        self,
        backend: &ArcServer,
    ) -> impl Stream<Item = Self::Output> + Send + Unpin + 'static {
        let backend = backend.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let stream = backend.player.queue_song(self.song, self.id);
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
impl PartialEq for QueueSong {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytmapi_rs::common::YoutubeID;

    fn make_download_song() -> DownloadSong {
        DownloadSong(
            VideoID::from_raw("test"),
            ListSongID(1),
            AudioQuality::Best,
        )
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
    fn download_song_partial_eq_matches() {
        let a = make_download_song();
        let b = make_download_song();
        assert_eq!(a, b);
    }

    #[test]
    fn download_song_partial_eq_differs_on_quality() {
        let a = make_download_song();
        let b = DownloadSong(
            VideoID::from_raw("test"),
            ListSongID(1),
            AudioQuality::High,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn download_song_partial_eq_differs_on_video_id() {
        let a = make_download_song();
        let b = DownloadSong(
            VideoID::from_raw("other"),
            ListSongID(1),
            AudioQuality::Best,
        );
        assert_ne!(a, b);
    }
}
