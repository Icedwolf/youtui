use crate::app::component::actionhandler::{
    Action, KeyRouter, Scrollable, TextHandler,
};
use crate::app::effect::Effects;
use crate::app::structures::{
    BrowserSongsList, ListSong, ListSongDisplayableField, ListStatus, Percentage, SongListComponent,
};
use crate::app::ui::action::AppAction;
use crate::app::ui::browser::get_sort_keybinds;
use crate::app::ui::browser::shared_components::{
    FilterManager, SortFilterTable, SortManager,
};
use crate::app::view::{AdvancedTableView, BasicConstraint, HasTitle, Loadable, TableView};
use crate::config::Config;
use crate::config::keymap::Keymap;
use crate::widgets::ScrollingTableState;
use itertools::Either;
use ratatui::text::Line;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use std::iter::{ExactSizeIterator, Iterator};
use std::marker::PhantomData;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum SongsInputRouting {
    #[default]
    List,
    Sort,
    Filter,
}

pub trait SongsPanelConfig: Sized + 'static {
    fn subcolumns() -> [ListSongDisplayableField; 5];
    fn layout() -> &'static [BasicConstraint];
    fn headings() -> [&'static str; 5];
    fn sortable_columns() -> &'static [usize];
    fn filterable_columns() -> &'static [usize];
    fn keybinds_key(config: &Config) -> &Keymap<AppAction>;
    fn context_name() -> &'static str;
}

#[derive(Clone)]
pub struct SongsPanel<C: SongsPanelConfig> {
    pub list: BrowserSongsList,
    pub route: SongsInputRouting,
    pub sort: SortManager,
    pub filter: FilterManager,
    cur_selected: usize,
    pub widget_state: ScrollingTableState,
    _cfg: PhantomData<C>,
    cached_title: RefCell<Option<Line<'static>>>,
    filtered_indices: Vec<usize>,
}

impl<C: SongsPanelConfig> SongsPanel<C> {
    pub fn new() -> Self {
        SongsPanel {
            cur_selected: Default::default(),
            list: Default::default(),
            route: Default::default(),
            sort: SortManager::new(),
            filter: FilterManager::new(),
            widget_state: Default::default(),
            _cfg: PhantomData,
            cached_title: RefCell::new(None),
            filtered_indices: Vec::new(),
        }
    }
    pub fn subcolumns_of_vec() -> [ListSongDisplayableField; 5] {
        C::subcolumns()
    }
    pub fn get_song_from_idx(&self, idx: usize) -> Option<&ListSong> {
        self.list.get_song_from_idx(idx)
    }
}

impl<C: SongsPanelConfig> SongListComponent for SongsPanel<C> {
    fn get_song_from_idx(&self, idx: usize) -> Option<&ListSong> {
        self.filtered_indices
            .get(idx)
            .and_then(|&actual_idx| self.list.get_song_from_idx(actual_idx))
    }
}

impl<C: SongsPanelConfig> TextHandler for SongsPanel<C> {
    fn get_text(&self) -> Option<&str> {
        self.filter.get_text()
    }
    fn is_text_handling(&self) -> bool {
        self.route == SongsInputRouting::Filter
    }
    fn clear_text(&mut self) -> bool {
        self.filter.clear_text()
    }
    fn handle_text_event_impl(
        &mut self,
        event: &crossterm::event::Event,
    ) -> Option<Effects<Self>> {
        self.filter
            .handle_text_event_impl(event)
            .map(|effect| effect.map(|this: &mut SongsPanel<C>| &mut this.filter))
    }
}

impl<C: SongsPanelConfig> KeyRouter<AppAction> for SongsPanel<C> {
    fn get_all_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<AppAction>> + 'a {
        std::iter::once(C::keybinds_key(config))
    }
    fn get_active_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<AppAction>> + 'a {
        match self.route {
            SongsInputRouting::List => Either::Left(std::iter::once(C::keybinds_key(config))),
            SongsInputRouting::Filter => Either::Left(std::iter::once(&config.keybinds.filter)),
            SongsInputRouting::Sort => Either::Right(get_sort_keybinds(config)),
        }
    }
}

impl<C: SongsPanelConfig> Loadable for SongsPanel<C> {
    fn is_loading(&self) -> bool {
        matches!(self.list.state, ListStatus::Loading)
    }
}

impl<C: SongsPanelConfig> Scrollable for SongsPanel<C> {
    fn increment_list(&mut self, amount: isize) {
        if self.sort.shown {
            self.sort.cur = self
                .sort
                .cur
                .saturating_add_signed(amount)
                .min(self.get_sortable_columns().len().saturating_sub(1));
        } else {
            self.cur_selected = self
                .cur_selected
                .saturating_add_signed(amount)
                .min(self.filtered_indices.len().saturating_sub(1))
        }
    }
    fn is_scrollable(&self) -> bool {
        !self.filter.shown
    }
}

