use crate::app::AppCallback;
use crate::app::component::actionhandler::{
    Scrollable, TextHandler, YoutuiEffect,
};
use crate::app::effect::Effects;
use crate::app::server::api::{AlbumSongsData, GetArtistSongsProgressUpdate};
use futures::StreamExt;
use crate::app::server::ArcServer;
use crate::app::structures::{ListSongAlbum, ListStatus, MaybeRc};
use crate::app::view::{ListView, TableView};
use std::sync::Arc;
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
    pub fn execute_search(&mut self, search_query: String) -> Effects<Self> {
        self.search_panel.status = ListStatus::Loading;
        Effects::new(move |server: &ArcServer| {
            let query = search_query.clone();
            let server = Arc::clone(server);
            async move {
                match server.api.search_artists(query).await {
                    Ok(artists) => Box::new(move |this: &mut ArtistSearchBrowser| {
                        this.replace_artist_list(artists);
                        Effects::none()
                    }) as Box<dyn FnOnce(&mut ArtistSearchBrowser) -> Effects<ArtistSearchBrowser> + Send>,
                    Err(error) => {
                        warn!("Artist search error: {error}");
                        Box::new(move |this: &mut ArtistSearchBrowser| {
                            this.search_panel.status = ListStatus::Error;
                            Effects::none()
                        })
                            as Box<dyn FnOnce(&mut ArtistSearchBrowser) -> Effects<ArtistSearchBrowser> + Send>
                    }
                }
            }
        }).kill_prev::<ArtistSearchBrowser>()
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
    pub fn execute_get_songs(&mut self, selected: usize) -> Effects<Self> {
        let Some(cur_artist) = self.search_panel.list.get(selected).cloned() else {
            debug!("Tried to get item from list with index out of range");
            return Effects::none();
        };
        let cur_artist_id: ArtistChannelID<'static> = cur_artist.browse_id;

        Effects::new_stream(move |server: &ArcServer| {
            let browse_id = cur_artist_id.clone();
            server.api.get_artist_songs(browse_id).map(move |update| {
                let browse_id = cur_artist_id.clone();
                Box::new(move |this: &mut ArtistSearchBrowser| {
                    match update {
                        GetArtistSongsProgressUpdate::Loading => this.handle_song_list_loading(),
                        GetArtistSongsProgressUpdate::NoSongsFound => this.handle_no_songs_found(),
                        GetArtistSongsProgressUpdate::GetArtistAlbumsError(e) => {
                            this.handle_search_artist_error(browse_id, e);
                        }
                        GetArtistSongsProgressUpdate::GetAlbumsSongsError { album_id, error } => {
                            this.handle_get_album_songs_error(browse_id, album_id, error);
                        }
                        GetArtistSongsProgressUpdate::AlbumProgress { current, total } => {
                            debug!("artist_songs: album progress {current}/{total}");
                        }
                        GetArtistSongsProgressUpdate::AllAlbumsSongs(albums) => {
                            this.handle_all_albums_songs(albums);
                        }
                        GetArtistSongsProgressUpdate::AllSongsSent => {
                            this.handle_song_list_loaded();
                        }
                    }
                    Effects::none()
                }) as Box<dyn FnOnce(&mut ArtistSearchBrowser) -> Effects<ArtistSearchBrowser> + Send>
            })
        }).block_concurrent::<ArtistSearchBrowser>()
    }
    pub fn add_album_to_playlist(&mut self) -> impl Into<YoutuiEffect<Self>> {
        let cur_idx = self.songs_panel.get_selected_item();
        let Some(cur_song) = self.songs_panel.get_song_from_idx(cur_idx) else {
            return (Effects::none(), None);
        };
        let Some(ref cur_album) = cur_song.album else {
            error!("Expected album details to be in list but they are missing!");
            return (Effects::none(), None);
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
            Effects::none(),
            Some(AppCallback::AddSongsToPlaylist(song_list)),
        )
    }
    pub fn play_album(&mut self) -> impl Into<YoutuiEffect<Self>> {
        let cur_idx = self.songs_panel.get_selected_item();
        let Some(cur_song) = self.songs_panel.get_song_from_idx(cur_idx) else {
            return (Effects::none(), None);
        };
        let Some(ref cur_album) = cur_song.album else {
            error!("Expected album details to be in list but they are missing!");
            return (Effects::none(), None);
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
            Effects::none(),
            Some(AppCallback::AddSongsToPlaylistAndPlay(song_list)),
        )
    }
    pub fn handle_search_artist_error(
        &mut self,
        _artist_id: ArtistChannelID<'static>,
        error: anyhow::Error,
    ) {
        self.songs_panel.list.state = ListStatus::Error;
        warn!("Artist search error for {_artist_id:?}: {error}");
    }
    pub fn handle_get_album_songs_error(
        &mut self,
        _artist_id: ArtistChannelID<'static>,
        _album_id: AlbumID<'static>,
        error: anyhow::Error,
    ) {
        warn!("Album songs error for {_album_id:?}: {error}");
    }
    pub fn replace_artist_list(&mut self, artist_list: Vec<SearchResultArtist>) {
        self.search_panel.list = artist_list;
        self.search_panel.status = ListStatus::Loaded;
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
            );
        }
        if let Err(e) = self.songs_panel.apply_all_sort_commands() {
            error!("Error <{e}> sorting album songs panel");
        }
        self.songs_panel.rebuild_filtered_indices();
        self.songs_panel.list.state = ListStatus::InProgress;
    }
}



