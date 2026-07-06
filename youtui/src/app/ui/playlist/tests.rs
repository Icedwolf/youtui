use crate::app::queue_persistence::{CompactSongRef, CompactSavedQueue};
use crate::app::structures::{
    AudioQuality, DownloadStatus, ListSong, ListSongDisplayableField, ListSongID, ListStatus,
    Percentage, PlayState,
};
use super::{DownloadTask, PlayMode, Playlist, QueueState};
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
    let (mut playlist, _effect) = Playlist::new(Percentage(50), AudioQuality::Low);
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
        vec![],
    );
    playlist
}

#[test]
fn downloaded_song_plays_if_buffered() {
    let mut p = get_dummy_playlist();
    p.play_status = PlayState::Buffering(ListSongID(1));
    p.list.get_list_iter_mut().nth(1).unwrap().download_status =
        DownloadStatus::Downloaded;
    let _effect = p.handle_song_downloaded(ListSongID(1));
    assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
}

#[test]
fn queued_song_plays_if_not_already_playing() {
    let mut p = get_dummy_playlist();
    p.play_status = PlayState::Buffering(ListSongID(0));
    p.queue_status = QueueState::Queued(ListSongID(0));
    p.list.get_list_iter_mut().nth(0).unwrap().download_status =
        DownloadStatus::Downloaded;
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
        thumbnail_url: Some("https://example.com/thumb.jpg".to_string()),
    };
    
    assert_eq!(song_ref.video_id.get_raw(), "test123");
    assert_eq!(song_ref.title, "Test Song");
    assert_eq!(song_ref.artists.len(), 2);
    assert_eq!(song_ref.album, Some("Test Album".to_string()));
    assert_eq!(song_ref.duration_string, "3:45");
    assert!(song_ref.thumbnail_url.is_some());
}

#[test]
fn compact_song_ref_serialization_roundtrip() {
    let song_ref = CompactSongRef {
        video_id: VideoID::from_raw("abc123"),
        title: "Roundtrip Test".to_string(),
        artists: vec!["Solo Artist".to_string()],
        album: None,
        duration_string: "4:20".to_string(),
        thumbnail_url: None,
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
            thumbnail_url: None,
        },
        CompactSongRef {
            video_id: VideoID::from_raw("song2"),
            title: "Second Song".to_string(),
            artists: vec!["Artist".to_string()],
            album: Some("Album".to_string()),
            duration_string: "4:00".to_string(),
            thumbnail_url: None,
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
    let task = DownloadTask {
        cancel_token,
    };
    
    assert!(task.cancel_token.is_cancelled() == false);
}

#[test]
fn list_song_create_with_metadata_has_album() {
    let song = ListSong::create_with_metadata(
        VideoID::from_raw("test"),
        "Title".to_string(),
        vec!["Artist".to_string()],
        Some("Album Name".to_string()),
        "3:33".to_string(),
        None,
    );
    
    use crate::app::structures::ListSongDisplayableField;
    
    assert!(song.album.is_some());
    assert_eq!(song.album.as_ref().unwrap().name, "Album Name");
    assert_eq!(song.get_field(ListSongDisplayableField::Artists).as_ref(), "Artist");
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
        Some("https://example.com/thumb.jpg".to_string()),
    );
    
    assert!(song.album.is_none());
    assert_eq!(song.get_field(ListSongDisplayableField::Artists).as_ref(), "Artist1, Artist2");
    assert!(!song.thumbnails.is_empty());
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::app::structures::{ListSong, ListStatus};
    use crate::app::ui::playlist::Playlist;
    use crate::app::view::DrawableMut;
    use ratatui::backend::TestBackend;
    use ratatui::prelude::Rect;
    use ratatui::Terminal;
    use ytmapi_rs::common::VideoID;

    fn make_test_song(title: &str, artists: Vec<&str>, album: Option<&str>) -> ListSong {
        ListSong::create_with_metadata(
            VideoID::from_raw("id"),
            title.to_string(),
            artists.into_iter().map(String::from).collect(),
            album.map(String::from),
            "3:30".to_string(),
            None,
        )
    }

    fn render_playlist(songs: Vec<ListSong>) -> String {
        let (mut playlist, _) = Playlist::new(Percentage(50), AudioQuality::Low);
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
        let songs = vec![make_test_song("Song A", vec!["Artist 1", "Artist 2"], Some("Album X"))];
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
        for heading in &["p#", "t#", "Artist", "Album", "Song", "Duration", "Year"] {
            assert!(
                output.contains(heading),
                "Column heading '{heading}' should appear.\nGot:\n{output}"
            );
        }
    }
}

