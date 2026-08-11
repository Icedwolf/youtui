use super::action::AppAction;
use crate::app::component::actionhandler::{
    Action, ActionHandler, Component, KeyRouter, Scrollable, TextHandler, YoutuiEffect,
};
use crate::app::effect::{Effects, MutationFn};
use crate::app::queue_persistence;
use crate::app::server::song_downloader::{
    cache_clear, create_decoder_from_cache, download_and_decode,
};
use crate::app::structures::{
    BrowserSongsList, DownloadStatus, ListSong, ListSongDisplayableField, ListSongID,
    Percentage, PlayState, SongListComponent,
};
use crate::app::ui::{AppCallback, WindowContext};
use crate::app::view::draw::{draw_loadable, draw_panel_mut, draw_table};
use crate::app::view::{BasicConstraint, DrawableMut, HasTitle, Loadable, TableView};
use crate::async_rodio_sink::{AllStopped, PlayUpdate, Stopped, VolumeUpdate};
use futures::{Stream, StreamExt};
use crate::config::Config;
use crate::config::keymap::Keymap;
use crate::core::PoisonRecovery;
use crate::widgets::ScrollingTableState;
use crossterm::event::{KeyCode, KeyModifiers};
use notify_rust::{Notification, Timeout};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use rodio::Source;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::iter;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, error, warn};
use ytmapi_rs::common::YoutubeID;

mod draw;
mod playback;

#[cfg(test)]
mod tests;

const SONGS_AHEAD_TO_BUFFER: usize = 1;
const SONGS_BEHIND_TO_SAVE: usize = 0;
const GAPLESS_PLAYBACK_THRESHOLD: Duration = Duration::from_secs(1);
pub const DEFAULT_UI_VOLUME: Percentage = Percentage(50);

/// Held-key (or repeated-key) shuffle toggles arrive faster than a download
/// can be cancelled. Every toggle used to call `regenerate_downloads_for_current`
/// immediately, spawning a fresh resolve + download before the previous one was
/// cancelled — a burst of key events spawned several yt-dlp processes at once.
/// Regeneration for shuffle toggles is now debounced to this trailing window.
const SHUFFLE_REGEN_DEBOUNCE_MS: u64 = 100;

fn is_cancellation_error(msg: &str) -> bool {
    msg.starts_with("download cancelled")
}

fn is_dead_video_error(msg: &str) -> bool {
    msg.starts_with("video unavailable")
}

/// True when a download failed because of an authentication/cookie problem
/// (stale login, winter bot check). Such failures must notify the user and
/// skip the song — they are a config/login issue, never a reason to remove it.
fn is_auth_error(msg: &str) -> bool {
    msg.starts_with("authentication error")
}

/// Cooldown between auth-error notifications so a queue of failed songs does
/// not spam one popup per song.
const AUTH_ERROR_NOTIF_COOLDOWN: Duration = Duration::from_secs(30);

/// After this many consecutive download failures of the currently-buffering
/// song, halt playback instead of walking the whole queue (a systemic failure
/// like a dead yt-dlp or exhausted resources would otherwise drain the list).
const HALT_AFTER_CONSECUTIVE_FAILURES: u8 = 5;

