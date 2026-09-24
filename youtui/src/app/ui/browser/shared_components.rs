use crate::app::AppCallback;
use crate::app::component::actionhandler::{Action, Component, TextHandler};
use crate::app::effect::Effects;
use crate::app::structures::{BrowserSongsList, ListSong, ListSongDisplayableField};
use crate::app::view::{
    AdvancedTableView, Filter, FilterString, SortDirection, TableFilterCommand, TableSortCommand,
};
use crate::drawutils::get_offset_after_list_resize;
use anyhow::{anyhow, bail};
use rat_text::text_input::{TextInputState, handle_events};
use ratatui::widgets::ListState;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use tracing::debug;

// --- Song playback helpers (shared by songsearch, artistsearch,
// playlistsearch) ---

pub(crate) fn play_song_impl<C: Component>(
    cur_song_idx: usize,
    get_song: impl FnOnce(usize) -> Option<ListSong>,
) -> (Effects<C>, Option<AppCallback>) {
    if let Some(cur_song) = get_song(cur_song_idx) {
        return (
            Effects::none(),
            Some(AppCallback::AddSongsToPlaylistAndPlay(vec![cur_song])),
        );
    }
    (Effects::none(), None)
}

pub(crate) fn add_song_to_playlist_impl<C: Component>(
    cur_song_idx: usize,
    get_song: impl FnOnce(usize) -> Option<ListSong>,
) -> (Effects<C>, Option<AppCallback>) {
    if let Some(cur_song) = get_song(cur_song_idx) {
        return (
            Effects::none(),
            Some(AppCallback::AddSongsToPlaylist(vec![cur_song])),
        );
    }
    (Effects::none(), None)
}

pub(crate) fn play_songs_impl<C: Component>(
    song_list: Vec<ListSong>,
) -> (Effects<C>, Option<AppCallback>) {
    (
        Effects::none(),
        Some(AppCallback::AddSongsToPlaylistAndPlay(song_list)),
    )
}

pub(crate) fn add_songs_to_playlist_impl<C: Component>(
    song_list: Vec<ListSong>,
) -> (Effects<C>, Option<AppCallback>) {
    (
        Effects::none(),
        Some(AppCallback::AddSongsToPlaylist(song_list)),
    )
}

#[derive(Default)]
pub struct SearchBlock {
    pub search_contents: TextInputState,
}
impl Component for SearchBlock {}

#[derive(Clone)]
pub struct FilterManager {
    pub filter_commands: Vec<TableFilterCommand>,
    pub filter_text: TextInputState,
    pub shown: bool,
}
impl Component for FilterManager {}

impl Default for FilterManager {
    fn default() -> Self {
        Self {
            filter_commands: Vec::new(),
            filter_text: TextInputState::new(),
            shown: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct SortManager {
    pub sort_commands: Vec<TableSortCommand>,
    pub shown: bool,
    pub cur: usize,
    pub state: ListState,
}
impl Component for SortManager {}

#[derive(PartialEq, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterAction {
    Close,
    ClearFilter,
    Apply,
}

#[derive(PartialEq, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortAction {
    Close,
    ClearSort,
    SortSelectedAsc,
    SortSelectedDesc,
}

#[derive(PartialEq, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserSearchAction {
    PrevSearchSuggestion,
    NextSearchSuggestion,
}

impl Action for FilterAction {
    fn context(&self) -> std::borrow::Cow<'_, str> {
        "Filter".into()
    }
    fn describe(&self) -> std::borrow::Cow<'_, str> {
        match self {
            FilterAction::Close => "Close Filter",
            FilterAction::Apply => "Apply filter",
            FilterAction::ClearFilter => "Clear filter",
        }
        .into()
    }
}

