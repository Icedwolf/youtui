use crate::app::component::actionhandler::{
    Action, KeyRouter, Scrollable, Suggestable, TextHandler,
};
use crate::app::effect::Effects;
use crate::app::ui::action::AppAction;
use crate::app::ui::browser::shared_components::SearchBlock;
use crate::app::view::{HasTitle, ListView};
use crate::config::Config;
use crate::config::keymap::Keymap;
use crate::widgets::ScrollingListState;
use ratatui::text::Line;
use std::borrow::Cow;
use std::iter::ExactSizeIterator;
use std::marker::PhantomData;
use ytmapi_rs::common::SearchSuggestion;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum SearchPanelInputRouting {
    #[default]
    Search,
    List,
}

pub trait SearchPanelConfig: Sized + 'static {
    type Item;
    fn list_keybind(config: &Config) -> &Keymap<AppAction>;
    fn display(item: &Self::Item) -> Cow<'_, str>;
    fn title() -> &'static str;
}

pub struct SearchPanel<C: SearchPanelConfig> {
    pub list: Vec<C::Item>,
    pub route: SearchPanelInputRouting,
    selected: usize,
    pub search_popped: bool,
    pub search: SearchBlock,
    pub widget_state: ScrollingListState,
    _cfg: PhantomData<C>,
}

impl<C: SearchPanelConfig> SearchPanel<C> {
    pub fn new() -> Self {
        SearchPanel {
            list: Default::default(),
            route: Default::default(),
            selected: Default::default(),
            search_popped: true,
            search: SearchBlock::default(),
            widget_state: Default::default(),
            _cfg: PhantomData,
        }
    }
    pub fn open_search(&mut self) {
        self.search_popped = true;
        self.route = SearchPanelInputRouting::Search;
    }
    pub fn close_search(&mut self) {
        self.search_popped = false;
        self.route = SearchPanelInputRouting::List;
    }
    pub fn go_to_first(&mut self) {
        self.selected = 0;
    }
    pub fn go_to_last(&mut self) {
        self.selected = self.list.len().saturating_sub(1);
    }
}

impl<C: SearchPanelConfig> crate::app::component::actionhandler::Component for SearchPanel<C> {}

impl<C: SearchPanelConfig> TextHandler for SearchPanel<C> {
    fn is_text_handling(&self) -> bool {
        self.route == SearchPanelInputRouting::Search
    }
    fn get_text(&self) -> Option<&str> {
        self.search.get_text()
    }
    fn replace_text(&mut self, text: impl Into<String>) {
        self.search.replace_text(text)
    }
    fn clear_text(&mut self) -> bool {
        self.search.clear_text()
    }
    fn handle_text_event_impl(
        &mut self,
        event: &crossterm::event::Event,
    ) -> Option<Effects<Self>> {
        self.search
            .handle_text_event_impl(event)
            .map(|effect| effect.map(|this: &mut SearchPanel<C>| &mut this.search))
    }
}

impl<C: SearchPanelConfig> Suggestable for SearchPanel<C> {
    fn get_search_suggestions(&self) -> &[SearchSuggestion] {
        self.search.get_search_suggestions()
    }
    fn has_search_suggestions(&self) -> bool {
        self.search.has_search_suggestions()
    }
}

impl<C: SearchPanelConfig> KeyRouter<AppAction> for SearchPanel<C> {
    fn get_all_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<AppAction>> + 'a {
        [C::list_keybind(config), &config.keybinds.browser_search].into_iter()
    }
    fn get_active_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<AppAction>> + 'a {
        match self.route {
            SearchPanelInputRouting::List => std::iter::once(C::list_keybind(config)),
            SearchPanelInputRouting::Search => std::iter::once(&config.keybinds.browser_search),
        }
    }
}

