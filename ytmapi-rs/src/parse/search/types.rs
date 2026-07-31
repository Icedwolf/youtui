use crate::common::{
    AlbumID, AlbumType, ArtistChannelID, EpisodeID, Explicit, PlaylistID, PodcastID, Thumbnail,
    UserChannelID, VideoID,
};
use crate::parse::{EpisodeDate, ParsedSongAlbum};
use crate::youtube_enums::YoutubeMusicVideoType;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResults {
    pub top_results: Vec<TopResult>,
    pub artists: Vec<SearchResultArtist>,
    pub albums: Vec<SearchResultAlbum>,
    pub featured_playlists: Vec<SearchResultFeaturedPlaylist>,
    pub community_playlists: Vec<BasicSearchResultCommunityPlaylist>,
    pub songs: Vec<SearchResultSong>,
    pub videos: Vec<SearchResultVideo>,
    pub podcasts: Vec<SearchResultPodcast>,
    pub episodes: Vec<SearchResultEpisode>,
    pub profiles: Vec<SearchResultProfile>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TopResultType {
    Artist,
    Playlist,
    Song,
    Video,
    Station,
    Podcast,
    #[serde(untagged)]
    Album(AlbumType),
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SearchResultType {
    TopResult,
    Artists,
    Albums,
    FeaturedPlaylists,
    CommunityPlaylists,
    Songs,
    Videos,
    Podcasts,
    Episodes,
    Profiles,
    Unknown,
}

impl<'de> serde::Deserialize<'de> for SearchResultType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "Top result" | "TopResult" => SearchResultType::TopResult,
            "Artists" => SearchResultType::Artists,
            "Albums" => SearchResultType::Albums,
            "Featured playlists" | "FeaturedPlaylists" => SearchResultType::FeaturedPlaylists,
            "Community playlists" | "CommunityPlaylists" => SearchResultType::CommunityPlaylists,
            "Songs" => SearchResultType::Songs,
            "Videos" => SearchResultType::Videos,
            "Podcasts" => SearchResultType::Podcasts,
            "Episodes" => SearchResultType::Episodes,
            "Profiles" => SearchResultType::Profiles,
            _ => SearchResultType::Unknown,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TopResult {
    pub result_name: String,
    pub result_type: Option<TopResultType>,
    pub thumbnails: Vec<Thumbnail>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<String>,
    pub year: Option<String>,
    pub subscribers: Option<String>,
    pub plays: Option<String>,
    pub publisher: Option<String>,
    pub byline: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResultArtist {
    pub artist: String,
    pub subscribers: Option<String>,
    pub browse_id: ArtistChannelID<'static>,
    pub thumbnails: Vec<Thumbnail>,
}

impl SearchResultArtist {
    #[must_use]
    pub fn new(
        artist: String,
        subscribers: Option<String>,
        browse_id: ArtistChannelID<'static>,
        thumbnails: Vec<Thumbnail>,
    ) -> Self {
        Self {
            artist,
            subscribers,
            browse_id,
            thumbnails,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResultPodcast {
    pub title: String,
    pub publisher: String,
    pub podcast_id: PodcastID<'static>,
    pub thumbnails: Vec<Thumbnail>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResultEpisode {
    pub title: String,
    pub date: EpisodeDate,
    pub channel_name: String,
    pub episode_id: EpisodeID<'static>,
    pub thumbnails: Vec<Thumbnail>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SearchResultVideo {
    #[non_exhaustive]
    Video {
        title: String,
        channel_name: String,
        video_id: VideoID<'static>,
        views: String,
        length: String,
        thumbnails: Vec<Thumbnail>,
    },
    #[non_exhaustive]
    VideoEpisode {
        title: String,
        date: EpisodeDate,
        channel_name: String,
        episode_id: EpisodeID<'static>,
        thumbnails: Vec<Thumbnail>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResultProfile {
    pub title: String,
    pub username: String,
    pub profile_id: UserChannelID<'static>,
    pub thumbnails: Vec<Thumbnail>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResultAlbum {
    pub title: String,
    pub artist: String,
    pub year: String,
    pub explicit: Explicit,
    pub album_id: AlbumID<'static>,
    pub album_type: AlbumType,
    pub thumbnails: Vec<Thumbnail>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResultSong {
    pub title: String,
    pub artist: String,
    pub album: Option<ParsedSongAlbum>,
    pub duration: String,
    pub plays: String,
    pub explicit: Explicit,
    pub video_id: VideoID<'static>,
    pub thumbnails: Vec<Thumbnail>,
    pub(crate) music_video_type: Option<YoutubeMusicVideoType>,
}

impl std::fmt::Debug for SearchResultSong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("SearchResultSong");
        s.field("title", &self.title);
        s.field("artist", &self.artist);
        s.field("album", &self.album);
        s.field("duration", &self.duration);
        s.field("plays", &self.plays);
        s.field("explicit", &self.explicit);
        s.field("video_id", &self.video_id);
        s.field("thumbnails", &self.thumbnails);
        s.field("music_video_type", &self.music_video_type);
        s.finish()
    }
}

impl SearchResultSong {
    pub fn is_audio_track(&self) -> bool {
        self.music_video_type == Some(YoutubeMusicVideoType::Atv)
    }
    pub fn music_video_type(&self) -> Option<&YoutubeMusicVideoType> {
        self.music_video_type.as_ref()
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SearchResultPlaylist {
    Featured(SearchResultFeaturedPlaylist),
    Community(SearchResultCommunityPlaylist),
    Podcast(SearchResultPodcast),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BasicSearchResultCommunityPlaylist {
    Podcast(SearchResultPodcast),
    Playlist(SearchResultCommunityPlaylist),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResultCommunityPlaylist {
    pub title: String,
    pub author: String,
    pub views: String,
    pub playlist_id: PlaylistID<'static>,
    pub thumbnails: Vec<Thumbnail>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResultFeaturedPlaylist {
    pub title: String,
    pub author: String,
    pub songs: String,
    pub playlist_id: PlaylistID<'static>,
    pub thumbnails: Vec<Thumbnail>,
}