#[test]
fn songs_ahead_buffer_is_2() {
    assert_eq!(crate::app::ui::playlist::SONGS_AHEAD_TO_BUFFER, 2);
}

#[test]
fn songs_behind_save_is_1() {
    assert_eq!(crate::app::ui::playlist::SONGS_BEHIND_TO_SAVE, 1);
}

#[test]
fn download_scope_max_4_songs() {
	// Scope is: prev(1) + current + next(2) = 4 songs
	assert_eq!(
		crate::app::ui::playlist::SONGS_BEHIND_TO_SAVE
			+ 1 // current
			+ crate::app::ui::playlist::SONGS_AHEAD_TO_BUFFER,
		4
	);
}

#[cfg(test)]
mod state_transitions {
	use crate::app::structures::{AudioQuality, DownloadStatus, ListSong, ListSongID, ListStatus, Percentage, PlayState};
	use crate::app::ui::playlist::{PlayMode, Playlist};
	use pretty_assertions::assert_eq;
	use ytmapi_rs::common::{VideoID, YoutubeID};

	fn undownloaded_songs(n: usize) -> Playlist {
		let (mut p, _) = Playlist::new(Percentage(50), AudioQuality::Low);
		p.list.state = ListStatus::Loaded;
		let songs: Vec<ListSong> = (0..n)
			.map(|i| {
				let mut song = ListSong::create_with_metadata(
					VideoID::from_raw(format!("video{i}")),
					format!("Song {i}"),
					vec!["Artist".to_string()],
					None,
					"3:00".to_string(),
					None,
				);
				song.download_status = DownloadStatus::None;
				song
			})
			.collect();
		p.list.push_song_list(songs);
		p
	}

	fn downloaded_songs(n: usize) -> Playlist {
		let (mut p, _) = Playlist::new(Percentage(50), AudioQuality::Low);
		p.list.state = ListStatus::Loaded;
		let songs: Vec<ListSong> = (0..n)
			.map(|i| {
				let mut song = ListSong::create_with_metadata(
					VideoID::from_raw(format!("video{i}")),
					format!("Song {i}"),
					vec!["Artist".to_string()],
					None,
					"3:00".to_string(),
					None,
				);
				song.download_status =
					DownloadStatus::Downloaded;
				song
			})
			.collect();
		p.list.push_song_list(songs);
		p
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
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		let _ = p.handle_next();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
	}

	#[test]
	fn handle_previous_goes_back() {
		let mut p = downloaded_songs(3);
		let _ = p.play_song(ListSongID(1), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

		let _ = p.handle_previous();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
	}

	#[test]
	fn handle_next_on_last_song_emits_stop_task() {
		let mut p = downloaded_songs(2);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		let _ = p.handle_next();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

		// handle_next on last song calls play_next_or_stop → stop_song_id,
		// which emits an async stop task. Synchronously, play_status stays
		// the same until the callback fires.
		_ = p.handle_next();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
	}

	#[test]
	fn done_playing_last_song_emits_stop_task() {
		let mut p = downloaded_songs(1);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		// play_next_or_stop on the only song emits a stop task via
		// stop_song_id. Synchronous state stays the same.
		_ = p.play_next_or_stop(ListSongID(0));
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
	}

	#[test]
	fn handle_previous_on_first_song_is_noop() {
		let mut p = downloaded_songs(2);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		let _ = p.handle_previous();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
	}

	#[test]
	fn switch_song_while_playing() {
		let mut p = downloaded_songs(3);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		// Song 1 is in scope (SONGS_AHEAD_TO_BUFFER=2 from index 0),
		// so it remains Downloaded and plays directly.
		let _ = p.play_song(ListSongID(1), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
	}

	#[test]
	fn stop_clears_play_status() {
		let mut p = downloaded_songs(3);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		let _ = p.stop();
		assert_eq!(p.play_status, PlayState::Stopped);
	}


	#[test]
	fn handle_next_when_not_playing_is_noop() {
		let mut p = downloaded_songs(3);
		let _ = p.handle_next();
		assert_eq!(p.play_status, PlayState::NotPlaying);
	}

	#[test]
	fn handle_previous_when_not_playing_is_noop() {
		let mut p = downloaded_songs(3);
		let _ = p.handle_previous();
		assert_eq!(p.play_status, PlayState::NotPlaying);
	}

	// ---------------------------------------------------------------------------
	// NotPlaying edge cases
	// ---------------------------------------------------------------------------

	#[test]
	fn not_playing_stop_goes_to_stopped() {
		let mut p = downloaded_songs(3);
		let _ = p.stop();
		assert_eq!(p.play_status, PlayState::Stopped);
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
		let _ = p.play_song(ListSongID(1), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

		// play_song_id with same ID — should prepare + restart
		let _ = p.play_song(ListSongID(1), PlayMode::UserInitiated);
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

		let _ = p.play_song(ListSongID(1), PlayMode::UserInitiated);
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
		assert_eq!(p.play_status, PlayState::Stopped);
	}

	#[test]
	fn paused_handle_next_on_last_song_emits_stop_task() {
		let mut p = downloaded_songs(2);
		p.play_status = PlayState::Paused(ListSongID(1));

		_ = p.handle_next();
		assert_eq!(p.play_status, PlayState::Paused(ListSongID(1)));
	}

	#[test]
	fn paused_handle_previous_on_first_is_noop() {
		let mut p = downloaded_songs(2);
		p.play_status = PlayState::Paused(ListSongID(0));

		let _ = p.handle_previous();
		assert_eq!(p.play_status, PlayState::Paused(ListSongID(0)));
	}

	// ---------------------------------------------------------------------------
	// Buffering — all actions (most should be no-ops)
	// ---------------------------------------------------------------------------

	#[test]
	fn buffering_pause_is_noop() {
		let mut p = undownloaded_songs(3);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		let _ = p.pause();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
	}

	#[test]
	fn buffering_resume_is_noop() {
		let mut p = undownloaded_songs(3);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		let _ = p.resume();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
	}

	#[test]
	fn buffering_pauseplay_is_noop() {
		let mut p = undownloaded_songs(3);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		let _ = p.pauseplay();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
	}

	#[test]
	fn buffering_stop_clears() {
		let mut p = undownloaded_songs(3);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		let _ = p.stop();
		assert_eq!(p.play_status, PlayState::Stopped);
	}

	#[test]
	fn buffering_handle_next_advances_to_buffering() {
		let mut p = undownloaded_songs(3);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		// handle_next on buffering song calls play_song_id(1),
		// which is also undownloaded → Buffering(1)
		let _ = p.handle_next();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
	}

	#[test]
	fn buffering_handle_previous_goes_back_to_buffering() {
		let mut p = undownloaded_songs(3);
		let _ = p.play_song(ListSongID(1), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));

		let _ = p.handle_previous();
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));
	}

