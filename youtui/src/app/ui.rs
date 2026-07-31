use self::browser::Browser;
use self::playlist::Playlist;
use super::AppCallback;
use super::component::actionhandler::{
    ActionHandler, Component, DominantKeyRouter, KeyHandleAction, KeyRouter, Scrollable,
    TextHandler, YoutuiEffect, apply_action_mapped, handle_key_stack,
};
use super::structures::{ListSong, Percentage};
use crate::app::effect::Effects;
use crate::app::ui::footer::FooterCache;
use crate::app::server::ArcServer;
use std::sync::Arc;
use crate::config::Config;
use crate::config::keymap::Keymap;
use crate::keyaction::{DisplayableKeyAction, DisplayableMode, flatten_keybinds_as_readable};
use crate::widgets::ScrollingTableState;
use action::{AppAction, ListAction, PAGE_KEY_LINES, TextEntryAction};
use crossterm::event::{Event, KeyEvent};
use itertools::Either;

pub mod action;
pub mod browser;
pub mod draw;
pub mod draw_media_controls;
mod footer;
mod header;
pub mod playlist;

// Which app level keyboard shortcuts function.
// What is displayed in header
// The main pane of the application
// XXX: This is a bit like a route.
#[derive(Debug)]
pub enum WindowContext {
    Browser,
    Playlist,
}

pub struct YoutuiWindow {
    context: WindowContext,
    pub playlist: Playlist,
    browser: Browser,
    config: Config,
    key_stack: Vec<KeyEvent>,
    help: HelpMenu,
    tick: u64,
    footer_cache: FooterCache,
}
impl Component for YoutuiWindow {}

pub struct HelpMenu {
    pub shown: bool,
    cur: usize,
    len: usize,
    pub widget_state: ScrollingTableState,
}

impl HelpMenu {
    fn new() -> Self {
        HelpMenu {
            shown: Default::default(),
            cur: Default::default(),
            len: Default::default(),
            widget_state: Default::default(),
        }
    }
}
impl Component for HelpMenu {}

impl Scrollable for HelpMenu {
    fn increment_list(&mut self, amount: isize) {
        self.cur = self
            .cur
            .saturating_add_signed(amount)
            .min(self.len.saturating_sub(1));
    }
    fn is_scrollable(&self) -> bool {
        true
    }
}

impl DominantKeyRouter<AppAction> for YoutuiWindow {
    fn dominant_keybinds_active(&self) -> bool {
        self.help.shown
            || match self.context {
                WindowContext::Browser => self.browser.dominant_keybinds_active(),
                WindowContext::Playlist => false,
            }
    }

    #[allow(refining_impl_trait_internal)]
    fn get_dominant_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> Box<dyn Iterator<Item = &'a Keymap<AppAction>> + 'a> {
        if self.help.shown {
            return Box::new([&config.keybinds.help, &config.keybinds.list].into_iter());
        }
        match self.context {
            WindowContext::Browser => Box::new(self.browser.get_dominant_keybinds(config)),
            WindowContext::Playlist => Box::new(self.playlist.get_active_keybinds(config)),
        }
    }
}

impl Scrollable for YoutuiWindow {
    fn increment_list(&mut self, amount: isize) {
        if self.help.shown {
            return self.help.increment_list(amount);
        }
        match self.context {
            WindowContext::Browser => self.browser.increment_list(amount),
            WindowContext::Playlist => self.playlist.increment_list(amount),
        }
    }
    fn is_scrollable(&self) -> bool {
        self.help.shown
            || match self.context {
                WindowContext::Browser => self.browser.is_scrollable(),
                WindowContext::Playlist => self.playlist.is_scrollable(),
            }
    }
}

impl KeyRouter<AppAction> for YoutuiWindow {
    #[allow(refining_impl_trait_internal)]
    fn get_active_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> Box<dyn Iterator<Item = &'a Keymap<AppAction>> + 'a> {
        let kb = if self.is_scrollable() {
            Either::Left(std::iter::once(&config.keybinds.list))
        } else {
            Either::Right(std::iter::empty())
        };
        if self.dominant_keybinds_active() {
            return Box::new(self.get_dominant_keybinds(config).chain(kb));
        }
        let kb = kb.chain(std::iter::once(&config.keybinds.global));
        let kb = if self.is_text_handling() {
            Either::Left(kb.chain(std::iter::once(&config.keybinds.text_entry)))
        } else {
            Either::Right(kb)
        };
        match self.context {
            WindowContext::Browser => Box::new(kb.chain(self.browser.get_active_keybinds(config))),
            WindowContext::Playlist => {
                Box::new(kb.chain(self.playlist.get_active_keybinds(config)))
            }
        }
    }
    #[allow(refining_impl_trait_internal)]
    fn get_all_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> Box<dyn Iterator<Item = &'a Keymap<AppAction>> + 'a> {
        Box::new(
            std::iter::once(&config.keybinds.global)
                .chain(self.browser.get_all_keybinds(config))
                .chain(self.playlist.get_all_keybinds(config)),
        )
    }
}

