use crate::app::structures::ListSongID;
use crate::async_rodio_sink::{self, AsyncRodio};
use futures::Stream;
use rodio::Source;
use std::time::Duration;

pub struct Player {
    rodio_handle: AsyncRodio<ListSongID>,
}

impl Player {
    #[must_use]
    pub fn new() -> Self {
        let rodio_handle = AsyncRodio::new();
        Self { rodio_handle }
    }
    #[must_use]
    pub fn autoplay_song(
        &self,
        song: Box<dyn Source<Item = f32> + Send + 'static>,
        song_id: ListSongID,
    ) -> impl Stream<Item = async_rodio_sink::AutoplayUpdate<ListSongID>> + 'static {
        self.rodio_handle.autoplay_song(song, song_id)
    }
    #[must_use]
    pub fn play_song(
        &self,
        song: Box<dyn Source<Item = f32> + Send + 'static>,
        song_id: ListSongID,
    ) -> impl Stream<Item = async_rodio_sink::PlayUpdate<ListSongID>> + 'static {
        self.rodio_handle.play_song(song, song_id)
    }
    #[must_use]
    pub async fn seek(
        &self,
        duration: Duration,
        direction: async_rodio_sink::SeekDirection,
    ) -> Option<async_rodio_sink::ProgressUpdate<ListSongID>> {
        self.rodio_handle.seek(duration, direction).await
    }
    #[must_use]
    pub async fn seek_to(
        &self,
        seek_to_pos: Duration,
        id: ListSongID,
    ) -> Option<async_rodio_sink::ProgressUpdate<ListSongID>> {
        self.rodio_handle.seek_to(seek_to_pos, id).await
    }
    #[must_use]
    pub async fn stop(&self, song_id: ListSongID) -> Option<async_rodio_sink::Stopped<ListSongID>> {
        self.rodio_handle.stop(song_id).await
    }
    #[must_use]
    pub async fn stop_all(&self) -> Option<async_rodio_sink::AllStopped> {
        self.rodio_handle.stop_all().await
    }
    #[must_use]
    pub async fn pause_play(
        &self,
        song_id: ListSongID,
    ) -> Option<async_rodio_sink::PausePlayResponse<ListSongID>> {
        self.rodio_handle.pause_play(song_id).await
    }
    #[must_use]
    pub async fn resume(
        &self,
        song_id: ListSongID,
    ) -> Option<async_rodio_sink::Resumed<ListSongID>> {
        self.rodio_handle.resume(song_id).await
    }
    #[must_use]
    pub async fn pause(&self, song_id: ListSongID) -> Option<async_rodio_sink::Paused<ListSongID>> {
        self.rodio_handle.pause(song_id).await
    }
    #[must_use]
    pub async fn increase_volume(&self, vol_inc: i8) -> Option<async_rodio_sink::VolumeUpdate> {
        self.rodio_handle.increase_volume(vol_inc).await
    }
    #[must_use]
    pub async fn set_volume(&self, new_vol: u8) -> Option<async_rodio_sink::VolumeUpdate> {
        self.rodio_handle.set_volume(new_vol).await
    }
}
