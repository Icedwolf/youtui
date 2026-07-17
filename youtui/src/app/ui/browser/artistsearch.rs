use crate::app::AppCallback;
use crate::app::component::actionhandler::{
    ComponentEffect, Scrollable, TextHandler, YoutuiEffect,
};
use crate::app::server::api::{AlbumSongsData, GetArtistSongsProgressUpdate};
use crate::app::server::{GetArtistSongs, HandleApiError, SearchArtists};
use crate::app::structures::{ListSongAlbum, ListStatus, MaybeRc};
use crate::app::view::{ListView, TableView};
use async_callback_manager::{AsyncTask, Constraint, NoOpHandler};
use tracing::{debug, error, warn};
use ytmapi_rs::common::{AlbumID, ArtistChannelID};
use ytmapi_rs::parse::SearchResultArtist;

pub mod search_panel;
pub mod songs_panel;

use crate::define_search_results_browser;

define_search_results_browser!(
    ArtistSearchBrowser,
    search_panel: search_panel::ArtistSearchPanel,
    songs_panel: songs_panel::AlbumSongsPanel,
    songs_action_ty: songs_panel::BrowserArtistSongsAction,
    search_action_ty: search_panel::BrowserArtistsAction,
    search_action_variant: DisplaySelectedArtistAlbums,
);
impl ArtistSearchBrowser {
    pub fn execute_search(&mut self, search_query: String) -> ComponentEffect<Self> {
        AsyncTask::new_future_try(
            SearchArtists(search_query),
            HandleSearchArtistsOk,
            HandleSearchArtistsError,
            Some(Constraint::new_kill_same_type()),
        )
    }
    pub fn handle_extra_song_action(
        &mut self,
        action: songs_panel::BrowserArtistSongsAction,
    ) -> YoutuiEffect<Self> {
        match action {
            songs_panel::BrowserArtistSongsAction::PlayAlbum => {
                return self.play_album().into();
            }
            songs_panel::BrowserArtistSongsAction::AddAlbumToPlaylist => {
                return self.add_album_to_playlist().into();
            }
            _ => {}
        }
        YoutuiEffect::new_no_op()
    }
    pub fn execute_get_songs(&mut self, selected: usize) -> ComponentEffect<Self> {
        let Some(cur_artist) = self.search_panel.list.get(selected).cloned() else {
            debug!("Tried to get item from list with index out of range");
            return AsyncTask::new_no_op();
        };
        let cur_artist_id: ArtistChannelID<'static> = cur_artist.browse_id;

        AsyncTask::new_stream(
            GetArtistSongs(cur_artist_id.clone()),
            HandleGetArtistSongsProgressUpdate(cur_artist_id),
            Some(Constraint::new_kill_same_type()),
        )
    }
    pub fn add_album_to_playlist(&mut self) -> impl Into<YoutuiEffect<Self>> {
        let cur_idx = self.songs_panel.get_selected_item();
        let Some(cur_song) = self.songs_panel.get_song_from_idx(cur_idx) else {
            return (AsyncTask::new_no_op(), None);
        };
        let Some(ref cur_album) = cur_song.album else {
            error!("Expected album details to be in list but they are missing!");
            return (AsyncTask::new_no_op(), None);
        };
        let song_list = self
            .songs_panel
            .list
            .get_list_iter()
            .filter(|song| {
                song.album
                    .as_ref()
                    .is_some_and(|album: &MaybeRc<ListSongAlbum>| album.as_ref().id == cur_album.id)
            })
            .cloned()
            .collect();
        (
            AsyncTask::new_no_op(),
            Some(AppCallback::AddSongsToPlaylist(song_list)),
        )
    }
    pub fn play_album(&mut self) -> impl Into<YoutuiEffect<Self>> {
        let cur_idx = self.songs_panel.get_selected_item();
        let Some(cur_song) = self.songs_panel.get_song_from_idx(cur_idx) else {
            return (AsyncTask::new_no_op(), None);
        };
        let Some(ref cur_album) = cur_song.album else {
            error!("Expected album details to be in list but they are missing!");
            return (AsyncTask::new_no_op(), None);
        };
        let song_list = self
            .songs_panel
            .list
            .get_list_iter()
            .filter(|song| {
                song.album
                    .as_ref()
                    .is_some_and(|album: &MaybeRc<ListSongAlbum>| album.as_ref().id == cur_album.id)
            })
            .cloned()
            .collect();
        (
            AsyncTask::new_no_op(),
            Some(AppCallback::AddSongsToPlaylistAndPlay(song_list)),
        )
    }
    pub fn handle_search_artist_error(
        &mut self,
        artist_id: ArtistChannelID<'static>,
        error: anyhow::Error,
    ) -> ComponentEffect<Self> {
        self.songs_panel.list.state = ListStatus::Error;
        AsyncTask::new_future(
            HandleApiError {
                error,
                message: format!("Error searching for artist {artist_id:?} albums"),
            },
            NoOpHandler,
            None,
        )
    }
    pub fn handle_get_album_songs_error(
        &mut self,
        artist_id: ArtistChannelID<'static>,
        album_id: AlbumID<'static>,
        error: anyhow::Error,
    ) -> ComponentEffect<Self> {
        warn!(
            "Received a get_album_songs_error. This will be logged but is not visible in the main ui!"
        );
        AsyncTask::new_future(
            HandleApiError {
                error,
                message: format!(
                    "Error getting songs for album {album_id:?}, artist {artist_id:?}"
                ),
            },
            NoOpHandler,
            None,
        )
    }
    pub fn replace_artist_list(&mut self, artist_list: Vec<SearchResultArtist>) {
        self.search_panel.list = artist_list;
        self.increment_cur_list(0);
    }
    pub fn handle_no_songs_found(&mut self) {
        self.songs_panel.list.state = ListStatus::Loaded;
    }
    pub fn handle_all_albums_songs(&mut self, albums: Vec<AlbumSongsData>) {
        self.songs_panel.list.clear();
        self.songs_panel.go_to_first();
        for data in albums {
            self.songs_panel.list.append_raw_album_songs(
                data.song_list,
                data.album,
                data.year,
                data.artists,
                data.thumbnails,
            );
        }
        if let Err(e) = self.songs_panel.apply_all_sort_commands() {
            error!("Error <{e}> sorting album songs panel");
        }
        self.songs_panel.list.state = ListStatus::InProgress;
    }
}

