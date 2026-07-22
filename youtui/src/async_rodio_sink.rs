use crate::app::structures::Percentage;
use anyhow::Context;
use async_callback_manager::PanickingReceiverStream;
use futures::Stream;
use futures::StreamExt;
use std::borrow::Borrow;
use rodio::Source;
use rodio::source::EmptyCallback;
use rodio::{ChannelCount, SampleRate};
use std::fmt::Debug;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
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

    fn handle_stop(&mut self, sink: &rodio::Player, song_id: I, tx: oneshot::Sender<()>) {
        debug!("Got message to stop playing {:?}", song_id);
        if self.cur_song_id != Some(song_id) {
            return;
        }
        if !sink.empty() {
            sink.stop()
        }
        self.cur_song_id = None;
        self.cur_song_duration = None;
        let _ = tx.send(());
    }

    fn handle_stop_all(&mut self, sink: &rodio::Player, tx: oneshot::Sender<()>) {
        debug!("Got message to stop playing all");
        if !sink.empty() {
            sink.stop()
        }
        self.cur_song_id = None;
        self.cur_song_duration = None;
        let _ = tx.send(());
    }

    fn handle_pause_play(
        &mut self,
        sink: &rodio::Player,
        song_id: I,
        tx: oneshot::Sender<AsyncRodioPlayActionTaken>,
    ) {
        debug!("Got message to pause / play {:?}", song_id);
        if self.cur_song_id != Some(song_id) {
            let _ = tx.send(AsyncRodioPlayActionTaken::Played);
            return;
        }
        if sink.is_paused() {
            sink.play();
            debug!("Sending Play message {:?}", song_id);
            let _ = tx.send(AsyncRodioPlayActionTaken::Played);
        } else if !sink.is_paused() && !sink.empty() {
            sink.pause();
            debug!("Sending Pause message {:?}", song_id);
            let _ = tx.send(AsyncRodioPlayActionTaken::Paused);
        } else {
            let _ = tx.send(AsyncRodioPlayActionTaken::Played);
        }
    }

    fn handle_resume(&mut self, sink: &rodio::Player, song_id: I, tx: oneshot::Sender<()>) {
        debug!("Got message to resume {:?}", song_id);
        if self.cur_song_id != Some(song_id) {
            let _ = tx.send(());
            return;
        }
        if sink.is_paused() {
            sink.play();
            debug!("Sending Played message {:?}", song_id);
        }
        let _ = tx.send(());
    }

    fn handle_pause(&mut self, sink: &rodio::Player, song_id: I, tx: oneshot::Sender<()>) {
        debug!("Got message to pause {:?}", song_id);
        if self.cur_song_id != Some(song_id) {
            let _ = tx.send(());
            return;
        }
        if !sink.is_paused() && !sink.empty() {
            sink.pause();
            debug!("Sending Paused message {:?}", song_id);
        }
        let _ = tx.send(());
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

    fn handle_seek(
        &mut self,
        sink: &rodio::Player,
        inc: Duration,
        direction: SeekDirection,
        tx: oneshot::Sender<(Duration, I)>,
    ) {
        debug!("Got request to seek {inc:?} in direction {direction:?}");
        let Some(cur_song_id) = self.cur_song_id else {
            debug!("Tried to seek, but no song loaded");
            return;
        };
        let cur_pos = sink.get_pos();
        let new_pos = match direction {
            SeekDirection::Forward => cur_pos.saturating_add(inc).min(
                self.cur_song_duration
                    .unwrap_or(cur_pos.saturating_add(inc)),
            ),
            SeekDirection::Back => cur_pos
                .saturating_sub(inc)
                .min(self.cur_song_duration.unwrap_or(cur_pos)),
        };
        debug!(
            "Executing seek request of {inc:?} in direction {direction:?}. \
             Song with ID {cur_song_id:?} will move from pos {cur_pos:?} to pos {new_pos:?}"
        );
        if let Err(e) = sink.try_seek(new_pos) {
            error!("Failed to seek {:?}", e);
        }
        std::thread::sleep(Duration::from_millis(5));
        let _ = tx.send((sink.get_pos(), cur_song_id));
    }

    fn handle_seek_to(
        &mut self,
        sink: &rodio::Player,
        seek_to_pos: Duration,
        song_id: I,
        tx: oneshot::Sender<(Duration, I)>,
    ) {
        debug!(
            "Got message to seek to {:?} in song {:?}",
            seek_to_pos, song_id
        );
        if self.cur_song_id != Some(song_id) {
            let _ = tx.send((Duration::ZERO, song_id));
            return;
        }
        let res = sink.try_seek(seek_to_pos.min(self.cur_song_duration.unwrap_or(seek_to_pos)));
        if let Err(e) = res {
            error!("Failed to seek {:?}", e);
        }
        std::thread::sleep(Duration::from_millis(5));
        let _ = tx.send((sink.get_pos(), song_id));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SeekDirection {
    Forward,
    Back,
}

enum AsyncRodioRequest<I> {
    PlaySong(
        Box<dyn Source<Item = f32> + Send + 'static>,
        I,
        mpsc::Sender<AsyncRodioResponse>,
    ),
    Stop(I, oneshot::Sender<()>),
    StopAll(oneshot::Sender<()>),
    PausePlay(I, oneshot::Sender<AsyncRodioPlayActionTaken>),
    Resume(I, oneshot::Sender<()>),
    Pause(I, oneshot::Sender<()>),
    IncreaseVolume(i8, oneshot::Sender<Percentage>),
    SetVolume(u8, oneshot::Sender<Percentage>),
    Seek(Duration, SeekDirection, oneshot::Sender<(Duration, I)>),
    SeekTo(Duration, I, oneshot::Sender<(Duration, I)>),
}

#[derive(Debug)]
pub(crate) enum AsyncRodioResponse {
    ProgressUpdate(Duration),
    StartedPlaying(Option<Duration>),
    StoppedPlaying,
}

#[derive(Debug)]
enum AsyncRodioPlayActionTaken {
    Paused,
    Played,
}

#[derive(Debug, PartialEq)]
pub struct VolumeUpdate(pub Percentage);
#[derive(Debug, PartialEq)]
pub struct ProgressUpdate<I> {
    pub duration: Duration,
    pub identifier: I,
}
#[derive(Debug, PartialEq)]
pub struct Stopped<I>(pub I);
#[derive(Debug, PartialEq)]
pub struct AllStopped;
#[derive(Debug, PartialEq)]
pub struct Resumed<I>(pub I);
#[derive(Debug)]
pub struct Paused<I>(pub I);
#[derive(Debug, PartialEq)]
pub enum PausePlayResponse<I> {
    Paused(I),
    Resumed(I),
}
#[derive(Debug, PartialEq)]
pub enum AutoplayUpdate<I>
where
    I: Debug,
{
    PlayProgress(Duration, I),
    Playing(Option<Duration>, I),
    DonePlaying(I),
}
#[derive(PartialEq, Debug)]
pub enum PlayUpdate<I>
where
    I: Debug,
{
    PlayProgress(Duration, I),
    Playing(Option<Duration>, I),
    DonePlaying(I),
}

impl<I: Debug> From<PlayUpdate<I>> for AutoplayUpdate<I> {
    fn from(update: PlayUpdate<I>) -> Self {
        match update {
            PlayUpdate::PlayProgress(d, id) => AutoplayUpdate::PlayProgress(d, id),
            PlayUpdate::Playing(d, id) => AutoplayUpdate::Playing(d, id),
            PlayUpdate::DonePlaying(id) => AutoplayUpdate::DonePlaying(id),
        }
    }
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
                    AsyncRodioRequest::Stop(song_id, tx) => {
                        state.handle_stop(&sink, song_id, tx);
                    }
                    AsyncRodioRequest::StopAll(tx) => {
                        state.handle_stop_all(&sink, tx);
                    }
                    AsyncRodioRequest::PausePlay(song_id, tx) => {
                        state.handle_pause_play(&sink, song_id, tx);
                    }
                    AsyncRodioRequest::Resume(song_id, tx) => {
                        state.handle_resume(&sink, song_id, tx);
                    }
                    AsyncRodioRequest::Pause(song_id, tx) => {
                        state.handle_pause(&sink, song_id, tx);
                    }
                    AsyncRodioRequest::IncreaseVolume(vol_inc, tx) => {
                        state.handle_increase_volume(&sink, vol_inc, tx);
                    }
                    AsyncRodioRequest::SetVolume(percentage, tx) => {
                        state.handle_set_volume(&sink, percentage, tx);
                    }
                    AsyncRodioRequest::Seek(inc, direction, tx) => {
                        state.handle_seek(&sink, inc, direction, tx);
                    }
                    AsyncRodioRequest::SeekTo(seek_to_pos, song_id, tx) => {
                        state.handle_seek_to(&sink, seek_to_pos, song_id, tx);
                    }
                }
            }
        });

        init_rx
            .recv()
            .context("Audio initialization thread panicked or failed to start")??;

        Ok(Self { tx })
    }

    pub fn autoplay_song(
        &self,
        song: Box<dyn Source<Item = f32> + Send + 'static>,
        identifier: I,
    ) -> impl Stream<Item = AutoplayUpdate<I>> + 'static {
        self.play_song(song, identifier).map(AutoplayUpdate::from)
    }

    pub fn play_song(
        &self,
        song: Box<dyn Source<Item = f32> + Send + 'static>,
        identifier: I,
    ) -> impl Stream<Item = PlayUpdate<I>> + 'static {
        let (tx, mut rx) = mpsc::channel(PLAYER_MSG_QUEUE_SIZE);
        let (streamtx, streamrx) = mpsc::channel(PLAYER_MSG_QUEUE_SIZE);
        let selftx = self.tx.clone();
        let handle = tokio::task::spawn(async move {
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
        PanickingReceiverStream::new(streamrx, handle)
    }

    pub async fn seek(
        &self,
        duration: Duration,
        direction: SeekDirection,
    ) -> Option<ProgressUpdate<I>> {
        let (tx, rx) = oneshot::channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::Seek(duration, direction, tx)).await;
        let Ok((current_duration, song_id)) = rx.await else {
            debug!("The song I tried to seek is no longer playing");
            return None;
        };
        Some(ProgressUpdate {
            duration: current_duration,
            identifier: song_id,
        })
    }

    pub async fn seek_to(&self, seek_to_pos: Duration, id: I) -> Option<ProgressUpdate<I>> {
        let (tx, rx) = oneshot::channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::SeekTo(seek_to_pos, id, tx)).await;
        let Ok((current_duration, song_id)) = rx.await else {
            debug!("The song I tried to seek is no longer playing");
            return None;
        };
        Some(ProgressUpdate {
            duration: current_duration,
            identifier: song_id,
        })
    }

    pub async fn stop(&self, identifier: I) -> Option<Stopped<I>> {
        let (tx, rx) = oneshot::channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::Stop(identifier, tx)).await;
        let Ok(_) = rx.await else {
            debug!("The song I tried to stop is no longer playing");
            return None;
        };
        Some(Stopped(identifier))
    }

    pub async fn stop_all(&self) -> Option<AllStopped> {
        let (tx, rx) = oneshot::channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::StopAll(tx)).await;
        let Ok(_) = rx.await else {
            error!("stop_all sender dropped - unknown reason");
            return None;
        };
        Some(AllStopped)
    }

    pub async fn pause_play(&self, identifier: I) -> Option<PausePlayResponse<I>> {
        let (tx, rx) = oneshot::channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::PausePlay(identifier, tx)).await;
        let Ok(play_action_taken) = rx.await else {
            debug!("The song I tried to pause/play was no longer selected",);
            return None;
        };
        match play_action_taken {
            AsyncRodioPlayActionTaken::Paused => Some(PausePlayResponse::Paused(identifier)),
            AsyncRodioPlayActionTaken::Played => Some(PausePlayResponse::Resumed(identifier)),
        }
    }

    pub async fn pause(&self, identifier: I) -> Option<Paused<I>> {
        let (tx, rx) = oneshot::channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::Pause(identifier, tx)).await;
        let Ok(_) = rx.await else {
            debug!("The song I tried to pause/play was no longer selected",);
            return None;
        };
        Some(Paused(identifier))
    }

    pub async fn resume(&self, identifier: I) -> Option<Resumed<I>> {
        let (tx, rx) = oneshot::channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::Resume(identifier, tx)).await;
        let Ok(_) = rx.await else {
            debug!("The song I tried to pause/play was no longer selected",);
            return None;
        };
        Some(Resumed(identifier))
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
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        let res = self.inner.try_seek(pos);
        if res.is_ok() {
            self.samples = (pos.as_secs_f64() * self.samples_per_sec) as u64;
        }
        res
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
    use std::time::Duration;

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
