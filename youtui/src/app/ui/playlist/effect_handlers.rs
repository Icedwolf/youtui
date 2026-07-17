use crate::app::component::actionhandler::ComponentEffect;
use crate::app::server::{ArcServer, DownloadProgressUpdate, TaskMetadata};
use crate::app::structures::ListSongID;
use crate::app::ui::playlist::Playlist;
use crate::async_rodio_sink::{
    AllStopped, AutoplayUpdate, PausePlayResponse, Paused, PlayUpdate, ProgressUpdate, Resumed,
    Stopped, VolumeUpdate,
};
use async_callback_manager::{AsyncTask, FrontendEffect};
use std::fmt::Debug;
use tracing::debug;
use ytmapi_rs::common::{VideoID, YoutubeID};

#[derive(Debug, PartialEq)]
pub struct HandleAllStopped;
#[derive(Debug, PartialEq)]
pub struct HandleStopped;
#[derive(Debug, PartialEq)]
pub struct HandleSetSongPlayProgress;
#[derive(Debug, PartialEq)]
pub struct HandleVolumeUpdate;
#[derive(Debug, PartialEq)]
pub struct HandlePausePlayResponse;
#[derive(Debug, PartialEq)]
pub struct HandleResumeResponse;
#[derive(Debug, PartialEq)]
pub struct HandlePausedResponse;
#[derive(Debug, PartialEq, Clone)]
pub struct HandlePlayUpdateOk;
#[derive(Debug, PartialEq, Clone)]
pub struct HandleAutoplayUpdateOk;
#[derive(Debug, PartialEq, Clone)]
pub struct HandleSongDownloadProgressUpdate(pub ListSongID);
#[derive(Debug, PartialEq)]
pub struct HandleResolveAudioResult(pub ListSongID);

#[derive(Debug, PartialEq)]
enum PlaylistEffect {
    SetStatusStopped(AllStopped),
    StopSongID(Stopped<ListSongID>),
    HandleSetSongPlayProgress(ProgressUpdate<ListSongID>),
    HandleVolumeUpdate(VolumeUpdate),
    HandlePausePlayResponse(PausePlayResponse<ListSongID>),
    HandleResumed(ListSongID),
    HandlePaused(ListSongID),
    HandleAutoplayUpdate(AutoplayUpdate<ListSongID>),
    HandleSongDownloadProgressUpdate(DownloadProgressUpdate, ListSongID),
    HandleResolveAudioResult(Option<VideoID<'static>>, ListSongID),
}
impl_youtui_task_handler!(HandleStopped, Stopped<ListSongID>, Playlist, |_, input| {
    PlaylistEffect::StopSongID(input)
});
impl_youtui_task_handler!(HandleAllStopped, AllStopped, Playlist, |_, input| {
    PlaylistEffect::SetStatusStopped(input)
});
impl_youtui_task_handler!(
    HandleSetSongPlayProgress,
    ProgressUpdate<ListSongID>,
    Playlist,
    |_, input| PlaylistEffect::HandleSetSongPlayProgress(input)
);
impl_youtui_task_handler!(HandleVolumeUpdate, VolumeUpdate, Playlist, |_, input| {
    PlaylistEffect::HandleVolumeUpdate(input)
});
impl_youtui_task_handler!(
    HandlePlayUpdateOk,
    PlayUpdate<ListSongID>,
    Playlist,
    |_, input: PlayUpdate<ListSongID>| { PlaylistEffect::HandleAutoplayUpdate(input.into()) }
);
impl_youtui_task_handler!(
    HandleAutoplayUpdateOk,
    AutoplayUpdate<ListSongID>,
    Playlist,
    |_, input| PlaylistEffect::HandleAutoplayUpdate(input)
);
impl_youtui_task_handler!(
    HandleSongDownloadProgressUpdate,
    DownloadProgressUpdate,
    Playlist,
    |this: HandleSongDownloadProgressUpdate, input| {
        PlaylistEffect::HandleSongDownloadProgressUpdate(input, this.0)
    }
);
impl_youtui_task_handler!(
    HandlePausePlayResponse,
    PausePlayResponse<ListSongID>,
    Playlist,
    |_, input| PlaylistEffect::HandlePausePlayResponse(input)
);
impl_youtui_task_handler!(
    HandleResumeResponse,
    Resumed<ListSongID>,
    Playlist,
    |_, input: Resumed<_>| PlaylistEffect::HandleResumed(input.0)
);
impl_youtui_task_handler!(
    HandlePausedResponse,
    Paused<ListSongID>,
    Playlist,
    |_, input: Paused<_>| PlaylistEffect::HandlePaused(input.0)
);
impl_youtui_task_handler!(
    HandleResolveAudioResult,
    Option<VideoID<'static>>,
    Playlist,
    |this: HandleResolveAudioResult, input| {
        PlaylistEffect::HandleResolveAudioResult(input, this.0)
    }
);

impl FrontendEffect<Playlist, ArcServer, TaskMetadata> for PlaylistEffect {
    fn apply(self, target: &mut Playlist) -> impl Into<ComponentEffect<Playlist>> {
        match self {
            PlaylistEffect::SetStatusStopped(msg) => {
                target.handle_all_stopped(msg);
            }
            PlaylistEffect::StopSongID(msg) => {
                target.handle_stopped(msg);
            }
            PlaylistEffect::HandlePausePlayResponse(msg) => {
                // Logic could go in handler instead.
                match msg {
                    PausePlayResponse::Paused(id) => target.handle_paused(id),
                    PausePlayResponse::Resumed(id) => target.handle_resumed(id),
                };
            }
            PlaylistEffect::HandleResumed(msg) => target.handle_resumed(msg),
            PlaylistEffect::HandlePaused(msg) => target.handle_paused(msg),
            PlaylistEffect::HandleSetSongPlayProgress(msg) => {
                return target.handle_set_song_play_progress(msg.duration, msg.identifier);
            }
            PlaylistEffect::HandleVolumeUpdate(msg) => target.handle_volume_update(msg),
            PlaylistEffect::HandleAutoplayUpdate(msg) => {
                return target.handle_autoplay_update(msg);
            }
            PlaylistEffect::HandleSongDownloadProgressUpdate(update, id) => {
                return target.handle_song_download_progress_update(update, id);
            }
            PlaylistEffect::HandleResolveAudioResult(resolved, id) => {
                if let Some(new_video_id) = resolved
                    && let Some(idx) = target.get_index_from_id(id)
                    && let Some(song) = target.list.get_list_iter_mut().nth(idx)
                {
                    debug!(
                        old = song.video_id.get_raw(),
                        new = new_video_id.get_raw(),
                        "Resolved queue song to Atv version"
                    );
                    song.video_id = new_video_id;
                }
                target.resolve_remaining = target.resolve_remaining.saturating_sub(1);
                if target.resolve_remaining == 0 {
                    target.resolving_audio = false;
                }
                return AsyncTask::new_no_op();
            }
        }
        AsyncTask::new_no_op()
    }
}
