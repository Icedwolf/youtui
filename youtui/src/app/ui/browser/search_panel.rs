use crate::app::component::actionhandler::{Action, KeyRouter, Scrollable, TextHandler};
use crate::app::effect::Effects;
use crate::app::structures::ListStatus;
use crate::app::ui::action::AppAction;
use crate::app::ui::browser::shared_components::SearchBlock;
use crate::app::view::{HasTitle, ListView, Loadable};
use crate::config::Config;
use crate::config::keymap::Keymap;
use crate::widgets::ScrollingListState;
use ratatui::text::Line;
use std::borrow::Cow;
use std::cell::RefCell;
use std::iter::ExactSizeIterator;
use std::marker::PhantomData;

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
    pub status: ListStatus,
    selected: usize,
    pub search_popped: bool,
    pub search: SearchBlock,
    pub widget_state: ScrollingListState,
    cached_title: RefCell<Option<(ListStatus, usize, Line<'static>)>>,
    _cfg: PhantomData<C>,
}

impl<C: SearchPanelConfig> SearchPanel<C> {
    pub fn new() -> Self {
        SearchPanel {
            list: Default::default(),
            route: Default::default(),
            status: ListStatus::New,
            selected: Default::default(),
            search_popped: true,
            search: SearchBlock::default(),
            widget_state: Default::default(),
            cached_title: RefCell::new(None),
            _cfg: PhantomData,
        }
    }
    pub fn clear_text(&mut self) -> bool {
        self.search.clear_text()
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
    fn handle_text_event_impl(&mut self, event: &crossterm::event::Event) -> Option<Effects<Self>> {
        self.search
            .handle_text_event_impl(event)
            .map(|effect| effect.map(|this: &mut SearchPanel<C>| &mut this.search))
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
        let len = self.list.len();
        {
            let cached = self.cached_title.borrow();
            if let Some((cached_status, cached_len, title)) = cached.as_ref()
                && cached_status == &self.status
                && *cached_len == len
            {
                return title.clone();
            }
        }
        let title = match self.status {
            ListStatus::New => Line::from(C::title()),
            ListStatus::Loading | ListStatus::InProgress => {
                Line::from(format!("{} - loading", C::title()))
            }
            ListStatus::Loaded => {
                if len == 0 {
                    Line::from(format!("{} - nothing found", C::title()))
                } else {
                    Line::from(format!("{} - {len} results", C::title()))
                }
            }
            ListStatus::Error => Line::from(format!("{} - Error received", C::title())),
        };
        *self.cached_title.borrow_mut() = Some((self.status.clone(), len, title.clone()));
        title
    }
}

impl<C: SearchPanelConfig> Loadable for SearchPanel<C> {
    fn is_loading(&self) -> bool {
        matches!(self.status, ListStatus::Loading | ListStatus::InProgress)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ytmapi_rs::common::{ArtistChannelID, YoutubeID};
    use ytmapi_rs::parse::SearchResultArtist;

    fn panel() -> SearchPanel<ArtistSearchConfig> {
        SearchPanel::new()
    }

    fn artist(name: &str, id: &str) -> SearchResultArtist {
        SearchResultArtist::new(
            name.to_string(),
            Some("1M subscribers".to_string()),
            ArtistChannelID::from_raw(id.to_string()),
        )
    }

    fn push(panel: &mut SearchPanel<ArtistSearchConfig>, name: &str, id: &str) {
        panel.list.push(artist(name, id));
    }

    #[test]
    fn get_title_reflects_loading_and_new_states() {
        assert_eq!(panel().get_title().to_string(), "Artists");
        let mut p = panel();
        p.status = ListStatus::Loading;
        assert_eq!(p.get_title().to_string(), "Artists - loading");
        assert!(p.is_loading());
    }

    #[test]
    fn get_title_loaded_shows_count_or_empty() {
        let mut p = panel();
        p.status = ListStatus::Loaded;
        assert_eq!(p.get_title().to_string(), "Artists - nothing found");
        push(&mut p, "The Beatles", "UC2XdaAVUannpujzv32jcouQ");
        push(&mut p, "John Lennon", "UCcSL2nYSJp_IgdzH0xBBdcg");
        assert_eq!(p.get_title().to_string(), "Artists - 2 results");
        assert!(!p.is_loading());
    }

    #[test]
    fn get_title_error_state() {
        let mut p = panel();
        p.status = ListStatus::Error;
        assert_eq!(p.get_title().to_string(), "Artists - Error received");
    }

    #[test]
    fn clear_text_empties_search_and_reports_nonempty() {
        let mut p = panel();
        assert!(!p.clear_text());
        p.search.search_contents.set_text("the beatles");
        assert!(p.clear_text());
        assert_eq!(p.search.search_contents.text(), "");
    }
}
