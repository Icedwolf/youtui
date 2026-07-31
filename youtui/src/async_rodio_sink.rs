use crate::app::structures::Percentage;
use anyhow::Context;
use futures::Stream;
use std::borrow::Borrow;
use rodio::Source;
use rodio::source::EmptyCallback;
use rodio::{ChannelCount, SampleRate};
use std::fmt::Debug;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, trace};

pub mod rodio {
    pub use rodio::*;
}

const PROGRESS_UPDATE_DELAY: Duration = Duration::from_millis(100);
const PLAYER_MSG_QUEUE_SIZE: usize = 50;

struct PlaybackState<I> {
    cur_song_duration: Option<Duration>,
    cur_song_id: Option<I>,
}

impl<I: Debug + PartialEq + Copy> PlaybackState<I> {
    fn handle_play_song(
        &mut self,
        sink: &rodio::Player,
        song: Box<dyn Source<Item = f32> + Send + 'static>,
        song_id: I,
        tx: &mpsc::Sender<AsyncRodioResponse>,
    ) {
        debug!("Inside PlaySong");
        self.cur_song_duration = song.total_duration();
        debug!(
            "Received request to play {song_id:?} of duration {:?}",
            self.cur_song_duration
        );
        if !sink.empty() {
            sink.stop()
        }
        let txs = tx.clone();
        let song = ProgressSource::new(song, PROGRESS_UPDATE_DELAY, move |pos| {
            let _ = txs.try_send(AsyncRodioResponse::ProgressUpdate(pos));
        });
        let on_done = on_done_cb(tx);
        sink.append(song);
        sink.append(on_done);
        if sink.is_paused() {
            sink.play();
        }
        debug!("Now playing {:?}", song_id);
        let _ = tx.try_send(AsyncRodioResponse::StartedPlaying(self.cur_song_duration));
        self.cur_song_id = Some(song_id);
    }

    fn handle_stop(&mut self, sink: &rodio::Player) {
        debug!("Stop");
        if !sink.empty() {
            sink.stop()
        }
        self.cur_song_id = None;
    }

    fn handle_pause(&mut self, sink: &rodio::Player) {
        debug!("Pause");
        if sink.is_paused() {
            sink.play();
        } else {
            sink.pause();
        }
    }

    fn handle_increase_volume(&self, sink: &rodio::Player, vol_inc: i8, tx: oneshot::Sender<Percentage>) {
        sink.set_volume((sink.volume() + vol_inc as f32 / 100.0).clamp(0.0, 1.0));
        let _ = tx.send(Percentage((sink.volume() * 100.0).round() as u8));
        debug!("Rodio sent volume update");
    }

    fn handle_set_volume(&self, sink: &rodio::Player, percentage: u8, tx: oneshot::Sender<Percentage>) {
        sink.set_volume((percentage as f32 / 100.0).clamp(0.0, 1.0));
        let _ = tx.send(Percentage((sink.volume() * 100.0).round() as u8));
        debug!("Rodio sent volume update");
    }

}

enum AsyncRodioRequest<I> {
    PlaySong(
        Box<dyn Source<Item = f32> + Send + 'static>,
        I,
        mpsc::Sender<AsyncRodioResponse>,
    ),
    Stop,
    Pause,
    IncreaseVolume(i8, oneshot::Sender<Percentage>),
    SetVolume(u8, oneshot::Sender<Percentage>),
}

#[derive(Debug)]
pub(crate) enum AsyncRodioResponse {
    ProgressUpdate(Duration),
    StartedPlaying(Option<Duration>),
    StoppedPlaying,
}

#[derive(Debug, PartialEq)]
pub struct VolumeUpdate(pub Percentage);
#[derive(Debug, PartialEq)]
pub struct Stopped<I>(pub I);
#[derive(Debug, PartialEq)]
pub struct AllStopped;
#[derive(PartialEq, Debug)]
pub enum PlayUpdate<I>
where
    I: Debug,
{
    PlayProgress(Duration, I),
    Playing(Option<Duration>, I),
    DonePlaying(I),
}

pub struct AsyncRodio<I>
where
    I: Debug,
{
    tx: std::sync::mpsc::Sender<AsyncRodioRequest<I>>,
}

