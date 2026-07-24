use crate::app::structures::ListSongID;
use crate::async_rodio_sink::{self, AsyncRodio};
use futures::Stream;
use rodio::Source;

pub struct Player {
    rodio_handle: AsyncRodio<ListSongID>,
}

impl Player {
    pub fn new() -> anyhow::Result<Self> {
        let rodio_handle = AsyncRodio::new()?;
        Ok(Self { rodio_handle })
    }
    pub fn play_song(
        &self,
        song: Box<dyn Source<Item = f32> + Send + 'static>,
        song_id: ListSongID,
    ) -> impl Stream<Item = async_rodio_sink::PlayUpdate<ListSongID>> + 'static {
        self.rodio_handle.play_song(song, song_id)
    }
    pub fn stop(&self) {
        self.rodio_handle.stop()
    }
    pub fn pause(&self) {
        self.rodio_handle.pause()
    }
    pub async fn increase_volume(&self, vol_inc: i8) -> Option<async_rodio_sink::VolumeUpdate> {
        self.rodio_handle.increase_volume(vol_inc).await
    }
    pub async fn set_volume(&self, new_vol: u8) -> Option<async_rodio_sink::VolumeUpdate> {
        self.rodio_handle.set_volume(new_vol).await
    }
}
