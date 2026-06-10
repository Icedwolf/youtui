use crate::app::component::actionhandler::ComponentEffect;
use crate::app::structures::ListSong;
use crate::app::structures::Thumbnail;
use crate::app::ui::playlist::Playlist;
use crate::get_data_dir;
use async_callback_manager::AsyncTask;
use fs_err as fs;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use tracing::{debug, info, warn};
use ytmapi_rs::common::VideoID;

const QUEUE_DIR: &str = "queues";
const AUTO_SAVE: &str = "__autosave";

fn get_largest_thumbnail_url(thumbs: &[Thumbnail]) -> Option<String> {
    thumbs.iter().max_by_key(|t| t.height * t.width).map(|t| t.url.clone())
}

impl From<&ListSong> for CompactSongRef {
    fn from(song: &ListSong) -> Self {
        CompactSongRef {
            video_id: song.video_id.clone(),
            title: song.title.clone(),
            artists: song.artists.iter().map(|a| a.name.clone()).collect(),
            album: song.album.as_ref().map(|a| a.name.clone()),
            duration_string: song.duration_string.clone(),
            thumbnail_url: get_largest_thumbnail_url(song.thumbnails.as_ref()),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct LegacySong {
    songs: Vec<ListSong>,
    current_index: Option<usize>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CompactSongRef {
    pub video_id: VideoID<'static>,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_string: String,
    pub thumbnail_url: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CompactSavedQueue {
    pub songs: Vec<CompactSongRef>,
    /// Actual (list) index of the playing song at save time, NOT a visual index.
    /// On load, this is used directly as `get_id_from_index(idx)` regardless of
    /// shuffle state, and then `enable_shuffle` syncs `cur_selected`. If shuffle
    /// was OFF at save time, this is the natural list position.
    pub current_index: Option<usize>,
    #[serde(default)]
    pub shuffle_enabled: bool,
    #[serde(default)]
    pub shuffle_seed: u64,
}

pub fn get_queue_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = get_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(QUEUE_DIR);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn save_queue(playlist: &Playlist, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let raw_songs: Vec<ListSong> = playlist.list.get_list_iter().cloned().collect();
    let songs: Vec<CompactSongRef> = raw_songs.iter().map(CompactSongRef::from).collect();

    let current_idx = playlist.get_cur_playing_index();
    let saved = CompactSavedQueue {
        songs,
        current_index: current_idx,
        shuffle_enabled: playlist.shuffle_enabled(),
        shuffle_seed: playlist.shuffle_seed(),
    };

    let queue_dir = get_queue_dir()?;
    let path = queue_dir.join(format!("{}.json", name));
    let temp_path = queue_dir.join(format!("{}.json.tmp", name));

    let json = serde_json::to_string_pretty(&saved)?;
    let mut temp_file = fs::File::create(&temp_path)?;
    temp_file.write_all(json.as_bytes())?;
    temp_file.sync_all()?;
    drop(temp_file);

    fs::rename(&temp_path, &path)?;

    info!(
        "Successfully saved queue '{}' ({} songs, current_index: {:?})",
        name,
        saved.songs.len(),
        current_idx
    );
    Ok(())
}

pub fn load_queue(playlist: &mut Playlist, name: &str) -> Result<ComponentEffect<Playlist>, Box<dyn std::error::Error>> {
    let path = get_queue_dir()?.join(format!("{}.json", name));
    debug!("Loading queue from path: {:?}", path);
    
    let json = fs::read_to_string(&path)?;
    debug!("Read JSON: {}", json);

    if let Ok(saved) = serde_json::from_str::<CompactSavedQueue>(&json) {
        debug!("Parsed as CompactSavedQueue ({} songs)", saved.songs.len());
        load_compact_queue(playlist, saved)
    } else if let Ok(saved) = serde_json::from_str::<LegacySong>(&json) {
        debug!("Parsed as LegacySong format ({} songs), will normalize", saved.songs.len());
        normalize_and_load(playlist, saved, name)
    } else {
        warn!("Queue file corrupted, starting fresh");
        Ok(AsyncTask::new_no_op())
    }
}

fn load_compact_queue(playlist: &mut Playlist, saved: CompactSavedQueue) -> Result<ComponentEffect<Playlist>, Box<dyn std::error::Error>> {
    debug!("Loaded compact queue with {} songs", saved.songs.len());
    debug!("Clearing playlist (reset)");
    let mut effect = playlist.reset();
    
    if !saved.songs.is_empty() {
        playlist.set_loaded_from_autosave(true);
        let songs: Vec<ListSong> = saved.songs
            .iter()
            .map(|ref_| {
                ListSong::create_with_metadata(
                    ref_.video_id.clone(),
                    ref_.title.clone(),
                    ref_.artists.clone(),
                    ref_.album.clone(),
                    ref_.duration_string.clone(),
                    ref_.thumbnail_url.clone(),
                )
            })
            .collect();
        
        debug!("Created {} songs from compact metadata", songs.len());
        let (first_id, push_effect) = playlist.push_song_list(songs);
        effect = effect.push(push_effect);
        // Remove any duplicates that might exist in the saved data
        playlist.deduplicate();

        if saved.shuffle_enabled {
            playlist.enable_shuffle(saved.shuffle_seed);
            debug!("Restored shuffle (seed={})", saved.shuffle_seed);
        }
        
        if let Some(idx) = saved.current_index {
            if let Some(song_id) = playlist.get_id_from_index(idx) {
                effect = effect.push(playlist.play_song_id(song_id));
                debug!("Restored playback to song at index {}", idx);
            } else {
                effect = effect.push(playlist.play_song_id(first_id));
                debug!("Saved index {} out of bounds, playing first song", idx);
            }
        }
        debug!("Load complete");
    } else {
        debug!("No songs to load from save file");
    }
    Ok(effect)
}

fn normalize_and_load(playlist: &mut Playlist, saved: LegacySong, name: &str) -> Result<ComponentEffect<Playlist>, Box<dyn std::error::Error>> {
    debug!("Normalizing queue file to compact format");
    let songs: Vec<CompactSongRef> = saved.songs.iter().map(|s| CompactSongRef::from(s)).collect();
    let current_idx = saved.current_index;
    let compact = CompactSavedQueue {
        songs,
        current_index: current_idx,
        shuffle_enabled: false,
        shuffle_seed: 0,
    };

    let queue_dir = get_queue_dir()?;
    let path = queue_dir.join(format!("{}.json", name));
    let temp_path = queue_dir.join(format!("{}.json.tmp", name));

    let json = serde_json::to_string_pretty(&compact)?;
    let mut file = fs::File::create(&temp_path)?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temp_path, &path)?;

    debug!("Normalized queue to compact format");
    load_compact_queue(playlist, compact)
}

pub fn auto_save(playlist: &Playlist) -> Result<(), Box<dyn std::error::Error>> {
    debug!("Auto-saving queue");
    save_queue(playlist, AUTO_SAVE)
}

pub fn auto_load(playlist: &mut Playlist) -> Result<ComponentEffect<Playlist>, Box<dyn std::error::Error>> {
    debug!("Auto-loading queue from __autosave.json");
    match load_queue(playlist, AUTO_SAVE) {
        Ok(effect) => {
            debug!("Auto-load succeeded, effect is_no_op={}", effect.is_no_op());
            Ok(effect)
        }
        Err(e) => {
            warn!("Auto-load failed: {e}");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytmapi_rs::common::YoutubeID;

    #[test]
    fn test_compact_song_ref_serialization() {
        let video_id = VideoID::from_raw("abc123");
        let song_ref = CompactSongRef {
            video_id: video_id.clone(),
            title: "Test Song".to_string(),
            artists: vec!["Artist 1".to_string(), "Artist 2".to_string()],
            album: Some("Test Album".to_string()),
            duration_string: "3:45".to_string(),
            thumbnail_url: Some("https://example.com/thumb.jpg".to_string()),
        };
        let json = serde_json::to_string_pretty(&song_ref).unwrap();
        assert!(json.contains("abc123"));
        assert!(json.contains("video_id"));
        assert!(json.contains("Test Song"));
        assert!(json.contains("Artist 1"));
        assert!(json.contains("Artist 2"));
        assert!(json.contains("Test Album"));
        assert!(json.contains("album"));
    }

    #[test]
    fn test_compact_song_ref_deserialization() {
        let json = r#"{
            "video_id": "test123",
            "title": "Loaded Song",
            "artists": ["Artist X", "Artist Y"],
            "album": "Album Name",
            "duration_string": "5:00",
            "thumbnail_url": null
        }"#;
        
        let song_ref: CompactSongRef = serde_json::from_str(json).unwrap();
        assert_eq!(song_ref.video_id.get_raw(), "test123");
        assert_eq!(song_ref.title, "Loaded Song");
        assert_eq!(song_ref.artists.len(), 2);
        assert_eq!(song_ref.artists[0], "Artist X");
        assert_eq!(song_ref.album, Some("Album Name".to_string()));
        assert_eq!(song_ref.duration_string, "5:00");
        assert!(song_ref.thumbnail_url.is_none());
    }

    #[test]
    fn test_compact_format_json_structure() {
        let song_ref = CompactSongRef {
            video_id: VideoID::from_raw("v123"),
            title: "Compact Song".to_string(),
            artists: vec!["Solo Artist".to_string()],
            album: Some("Album Title".to_string()),
            duration_string: "3:33".to_string(),
            thumbnail_url: Some("https://example.com/img.jpg".to_string()),
        };
        
        let json = serde_json::to_string(&song_ref).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        
        // Verify structure has only compact fields
        let keys: Vec<_> = parsed.as_object().unwrap().keys().collect();
        let expected_keys = vec!["video_id", "title", "artists", "album", "duration_string", "thumbnail_url"];
        for key in expected_keys {
            assert!(keys.iter().any(|k| *k == key));
        }
        
        // Verify no heavy fields
        let excluded_keys = vec!["thumbnails", "artists_string"];
        for key in excluded_keys {
            assert!(!keys.iter().any(|k| *k == key));
        }
    }

    #[test]
    fn test_artists_serialization_format() {
        let song_ref = CompactSongRef {
            video_id: VideoID::from_raw("multi"),
            title: "Multi Artist Song".to_string(),
            artists: vec!["First".to_string(), "Second".to_string(), "Third".to_string()],
            album: None,
            duration_string: "4:00".to_string(),
            thumbnail_url: None,
        };
        
        let json = serde_json::to_string(&song_ref).unwrap();
        // Artists should be a JSON array
        assert!(json.contains(r#""artists":["First","Second","Third"]"#));
    }

    #[test]
    fn test_load_compact_queue_populates_songs() {
        let saved = CompactSavedQueue {
            current_index: Some(1),
            shuffle_enabled: false,
            shuffle_seed: 0,
            songs: vec![
                CompactSongRef {
                    video_id: VideoID::from_raw("v1"),
                    title: "Track One".to_string(),
                    artists: vec!["Alice".to_string()],
                    album: Some("Album X".to_string()),
                    duration_string: "3:00".to_string(),
                    thumbnail_url: None,
                },
                CompactSongRef {
                    video_id: VideoID::from_raw("v2"),
                    title: "Track Two".to_string(),
                    artists: vec!["Bob".to_string(), "Carol".to_string()],
                    album: None,
                    duration_string: "4:30".to_string(),
                    thumbnail_url: Some("https://example.com/t.jpg".to_string()),
                },
            ],
        };
        let (mut playlist, _effect) = Playlist::new();
        let effect = load_compact_queue(&mut playlist, saved).unwrap();

        let songs: Vec<_> = playlist.list.get_list_iter().collect();
        assert_eq!(songs.len(), 2);
        assert_eq!(songs[0].video_id.get_raw(), "v1");
        assert_eq!(songs[0].title, "Track One");
        assert_eq!(songs[0].artists.len(), 1);
        assert_eq!(songs[0].artists[0].name, "Alice");
        assert_eq!(songs[1].video_id.get_raw(), "v2");
        assert_eq!(songs[1].title, "Track Two");
        assert_eq!(songs[1].artists.len(), 2);
        assert!(playlist.get_cur_playing_index().is_some(),
            "current_index=1 should set a playing song");
        assert!(!effect.is_no_op(),
            "load with songs should produce a real effect");
    }

    #[test]
    fn test_load_compact_queue_empty() {
        let saved = CompactSavedQueue {
            current_index: None,
            shuffle_enabled: false,
            shuffle_seed: 0,
            songs: vec![],
        };
        let (mut playlist, _effect) = Playlist::new();
        let effect = load_compact_queue(&mut playlist, saved).unwrap();
        assert_eq!(playlist.list.get_list_iter().count(), 0);
        assert!(playlist.get_cur_playing_index().is_none());
        assert!(effect.is_no_op(),
            "empty load should produce no-op");
    }

    #[test]
    fn test_save_load_filesystem_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("YOUTUI_DATA_DIR", tmp.path()) };

        // save + load with songs
        let songs = vec![
            ListSong::create_with_metadata(
                VideoID::from_raw("a1"), "Alpha".to_string(), vec!["Art A".to_string()],
                Some("Alb A".to_string()), "2:00".to_string(), None,
            ),
            ListSong::create_with_metadata(
                VideoID::from_raw("b2"), "Beta".to_string(), vec!["Art B".to_string()],
                None, "3:15".to_string(), Some("https://ex.co/t.jpg".to_string()),
            ),
        ];
        let (mut playlist, _effect) = Playlist::new();
        let (_first_id, _) = playlist.push_song_list(songs);
        save_queue(&playlist, "fs_test").unwrap();
        assert!(tmp.path().join("queues/fs_test.json").exists());

        let (mut loaded, _effect) = Playlist::new();
        let _ = load_queue(&mut loaded, "fs_test").unwrap();
        let loaded_songs: Vec<_> = loaded.list.get_list_iter().collect();
        assert_eq!(loaded_songs.len(), 2);
        assert_eq!(loaded_songs[0].video_id.get_raw(), "a1");
        assert_eq!(loaded_songs[0].title, "Alpha");
        assert_eq!(loaded_songs[1].video_id.get_raw(), "b2");
        assert_eq!(loaded_songs[1].title, "Beta");

        // autosave + autoload
        let (mut p2, _) = Playlist::new();
        let xsongs = vec![ListSong::create_with_metadata(
            VideoID::from_raw("x"), "X".to_string(), vec!["Y".to_string()],
            None, "1:00".to_string(), None,
        )];
        let (_first_id2, _) = p2.push_song_list(xsongs);
        auto_save(&p2).unwrap();
        assert!(tmp.path().join("queues/__autosave.json").exists());

        let (mut loaded2, _) = Playlist::new();
        let _ = auto_load(&mut loaded2).unwrap();
        assert_eq!(loaded2.list.get_list_iter().count(), 1);
        assert_eq!(loaded2.list.get_list_iter().next().unwrap().video_id.get_raw(), "x");

        // load nonexistent file errors
        let (mut p3, _) = Playlist::new();
        let result = load_queue(&mut p3, "no_such_queue");
        assert!(result.is_err(), "loading nonexistent queue should error");
    }
}