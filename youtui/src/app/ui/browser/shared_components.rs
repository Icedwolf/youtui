use crate::app::AppCallback;
use crate::app::component::actionhandler::{
    Action, Component, Suggestable, TextHandler,
};
use crate::app::effect::Effects;
use crate::app::structures::ListSong;
use crate::app::view::{TableFilterCommand, TableSortCommand};
use rat_text::text_input::{TextInputState, handle_events};
use ratatui::widgets::ListState;
use serde::{Deserialize, Serialize};
use ytmapi_rs::common::SearchSuggestion;

// --- Song playback helpers (shared by songsearch, artistsearch, playlistsearch) ---

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
    search_suggestions: Vec<SearchSuggestion>,
    pub suggestions_cur: Option<usize>,
    last_fetched_text: Option<String>,
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

impl SortManager {
    pub fn new() -> Self {
        SortManager {
            sort_commands: Default::default(),
            shown: Default::default(),
            cur: Default::default(),
            state: Default::default(),
        }
    }
}
impl FilterManager {
    pub fn new() -> Self {
        Self {
            filter_text: Default::default(),
            filter_commands: Default::default(),
            shown: Default::default(),
        }
    }
}
impl TextHandler for FilterManager {
    fn is_text_handling(&self) -> bool {
        true
    }
    fn get_text(&self) -> std::option::Option<&str> {
        Some(self.filter_text.text())
    }
    fn replace_text(&mut self, text: impl Into<String>) {
        self.filter_text.set_text(text)
    }
    fn clear_text(&mut self) -> bool {
        self.filter_text.clear()
    }
    fn handle_text_event_impl(
        &mut self,
        event: &crossterm::event::Event,
    ) -> Option<Effects<Self>> {
        match handle_events(&mut self.filter_text, true, event) {
            rat_text::event::TextOutcome::Continue => None,
            rat_text::event::TextOutcome::Unchanged => None,
            rat_text::event::TextOutcome::Changed => Some(Effects::none()),
            rat_text::event::TextOutcome::TextChanged => Some(Effects::none()),
        }
    }
}

impl TextHandler for SearchBlock {
    fn is_text_handling(&self) -> bool {
        true
    }
    fn get_text(&self) -> std::option::Option<&str> {
        Some(self.search_contents.text())
    }
    fn replace_text(&mut self, text: impl Into<String>) {
        self.search_contents.set_text(text);
        self.search_contents.move_to_line_end(false);
    }
    fn clear_text(&mut self) -> bool {
        self.search_suggestions.clear();
        self.search_contents.clear()
    }
    fn handle_text_event_impl(
        &mut self,
        event: &crossterm::event::Event,
    ) -> Option<Effects<Self>> {
        match handle_events(&mut self.search_contents, true, event) {
            rat_text::event::TextOutcome::Continue => None,
            rat_text::event::TextOutcome::Unchanged => Some(Effects::none()),
            rat_text::event::TextOutcome::Changed => Some(Effects::none()),
            rat_text::event::TextOutcome::TextChanged => Some(self.fetch_search_suggestions()),
        }
    }
}

impl Suggestable for SearchBlock {
    fn get_search_suggestions(&self) -> &[SearchSuggestion] {
        self.search_suggestions.as_slice()
    }
    fn has_search_suggestions(&self) -> bool {
        !self.search_suggestions.is_empty()
    }
}

impl SearchBlock {
    pub fn delete_word(&mut self) {
        if !self.search_contents.is_empty() {
            let _ = self.search_contents.delete_prev_word();
        }
    }

    // Ask the UI for search suggestions for the current query
    fn fetch_search_suggestions(&mut self) -> Effects<Self> {
        // No need to fetch search suggestions if contents is empty.
        if self.search_contents.is_empty() {
            self.search_suggestions.clear();
            self.last_fetched_text = None;
            return Effects::none();
        }
        let text = self.search_contents.text().to_owned();
        // Skip if text hasn't changed since last fetch (debounce).
        if self.last_fetched_text.as_deref() == Some(&text) {
            return Effects::none();
        }
        self.last_fetched_text = Some(text.clone());
        Effects::none()
    }
    pub fn increment_list(&mut self, amount: isize) {
        if !self.search_suggestions.is_empty() {
            let cur = self
                .suggestions_cur
                .map(|cur| {
                    cur.saturating_add_signed(amount)
                        .min(self.search_suggestions.len() - 1)
                })
                .unwrap_or_default();
            self.suggestions_cur = Some(cur);
            // Bounds-checked via get(): a desync must not panic the event loop.
            if let Some(value) = self.search_suggestions.get(cur) {
                // Clone is ok here as we want to duplicate the search suggestion.
                self.replace_text(value.get_text());
            }
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
            fn get_text(&self) -> std::option::Option<&str> {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self.search_panel.get_text(),
                    SearchBrowserSide::Songs => self.songs_panel.get_text(),
                }
            }
            fn replace_text(&mut self, text: impl Into<String>) {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self.search_panel.replace_text(text),
                    SearchBrowserSide::Songs => self.songs_panel.replace_text(text),
                }
            }
            fn clear_text(&mut self) -> bool {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self.search_panel.clear_text(),
                    SearchBrowserSide::Songs => self.songs_panel.clear_text(),
                }
            }
            fn handle_text_event_impl(
                &mut self,
                event: &crossterm::event::Event,
            ) -> std::option::Option<$crate::app::effect::Effects<Self>>
            {
                use $crate::app::ui::browser::shared_components::SearchBrowserSide;
                match self.side {
                    SearchBrowserSide::Search => self
                        .search_panel
                        .handle_text_event_impl(event)
                        .map(|effect| {
                            effect.map(|this: &mut $name| &mut this.search_panel)
                        }),
                    SearchBrowserSide::Songs => {
                        self.songs_panel
                            .handle_text_event_impl(event)
                            .map(|effect| {
                                effect.map(|this: &mut $name| &mut this.songs_panel)
                            })
                    }
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
                action: $crate::app::ui::browser::shared_components::BrowserSearchAction,
            ) -> impl Into<$crate::app::component::actionhandler::YoutuiEffect<Self>> {
                use $crate::app::ui::browser::shared_components::BrowserSearchAction;
                match action {
                    BrowserSearchAction::PrevSearchSuggestion => {
                        self.search_panel.search.increment_list(-1)
                    }
                    BrowserSearchAction::NextSearchSuggestion => {
                        self.search_panel.search.increment_list(1)
                    }
                }
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
            pub fn search(
                &mut self,
            ) -> $crate::app::effect::Effects<Self> {
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
            pub fn get_songs(
                &mut self,
            ) -> $crate::app::effect::Effects<Self> {
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