#[derive(PartialEq, Debug)]
pub struct HandleSearchArtistsOk;
#[derive(PartialEq, Debug)]
pub struct HandleSearchArtistsError;
#[derive(PartialEq, Debug, Clone)]
pub struct HandleGetArtistSongsProgressUpdate(pub ArtistChannelID<'static>);

impl_youtui_task_handler!(
    HandleSearchArtistsOk,
    Vec<SearchResultArtist>,
    ArtistSearchBrowser,
    |_, input| { |this: &mut ArtistSearchBrowser| this.replace_artist_list(input) }
);
impl_youtui_task_handler!(
    HandleSearchArtistsError,
    anyhow::Error,
    ArtistSearchBrowser,
    |_, error| {
        |_: &mut ArtistSearchBrowser| {
            AsyncTask::new_future(
                HandleApiError {
                    error,
                    message: "Error received getting artists".to_string(),
                },
                NoOpHandler,
                None,
            )
        }
    }
);
impl_youtui_task_handler!(
    HandleGetArtistSongsProgressUpdate,
    GetArtistSongsProgressUpdate,
    ArtistSearchBrowser,
    |HandleGetArtistSongsProgressUpdate(cur_artist_id), input| {
        |this: &mut ArtistSearchBrowser| {
            match input {
                GetArtistSongsProgressUpdate::Loading => this.handle_song_list_loading(),
                GetArtistSongsProgressUpdate::NoSongsFound => this.handle_no_songs_found(),
                GetArtistSongsProgressUpdate::GetArtistAlbumsError(e) => {
                    return this.handle_search_artist_error(cur_artist_id, e);
                }
                GetArtistSongsProgressUpdate::GetAlbumsSongsError { album_id, error } => {
                    return this.handle_get_album_songs_error(cur_artist_id, album_id, error);
                }
                GetArtistSongsProgressUpdate::AlbumProgress { current, total } => {
                    debug!("artist_songs: album progress {current}/{total}");
                }
                GetArtistSongsProgressUpdate::AllAlbumsSongs(albums) => {
                    this.handle_all_albums_songs(albums);
                }
                GetArtistSongsProgressUpdate::AllSongsSent => this.handle_song_list_loaded(),
            }
            AsyncTask::new_no_op()
        }
    }
);
#[cfg(test)]
mod tests {
    use crate::app::server::SearchArtists;
    use crate::app::ui::browser::artistsearch::{
        ArtistSearchBrowser, HandleSearchArtistsError, HandleSearchArtistsOk,
    };
    use async_callback_manager::{AsyncTask, Constraint};

    fn get_dummy_artist_search_browser() -> ArtistSearchBrowser {
        ArtistSearchBrowser::new(
            crate::app::ui::browser::artistsearch::search_panel::ArtistSearchPanel::new(),
            crate::app::ui::browser::artistsearch::songs_panel::AlbumSongsPanel::new(),
        )
    }

    #[test]
    fn test_on_submit_action_search_box_cleared() {
        let mut browser = get_dummy_artist_search_browser();
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
        let mut browser = get_dummy_artist_search_browser();
        browser
            .search_panel
            .search
            .search_contents
            .set_text("Search!");
        let effect = browser.search();
        let expected_effect = AsyncTask::new_future_try(
            SearchArtists("Search!".to_string()),
            HandleSearchArtistsOk,
            HandleSearchArtistsError,
            Some(Constraint::new_kill_same_type()),
        );
        assert_eq!(effect, expected_effect);
    }
}
