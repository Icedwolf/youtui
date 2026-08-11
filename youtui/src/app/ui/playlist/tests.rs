use super::{DownloadTask, Playlist, QueueState};
use crate::app::queue_persistence::{CompactSavedQueue, CompactSongRef};
use crate::app::structures::{
    DownloadStatus, ListSong, ListSongDisplayableField, ListSongID, ListStatus,
    Percentage, PlayState,
};
use pretty_assertions::assert_eq;
use std::sync::{Arc, OnceLock};
use ytmapi_rs::auth::BrowserToken;
use ytmapi_rs::common::{AlbumID, VideoID, YoutubeID};
use ytmapi_rs::parse::{GetAlbum, ParsedSongAlbum};
use ytmapi_rs::query::GetAlbumQuery;

static DUMMY_ALBUM: OnceLock<GetAlbum> = OnceLock::new();

fn get_dummy_album() -> GetAlbum {
    DUMMY_ALBUM
        .get_or_init(|| {
            let json = include_str!("../../../../../ytmapi-rs/test_json/get_album_20240724.json");
            ytmapi_rs::process_json::<_, BrowserToken>(
                json.to_owned(),
                GetAlbumQuery::new(AlbumID::from_raw("")),
            )
            .unwrap()
        })
        .clone()
}

fn get_dummy_playlist() -> Playlist {
    let (mut playlist, _effect) = Playlist::new(Percentage(50));
    playlist.list.state = ListStatus::Loaded;
    let GetAlbum {
        title,
        year,
        tracks,
        ..
    } = get_dummy_album();
    playlist.list.append_raw_album_songs(
        tracks,
        ParsedSongAlbum {
            name: title,
            id: AlbumID::from_raw(""),
        },
        year,
        vec![],
    );
    playlist
}

#[test]
fn downloaded_song_plays_if_buffered() {
    let mut p = get_dummy_playlist();
    p.play_status = PlayState::Buffering(ListSongID(1));
    p.list.get_list_iter_mut().nth(1).unwrap().download_status = DownloadStatus::Downloaded;
    let _effect = p.handle_song_downloaded(ListSongID(1));
    assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
}

#[test]
fn queued_song_plays_if_not_already_playing() {
    let mut p = get_dummy_playlist();
    p.play_status = PlayState::Buffering(ListSongID(0));
    p.queue_status = QueueState::Queued(ListSongID(0));
    p.list.get_list_iter_mut().next().unwrap().download_status = DownloadStatus::Downloaded;
    let _effect = p.handle_song_downloaded(ListSongID(0));
    assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    // handle_song_downloaded no longer calls autoplay_song_id;
    // PlaySong/AutoplaySong creation happens in handle_song_download_progress_update
    assert_eq!(p.queue_status, QueueState::Queued(ListSongID(0)));
}

#[test]
fn compact_song_ref_contains_all_fields() {
    let song_ref = CompactSongRef {
        video_id: VideoID::from_raw("test123"),
        title: "Test Song".to_string(),
        artists: vec!["Artist 1".to_string(), "Artist 2".to_string()],
        album: Some("Test Album".to_string()),
        duration_string: "3:45".to_string(),
    };

    assert_eq!(song_ref.video_id.get_raw(), "test123");
    assert_eq!(song_ref.title, "Test Song");
    assert_eq!(song_ref.artists.len(), 2);
    assert_eq!(song_ref.album, Some("Test Album".to_string()));
    assert_eq!(song_ref.duration_string, "3:45");
}

#[test]
fn compact_song_ref_serialization_roundtrip() {
    let song_ref = CompactSongRef {
        video_id: VideoID::from_raw("abc123"),
        title: "Roundtrip Test".to_string(),
        artists: vec!["Solo Artist".to_string()],
        album: None,
        duration_string: "4:20".to_string(),
    };

    let json = serde_json::to_string(&song_ref).unwrap();
    let parsed: CompactSongRef = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.video_id.get_raw(), song_ref.video_id.get_raw());
    assert_eq!(parsed.title, song_ref.title);
    assert_eq!(parsed.artists, song_ref.artists);
    assert_eq!(parsed.album, song_ref.album);
    assert_eq!(parsed.duration_string, song_ref.duration_string);
}

#[test]
fn compact_queue_with_current_index() {
    let songs = vec![
        CompactSongRef {
            video_id: VideoID::from_raw("song1"),
            title: "First Song".to_string(),
            artists: vec!["Artist".to_string()],
            album: Some("Album".to_string()),
            duration_string: "3:00".to_string(),
        },
        CompactSongRef {
            video_id: VideoID::from_raw("song2"),
            title: "Second Song".to_string(),
            artists: vec!["Artist".to_string()],
            album: Some("Album".to_string()),
            duration_string: "4:00".to_string(),
        },
    ];

    let queue = CompactSavedQueue {
        songs,
        current_index: Some(1),
        shuffle_enabled: false,
        shuffle_seed: 0,
    };

    let json = serde_json::to_string(&queue).unwrap();
    let parsed: CompactSavedQueue = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.songs.len(), 2);
    assert_eq!(parsed.current_index, Some(1));
    assert_eq!(parsed.songs[1].title, "Second Song");
}

#[test]
fn download_task_creation() {
    let cancel_token = Arc::new(tokio_util::sync::CancellationToken::new());
    let task = DownloadTask { cancel_token };

    assert!(!task.cancel_token.is_cancelled());
}

#[test]
fn list_song_create_with_metadata_has_album() {
    let song = ListSong::create_with_metadata(
        VideoID::from_raw("test"),
        "Title".to_string(),
        vec!["Artist".to_string()],
        Some("Album Name".to_string()),
        "3:33".to_string(),
    );

    use crate::app::structures::ListSongDisplayableField;

    assert!(song.album.is_some());
    assert_eq!(song.album.as_ref().unwrap().name, "Album Name");
    assert_eq!(
        song.get_field(ListSongDisplayableField::Artists).as_ref(),
        "Artist"
    );
    assert_eq!(song.title, "Title");
}

