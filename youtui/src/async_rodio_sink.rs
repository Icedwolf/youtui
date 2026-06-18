//! Provides an asynchronous handle to a rodio sink, specifically designed to
//! handle gapless playback.
//! This module has been designed to be implemented as a library in future.
use crate::app::structures::Percentage;
use async_callback_manager::PanickingReceiverStream;
use futures::Stream;
use rodio::Source;
use rodio::cpal::FromSample;
use rodio::source::{EmptyCallback, PeriodicAccess, TrackPosition};
use std::borrow::Borrow;
use std::fmt::Debug;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, trace, warn};

pub mod rodio {
    pub use rodio::*;
}

const PROGRESS_UPDATE_DELAY: Duration = Duration::from_millis(100);
const PLAYER_MSG_QUEUE_SIZE: usize = 50;

struct PlaybackState<I> {
    cur_song_duration: Option<Duration>,
    next_song_duration: Option<Duration>,
    cur_song_id: Option<I>,
    next_song_id: Option<I>,
}

impl<I: Debug + PartialEq + Copy> PlaybackState<I> {
    fn handle_autoplay_song<S>(
        &mut self,
        sink: &rodio::Player,
        song: S,
        song_id: I,
        tx: &RodioMpscSender<AsyncRodioResponse>,
    ) where
        S: Source + Send + Sync + 'static,
        f32: FromSample<S::Item>,
        S::Item: Send,
    {
        if Some(song_id) == self.next_song_id {
            info!(
                "Received autoplay for {:?}, it's already queued up. It will play automatically.",
                song_id
            );
            self.cur_song_id = Some(song_id);
            self.next_song_id = None;
            self.cur_song_duration = self.next_song_duration;
            self.next_song_duration = None;
            blocking_send_or_error(tx.0.clone(), AsyncRodioResponse::AutoplayingQueued);
            return;
        }
        if Some(song_id) == self.cur_song_id {
            error!(
                "Received autoplay for {:?}, it's already playing. I was expecting it to be queued up.",
                song_id
            );
            blocking_send_or_error(tx.0.clone(), AsyncRodioResponse::AutoplayingQueued);
            return;
        }
        info!(
            "Autoplaying a song that wasn't queued; clearing queue. Queued: {:?}",
            self.next_song_id
        );
        self.cur_song_duration = song.total_duration();
        tracing::debug!(
            "Received request to autoplay {song_id:?} of duration {:?}",
            self.cur_song_duration
        );
        if !sink.empty() {
            sink.stop()
        }
        let txs = tx.0.clone();
        let song = add_periodic_access(song, PROGRESS_UPDATE_DELAY, move |s| {
            blocking_send_or_error(
                &txs,
                AsyncRodioResponse::ProgressUpdate(s.get_pos()),
            );
        });
        let on_done = on_done_cb(tx);
        sink.append(song);
        sink.append(on_done);
        if sink.is_paused() {
            sink.play();
        }
        debug!("Now playing {:?}", song_id);
        blocking_send_or_error(
            tx.0.clone(),
            AsyncRodioResponse::StartedPlaying(self.cur_song_duration),
        );
        self.cur_song_id = Some(song_id);
        self.next_song_id = None;
        self.next_song_duration = None;
    }

    fn handle_queue_song<S>(
        &mut self,
        sink: &rodio::Player,
        song: S,
        song_id: I,
        tx: &RodioMpscSender<AsyncRodioResponse>,
    ) where
        S: Source + Send + Sync + 'static,
        f32: FromSample<S::Item>,
        S::Item: Send,
    {
        if sink.empty() {
            error!(
                "Tried to queue up a song, but sink was empty... Continuing anyway"
            );
        }
        self.next_song_duration = song.total_duration();
        tracing::debug!(
            "Received request to queue {song_id:?} of duration {:?}",
            self.next_song_duration
        );
        blocking_send_or_error(
            &tx.0,
            AsyncRodioResponse::Queued(self.next_song_duration),
        );
        let txs = tx.0.clone();
        let song = add_periodic_access(song, PROGRESS_UPDATE_DELAY, move |s| {
            blocking_send_or_error(
                &txs,
                AsyncRodioResponse::ProgressUpdate(s.get_pos()),
            );
        });
        let on_done = on_done_cb(tx);
        sink.append(song);
        sink.append(on_done);
        self.next_song_id = Some(song_id);
    }

