use super::Browser;
use super::artistsearch::ArtistSearchBrowser;
use super::search_panel::{SearchPanel, SearchPanelConfig, SearchPanelInputRouting};
use super::shared_components::{SearchBlock, SearchBrowserSide, SortFilterTable};
use super::songs_panel::SongsInputRouting;
use super::songsearch::SongSearchBrowser;
use crate::app::ui::browser::playlistsearch::PlaylistSearchBrowser;
use crate::app::view::draw::{draw_advanced_table, draw_list, draw_loadable, draw_panel_mut};
use crate::app::view::{HasTitle, Loadable};
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
    // Potentially could handle this better.
    let albumsongsselected = selected
        && browser.side == SearchBrowserSide::Songs
        && browser.songs_panel.route == SongsInputRouting::List;
    let artistselected = !albumsongsselected
        && selected
        && browser.side == SearchBrowserSide::Search
        && browser.search_panel.route == SearchPanelInputRouting::List;
    draw_split_search_browser_core(
        f,
        chunk,
        cur_tick,
        [Constraint::Max(30), Constraint::Min(0)],
        "Search Artists",
        true,
        artistselected,
        albumsongsselected,
        &mut browser.search_panel,
        &mut browser.songs_panel,
    );
}

pub fn draw_playlist_search_browser(
    f: &mut Frame,
    browser: &mut PlaylistSearchBrowser,
    chunk: Rect,
    selected: bool,
    cur_tick: u64,
) {
    let songs_selected = selected
        && browser.side == SearchBrowserSide::Songs
        && browser.songs_panel.route == SongsInputRouting::List;
    let playlists_selected = !songs_selected
        && selected
        && browser.side == SearchBrowserSide::Search
        && browser.search_panel.route == SearchPanelInputRouting::List;
    draw_split_search_browser_core(
        f,
        chunk,
        cur_tick,
        [Constraint::Percentage(30), Constraint::Percentage(70)],
        "Search Playlists",
        // The playlist left panel historically draws without the
        // `draw_loadable` loading overlay; preserved exactly.
        false,
        playlists_selected,
        songs_selected,
        &mut browser.search_panel,
        &mut browser.songs_panel,
    );
}