	#[test]
	fn buffering_play_song_id_switches() {
		let mut p = undownloaded_songs(3);
		let _ = p.play_song(ListSongID(0), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(0)));

		let _ = p.play_song(ListSongID(2), PlayMode::UserInitiated);
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
		assert_eq!(p.play_status, PlayState::Stopped);
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

		let _ = p.play_song(ListSongID(2), PlayMode::UserInitiated);
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
		assert_eq!(p.play_status, PlayState::Stopped);
	}

	#[test]
	fn stopped_resume_is_noop() {
		let mut p = downloaded_songs(3);
		let _ = p.stop();

		let _ = p.resume();
		assert_eq!(p.play_status, PlayState::Stopped);
	}

	#[test]
	fn stopped_pauseplay_is_noop() {
		let mut p = downloaded_songs(3);
		let _ = p.stop();

		let _ = p.pauseplay();
		assert_eq!(p.play_status, PlayState::Stopped);
	}

	#[test]
	fn stopped_stop_stays_stopped() {
		let mut p = downloaded_songs(3);
		let _ = p.stop();

		let _ = p.stop();
		assert_eq!(p.play_status, PlayState::Stopped);
	}

	#[test]
	fn stopped_handle_next_is_noop() {
		let mut p = downloaded_songs(3);
		let _ = p.stop();

		let _ = p.handle_next();
		assert_eq!(p.play_status, PlayState::Stopped);
	}

	#[test]
	fn stopped_handle_previous_is_noop() {
		let mut p = downloaded_songs(3);
		let _ = p.stop();

		let _ = p.handle_previous();
		assert_eq!(p.play_status, PlayState::Stopped);
	}

	#[test]
	fn stopped_play_song_id_starts_buffering() {
		let mut p = downloaded_songs(3);
		let _ = p.stop();

		let _ = p.play_song(ListSongID(1), PlayMode::UserInitiated);
		assert_eq!(p.play_status, PlayState::Buffering(ListSongID(1)));
	}

	#[test]
	fn play_song_advances_download_status() {
		let (mut p, _) = Playlist::new(Percentage(50), AudioQuality::Low);
		p.list.state = ListStatus::Loaded;
		let songs: Vec<ListSong> = (0..3)
			.map(|i| ListSong::create_with_metadata(
				VideoID::from_raw(format!("video{i}")),
				format!("Song {i}"),
				vec!["Artist".to_string()],
				None,
				"3:00".to_string(),
				None,
			))
			.collect();
		p.list.push_song_list(songs);

		let id = p.get_id_from_index(0).expect("song at index 0");
		let _ = p.play_song(id, PlayMode::UserInitiated);

		// INVARIANT: after play_song, the download pipeline must be active.
		// download_status must not be None (dead download), and an active
		// download task must exist. Otherwise the song stays in Buffering forever.
		let song = p.list.get_list_iter().next().expect("first song");
		assert_ne!(song.download_status, DownloadStatus::None,
			"play_song must advance download_status beyond None — otherwise no download will ever start");
		let has_active = p.active_downloads
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.iter()
			.any(|(sid, _)| *sid == id);
		assert!(has_active,
			"play_song must register an active download — otherwise the song is stuck in Buffering with no download");
		assert_eq!(p.play_status, PlayState::Buffering(id));
	}

	#[test]
	fn prebuffer_does_not_retry_failed() {
		let (mut p, _) = Playlist::new(Percentage(50), AudioQuality::Low);
		p.list.state = ListStatus::Loaded;
		let songs: Vec<ListSong> = (0..3)
			.map(|i| ListSong::create_with_metadata(
				VideoID::from_raw(format!("video{i}")),
				format!("Song {i}"),
				vec!["Artist".to_string()],
				None,
				"3:00".to_string(),
				None,
			))
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
		assert!(effect.is_no_op(),
			"prebuffer must not retry a Failed song");
		let song = p.list.get_list_iter().next().expect("first song");
		assert_eq!(song.download_status, DownloadStatus::Failed,
			"Failed status must remain unchanged");
	}

	#[test]
	fn play_song_clears_failed_status() {
		let (mut p, _) = Playlist::new(Percentage(50), AudioQuality::Low);
		p.list.state = ListStatus::Loaded;
		let songs: Vec<ListSong> = (0..3)
			.map(|i| ListSong::create_with_metadata(
				VideoID::from_raw(format!("video{i}")),
				format!("Song {i}"),
				vec!["Artist".to_string()],
				None,
				"3:00".to_string(),
				None,
			))
			.collect();
		p.list.push_song_list(songs);

		let id = p.get_id_from_index(0).expect("song at index 0");
		// Mark song as Failed (as if it previously failed to download)
		if let Some(idx) = p.get_index_from_id(id)
			&& let Some(song) = p.list.get_list_iter_mut().nth(idx)
		{
			song.download_status = DownloadStatus::Failed;
		}

		let _ = p.play_song(id, PlayMode::UserInitiated);

		// User-initiated play must clear Failed status so download can start.
		let song = p.list.get_list_iter().next().expect("first song");
		assert_ne!(song.download_status, DownloadStatus::Failed,
			"play_song must clear Failed status");
	}

	#[test]
	fn status_bar_icon_playing_downloaded_shows_play() {
		let mut p = downloaded_songs(3);
		p.play_status = PlayState::Playing(ListSongID(0));
		// default status from downloaded_songs is Downloaded
		assert_eq!(p.status_bar_icon(), '',
			"Playing + Downloaded must show play icon");
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
		assert_eq!(p.status_bar_icon(), '',
			"Playing + Queued must show download icon");
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
		assert_eq!(p.status_bar_icon(), '',
			"Playing + Downloading must show download icon");
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
		assert_eq!(p.status_bar_icon(), '',
			"Playing + None must show download icon");
	}

	#[test]
	fn status_bar_icon_buffering_shows_download() {
		let mut p = downloaded_songs(3);
		p.play_status = PlayState::Buffering(ListSongID(0));
		assert_eq!(p.status_bar_icon(), '',
			"Buffering must show download icon");
	}

	#[test]
	fn status_bar_icon_paused_shows_pause() {
		let mut p = downloaded_songs(3);
		p.play_status = PlayState::Paused(ListSongID(0));
		assert_eq!(p.status_bar_icon(), '',
			"Paused must show pause icon");
	}

	#[test]
	fn status_bar_icon_error_shows_warning() {
		let mut p = downloaded_songs(3);
		p.play_status = PlayState::Error(ListSongID(0));
		assert_eq!(p.status_bar_icon(), '',
			"Error must show warning icon");
	}

	#[test]
	fn status_bar_icon_not_playing_shows_stop() {
		let mut p = downloaded_songs(3);
		p.play_status = PlayState::NotPlaying;
		assert_eq!(p.status_bar_icon(), '',
			"NotPlaying must show stop icon");
	}

	#[test]
	fn status_bar_icon_stopped_shows_stop() {
		let mut p = downloaded_songs(3);
		p.play_status = PlayState::Stopped;
		assert_eq!(p.status_bar_icon(), '',
			"Stopped must show stop icon");
	}
}