pub enum DownloadProgressUpdate {
    Downloading,
    Completed(Box<dyn Source<Item = f32> + Send + 'static>),
    Error(String),
}

fn build_visual_map(indices: &[usize], list_len: usize) -> Vec<Option<usize>> {
    let mut map = vec![None; list_len];
    for (vis, &actual) in indices.iter().enumerate() {
        if actual < list_len {
            map[actual] = Some(vis);
        }
    }
    map
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueueState {
    NotQueued,
    Queued(ListSongID),
}

#[derive(Debug, Clone)]
pub struct DownloadTask {
    cancel_token: Arc<tokio_util::sync::CancellationToken>,
}

pub struct Playlist {
    pub list: BrowserSongsList,
    id_to_index_cache: HashMap<ListSongID, usize>,
    cur_played_dur: Option<Duration>,
    pub play_status: PlayState,
    pub queue_status: QueueState,
    volume: Percentage,
    cur_selected: usize,
    pub widget_state: ScrollingTableState,
    shuffle_enabled: bool,
    shuffle_indices: Vec<usize>,
    shuffle_seed: u64,
    shuffle_visual_map: Vec<Option<usize>>,
    active_downloads: Arc<std::sync::Mutex<Vec<(ListSongID, DownloadTask)>>>,
    download_queue: VecDeque<ListSongID>,
    search_enabled: bool,
    search_text: String,
    search_indices: Vec<usize>,
    search_visual_map: Vec<Option<usize>>,
    pre_search_selected: usize,
    loaded_from_autosave: bool,
    preloaded_sources: HashMap<ListSongID, Box<dyn Source<Item = f32> + Send + 'static>>,
    play_next_queue: VecDeque<ListSongID>,
    resolving_audio: bool,
    resolve_remaining: usize,
    cached_title: RefCell<Option<Line<'static>>>,
    notifications_enabled: bool,
    auth_notif_last: Option<std::time::Instant>,
    consecutive_download_failures: u8,
    shuffle_regen_token: Option<tokio_util::sync::CancellationToken>,
}

impl Component for Playlist {}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistAction {
    ViewBrowser,
    PlaySelected,
    DeleteSelected,
    DeleteAll,
    ToggleShuffle,
    ToggleSearch,
    SaveQueue,
    LoadQueue,
    ClearSearch,
    ResolveAudioTracks,
    AddToPlayNext,
}

impl Action for PlaylistAction {
    fn context(&self) -> std::borrow::Cow<'_, str> {
        "Playlist".into()
    }

    fn describe(&self) -> std::borrow::Cow<'_, str> {
        match self {
            PlaylistAction::ViewBrowser => "View Browser",
            PlaylistAction::PlaySelected => "Play Selected",
            PlaylistAction::DeleteSelected => "Delete Selected",
            PlaylistAction::DeleteAll => "Delete All",
            PlaylistAction::ToggleShuffle => "Toggle Shuffle",
            PlaylistAction::ToggleSearch => "Toggle Search",
            PlaylistAction::ClearSearch => "Clear Search",
            PlaylistAction::SaveQueue => "Save Queue",
            PlaylistAction::LoadQueue => "Load Queue",
            PlaylistAction::ResolveAudioTracks => "Resolve Audio Tracks",
            PlaylistAction::AddToPlayNext => "Add To Play Next",
        }
        .into()
    }
}

impl ActionHandler<PlaylistAction> for Playlist {
    fn apply_action(&mut self, action: PlaylistAction) -> impl Into<YoutuiEffect<Playlist>> {
        match action {
            PlaylistAction::ViewBrowser => (Effects::none(), Some(self.view_browser())),
            PlaylistAction::PlaySelected => (self.play_selected(), None),
            PlaylistAction::DeleteSelected => (self.delete_selected(), None),
            PlaylistAction::DeleteAll => (self.delete_all(), None),
            PlaylistAction::ToggleShuffle => (self.toggle_shuffle(), None),
            PlaylistAction::ToggleSearch => (self.toggle_search(), None),
            PlaylistAction::ClearSearch => (self.clear_search(), None),
            PlaylistAction::SaveQueue => {
                if let Err(e) = queue_persistence::auto_save(self) {
                    warn!("Failed to auto-save queue: {e}");
                }
                (Effects::none(), None)
            }
            PlaylistAction::LoadQueue => match queue_persistence::auto_load(self) {
                Ok(effect) => (effect, None),
                Err(e) => {
                    error!("Failed to auto-load queue: {e}");
                    (Effects::none(), None)
                }
            },
            PlaylistAction::ResolveAudioTracks => {
                if self.resolving_audio {
                    return (Effects::none(), None);
                }
                let unchecked: Vec<ListSong> = self
                    .list
                    .get_list_iter_mut()
                    .filter_map(|s| {
                        if s.resolution_checked {
                            None
                        } else {
                            s.resolution_checked = true;
                            Some(s.clone())
                        }
                    })
                    .collect();
                if unchecked.is_empty() {
                    return (Effects::none(), None);
                }
                self.resolve_remaining = unchecked.len();
                self.resolving_audio = true;
                let mut effect = Effects::none();
                for _ in &unchecked {
                    effect = effect.push(Effects::new(
                        |_: &crate::app::server::ArcServer| async move {
                            Box::new(|_: &mut Playlist| Effects::none()) as Box<dyn FnOnce(&mut Playlist) -> Effects<Playlist> + Send>
                        }
                    ));
                }
                self.resolve_remaining = 0;
                self.resolving_audio = false;
                (effect, None)
            }
            PlaylistAction::AddToPlayNext => {
                if !self.search_text.is_empty() && self.search_indices.is_empty() {
                    return (Effects::none(), None);
                }
                if self.list.get_list_iter().len() == 0 {
                    return (Effects::none(), None);
                }
                let actual_index = self.visual_to_actual_index(self.cur_selected);
                let Some(id) = self.get_id_from_index(actual_index) else {
                    return (Effects::none(), None);
                };
                if self.get_cur_playing_id() == Some(id) {
                    return (Effects::none(), None);
                }
                if self.play_next_queue.contains(&id) {
                    return (Effects::none(), None);
                }
                let song_title = self
                    .get_song_from_id(id)
                    .map(|s| s.title.clone())
                    .unwrap_or_default();
                self.play_next_queue.push_back(id);
                self.cached_title.borrow_mut().take();
                if !song_title.is_empty()
                    && let Ok(rt) = tokio::runtime::Handle::try_current()
                {
                    drop(rt.spawn(async move {
                        if let Err(e) = Notification::new()
                            .summary("Play Next")
                            .body(&song_title)
                            .appname("youtui")
                            .timeout(Timeout::Milliseconds(3000))
                            .show()
                        {
                            debug!("play-next notification failed: {e}");
                        }
                    }));
                }
                (Effects::none(), None)
            }
        }
    }
}