    fn handle_play_song<S>(
        &mut self,
        sink: &rodio::Player,
        song: S,
        song_id: I,
        tx: &RodioMpscSender<AsyncRodioResponse>,
    ) where
        S: Source + Send + Sync + 'static,
        f32: FromSample<S::Item>,
        S::Item: Send,
    {
        tracing::info!("Inside PlaySong");
        self.cur_song_duration = song.total_duration();
        tracing::info!(
            "Received request to play {song_id:?} of duration {:?}",
            self.cur_song_duration
        );
        if !sink.empty() {
            sink.stop()
        }
        let txs = tx.0.clone();
        let song = add_periodic_access(song, PROGRESS_UPDATE_DELAY, move |s| {
            blocking_send_or_error(
                &txs,
                AsyncRodioResponse::ProgressUpdate(s.get_pos()),
            );
        });
        let on_done = on_done_cb(tx);
        sink.append(song);
        sink.append(on_done);
        if sink.is_paused() {
            sink.play();
        }
        debug!("Now playing {:?}", song_id);
        blocking_send_or_error(
            tx.0.clone(),
            AsyncRodioResponse::StartedPlaying(self.cur_song_duration),
        );
        self.cur_song_id = Some(song_id);
        self.next_song_id = None;
    }

    fn handle_stop(&mut self, sink: &rodio::Player, song_id: I, tx: RodioOneshot<()>) {
        info!("Got message to stop playing {:?}", song_id);
        if self.cur_song_id != Some(song_id) {
            return;
        }
        if !sink.empty() {
            sink.stop()
        }
        self.cur_song_id = None;
        self.next_song_id = None;
        self.cur_song_duration = None;
        oneshot_send_or_error(tx.0, ());
    }

    fn handle_stop_all(&mut self, sink: &rodio::Player, tx: RodioOneshot<()>) {
        info!("Got message to stop playing all");
        if !sink.empty() {
            sink.stop()
        }
        self.cur_song_id = None;
        self.next_song_id = None;
        self.cur_song_duration = None;
        oneshot_send_or_error(tx.0, ());
    }

    fn handle_pause_play(&mut self, sink: &rodio::Player, song_id: I, tx: RodioOneshot<AsyncRodioPlayActionTaken>) {
        info!("Got message to pause / play {:?}", song_id);
        if self.cur_song_id != Some(song_id) {
            return;
        }
        if sink.is_paused() {
            sink.play();
            info!("Sending Play message {:?}", song_id);
            oneshot_send_or_error(tx.0, AsyncRodioPlayActionTaken::Played);
        } else if !sink.is_paused() && !sink.empty() {
            sink.pause();
            info!("Sending Pause message {:?}", song_id);
            oneshot_send_or_error(tx.0, AsyncRodioPlayActionTaken::Paused);
        }
    }

    fn handle_resume(&mut self, sink: &rodio::Player, song_id: I, tx: RodioOneshot<()>) {
        info!("Got message to resume {:?}", song_id);
        if self.cur_song_id != Some(song_id) {
            return;
        }
        if sink.is_paused() {
            sink.play();
            info!("Sending Played message {:?}", song_id);
            oneshot_send_or_error(tx.0, ());
        }
    }

    fn handle_pause(&mut self, sink: &rodio::Player, song_id: I, tx: RodioOneshot<()>) {
        info!("Got message to pause {:?}", song_id);
        if self.cur_song_id != Some(song_id) {
            return;
        }
        if !sink.is_paused() && !sink.empty() {
            sink.pause();
            info!("Sending Paused message {:?}", song_id);
            oneshot_send_or_error(tx.0, ());
        }
    }

    fn handle_increase_volume(&self, sink: &rodio::Player, vol_inc: i8, tx: RodioOneshot<Percentage>) {
        sink.set_volume((sink.volume() + vol_inc as f32 / 100.0).clamp(0.0, 1.0));
        oneshot_send_or_error(
            tx.0,
            Percentage((sink.volume() * 100.0).round() as u8),
        );
        info!("Rodio sent volume update");
    }

    fn handle_set_volume(&self, sink: &rodio::Player, percentage: u8, tx: RodioOneshot<Percentage>) {
        sink.set_volume((percentage as f32 / 100.0).clamp(0.0, 1.0));
        oneshot_send_or_error(
            tx.0,
            Percentage((sink.volume() * 100.0).round() as u8),
        );
        info!("Rodio sent volume update");
    }