impl Action for SortAction {
    fn context(&self) -> std::borrow::Cow<'_, str> {
        "Sort".into()
    }
    fn describe(&self) -> std::borrow::Cow<'_, str> {
        match self {
            SortAction::Close => "Close sort",
            SortAction::ClearSort => "Clear sort",
            SortAction::SortSelectedAsc => "Sort ascending",
            SortAction::SortSelectedDesc => "Sort descending",
        }
        .into()
    }
}

impl Action for BrowserSearchAction {
    fn context(&self) -> std::borrow::Cow<'_, str> {
        "Browser Search Panel".into()
    }
    fn describe(&self) -> std::borrow::Cow<'_, str> {
        match self {
            BrowserSearchAction::PrevSearchSuggestion => "Prev Search Suggestion",
            BrowserSearchAction::NextSearchSuggestion => "Next Search Suggestion",
        }
        .into()
    }
}

impl FilterManager {
    pub fn get_text(&self) -> std::option::Option<&str> {
        Some(self.filter_text.text())
    }
}
impl TextHandler for FilterManager {
    fn is_text_handling(&self) -> bool {
        // Vestigial: no caller consults this. The gate that actually decides
        // whether filter text is handled lives in `SongsPanel::is_text_handling`
        // (route == Filter) — this true is never reached.
        true
    }
    fn handle_text_event_impl(&mut self, event: &crossterm::event::Event) -> Option<Effects<Self>> {
        match handle_events(&mut self.filter_text, true, event) {
            rat_text::event::TextOutcome::Continue => None,
            _ => Some(Effects::none()),
        }
    }
}

impl SearchBlock {
    pub fn get_text(&self) -> std::option::Option<&str> {
        Some(self.search_contents.text())
    }
    pub fn clear_text(&mut self) -> bool {
        self.search_contents.clear()
    }
}
impl TextHandler for SearchBlock {
    fn is_text_handling(&self) -> bool {
        // Vestigial: no caller consults this. The gate that actually decides
        // whether search text is handled lives in `SearchPanel::is_text_handling`
        // (route == Search) — this true is never reached.
        true
    }
    fn handle_text_event_impl(&mut self, event: &crossterm::event::Event) -> Option<Effects<Self>> {
        match handle_events(&mut self.search_contents, true, event) {
            rat_text::event::TextOutcome::Continue => None,
            _ => Some(Effects::none()),
        }
    }
}

impl SearchBlock {
    pub fn delete_word(&mut self) {
        if !self.search_contents.is_empty() {
            let _ = self.search_contents.delete_prev_word();
        }
    }
}

#[macro_export]
macro_rules! define_browser_songs_action {
    ($name:ident, $context:expr $(, $variant:ident($desc:expr) )*) => {
        #[derive(PartialEq, Clone, Copy, Debug, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            Filter,
            Sort,
            PlaySong,
            PlaySongs,
            AddSongToPlaylist,
            AddSongsToPlaylist,
            $($variant,)*
        }
        impl Action for $name {
            fn context(&self) -> std::borrow::Cow<'_, str> {
                std::borrow::Cow::Borrowed($context)
            }
            fn describe(&self) -> std::borrow::Cow<'_, str> {
                match self {
                    Self::Filter => "Filter",
                    Self::Sort => "Sort",
                    Self::PlaySong => "Play song",
                    Self::PlaySongs => "Play songs",
                    Self::AddSongToPlaylist => "Add song to playlist",
                    Self::AddSongsToPlaylist => "Add songs to playlist",
                    $(Self::$variant => $desc,)*
                }
                .into()
            }
        }
    };
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SearchBrowserSide {
    #[default]
    Search,
    Songs,
}