impl<C: SearchPanelConfig> Scrollable for SearchPanel<C> {
    fn increment_list(&mut self, amount: isize) {
        self.selected = self
            .selected
            .checked_add_signed(amount)
            .unwrap_or(0)
            .min(self.len().checked_add_signed(-1).unwrap_or(0));
    }
    fn is_scrollable(&self) -> bool {
        self.route == SearchPanelInputRouting::List
    }
}

impl<C: SearchPanelConfig> ListView for SearchPanel<C> {
    fn get_selected_item(&self) -> usize {
        self.selected
    }
    fn get_state(&self) -> &ScrollingListState {
        &self.widget_state
    }
    fn get_mut_state(&mut self) -> &mut ScrollingListState {
        &mut self.widget_state
    }
    fn get_items(&self) -> impl ExactSizeIterator<Item = Cow<'_, str>> + '_ {
        self.list.iter().map(C::display)
    }
}

impl<C: SearchPanelConfig> HasTitle for SearchPanel<C> {
    fn get_title(&self) -> Line<'static> {
        Line::from(C::title())
    }
}

/// Consolidation of the two SearchResultPlaylist types (non-podcast).
#[derive(Clone, Debug)]
pub struct NonPodcastSearchResultPlaylist {
    pub title: String,
    pub playlist_id: ytmapi_rs::common::PlaylistID<'static>,
}

impl NonPodcastSearchResultPlaylist {
    pub fn new(
        p: ytmapi_rs::parse::SearchResultPlaylist,
    ) -> Option<NonPodcastSearchResultPlaylist> {
        use ytmapi_rs::parse::SearchResultPlaylist;
        match p {
            SearchResultPlaylist::Featured(p) => Some(NonPodcastSearchResultPlaylist {
                title: p.title,
                playlist_id: p.playlist_id,
            }),
            SearchResultPlaylist::Community(p) => Some(NonPodcastSearchResultPlaylist {
                title: p.title,
                playlist_id: p.playlist_id,
            }),
            SearchResultPlaylist::Podcast(_) => None,
            other => {
                tracing::warn!(
                    "New SearchResultPlaylist type {:?} has been implemented by ytmapi-rs and this is currently ignored by youtui",
                    other
                );
                None
            }
        }
    }
}

// ── Artist config ──────────────────────────────────────────────────────

pub struct ArtistSearchConfig;
impl SearchPanelConfig for ArtistSearchConfig {
    type Item = ytmapi_rs::parse::SearchResultArtist;
    fn list_keybind(config: &Config) -> &Keymap<AppAction> {
        &config.keybinds.browser_artists
    }
    fn display(item: &Self::Item) -> Cow<'_, str> {
        (&item.artist).into()
    }
    fn title() -> &'static str {
        "Artists"
    }
}

#[derive(PartialEq, Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserArtistsAction {
    DisplaySelectedArtistAlbums,
}

impl Action for BrowserArtistsAction {
    fn context(&self) -> Cow<'_, str> {
        Cow::Borrowed("Artist Search Panel")
    }
    fn describe(&self) -> Cow<'_, str> {
        match self {
            Self::DisplaySelectedArtistAlbums => "Display albums for selected artist",
        }
        .into()
    }
}

// ── Playlist config ────────────────────────────────────────────────────

pub struct PlaylistSearchConfig;
impl SearchPanelConfig for PlaylistSearchConfig {
    type Item = NonPodcastSearchResultPlaylist;
    fn list_keybind(config: &Config) -> &Keymap<AppAction> {
        &config.keybinds.browser_playlists
    }
    fn display(item: &Self::Item) -> Cow<'_, str> {
        (&item.title).into()
    }
    fn title() -> &'static str {
        "Playlists"
    }
}

#[derive(PartialEq, Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPlaylistsAction {
    DisplaySelectedPlaylist,
}

impl Action for BrowserPlaylistsAction {
    fn context(&self) -> Cow<'_, str> {
        Cow::Borrowed("Playlist Search Panel")
    }
    fn describe(&self) -> Cow<'_, str> {
        match self {
            Self::DisplaySelectedPlaylist => "Display selected playlist",
        }
        .into()
    }
}