    fn handle_seek(&mut self, sink: &rodio::Player, inc: Duration, direction: SeekDirection, tx: RodioOneshot<(Duration, I)>) {
        info!("Got request to seek {inc:?} in direction {direction:?}");
        let Some(cur_song_id) = self.cur_song_id else {
            warn!("Tried to seek, but no song loaded");
            return;
        };
        let cur_pos = sink.get_pos();
        let new_pos = match direction {
            SeekDirection::Forward => cur_pos
                .saturating_add(inc)
                .min(self.cur_song_duration.unwrap_or_default()),
            SeekDirection::Back => cur_pos
                .saturating_sub(inc)
                .min(self.cur_song_duration.unwrap_or_default()),
        };
        debug!(
            "Executing seek request of {inc:?} in direction {direction:?}. \
             Song with ID {cur_song_id:?} will move from pos {cur_pos:?} to pos {new_pos:?}"
        );
        if let Err(e) = sink.try_seek(new_pos) {
            error!("Failed to seek {:?}", e);
        }
        std::thread::sleep(Duration::from_millis(5));
        oneshot_send_or_error(tx.0, (sink.get_pos(), cur_song_id));
    }

    fn handle_seek_to(&mut self, sink: &rodio::Player, seek_to_pos: Duration, song_id: I, tx: RodioOneshot<(Duration, I)>) {
        info!(
            "Got message to seek to {:?} in song {:?}",
            seek_to_pos, song_id
        );
        if self.cur_song_id != Some(song_id) {
            return;
        }
        let res = sink.try_seek(seek_to_pos.min(self.cur_song_duration.unwrap_or_default()));
        if let Err(e) = res {
            error!("Failed to seek {:?}", e);
        }
        std::thread::sleep(Duration::from_millis(5));
        oneshot_send_or_error(tx.0, (sink.get_pos(), song_id));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SeekDirection {
    Forward,
    Back,
}

#[derive(Debug)]
enum AsyncRodioRequest<S, I> {
    PlaySong(S, I, RodioMpscSender<AsyncRodioResponse>),
    AutoplaySong(S, I, RodioMpscSender<AsyncRodioResponse>),
    QueueSong(S, I, RodioMpscSender<AsyncRodioResponse>),
    Stop(I, RodioOneshot<()>),
    StopAll(RodioOneshot<()>),
    PausePlay(I, RodioOneshot<AsyncRodioPlayActionTaken>),
    Resume(I, RodioOneshot<()>),
    Pause(I, RodioOneshot<()>),
    IncreaseVolume(i8, RodioOneshot<Percentage>),
    SetVolume(u8, RodioOneshot<Percentage>),
    Seek(Duration, SeekDirection, RodioOneshot<(Duration, I)>),
    SeekTo(Duration, I, RodioOneshot<(Duration, I)>),
}
#[derive(Debug)]
pub(crate) enum AsyncRodioResponse {
    ProgressUpdate(Duration),
    StartedPlaying(Option<Duration>),
    Queued(Option<Duration>),
    AutoplayingQueued,
    StoppedPlaying,
}
/// The action rodio took when it received a PausePlay message.
#[derive(Debug)]
enum AsyncRodioPlayActionTaken {
    Paused,
    Played,
}

/// Newtype for oneshot sender with custom debug implementation.
struct RodioOneshot<T>(oneshot::Sender<T>);
fn rodio_oneshot_channel<T>() -> (RodioOneshot<T>, oneshot::Receiver<T>) {
    let (tx, rx) = oneshot::channel();
    (RodioOneshot(tx), rx)
}
impl<T> Debug for RodioOneshot<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Oneshot channel - {}", std::any::type_name::<T>())
    }
}

/// Newtype for mpsc sender with custom debug implementation.
struct RodioMpscSender<T>(mpsc::Sender<T>);
fn rodio_mpsc_channel<T>(buffer: usize) -> (RodioMpscSender<T>, mpsc::Receiver<T>) {
    let (tx, rx) = mpsc::channel(buffer);
    (RodioMpscSender(tx), rx)
}
impl<T> Debug for RodioMpscSender<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mpsc channel - {}", std::any::type_name::<T>())
    }
}
impl<T> From<RodioOneshot<T>> for oneshot::Sender<T> {
    fn from(value: RodioOneshot<T>) -> Self {
        value.0
    }
}

#[derive(Debug, PartialEq)]
pub struct VolumeUpdate(pub Percentage);
#[derive(Debug, PartialEq)]
pub struct ProgressUpdate<I> {
    pub duration: Duration,
    pub identifier: I,
}
// NOTE: At this stage this difference between DonePlaying and Stopped is
// very thin. DonePlaying means that the song has been dropped by the player,
// whereas Stopped simply means that a Stop message to the player was succesful.
#[derive(Debug, PartialEq)]
pub struct Stopped<I>(pub I);
/// Message to say that playback has stopped - all songs.
#[derive(Debug, PartialEq)]
pub struct AllStopped;
#[derive(Debug, PartialEq)]
pub struct Resumed<I>(pub I);
#[derive(Debug)]
pub struct Paused<I>(pub I);
// This is different to Paused and Resumed, as a PausePlay message could return
// either.
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
    AutoplayQueued(I),
    Error(String),
}
#[derive(PartialEq, Debug)]
pub enum PlayUpdate<I>
where
    I: Debug,
{
    PlayProgress(Duration, I),
    Playing(Option<Duration>, I),
    DonePlaying(I),
    Error(String),
}
#[derive(Debug, PartialEq)]
pub enum QueueUpdate<I>
where
    I: Debug,
{
    PlayProgress(Duration, I),
    Queued(Option<Duration>, I),
    DonePlaying(I),
    Error(String),
}

