use crate::app::structures::{ListSongID, PlayState};
use crate::drawutils::{
    BUTTON_BG_COLOUR, BUTTON_FG_COLOUR, PROGRESS_BG_COLOUR, PROGRESS_FG_COLOUR,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use std::borrow::Cow;
use std::time::Duration;

pub struct FooterCache {
    pub song_and_artists: String,
    pub album_title: String,
    pub progress_str: String,
    pub duration_str: String,
    pub last_song_id: Option<ListSongID>,
    pub last_progress_secs: usize,
    pub last_vol: u8,
}

impl FooterCache {
    pub fn new() -> Self {
        Self {
            song_and_artists: String::new(),
            album_title: String::new(),
            progress_str: String::new(),
            duration_str: String::new(),
            last_song_id: None,
            last_progress_secs: usize::MAX,
            last_vol: u8::MAX,
        }
    }
}

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

fn truncate(s: &str, max_len: usize) -> Cow<'_, str> {
    if s.len() <= max_len {
        Cow::Borrowed(s)
    } else {
        let mut t: String = s.chars().take(max_len.saturating_sub(1)).collect();
        t.push('…');
        Cow::Owned(t)
    }
}

pub fn draw_footer(f: &mut Frame, w: &mut super::YoutuiWindow, chunk: Rect) {
    let cur_active_id = match w.playlist.play_status {
        PlayState::Error(id)
        | PlayState::Playing(id)
        | PlayState::Paused(id)
        | PlayState::Buffering(id) => Some(id),
        PlayState::NotPlaying | PlayState::Stopped => None,
    };

    let mut duration = 0;
    let mut progress = Duration::default();
    let play_ratio = if let Some(id) = cur_active_id
        && matches!(
            w.playlist.play_status,
            PlayState::Playing(_) | PlayState::Paused(_)
        ) {
        let song = w.playlist.get_song_from_id(id);
        if let Some(song) = song {
            duration = song
                .actual_duration
                .map(|d| d.as_secs() as usize)
                .filter(|&secs| secs < 7200 || secs <= song.duration_secs * 2)
                .unwrap_or(song.duration_secs);
            progress = w.playlist.get_cur_played_dur().unwrap_or_default();
            (progress.as_secs_f64() / duration as f64).clamp(0.0, 1.0)
        } else {
            0.0
        }
    } else {
        0.0
    };

    let progress_secs = progress.as_secs() as usize;
    if w.footer_cache.last_song_id != cur_active_id {
        w.footer_cache.last_song_id = cur_active_id;
        if let Some(id) = cur_active_id {
            if let Some(song) = w.playlist.get_song_from_id(id) {
                w.footer_cache.song_and_artists = format!(
                    "{} {} - {}",
                    w.playlist.status_bar_icon(),
                    song.title,
                    song.artists_string,
                );
                w.footer_cache.album_title = song
                    .album
                    .as_ref()
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
            } else {
                w.footer_cache.song_and_artists.clear();
                w.footer_cache.album_title.clear();
            }
            w.footer_cache.duration_str = secs_to_time_string(duration);
        } else {
            w.footer_cache.song_and_artists.clear();
            w.footer_cache.album_title.clear();
            w.footer_cache.duration_str.clear();
        }
        w.footer_cache.progress_str.clear();
        w.footer_cache.last_progress_secs = usize::MAX;
    }
    if w.footer_cache.last_progress_secs != progress_secs {
        w.footer_cache.last_progress_secs = progress_secs;
        w.footer_cache.progress_str = secs_to_time_string(progress_secs);
    }
    let bar_str = format!(
        "{}/{}",
        w.footer_cache.progress_str, w.footer_cache.duration_str
    );

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

    // Truncate text to available width to avoid visual overflow.
    let max_text_width = song_text_chunk.width.saturating_sub(2) as usize;
    let song_line = truncate(&w.footer_cache.song_and_artists, max_text_width);
    let album_line = truncate(&w.footer_cache.album_title, max_text_width);
    let footer = Paragraph::new(vec![Line::from(song_line), Line::from(album_line)]);
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
    if w.footer_cache.last_vol != vol {
        w.footer_cache.last_vol = vol;
    }
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
