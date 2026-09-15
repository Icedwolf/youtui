use super::get_sort_keybinds;
use super::shared_components::{
    BrowserSearchAction, FilterAction, FilterManager, SearchBlock, SortAction, SortFilterTable,
    SortManager, add_song_to_playlist_impl, add_songs_to_playlist_impl,
    play_song_impl, play_songs_impl,
};
use crate::app::component::actionhandler::{
    Action, ActionHandler, Component, KeyRouter, Scrollable, TextHandler,
    YoutuiEffect,
};
use crate::app::effect::Effects;
use crate::app::server::ArcServer;
use std::sync::Arc;
use crate::app::structures::{
    BrowserSongsList, ListSongDisplayableField, ListStatus, Percentage, SongListComponent,
};
use crate::app::ui::action::{AppAction, TextEntryAction};
use crate::app::view::{AdvancedTableView, BasicConstraint, HasTitle, Loadable, TableView};
use crate::config::Config;
use crate::config::keymap::Keymap;
use crate::widgets::ScrollingTableState;
use itertools::Either;
use ratatui::text::Line;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use tracing::{debug, warn};
use ytmapi_rs::parse::SearchResultSong;

pub struct SongSearchBrowser {
    pub input_routing: InputRouting,
    song_list: BrowserSongsList,
    cur_selected: usize,
    pub search_popped: bool,
    pub search: SearchBlock,
    pub widget_state: ScrollingTableState,
    pub sort: SortManager,
    pub     filter: FilterManager,
    filtered_indices: Vec<usize>,
    cached_title: RefCell<Option<(ListStatus, usize, Line<'static>)>>,
}
impl Component for SongSearchBrowser {}

use crate::define_browser_songs_action;

define_browser_songs_action!(BrowserSongsAction, "Song Search Browser");

#[derive(Default)]
pub enum InputRouting {
    List,
    #[default]
    Search,
    Filter,
    Sort,
}

impl Scrollable for SongSearchBrowser {
    fn increment_list(&mut self, amount: isize) {
        match self.input_routing {
            InputRouting::List => {
                self.cur_selected = self
                    .cur_selected
                    .saturating_add_signed(amount)
                    .min(self.filtered_indices.len().saturating_sub(1))
            }
            InputRouting::Sort => {
                self.sort.cur = self
                    .sort
                    .cur
                    .saturating_add_signed(amount)
                    .min(self.get_sortable_columns().len().saturating_sub(1));
            }
            InputRouting::Search => debug!("Tried to increment list when in search box"),
            InputRouting::Filter => debug!("Tried to increment list when filter popup shown"),
        }
    }
    fn is_scrollable(&self) -> bool {
        matches!(self.input_routing, InputRouting::Sort | InputRouting::List)
    }
}
impl TextHandler for SongSearchBrowser {
    fn is_text_handling(&self) -> bool {
        matches!(
            self.input_routing,
            InputRouting::Filter | InputRouting::Search
        )
    }
    fn get_text(&self) -> std::option::Option<&str> {
        match self.input_routing {
            InputRouting::Filter => self.filter.get_text(),
            InputRouting::Search => self.search.get_text(),
            InputRouting::List | InputRouting::Sort => None,
        }
    }
    fn clear_text(&mut self) -> bool {
        match self.input_routing {
            InputRouting::Search => self.search.clear_text(),
            InputRouting::Filter => self.filter.clear_text(),
            InputRouting::List => false,
            InputRouting::Sort => false,
        }
    }
    fn handle_text_event_impl(
        &mut self,
        event: &crossterm::event::Event,
    ) -> Option<Effects<Self>> {
        match self.input_routing {
            InputRouting::Search => self
                .search
                .handle_text_event_impl(event)
                .map(|effect| effect.map(|this: &mut Self| &mut this.search)),
            InputRouting::Filter => self
                .filter
                .handle_text_event_impl(event)
                .map(|effect| effect.map(|this: &mut Self| &mut this.filter)),
            InputRouting::List => None,
            InputRouting::Sort => None,
        }
    }
}
impl ActionHandler<FilterAction> for SongSearchBrowser {
    fn apply_action(&mut self, action: FilterAction) -> impl Into<YoutuiEffect<Self>> {
        match action {
            FilterAction::Close => self.toggle_filter(),
            FilterAction::Apply => self.apply_filter(),
            FilterAction::ClearFilter => self.clear_filter(),
        };
        Effects::none()
    }
}
impl ActionHandler<SortAction> for SongSearchBrowser {
    fn apply_action(&mut self, action: SortAction) -> impl Into<YoutuiEffect<Self>> {
        match action {
            SortAction::SortSelectedAsc => self.handle_sort_cur_asc(),
            SortAction::SortSelectedDesc => self.handle_sort_cur_desc(),
            SortAction::Close => self.close_sort(),
            SortAction::ClearSort => self.handle_clear_sort(),
        }
        Effects::none()
    }
}
impl ActionHandler<BrowserSearchAction> for SongSearchBrowser {
    fn apply_action(&mut self, _action: BrowserSearchAction) -> impl Into<YoutuiEffect<Self>> {
        // Search suggestions were removed (never fetched) — see AGENTS.md scope.
        Effects::none()
    }
}
impl ActionHandler<BrowserSongsAction> for SongSearchBrowser {
    fn apply_action(&mut self, action: BrowserSongsAction) -> impl Into<YoutuiEffect<Self>> {
        match action {
            BrowserSongsAction::Filter => self.toggle_filter(),
            BrowserSongsAction::Sort => self.handle_pop_sort(),
            BrowserSongsAction::PlaySong => return self.play_song().into(),
            BrowserSongsAction::PlaySongs => return self.play_songs().into(),
            BrowserSongsAction::AddSongToPlaylist => return self.add_song_to_playlist().into(),
            BrowserSongsAction::AddSongsToPlaylist => return self.add_songs_to_playlist().into(),
        }
        YoutuiEffect::new_no_op()
    }
}
impl KeyRouter<AppAction> for SongSearchBrowser {
    fn get_all_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<AppAction>> + 'a {
        [
            &config.keybinds.browser_songs,
            &config.keybinds.browser_search,
        ]
        .into_iter()
    }
    fn get_active_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<AppAction>> + 'a {
        match self.input_routing {
            InputRouting::List => Either::Left(std::iter::once(&config.keybinds.browser_songs)),
            InputRouting::Search => Either::Left(std::iter::once(&config.keybinds.browser_search)),
            InputRouting::Filter => Either::Left(std::iter::once(&config.keybinds.filter)),
            InputRouting::Sort => Either::Right(get_sort_keybinds(config)),
        }
    }
}
impl SongListComponent for SongSearchBrowser {
    fn get_song_from_idx(&self, idx: usize) -> Option<&crate::app::structures::ListSong> {
        self.filtered_indices.get(idx).and_then(|&i| self.song_list.get_song_from_idx(i))
    }
}
impl Loadable for SongSearchBrowser {
    fn is_loading(&self) -> bool {
        matches!(
            self.song_list.state,
            crate::app::structures::ListStatus::Loading
        )
    }
}
impl TableView for SongSearchBrowser {
    fn get_selected_item(&self) -> usize {
        self.cur_selected
    }
    fn get_state(&self) -> &ScrollingTableState {
        &self.widget_state
    }
    fn get_layout(&self) -> &[crate::app::view::BasicConstraint] {
        &[
            BasicConstraint::Percentage(Percentage(40)),
            BasicConstraint::Percentage(Percentage(30)),
            BasicConstraint::Percentage(Percentage(30)),
            BasicConstraint::Length(8),
            BasicConstraint::Length(10),
        ]
    }
    fn get_highlighted_row(&self) -> Option<usize> {
        None
    }
    fn get_items(&self) -> impl ExactSizeIterator<Item = impl Iterator<Item = Cow<'_, str>> + '_> {
        self.song_list
            .get_list_iter()
            .map(|ls| ls.get_fields(Self::subcolumns_of_vec()).into_iter())
    }
    fn get_headings(&self) -> impl Iterator<Item = &'static str> {
        ["Song", "Artist", "Album", "Duration", "Plays"].into_iter()
    }
    fn get_mut_state(&mut self) -> &mut ScrollingTableState {
        &mut self.widget_state
    }
}
impl AdvancedTableView for SongSearchBrowser {
    fn get_sortable_columns(&self) -> &[usize] {
        &[0, 1, 2]
    }
    fn get_filterable_columns(&self) -> &[usize] {
        &[0, 1, 2]
    }
}
impl SortFilterTable for SongSearchBrowser {
    fn get_songs(&self) -> &BrowserSongsList {
        &self.song_list
    }
    fn get_mut_songs(&mut self) -> &mut BrowserSongsList {
        &mut self.song_list
    }
    fn set_route_list(&mut self) {
        self.input_routing = InputRouting::List;
    }
    fn set_route_sort(&mut self) {
        self.input_routing = InputRouting::Sort;
    }
    fn set_route_filter(&mut self) {
        self.input_routing = InputRouting::Filter;
    }
    fn route_is_list(&self) -> bool {
        matches!(self.input_routing, InputRouting::List)
    }
    fn route_is_sort(&self) -> bool {
        matches!(self.input_routing, InputRouting::Sort)
    }
    fn set_cur_selected(&mut self, idx: usize) {
        self.cur_selected = idx;
    }
    fn set_filtered_indices(&mut self, indices: Vec<usize>) {
        self.filtered_indices = indices;
    }
    fn get_filtered_indices(&self) -> &[usize] {
        &self.filtered_indices
    }
    fn get_filter_manager(&self) -> &FilterManager {
        &self.filter
    }
    fn get_mut_filter_manager(&mut self) -> &mut FilterManager {
        &mut self.filter
    }
    fn get_sort_manager(&self) -> &SortManager {
        &self.sort
    }
    fn get_mut_sort_manager(&mut self) -> &mut SortManager {
        &mut self.sort
    }
    fn get_subcolumns() -> [ListSongDisplayableField; 5] {
        Self::subcolumns_of_vec()
    }
}
impl HasTitle for SongSearchBrowser {
    fn get_title(&self) -> Line<'static> {
        let len = self.song_list.get_list_iter().len();
        {
            let cached = self.cached_title.borrow();
            if let Some((cached_state, cached_len, title)) = cached.as_ref()
                && cached_state == &self.song_list.state
                && *cached_len == len
            {
                return title.clone();
            }
        }
        let title = match &self.song_list.state {
            ListStatus::New => Line::from("Songs"),
            ListStatus::Loading => Line::from("Songs - loading"),
            ListStatus::InProgress => Line::from(format!(
                "Songs - {} results - loading",
                len
            )),
            ListStatus::Loaded => {
                if len == 0 {
                    Line::from("Songs - no songs found")
                } else {
                    Line::from(format!("Songs - {len} results"))
                }
            }
            ListStatus::Error => Line::from("Songs - Error received"),
        };
        *self.cached_title.borrow_mut() = Some((self.song_list.state.clone(), len, title.clone()));
        title
    }
}
impl SongSearchBrowser {
    pub fn new() -> Self {
        Self {
            input_routing: Default::default(),
            song_list: Default::default(),
            search_popped: true,
            search: Default::default(),
            widget_state: Default::default(),
            sort: Default::default(),
            filter: Default::default(),
            cur_selected: Default::default(),
            filtered_indices: Vec::new(),
            cached_title: RefCell::new(None),
        }
    }
    pub fn subcolumns_of_vec() -> [ListSongDisplayableField; 5] {
        [
            ListSongDisplayableField::Song,
            ListSongDisplayableField::Artists,
            ListSongDisplayableField::Album,
            ListSongDisplayableField::Duration,
            ListSongDisplayableField::Plays,
        ]
    }
    pub fn handle_text_entry_action(&mut self, action: TextEntryAction) -> Effects<Self> {
        if self.is_text_handling()
            && self.search_popped
            && matches!(self.input_routing, InputRouting::Search)
        {
            match action {
                TextEntryAction::Submit => {
                    return self.search();
                }
                TextEntryAction::DeleteWord => {
                    self.search.delete_word();
                    return Effects::none();
                }
                _ => return Effects::none(),
            }
        }
        Effects::none()
    }
    pub fn handle_toggle_search(&mut self) {
        if self.search_popped {
            self.search_popped = false;
            self.input_routing = InputRouting::List;
        } else {
            self.search_popped = true;
            self.input_routing = InputRouting::Search;
        }
    }
    pub fn search(&mut self) -> Effects<Self> {
        self.search_popped = false;
        self.input_routing = InputRouting::List;
        let Some(search_query) = self.search.get_text().map(|s| s.to_string()) else {
            return Effects::none();
        };
        self.search.clear_text();
        self.song_list.state = ListStatus::Loading;

        Effects::new(move |server: &ArcServer| {
            let query = search_query.clone();
            let server = Arc::clone(server);
            async move {
                match server.api.search_songs(query).await {
                    Ok(songs) => Box::new(move |this: &mut SongSearchBrowser| {
                        this.replace_song_list(songs);
                        Effects::none()
                    }) as Box<dyn FnOnce(&mut SongSearchBrowser) -> Effects<SongSearchBrowser> + Send>,
                    Err(error) => {
                        warn!("Song search error: {error}");
                        Box::new(move |this: &mut SongSearchBrowser| {
                            this.song_list.state = ListStatus::Error;
                            Effects::none()
                        })
                            as Box<dyn FnOnce(&mut SongSearchBrowser) -> Effects<SongSearchBrowser> + Send>
                    }
                }
            }
        }).kill_prev::<SongSearchBrowser>()
    }
    pub fn play_song(&mut self) -> impl Into<YoutuiEffect<Self>> + use<> {
        play_song_impl::<Self>(self.get_selected_item(), |idx| {
            self.get_song_from_idx(idx).cloned()
        })
    }
    pub fn play_songs(&mut self) -> impl Into<YoutuiEffect<Self>> + use<> {
        let cur_idx = self.get_selected_item();
        let song_list = self
            .get_filtered_list_iter()
            .skip(cur_idx)
            .cloned()
            .collect();
        play_songs_impl::<Self>(song_list)
    }
    pub fn add_song_to_playlist(&mut self) -> impl Into<YoutuiEffect<Self>> + use<> {
        add_song_to_playlist_impl::<Self>(self.get_selected_item(), |idx| {
            self.get_song_from_idx(idx).cloned()
        })
    }
    pub fn add_songs_to_playlist(&mut self) -> impl Into<YoutuiEffect<Self>> + use<> {
        let cur_idx = self.get_selected_item();
        let song_list = self
            .get_filtered_list_iter()
            .skip(cur_idx)
            .cloned()
            .collect();
        add_songs_to_playlist_impl::<Self>(song_list)
    }
    pub fn replace_song_list(&mut self, song_list: Vec<SearchResultSong>) {
        self.song_list.clear();
        self.song_list.append_raw_search_result_songs(song_list);
        self.song_list.state = ListStatus::Loaded;
        self.rebuild_filtered_indices();
        if let Err(e) = self.apply_all_sort_commands() {
            debug!("Tried to sort a column that is not sortable - error {e}")
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::structures::ListSong;

    #[test]
    fn replace_song_list_marks_results_loaded() {
        let mut browser = SongSearchBrowser::new();
        browser.replace_song_list(Vec::new());
        assert_eq!(browser.song_list.state, ListStatus::Loaded);
    }

    #[test]
    fn search_triggers_loading_state() {
        let mut browser = SongSearchBrowser::new();
        browser.search.search_contents.set_text("some query");
        let _effects = browser.search();
        assert_eq!(browser.song_list.state, ListStatus::Loading);
    }

    #[test]
    fn song_search_sort_route_navigation_targets_sort_cursor() {
        let mut browser = SongSearchBrowser::new();
        browser.input_routing = InputRouting::Sort;
        // SongSearchBrowser sortable columns are &[0, 1, 2], so the last index is 2.
        browser.sort.cur = 2;
        browser.go_to_first();
        assert_eq!(browser.sort.cur, 0);
        browser.go_to_last();
        assert_eq!(browser.sort.cur, 2);
    }

    fn song(id: &str, album: &str) -> ListSong {
        use ytmapi_rs::common::{VideoID, YoutubeID};
        ListSong::create_with_metadata(
            VideoID::from_raw(id.to_owned()),
            "Title".into(),
            vec!["Artist".into()],
            Some(album.to_owned()),
            "3:00".into(),
        )
    }

    #[test]
    fn filtered_selection_targets_filtered_song() {
        let mut browser = SongSearchBrowser::new();
        browser.song_list.push_song_list(vec![
            song("a", "Alpha"),
            song("b", "Beta"),
            song("c", "Alpha"),
            song("d", "Gamma"),
        ]);
        browser.cur_selected = 1;
        browser.filter.filter_text.set_text("Alpha");
        browser.apply_filter();
        assert_eq!(browser.filtered_indices, vec![0, 2]);
        // Visible row 1 (the second Alpha song) must NOT resolve as song_list[1].
        assert_eq!(browser.get_song_from_idx(1).unwrap().title, "Title");
    }

    #[test]
    fn title_cache_tracks_state_and_len() {
        use ytmapi_rs::common::VideoID;
        use ytmapi_rs::common::YoutubeID;
        let mut browser = SongSearchBrowser::new();
        assert_eq!(browser.get_title().to_string(), "Songs");
        // Same call twice returns the cached clone.
        assert_eq!(browser.get_title().to_string(), "Songs");
        // State change invalidates the cached title.
        browser.song_list.state = ListStatus::Loaded;
        assert_eq!(browser.get_title().to_string(), "Songs - no songs found");
        // Push a song; length change invalidates the cached title.
        let song = ListSong::create_with_metadata(
            VideoID::from_raw("testvid".to_owned()),
            "Title".into(),
            vec!["Artist".into()],
            None,
            "3:00".into(),
        );
        browser.song_list.state = ListStatus::Loaded;
        browser.song_list.push_song_list(vec![song]);
        assert_eq!(browser.get_title().to_string(), "Songs - 1 results");
    }
}



