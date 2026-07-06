use crate::app::structures::PlayState;
use crate::drawutils::{
    BUTTON_BG_COLOUR, BUTTON_FG_COLOUR, PROGRESS_BG_COLOUR, PROGRESS_FG_COLOUR,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use std::time::Duration;

pub fn secs_to_time_string(secs: usize) -> String {
    // Naive implementation
    let hours = secs / 3600;
    let rem_mins = (secs - (hours * 3600)) / 60;
    let rem_secs = secs - (hours * 3600 + rem_mins * 60);
    if hours > 0 {
        format!("{hours}:{rem_mins:02}:{rem_secs:02}")
    } else {
        format!("{rem_mins:02}:{rem_secs:02}")
    }
}

pub fn draw_footer(
    f: &mut Frame,
    w: &mut super::YoutuiWindow,
    chunk: Rect,
) {
    let mut duration = 0;
    let mut progress = Duration::default();
    let play_ratio = match &w.playlist.play_status {
        PlayState::Playing(id) | PlayState::Paused(id) => {
            duration = w
                .playlist
                .get_song_from_id(*id)
                .map(|s| {
                    s.actual_duration
                        .map(|d| d.as_secs() as usize)
                        .filter(|&secs| {
                            // Streaming WAV decoder may report bogus duration
                            // (sentinel from unknown chunk size). Fall back to
                            // API metadata if decoder report is unreasonable.
                            secs < 7200 || secs <= s.duration_secs * 2
                        })
                        .unwrap_or(s.duration_secs)
                })
                .unwrap_or(0);
            progress = w.playlist.get_cur_played_dur().unwrap_or_default();
            (progress.as_secs_f64() / duration as f64).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };
    let progress_str = secs_to_time_string(progress.as_secs() as usize);
    let duration_str = secs_to_time_string(duration);
    let bar_str = format!("{progress_str}/{duration_str}");

    let cur_active_song = match w.playlist.play_status {
        PlayState::Error(id)
        | PlayState::Playing(id)
        | PlayState::Paused(id)
        | PlayState::Buffering(id) => w.playlist.get_song_from_id(id),
        PlayState::NotPlaying | PlayState::Stopped => None,
    };
    let block = Block::default()
        .title("Status")
        .title(Line::from("Youtui").right_aligned())
        .borders(Borders::ALL);
    let block_inner = block.inner(chunk);
    let [progress_bar_section, vol_bar_chunk] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(6)])
        .areas(block_inner);
    let [song_text_chunk, progress_bar_chunk] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(2), Constraint::Max(1)])
        .areas(progress_bar_section);

    let song_and_artists_string = cur_active_song
        .map(|song| {
            let mut s = format!(
                "{} {} - ",
                w.playlist.status_bar_icon(),
                song.title,
            );
            for (i, artist) in song.artists.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&artist.name);
            }
            s
        })
        .unwrap_or_default();
    let album_title = cur_active_song
        .and_then(|s| s.album.as_ref())
        .map(|s| s.name.as_str())
        .unwrap_or_default();
    // Truncate text to available width to avoid visual overflow.
    let max_text_width = song_text_chunk.width.saturating_sub(2) as usize;
    let truncate = |s: &str| -> String {
        if s.len() <= max_text_width {
            s.to_string()
        } else {
            let mut t: String = s.chars().take(max_text_width.saturating_sub(1)).collect();
            t.push('…');
            t
        }
    };
    let song_line = truncate(&song_and_artists_string);
    let album_line = truncate(album_title);
    let footer = Paragraph::new(vec![
        Line::from(song_line),
        Line::from(album_line),
    ]);
    let bar = Gauge::default()
        .label(bar_str)
        .gauge_style(
            Style::default()
                .fg(PROGRESS_FG_COLOUR)
                .bg(PROGRESS_BG_COLOUR),
        )
        .ratio(play_ratio);
    let left_arrow = Paragraph::new(Line::from(vec![
        Span::styled(
            "< [",
            Style::new()
                .fg(BUTTON_FG_COLOUR)
                .bg(BUTTON_BG_COLOUR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]));
    let right_arrow = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "] >",
            Style::new()
                .fg(BUTTON_FG_COLOUR)
                .bg(BUTTON_BG_COLOUR)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    let vol = w.playlist.volume().0;
    let vol_bar_spans = vec![
        Line::from(Span::styled(
            " + ",
            Style::new()
                .fg(BUTTON_FG_COLOUR)
                .bg(BUTTON_BG_COLOUR)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw(format!("{vol:>3}"))),
        Line::from(Span::styled(
            " - ",
            Style::new()
                .fg(BUTTON_FG_COLOUR)
                .bg(BUTTON_BG_COLOUR)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    let vol_bar = Paragraph::new(vol_bar_spans).alignment(Alignment::Right);
    let [left_arrow_chunk, progress_bar_chunk, right_arrow_chunk] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Max(4), Constraint::Min(1), Constraint::Max(4)])
        .areas(progress_bar_chunk);
    f.render_widget(bar, progress_bar_chunk);
    f.render_widget(left_arrow, left_arrow_chunk);
    f.render_widget(right_arrow, right_arrow_chunk);
    f.render_widget(block, chunk);
    f.render_widget(footer, song_text_chunk);
    f.render_widget(vol_bar, vol_bar_chunk);
}
