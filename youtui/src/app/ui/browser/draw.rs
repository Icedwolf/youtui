use super::Browser;
use super::artistsearch::ArtistSearchBrowser;
use super::search_panel::SearchPanelInputRouting;
use super::shared_components::SearchBlock;
use super::shared_components::SearchBrowserSide;
use super::songs_panel::SongsInputRouting;
use super::songsearch::SongSearchBrowser;
use crate::app::component::actionhandler::Suggestable;
use crate::app::ui::browser::playlistsearch::PlaylistSearchBrowser;
use crate::app::view::draw::{draw_advanced_table, draw_list, draw_loadable, draw_panel_mut};
use crate::drawutils::{
    ROW_HIGHLIGHT_COLOUR, SELECTED_BORDER_COLOUR, TEXT_COLOUR, below_left_rect, bottom_of_rect,
    draw_text_box,
};
use ratatui::Frame;
use ratatui::prelude::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
use ytmapi_rs::common::{SuggestionType, TextRun};

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
                draw_list(f, t, chunk, cur_tick);
                None
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
                draw_list(f, t, chunk, cur_tick);
                None
            },
        );
        draw_search_box(
            f,
            "Search Artists",
            &mut browser.search_panel.search,
            search_box_chunk,
        );
        // Should this be part of draw_search_box
        if browser.search_panel.has_search_suggestions() {
            draw_search_suggestions(
                f,
                &browser.search_panel.search,
                search_box_chunk,
                artists_chunk,
            )
        }
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
        // Should this be part of draw_search_box
        if browser.search_panel.has_search_suggestions() {
            draw_search_suggestions(
                f,
                &browser.search_panel.search,
                search_box_chunk,
                playlists_chunk,
            )
        }
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
        // Should this be part of draw_search_box
        if browser.has_search_suggestions() {
            draw_search_suggestions(f, &browser.search, search_box_chunk, chunk)
        }
    }
}

fn draw_search_box(f: &mut Frame, title: impl AsRef<str>, search: &mut SearchBlock, chunk: Rect) {
    draw_text_box(f, title, &mut search.search_contents, chunk);
}

fn draw_search_suggestions(f: &mut Frame, search: &SearchBlock, chunk: Rect, max_bounds: Rect) {
    let suggestions = search.get_search_suggestions();
    let height = suggestions.len() + 1;
    let divider_chunk = bottom_of_rect(chunk);
    let suggestion_chunk = below_left_rect(
        height.try_into().unwrap_or(u16::MAX),
        chunk.width,
        chunk,
        max_bounds,
    );
    let [suggestion_side_borders_chunk, suggestion_list_chunk] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .areas(suggestion_chunk);
    let mut list_state = ListState::default().with_selected(search.suggestions_cur);
    // Cap max text width to avoid visual overflow.
    let max_suggestion_width = suggestion_list_chunk.width.saturating_sub(2) as usize;
    let list_items = suggestions.iter().map(|s| {
        let icon = match s.suggestion_type {
            SuggestionType::History => Span::raw(" "),
            SuggestionType::Prediction => Span::raw(" "),
        };
        let icon_width = match s.suggestion_type {
            SuggestionType::History => 3,
            SuggestionType::Prediction => 3,
        };
        let avail = max_suggestion_width.saturating_sub(icon_width);
        let mut remaining = avail;
        let mut spans: Vec<Span> = Vec::new();
        spans.push(icon);
        for run in &s.runs {
            if remaining == 0 {
                break;
            }
            let text = match run {
                TextRun::Bold(s) | TextRun::Normal(s) => s,
            };
            if text.len() <= remaining {
                spans.push(match run {
                    TextRun::Bold(str) => {
                        Span::styled(str.clone(), Style::new().add_modifier(Modifier::BOLD))
                    }
                    TextRun::Normal(str) => Span::raw(str.clone()),
                });
                remaining = remaining.saturating_sub(text.len());
            } else {
                let truncated: String = text.chars().take(remaining.saturating_sub(1)).collect();
                let mut t = truncated;
                t.push('…');
                spans.push(match run {
                    TextRun::Bold(_) => Span::styled(t, Style::new().add_modifier(Modifier::BOLD)),
                    TextRun::Normal(_) => Span::raw(t),
                });
                remaining = 0;
            }
        }
        ListItem::new(Line::from_iter(spans))
    });
    let block = List::new(list_items)
        .style(Style::new().fg(TEXT_COLOUR))
        .highlight_style(Style::new().bg(ROW_HIGHLIGHT_COLOUR))
        .block(
            Block::default()
                .borders(Borders::all().difference(Borders::TOP))
                .style(Style::new().fg(SELECTED_BORDER_COLOUR)),
        );
    let side_borders = Block::default()
        .borders(Borders::LEFT.union(Borders::RIGHT))
        .style(Style::new().fg(SELECTED_BORDER_COLOUR));
    let divider = Block::default().borders(Borders::TOP);
    f.render_widget(Clear, suggestion_chunk);
    f.render_widget(side_borders, suggestion_side_borders_chunk);
    f.render_widget(Clear, divider_chunk);
    f.render_widget(divider, divider_chunk);
    f.render_stateful_widget(block, suggestion_list_chunk, &mut list_state);
}