#[test]
fn list_song_create_with_metadata_no_album() {
    let song = ListSong::create_with_metadata(
        VideoID::from_raw("test"),
        "Title".to_string(),
        vec!["Artist1".to_string(), "Artist2".to_string()],
        None,
        "4:00".to_string(),
    );

    assert!(song.album.is_none());
    assert_eq!(
        song.get_field(ListSongDisplayableField::Artists).as_ref(),
        "Artist1, Artist2"
    );
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::app::structures::{ListSong, ListStatus};
    use crate::app::ui::playlist::Playlist;
    use crate::app::view::DrawableMut;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::prelude::Rect;
    use ytmapi_rs::common::VideoID;

    fn make_test_song(title: &str, artists: Vec<&str>, album: Option<&str>) -> ListSong {
        ListSong::create_with_metadata(
            VideoID::from_raw("id"),
            title.to_string(),
            artists.into_iter().map(String::from).collect(),
            album.map(String::from),
            "3:30".to_string(),
        )
    }

    fn render_playlist(songs: Vec<ListSong>) -> String {
        let (mut playlist, _) = Playlist::new(Percentage(50));
        playlist.list.state = ListStatus::Loaded;
        playlist.list.push_song_list(songs);
        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                playlist.draw_mut_chunk(f, Rect::new(0, 0, 120, 20), true, 0);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..20)
            .map(|row| {
                (0..120)
                    .map(|col| buffer[(col as u16, row as u16)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Verify that the artist column renders comma-separated artist names.
    #[test]
    fn render_shows_artist_names() {
        let songs = vec![make_test_song(
            "Song A",
            vec!["Artist 1", "Artist 2"],
            Some("Album X"),
        )];
        let output = render_playlist(songs);
        assert!(
            output.contains("Artist 1, Artist 2"),
            "Artist names should appear comma-separated in rendered output.\nGot:\n{output}"
        );
        assert!(
            output.contains("Song A"),
            "Song title should appear in rendered output.\nGot:\n{output}"
        );
    }

    /// Verify that the album name renders correctly.
    #[test]
    fn render_shows_album_name() {
        let songs = vec![make_test_song("Song B", vec!["Artist"], Some("My Album"))];
        let output = render_playlist(songs);
        assert!(
            output.contains("My Album"),
            "Album name should appear in rendered output.\nGot:\n{output}"
        );
    }

    /// Verify that songs without an album don't crash and render gracefully.
    #[test]
    fn render_no_album_does_not_crash() {
        let songs = vec![make_test_song("Song C", vec!["Artist"], None)];
        let output = render_playlist(songs);
        assert!(
            output.contains("Song C"),
            "Song without album should still render.\nGot:\n{output}"
        );
    }

    /// Verify that the artists_string cache works in rendered output
    /// (multiple artists joined by comma).
    #[test]
    fn render_multiple_artists_joined_by_comma() {
        let songs = vec![make_test_song(
            "Multi Art",
            vec!["Alpha", "Beta", "Gamma"],
            Some("Various"),
        )];
        let output = render_playlist(songs);
        assert!(
            output.contains("Alpha, Beta, Gamma"),
            "Three artists should be joined by comma+space.\nGot:\n{output}"
        );
    }

    /// Regression test: empty artists vec should not crash.
    #[test]
    fn render_empty_artists_does_not_crash() {
        let songs = vec![make_test_song("No Artist", vec![], Some("Lonely"))];
        let output = render_playlist(songs);
        assert!(
            output.contains("No Artist"),
            "Song with no artists should still render.\nGot:\n{output}"
        );
    }

    /// Verify the column headings are present.
    #[test]
    fn render_shows_column_headings() {
        let songs = vec![make_test_song("Any", vec!["A"], Some("B"))];
        let output = render_playlist(songs);
        for heading in &["Song", "Artists", "Album", "Year", "Duration"] {
            assert!(
                output.contains(heading),
                "Column heading '{heading}' should appear.\nGot:\n{output}"
            );
        }
    }
}

#[test]
fn songs_ahead_buffer_is_1() {
    assert_eq!(crate::app::ui::playlist::SONGS_AHEAD_TO_BUFFER, 1);
}

#[test]
fn songs_behind_save_is_0() {
    assert_eq!(crate::app::ui::playlist::SONGS_BEHIND_TO_SAVE, 0);
}

#[test]
fn download_scope_max_2_songs() {
    // Scope is: play-next entries + next(1) = up to 2 songs (current excluded)
    assert_eq!(
        1 // current
			+ crate::app::ui::playlist::SONGS_AHEAD_TO_BUFFER,
        2
    );
}

#[cfg(test)]
mod state_transitions {
    use crate::app::component::actionhandler::ActionHandler;
    use crate::app::structures::{
        DownloadStatus, ListSong, ListSongID, ListStatus, Percentage, PlayState,
    };
    use crate::app::ui::playlist::{
        DownloadProgressUpdate, HALT_AFTER_CONSECUTIVE_FAILURES, Playlist, PlaylistAction,
        QueueState, is_auth_error, is_dead_video_error,
    };
    use crate::app::view::HasTitle;
    use pretty_assertions::assert_eq;
    use ratatui::style::Color;
    use ytmapi_rs::common::{VideoID, YoutubeID};

    fn undownloaded_songs(n: usize) -> Playlist {
        let (mut p, _) = Playlist::new(Percentage(50));
        p.list.state = ListStatus::Loaded;
        let songs: Vec<ListSong> = (0..n)
            .map(|i| {
                let mut song = ListSong::create_with_metadata(
                    VideoID::from_raw(format!("video{i}")),
                    format!("Song {i}"),
                    vec!["Artist".to_string()],
                    None,
                    "3:00".to_string(),
                );
                song.download_status = DownloadStatus::None;
                song
            })
            .collect();
        p.list.push_song_list(songs);
        p
    }

    fn downloaded_songs(n: usize) -> Playlist {
        let (mut p, _) = Playlist::new(Percentage(50));
        p.list.state = ListStatus::Loaded;
        let songs: Vec<ListSong> = (0..n)
            .map(|i| {
                let mut song = ListSong::create_with_metadata(
                    VideoID::from_raw(format!("video{i}")),
                    format!("Song {i}"),
                    vec!["Artist".to_string()],
                    None,
                    "3:00".to_string(),
                );
                song.download_status = DownloadStatus::Downloaded;
                song
            })
            .collect();
        p.list.push_song_list(songs);
        p
    }

    #[test]
    fn permanently_unavailable_song_is_skipped_but_kept_in_queue() {
        let mut p = downloaded_songs(2);
        p.set_notifications_enabled(false);
        p.play_status = PlayState::Buffering(ListSongID(0));
        let _effect = p.handle_song_download_progress_update(
            DownloadProgressUpdate::Error("video unavailable (yt-dlp error)".to_string()),
            ListSongID(0),
        );
        // Dead song is session-remembered but never removed; next song starts.
        assert_eq!(p.list.get_list_iter().count(), 2);
        assert!(p.get_index_from_id(ListSongID(0)).is_some());
        assert!(p.list.session_dead_videos.contains("video0"));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn single_dead_song_stops_cleanly() {
        let mut p = downloaded_songs(1);
        p.set_notifications_enabled(false);
        p.play_status = PlayState::Buffering(ListSongID(0));
        let _effect = p.handle_song_download_progress_update(
            DownloadProgressUpdate::Error("video unavailable (yt-dlp error)".to_string()),
            ListSongID(0),
        );
        // Song is kept in the queue (never removed), but playback stops.
        assert_eq!(p.list.get_list_iter().count(), 1);
        assert!(p.get_index_from_id(ListSongID(0)).is_some());
        assert_eq!(p.play_status, PlayState::NotPlaying);
        assert_eq!(p.queue_status, QueueState::NotQueued);
    }

    #[test]
    fn session_dead_song_is_skipped_on_auto_advance() {
        let mut p = downloaded_songs(3);
        p.set_notifications_enabled(false);
        p.list.session_dead_videos.insert("video1".to_string());
        p.play_status = PlayState::Buffering(ListSongID(0));
        // Error on song 0 must auto-advance, jumping over the session-dead song 1.
        let _effect = p.handle_song_download_progress_update(
            DownloadProgressUpdate::Error("HTTP Error 429: Too Many Requests".to_string()),
            ListSongID(0),
        );
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(2)));
    }

    #[test]
    fn session_dead_is_not_persisted_across_playlists() {
        // New sessions start with an empty dead set (no disk persistence).
        assert!(Playlist::new(Percentage(50)).0.list.session_dead_videos.is_empty());
    }

    #[test]
    fn transient_error_does_not_remove_song() {
        let mut p = downloaded_songs(2);
        p.set_notifications_enabled(false);
        p.play_status = PlayState::Buffering(ListSongID(0));
        let _effect = p.handle_song_download_progress_update(
            DownloadProgressUpdate::Error("HTTP Error 429: Too Many Requests".to_string()),
            ListSongID(0),
        );
        // Transient failure keeps the song and advances playback only.
        assert_eq!(p.list.get_list_iter().count(), 2);
        assert!(p.get_index_from_id(ListSongID(0)).is_some());
    }

    #[test]
    fn dead_video_error_classifier() {
        assert!(is_dead_video_error("video unavailable (yt-dlp error)"));
        assert!(!is_dead_video_error("download cancelled"));
        assert!(!is_dead_video_error("HTTP Error 429: Too Many Requests"));
    }

    #[test]
    fn auth_error_classifier() {
        assert!(is_auth_error("authentication error (stale cookies)"));
        assert!(!is_auth_error("video unavailable (yt-dlp error)"));
        assert!(!is_auth_error("download cancelled"));
        assert!(!is_auth_error("HTTP Error 429: Too Many Requests"));
    }

    #[test]
    fn auth_error_skips_without_removing_song() {
        let mut p = downloaded_songs(2);
        p.set_notifications_enabled(false);
        p.play_status = PlayState::Buffering(ListSongID(0));
        let _effect = p.handle_song_download_progress_update(
            DownloadProgressUpdate::Error("authentication error (stale cookies)".to_string()),
            ListSongID(0),
        );
        // Auth failure skips playback but must never remove the song from the
        // queue — it is a login problem, not a dead video.
        assert_eq!(p.list.get_list_iter().count(), 2);
        assert!(p.get_index_from_id(ListSongID(0)).is_some());
    }

    #[test]
    fn repeated_transient_failures_halt_instead_of_draining() {
        let n = HALT_AFTER_CONSECUTIVE_FAILURES as usize + 5;
        let mut p = downloaded_songs(n);
        p.set_notifications_enabled(false);
        for i in 0..HALT_AFTER_CONSECUTIVE_FAILURES as usize {
            p.play_status = PlayState::Buffering(ListSongID(i));
            let _effect = p.handle_song_download_progress_update(
                DownloadProgressUpdate::Error("HTTP Error 429: Too Many Requests".to_string()),
                ListSongID(i),
            );
        }
        // A systemic failure must halt the player instead of walking the queue.
        assert_eq!(p.play_status, PlayState::NotPlaying);
        assert_eq!(p.queue_status, QueueState::NotQueued);
        assert_eq!(
            p.list.get_list_iter().count(),
            n,
            "transient failures must never remove songs"
        );
    }

    #[test]
    fn repeated_dead_videos_do_not_halt() {
        let n = HALT_AFTER_CONSECUTIVE_FAILURES as usize + 5;
        let mut p = downloaded_songs(n);
        p.set_notifications_enabled(false);
        for i in 0..HALT_AFTER_CONSECUTIVE_FAILURES as usize {
            p.play_status = PlayState::Buffering(ListSongID(i));
            let _effect = p.handle_song_download_progress_update(
                DownloadProgressUpdate::Error("video unavailable (yt-dlp error)".to_string()),
                ListSongID(i),
            );
        }
        // Deleted videos are definitive per-song conditions, not systemic
        // failures: the player must keep advancing, never halt.
        assert_eq!(
            p.play_status,
            PlayState::Buffering(ListSongID(HALT_AFTER_CONSECUTIVE_FAILURES as usize))
        );
        assert_eq!(
            p.list.get_list_iter().count(),
            n,
            "dead videos must never halt playback or drain the queue"
        );
    }

    #[test]
    fn repeated_auth_errors_do_not_halt() {
        let n = HALT_AFTER_CONSECUTIVE_FAILURES as usize + 5;
        let mut p = downloaded_songs(n);
        p.set_notifications_enabled(false);
        for i in 0..HALT_AFTER_CONSECUTIVE_FAILURES as usize {
            p.play_status = PlayState::Buffering(ListSongID(i));
            let _effect = p.handle_song_download_progress_update(
                DownloadProgressUpdate::Error("authentication error (stale cookies)".to_string()),
                ListSongID(i),
            );
        }
        // Auth failures (18+ tracks, stale session) are per-song/login
        // conditions: playback advances with per-song notifications, never halt.
        assert_eq!(
            p.play_status,
            PlayState::Buffering(ListSongID(HALT_AFTER_CONSECUTIVE_FAILURES as usize))
        );
        assert_eq!(
            p.list.get_list_iter().count(),
            n,
            "auth failures must never halt playback or drain the queue"
        );
    }

    #[test]
    fn transient_errors_below_threshold_still_advance() {
        let mut p = downloaded_songs(HALT_AFTER_CONSECUTIVE_FAILURES as usize + 5);
        p.set_notifications_enabled(false);
        for i in 0..(HALT_AFTER_CONSECUTIVE_FAILURES as usize - 1) {
            p.play_status = PlayState::Buffering(ListSongID(i));
            let _effect = p.handle_song_download_progress_update(
                DownloadProgressUpdate::Error("HTTP Error 429: Too Many Requests".to_string()),
                ListSongID(i),
            );
        }
        // Just below the threshold the player keeps advancing.
        assert_eq!(
            p.play_status,
            PlayState::Buffering(ListSongID(HALT_AFTER_CONSECUTIVE_FAILURES as usize - 1))
        );
    }

    #[test]
    fn successful_download_resets_failure_counter() {
        let mut p = downloaded_songs(HALT_AFTER_CONSECUTIVE_FAILURES as usize * 2);
        p.set_notifications_enabled(false);

        for i in 0..2usize {
            p.play_status = PlayState::Buffering(ListSongID(i));
            let _effect = p.handle_song_download_progress_update(
                DownloadProgressUpdate::Error("HTTP Error 429: Too Many Requests".to_string()),
                ListSongID(i),
            );
        }

        // A success resets the run, so the count restarts from zero.
        p.play_status = PlayState::NotPlaying;
        let _effect = p.handle_song_download_progress_update(
            DownloadProgressUpdate::Completed(Box::new(
                rodio::buffer::SamplesBuffer::new(
                    std::num::NonZeroU16::new(1).unwrap(),
                    std::num::NonZeroU32::new(44100).unwrap(),
                    vec![0.0f32; 4410],
                ),
            )),
            ListSongID(2),
        );

        // Two failures after the success are still below the threshold.
        for i in 3..5usize {
            p.play_status = PlayState::Buffering(ListSongID(i));
            let _effect = p.handle_song_download_progress_update(
                DownloadProgressUpdate::Error("HTTP Error 429: Too Many Requests".to_string()),
                ListSongID(i),
            );
        }
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(5)));
    }

    #[test]
    fn draw_get_items_survives_desynced_shuffle_map() {
        use crate::app::view::TableView;
        let mut p = undownloaded_songs(3);
        p.shuffle_enabled = true;
        p.shuffle_indices = vec![0, 1, 2];
        // Simulate a stale inverse map: one entry points at a removed row.
        p.shuffle_visual_map = vec![Some(0), Some(1), Some(5)];
        // Must render an empty row instead of panicking the whole TUI.
        let rows: Vec<_> = p.get_items().collect();
        assert_eq!(rows.len(), 3, "all rows must still render, no panic");
    }

    #[test]
    fn play_pause_resume_cycle() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Playing(ListSongID(0));
        let _ = p.pauseplay();
        assert_eq!(p.play_status, PlayState::Paused(ListSongID(0)));

        let _ = p.pauseplay();
        assert_eq!(p.play_status, PlayState::Playing(ListSongID(0)));
    }

    #[test]
    fn play_pause_resume_separate_methods() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Playing(ListSongID(0));
        let _ = p.pause();
        assert_eq!(p.play_status, PlayState::Paused(ListSongID(0)));

        let _ = p.resume();
        assert_eq!(p.play_status, PlayState::Playing(ListSongID(0)));
    }

    #[test]
    fn pause_when_not_playing_is_noop() {
        let mut p = downloaded_songs(3);
        let _ = p.pause();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn resume_when_not_paused_is_noop() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Playing(ListSongID(0));

        let _ = p.resume();
        assert_eq!(p.play_status, PlayState::Playing(ListSongID(0)));
    }

    #[test]
    fn handle_next_advances() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        let _ = p.handle_next();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn handle_previous_goes_back() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(1));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

        let _ = p.handle_previous();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn handle_next_on_last_song_wraps_to_first() {
        let mut p = downloaded_songs(2);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        let _ = p.handle_next();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

        // handle_next on last song wraps around to first song
        _ = p.handle_next();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn done_playing_last_song_wraps_to_first() {
        let mut p = downloaded_songs(1);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        // play_next_or_stop on the only song wraps around.
        _ = p.play_next_or_stop(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn handle_previous_on_first_song_wraps_to_last() {
        let mut p = downloaded_songs(2);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        let _ = p.handle_previous();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn switch_song_while_playing() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        // Song 1 is in scope (SONGS_AHEAD_TO_BUFFER=2 from index 0),
        // so it remains Downloaded and plays directly.
        let _ = p.play_song(ListSongID(1));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn stop_clears_play_status() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        let _ = p.stop();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn handle_next_when_not_playing_is_noop() {
        let mut p = downloaded_songs(3);
        let _ = p.handle_next();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn handle_previous_when_not_playing_jumps_to_last() {
        let mut p = downloaded_songs(3);
        // NotPlaying → play_prev jumps to the last song
        p.play_status = PlayState::NotPlaying;
        let _ = p.handle_previous();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(2)));
    }

    // ---------------------------------------------------------------------------
    // NotPlaying edge cases
    // ---------------------------------------------------------------------------

    #[test]
    fn not_playing_stop_goes_to_stopped() {
        let mut p = downloaded_songs(3);
        let _ = p.stop();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn not_playing_pauseplay_is_noop() {
        let mut p = downloaded_songs(3);
        let _ = p.pauseplay();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    // ---------------------------------------------------------------------------
    // Playing edge cases
    // ---------------------------------------------------------------------------

    #[test]
    fn replay_same_song_while_playing() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(1));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

        // play_song_id with same ID — should prepare + restart
        let _ = p.play_song(ListSongID(1));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    // ---------------------------------------------------------------------------
    // Paused — all actions
    // ---------------------------------------------------------------------------

    #[test]
    fn paused_pause_is_noop() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Paused(ListSongID(0));

        let _ = p.pause();
        assert_eq!(p.play_status, PlayState::Paused(ListSongID(0)));
    }

    #[test]
    fn paused_play_song_id_switches() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Paused(ListSongID(0));

        let _ = p.play_song(ListSongID(1));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn paused_handle_next_advances() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Paused(ListSongID(0));

        let _ = p.handle_next();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn paused_handle_previous_goes_back() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Paused(ListSongID(1));

        let _ = p.handle_previous();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn paused_stop_clears() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Paused(ListSongID(0));

        let _ = p.stop();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn paused_handle_next_on_last_song_wraps_to_first() {
        let mut p = downloaded_songs(2);
        p.play_status = PlayState::Paused(ListSongID(1));

        _ = p.handle_next();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn paused_handle_previous_on_first_wraps_to_last() {
        let mut p = downloaded_songs(2);
        p.play_status = PlayState::Paused(ListSongID(0));

        let _ = p.handle_previous();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    // ---------------------------------------------------------------------------
    // Buffering — all actions (most should be no-ops)
    // ---------------------------------------------------------------------------

    #[test]
    fn buffering_pause_is_noop() {
        let mut p = undownloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        let _ = p.pause();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn buffering_resume_is_noop() {
        let mut p = undownloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        let _ = p.resume();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn buffering_pauseplay_is_noop() {
        let mut p = undownloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        let _ = p.pauseplay();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn buffering_stop_clears() {
        let mut p = undownloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        let _ = p.stop();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn buffering_handle_next_advances_to_buffering() {
        let mut p = undownloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        // handle_next on buffering song calls play_song_id(1),
        // which is also undownloaded → Buffering(1)
        let _ = p.handle_next();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn buffering_handle_previous_goes_back_to_buffering() {
        let mut p = undownloaded_songs(3);
        let _ = p.play_song(ListSongID(1));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

        let _ = p.handle_previous();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn buffering_play_song_id_switches() {
        let mut p = undownloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        let _ = p.play_song(ListSongID(2));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(2)));
    }

    // ---------------------------------------------------------------------------
    // Error — all actions
    // ---------------------------------------------------------------------------

    #[test]
    fn error_pause_is_noop() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Error(ListSongID(0));

        let _ = p.pause();
        assert_eq!(p.play_status, PlayState::Error(ListSongID(0)));
    }

    #[test]
    fn error_resume_is_noop() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Error(ListSongID(0));

        let _ = p.resume();
        assert_eq!(p.play_status, PlayState::Error(ListSongID(0)));
    }

    #[test]
    fn error_pauseplay_is_noop() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Error(ListSongID(0));

        let _ = p.pauseplay();
        assert_eq!(p.play_status, PlayState::Error(ListSongID(0)));
    }

    #[test]
    fn error_stop_clears() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Error(ListSongID(0));

        let _ = p.stop();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn error_handle_next_advances_to_playing() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Error(ListSongID(0));

        let _ = p.handle_next();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn error_handle_previous_goes_back_to_playing() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Error(ListSongID(1));

        let _ = p.handle_previous();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
    }

    #[test]
    fn error_play_song_id_recovers() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Error(ListSongID(0));

        let _ = p.play_song(ListSongID(2));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(2)));
    }

    // ---------------------------------------------------------------------------
    // Stopped — all actions
    // ---------------------------------------------------------------------------

    #[test]
    fn stopped_pause_is_noop() {
        let mut p = downloaded_songs(3);
        let _ = p.stop();

        let _ = p.pause();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn stopped_resume_is_noop() {
        let mut p = downloaded_songs(3);
        let _ = p.stop();

        let _ = p.resume();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn stopped_pauseplay_is_noop() {
        let mut p = downloaded_songs(3);
        let _ = p.stop();

        let _ = p.pauseplay();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn stopped_stop_stays_stopped() {
        let mut p = downloaded_songs(3);
        let _ = p.stop();

        let _ = p.stop();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn stopped_handle_next_is_noop() {
        let mut p = downloaded_songs(3);
        let _ = p.stop();

        let _ = p.handle_next();
        assert_eq!(p.play_status, PlayState::NotPlaying);
    }

    #[test]
    fn stopped_handle_previous_jumps_to_last() {
        let mut p = downloaded_songs(3);
        let _ = p.stop();

        let _ = p.handle_previous();
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(2)));
    }

    #[test]
    fn stopped_play_song_id_starts_buffering() {
        let mut p = downloaded_songs(3);
        let _ = p.stop();

        let _ = p.play_song(ListSongID(1));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn play_song_advances_download_status() {
        let (mut p, _) = Playlist::new(Percentage(50));
        p.list.state = ListStatus::Loaded;
        let songs: Vec<ListSong> = (0..3)
            .map(|i| {
                ListSong::create_with_metadata(
                    VideoID::from_raw(format!("video{i}")),
                    format!("Song {i}"),
                    vec!["Artist".to_string()],
                    None,
                    "3:00".to_string(),
                )
            })
            .collect();
        p.list.push_song_list(songs);

        let id = p.get_id_from_index(0).expect("song at index 0");
        let _ = p.play_song(id);

        // INVARIANT: after play_song, the download pipeline must be active.
        // download_status must not be None (dead download), and an active
        // download task must exist. Otherwise the song stays in Buffering forever.
        let song = p.list.get_list_iter().next().expect("first song");
        assert_ne!(
            song.download_status,
            DownloadStatus::None,
            "play_song must advance download_status beyond None — otherwise no download will ever start"
        );
        let has_active = p
            .active_downloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(sid, _)| *sid == id);
        assert!(
            has_active,
            "play_song must register an active download — otherwise the song is stuck in Buffering with no download"
        );
        assert_eq!(p.play_status, PlayState::Buffering(id));
    }

    #[test]
    fn prebuffer_does_not_retry_failed() {
        let (mut p, _) = Playlist::new(Percentage(50));
        p.list.state = ListStatus::Loaded;
        let songs: Vec<ListSong> = (0..3)
            .map(|i| {
                ListSong::create_with_metadata(
                    VideoID::from_raw(format!("video{i}")),
                    format!("Song {i}"),
                    vec!["Artist".to_string()],
                    None,
                    "3:00".to_string(),
                )
            })
            .collect();
        p.list.push_song_list(songs);

        let id = p.get_id_from_index(0).expect("song at index 0");
        // Mark song as Failed (as if it previously failed to download)
        if let Some(idx) = p.get_index_from_id(id)
            && let Some(song) = p.list.get_list_iter_mut().nth(idx)
        {
            song.download_status = DownloadStatus::Failed;
        }

        let effect = p.download_song(id);

        // Prebuffer must NOT retry a Failed song — no download task created.
        assert!(effect.is_empty(), "prebuffer must not retry a Failed song");
        let song = p.list.get_list_iter().next().expect("first song");
        assert_eq!(
            song.download_status,
            DownloadStatus::Failed,
            "Failed status must remain unchanged"
        );
    }

    #[test]
    fn play_song_clears_failed_status() {
        let (mut p, _) = Playlist::new(Percentage(50));
        p.list.state = ListStatus::Loaded;
        let songs: Vec<ListSong> = (0..3)
            .map(|i| {
                ListSong::create_with_metadata(
                    VideoID::from_raw(format!("video{i}")),
                    format!("Song {i}"),
                    vec!["Artist".to_string()],
                    None,
                    "3:00".to_string(),
                )
            })
            .collect();
        p.list.push_song_list(songs);

        let id = p.get_id_from_index(0).expect("song at index 0");
        // Mark song as Failed (as if it previously failed to download)
        if let Some(idx) = p.get_index_from_id(id)
            && let Some(song) = p.list.get_list_iter_mut().nth(idx)
        {
            song.download_status = DownloadStatus::Failed;
        }

        let _ = p.play_song(id);

        // User-initiated play must clear Failed status so download can start.
        let song = p.list.get_list_iter().next().expect("first song");
        assert_ne!(
            song.download_status,
            DownloadStatus::Failed,
            "play_song must clear Failed status"
        );
    }

    #[test]
    fn status_bar_icon_playing_downloaded_shows_play() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Playing(ListSongID(0));
        // default status from downloaded_songs is Downloaded
        assert_eq!(
            p.status_bar_icon(),
            '',
            "Playing + Downloaded must show play icon"
        );
    }

    #[test]
    fn status_bar_icon_playing_queued_shows_download() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Playing(ListSongID(0));
        if let Some(idx) = p.get_index_from_id(ListSongID(0))
            && let Some(song) = p.list.get_list_iter_mut().nth(idx)
        {
            song.download_status = DownloadStatus::Queued;
        }
        assert_eq!(
            p.status_bar_icon(),
            '',
            "Playing + Queued must show download icon"
        );
    }

    #[test]
    fn status_bar_icon_playing_downloading_shows_download() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Playing(ListSongID(0));
        if let Some(idx) = p.get_index_from_id(ListSongID(0))
            && let Some(song) = p.list.get_list_iter_mut().nth(idx)
        {
            song.download_status = DownloadStatus::Downloading(Percentage(50));
        }
        assert_eq!(
            p.status_bar_icon(),
            '',
            "Playing + Downloading must show download icon"
        );
    }

    #[test]
    fn status_bar_icon_playing_none_shows_download() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Playing(ListSongID(0));
        if let Some(idx) = p.get_index_from_id(ListSongID(0))
            && let Some(song) = p.list.get_list_iter_mut().nth(idx)
        {
            song.download_status = DownloadStatus::None;
        }
        assert_eq!(
            p.status_bar_icon(),
            '',
            "Playing + None must show download icon"
        );
    }

    #[test]
    fn status_bar_icon_buffering_shows_download() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Buffering(ListSongID(0));
        assert_eq!(
            p.status_bar_icon(),
            '',
            "Buffering must show download icon"
        );
    }

    #[test]
    fn status_bar_icon_paused_shows_pause() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Paused(ListSongID(0));
        assert_eq!(p.status_bar_icon(), '', "Paused must show pause icon");
    }

    #[test]
    fn status_bar_icon_error_shows_warning() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::Error(ListSongID(0));
        assert_eq!(p.status_bar_icon(), '', "Error must show warning icon");
    }

    #[test]
    fn status_bar_icon_not_playing_shows_stop() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::NotPlaying;
        assert_eq!(p.status_bar_icon(), '', "NotPlaying must show stop icon");
    }

    #[test]
    fn status_bar_icon_stopped_shows_stop() {
        let mut p = downloaded_songs(3);
        p.play_status = PlayState::NotPlaying;
        assert_eq!(p.status_bar_icon(), '', "Stopped must show stop icon");
    }

    #[test]
    fn search_no_results_shows_red_title() {
        let mut p = downloaded_songs(3);
        p.search_text = "zzzzz_no_match".to_string();
        p.search_enabled = true;
        p.update_search_indices();
        assert!(p.search_indices.is_empty(), "search should match nothing");
        let title = p.get_title();
        let last_span = title.spans.last().unwrap();
        assert_eq!(
            last_span.style.fg,
            Some(Color::Red),
            "search indicator should be red when no results"
        );
        assert!(
            title
                .spans
                .iter()
                .any(|s| s.content.contains("zzzzz_no_match")),
            "search text should appear in title"
        );
    }

    #[test]
    fn search_with_results_shows_plain_title() {
        let mut p = downloaded_songs(3);
        p.search_text = "Song".to_string();
        p.search_enabled = true;
        p.update_search_indices();
        assert!(!p.search_indices.is_empty(), "search should match songs");
        let title = p.get_title();
        // A single span = no styled search indicator
        assert_eq!(
            title.spans.len(),
            1,
            "title with search results should be a single plain span"
        );
        assert!(
            title.spans[0].content.contains("[SEARCH: Song]"),
            "search indicator should appear in title"
        );
    }

    #[test]
    fn search_no_results_returns_to_plain_when_cleared() {
        let mut p = downloaded_songs(3);
        // Search with no results
        p.search_text = "zzzzz".to_string();
        p.search_enabled = true;
        p.update_search_indices();
        assert!(p.search_indices.is_empty());
        // Clear search — should go back to plain title
        p.search_text.clear();
        p.search_enabled = false;
        p.cached_title.borrow_mut().take();
        let title = p.get_title();
        assert_eq!(
            title.spans.len(),
            1,
            "cleared search should be a single plain span"
        );
    }

    #[test]
    fn search_no_results_rebuilds_with_red_when_keystroke_revalidates() {
        let mut p = downloaded_songs(3);
        // Simulate partial search that eventually gets results
        p.search_text = "zzzzz_partial".to_string();
        p.search_enabled = true;
        p.update_search_indices();
        assert!(p.search_indices.is_empty());
        let title = p.get_title();
        assert_eq!(
            title.spans.last().and_then(|s| s.style.fg),
            Some(Color::Red),
            "no results should show red"
        );
        // Now change search to something that matches
        p.search_text = "Song".to_string();
        p.cached_title.borrow_mut().take();
        p.update_search_indices();
        assert!(!p.search_indices.is_empty());
        let title2 = p.get_title();
        assert_eq!(
            title2.spans.len(),
            1,
            "matching search should be a single plain span"
        );
    }

    #[test]
    fn multi_word_search_narrows_results() {
        let mut p = downloaded_songs(3);
        // Single word "Song" matches all songs (title: "Song 0", "Song 1", "Song 2")
        p.search_text = "Song".to_string();
        p.search_enabled = true;
        p.update_search_indices();
        let all_count = p.search_indices.len();
        assert_eq!(all_count, 3, "\"Song\" should match all 3 songs");
        // Narrow: "Song 0" should match only the first song
        p.search_text = "Song 0".to_string();
        p.update_search_indices();
        assert_eq!(
            p.search_indices.len(),
            1,
            "multi-word \"Song 0\" should narrow to 1 result, got {}",
            p.search_indices.len()
        );
    }

    #[test]
    fn multi_word_search_matches_across_fields() {
        let mut p = downloaded_songs(3);
        // "Song Artist" — first word in title, second in artist
        p.search_text = "Song Artist".to_string();
        p.search_enabled = true;
        p.update_search_indices();
        assert_eq!(
            p.search_indices.len(),
            3,
            "words in title and artist should match all songs"
        );
    }

    #[test]
    fn multi_word_no_match_if_one_word_fails() {
        let mut p = downloaded_songs(3);
        // All songs match "Song", but none match "zzzzz"
        p.search_text = "Song zzzzz".to_string();
        p.search_enabled = true;
        p.update_search_indices();
        assert!(
            p.search_indices.is_empty(),
            "AND search: one non-matching word should block results"
        );
    }

    #[test]
    fn multi_word_whitespace_variants() {
        let mut p = downloaded_songs(3);
        // Double space should still split correctly
        p.search_text = "Song  Artist".to_string();
        p.search_enabled = true;
        p.update_search_indices();
        assert_eq!(
            p.search_indices.len(),
            3,
            "extra whitespace should still match"
        );
    }

    // ---------------------------------------------------------------------------
    // Play-next queue
    // ---------------------------------------------------------------------------

    #[test]
    fn add_to_play_next_adds_song() {
        let mut p = downloaded_songs(3);
        let id = p.get_id_from_index(0).expect("song at index 0");
        p.cur_selected = 0;
        let _ = p.apply_action(PlaylistAction::AddToPlayNext);
        assert_eq!(p.play_next_queue.len(), 1);
        assert_eq!(p.play_next_queue[0], id);
    }

    #[test]
    fn add_to_play_next_duplicate_not_added() {
        let mut p = downloaded_songs(3);
        p.cur_selected = 0;
        let _ = p.apply_action(PlaylistAction::AddToPlayNext);
        assert_eq!(p.play_next_queue.len(), 1);
        // Try adding the same song again
        let _ = p.apply_action(PlaylistAction::AddToPlayNext);
        assert_eq!(p.play_next_queue.len(), 1, "duplicate should not be added");
    }

    #[test]
    fn add_to_play_next_current_song_not_added() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        p.cur_selected = 0;
        // Current song is song 0, should not add it to play-next queue
        let _ = p.apply_action(PlaylistAction::AddToPlayNext);
        assert_eq!(
            p.play_next_queue.len(),
            0,
            "current song should not be added"
        );
    }

    #[test]
    fn play_next_or_stop_uses_play_next_queue() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        // Add song 2 to play-next queue
        p.play_next_queue.push_back(ListSongID(2));

        // play_next_or_stop should pop from queue instead of playing natural next (song 1)
        let _ = p.play_next_or_stop(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(2)));
    }

    #[test]
    fn play_next_queue_empty_falls_through() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        // Queue is empty, so play_next_or_stop should use natural order (song 1)
        let _ = p.play_next_or_stop(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
    }

    #[test]
    fn play_next_queue_fifo_order() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

        // Add song 1 and song 2 to play-next queue
        p.play_next_queue.push_back(ListSongID(1));
        p.play_next_queue.push_back(ListSongID(2));

        // First next → plays song 1 (pop front)
        let _ = p.play_next_or_stop(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

        // Second next → plays song 2 (pop front)
        let _ = p.play_next_or_stop(ListSongID(1));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(2)));
    }

    #[test]
    fn play_next_queue_clears_after_consumption() {
        let mut p = downloaded_songs(3);
        let _ = p.play_song(ListSongID(0));

        p.play_next_queue.push_back(ListSongID(1));
        let _ = p.play_next_or_stop(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

        // After consuming, queue should be empty and next falls through to natural order
        assert!(p.play_next_queue.is_empty());
        let _ = p.play_next_or_stop(ListSongID(1));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(2)));
    }

    #[test]
    fn play_next_queue_shows_count_in_title() {
        let mut p = downloaded_songs(3);
        // No items in queue
        let title = p.get_title();
        assert!(
            !title.spans.iter().any(|s| s.content.contains("[NEXT:")),
            "empty queue should not show NEXT indicator"
        );

        // Add 2 songs to play-next queue
        p.play_next_queue.push_back(ListSongID(0));
        p.play_next_queue.push_back(ListSongID(1));
        p.cached_title.borrow_mut().take();
        let title2 = p.get_title();
        assert!(
            title2.spans.iter().any(|s| s.content.contains("[NEXT: 2]")),
            "queue with 2 items should show [NEXT: 2]"
        );
    }

    #[test]
    fn play_next_queue_tracks_songs() {
        let mut p = downloaded_songs(3);
        p.play_next_queue.push_back(ListSongID(1));

        // Song 1 should be in play-next queue
        assert!(p.play_next_queue.contains(&ListSongID(1)));
        assert!(!p.play_next_queue.contains(&ListSongID(0)));
        assert!(!p.play_next_queue.contains(&ListSongID(2)));
    }

    #[test]
    fn play_next_queue_cleared_on_clear() {
        let mut p = downloaded_songs(3);
        p.play_next_queue.push_back(ListSongID(0));
        p.play_next_queue.push_back(ListSongID(1));
        assert_eq!(p.play_next_queue.len(), 2);
        p.clear();
        assert!(
            p.play_next_queue.is_empty(),
            "clear should empty play-next queue"
        );
    }

    #[test]
    fn play_next_queue_persists_across_shuffle_toggle() {
        let mut p = downloaded_songs(3);
        p.play_next_queue.push_back(ListSongID(0));
        p.play_next_queue.push_back(ListSongID(2));
        assert_eq!(p.play_next_queue.len(), 2);

        // Toggle shuffle on
        p.shuffle_enabled = true;
        p.generate_shuffle_indices();
        assert_eq!(
            p.play_next_queue.len(),
            2,
            "shuffle on should preserve play-next queue"
        );

        // Toggle shuffle off
        p.shuffle_enabled = false;
        p.shuffle_indices.clear();
        assert_eq!(
            p.play_next_queue.len(),
            2,
            "shuffle off should preserve play-next queue"
        );
    }

    #[test]
    fn play_next_queue_survives_stale_ids_gracefully() {
        let mut p = downloaded_songs(3);
        // Add a song to play-next queue
        p.play_next_queue.push_back(ListSongID(1));

        // Now pop and play it — should succeed
        let _ = p.play_song(ListSongID(0));
        p.play_status = PlayState::Playing(ListSongID(0));
        p.queue_status = super::QueueState::NotQueued;

        let _effect = p.play_next_or_stop(ListSongID(0));
        assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
        assert!(
            p.play_next_queue.is_empty(),
            "queue should be empty after consuming"
        );
    }

    #[test]
    fn prebuffer_includes_play_next_entries() {
        let mut p = undownloaded_songs(5);
        // Song 0 is current, add songs 3 and 4 to play-next queue
        p.play_next_queue.push_back(ListSongID(3));
        p.play_next_queue.push_back(ListSongID(4));

        let _effect = p.download_upcoming_from_id(ListSongID(0));

        // First play-next entry was popped to start download (entry 3).
        // With 1 ahead slot, entry 4 stays in play_next_queue (not yet downloaded).
        // Entry 1 (natural next) fills the remaining ahead slot.
        let dq: Vec<ListSongID> = p.download_queue.iter().copied().collect();
        assert!(
            dq.contains(&ListSongID(1)),
            "natural next song 1 should be in download queue"
        );
        assert!(
            !dq.contains(&ListSongID(4)),
            "play-next entry 4 should NOT be in download queue (scope=1 ahead)"
        );
        assert!(
            p.play_next_queue.contains(&ListSongID(4)),
            "entry 4 should remain in play-next queue for later"
        );
    }

    #[test]
    fn prebuffer_stale_play_next_ids_ignored() {
        let mut p = undownloaded_songs(5);
        // Add stale ID (not in playlist)
        p.play_next_queue.push_back(ListSongID(99));

        // Should not crash
        let _effect = p.download_upcoming_from_id(ListSongID(0));

        // Stale ID should not be in download queue
        assert!(!p.download_queue.contains(&ListSongID(99)));
    }

    #[test]
    fn prebuffer_play_next_priority_over_natural_order() {
        let mut p = undownloaded_songs(5);
        // Current song is 0, 1 ahead slot available.
        // Play-next queue has [3, 4] — entry 3 fills the slot, 4 stays in queue.
        p.play_next_queue.push_back(ListSongID(3));
        p.play_next_queue.push_back(ListSongID(4));

        let _effect = p.download_upcoming_from_id(ListSongID(0));

        // Entry 3 was started as the immediate download (popped from download_queue).
        // Entry 1 fills the remaining scope slot.
        // Entry 4 stays in play_next_queue (scope full).
        let dq: Vec<ListSongID> = p.download_queue.iter().copied().collect();
        assert_eq!(dq.len(), 1, "entry 1 remains in download queue");
        assert!(
            dq.contains(&ListSongID(1)),
            "natural next song 1 should be in download queue"
        );
        assert!(
            p.play_next_queue.contains(&ListSongID(3)),
            "entry 3 still in play_next_queue (only popped on actual playback)"
        );
        assert!(
            p.play_next_queue.contains(&ListSongID(4)),
            "entry 4 still in play_next_queue"
        );
    }

    #[test]
    fn held_shuffle_key_burst_keeps_only_latest_regen_token() {
        let mut p = undownloaded_songs(3);
        p.set_notifications_enabled(false);
        p.play_status = PlayState::Buffering(ListSongID(0));

        let _effect = p.toggle_shuffle();
        let first = p.shuffle_regen_token.clone();
        assert!(
            first.is_some(),
            "shuffle toggle with a playing song must schedule a debounced regeneration"
        );

        let _effect = p.toggle_shuffle();
        if let Some(first) = first {
            assert!(
                first.is_cancelled(),
                "a superseded shuffle regen must be cancelled, never left running"
            );
        }

        let _effect = p.toggle_shuffle();
        let effect = p.toggle_shuffle();
        drop(effect);

        let last = p
            .shuffle_regen_token
            .as_ref()
            .expect("latest toggle must still have a pending regen");
        assert!(
            !last.is_cancelled(),
            "the final toggle in the burst must be the surviving (live) regen"
        );
    }

    #[test]
    fn idle_shuffle_toggle_does_not_schedule_regen() {
        let mut p = undownloaded_songs(2);
        p.set_notifications_enabled(false);

        let effect = p.toggle_shuffle();
        drop(effect);
        assert!(
            p.shuffle_regen_token.is_none(),
            "with nothing playing, a shuffle toggle must not schedule a regen timer"
        );
    }
}