pub struct AsyncRodio<S, I>
where
    I: Debug,
{
    _handle: tokio::task::JoinHandle<()>,
    tx: std::sync::mpsc::Sender<AsyncRodioRequest<S, I>>,
}

impl<S, I> Default for AsyncRodio<S, I>
where
    S: Source + Send + Sync + 'static,
    f32: FromSample<S::Item>,
    S::Item: Send,
    I: Debug + PartialEq + Copy + Send + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<S, I> AsyncRodio<S, I>
where
    S: Source + Send + Sync + 'static,
    f32: FromSample<S::Item>,
    S::Item: Send,
    I: Debug + PartialEq + Copy + Send + 'static,
{
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<AsyncRodioRequest<S, I>>();
        let _handle = tokio::task::spawn_blocking(move || {
            // With "tracing" feature rodio logs through tracing::error! instead
            // of eprintln!, but tracing-log bridges those to the `log` crate,
            // which tui_logger::init_logger() captures regardless of the tracing
            // subscriber's Target filter. Use a custom callback at trace level
            // so ALSA "Buffer underrun/overrun" messages stay out of the TUI.
            // Buffer is forced to 4096 samples (~93ms @ 44.1kHz) — some ALSA
            // configs report tiny defaults that cause repeated underruns.
            let mut mixer_device_sink = rodio::DeviceSinkBuilder::from_default_device()
                .expect("Expect default audio device")
                .with_buffer_size(rodio::cpal::BufferSize::Fixed(4096))
                .with_error_callback(|err| tracing::trace!("audio stream error: {err}"))
                .open_sink_or_fallback()
                .expect("Expect to get a handle to output stream");
            mixer_device_sink.log_on_drop(false);
            let sink = rodio::Player::connect_new(mixer_device_sink.mixer());
            // Hopefully someone else can't create a song with the same ID?!
            let mut state = PlaybackState {
                cur_song_duration: None,
                next_song_duration: None,
                cur_song_id: None,
                next_song_id: None,
            };
            // There is no need for a drop implementation on AsyncRodio, since if AsyncRodio
            // has dropped with it's sender, receive loop will receive Err and end.
            while let Ok(msg) = rx.recv() {
                match msg {
                    AsyncRodioRequest::AutoplaySong(song, song_id, tx) => {
                        state.handle_autoplay_song(&sink, song, song_id, &tx);
                    }
                    AsyncRodioRequest::QueueSong(song, song_id, tx) => {
                        state.handle_queue_song(&sink, song, song_id, &tx);
                    }
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
        Self { _handle, tx }
    }
    pub fn autoplay_song(
        &self,
        song: S,
        identifier: I,
    ) -> impl Stream<Item = AutoplayUpdate<I>> + use<S, I> {
        let (tx, mut rx) = rodio_mpsc_channel(PLAYER_MSG_QUEUE_SIZE);
        let (streamtx, streamrx) = tokio::sync::mpsc::channel(PLAYER_MSG_QUEUE_SIZE);
        let selftx = self.tx.clone();
        let handle = tokio::task::spawn(async move {
            std_send_or_error(
                selftx,
                AsyncRodioRequest::AutoplaySong(song, identifier, tx),
            )
            .await;
            while let Some(msg) = rx.recv().await {
                match msg {
                    AsyncRodioResponse::ProgressUpdate(duration) => {
                        send_or_error(
                            &streamtx,
                            AutoplayUpdate::PlayProgress(duration, identifier),
                        )
                        .await;
                    }
                    AsyncRodioResponse::Queued(_) => {
                        send_or_error(
                            &streamtx,
                            AutoplayUpdate::Error(format!(
                                "Received queued message, but I wasn't queued... {identifier:?}"
                            )),
                        )
                        .await;
                    }
                    // This is the case where the song we asked to play is already
                    // queued. In this case, this task can finish, as the task that
                    // added the song to the queue is responsible for the playback
                    // updates.
                    AsyncRodioResponse::AutoplayingQueued => {
                        send_or_error(&streamtx, AutoplayUpdate::AutoplayQueued(identifier)).await;
                        return;
                    }
                    AsyncRodioResponse::StartedPlaying(duration) => {
                        info!("audio_output_started: song_id={:?}, duration={:?}", identifier, duration);
                        send_or_error(&streamtx, AutoplayUpdate::Playing(duration, identifier))
                            .await;
                    }
                    AsyncRodioResponse::StoppedPlaying => {
                        send_or_error(&streamtx, AutoplayUpdate::DonePlaying(identifier)).await;
                        return;
                    }
                }
            }
            // Channel closed during shutdown - this is expected on app exit
            info!(
                "Playback channel closed for {:?} before final status received",
                identifier
            );
        });
        PanickingReceiverStream::new(streamrx, handle)
    }
    pub fn queue_song(
        &self,
        song: S,
        identifier: I,
    ) -> impl Stream<Item = QueueUpdate<I>> + use<S, I> {
        let (tx, mut rx) = rodio_mpsc_channel(PLAYER_MSG_QUEUE_SIZE);
        let (streamtx, streamrx) = tokio::sync::mpsc::channel(PLAYER_MSG_QUEUE_SIZE);
        let selftx = self.tx.clone();
        let handle = tokio::task::spawn(async move {
            std_send_or_error(selftx, AsyncRodioRequest::QueueSong(song, identifier, tx)).await;
            while let Some(msg) = rx.recv().await {
                match msg {
                    AsyncRodioResponse::ProgressUpdate(duration) => {
                        send_or_error(&streamtx, QueueUpdate::PlayProgress(duration, identifier))
                            .await;
                    }
                    AsyncRodioResponse::Queued(duration) => {
                        send_or_error(&streamtx, QueueUpdate::Queued(duration, identifier)).await;
                    }
                    AsyncRodioResponse::AutoplayingQueued => {
                        send_or_error(
                            &streamtx,
                            QueueUpdate::Error(format!(
                                "Received AutoPlayingQueued message, but I asked to queue... {identifier:?}"
                            )),
                        )
                        .await;
                    }
                    AsyncRodioResponse::StartedPlaying(_) => {
                        send_or_error(
                            &streamtx,
                            QueueUpdate::Error(format!(
                                "Received StartedPlaying message, but I asked to queue... {identifier:?}",
                            )),
                        )
                        .await;
                    }
                    AsyncRodioResponse::StoppedPlaying => {
                        send_or_error(&streamtx, QueueUpdate::DonePlaying(identifier)).await;
                        return;
                    }
                }
            }
            // Channel closed during shutdown - this is expected on app exit
            info!(
                "Playback channel closed for {:?} before final status received",
                identifier
            );
        });
        PanickingReceiverStream::new(streamrx, handle)
    }
    pub fn play_song(
        &self,
        song: S,
        identifier: I,
    ) -> impl Stream<Item = PlayUpdate<I>> + use<S, I> {
        let (tx, mut rx) = rodio_mpsc_channel(PLAYER_MSG_QUEUE_SIZE);
        let (streamtx, streamrx) = tokio::sync::mpsc::channel(PLAYER_MSG_QUEUE_SIZE);
        let selftx = self.tx.clone();
        let handle = tokio::task::spawn(async move {
            std_send_or_error(selftx, AsyncRodioRequest::PlaySong(song, identifier, tx)).await;
            while let Some(msg) = rx.recv().await {
                trace!("Received {msg:?}");
                match msg {
                    AsyncRodioResponse::ProgressUpdate(duration) => {
                        send_or_error(&streamtx, PlayUpdate::PlayProgress(duration, identifier))
                            .await;
                    }
                    AsyncRodioResponse::Queued(_) => {
                        send_or_error(
                            &streamtx,
                            PlayUpdate::Error(format!(
                                "Received Queued message, but I wasn't queued... {identifier:?}"
                            )),
                        )
                        .await;
                    }
                    AsyncRodioResponse::AutoplayingQueued => {
                        send_or_error(
                            &streamtx,
                            PlayUpdate::Error(format!(
                                "Received AutoPlayingQueued message, but I asked to play... {identifier:?}"
                            )),
                        )
                        .await;
                    }
                    AsyncRodioResponse::StartedPlaying(duration) => {
                        info!("audio_output_started: song_id={:?}, duration={:?}", identifier, duration);
                        send_or_error(&streamtx, PlayUpdate::Playing(duration, identifier)).await;
                    }
                    AsyncRodioResponse::StoppedPlaying => {
                        send_or_error(&streamtx, PlayUpdate::DonePlaying(identifier)).await;
                        return;
                    }
                }
            }
            // Channel closed during shutdown - this is expected on app exit
            info!(
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
        let (tx, rx) = rodio_oneshot_channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::Seek(duration, direction, tx)).await;
        let Ok((current_duration, song_id)) = rx.await else {
            // This happens intentionally - when a seek is requested for a song
            // but all songs have finished, instead of sending a reply, rodio will drop
            // sender.
            info!("The song I tried to seek is no longer playing");
            return None;
        };
        Some(ProgressUpdate {
            duration: current_duration,
            identifier: song_id,
        })
    }
    pub async fn seek_to(&self, seek_to_pos: Duration, id: I) -> Option<ProgressUpdate<I>> {
        let (tx, rx) = rodio_oneshot_channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::SeekTo(seek_to_pos, id, tx)).await;
        let Ok((current_duration, song_id)) = rx.await else {
            // This happens intentionally - when a seek is requested for a song
            // that's no longer playing, instead of sending a reply, rodio will drop
            // sender.
            info!("The song I tried to seek is no longer playing");
            return None;
        };
        Some(ProgressUpdate {
            duration: current_duration,
            identifier: song_id,
        })
    }
    pub async fn stop(&self, identifier: I) -> Option<Stopped<I>> {
        let (tx, rx) = rodio_oneshot_channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::Stop(identifier, tx)).await;
        let Ok(_) = rx.await else {
            // This happens intentionally - when a stop is requested for a song
            // that's no longer playing, instead of sending a reply, rodio will drop sender.
            info!("The song I tried to stop is no longer playing");
            return None;
        };
        Some(Stopped(identifier))
    }
    pub async fn stop_all(&self) -> Option<AllStopped> {
        let (tx, rx) = rodio_oneshot_channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::StopAll(tx)).await;
        let Ok(_) = rx.await else {
            // Should never happen!
            error!("stop_all sender dropped - unknown reason");
            return None;
        };
        Some(AllStopped)
    }
    pub async fn pause_play(&self, identifier: I) -> Option<PausePlayResponse<I>> {
        let (tx, rx) = rodio_oneshot_channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::PausePlay(identifier, tx)).await;
        let Ok(play_action_taken) = rx.await else {
            // This happens intentionally - when a pauseplay is requested for a song
            // that's no longer playing, instead of sending a reply, rodio will drop sender.
            info!("The song I tried to pause/play was no longer selected",);
            return None;
        };
        match play_action_taken {
            AsyncRodioPlayActionTaken::Paused => Some(PausePlayResponse::Paused(identifier)),
            AsyncRodioPlayActionTaken::Played => Some(PausePlayResponse::Resumed(identifier)),
        }
    }
    pub async fn pause(&self, identifier: I) -> Option<Paused<I>> {
        let (tx, rx) = rodio_oneshot_channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::Pause(identifier, tx)).await;
        let Ok(_) = rx.await else {
            // This happens intentionally - when a pauseplay is requested for a song
            // that's no longer playing, instead of sending a reply, rodio will drop sender.
            info!("The song I tried to pause/play was no longer selected",);
            return None;
        };
        Some(Paused(identifier))
    }
    pub async fn resume(&self, identifier: I) -> Option<Resumed<I>> {
        let (tx, rx) = rodio_oneshot_channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::Resume(identifier, tx)).await;
        let Ok(_) = rx.await else {
            // This happens intentionally - when a pauseplay is requested for a song
            // that's no longer playing, instead of sending a reply, rodio will drop sender.
            info!("The song I tried to pause/play was no longer selected",);
            return None;
        };
        Some(Resumed(identifier))
    }
    pub async fn increase_volume(&self, vol_inc: i8) -> Option<VolumeUpdate> {
        let (tx, rx) = rodio_oneshot_channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::IncreaseVolume(vol_inc, tx)).await;
        let Ok(current_volume) = rx.await else {
            // Should never happen!
            error!("The player has been dropped while I was waiting for a volume update for",);
            return None;
        };
        Some(VolumeUpdate(current_volume))
    }
    pub async fn set_volume(&self, new_vol: u8) -> Option<VolumeUpdate> {
        let (tx, rx) = rodio_oneshot_channel();
        std_send_or_error(&self.tx, AsyncRodioRequest::SetVolume(new_vol, tx)).await;
        let Ok(current_volume) = rx.await else {
            // Should never happen!
            error!("The player has been dropped while I was waiting for a volume update for",);
            return None;
        };
        Some(VolumeUpdate(current_volume))
    }
}



