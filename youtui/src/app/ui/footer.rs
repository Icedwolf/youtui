use crate::app::structures::{ListSongID, PlayState};
use crate::drawutils::{
    BUTTON_BG_COLOUR, BUTTON_FG_COLOUR, PROGRESS_BG_COLOUR, PROGRESS_FG_COLOUR,
    resolve_display_duration,
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
    pub last_duration: usize,
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
            last_duration: usize::MAX,
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

/// Refresh the footer's cached strings with the least allocation possible.
///
/// `song_meta` (pre-formatted "icon title - artist" and album strings) is only
/// supplied on a song-id transition into a known song; `None` is passed every
/// other frame so the strings are rebuilt only when the song changes.
///
/// The duration string is keyed on its own resolved value, not the song id: the
/// decoder-reported duration only becomes known when playback actually starts
/// (`handle_playing`), i.e. *after* the song id already transitioned during
/// buffering — keying on the id alone left `duration_str` stuck at `00:00` for
/// the whole song.
fn refresh_footer_cache(
    cache: &mut FooterCache,
    cur_active_id: Option<ListSongID>,
    song_meta: Option<(String, String)>,
    resolved_duration: usize,
    progress_secs: usize,
) {
    if cache.last_song_id != cur_active_id {
        cache.last_song_id = cur_active_id;
        match song_meta {
            Some((song_and_artists, album_title)) => {
                cache.song_and_artists = song_and_artists;
                cache.album_title = album_title;
                cache.last_duration = usize::MAX;
            }
            None => {
                cache.song_and_artists.clear();
                cache.album_title.clear();
                cache.duration_str.clear();
                cache.last_duration = resolved_duration;
            }
        }
        cache.progress_str.clear();
        cache.last_progress_secs = usize::MAX;
    }
    if cache.last_duration != resolved_duration {
        cache.last_duration = resolved_duration;
        cache.duration_str = secs_to_time_string(resolved_duration);
    }
    if cache.last_progress_secs != progress_secs {
        cache.last_progress_secs = progress_secs;
        cache.progress_str = secs_to_time_string(progress_secs);
    }
}

pub fn draw_footer(f: &mut Frame, w: &mut super::YoutuiWindow, chunk: Rect) {
    let cur_active_id = match w.playlist.play_status {
        PlayState::Error(id)
        | PlayState::Playing(id)
        | PlayState::Paused(id)
        | PlayState::Buffering(id) => Some(id),
        PlayState::NotPlaying  => None,
    };

    let mut duration = 0;
    let mut progress = Duration::default();
    if let Some(id) = cur_active_id
        && let Some(song) = w.playlist.get_song_from_id(id)
    {
        duration = resolve_display_duration(song.actual_duration, song.duration_secs);
        if matches!(
            w.playlist.play_status,
            PlayState::Playing(_) | PlayState::Paused(_)
        ) {
            progress = w.playlist.get_cur_played_dur().unwrap_or_default();
        }
    }
    let play_ratio = if duration == 0 {
        0.0
    } else {
        (progress.as_secs_f64() / duration as f64).clamp(0.0, 1.0)
    };

    let progress_secs = progress.as_secs() as usize;
    let song_meta = if w.footer_cache.last_song_id != cur_active_id {
        cur_active_id
            .and_then(|id| w.playlist.get_song_from_id(id))
            .map(|song| {
                (
                    format!(
                        "{} {} - {}",
                        w.playlist.status_bar_icon(),
                        song.title,
                        song.artists_string,
                    ),
                    song.album
                        .as_ref()
                        .map(|a| a.name.clone())
                        .unwrap_or_default(),
                )
            })
    } else {
        None
    };
    refresh_footer_cache(
        &mut w.footer_cache,
        cur_active_id,
        song_meta,
        duration,
        progress_secs,
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate the Buffering frame for a new song: the id arrives but the
    /// resolved duration is still zero (playback has not started and the
    /// decoder-reported `actual_duration` is not known yet).
    fn buffering_frame(cache: &mut FooterCache, id: ListSongID) {
        refresh_footer_cache(
            cache,
            Some(id),
            Some(("S - A".to_string(), "Al".to_string())),
            0,
            0,
        );
    }

    #[test]
    fn duration_string_refreshes_when_playing_starts_same_song_id() {
        let id = ListSongID(7);
        let mut cache = FooterCache::new();
        buffering_frame(&mut cache, id);
        assert_eq!(cache.duration_str, "00:00");

        // Playback starts: same song id, duration now known. The song id has
        // NOT changed, so an id-keyed cache would never refresh this.
        refresh_footer_cache(&mut cache, Some(id), None, 185, 0);
        assert_eq!(cache.duration_str, "03:05");

        // Subsequent frames keep the cached string (no churn).
        refresh_footer_cache(&mut cache, Some(id), None, 185, 5);
        assert_eq!(cache.duration_str, "03:05");
    }

    #[test]
    fn zero_duration_renders_when_unknown_and_recovers() {
        let id = ListSongID(7);
        let mut cache = FooterCache::new();
        buffering_frame(&mut cache, id);
        refresh_footer_cache(&mut cache, Some(id), None, 185, 0);
        assert_eq!(cache.duration_str, "03:05");
        // A genuinely-resolved zero (song with no duration metadata) renders
        // 00:00 honestly, and recovers when the duration becomes known again.
        refresh_footer_cache(&mut cache, Some(id), None, 0, 0);
        assert_eq!(cache.duration_str, "00:00");
        refresh_footer_cache(&mut cache, Some(id), None, 185, 5);
        assert_eq!(cache.duration_str, "03:05");
    }

    #[test]
    fn duration_string_switches_between_songs() {
        let id_a = ListSongID(7);
        let id_b = ListSongID(8);
        let mut cache = FooterCache::new();
        buffering_frame(&mut cache, id_a);
        refresh_footer_cache(&mut cache, Some(id_a), None, 185, 0);
        assert_eq!(cache.duration_str, "03:05");

        buffering_frame(&mut cache, id_b);
        assert_eq!(cache.duration_str, "00:00");
        refresh_footer_cache(&mut cache, Some(id_b), None, 90, 0);
        assert_eq!(cache.duration_str, "01:30");
    }

    #[test]
    fn stop_clears_duration_string() {
        let id = ListSongID(7);
        let mut cache = FooterCache::new();
        buffering_frame(&mut cache, id);
        refresh_footer_cache(&mut cache, Some(id), None, 185, 5);
        assert_eq!(cache.duration_str, "03:05");

        refresh_footer_cache(&mut cache, None, None, 0, 0);
        assert_eq!(cache.duration_str, "");
    }

    #[test]
    fn progress_string_tracks_progress_secs() {
        let id = ListSongID(7);
        let mut cache = FooterCache::new();
        buffering_frame(&mut cache, id);
        refresh_footer_cache(&mut cache, Some(id), None, 185, 65);
        assert_eq!(cache.progress_str, "01:05");
        // Same secs: no churn.
        refresh_footer_cache(&mut cache, Some(id), None, 185, 65);
        assert_eq!(cache.progress_str, "01:05");
        // Next sec: update.
        refresh_footer_cache(&mut cache, Some(id), None, 185, 66);
        assert_eq!(cache.progress_str, "01:06");
    }
}
