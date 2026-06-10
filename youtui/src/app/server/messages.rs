use super::ArcServer;
use super::api::GetArtistSongsProgressUpdate;
use super::player::{DecodedInMemSong, Player};
use super::song_downloader::{DownloadProgressUpdate, InMemSong};
use crate::app::server::api::GetPlaylistSongsProgressUpdate;
use crate::app::AudioQuality;
use crate::app::structures::ListSongID;
use crate::async_rodio_sink::rodio::decoder::DecoderError;
use crate::async_rodio_sink::{
    AllStopped, AutoplayUpdate, PausePlayResponse, Paused, PlayUpdate, ProgressUpdate, QueueUpdate,
    Resumed, SeekDirection, Stopped, VolumeUpdate,
};
use anyhow::{Error, Result};
use async_callback_manager::{BackendStreamingTask, BackendTask, MapFn};
use futures::{Future, Stream};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use ytmapi_rs::common::{ArtistChannelID, PlaylistID, SearchSuggestion, VideoID};
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
pub struct DownloadSong(pub VideoID<'static>, pub ListSongID, pub Arc<CancellationToken>, pub AudioQuality);

impl PartialEq for DownloadSong {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 && self.1 == other.1 && self.3 == other.3
    }
}

// Player Requests documentation:
// NOTE: I considered giving player more control of the playback than playlist,
// and increasing message size. However this seems to be more combinatorially
// difficult without a well defined data structure.

// XXX: This should be programmed to be unkillable.
// Case:
// Cur volume: 5
// Send IncreaseVolume(5)
// Send IncreaseVolume(5), killing previous task
// Volume will now be 10 - should be 15, should not allow caller to cause this.
// New note - 2025:
// SetVolume should be able to kill IncreaseVolume however...
#[derive(PartialEq, Debug)]
pub struct IncreaseVolume(pub i8);
#[derive(Debug, PartialEq)]
pub struct SetVolume(pub u8);
/// Seek forwards or backwards a duration in a song.
#[derive(Debug, PartialEq)]
pub struct Seek {
    pub duration: Duration,
    pub direction: SeekDirection,
}
/// Seek to a target position in a song.
#[derive(Debug, PartialEq)]
pub struct SeekTo {
    pub position: Duration,
    // Unlike seeking forward or back, it would be odd if user was expecting to seek to pos x in
    // song a but due to a race condition seek applied to song b.
    pub id: ListSongID,
}
/// Stop a song if it is still currently playing.
#[derive(Debug, PartialEq)]
pub struct Stop(pub ListSongID);
/// Stop the player, regardless of what song is playing.
#[derive(Debug, PartialEq)]
pub struct StopAll;
#[derive(Debug, PartialEq)]
pub struct PausePlay(pub ListSongID);
#[derive(Debug, PartialEq)]
pub struct Resume(pub ListSongID);
#[derive(Debug, PartialEq)]
pub struct Pause(pub ListSongID);
/// Decode a song into a format that can be played.
#[derive(PartialEq, Debug)]
pub struct DecodeSong(pub Arc<InMemSong>);
/// Play a song, starting from the start, regardless what's queued.
#[derive(Debug)]
pub struct PlaySong {
    pub song: DecodedInMemSong,
    pub id: ListSongID,
}
/// Play a song, unless it's already queued.
#[derive(Debug)]
pub struct AutoplaySong {
    pub song: DecodedInMemSong,
    pub id: ListSongID,
}
/// Queue a song to play next.
#[derive(Debug)]
pub struct QueueSong {
    pub song: DecodedInMemSong,
    pub id: ListSongID,
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
        async move { backend.api.search_songs(self.0).await }
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
        backend.song_downloader.download_song(self.0, self.1, Some(self.2), self.3)
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
impl BackendTask<ArcServer> for DecodeSong {
    type Output = std::result::Result<DecodedInMemSong, DecoderError>;
    type MetadataType = TaskMetadata;
    fn into_future(
        self,
        _backend: &ArcServer,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        Player::try_decode(self.0)
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
        backend.player.play_song(self.song, self.id)
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
        backend.player.autoplay_song(self.song, self.id)
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
        backend.player.queue_song(self.song, self.id)
    }
    fn metadata() -> Vec<Self::MetadataType> {
        vec![TaskMetadata::PlayingSong]
    }
}

#[derive(PartialEq, Debug)]
pub struct PlayDecodedSong(pub ListSongID);
impl MapFn<DecodedInMemSong> for PlayDecodedSong {
    type Output = PlaySong;
    fn apply(self, input: DecodedInMemSong) -> Self::Output {
        tracing::info!("Song decoded succesfully. {:?}", self.0);
        PlaySong {
            song: input,
            id: self.0,
        }
    }
}
#[derive(PartialEq, Debug)]
pub struct AutoplayDecodedSong(pub ListSongID);
impl MapFn<DecodedInMemSong> for AutoplayDecodedSong {
    type Output = AutoplaySong;
    fn apply(self, input: DecodedInMemSong) -> Self::Output {
        tracing::info!("Song decoded succesfully. {:?}", self.0);
        AutoplaySong {
            song: input,
            id: self.0,
        }
    }
}
#[derive(PartialEq, Debug)]
pub struct QueueDecodedSong(pub ListSongID);
impl MapFn<DecodedInMemSong> for QueueDecodedSong {
    type Output = QueueSong;
    fn apply(self, input: DecodedInMemSong) -> Self::Output {
        tracing::info!("Song decoded succesfully. {:?}", self.0);
        QueueSong {
            song: input,
            id: self.0,
        }
    }
}

// FusedTask in async-callback-manager derives PartialEq unconditionally,
// so these task types must implement PartialEq in all build configurations.
// We compare all fields except DecodedInMemSong (wraps a Decoder, not comparable).
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
            Arc::new(CancellationToken::new()),
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
            Arc::new(CancellationToken::new()),
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
            Arc::new(CancellationToken::new()),
            AudioQuality::Best,
        );
        assert_ne!(a, b);
    }
}
