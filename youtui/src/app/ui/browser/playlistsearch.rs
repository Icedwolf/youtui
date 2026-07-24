use crate::app::component::actionhandler::{
    Scrollable, TextHandler, YoutuiEffect,
};
use crate::app::effect::Effects;
use crate::app::server::api::GetPlaylistSongsProgressUpdate;
use crate::app::server::ArcServer;
use futures::StreamExt;
use crate::app::structures::ListStatus;
use crate::app::view::{ListView, TableView};
use search_panel::NonPodcastSearchResultPlaylist;
use std::sync::Arc;
use tracing::{debug, error, warn};
use ytmapi_rs::common::PlaylistID;
use ytmapi_rs::parse::{PlaylistItem, SearchResultPlaylist};


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
    pub fn execute_search(&mut self, search_query: String) -> Effects<Self> {
        Effects::new(move |server: &ArcServer| {
            let query = search_query.clone();
            let server = Arc::clone(server);
            async move {
                match server.api.search_playlists(query).await {
                    Ok(playlists) => Box::new(move |this: &mut PlaylistSearchBrowser| {
                        this.replace_playlist_list(playlists);
                        Effects::none()
                    }) as Box<dyn FnOnce(&mut PlaylistSearchBrowser) -> Effects<PlaylistSearchBrowser> + Send>,
                    Err(error) => {
                        warn!("Playlist search error: {error}");
                        Box::new(|_: &mut PlaylistSearchBrowser| Effects::none())
                            as Box<dyn FnOnce(&mut PlaylistSearchBrowser) -> Effects<PlaylistSearchBrowser> + Send>
                    }
                }
            }
        }).kill_prev::<PlaylistSearchBrowser>()
    }
    pub fn handle_extra_song_action(
        &mut self,
        _action: songs_panel::BrowserPlaylistSongsAction,
    ) -> YoutuiEffect<Self> {
        Effects::none().into()
    }
    pub fn execute_get_songs(&mut self, selected: usize) -> Effects<Self> {
        let Some(cur_playlist) = self.search_panel.list.get(selected).cloned() else {
            debug!("Tried to get item from list with index out of range");
            return Effects::none();
        };

        Effects::new_stream(move |server: &ArcServer| {
            let pid = cur_playlist.playlist_id.clone();
            server.api.get_playlist_songs(pid).map(move |update| {
                Box::new(move |this: &mut PlaylistSearchBrowser| {
                    match update {
                        GetPlaylistSongsProgressUpdate::Loading => this.handle_song_list_loading(),
                        GetPlaylistSongsProgressUpdate::Songs(items) => {
                            this.handle_append_song_list(items);
                        }
                        GetPlaylistSongsProgressUpdate::GetPlaylistSongsError { playlist_id, error } => {
                            this.handle_search_playlist_error(playlist_id, error);
                        }
                        GetPlaylistSongsProgressUpdate::AllSongsSent => {
                            this.handle_song_list_loaded();
                        }
                    }
                    Effects::none()
                }) as Box<dyn FnOnce(&mut PlaylistSearchBrowser) -> Effects<PlaylistSearchBrowser> + Send>
            })
        }).block_concurrent::<PlaylistSearchBrowser>()
    }
    pub fn handle_search_playlist_error(
        &mut self,
        _playlist_id: PlaylistID<'static>,
        error: anyhow::Error,
    ) {
        self.songs_panel.list.state = ListStatus::Error;
        warn!("Playlist songs error: {error}");
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
        self.songs_panel.rebuild_filtered_indices();
        self.songs_panel.list.state = ListStatus::InProgress;
    }
}