impl<C: SongsPanelConfig> TableView for SongsPanel<C> {
    fn get_selected_item(&self) -> usize {
        self.cur_selected
    }
    fn get_state(&self) -> &ScrollingTableState {
        &self.widget_state
    }
    fn get_layout(&self) -> &[BasicConstraint] {
        C::layout()
    }
    fn get_items(&self) -> impl ExactSizeIterator<Item = impl Iterator<Item = Cow<'_, str>> + '_> {
        self.list
            .get_list_iter()
            .map(|ls| ls.get_fields(Self::subcolumns_of_vec()).into_iter())
    }
    fn get_headings(&self) -> impl Iterator<Item = &'static str> {
        C::headings().into_iter()
    }
    fn get_highlighted_row(&self) -> Option<usize> {
        None
    }
    fn get_mut_state(&mut self) -> &mut ScrollingTableState {
        &mut self.widget_state
    }
}

impl<C: SongsPanelConfig> AdvancedTableView for SongsPanel<C> {
    fn get_sortable_columns(&self) -> &[usize] {
        C::sortable_columns()
    }
    fn get_filterable_columns(&self) -> &[usize] {
        C::filterable_columns()
    }
}

impl<C: SongsPanelConfig> SortFilterTable for SongsPanel<C> {
    fn get_songs(&self) -> &BrowserSongsList {
        &self.list
    }
    fn get_mut_songs(&mut self) -> &mut BrowserSongsList {
        &mut self.list
    }
    fn set_route_list(&mut self) {
        self.route = SongsInputRouting::List;
    }
    fn set_route_sort(&mut self) {
        self.route = SongsInputRouting::Sort;
    }
    fn set_route_filter(&mut self) {
        self.route = SongsInputRouting::Filter;
    }
    fn route_is_list(&self) -> bool {
        self.route == SongsInputRouting::List
    }
    fn route_is_sort(&self) -> bool {
        self.route == SongsInputRouting::Sort
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

impl<C: SongsPanelConfig> HasTitle for SongsPanel<C> {
    fn get_title(&self) -> Line<'static> {
        if let Some(cached) = self.cached_title.borrow().as_ref() {
            return cached.clone();
        }
        let title = match self.list.state {
            ListStatus::New => Line::from("Songs"),
            ListStatus::Loading => Line::from("Songs - loading"),
            ListStatus::InProgress => Line::from(format!(
                "Songs - {} results - loading",
                self.list.get_list_iter().len()
            )),
            ListStatus::Loaded => {
                let len = self.list.get_list_iter().len();
                if len == 0 {
                    Line::from("Songs - no songs found")
                } else {
                    Line::from(format!("Songs - {len} results"))
                }
            }
            ListStatus::Error => Line::from("Songs - Error received"),
        };
        *self.cached_title.borrow_mut() = Some(title.clone());
        title
    }
}

impl<C: SongsPanelConfig> crate::app::component::actionhandler::Component for SongsPanel<C> {}

// ── Config implementations ──────────────────────────────────────────────

pub struct ArtistSongsConfig;
impl SongsPanelConfig for ArtistSongsConfig {
    fn subcolumns() -> [ListSongDisplayableField; 5] {
        [
            ListSongDisplayableField::TrackNo,
            ListSongDisplayableField::Album,
            ListSongDisplayableField::Song,
            ListSongDisplayableField::Duration,
            ListSongDisplayableField::Year,
        ]
    }
    fn layout() -> &'static [BasicConstraint] {
        &[
            BasicConstraint::Length(4),
            BasicConstraint::Percentage(Percentage(50)),
            BasicConstraint::Percentage(Percentage(50)),
            BasicConstraint::Length(8),
            BasicConstraint::Length(5),
        ]
    }
    fn headings() -> [&'static str; 5] {
        ["#", "Album", "Song", "Duration", "Year"]
    }
    fn sortable_columns() -> &'static [usize] {
        &[1, 4]
    }
    fn filterable_columns() -> &'static [usize] {
        &[1, 2, 4]
    }
    fn keybinds_key(config: &Config) -> &Keymap<AppAction> {
        &config.keybinds.browser_artist_songs
    }
    fn context_name() -> &'static str {
        "Artist Songs Panel"
    }
}