/// Shared body of the artist/playlist split search browsers. The two differ
/// only in left-panel layout constraints, the search-box label, and whether
/// the left panel shows the `draw_loadable` loading overlay; the right songs
/// table is identical. Render-parity locked by `tests::*_render_parity`.
#[allow(clippy::too_many_arguments)]
fn draw_split_search_browser_core<C: SearchPanelConfig, S>(
    f: &mut Frame,
    chunk: Rect,
    cur_tick: u64,
    left_constraints: [Constraint; 2],
    search_title: &str,
    loadable_left: bool,
    left_selected: bool,
    songs_selected: bool,
    search_panel: &mut SearchPanel<C>,
    songs_panel: &mut S,
) where
    S: SortFilterTable + HasTitle + Loadable,
{
    let [left_chunk, songs_chunk] =
        Layout::new(Direction::Horizontal, left_constraints).areas(chunk);

    let draw_left_panel = |t: &mut SearchPanel<C>, f: &mut Frame, chunk: Rect| {
        if loadable_left {
            draw_loadable(f, t, chunk, cur_tick, |t, f, chunk| {
                draw_list(f, t, chunk, cur_tick);
                None
            })
        } else {
            draw_list(f, t, chunk, cur_tick);
            None
        }
    };

    if !search_panel.search_popped {
        draw_panel_mut(f, search_panel, left_chunk, left_selected, |t, f, chunk| {
            draw_left_panel(t, f, chunk)
        });
    } else {
        let [search_box_chunk, shrunk_left_chunk] = Layout::default()
            .direction(Direction::Vertical)
            .margin(0)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .areas(left_chunk);
        draw_panel_mut(
            f,
            search_panel,
            shrunk_left_chunk,
            left_selected,
            |t, f, chunk| draw_left_panel(t, f, chunk),
        );
        draw_search_box(f, search_title, &mut search_panel.search, search_box_chunk);
    }
    draw_panel_mut(
        f,
        songs_panel,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::structures::{ListSong, ListStatus};
    use crate::app::ui::browser::artistsearch::ArtistSearchBrowser;
    use crate::app::ui::browser::artistsearch::search_panel::ArtistSearchPanel;
    use crate::app::ui::browser::artistsearch::songs_panel::AlbumSongsPanel;
    use crate::app::ui::browser::playlistsearch::PlaylistSearchBrowser;
    use crate::app::ui::browser::playlistsearch::search_panel::PlaylistSearchPanel;
    use crate::app::ui::browser::playlistsearch::songs_panel::PlaylistSongsPanel;
    use crate::app::ui::browser::search_panel::SearchPanelInputRouting;
    use crate::app::ui::browser::shared_components::{SearchBrowserSide, SortFilterTable};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ytmapi_rs::common::{ArtistChannelID, PlaylistID, VideoID, YoutubeID};
    use ytmapi_rs::parse::SearchResultArtist;

    fn song(id: &str, title: &str, album: &str) -> ListSong {
        ListSong::create_with_metadata(
            VideoID::from_raw(id.to_owned()),
            title.to_owned(),
            vec!["AAA".into()],
            Some(album.to_owned()),
            "3:00".into(),
        )
    }

    fn artist_browser(popped: bool, loading: bool) -> ArtistSearchBrowser {
        let mut search_panel = ArtistSearchPanel::new();
        search_panel.list.push(SearchResultArtist::new(
            "AAA".to_string(),
            None,
            ArtistChannelID::from_raw("UCabc".to_string()),
        ));
        search_panel.status = if loading {
            ListStatus::Loading
        } else {
            ListStatus::Loaded
        };
        let mut songs_panel = AlbumSongsPanel::new();
        songs_panel.list.push_song_list(vec![
            song("a", "Song Q", "Album Q"),
            song("b", "Song R", "Album R"),
        ]);
        songs_panel.list.state = ListStatus::Loaded;
        songs_panel.rebuild_filtered_indices();
        let mut browser = ArtistSearchBrowser::new(search_panel, songs_panel);
        browser.side = SearchBrowserSide::Search;
        browser.search_panel.search_popped = popped;
        browser.search_panel.route = if popped {
            SearchPanelInputRouting::Search
        } else {
            SearchPanelInputRouting::List
        };
        browser
    }

    fn playlist_browser(popped: bool, loading: bool) -> PlaylistSearchBrowser {
        use crate::app::ui::browser::search_panel::NonPodcastSearchResultPlaylist;
        let mut search_panel = PlaylistSearchPanel::new();
        search_panel.list.push(NonPodcastSearchResultPlaylist {
            title: "AAA".to_string(),
            playlist_id: PlaylistID::from_raw("PLabc".to_string()),
        });
        search_panel.status = if loading {
            ListStatus::Loading
        } else {
            ListStatus::Loaded
        };
        let mut songs_panel = PlaylistSongsPanel::new();
        songs_panel.list.push_song_list(vec![
            song("a", "Song Q", "Album Q"),
            song("b", "Song R", "Album R"),
        ]);
        songs_panel.list.state = ListStatus::Loaded;
        songs_panel.rebuild_filtered_indices();
        let mut browser = PlaylistSearchBrowser::new(search_panel, songs_panel);
        browser.side = SearchBrowserSide::Search;
        browser.search_panel.search_popped = popped;
        browser.search_panel.route = if popped {
            SearchPanelInputRouting::Search
        } else {
            SearchPanelInputRouting::List
        };
        browser
    }

    /// Render a browser through its split draw fn and return the visible text
    /// layer (one line per terminal row, trailing whitespace trimmed).
    fn snapshot_from_terminal(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let (w, h) = (buffer.area.width, buffer.area.height);
        let mut rows = Vec::with_capacity(h as usize);
        for y in 0..h {
            let mut row = String::with_capacity(w as usize);
            for x in 0..w {
                row.push_str(buffer[(x, y)].symbol());
            }
            rows.push(row.trim_end().to_string());
        }
        rows.join("\n")
    }

    fn render_artist(browser: &mut ArtistSearchBrowser, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_artist_search_browser(f, browser, f.area(), true, 0))
            .unwrap();
        snapshot_from_terminal(&terminal)
    }

    fn render_playlist(browser: &mut PlaylistSearchBrowser, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_playlist_search_browser(f, browser, f.area(), true, 0))
            .unwrap();
        snapshot_from_terminal(&terminal)
    }

    /// Pre-refactor render of the artist/playlist split browsers (see the
    /// `_render_parity` tests): 6 snapshots in fixed order — A_LOADED,
    /// A_POPPED, A_LOADING, P_LOADED, P_POPPED, P_LOADING — each a
    /// "// NAME" comment line followed by the 20 terminal rows.
    const EXPECTED: &str = include_str!("draw_parity.txt");

    /// Split `EXPECTED` into the six named snapshots (by order).
    fn expected_snapshots() -> Vec<String> {
        EXPECTED
            .split("\n----\n")
            .map(|chunk| chunk.lines().skip(1).collect::<Vec<_>>().join("\n"))
            .collect()
    }

    #[test]
    fn artist_split_browser_render_parity() {
        let snaps = expected_snapshots();
        assert_eq!(
            render_artist(&mut artist_browser(false, false), 100, 20),
            snaps[0],
            "artist browser, search closed"
        );
        assert_eq!(
            render_artist(&mut artist_browser(true, false), 100, 20),
            snaps[1],
            "artist browser, search opened"
        );
        assert_eq!(
            render_artist(&mut artist_browser(false, true), 100, 20),
            snaps[2],
            "artist browser, left panel loading (draw_loadable overlay)"
        );
    }

    #[test]
    fn playlist_split_browser_render_parity() {
        let snaps = expected_snapshots();
        assert_eq!(
            render_playlist(&mut playlist_browser(false, false), 100, 20),
            snaps[3],
            "playlist browser, search closed"
        );
        assert_eq!(
            render_playlist(&mut playlist_browser(true, false), 100, 20),
            snaps[4],
            "playlist browser, search opened"
        );
        assert_eq!(
            render_playlist(&mut playlist_browser(false, true), 100, 20),
            snaps[5],
            "playlist browser, loading: left panel draws WITHOUT draw_loadable"
        );
    }
}
