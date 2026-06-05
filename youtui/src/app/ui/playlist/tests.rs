use crate::app::queue_persistence::{CompactSongRef, CompactSavedQueue};
use crate::app::server::song_downloader::InMemSong;
use crate::app::server::{DecodeSong, PlayDecodedSong, Stop, TaskMetadata};
use crate::app::structures::{
    DownloadStatus, ListSong, ListSongDisplayableField, ListSongID, ListStatus, PlayState,
};
use crate::app::ui::playlist::{
    DownloadTask, HandlePlayUpdateError, HandlePlayUpdateOk, HandleStopped, Playlist, QueueState,
};
use async_callback_manager::{AsyncTask, Constraint, TryBackendTaskExt};
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
    let (mut playlist, _effect) = Playlist::new();
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
    let dummy_song = Arc::new(InMemSong(vec![1]));
    p.list.get_list_iter_mut().nth(1).unwrap().download_status =
        DownloadStatus::Downloaded(dummy_song.clone());
    let effect = p.handle_song_downloaded(ListSongID(1));
    assert_eq!(p.play_status, PlayState::Playing(ListSongID(1)));
    let expected_effect = AsyncTask::new_stream_try(
        DecodeSong(dummy_song.clone()).map_stream(PlayDecodedSong(ListSongID(1))),
        HandlePlayUpdateOk,
        HandlePlayUpdateError(ListSongID(1)),
        Some(Constraint::new_block_matching_metadata(
            TaskMetadata::PlayingSong,
        )),
    );
    assert!(
        effect.contains(&expected_effect),
        "Expected to contain effect to play song {:?}",
        expected_effect
    );
}

#[test]
fn queued_song_plays_if_not_already_playing() {
    let mut p = get_dummy_playlist();
    p.play_status = PlayState::Buffering(ListSongID(0));
    p.queue_status = QueueState::Queued(ListSongID(0));
    let dummy_song = Arc::new(InMemSong(vec![1]));
    p.list.get_list_iter_mut().nth(0).unwrap().download_status =
        DownloadStatus::Downloaded(dummy_song.clone());
    let _effect = p.handle_song_downloaded(ListSongID(0));
    assert_eq!(p.play_status, PlayState::Playing(ListSongID(0)));
    // queue_status is set to NotQueued by autoplay_song_id
    assert_eq!(p.queue_status, QueueState::NotQueued);
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
        let (mut playlist, _) = Playlist::new();
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