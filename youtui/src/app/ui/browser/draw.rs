use super::Browser;
use super::artistsearch::ArtistSearchBrowser;
use super::search_panel::SearchPanelInputRouting;
use super::shared_components::SearchBlock;
use super::shared_components::SearchBrowserSide;
use super::songs_panel::SongsInputRouting;
use super::songsearch::SongSearchBrowser;
use crate::app::ui::browser::playlistsearch::PlaylistSearchBrowser;
use crate::app::view::draw::{draw_advanced_table, draw_list, draw_loadable, draw_panel_mut};
use crate::drawutils::draw_text_box;
use ratatui::Frame;
use ratatui::prelude::{Constraint, Direction, Layout, Rect};

pub fn draw_browser(
    f: &mut Frame,
    browser: &mut Browser,
    chunk: Rect,
    selected: bool,
    cur_tick: u64,
) {
    match browser.variant {
        super::BrowserVariant::Artist => draw_artist_search_browser(
            f,
            &mut browser.artist_search_browser,
            chunk,
            selected,
            cur_tick,
        ),
        super::BrowserVariant::Song => draw_song_search_browser(
            f,
            &mut browser.song_search_browser,
            chunk,
            selected,
            cur_tick,
        ),
        super::BrowserVariant::Playlist => draw_playlist_search_browser(
            f,
            &mut browser.playlist_search_browser,
            chunk,
            selected,
            cur_tick,
        ),
    }
}
pub fn draw_artist_search_browser(
    f: &mut Frame,
    browser: &mut ArtistSearchBrowser,
    chunk: Rect,
    selected: bool,
    cur_tick: u64,
) {
    let [artists_chunk, songs_chunk] = Layout::new(
        ratatui::prelude::Direction::Horizontal,
        [Constraint::Max(30), Constraint::Min(0)],
    )
    .areas(chunk);
    // Potentially could handle this better.
    let albumsongsselected = selected
        && browser.side == SearchBrowserSide::Songs
        && browser.songs_panel.route == SongsInputRouting::List;
    let artistselected = !albumsongsselected
        && selected
        && browser.side == SearchBrowserSide::Search
        && browser.search_panel.route == SearchPanelInputRouting::List;

    if !browser.search_panel.search_popped {
        draw_panel_mut(
            f,
            &mut browser.search_panel,
            artists_chunk,
            artistselected,
            |t, f, chunk| {
                draw_loadable(f, t, chunk, cur_tick, |t, f, chunk| {
                    draw_list(f, t, chunk, cur_tick);
                    None
                })
            },
        );
    } else {
        let [search_box_chunk, shrunk_artists_chunk] = Layout::default()
            .direction(Direction::Vertical)
            .margin(0)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .areas(artists_chunk);
        draw_panel_mut(
            f,
            &mut browser.search_panel,
            shrunk_artists_chunk,
            artistselected,
            |t, f, chunk| {
                draw_loadable(f, t, chunk, cur_tick, |t, f, chunk| {
                    draw_list(f, t, chunk, cur_tick);
                    None
                })
            },
        );
        draw_search_box(
            f,
            "Search Artists",
            &mut browser.search_panel.search,
            search_box_chunk,
        );
    }
    draw_panel_mut(
        f,
        &mut browser.songs_panel,
        songs_chunk,
        albumsongsselected,
        |t, f, chunk| {
            draw_loadable(f, t, chunk, cur_tick, |t, f, chunk| {
                Some(draw_advanced_table(f, t, chunk, cur_tick))
            })
        },
    );
}
pub fn draw_playlist_search_browser(
    f: &mut Frame,
    browser: &mut PlaylistSearchBrowser,
    chunk: Rect,
    selected: bool,
    cur_tick: u64,
) {
    let [playlists_chunk, songs_chunk] = Layout::new(
        ratatui::prelude::Direction::Horizontal,
        [Constraint::Percentage(30), Constraint::Percentage(70)],
    )
    .areas(chunk);
    // Potentially could handle this better.
    let songs_selected = selected
        && browser.side == SearchBrowserSide::Songs
        && browser.songs_panel.route == SongsInputRouting::List;
    let playlists_selected = !songs_selected
        && selected
        && browser.side == SearchBrowserSide::Search
        && browser.search_panel.route == SearchPanelInputRouting::List;

    if !browser.search_panel.search_popped {
        draw_panel_mut(
            f,
            &mut browser.search_panel,
            playlists_chunk,
            playlists_selected,
            |t, f, chunk| {
                draw_list(f, t, chunk, cur_tick);
                None
            },
        );
    } else {
        let [search_box_chunk, shrunk_playlists_chunk] = Layout::default()
            .direction(Direction::Vertical)
            .margin(0)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .areas(playlists_chunk);
        draw_panel_mut(
            f,
            &mut browser.search_panel,
            shrunk_playlists_chunk,
            playlists_selected,
            |t, f, chunk| {
                draw_list(f, t, chunk, cur_tick);
                None
            },
        );
        draw_search_box(
            f,
            "Search Playlists",
            &mut browser.search_panel.search,
            search_box_chunk,
        );
    }
    draw_panel_mut(
        f,
        &mut browser.songs_panel,
        songs_chunk,
        songs_selected,
        |t, f, chunk| {
            draw_loadable(f, t, chunk, cur_tick, |t, f, chunk| {
                Some(draw_advanced_table(f, t, chunk, cur_tick))
            })
        },
    );
}
pub fn draw_song_search_browser(
    f: &mut Frame,
    browser: &mut SongSearchBrowser,
    chunk: Rect,
    selected: bool,
    cur_tick: u64,
) {
    if !browser.search_popped {
        draw_panel_mut(f, browser, chunk, selected, |t, f, chunk| {
            draw_loadable(f, t, chunk, cur_tick, |t, f, chunk| {
                Some(draw_advanced_table(f, t, chunk, cur_tick))
            })
        });
    } else {
        let [search_box_chunk, new_chunk] = Layout::default()
            .direction(Direction::Vertical)
            .margin(0)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .areas(chunk);
        draw_panel_mut(f, browser, new_chunk, false, |t, f, chunk| {
            draw_loadable(f, t, chunk, cur_tick, |t, f, chunk| {
                Some(draw_advanced_table(f, t, chunk, cur_tick))
            })
        });
        draw_search_box(f, "Search Songs", &mut browser.search, search_box_chunk);
    }
}

fn draw_search_box(f: &mut Frame, title: impl AsRef<str>, search: &mut SearchBlock, chunk: Rect) {
    draw_text_box(f, title, &mut search.search_contents, chunk);
}