#[macro_export]
macro_rules! define_search_results_browser {
    (
        $name:ident,
        search_panel: $search_panel:ty,
        songs_panel: $songs_panel:ty,
        songs_action_ty: $songs_action_ty:ty,
        search_action_ty: $search_action_ty:ty,
        search_action_variant: $search_action_variant:ident,
    ) => {
        pub struct $name {
            pub side: $crate::app::ui::browser::shared_components::SearchBrowserSide,
            pub prev_side: $crate::app::ui::browser::shared_components::SearchBrowserSide,
            pub search_panel: $search_panel,
            pub songs_panel: $songs_panel,
        }
        impl $crate::app::component::actionhandler::Component for $name {}

        impl $crate::app::component::actionhandler::Scrollable for $name {
            fn increment_list(&mut self, amount: isize) {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self.search_panel.increment_list(amount),
                    SearchBrowserSide::Songs => self.songs_panel.increment_list(amount),
                }
            }
            fn is_scrollable(&self) -> bool {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self.search_panel.is_scrollable(),
                    SearchBrowserSide::Songs => self.songs_panel.is_scrollable(),
                }
            }
        }

        impl $crate::app::component::actionhandler::TextHandler for $name {
            fn is_text_handling(&self) -> bool {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self.search_panel.is_text_handling(),
                    SearchBrowserSide::Songs => self.songs_panel.is_text_handling(),
                }
            }
            fn handle_text_event_impl(
                &mut self,
                event: &crossterm::event::Event,
            ) -> std::option::Option<$crate::app::effect::Effects<Self>> {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self
                        .search_panel
                        .handle_text_event_impl(event)
                        .map(|effect| effect.map(|this: &mut $name| &mut this.search_panel)),
                    SearchBrowserSide::Songs => self
                        .songs_panel
                        .handle_text_event_impl(event)
                        .map(|effect| effect.map(|this: &mut $name| &mut this.songs_panel)),
                }
            }
        }

        impl
            $crate::app::component::actionhandler::ActionHandler<
                $crate::app::ui::browser::shared_components::FilterAction,
            > for $name
        {
            fn apply_action(
                &mut self,
                action: $crate::app::ui::browser::shared_components::FilterAction,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                use $crate::app::ui::browser::shared_components::FilterAction;
                match action {
                    FilterAction::Close => self.songs_panel.toggle_filter(),
                    FilterAction::Apply => self.songs_panel.apply_filter(),
                    FilterAction::ClearFilter => self.songs_panel.clear_filter(),
                };
                Effects::none()
            }
        }
        impl
            $crate::app::component::actionhandler::ActionHandler<
                $crate::app::ui::browser::shared_components::SortAction,
            > for $name
        {
            fn apply_action(
                &mut self,
                action: $crate::app::ui::browser::shared_components::SortAction,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                use $crate::app::ui::browser::shared_components::SortAction;
                match action {
                    SortAction::SortSelectedAsc => self.songs_panel.handle_sort_cur_asc(),
                    SortAction::SortSelectedDesc => self.songs_panel.handle_sort_cur_desc(),
                    SortAction::Close => self.songs_panel.close_sort(),
                    SortAction::ClearSort => self.songs_panel.handle_clear_sort(),
                }
                Effects::none()
            }
        }
        impl
            $crate::app::component::actionhandler::ActionHandler<
                $crate::app::ui::browser::shared_components::BrowserSearchAction,
            > for $name
        {
            fn apply_action(
                &mut self,
                _action: $crate::app::ui::browser::shared_components::BrowserSearchAction,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                // Search suggestions were removed (the fetch was never wired and
                // the list never populated — see AGENTS.md scope). The keybinds
                // remain as no-ops solely so existing config.toml
                // `[keybinds.browser_search]` sections keep parsing.
                Effects::none()
            }
        }
        impl $crate::app::component::actionhandler::ActionHandler<$songs_action_ty> for $name {
            fn apply_action(
                &mut self,
                action: $songs_action_ty,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                #[allow(unreachable_patterns)]
                match action {
                    <$songs_action_ty>::PlaySong => return self.play_song().into(),
                    <$songs_action_ty>::PlaySongs => return self.play_songs().into(),
                    <$songs_action_ty>::AddSongToPlaylist => {
                        return self.add_song_to_playlist().into();
                    }
                    <$songs_action_ty>::AddSongsToPlaylist => {
                        return self.add_songs_to_playlist().into();
                    }
                    <$songs_action_ty>::Sort => self.songs_panel.handle_pop_sort(),
                    <$songs_action_ty>::Filter => self.songs_panel.toggle_filter(),
                    _ => {}
                }
                self.handle_extra_song_action(action)
            }
        }
        impl $crate::app::component::actionhandler::ActionHandler<$search_action_ty> for $name {
            fn apply_action(
                &mut self,
                action: $search_action_ty,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                match action {
                    <$search_action_ty>::$search_action_variant => self.get_songs(),
                }
            }
        }

        impl $crate::app::component::actionhandler::KeyRouter<$crate::app::ui::action::AppAction>
            for $name
        {
            fn get_all_keybinds<'a>(
                &self,
                config: &'a $crate::config::Config,
            ) -> impl std::iter::Iterator<
                Item = &'a $crate::config::keymap::Keymap<$crate::app::ui::action::AppAction>,
            > + 'a {
                self.search_panel
                    .get_all_keybinds(config)
                    .chain(self.songs_panel.get_all_keybinds(config))
            }
            fn get_active_keybinds<'a>(
                &self,
                config: &'a $crate::config::Config,
            ) -> impl std::iter::Iterator<
                Item = &'a $crate::config::keymap::Keymap<$crate::app::ui::action::AppAction>,
            > + 'a {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => {
                        itertools::Either::Left(self.search_panel.get_active_keybinds(config))
                    }
                    SearchBrowserSide::Songs => {
                        itertools::Either::Right(self.songs_panel.get_active_keybinds(config))
                    }
                }
            }
        }

        impl $name {
            pub fn left(&mut self) {
                self.change_routing(
                    $crate::app::ui::browser::shared_components::SearchBrowserSide::Search,
                );
            }
            pub fn right(&mut self) {
                self.change_routing(
                    $crate::app::ui::browser::shared_components::SearchBrowserSide::Songs,
                );
            }
            pub fn new(search_panel: $search_panel, songs_panel: $songs_panel) -> Self {
                Self {
                    side: Default::default(),
                    prev_side: Default::default(),
                    search_panel,
                    songs_panel,
                }
            }
            pub fn handle_toggle_search(&mut self) {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                if self.search_panel.search_popped {
                    self.search_panel.close_search();
                    self.revert_routing();
                } else {
                    self.search_panel.open_search();
                    self.change_routing(SearchBrowserSide::Search);
                }
            }
            pub fn handle_text_entry_action(
                &mut self,
                action: $crate::app::ui::action::TextEntryAction,
            ) -> $crate::app::effect::Effects<Self> {
                use $crate::app::ui::action::TextEntryAction;
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                if self.is_text_handling()
                    && self.search_panel.search_popped
                    && self.side == SearchBrowserSide::Search
                {
                    match action {
                        TextEntryAction::Submit => {
                            return self.search();
                        }
                        TextEntryAction::DeleteWord => {
                            self.search_panel.search.delete_word();
                            return Effects::none();
                        }
                        _ => return Effects::none(),
                    }
                }
                Effects::none()
            }
            pub fn search(&mut self) -> $crate::app::effect::Effects<Self> {
                self.search_panel.close_search();
                let Some(search_query) = self
                    .search_panel
                    .search
                    .get_text()
                    .map(|s: &str| s.to_string())
                else {
                    return Effects::none();
                };
                self.search_panel.clear_text();
                self.execute_search(search_query)
            }
            pub fn get_songs(&mut self) -> $crate::app::effect::Effects<Self> {
                let selected = self.search_panel.get_selected_item();
                self.change_routing(
                    $crate::app::ui::browser::shared_components::SearchBrowserSide::Songs,
                );
                self.songs_panel.list.clear();
                self.execute_get_songs(selected)
            }
            pub fn handle_song_list_loaded(&mut self) {
                self.songs_panel.list.state = $crate::app::structures::ListStatus::Loaded;
            }
            pub fn handle_song_list_loading(&mut self) {
                self.songs_panel.list.state = $crate::app::structures::ListStatus::Loading;
            }
            pub fn play_song(
                &mut self,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                $crate::app::ui::browser::shared_components::play_song_impl::<Self>(
                    self.songs_panel.get_selected_item(),
                    |idx| self.songs_panel.get_song_from_idx(idx).cloned(),
                )
            }
            pub fn play_songs(
                &mut self,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                let cur_idx = self.songs_panel.get_selected_item();
                let song_list = self
                    .songs_panel
                    .get_filtered_list_iter()
                    .skip(cur_idx)
                    .cloned()
                    .collect();
                $crate::app::ui::browser::shared_components::play_songs_impl::<Self>(song_list)
            }
            pub fn add_song_to_playlist(
                &mut self,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                $crate::app::ui::browser::shared_components::add_song_to_playlist_impl::<Self>(
                    self.songs_panel.get_selected_item(),
                    |idx| self.songs_panel.get_song_from_idx(idx).cloned(),
                )
            }
            pub fn add_songs_to_playlist(
                &mut self,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                let cur_idx = self.songs_panel.get_selected_item();
                let song_list = self
                    .songs_panel
                    .get_filtered_list_iter()
                    .skip(cur_idx)
                    .cloned()
                    .collect();
                $crate::app::ui::browser::shared_components::add_songs_to_playlist_impl::<Self>(
                    song_list,
                )
            }
            fn increment_cur_list(&mut self, increment: isize) {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => {
                        self.search_panel.increment_list(increment);
                    }
                    SearchBrowserSide::Songs => {
                        self.songs_panel.increment_list(increment);
                    }
                };
            }
            pub fn revert_routing(&mut self) {
                std::mem::swap(&mut self.side, &mut self.prev_side);
            }
            pub fn change_routing(
                &mut self,
                side: $crate::app::ui::browser::shared_components::SearchBrowserSide,
            ) {
                self.prev_side = std::mem::replace(&mut self.side, side);
            }
            pub fn go_to_first(&mut self) {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self.search_panel.go_to_first(),
                    SearchBrowserSide::Songs => self.songs_panel.go_to_first(),
                }
            }
            pub fn go_to_last(&mut self) {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self.search_panel.go_to_last(),
                    SearchBrowserSide::Songs => self.songs_panel.go_to_last(),
                }
            }
        }
    };
}