/// Specific helper function to generate a source that sends a stopped playing
/// message to the sender.
fn on_done_cb(tx: &RodioMpscSender<AsyncRodioResponse>) -> EmptyCallback {
    let tx = tx.0.clone();
    let cb = move || {
        blocking_send_or_error(&tx, AsyncRodioResponse::StoppedPlaying);
    };
    EmptyCallback::new(Box::new(cb))
}

/// Add a periodic access callback to song.
fn add_periodic_access<S>(
    song: S,
    interval: Duration,
    callback: impl FnMut(&mut TrackPosition<S>),
) -> PeriodicAccess<TrackPosition<S>, impl FnMut(&mut TrackPosition<S>)>
where
    S: Source + Send + Sync + 'static,
    f32: FromSample<S::Item>,
    S::Item: Send,
{
    song.track_position().periodic_access(interval, callback)
}

/* #### BELOW CODE COPIED FROM youtui::core #### */
/// Send a message to the specified Tokio mpsc::Sender, and if sending fails,
/// log an error with Tracing.
pub async fn send_or_error<T, S: Borrow<mpsc::Sender<T>>>(tx: S, msg: T) {
    tx.borrow()
        .send(msg)
        .await
        .unwrap_or_else(|e| error!("Error {e} received when sending message"));
}
pub async fn std_send_or_error<T, S: Borrow<std::sync::mpsc::Sender<T>>>(tx: S, msg: T) {
    tx.borrow()
        .send(msg)
        .unwrap_or_else(|e| error!("Error {e} received when sending message"));
}
/// Send a message to the specified Tokio mpsc::Sender, and if sending fails,
/// log an error with Tracing.
pub fn blocking_send_or_error<T, S: Borrow<mpsc::Sender<T>>>(tx: S, msg: T) {
    tx.borrow()
        .blocking_send(msg)
        .unwrap_or_else(|e| error!("Error {e} received when sending message"));
}
/// Send a message to the specified Tokio oneshot::Sender, and if sending fails,
/// log an error with Tracing.
pub fn oneshot_send_or_error<T: Debug, S: Into<oneshot::Sender<T>>>(tx: S, msg: T) {
    tx.into()
        .send(msg)
        .unwrap_or_else(|e| error!("Error received when sending message {:?}", e));
}
/* #### ABOVE CODE COPIED FROM youtui::core #### */