pub struct PlaylistSongsConfig;
impl SongsPanelConfig for PlaylistSongsConfig {
    fn subcolumns() -> [ListSongDisplayableField; 5] {
        [
            ListSongDisplayableField::TrackNo,
            ListSongDisplayableField::Artists,
            ListSongDisplayableField::Album,
            ListSongDisplayableField::Song,
            ListSongDisplayableField::Duration,
        ]
    }
    fn layout() -> &'static [BasicConstraint] {
        &[
            BasicConstraint::Length(4),
            BasicConstraint::Percentage(Percentage(25)),
            BasicConstraint::Percentage(Percentage(30)),
            BasicConstraint::Percentage(Percentage(45)),
            BasicConstraint::Length(8),
        ]
    }
    fn headings() -> [&'static str; 5] {
        ["#", "Artist", "Album", "Song", "Duration"]
    }
    fn sortable_columns() -> &'static [usize] {
        &[0, 1, 2, 3]
    }
    fn filterable_columns() -> &'static [usize] {
        &[1, 2, 3]
    }
    fn keybinds_key(config: &Config) -> &Keymap<AppAction> {
        &config.keybinds.browser_playlist_songs
    }
    fn context_name() -> &'static str {
        "Playlist Songs Panel"
    }
}

// ── Action enums ────────────────────────────────────────────────────────

use crate::define_browser_songs_action;

define_browser_songs_action!(
    BrowserArtistSongsAction,
    ArtistSongsConfig::context_name(),
    PlayAlbum("Play album"),
    AddAlbumToPlaylist("Add album to playlist")
);

define_browser_songs_action!(
    BrowserPlaylistSongsAction,
    PlaylistSongsConfig::context_name()
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::structures::ListSong;
    use crate::app::view::{Filter, FilterString, SortDirection, TableFilterCommand, TableSortCommand};
    use ytmapi_rs::common::{VideoID, YoutubeID};

    fn song_for(video_id: &str, title: &str, album: &str) -> ListSong {
        ListSong::create_with_metadata(
            VideoID::from_raw(video_id.to_owned()),
            title.to_owned(),
            vec!["Artist".into()],
            Some(album.to_owned()),
            "3:00".into(),
        )
    }

    fn panel_with(songs: Vec<ListSong>) -> SongsPanel<ArtistSongsConfig> {
        let mut panel = SongsPanel::new();
        panel.list.push_song_list(songs);
        panel
    }

    fn album_filter(text: &str) -> TableFilterCommand {
        // ArtistSongsConfig filterable_columns are &[1, 2, 4] -> Album/Song/Year.
        TableFilterCommand::All(Filter::Contains(FilterString::case_insensitive(text.into())))
    }

    #[test]
    fn apply_filter_clamps_selection_to_shrunk_filtered_list() {
        let mut panel = panel_with(vec![
            song_for("a", "ta", "Alpha"),
            song_for("b", "tb", "Alpha"),
            song_for("c", "tc", "Beta"),
            song_for("d", "td", "Beta"),
        ]);
        panel.cur_selected = 3;
        panel.filter.filter_text.set_text("Alpha");
        panel.apply_filter();
        assert_eq!(panel.filtered_indices, vec![0, 1]);
        assert_eq!(panel.cur_selected, 1);
        assert_eq!(panel.route, SongsInputRouting::List);
        assert!(!panel.filter.shown);
        assert_eq!(panel.widget_state.offset(), 0);
    }

    #[test]
    fn push_sort_command_dedups_same_column() {
        let mut panel = panel_with(vec![
            song_for("a", "ta", "C"),
            song_for("b", "tb", "A"),
            song_for("c", "tc", "B"),
        ]);
        panel
            .push_sort_command(TableSortCommand {
                column: 1,
                direction: SortDirection::Asc,
            })
            .unwrap();
        assert_eq!(panel.get_song_from_idx(0).unwrap().title, "tb");
        panel
            .push_sort_command(TableSortCommand {
                column: 1,
                direction: SortDirection::Desc,
            })
            .unwrap();
        assert_eq!(panel.get_sort_commands().len(), 1);
        assert_eq!(panel.get_song_from_idx(0).unwrap().title, "ta");
    }

    #[test]
    fn rebuild_filtered_indices_intersects_commands_and_clear_restores() {
        let mut panel = panel_with(vec![
            song_for("a", "ta", "Alpha"),
            song_for("b", "tb", "Alpha"),
            song_for("c", "tc", "Beta"),
            song_for("d", "td", "Beta"),
        ]);
        panel.filter.filter_commands = vec![album_filter("Alpha"), album_filter("tb")];
        panel.rebuild_filtered_indices();
        assert_eq!(panel.filtered_indices, vec![1]);
        panel.clear_filter_commands();
        assert_eq!(panel.filtered_indices, vec![0, 1, 2, 3]);
    }
}