/// A table may display columns in a different order, adjust the index to a new
/// index based on a list of correct indexes.
pub fn get_adjusted_list_column<T: Copy, const N: usize>(
    target_col: usize,
    adjusted_cols: [T; N],
) -> Option<T> {
    adjusted_cols.get(target_col).copied()
}

/// Shared filter/sort/route behavior for `SongsPanel` and `SongSearchBrowser`.
///
/// Callers dispatch through the concrete types; Rust resolves these methods
/// automatically via the trait whenever the trait is in scope.
pub(crate) trait SortFilterTable: AdvancedTableView {
    // ---- accessors (one-liners per implementor) ----
    fn get_songs(&self) -> &BrowserSongsList;
    fn get_mut_songs(&mut self) -> &mut BrowserSongsList;
    fn set_route_list(&mut self);
    fn set_route_sort(&mut self);
    fn set_route_filter(&mut self);
    fn route_is_list(&self) -> bool;
    fn route_is_sort(&self) -> bool;
    fn set_cur_selected(&mut self, idx: usize);
    fn set_filtered_indices(&mut self, indices: Vec<usize>);
    fn get_filtered_indices(&self) -> &[usize];
    fn get_filter_manager(&self) -> &FilterManager;
    fn get_mut_filter_manager(&mut self) -> &mut FilterManager;
    fn get_sort_manager(&self) -> &SortManager;
    fn get_mut_sort_manager(&mut self) -> &mut SortManager;
    fn get_subcolumns() -> [ListSongDisplayableField; 5];