#[allow(dead_code)]
pub(crate) fn map_to_play_update<I: Debug + PartialEq + Copy>(msg: AsyncRodioResponse, id: I) -> PlayUpdate<I> {
    match msg {
        AsyncRodioResponse::ProgressUpdate(d) => PlayUpdate::PlayProgress(d, id),
        AsyncRodioResponse::Queued(_) => PlayUpdate::Error("Received Queued message, but I wasn't queued...".into()),
        AsyncRodioResponse::AutoplayingQueued => PlayUpdate::Error("Received AutoPlayingQueued message, but I asked to play...".into()),
        AsyncRodioResponse::StartedPlaying(d) => PlayUpdate::Playing(d, id),
        AsyncRodioResponse::StoppedPlaying => PlayUpdate::DonePlaying(id),
    }
}

#[allow(dead_code)]
pub(crate) fn map_to_queue_update<I: Debug + PartialEq + Copy>(msg: AsyncRodioResponse, id: I) -> QueueUpdate<I> {
    match msg {
        AsyncRodioResponse::ProgressUpdate(d) => QueueUpdate::PlayProgress(d, id),
        AsyncRodioResponse::Queued(d) => QueueUpdate::Queued(d, id),
        AsyncRodioResponse::AutoplayingQueued => QueueUpdate::Error("Received AutoPlayingQueued message, but I asked to queue...".into()),
        AsyncRodioResponse::StartedPlaying(_) => QueueUpdate::Error("Received StartedPlaying message, but I asked to queue...".into()),
        AsyncRodioResponse::StoppedPlaying => QueueUpdate::DonePlaying(id),
    }
}

