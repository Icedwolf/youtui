use super::view::SortDirection;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::ops::Deref;
use std::rc::Rc;
use std::time::Duration;
use tracing::{debug, info, warn};
use ytmapi_rs::common::{
    AlbumID, ArtistChannelID, Explicit, UploadAlbumID, UploadArtistID, VideoID, YoutubeID,
};
use ytmapi_rs::parse::{
    AlbumSong, ParsedSongAlbum, ParsedSongArtist, ParsedUploadArtist, ParsedUploadSongAlbum,
    PlaylistEpisode, PlaylistItem, PlaylistSong, PlaylistUploadSong, PlaylistVideo,
    SearchResultSong,
};

pub trait SongListComponent {
    fn get_song_from_idx(&self, idx: usize) -> Option<&ListSong>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MaybeRc<T> {
    Rc(Rc<T>),
    Owned(T),
}
impl<T> Deref for MaybeRc<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        match self {
            MaybeRc::Rc(rc) => rc.deref(),
            MaybeRc::Owned(t) => t,
        }
    }
}
impl<T> AsRef<T> for MaybeRc<T> {
    fn as_ref(&self) -> &T {
        match self {
            MaybeRc::Rc(rc) => rc,
            MaybeRc::Owned(t) => t,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserSongsList {
    pub state: ListStatus,
    list: Vec<ListSong>,
    pub next_id: ListSongID,
    /// video_ids whose stream is permanently unavailable, remembered for the
    /// lifetime of this process only (never persisted). Used to skip them on
    /// auto-advance without removing them from the user's queue.
    pub(crate) session_dead_videos: std::collections::HashSet<String>,
}

// As this is a simple wrapper type we implement Copy for ease of handling
#[derive(Clone, PartialEq, Copy, Debug, PartialOrd, Hash, Eq, Serialize, Deserialize)]
pub struct ListSongID(#[cfg(test)] pub usize, #[cfg(not(test))] usize);

// As this is a simple wrapper type we implement Copy for ease of handling
#[derive(Clone, PartialEq, Copy, Debug, Default, PartialOrd, Serialize, Deserialize)]
pub struct Percentage(pub u8);

fn duration_string_to_secs(s: &str) -> usize {
    s.rsplit(':')
        .flat_map(|n| n.parse::<usize>().ok())
        .zip([1, 60, 3600])
        .fold(0, |acc, (time, multiplier)| acc + time * multiplier)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListSong {
    pub video_id: VideoID<'static>,
    pub track_no: Option<usize>,
    pub plays: String,
    pub title: String,
    pub explicit: Option<Explicit>,
    pub download_status: DownloadStatus,
    pub id: ListSongID,
    pub duration_string: String,
    pub actual_duration: Option<Duration>,
    #[serde(default)]
    pub duration_secs: usize,
    #[serde(skip)]
    pub title_lower: String,
    #[serde(skip)]
    pub album_lower: String,
    #[serde(skip)]
    pub artists_lower: String,
    #[serde(skip)]
    pub artists_string: String,
    #[serde(skip)]
    pub track_no_string: String,
    #[serde(default, skip)]
    pub resolution_checked: bool,
    pub year: Option<Rc<String>>,
    pub artists: MaybeRc<Vec<ListSongArtist>>,
    pub album: Option<MaybeRc<ListSongAlbum>>,
}

impl PartialEq for ListSong {
    fn eq(&self, other: &Self) -> bool {
        self.video_id == other.video_id
            && self.track_no == other.track_no
            && self.plays == other.plays
            && self.title == other.title
            && self.explicit == other.explicit
            && self.download_status == other.download_status
            && self.id == other.id
            && self.duration_string == other.duration_string
            && self.actual_duration == other.actual_duration
            && self.year == other.year
            && self.artists == other.artists
            && self.album == other.album
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListSongArtist {
    pub name: String,
    pub id: Option<ArtistOrUploadArtistID>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListSongAlbum {
    pub name: String,
    pub id: AlbumOrUploadAlbumID,
}

impl From<ParsedSongArtist> for ListSongArtist {
    fn from(value: ParsedSongArtist) -> Self {
        let ParsedSongArtist { name, id } = value;
        Self {
            name,
            id: id.map(ArtistOrUploadArtistID::Artist),
        }
    }
}

impl From<ParsedUploadArtist> for ListSongArtist {
    fn from(value: ParsedUploadArtist) -> Self {
        let ParsedUploadArtist { name, id } = value;
        Self {
            name,
            id: id.map(ArtistOrUploadArtistID::UploadArtist),
        }
    }
}

impl From<ParsedSongAlbum> for ListSongAlbum {
    fn from(value: ParsedSongAlbum) -> Self {
        let ParsedSongAlbum { name, id } = value;
        Self {
            name,
            id: AlbumOrUploadAlbumID::Album(id),
        }
    }
}

impl From<ParsedUploadSongAlbum> for ListSongAlbum {
    fn from(value: ParsedUploadSongAlbum) -> Self {
        let ParsedUploadSongAlbum { name, id } = value;
        Self {
            name,
            id: AlbumOrUploadAlbumID::UploadAlbum(id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ArtistOrUploadArtistID {
    Artist(ArtistChannelID<'static>),
    UploadArtist(UploadArtistID<'static>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AlbumOrUploadAlbumID {
    Album(AlbumID<'static>),
    UploadAlbum(UploadAlbumID<'static>),
}

#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListSongDisplayableField {
    DownloadStatus,
    TrackNo,
    Artists,
    Album,
    Song,
    Duration,
    Year,
    Plays,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ListStatus {
    New,
    Loading,
    InProgress,
    Loaded,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub enum DownloadStatus {
    #[default]
    None,
    Queued,
    Downloading(Percentage),
    Downloaded,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlayState {
    NotPlaying,
    Playing(ListSongID),
    Paused(ListSongID),
    Error(ListSongID),
    Buffering(ListSongID),
}

impl DownloadStatus {
    pub fn list_icon_str(&self) -> &'static str {
        match self {
            Self::Failed => "X",
            Self::Queued => "↓",
            Self::None => " ",
            Self::Downloading(_) => "↓",
            Self::Downloaded => "✓",
        }
    }
}

fn compute_artists_string(artists: &[ListSongArtist]) -> String {
    Itertools::intersperse(artists.iter().map(|a| a.name.as_str()), ", ").collect()
}

fn compute_lowercached(
    title: &str,
    album: Option<&str>,
    artists: &[ListSongArtist],
) -> (String, String, String) {
    let title_lower = title.to_lowercase();
    let album_lower = album.unwrap_or_default().to_lowercase();
    let artists_lower = compute_artists_string(artists).to_lowercase();
    (title_lower, album_lower, artists_lower)
}

impl ListSong {
    pub fn ensure_cached_fields(&mut self) {
        if self.artists_string.is_empty() {
            self.artists_string = compute_artists_string(&self.artists);
        }
        if self.track_no_string.is_empty() {
            self.track_no_string = self.track_no.map(|n| n.to_string()).unwrap_or_default();
        }
    }
    pub fn get_fields<const N: usize>(
        &self,
        fields: [ListSongDisplayableField; N],
    ) -> [Cow<'_, str>; N] {
        fields.map(|field| self.get_field(field))
    }
    pub fn get_field_lower<'a>(&'a self, field: ListSongDisplayableField) -> Cow<'a, str> {
        match field {
            ListSongDisplayableField::Song => Cow::Borrowed(&self.title_lower),
            ListSongDisplayableField::Artists => Cow::Borrowed(&self.artists_lower),
            ListSongDisplayableField::Album => Cow::Borrowed(&self.album_lower),
            // These fields are already ASCII (digits/formatted strings) — no lowering needed
            ListSongDisplayableField::Year
            | ListSongDisplayableField::Duration
            | ListSongDisplayableField::TrackNo
            | ListSongDisplayableField::Plays
            | ListSongDisplayableField::DownloadStatus => self.get_field(field),
        }
    }
    pub fn get_field(&self, field: ListSongDisplayableField) -> Cow<'_, str> {
        match field {
            ListSongDisplayableField::DownloadStatus => {
                Cow::Borrowed(self.download_status.list_icon_str())
            }
            ListSongDisplayableField::TrackNo => Cow::Borrowed(self.track_no_string.as_str()),
            ListSongDisplayableField::Artists => Cow::Borrowed(self.artists_string.as_str()),
            ListSongDisplayableField::Album => self
                .album
                .as_ref()
                .map(|album| album.as_ref().name.as_str())
                .unwrap_or_default()
                .into(),
            ListSongDisplayableField::Year => self
                .year
                .as_ref()
                .map(|year| year.as_str())
                .unwrap_or_default()
                .into(),
            ListSongDisplayableField::Song => self.title.as_str().into(),
            ListSongDisplayableField::Duration => self.duration_string.as_str().into(),
            ListSongDisplayableField::Plays => self.plays.as_str().into(),
        }
    }
    pub fn create_with_metadata(
        video_id: VideoID<'static>,
        title: String,
        artists: Vec<String>,
        album: Option<String>,
        duration_string: String,
    ) -> Self {
        let list_artists: Vec<ListSongArtist> = artists
            .iter()
            .map(|name| ListSongArtist {
                name: name.clone(),
                id: None,
            })
            .collect();
        let list_album = album.map(|name| {
            MaybeRc::Owned(ListSongAlbum {
                name,
                id: AlbumOrUploadAlbumID::Album(AlbumID::from_raw("")),
            })
        });
        let album_ref = list_album.as_ref().map(|a| a.as_ref().name.as_str());
        let artists_string = compute_artists_string(&list_artists);
        let (title_lower, album_lower, artists_lower) =
            compute_lowercached(&title, album_ref, &list_artists);
        ListSong {
            video_id,
            track_no: None,
            plays: String::new(),
            title,
            explicit: None,
            download_status: DownloadStatus::None,
            id: ListSongID(0),
            duration_secs: duration_string_to_secs(&duration_string),
            duration_string,
            actual_duration: None,
            title_lower,
            album_lower,
            artists_lower,
            artists_string,
            track_no_string: String::new(),
            resolution_checked: false,
            year: None,
            artists: MaybeRc::Owned(list_artists),
            album: list_album,
        }
    }
}

impl Default for BrowserSongsList {
    fn default() -> Self {
        BrowserSongsList {
            state: ListStatus::New,
            list: Vec::new(),
            next_id: ListSongID(0),
            session_dead_videos: std::collections::HashSet::new(),
        }
    }
}

impl BrowserSongsList {
    pub fn get_list_iter(&self) -> std::slice::Iter<'_, ListSong> {
        self.list.iter()
    }
    pub fn get_list_iter_mut(&mut self) -> std::slice::IterMut<'_, ListSong> {
        self.list.iter_mut()
    }
    pub fn sort(&mut self, field: ListSongDisplayableField, direction: SortDirection) {
        self.list.sort_by(|a, b| match direction {
            SortDirection::Asc => a
                .get_field(field)
                .partial_cmp(&b.get_field(field))
                .unwrap_or(std::cmp::Ordering::Equal),
            SortDirection::Desc => b
                .get_field(field)
                .partial_cmp(&a.get_field(field))
                .unwrap_or(std::cmp::Ordering::Equal),
        });
    }
    pub fn clear(&mut self) {
        // We can't reset the ID, so it's left out and we'll keep incrementing.
        self.state = ListStatus::New;
        self.list.clear();
    }
    pub fn append_raw_album_songs(
        &mut self,
        raw_list: Vec<AlbumSong>,
        album: ParsedSongAlbum,
        year: String,
        artists: Vec<ParsedSongArtist>,
    ) {
        use std::collections::hash_map::Entry;
        for song in &raw_list {
            debug!(
                "album_song: title={:?} video_id={:?} duration={:?} mvt={:?}",
                song.title,
                song.video_id.get_raw(),
                song.duration,
                song.music_video_type()
            );
        }
        let mut best: std::collections::HashMap<&str, &AlbumSong> =
            std::collections::HashMap::new();
        for song in &raw_list {
            match best.entry(song.title.as_str()) {
                Entry::Occupied(mut e) => {
                    if song.is_audio_track() && !e.get().is_audio_track() {
                        e.insert(song);
                    }
                }
                Entry::Vacant(e) => {
                    e.insert(song);
                }
            }
        }
        let year = Rc::new(year);
        let album = Rc::new(ListSongAlbum::from(album));
        let artists = Rc::new(artists.into_iter().map(Into::into).collect::<Vec<_>>());
        for song in best.into_values() {
            self.add_raw_album_song(
                song.clone(),
                album.clone(),
                year.clone(),
                artists.clone(),
            );
        }
    }
    pub fn append_raw_playlist_items(&mut self, raw_list: Vec<PlaylistItem>) {
        for song in raw_list {
            if self.add_raw_playlist_item(song).is_none() {
                warn!("Skipped unsupported playlist item");
            }
        }
    }
    pub fn append_raw_search_result_songs(&mut self, raw_list: Vec<SearchResultSong>) {
        for song in &raw_list {
            debug!(
                "search_song: title={:?} artist={:?} video_id={:?} duration={:?} mvt={:?}",
                song.title,
                song.artist,
                song.video_id.get_raw(),
                song.duration,
                song.music_video_type()
            );
        }
        use std::collections::hash_map::Entry;
        let mut best: std::collections::HashMap<(&str, &str), &SearchResultSong> =
            std::collections::HashMap::new();
        for song in &raw_list {
            let key = (song.title.as_str(), song.artist.as_str());
            match best.entry(key) {
                Entry::Occupied(mut e) => {
                    if song.is_audio_track() && !e.get().is_audio_track() {
                        e.insert(song);
                    }
                }
                Entry::Vacant(e) => {
                    e.insert(song);
                }
            }
        }
        for song in best.into_values() {
            self.add_raw_search_result_song(song.clone());
        }
    }
    pub fn add_raw_album_song(
        &mut self,
        song: AlbumSong,
        album: Rc<ListSongAlbum>,
        year: Rc<String>,
        artists: Rc<Vec<ListSongArtist>>,
    ) -> ListSongID {
        let id = self.create_next_id();
        let AlbumSong {
            video_id,
            track_no,
            duration,
            plays,
            title,
            explicit,
            ..
        } = song;
        let artists_string = compute_artists_string(&artists);
        let track_no_string = track_no.to_string();
        let (title_lower, album_lower, artists_lower) =
            compute_lowercached(&title, Some(&album.name), &artists);
        self.list.push(ListSong {
            download_status: DownloadStatus::None,
            id,
            year: Some(year),
            artists: MaybeRc::Rc(artists),
            album: Some(MaybeRc::Rc(album)),
            actual_duration: None,
            duration_secs: duration_string_to_secs(&duration),
            video_id,
            track_no: Some(track_no),
            plays,
            title,
            explicit: Some(explicit),
            duration_string: duration,
            title_lower,
            album_lower,
            artists_lower,
            artists_string,
            track_no_string,
            resolution_checked: false,
        });
        id
    }
    pub fn add_raw_search_result_song(&mut self, song: SearchResultSong) -> ListSongID {
        let id = self.create_next_id();
        let SearchResultSong {
            title,
            artist,
            album,
            duration,
            plays,
            explicit,
            video_id,
            ..
        } = song;
        let search_album = album.map(Into::<ListSongAlbum>::into).map(MaybeRc::Owned);
        let search_artists = vec![ListSongArtist {
            name: artist,
            id: None,
        }];
        let artists_string = compute_artists_string(&search_artists);
        let track_no_string = String::new();
        let (title_lower, album_lower, artists_lower) = compute_lowercached(
            &title,
            search_album.as_ref().map(|a| a.as_ref().name.as_str()),
            &search_artists,
        );
        self.list.push(ListSong {
            download_status: DownloadStatus::None,
            id,
            year: None,
            artists: MaybeRc::Owned(search_artists),
            album: search_album,
            actual_duration: None,
            duration_secs: duration_string_to_secs(&duration),
            video_id,
            track_no: None,
            plays,
            title,
            explicit: Some(explicit),
            duration_string: duration,
            title_lower,
            album_lower,
            artists_lower,
            artists_string,
            track_no_string,
            resolution_checked: false,
        });
        id
    }
    fn add_raw_playlist_item(&mut self, item: PlaylistItem) -> Option<ListSongID> {
        let id = self.create_next_id();
        let (track_no, title, video_id, duration, artists, album, explicit) = match item
        {
            PlaylistItem::Song(PlaylistSong {
                video_id,
                album,
                duration,
                title,
                artists,
                track_no,
                explicit,
                ..
            }) => (
                track_no,
                title,
                video_id,
                duration,
                artists.into_iter().map(Into::into).collect(),
                Some(album.into()),
                Some(explicit),
            ),
            PlaylistItem::Video(PlaylistVideo {
                video_id,
                duration,
                title,
                track_no,
                ..
            }) => (
                track_no,
                title,
                video_id,
                duration,
                vec![],
                None,
                None,
            ),
            // Episode has no video id, so we can't currently handle it as a ListSong.
            PlaylistItem::Episode(PlaylistEpisode { .. }) => {
                warn!("Skipping podcast episode — no video_id, cannot represent as ListSong");
                return None;
            }
            PlaylistItem::UploadSong(PlaylistUploadSong {
                video_id,
                duration,
                title,
                artists,
                album,
                track_no,
                ..
            }) => (
                track_no,
                title,
                video_id,
                duration,
                artists.into_iter().map(Into::into).collect(),
                album.map(Into::into),
                None,
            ),
        };
        let artists_string = compute_artists_string(&artists);
        let track_no_string = track_no.to_string();
        let (title_lower, album_lower, artists_lower) = compute_lowercached(
            &title,
            album.as_ref().map(|a: &ListSongAlbum| a.name.as_str()),
            &artists,
        );
        self.list.push(ListSong {
            download_status: DownloadStatus::None,
            id,
            year: None,
            artists: MaybeRc::Owned(artists),
            album: album.map(MaybeRc::Owned),
            actual_duration: None,
            duration_secs: duration_string_to_secs(&duration),
            video_id,
            track_no: Some(track_no),
            plays: String::new(),
            title,
            explicit,
            duration_string: duration,
            title_lower,
            album_lower,
            artists_lower,
            artists_string,
            track_no_string,
            resolution_checked: false,
        });
        Some(id)
    }
    pub fn push_song_list(&mut self, song_list: Vec<ListSong>) -> ListSongID {
        // Use owned String set to avoid borrow-vs-move conflicts.
        // Filters both against existing list AND within the incoming batch
        // (keeps first occurrence of each video_id).
        let mut filtered = {
            let mut existing: std::collections::HashSet<String> = self
                .list
                .iter()
                .map(|s| s.video_id.get_raw().to_owned())
                .collect();
            let mut filtered = Vec::with_capacity(song_list.len());
            for song in song_list {
                let raw = song.video_id.get_raw().to_owned();
                if existing.contains(&raw) {
                    continue;
                }
                existing.insert(raw);
                filtered.push(song);
            }
            filtered
        };
        if filtered.is_empty() {
            return self.next_id;
        }
        let first_id = self.create_next_id();
        let mut first = filtered.remove(0);
        first.id = first_id;
        first.ensure_cached_fields();
        self.list.push(first);
        for mut song in filtered {
            song.id = self.create_next_id();
            song.ensure_cached_fields();
            self.list.push(song);
        }
        first_id
    }
    /// Safely deletes the song at index if it exists, and returns it.
    pub fn remove_song_index(&mut self, idx: usize) -> Option<ListSong> {
        // Guard against index out of bounds
        if self.list.len() <= idx {
            return None;
        }
        Some(self.list.remove(idx))
    }
    /// Remove songs with duplicate `video_id`s, keeping the first occurrence.
    /// Returns the number of duplicates removed.
    pub fn deduplicate(&mut self) -> usize {
        let len_before = self.list.len();
        if len_before < 2 {
            return 0;
        }
        // O(n) HashSet scan instead of O(n²) Vec::contains.
        // We collect indices to remove, then remove in reverse to avoid
        // O(n) shifts per removal (each remove is O(1) amortized from back).
        let (to_remove, _): (Vec<usize>, std::collections::HashSet<&str>) = {
            let mut seen = std::collections::HashSet::with_capacity(len_before);
            let mut to_remove = Vec::with_capacity(len_before);
            for (i, song) in self.list.iter().enumerate() {
                let raw = song.video_id.get_raw();
                if seen.contains(raw) {
                    to_remove.push(i);
                } else {
                    seen.insert(raw);
                }
            }
            (to_remove, seen)
        };
        if to_remove.is_empty() {
            return 0;
        }
        for i in to_remove.into_iter().rev() {
            self.list.remove(i);
        }
        let removed = len_before - self.list.len();
        if removed > 0 {
            info!("Removed {removed} duplicates from playlist");
        }
        removed
    }

    pub fn create_next_id(&mut self) -> ListSongID {
        let id = self.next_id;
        self.next_id.0 += 1;
        id
    }
    pub fn get_song_from_idx(&self, idx: usize) -> Option<&ListSong> {
        self.list.get(idx)
    }

    #[cfg(test)]
    /// Bypasses dedup filtering for direct injection into self.list.
    /// Used by tests that need specific duplicate arrangements.
    pub fn push_songs_direct(&mut self, songs: Vec<ListSong>) {
        for mut song in songs {
            song.id = self.create_next_id();
            song.ensure_cached_fields();
            self.list.push(song);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytmapi_rs::common::VideoID;

    fn song(id: &str) -> ListSong {
        ListSong::create_with_metadata(
            VideoID::from_raw(id.to_owned()),
            "Song".into(),
            vec!["Artist".into()],
            None,
            "3:00".into(),
        )
    }

    fn songs(ids: &[&str]) -> Vec<ListSong> {
        ids.iter().map(|id| song(id)).collect()
    }

    fn collect_ids(list: &BrowserSongsList) -> Vec<String> {
        list.get_list_iter()
            .map(|s| s.video_id.get_raw().to_string())
            .collect()
    }

    // --- deduplicate tests ---
    //
    // NOTE: These use push_songs_direct() to inject duplicates bypassing
    // push_song_list's within-batch dedup filtering. deduplicate() is the
    // safety net for data that entered the list before filtering existed
    // (e.g. legacy autosave files).

    #[test]
    fn dedup_empty_returns_0() {
        let mut list = BrowserSongsList::default();
        assert_eq!(list.deduplicate(), 0);
        assert_eq!(collect_ids(&list).len(), 0);
    }

    #[test]
    fn dedup_single_returns_0() {
        let mut list = BrowserSongsList::default();
        list.push_songs_direct(songs(&["a"]));
        assert_eq!(list.deduplicate(), 0);
        assert_eq!(collect_ids(&list), vec!["a"]);
    }

    #[test]
    fn dedup_no_duplicates_returns_0() {
        let mut list = BrowserSongsList::default();
        list.push_songs_direct(songs(&["a", "b", "c"]));
        assert_eq!(list.deduplicate(), 0);
        assert_eq!(collect_ids(&list), vec!["a", "b", "c"]);
    }

    #[test]
    fn dedup_adjacent_duplicates_keeps_first() {
        let mut list = BrowserSongsList::default();
        list.push_songs_direct(songs(&["a", "a", "b"]));
        assert_eq!(list.deduplicate(), 1);
        assert_eq!(collect_ids(&list), vec!["a", "b"]);
    }

    #[test]
    fn dedup_non_adjacent_duplicates_keeps_first() {
        let mut list = BrowserSongsList::default();
        list.push_songs_direct(songs(&["a", "b", "a"]));
        assert_eq!(list.deduplicate(), 1);
        assert_eq!(collect_ids(&list), vec!["a", "b"]);
    }

    #[test]
    fn dedup_all_same_keeps_one() {
        let mut list = BrowserSongsList::default();
        list.push_songs_direct(songs(&["a", "a", "a", "a"]));
        assert_eq!(list.deduplicate(), 3);
        assert_eq!(collect_ids(&list), vec!["a"]);
    }

    #[test]
    fn dedup_multiple_distinct_duplicates() {
        let mut list = BrowserSongsList::default();
        list.push_songs_direct(songs(&["a", "b", "a", "c", "b"]));
        assert_eq!(list.deduplicate(), 2);
        assert_eq!(collect_ids(&list), vec!["a", "b", "c"]);
    }

    #[test]
    fn dedup_interleaved_keeps_first_of_each() {
        let mut list = BrowserSongsList::default();
        list.push_songs_direct(songs(&["a", "b", "a", "b"]));
        assert_eq!(list.deduplicate(), 2);
        assert_eq!(collect_ids(&list), vec!["a", "b"]);
    }

    #[test]
    fn dedup_idempotent() {
        let mut list = BrowserSongsList::default();
        list.push_songs_direct(songs(&["a", "b", "a", "c", "b"]));
        list.deduplicate();
        // Second call should remove nothing
        assert_eq!(list.deduplicate(), 0);
        assert_eq!(collect_ids(&list), vec!["a", "b", "c"]);
    }

    // --- push_song_list dedup filter tests ---

    #[test]
    fn push_song_list_empty_to_empty_adds_nothing() {
        let mut list = BrowserSongsList::default();
        list.push_song_list(vec![]);
        assert_eq!(collect_ids(&list).len(), 0);
    }

    #[test]
    fn push_song_list_no_overlap_appends_all() {
        let mut list = BrowserSongsList::default();
        list.push_song_list(songs(&["a", "b"]));
        list.push_song_list(songs(&["c", "d"]));
        assert_eq!(collect_ids(&list), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn push_song_list_partial_overlap_skips_dupes() {
        let mut list = BrowserSongsList::default();
        list.push_song_list(songs(&["a", "b"]));
        let id = list.push_song_list(songs(&["b", "c"]));
        // Returns id for 'c' (first of the filtered batch)
        assert!(id.0 > 0);
        assert_eq!(collect_ids(&list), vec!["a", "b", "c"]);
    }

    #[test]
    fn push_song_list_full_overlap_skips_all() {
        let mut list = BrowserSongsList::default();
        list.push_song_list(songs(&["a", "b"]));
        let len_before = list.get_list_iter().count();
        let id = list.push_song_list(songs(&["a", "b"]));
        // No new song added, list unchanged
        assert_eq!(list.get_list_iter().count(), len_before);
        assert_eq!(collect_ids(&list), vec!["a", "b"]);
        // Returned id is a valid next_id
        assert_ne!(id, ListSongID(0));
    }

    #[test]
    fn push_song_list_all_incoming_dupes_list_unchanged() {
        let mut list = BrowserSongsList::default();
        list.push_song_list(songs(&["a"]));
        let len_before = list.get_list_iter().count();
        let id = list.push_song_list(vec![song("a")]);
        assert_eq!(list.get_list_iter().count(), len_before);
        assert_eq!(collect_ids(&list), vec!["a"]);
        assert!(id.0 > 0);
    }

    #[test]
    fn push_song_list_dupes_in_incoming_only_keeps_first() {
        let mut list = BrowserSongsList::default();
        list.push_song_list(songs(&["a", "a", "b"]));
        assert_eq!(collect_ids(&list), vec!["a", "b"]);
    }

    #[test]
    fn push_song_list_dupes_in_incoming_with_existing() {
        let mut list = BrowserSongsList::default();
        list.push_song_list(songs(&["a"]));
        let id = list.push_song_list(songs(&["a", "b", "a"]));
        assert!(id.0 > 0);
        assert_eq!(collect_ids(&list), vec!["a", "b"]);
    }

    /// 58k unique songs: verify O(n) dedup completes in < 500ms (would take
    /// minutes with O(n²)).
    #[test]
    fn dedup_58k_unique_performance() {
        let mut list = BrowserSongsList::default();
        let many: Vec<ListSong> = (0..58_000u32)
            .map(|i| song(&format!("video_{i}")))
            .collect();
        list.push_song_list(many);
        let start = std::time::Instant::now();
        let removed = list.deduplicate();
        let elapsed = start.elapsed();
        assert_eq!(removed, 0);
        assert!(
            elapsed.as_millis() < 500,
            "dedup(58000) took {}ms, expected <500ms for O(n)",
            elapsed.as_millis(),
        );
        eprintln!("[PERF] dedup(58000 unique): {}ms", elapsed.as_millis(),);
    }

    /// Verify push_song_list with 58k songs doesn't O(n²) on existing lookup
    #[test]
    fn push_song_list_58k_no_scan_regression() {
        let mut list = BrowserSongsList::default();
        let batch1: Vec<ListSong> = (0..58_000u32)
            .map(|i| song(&format!("video_{i}")))
            .collect();
        let batch2: Vec<ListSong> = (0..58_000u32)
            .map(|i| song(&format!("video_{}", i + 58_000)))
            .collect();
        list.push_song_list(batch1);
        let start = std::time::Instant::now();
        list.push_song_list(batch2);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 1000,
            "push_song_list(58000 existing, 58000 new) took {}ms, expected <1000ms",
            elapsed.as_millis(),
        );
        eprintln!(
            "[PERF] push_song_list(58000 existing, 58000 new): {}ms",
            elapsed.as_millis(),
        );
        assert_eq!(list.get_list_iter().count(), 116_000);
    }
}

#[cfg(test)]
#[cfg(not(debug_assertions))]
mod bench {
    use super::*;
    use ytmapi_rs::common::VideoID;

    const ITERATIONS: usize = 10_000;

    fn make_browser_songs(count: usize) -> Vec<ListSong> {
        (0..count)
            .map(|i| {
                ListSong::create_with_metadata(
                    VideoID::from_raw(format!("video_{i}")),
                    format!("Song {i}"),
                    vec!["Artist A".into(), "Artist B".into()],
                    Some("Album".into()),
                    "3:30".into(),
                )
            })
            .collect()
    }

    #[test]
    fn bench_get_field_artists_cached() {
        let songs = make_browser_songs(100);
        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            for song in &songs {
                let result = song.get_field(ListSongDisplayableField::Artists);
                std::hint::black_box(&result);
            }
        }
        let elapsed = start.elapsed();
        let total_ns = elapsed.as_nanos() as f64;
        let per_call = total_ns / (ITERATIONS as f64 * songs.len() as f64);
        let total_ms = elapsed.as_secs_f64() * 1000.0;
        assert!(
            per_call < 100.0,
            "get_field(Artists) too slow: {per_call:.1}ns/call"
        );
        eprintln!(
            "[BENCH] get_field(Artists) — {ITERATIONS}×{} songs: {total_ms:.3}ms total, ~{per_call:.1}ns/call",
            songs.len()
        );
    }

    #[test]
    fn bench_get_field_track_no_cached() {
        let songs = make_browser_songs(100);
        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            for song in &songs {
                let result = song.get_field(ListSongDisplayableField::TrackNo);
                std::hint::black_box(&result);
            }
        }
        let elapsed = start.elapsed();
        let total_ns = elapsed.as_nanos() as f64;
        let per_call = total_ns / (ITERATIONS as f64 * songs.len() as f64);
        let total_ms = elapsed.as_secs_f64() * 1000.0;
        assert!(
            per_call < 100.0,
            "get_field(TrackNo) too slow: {per_call:.1}ns/call"
        );
        eprintln!(
            "[BENCH] get_field(TrackNo) — {ITERATIONS}×{} songs: {total_ms:.3}ms total, ~{per_call:.1}ns/call",
            songs.len()
        );
    }

    #[test]
    fn bench_get_fields_4col() {
        let songs = make_browser_songs(100);
        let fields = [
            ListSongDisplayableField::Song,
            ListSongDisplayableField::Artists,
            ListSongDisplayableField::Album,
            ListSongDisplayableField::Duration,
        ];
        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            for song in &songs {
                let result = song.get_fields(fields);
                std::hint::black_box(&result);
            }
        }
        let elapsed = start.elapsed();
        let total_ns = elapsed.as_nanos() as f64;
        let per_call = total_ns / (ITERATIONS as f64 * songs.len() as f64);
        let total_ms = elapsed.as_secs_f64() * 1000.0;
        assert!(
            per_call < 1000.0,
            "get_fields(4col) too slow: {per_call:.1}ns/call"
        );
        eprintln!(
            "[BENCH] get_fields(4col) — {ITERATIONS}×{} songs: {total_ms:.3}ms total, ~{per_call:.1}ns/call",
            songs.len()
        );
    }

    #[test]
    fn bench_compute_lowercached() {
        let artists = vec![
            ListSongArtist {
                name: "Artist A".into(),
                id: None,
            },
            ListSongArtist {
                name: "Artist B".into(),
                id: None,
            },
        ];
        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            let result = compute_lowercached("Song Title", Some("Album Name"), &artists);
            std::hint::black_box(&result);
        }
        let elapsed = start.elapsed();
        let total_ns = elapsed.as_nanos() as f64;
        let per_call = total_ns / ITERATIONS as f64;
        let total_ms = elapsed.as_secs_f64() * 1000.0;
        assert!(
            per_call < 100_000.0,
            "compute_lowercached too slow: {per_call:.1}ns/call"
        );
        eprintln!(
            "[BENCH] compute_lowercached — {ITERATIONS}×: {total_ms:.3}ms total, ~{per_call:.1}ns/call"
        );
    }

    #[test]
    fn bench_create_with_metadata() {
        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            let result = ListSong::create_with_metadata(
                VideoID::from_raw("video_id"),
                "Song Title".into(),
                vec!["Artist A".into(), "Artist B".into()],
                Some("Album".into()),
                "3:30".into(),
            );
            std::hint::black_box(&result);
        }
        let elapsed = start.elapsed();
        let total_ns = elapsed.as_nanos() as f64;
        let per_call = total_ns / ITERATIONS as f64;
        let total_ms = elapsed.as_secs_f64() * 1000.0;
        assert!(
            per_call < 1_000_000.0,
            "create_with_metadata too slow: {per_call:.1}ns/call"
        );
        eprintln!(
            "[BENCH] create_with_metadata — {ITERATIONS}×: {total_ms:.3}ms total, ~{per_call:.1}ns/call"
        );
    }
}

#[cfg(all(test, not(debug_assertions)))]
mod criterion_benches {
    use super::*;
    use criterion::Criterion;
    use ytmapi_rs::common::VideoID;

    fn make_songs(count: usize) -> Vec<ListSong> {
        (0..count)
            .map(|i| {
                ListSong::create_with_metadata(
                    VideoID::from_raw(format!("video_{i}")),
                    format!("Song {i}"),
                    vec!["Artist A".into(), "Artist B".into()],
                    Some("Album".into()),
                    "3:30".into(),
                )
            })
            .collect()
    }

    #[test]
    fn criterion_get_field_hot_path() {
        let songs = make_songs(100);
        let mut c = Criterion::default();

        c.bench_function("get_field/artists_cached", |b| {
            b.iter(|| {
                for song in &songs {
                    std::hint::black_box(song.get_field(ListSongDisplayableField::Artists));
                }
            });
        });

        c.bench_function("get_field/track_no_cached", |b| {
            b.iter(|| {
                for song in &songs {
                    std::hint::black_box(song.get_field(ListSongDisplayableField::TrackNo));
                }
            });
        });

        let fields_4 = [
            ListSongDisplayableField::Song,
            ListSongDisplayableField::Artists,
            ListSongDisplayableField::Album,
            ListSongDisplayableField::Duration,
        ];
        c.bench_function("get_fields/4col", |b| {
            b.iter(|| {
                for song in &songs {
                    std::hint::black_box(song.get_fields(fields_4));
                }
            });
        });

        let fields_7 = [
            ListSongDisplayableField::DownloadStatus,
            ListSongDisplayableField::TrackNo,
            ListSongDisplayableField::Artists,
            ListSongDisplayableField::Album,
            ListSongDisplayableField::Song,
            ListSongDisplayableField::Duration,
            ListSongDisplayableField::Year,
        ];
        c.bench_function("get_fields/7col", |b| {
            b.iter(|| {
                for song in &songs {
                    std::hint::black_box(song.get_fields(fields_7));
                }
            });
        });

        c.bench_function("create_with_metadata", |b| {
            b.iter(|| {
                std::hint::black_box(ListSong::create_with_metadata(
                    VideoID::from_raw("video_id"),
                    "Song Title".into(),
                    vec!["Artist A".into(), "Artist B".into()],
                    Some("Album".into()),
                    "3:30".into(),
                ));
            });
        });
    }
}