    // ---- defaults (canonical duplicated body) ----

    fn apply_all_sort_commands(&mut self) -> anyhow::Result<()> {
        let sort_commands = self.get_sort_commands().to_vec();
        for c in sort_commands.iter() {
            if !self.get_sortable_columns().contains(&c.column) {
                bail!(format!("Unable to sort column {}", c.column,));
            }
            let col =
                get_adjusted_list_column(c.column, Self::get_subcolumns()).ok_or_else(|| {
                    anyhow!(
                        "Unable to sort column {}, doesn't match underlying list",
                        c.column
                    )
                })?;
            self.get_mut_songs().sort(col, c.direction);
        }
        self.rebuild_filtered_indices();
        Ok(())
    }

    fn get_filtered_list_iter(&self) -> impl Iterator<Item = &ListSong> + '_ {
        self.get_songs().get_list_iter().filter(move |ls| {
            self.get_filter_commands()
                .iter()
                .fold(true, |acc, command| {
                    let match_found = command.matches_row(
                        ls,
                        Self::get_subcolumns(),
                        self.get_filterable_columns(),
                    );
                    acc && match_found
                })
        })
    }

    fn rebuild_filtered_indices(&mut self) {
        let songs = self.get_songs();
        let cmds = self.get_filter_commands();
        let cols = self.get_filterable_columns();
        let sub = Self::get_subcolumns();
        let indices: Vec<usize> = songs
            .get_list_iter()
            .enumerate()
            .filter(|(_, ls)| {
                cmds.iter()
                    .all(|command| command.matches_row(ls, sub, cols))
            })
            .map(|(actual_idx, _)| actual_idx)
            .collect();
        self.set_filtered_indices(indices);
    }

    fn get_filtered_count(&self) -> usize {
        self.get_filtered_indices().len()
    }

    fn get_filtered_items(&self) -> impl Iterator<Item = impl Iterator<Item = Cow<'_, str>> + '_> {
        self.get_filtered_indices()
            .iter()
            .filter_map(|&idx| self.get_songs().get_song_from_idx(idx))
            .map(|ls| ls.get_fields(Self::get_subcolumns()).into_iter())
    }

    fn get_sort_commands(&self) -> &[TableSortCommand] {
        &self.get_sort_manager().sort_commands
    }

    fn push_sort_command(&mut self, sort_command: TableSortCommand) -> anyhow::Result<()> {
        if !self.get_sortable_columns().contains(&sort_command.column) {
            bail!(format!("Unable to sort column {}", sort_command.column,));
        }
        self.get_mut_songs().sort(
            get_adjusted_list_column(sort_command.column, Self::get_subcolumns())
                .expect("column was validated against sortable_columns"),
            sort_command.direction,
        );
        self.get_mut_sort_manager()
            .sort_commands
            .retain(|cmd| cmd.column != sort_command.column);
        self.get_mut_sort_manager().sort_commands.push(sort_command);
        self.rebuild_filtered_indices();
        Ok(())
    }

    fn clear_sort_commands(&mut self) {
        self.get_mut_sort_manager().sort_commands.clear();
    }

    fn get_filter_commands(&self) -> &[TableFilterCommand] {
        &self.get_filter_manager().filter_commands
    }

    fn clear_filter_commands(&mut self) {
        self.get_mut_filter_manager().filter_commands.clear();
        self.rebuild_filtered_indices();
    }

    fn get_sort_popup_cur(&self) -> usize {
        self.get_sort_manager().cur
    }

    fn sort_popup_shown(&self) -> bool {
        self.get_sort_manager().shown
    }

    fn filter_popup_shown(&self) -> bool {
        self.get_filter_manager().shown
    }

    fn get_sort_state(&self) -> &ratatui::widgets::ListState {
        &self.get_sort_manager().state
    }

    fn get_mut_sort_state(&mut self) -> &mut ratatui::widgets::ListState {
        &mut self.get_mut_sort_manager().state
    }

    fn get_mut_filter_state(&mut self) -> &mut rat_text::text_input::TextInputState {
        &mut self.get_mut_filter_manager().filter_text
    }

    fn apply_filter(&mut self) {
        self.get_mut_filter_manager().shown = false;
        self.set_route_list();
        let Some(filter) = self.get_filter_manager().get_text().map(|s| s.to_string()) else {
            return;
        };
        let cmd = TableFilterCommand::All(Filter::Contains(FilterString::case_insensitive(filter)));
        let prev_max_cur = self.get_filtered_count().saturating_sub(1);
        let prev_cur = self.get_selected_item();
        let prev_offset = self.get_state().offset();
        self.get_mut_filter_manager().filter_commands.push(cmd);
        self.rebuild_filtered_indices();
        let new_max_cur = self.get_filtered_count().saturating_sub(1);
        let new_cur = self.get_selected_item().min(new_max_cur);
        self.set_cur_selected(new_cur);
        *self.get_mut_state().offset_mut() =
            get_offset_after_list_resize(prev_offset, prev_cur, prev_max_cur, new_cur, new_max_cur);
    }

    fn clear_filter(&mut self) {
        self.get_mut_filter_manager().shown = false;
        self.set_route_list();
        self.clear_filter_commands();
    }

    fn open_sort(&mut self) {
        self.get_mut_sort_manager().shown = true;
        self.set_route_sort();
    }

    fn toggle_filter(&mut self) {
        let shown = self.filter_popup_shown();
        if !shown {
            self.get_mut_filter_manager().filter_text.clear();
            self.set_route_filter();
        } else {
            self.set_route_list();
        }
        self.get_mut_filter_manager().shown = !shown;
    }

    fn close_sort(&mut self) {
        self.get_mut_sort_manager().shown = false;
        self.set_route_list();
    }

    fn handle_pop_sort(&mut self) {
        self.get_mut_sort_manager().cur = 0;
        self.open_sort();
    }

    fn handle_clear_sort(&mut self) {
        self.close_sort();
        self.clear_sort_commands();
    }

    fn handle_sort_cur_asc(&mut self) {
        let Some(column) = self
            .get_sortable_columns()
            .get(self.get_sort_manager().cur)
            .copied()
        else {
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

    fn handle_sort_cur_desc(&mut self) {
        let Some(column) = self
            .get_sortable_columns()
            .get(self.get_sort_manager().cur)
            .copied()
        else {
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

    fn go_to_first(&mut self) {
        if self.route_is_sort() {
            self.get_mut_sort_manager().cur = 0;
        } else if self.route_is_list() {
            self.set_cur_selected(0);
        } else {
            debug!("go_to_first called while in filter/search mode");
        }
    }

    fn go_to_last(&mut self) {
        if self.route_is_sort() {
            self.get_mut_sort_manager().cur = self.get_sortable_columns().len().saturating_sub(1);
        } else if self.route_is_list() {
            self.set_cur_selected(self.get_filtered_count().saturating_sub(1));
        } else {
            debug!("go_to_last called while in filter/search mode");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_block_get_text_roundtrips_contents() {
        let mut block = SearchBlock::default();
        assert_eq!(block.get_text(), Some(""));
        block.search_contents.set_text("query");
        assert_eq!(block.get_text(), Some("query"));
    }

    #[test]
    fn search_block_clear_text_empties_and_reports_nonempty() {
        let mut block = SearchBlock::default();
        block.search_contents.set_text("query");
        assert!(block.clear_text());
        assert_eq!(block.get_text(), Some(""));
        // Empty clear reports false (rat-text TextInputState::clear contract).
        assert!(!block.clear_text());
    }

    #[test]
    fn filter_manager_get_text_roundtrips_contents() {
        let mut filter = FilterManager::default();
        assert_eq!(filter.get_text(), Some(""));
        filter.filter_text.set_text("album");
        assert_eq!(filter.get_text(), Some("album"));
    }
}