#[allow(dead_code)]
pub(crate) fn map_to_autoplay_update<I: Debug + PartialEq + Copy>(msg: AsyncRodioResponse, id: I) -> AutoplayUpdate<I> {
    match msg {
        AsyncRodioResponse::ProgressUpdate(d) => AutoplayUpdate::PlayProgress(d, id),
        AsyncRodioResponse::Queued(_) => AutoplayUpdate::Error("Received queued message, but I wasn't queued...".into()),
        AsyncRodioResponse::AutoplayingQueued => AutoplayUpdate::AutoplayQueued(id),
        AsyncRodioResponse::StartedPlaying(d) => AutoplayUpdate::Playing(d, id),
        AsyncRodioResponse::StoppedPlaying => AutoplayUpdate::DonePlaying(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn map_play_update_progress() {
        let id = 42u64;
        let result = map_to_play_update(AsyncRodioResponse::ProgressUpdate(Duration::from_secs(5)), id);
        assert_eq!(result, PlayUpdate::PlayProgress(Duration::from_secs(5), 42));
    }

    #[test]
    fn map_play_update_queued_is_error() {
        let result = map_to_play_update(AsyncRodioResponse::Queued(Some(Duration::from_secs(10))), 1u64);
        assert!(matches!(result, PlayUpdate::Error(_)));
    }

    #[test]
    fn map_play_update_autoplay_queued_is_error() {
        let result = map_to_play_update(AsyncRodioResponse::AutoplayingQueued, 1u64);
        assert!(matches!(result, PlayUpdate::Error(_)));
    }

    #[test]
    fn map_play_update_started_playing() {
        let result = map_to_play_update(AsyncRodioResponse::StartedPlaying(Some(Duration::from_secs(30))), 2u64);
        assert_eq!(result, PlayUpdate::Playing(Some(Duration::from_secs(30)), 2));
    }

    #[test]
    fn map_play_update_started_playing_none() {
        let result = map_to_play_update(AsyncRodioResponse::StartedPlaying(None), 2u64);
        assert_eq!(result, PlayUpdate::Playing(None, 2));
    }

    #[test]
    fn map_play_update_stopped() {
        let result = map_to_play_update(AsyncRodioResponse::StoppedPlaying, 3u64);
        assert_eq!(result, PlayUpdate::DonePlaying(3));
    }

    #[test]
    fn map_queue_update_progress() {
        let result = map_to_queue_update(AsyncRodioResponse::ProgressUpdate(Duration::from_secs(5)), 1u64);
        assert_eq!(result, QueueUpdate::PlayProgress(Duration::from_secs(5), 1));
    }

    #[test]
    fn map_queue_update_queued() {
        let result = map_to_queue_update(AsyncRodioResponse::Queued(Some(Duration::from_secs(10))), 1u64);
        assert_eq!(result, QueueUpdate::Queued(Some(Duration::from_secs(10)), 1));
    }

    #[test]
    fn map_queue_update_queued_none() {
        let result = map_to_queue_update(AsyncRodioResponse::Queued(None), 1u64);
        assert_eq!(result, QueueUpdate::Queued(None, 1));
    }

    #[test]
    fn map_queue_update_autoplay_queued_is_error() {
        let result = map_to_queue_update(AsyncRodioResponse::AutoplayingQueued, 1u64);
        assert!(matches!(result, QueueUpdate::Error(_)));
    }

    #[test]
    fn map_queue_update_started_playing_is_error() {
        let result = map_to_queue_update(AsyncRodioResponse::StartedPlaying(Some(Duration::from_secs(30))), 1u64);
        assert!(matches!(result, QueueUpdate::Error(_)));
    }

    #[test]
    fn map_queue_update_stopped() {
        let result = map_to_queue_update(AsyncRodioResponse::StoppedPlaying, 2u64);
        assert_eq!(result, QueueUpdate::DonePlaying(2));
    }

    #[test]
    fn map_autoplay_update_progress() {
        let result = map_to_autoplay_update(AsyncRodioResponse::ProgressUpdate(Duration::from_millis(500)), 1u64);
        assert_eq!(result, AutoplayUpdate::PlayProgress(Duration::from_millis(500), 1));
    }

    #[test]
    fn map_autoplay_update_queued_is_error() {
        let result = map_to_autoplay_update(AsyncRodioResponse::Queued(Some(Duration::from_secs(10))), 1u64);
        assert!(matches!(result, AutoplayUpdate::Error(_)));
    }

    #[test]
    fn map_autoplay_update_autoplay_queued() {
        let result = map_to_autoplay_update(AsyncRodioResponse::AutoplayingQueued, 1u64);
        assert_eq!(result, AutoplayUpdate::AutoplayQueued(1));
    }

    #[test]
    fn map_autoplay_update_started_playing() {
        let result = map_to_autoplay_update(AsyncRodioResponse::StartedPlaying(Some(Duration::from_secs(30))), 2u64);
        assert_eq!(result, AutoplayUpdate::Playing(Some(Duration::from_secs(30)), 2));
    }

    #[test]
    fn map_autoplay_update_stopped() {
        let result = map_to_autoplay_update(AsyncRodioResponse::StoppedPlaying, 3u64);
        assert_eq!(result, AutoplayUpdate::DonePlaying(3));
    }

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
