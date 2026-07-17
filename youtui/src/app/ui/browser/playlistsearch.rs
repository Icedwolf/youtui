use crate::app::component::actionhandler::{
    ComponentEffect, Scrollable, TextHandler, YoutuiEffect,
};
use crate::app::server::api::GetPlaylistSongsProgressUpdate;
use crate::app::server::{GetPlaylistSongs, HandleApiError, SearchPlaylists};
use crate::app::structures::ListStatus;
use crate::app::view::{ListView, TableView};
use async_callback_manager::{AsyncTask, Constraint, NoOpHandler};
use search_panel::NonPodcastSearchResultPlaylist;
use tracing::{debug, error};
use ytmapi_rs::common::PlaylistID;
use ytmapi_rs::parse::{PlaylistItem, SearchResultPlaylist};

const MAX_PLAYLIST_SONGS: usize = 1000;

pub mod search_panel;
pub mod songs_panel;

use crate::define_search_results_browser;

define_search_results_browser!(
    PlaylistSearchBrowser,
    search_panel: search_panel::PlaylistSearchPanel,
    songs_panel: songs_panel::PlaylistSongsPanel,
    songs_action_ty: songs_panel::BrowserPlaylistSongsAction,
    search_action_ty: search_panel::BrowserPlaylistsAction,
    search_action_variant: DisplaySelectedPlaylist,
);
impl PlaylistSearchBrowser {
    pub fn execute_search(&mut self, search_query: String) -> ComponentEffect<Self> {
        AsyncTask::new_future_try(
            SearchPlaylists(search_query),
            HandleSearchPlaylistsOk,
            HandleSearchPlaylistsErr,
            Some(Constraint::new_kill_same_type()),
        )
    }
    pub fn handle_extra_song_action(
        &mut self,
        _action: songs_panel::BrowserPlaylistSongsAction,
    ) -> YoutuiEffect<Self> {
        YoutuiEffect::new_no_op()
    }
    pub fn execute_get_songs(&mut self, selected: usize) -> ComponentEffect<Self> {
        let Some(cur_playlist) = self.search_panel.list.get(selected).cloned() else {
            debug!("Tried to get item from list with index out of range");
            return AsyncTask::new_no_op();
        };

        AsyncTask::new_stream(
            GetPlaylistSongs {
                playlist_id: cur_playlist.playlist_id,
                max_songs: MAX_PLAYLIST_SONGS,
            },
            HandleGetPlaylistSongs,
            Some(Constraint::new_kill_same_type()),
        )
    }
    pub fn handle_search_playlist_error(
        &mut self,
        playlist_id: PlaylistID<'static>,
        error: anyhow::Error,
    ) -> ComponentEffect<Self> {
        self.songs_panel.list.state = ListStatus::Error;
        AsyncTask::new_future(
            HandleApiError {
                error,
                message: format!("Error searching for playlist {playlist_id:?} tracks"),
            },
            NoOpHandler,
            None,
        )
    }
    pub fn replace_playlist_list(&mut self, playlist_list: Vec<SearchResultPlaylist>) {
        self.search_panel.list = playlist_list
            .into_iter()
            .filter_map(NonPodcastSearchResultPlaylist::new)
            .collect();
        self.increment_cur_list(0);
    }
    pub fn handle_append_song_list(&mut self, song_list: Vec<PlaylistItem>) {
        self.songs_panel.list.append_raw_playlist_items(song_list);
        if let Err(e) = self.songs_panel.apply_all_sort_commands() {
            error!("Error <{e}> sorting playlist songs panel");
        }
        self.songs_panel.list.state = ListStatus::InProgress;
    }
}

#[derive(Debug, PartialEq)]
struct HandleSearchPlaylistsOk;
#[derive(Debug, PartialEq)]
struct HandleSearchPlaylistsErr;
#[derive(Debug, PartialEq, Clone)]
struct HandleGetPlaylistSongs;

impl_youtui_task_handler!(
    HandleSearchPlaylistsOk,
    Vec<SearchResultPlaylist>,
    PlaylistSearchBrowser,
    |_, playlists| |this: &mut PlaylistSearchBrowser| this.replace_playlist_list(playlists)
);
impl_youtui_task_handler!(
    HandleSearchPlaylistsErr,
    anyhow::Error,
    PlaylistSearchBrowser,
    |_, error| |_: &mut PlaylistSearchBrowser| AsyncTask::new_future(
        HandleApiError {
            error,
            message: "Error received getting playlists".to_string(),
        },
        NoOpHandler,
        None,
    )
);
impl_youtui_task_handler!(
    HandleGetPlaylistSongs,
    GetPlaylistSongsProgressUpdate,
    PlaylistSearchBrowser,
    |_, item| |this: &mut PlaylistSearchBrowser| {
        match item {
            GetPlaylistSongsProgressUpdate::Loading => this.handle_song_list_loading(),
            GetPlaylistSongsProgressUpdate::Songs(playlist_items) => {
                this.handle_append_song_list(playlist_items)
            }
            GetPlaylistSongsProgressUpdate::GetPlaylistSongsError { playlist_id, error } => {
                return this.handle_search_playlist_error(playlist_id, error);
            }
            GetPlaylistSongsProgressUpdate::AllSongsSent => this.handle_song_list_loaded(),
        }
        AsyncTask::new_no_op()
    }
);
#[cfg(test)]
mod tests {
    use crate::app::server::SearchPlaylists;
    use crate::app::ui::browser::playlistsearch::{
        HandleSearchPlaylistsErr, HandleSearchPlaylistsOk, PlaylistSearchBrowser,
    };
    use async_callback_manager::{AsyncTask, Constraint};

    fn get_dummy_playlist_search_browser() -> PlaylistSearchBrowser {
        PlaylistSearchBrowser::new(
            crate::app::ui::browser::playlistsearch::search_panel::PlaylistSearchPanel::new(),
            crate::app::ui::browser::playlistsearch::songs_panel::PlaylistSongsPanel::new(),
        )
    }

    #[test]
    fn test_on_submit_action_search_box_cleared() {
        let mut browser = get_dummy_playlist_search_browser();
        browser
            .search_panel
            .search
            .search_contents
            .set_text("Search!");
        let browser_text = browser.search_panel.search.search_contents.text();
        assert!(!browser_text.is_empty());
        let _ = browser.handle_text_entry_action(crate::app::ui::action::TextEntryAction::Submit);
        let browser_text = browser.search_panel.search.search_contents.text();
        assert!(browser_text.is_empty());
    }
    #[test]
    fn test_search_returns_effect() {
        let mut browser = get_dummy_playlist_search_browser();
        browser
            .search_panel
            .search
            .search_contents
            .set_text("Search!");
        let effect = browser.search();
        let expected_effect = AsyncTask::new_future_try(
            SearchPlaylists("Search!".to_string()),
            HandleSearchPlaylistsOk,
            HandleSearchPlaylistsErr,
            Some(Constraint::new_kill_same_type()),
        );
        assert_eq!(effect, expected_effect);
    }
}