impl TextHandler for YoutuiWindow {
    fn is_text_handling(&self) -> bool {
        if self.help.shown {
            return false;
        }
        match self.context {
            WindowContext::Browser => self.browser.is_text_handling(),
            WindowContext::Playlist => self.playlist.is_text_handling(),
        }
    }
    fn get_text(&self) -> std::option::Option<&str> {
        match self.context {
            WindowContext::Browser => self.browser.get_text(),
            WindowContext::Playlist => self.playlist.get_text(),
        }
    }
    fn replace_text(&mut self, text: impl Into<String>) {
        match self.context {
            WindowContext::Browser => self.browser.replace_text(text),
            WindowContext::Playlist => self.playlist.replace_text(text),
        }
    }
    fn clear_text(&mut self) -> bool {
        match self.context {
            WindowContext::Browser => self.browser.clear_text(),
            WindowContext::Playlist => self.playlist.clear_text(),
        }
    }
    fn handle_text_event_impl(&mut self, event: &Event) -> Option<Effects<Self>> {
        match self.context {
            WindowContext::Browser => self
                .browser
                .handle_text_event_impl(event)
                .map(|effect| effect.map(|this: &mut YoutuiWindow| &mut this.browser)),
            WindowContext::Playlist => self
                .playlist
                .handle_text_event_impl(event)
                .map(|effect| effect.map(|this: &mut YoutuiWindow| &mut this.playlist)),
        }
    }
}

impl ActionHandler<AppAction> for YoutuiWindow {
    fn apply_action(&mut self, action: AppAction) -> impl Into<YoutuiEffect<Self>> {
        // NOTE: This is the place to check if we _should_ be handling an action.
        // For example if a user has set custom 'playlist' keybinds that trigger
        // 'browser' actions, but browser is not shown currently, this could be filtered
        // out here.
        match action {
            AppAction::VolUp => {
                return Into::<YoutuiEffect<Self>>::into(self.handle_increase_volume(5));
            }
            AppAction::VolDown => return self.handle_increase_volume(-5).into(),
            AppAction::NextSong => return self.handle_next().into(),
            AppAction::PrevSong => return self.handle_prev().into(),
            AppAction::ToggleHelp => self.toggle_help(),
            AppAction::Quit => return (Effects::none(), Some(AppCallback::Quit)).into(),
            AppAction::PlayPause => return self.pauseplay().into(),
            AppAction::Playlist(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.playlist);
            }
            AppAction::Help(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.help);
            }
            AppAction::Browser(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.browser);
            }
            AppAction::Filter(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.browser);
            }
            AppAction::Sort(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.browser);
            }
            AppAction::BrowserArtists(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.browser);
            }
            AppAction::BrowserSearch(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.browser);
            }
            AppAction::BrowserArtistSongs(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.browser);
            }
            AppAction::BrowserSongs(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.browser);
            }
            AppAction::BrowserPlaylists(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.browser);
            }
            AppAction::BrowserPlaylistSongs(a) => {
                return apply_action_mapped(self, a, |this: &mut Self| &mut this.browser);
            }
            AppAction::TextEntry(a) => return self.handle_text_entry_action(a).into(),
            AppAction::List(a) => return self.handle_list_action(a).into(),
            AppAction::NoOp => (),
        };
        Effects::none().into()
    }
}