impl<I> AsyncRodio<I>
where
    I: Debug + PartialEq + Copy + Send + 'static,
{
    pub fn new() -> anyhow::Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<AsyncRodioRequest<I>>();
        let (init_tx, init_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();

        tokio::task::spawn_blocking(move || {
            let mixer_device_sink = match (|| -> anyhow::Result<_> {
                let builder = rodio::DeviceSinkBuilder::from_default_device()
                    .map_err(|e| anyhow::anyhow!("Audio device unavailable: {e}"))?;
                let mut sink = builder
                    .with_buffer_size(rodio::cpal::BufferSize::Fixed(4096))
                    .with_error_callback(|err| trace!("audio stream error: {err}"))
                    .open_sink_or_fallback()
                    .map_err(|e| anyhow::anyhow!("Failed to open audio sink: {e}"))?;
                sink.log_on_drop(false);
                Ok(sink)
            })() {
                Ok(sink) => {
                    let _ = init_tx.send(Ok(()));
                    sink
                }
                Err(e) => {
                    let _ = init_tx.send(Err(e));
                    return;
                }
            };

            let sink = rodio::Player::connect_new(mixer_device_sink.mixer());
            let mut state = PlaybackState {
                cur_song_duration: None,
                cur_song_id: None,
            };
            while let Ok(msg) = rx.recv() {
                match msg {
                    AsyncRodioRequest::PlaySong(song, song_id, tx) => {
                        state.handle_play_song(&sink, song, song_id, &tx);
                    }
                    AsyncRodioRequest::Stop => {
                        state.handle_stop(&sink);
                    }
                    AsyncRodioRequest::Pause => {
                        state.handle_pause(&sink);
                    }
                    AsyncRodioRequest::IncreaseVolume(vol_inc, tx) => {
                        state.handle_increase_volume(&sink, vol_inc, tx);
                    }
                    AsyncRodioRequest::SetVolume(percentage, tx) => {
                        state.handle_set_volume(&sink, percentage, tx);
                    }
                }
            }
        });

        init_rx
            .recv()
            .context("Audio initialization thread panicked or failed to start")??;

        Ok(Self { tx })
    }

    pub fn play_song(
        &self,
        song: Box<dyn Source<Item = f32> + Send + 'static>,
        identifier: I,
    ) -> impl Stream<Item = PlayUpdate<I>> + 'static {
        let (tx, mut rx) = mpsc::channel(PLAYER_MSG_QUEUE_SIZE);
        let (streamtx, streamrx) = mpsc::channel(PLAYER_MSG_QUEUE_SIZE);
        let selftx = self.tx.clone();
        let _handle = tokio::task::spawn(async move {
            std_send_or_error(&selftx, AsyncRodioRequest::PlaySong(song, identifier, tx)).await;
            while let Some(msg) = rx.recv().await {
                trace!("Received {msg:?}");
                match msg {
                    AsyncRodioResponse::ProgressUpdate(duration) => {
                        send_or_error(&streamtx, PlayUpdate::PlayProgress(duration, identifier)).await;
                    }
                    AsyncRodioResponse::StartedPlaying(duration) => {
                        debug!(
                            "audio_output_started: song_id={:?}, duration={:?}",
                            identifier, duration
                        );
                        send_or_error(&streamtx, PlayUpdate::Playing(duration, identifier)).await;
                    }
                    AsyncRodioResponse::StoppedPlaying => {
                        send_or_error(&streamtx, PlayUpdate::DonePlaying(identifier)).await;
                        return;
                    }
                }
            }
            debug!(
                "Playback channel closed for {:?} before final status received",
                identifier
            );
        });
        ReceiverStream::new(streamrx)
    }

    pub fn stop(&self) {
        let _ = self.tx.send(AsyncRodioRequest::Stop);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(AsyncRodioRequest::Pause);
    }

    pub async fn increase_volume(&self, vol_inc: i8) -> Option<VolumeUpdate> {
        let (tx, rx) = oneshot::channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::IncreaseVolume(vol_inc, tx)).await;
        let Ok(current_volume) = rx.await else {
            error!("The player has been dropped while I was waiting for a volume update for",);
            return None;
        };
        Some(VolumeUpdate(current_volume))
    }

    pub async fn set_volume(&self, new_vol: u8) -> Option<VolumeUpdate> {
        let (tx, rx) = oneshot::channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::SetVolume(new_vol, tx)).await;
        let Ok(current_volume) = rx.await else {
            error!("The player has been dropped while I was waiting for a volume update for",);
            return None;
        };
        Some(VolumeUpdate(current_volume))
    }
}

fn on_done_cb(tx: &mpsc::Sender<AsyncRodioResponse>) -> EmptyCallback {
    let tx = tx.clone();
    let cb = move || {
        let _ = tx.try_send(AsyncRodioResponse::StoppedPlaying);
    };
    EmptyCallback::new(Box::new(cb))
}

struct ProgressSource {
    inner: Box<dyn Source<Item = f32> + Send + 'static>,
    callback: Box<dyn FnMut(Duration) + Send>,
    interval: Duration,
    last_access: std::time::Instant,
    samples: u64,
    samples_per_sec: f64,
}

impl ProgressSource {
    fn new(
        inner: Box<dyn Source<Item = f32> + Send + 'static>,
        interval: Duration,
        callback: impl FnMut(Duration) + Send + 'static,
    ) -> Self {
        let sample_rate = inner.sample_rate();
        let channels = inner.channels();
        ProgressSource {
            inner,
            callback: Box::new(callback),
            interval,
            last_access: std::time::Instant::now(),
            samples: 0,
            samples_per_sec: sample_rate.get() as f64 * channels.get() as f64,
        }
    }
}

impl Iterator for ProgressSource {
    type Item = f32;
    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        self.samples += 1;
        let elapsed = self.last_access.elapsed();
        if elapsed >= self.interval {
            self.last_access = std::time::Instant::now();
            let pos = Duration::from_secs_f64(self.samples as f64 / self.samples_per_sec);
            (self.callback)(pos);
        }
        Some(sample)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl Source for ProgressSource {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }
    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }
    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

pub async fn send_or_error<T, S: Borrow<mpsc::Sender<T>>>(tx: S, msg: T) {
    tx.borrow()
        .send(msg)
        .await
        .unwrap_or_else(|e| debug!("Error {e} received when sending message"));
}

pub async fn std_send_or_error<T>(tx: &std::sync::mpsc::Sender<T>, msg: T) {
    tx.send(msg)
        .unwrap_or_else(|e| debug!("Error {e} received when sending message"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentage_into_u8() {
        let p = Percentage(75);
        let val = p.0;
        assert_eq!(val, 75);
    }

    #[test]
    fn percentage_equality() {
        assert_eq!(Percentage(50), Percentage(50));
        assert_ne!(Percentage(50), Percentage(100));
    }
}