impl KeyRouter<AppAction> for Playlist {
    fn get_all_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<AppAction>> + 'a {
        self.get_active_keybinds(config)
    }

    fn get_active_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<AppAction>> + 'a {
        std::iter::once(&config.keybinds.playlist)
    }
}

impl TextHandler for Playlist {
    fn is_text_handling(&self) -> bool {
        self.search_enabled
    }

    fn get_text(&self) -> std::option::Option<&str> {
        if self.search_enabled {
            Some(&self.search_text)
        } else {
            None
        }
    }

    fn replace_text(&mut self, text: impl Into<String>) {
        self.search_text = text.into();
        self.update_search_indices();
        self.cached_title.borrow_mut().take();
    }

    fn clear_text(&mut self) -> bool {
        if !self.search_text.is_empty() {
            self.search_text.clear();
            self.update_search_indices();
            true
        } else {
            false
        }
    }

    fn handle_text_event_impl(
        &mut self,
        event: &crossterm::event::Event,
    ) -> Option<Effects<Self>> {
        if !self.search_enabled {
            return None;
        }

        match event {
            crossterm::event::Event::Key(key_event) => match key_event.code {
                KeyCode::Char('w') if key_event.modifiers == KeyModifiers::CONTROL => {
                    if !self.search_text.is_empty() {
                        let trimmed = self.search_text.trim_end().len();
                        let last_word_start = self.search_text[..trimmed]
                            .rfind(char::is_whitespace)
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        self.search_text.truncate(last_word_start);
                        self.update_search_indices();
                        self.cached_title.borrow_mut().take();
                        self.cur_selected = self.cur_selected.min(self.get_max_visual_index());
                        Some(Effects::none())
                    } else {
                        None
                    }
                }
                KeyCode::Char(c) => {
                    self.search_text.push(c);
                    self.update_search_indices();
                    self.cached_title.borrow_mut().take();
                    self.cur_selected = self.cur_selected.min(self.get_max_visual_index());
                    Some(Effects::none())
                }
                KeyCode::Backspace => {
                    if !self.search_text.is_empty() {
                        self.search_text.pop();
                        self.update_search_indices();
                        self.cached_title.borrow_mut().take();
                        self.cur_selected = self.cur_selected.min(self.get_max_visual_index());
                        return Some(Effects::none());
                    }
                    None
                }
                KeyCode::Esc | KeyCode::Enter => {
                    self.search_enabled = false;
                    self.cached_title.borrow_mut().take();
                    Some(Effects::none())
                }
                _ => None,
            },
            _ => None,
        }
    }
}

impl DrawableMut for Playlist {
    fn draw_mut_chunk(&mut self, f: &mut Frame, chunk: Rect, selected: bool, cur_tick: u64) {
        draw_panel_mut(f, self, chunk, selected, |t, f, chunk| {
            draw_loadable(f, t, chunk, cur_tick, |t, f, chunk| {
                Some(draw_table(f, t, chunk, cur_tick))
            })
        });
    }
}

impl Loadable for Playlist {
    fn is_loading(&self) -> bool {
        false
    }
}

impl Scrollable for Playlist {
    fn increment_list(&mut self, amount: isize) {
        let max_index = self.get_max_visual_index();
        self.cur_selected = self
            .cur_selected
            .saturating_add_signed(amount)
            .min(max_index);
    }

    fn is_scrollable(&self) -> bool {
        true
    }
}

impl SongListComponent for Playlist {
    fn get_song_from_idx(&self, idx: usize) -> Option<&ListSong> {
        self.list.get_list_iter().nth(idx)
    }
}