impl YoutuiWindow {
    pub fn new(config: Config) -> (YoutuiWindow, Effects<YoutuiWindow>) {
        let (playlist, task) = Playlist::new(Percentage(config.volume));
        let this = YoutuiWindow {
            context: WindowContext::Browser,
            playlist,
            config,
            browser: Browser::new(),
            key_stack: Vec::new(),
            help: HelpMenu::new(),
            tick: 0,
            footer_cache: FooterCache::new(),
        };
        (
            this,
            task.map(|this: &mut Self| &mut this.playlist),
        )
    }
    pub fn get_help_list_items(&self) -> impl Iterator<Item = DisplayableKeyAction<'_>> {
        let base: Vec<DisplayableKeyAction<'_>> = match self.context {
            WindowContext::Browser => {
                flatten_keybinds_as_readable(self.browser.get_all_keybinds(&self.config))
            }
            WindowContext::Playlist => {
                flatten_keybinds_as_readable(self.playlist.get_all_keybinds(&self.config))
            }
        };
        base.into_iter().chain(flatten_keybinds_as_readable(
            std::iter::once(&self.config.keybinds.global)
                .chain(std::iter::once(&self.config.keybinds.list))
                .chain(std::iter::once(&self.config.keybinds.text_entry)),
        ))
    }
    pub async fn handle_crossterm_event(
        &mut self,
        event: crossterm::event::Event,
    ) -> YoutuiEffect<Self> {
        // TODO: This should be intercepted and keycodes mapped by us instead of going
        // direct to rat-text.
        if let Some(effect) = self.try_handle_text(&event) {
            return effect.into();
        };
        // Splitting out event types removes one layer of indentation.
        match event {
            Event::Key(k) => return self.handle_key_event(k),
            Event::Mouse(m) => return self.handle_mouse_event(m).into(),
            Event::Resize(..) => tracing::debug!("Received Resize event"),
            other => tracing::warn!("Received unimplemented {:?} event", other),
        }
        Effects::none().into()
    }
    pub async fn handle_media_controls_event(
        &mut self,
        event: souvlaki::MediaControlEvent,
    ) -> YoutuiEffect<Self> {
        match event {
            souvlaki::MediaControlEvent::Play => return self.resume().into(),
            souvlaki::MediaControlEvent::Pause => return self.pause().into(),
            souvlaki::MediaControlEvent::Toggle => return self.pauseplay().into(),
            souvlaki::MediaControlEvent::Next => return self.handle_next().into(),
            souvlaki::MediaControlEvent::Previous => return self.handle_prev().into(),
            souvlaki::MediaControlEvent::Stop => return self.stop().into(),
            souvlaki::MediaControlEvent::SetVolume(v) => {
                return self.handle_set_volume((v * 100.0) as u8).into();
            }
            souvlaki::MediaControlEvent::Quit => {
                return (Effects::none(), Some(AppCallback::Quit)).into();
            }
            _ => {
                tracing::debug!("Received intentionally unhandled event {:?}", event)
            }
        }
        Effects::none().into()
    }
    pub async fn handle_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        self.playlist.handle_tick().await;
    }
    fn handle_key_event(&mut self, key_event: crossterm::event::KeyEvent) -> YoutuiEffect<Self> {
        self.key_stack.push(key_event);
        self.global_handle_key_stack()
    }
    fn handle_mouse_event(
        &mut self,
        mouse_event: crossterm::event::MouseEvent,
    ) -> Effects<Self> {
        tracing::debug!("Received unimplemented {:?} mouse event", mouse_event);
        Effects::none()
    }
    pub fn handle_list_action(&mut self, action: ListAction) -> Effects<Self> {
        if self.is_scrollable() {
            match action {
                ListAction::Up => self.increment_list(-1),
                ListAction::Down => self.increment_list(1),
                ListAction::PageUp => self.increment_list(-PAGE_KEY_LINES),
                ListAction::PageDown => self.increment_list(PAGE_KEY_LINES),
                ListAction::First => {
                    if self.help.shown {
                        self.help.cur = 0;
                    } else {
                        match self.context {
                            WindowContext::Browser => self.browser.go_to_first(),
                            WindowContext::Playlist => self.playlist.go_to_first(),
                        }
                    }
                }
                ListAction::Last => {
                    if self.help.shown {
                        self.help.cur = self.help.len.saturating_sub(1);
                    } else {
                        match self.context {
                            WindowContext::Browser => self.browser.go_to_last(),
                            WindowContext::Playlist => self.playlist.go_to_last(),
                        }
                    }
                }
            }
        }
        Effects::none()
    }
    pub fn handle_text_entry_action(&mut self, action: TextEntryAction) -> Effects<Self> {
        if !self.is_text_handling() {
            return Effects::none();
        }
        match self.context {
            WindowContext::Browser => self
                .browser
                .handle_text_entry_action(action)
                .map(|this: &mut Self| &mut this.browser),
            WindowContext::Playlist => Effects::none(),
        }
    }
    pub fn pauseplay(&mut self) -> Effects<Self> {
        self.playlist
            .pauseplay()
            .map(|this: &mut Self| &mut this.playlist)
    }
    pub fn resume(&mut self) -> Effects<Self> {
        self.playlist
            .resume()
            .map(|this: &mut Self| &mut this.playlist)
    }
    pub fn pause(&mut self) -> Effects<Self> {
        self.playlist
            .pause()
            .map(|this: &mut Self| &mut this.playlist)
    }
    pub fn stop(&mut self) -> Effects<Self> {
        self.playlist
            .stop()
            .map(|this: &mut Self| &mut this.playlist)
    }
    pub fn handle_next(&mut self) -> Effects<Self> {
        self.playlist
            .handle_next()
            .map(|this: &mut Self| &mut this.playlist)
    }
    pub fn handle_prev(&mut self) -> Effects<Self> {
        self.playlist
            .handle_previous()
            .map(|this: &mut Self| &mut this.playlist)
    }
    pub fn handle_increase_volume(&mut self, inc: i8) -> Effects<Self> {
        self.increase_volume(inc);
        Effects::new(move |server: &ArcServer| {
            let server = Arc::clone(server);
            async move {
                let update = server.player.increase_volume(inc).await;
                Box::new(move |this: &mut YoutuiWindow| {
                    if let Some(update) = update {
                        this.playlist.handle_volume_update(update);
                    }
                    Effects::none()
                }) as Box<dyn FnOnce(&mut YoutuiWindow) -> Effects<YoutuiWindow> + Send>
            }
        })
    }
    pub fn handle_set_volume(&mut self, new_vol: u8) -> Effects<Self> {
        self.set_volume(new_vol);
        Effects::new(move |server: &ArcServer| {
            let server = Arc::clone(server);
            async move {
                let update = server.player.set_volume(new_vol).await;
                Box::new(move |this: &mut YoutuiWindow| {
                    if let Some(update) = update {
                        this.playlist.handle_volume_update(update);
                    }
                    Effects::none()
                }) as Box<dyn FnOnce(&mut YoutuiWindow) -> Effects<YoutuiWindow> + Send>
            }
        })
    }
    pub fn finish_handle_add_songs_to_playlist_and_play(
        &mut self,
        song_list: Vec<ListSong>,
    ) -> Effects<Playlist> {
        let e = self.playlist.reset();
        let (id, next_effect) = self.playlist.push_song_list(song_list);
        e.push(next_effect).push(self.playlist.play_song(id))
    }
    pub fn handle_add_songs_to_playlist(
        &mut self,
        song_list: Vec<ListSong>,
    ) -> Effects<Self> {
        let (_, effect) = self.playlist.push_song_list(song_list);
        effect.map(|this: &mut Self| &mut this.playlist)
    }
    pub fn handle_add_songs_to_playlist_and_play(
        &mut self,
        song_list: Vec<ListSong>,
    ) -> Effects<Self> {
        let inner = self.finish_handle_add_songs_to_playlist_and_play(song_list);
        inner.map(|this: &mut Self| &mut this.playlist)
    }
    fn global_handle_key_stack(&mut self) -> YoutuiEffect<Self> {
        match handle_key_stack(self.get_active_keybinds(&self.config), &self.key_stack) {
            KeyHandleAction::Action(a) => {
                let effect = self.apply_action(a).into();
                self.key_stack.clear();
                effect
            }
            KeyHandleAction::Mode { .. } => Effects::none().into(),
            KeyHandleAction::NoMap => {
                self.key_stack.clear();
                Effects::none().into()
            }
        }
    }
    fn key_pending(&self) -> bool {
        !self.key_stack.is_empty()
    }
    pub fn toggle_help(&mut self) {
        if self.help.shown {
            self.help.shown = false;
        } else {
            self.help.shown = true;
            // Setup Help menu parameters
            self.help.cur = 0;
            // We have to get the keybind length this way as the help menu iterator is not
            // ExactSized
            self.help.len = self.get_help_list_items().count();
        }
    }
    /// Visually increment the volume, note, does not actually change the
    /// volume.
    fn increase_volume(&mut self, inc: i8) {
        self.playlist.increase_volume(inc);
    }
    /// Visually set the volume, note, does not actually change the volume.
    fn set_volume(&mut self, new_vol: u8) {
        self.playlist.set_volume(new_vol);
    }
    pub fn handle_change_context(&mut self, new_context: WindowContext) {
        if matches!(new_context, WindowContext::Browser) {
            self.browser.close_search_all();
        }
        self.context = new_context;
    }
    // The downside of this approach is that if draw_popup is calling this function,
    // it is gettign called every tick.
    // Consider a way to set this in the in state memory.
    fn get_cur_displayable_mode(
        &self,
    ) -> Option<DisplayableMode<'_, impl Iterator<Item = DisplayableKeyAction<'_>>>> {
        let KeyHandleAction::Mode { name, keys } =
            handle_key_stack(self.get_active_keybinds(&self.config), &self.key_stack)
        else {
            return None;
        };
        let displayable_commands = keys
            .iter()
            .map(|(kb, kt)| DisplayableKeyAction::from_keybind_and_action_tree(kb, kt));
        Some(DisplayableMode {
            displayable_commands,
            description: name.into(),
        })
    }
}


