use crate::app::component::actionhandler::{
    Action, ComponentEffect, KeyRouter, Scrollable, TextHandler,
};
use crate::app::server::ArcServer;
use crate::app::server::TaskMetadata;
use crate::app::structures::{
    BrowserSongsList, ListSong, ListSongDisplayableField, ListStatus, Percentage, SongListComponent,
};
use crate::app::ui::action::AppAction;
use crate::app::ui::browser::get_sort_keybinds;
use crate::app::ui::browser::shared_components::{
    FilterManager, SortManager, get_adjusted_list_column,
};
use crate::app::view::{
    AdvancedTableView, BasicConstraint, Filter as ViewFilter, FilterString, HasTitle, Loadable,
    SortDirection, TableFilterCommand, TableSortCommand, TableView,
};
use crate::config::Config;
use crate::config::keymap::Keymap;
use crate::drawutils::get_offset_after_list_resize;
use crate::widgets::ScrollingTableState;
use anyhow::{Result, bail};
use itertools::Either;
use ratatui::text::Line;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use std::iter::{ExactSizeIterator, Iterator};
use std::marker::PhantomData;
use tracing::debug;

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
    pub fn apply_all_sort_commands(&mut self) -> Result<()> {
        for c in self.sort.sort_commands.iter() {
            if !self.get_sortable_columns().contains(&c.column) {
                bail!(format!("Unable to sort column {}", c.column,));
            }
            let col =
                get_adjusted_list_column(c.column, Self::subcolumns_of_vec()).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Unable to sort column {}, doesn't match underlying list",
                        c.column
                    )
                })?;
            self.list.sort(col, c.direction);
        }
        Ok(())
    }
    pub fn get_filtered_list_iter(&self) -> impl Iterator<Item = &ListSong> {
        self.list.get_list_iter().filter(move |ls| {
            self.get_filter_commands()
                .iter()
                .fold(true, |acc, command| {
                    let match_found = command.matches_row(
                        ls,
                        Self::subcolumns_of_vec(),
                        self.get_filterable_columns(),
                    );
                    acc && match_found
                })
        })
    }
    pub fn rebuild_filtered_indices(&mut self) {
        self.filtered_indices = self
            .list
            .get_list_iter()
            .enumerate()
            .filter(|(_, ls)| {
                self.get_filter_commands()
                    .iter()
                    .all(|command| command.matches_row(ls, Self::subcolumns_of_vec(), self.get_filterable_columns()))
            })
            .map(|(actual_idx, _)| actual_idx)
            .collect();
    }

    pub fn apply_filter(&mut self) {
        self.filter.shown = false;
        self.route = SongsInputRouting::List;
        let Some(filter) = self.filter.get_text().map(|s| s.to_string()) else {
            return;
        };
        let cmd =
            TableFilterCommand::All(ViewFilter::Contains(FilterString::case_insensitive(filter)));
        let prev_max_cur = self.filtered_indices.len().saturating_sub(1);
        let prev_cur = self.cur_selected;
        let prev_offset = self.widget_state.offset();
        self.filter.filter_commands.push(cmd);
        self.rebuild_filtered_indices();
        let new_max_cur = self.filtered_indices.len().saturating_sub(1);
        self.cur_selected = self.cur_selected.min(new_max_cur);
        *self.widget_state.offset_mut() = get_offset_after_list_resize(
            prev_offset,
            prev_cur,
            prev_max_cur,
            self.cur_selected,
            new_max_cur,
        );
    }
    pub fn clear_filter(&mut self) {
        self.filter.shown = false;
        self.route = SongsInputRouting::List;
        self.filter.filter_commands.clear();
        self.rebuild_filtered_indices();
    }
    fn open_sort(&mut self) {
        self.sort.shown = true;
        self.route = SongsInputRouting::Sort;
    }
    pub fn toggle_filter(&mut self) {
        let shown = self.filter.shown;
        if !shown {
            self.filter.filter_text.clear();
            self.route = SongsInputRouting::Filter;
        } else {
            self.route = SongsInputRouting::List;
        }
        self.filter.shown = !shown;
    }
    pub fn close_sort(&mut self) {
        self.sort.shown = false;
        self.route = SongsInputRouting::List;
    }
    pub fn handle_pop_sort(&mut self) {
        self.sort.cur = 0;
        self.open_sort();
    }
    pub fn handle_clear_sort(&mut self) {
        self.close_sort();
        self.clear_sort_commands();
    }
    pub fn handle_sort_cur_asc(&mut self) {
        let Some(column) = self.get_sortable_columns().get(self.sort.cur).copied() else {
            debug!("Tried to index sortable columns but was out of range");
            return;
        };
        if let Err(e) = self.push_sort_command(TableSortCommand {
            column,
            direction: SortDirection::Asc,
        }) {
            debug!("Tried to sort a column that is not sortable - error {e}")
        };
        self.close_sort();
    }
    pub fn handle_sort_cur_desc(&mut self) {
        let Some(column) = self.get_sortable_columns().get(self.sort.cur).copied() else {
            debug!("Tried to index sortable columns but was out of range");
            return;
        };
        if let Err(e) = self.push_sort_command(TableSortCommand {
            column,
            direction: SortDirection::Desc,
        }) {
            debug!("Tried to sort a column that is not sortable - error {e}")
        };
        self.close_sort();
    }
    pub fn get_song_from_idx(&self, idx: usize) -> Option<&ListSong> {
        self.list.get_song_from_idx(idx)
    }
    pub fn go_to_first(&mut self) {
        match self.route {
            SongsInputRouting::List => self.cur_selected = 0,
            SongsInputRouting::Sort => self.cur_selected = 0,
            SongsInputRouting::Filter => debug!("go_to_first called while in filter mode"),
        }
    }
    pub fn go_to_last(&mut self) {
        match self.route {
            SongsInputRouting::List => {
                self.cur_selected = self.filtered_indices.len().saturating_sub(1);
            }
            SongsInputRouting::Sort => {
                self.cur_selected = self.get_sortable_columns().len().saturating_sub(1);
            }
            SongsInputRouting::Filter => debug!("go_to_last called while in filter mode"),
        }
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
    fn replace_text(&mut self, text: impl Into<String>) {
        self.filter.replace_text(text)
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
    ) -> Option<ComponentEffect<Self>> {
        self.filter
            .handle_text_event_impl(event)
            .map(|effect| effect.map_frontend(|this: &mut SongsPanel<C>| &mut this.filter))
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
    fn get_filtered_count(&self) -> usize {
        self.filtered_indices.len()
    }
    fn get_sortable_columns(&self) -> &[usize] {
        C::sortable_columns()
    }
    fn push_sort_command(&mut self, sort_command: TableSortCommand) -> Result<()> {
        if !self.get_sortable_columns().contains(&sort_command.column) {
            bail!(format!("Unable to sort column {}", sort_command.column,));
        }
        self.list.sort(
            get_adjusted_list_column(sort_command.column, Self::subcolumns_of_vec())
                .expect("column was validated against sortable_columns"),
            sort_command.direction,
        );
        self.sort
            .sort_commands
            .retain(|cmd| cmd.column != sort_command.column);
        self.sort.sort_commands.push(sort_command);
        Ok(())
    }
    fn clear_sort_commands(&mut self) {
        self.sort.sort_commands.clear();
    }
    fn get_sort_commands(&self) -> &[TableSortCommand] {
        &self.sort.sort_commands
    }
    fn get_filtered_items(&self) -> impl Iterator<Item = impl Iterator<Item = Cow<'_, str>> + '_> {
        self.get_filtered_list_iter()
            .map(|ls| ls.get_fields(Self::subcolumns_of_vec()).into_iter())
    }
    fn get_filterable_columns(&self) -> &[usize] {
        C::filterable_columns()
    }
    fn get_filter_commands(&self) -> &[TableFilterCommand] {
        &self.filter.filter_commands
    }
    fn clear_filter_commands(&mut self) {
        self.filter.filter_commands.clear();
        self.rebuild_filtered_indices();
    }
    fn get_sort_popup_cur(&self) -> usize {
        self.sort.cur
    }
    fn sort_popup_shown(&self) -> bool {
        self.sort.shown
    }
    fn filter_popup_shown(&self) -> bool {
        self.filter.shown
    }
    fn get_sort_state(&self) -> &ratatui::widgets::ListState {
        &self.sort.state
    }
    fn get_mut_sort_state(&mut self) -> &mut ratatui::widgets::ListState {
        &mut self.sort.state
    }
    fn get_mut_filter_state(&mut self) -> &mut rat_text::text_input::TextInputState {
        &mut self.filter.filter_text
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

impl<C: SongsPanelConfig> crate::app::component::actionhandler::Component for SongsPanel<C> {
    type Bkend = ArcServer;
    type Md = TaskMetadata;
}

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
