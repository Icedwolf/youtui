use super::{
    DISPLAY_POLICY, ParseFrom, ProcessedResult, flex_column_item_pointer, parse_flex_column_item,
};
use crate::common::{ContinuationParams, Explicit, SearchSuggestion, SuggestionType, TextRun, Thumbnail};
use crate::continuations::ParseFromContinuable;
use crate::nav_consts::*;
use crate::query::search::UnfilteredSearchType;
use crate::query::search::filteredsearch::{
    AlbumsFilter, ArtistsFilter, CommunityPlaylistsFilter, EpisodesFilter, FeaturedPlaylistsFilter,
    FilteredSearch, FilteredSearchType, PlaylistsFilter, PodcastsFilter, ProfilesFilter,
    SongsFilter, VideosFilter,
};
use crate::query::*;
use crate::parse::{EpisodeDate, ParsedSongAlbum};
use crate::youtube_enums::{PlaylistEndpointParams, YoutubeMusicPageType, YoutubeMusicVideoType};
use crate::{Error, Result};
use const_format::concatcp;
use itertools::Itertools;
use json_crawler::{JsonCrawler, JsonCrawlerBorrowed, JsonCrawlerIterator, JsonCrawlerOwned};
use serde::de::IntoDeserializer;
use serde::Deserialize;

mod types;
pub use types::*;

#[cfg(test)]
mod tests;

// TODO: Type safety
fn parse_basic_search_result_from_section_list_contents(
    mut section_list_contents: BasicSearchSectionListContents,
) -> Result<SearchResults> {
    // Imperative solution, may be able to make more functional.
    let mut top_results = Vec::new();
    let mut artists = Vec::new();
    let mut albums = Vec::new();
    let mut featured_playlists = Vec::new();
    let community_playlists = Vec::new();
    let mut songs = Vec::new();
    let videos = Vec::new();
    let podcasts = Vec::new();
    let episodes = Vec::new();
    let profiles = Vec::new();

    let music_card_shelf = section_list_contents
        .0
        .try_iter_mut()?
        .find_path(MUSIC_CARD_SHELF)
        .ok();
    if let Some(music_card_shelf) = music_card_shelf {
        top_results = parse_top_results_from_music_card_shelf_contents(music_card_shelf)?
    }
    let results_iter = section_list_contents
        .0
        .try_into_iter()?
        .filter_map(|item| item.navigate_pointer(MUSIC_SHELF).ok());

    for mut category in results_iter {
        match category.take_value_pointer::<SearchResultType>(TITLE_TEXT)? {
            SearchResultType::TopResult => {
                top_results = category
                    .navigate_pointer("/contents")?
                    .try_iter_mut()?
                    .filter_map(|r| parse_top_result_from_music_shelf_contents(r).transpose())
                    .collect::<Result<Vec<TopResult>>>()?;
            }
            // TODO: Use a navigation constant
            SearchResultType::Artists => {
                artists = category
                    .navigate_pointer("/contents")?
                    .try_iter_mut()?
                    .filter_map(|r| parse_artist_search_result_from_music_shelf_contents(r).ok())
                    .collect();
            }
            SearchResultType::Albums => {
                albums = category
                    .navigate_pointer("/contents")?
                    .try_iter_mut()?
                    .map(|r| parse_album_search_result_from_music_shelf_contents(r))
                    .collect::<Result<Vec<SearchResultAlbum>>>()?
            }
            SearchResultType::FeaturedPlaylists => {
                featured_playlists = category
                    .navigate_pointer("/contents")?
                    .try_iter_mut()?
                    .map(|r| parse_featured_playlist_search_result_from_music_shelf_contents(r))
                    .collect::<Result<Vec<SearchResultFeaturedPlaylist>>>()?
            }
            SearchResultType::Songs => {
                songs = category
                    .navigate_pointer("/contents")?
                    .try_iter_mut()?
                    .map(|r| parse_song_search_result_from_music_shelf_contents(r))
                    .collect::<Result<Vec<SearchResultSong>>>()?
            }
            // Non-music categories silently dropped — music-only client.
            SearchResultType::CommunityPlaylists
            | SearchResultType::Videos
            | SearchResultType::Podcasts
            | SearchResultType::Episodes
            | SearchResultType::Profiles
            | SearchResultType::Unknown => {}
        }
    }
    Ok(SearchResults {
        top_results,
        artists,
        albums,
        featured_playlists,
        community_playlists,
        songs,
        videos,
        podcasts,
        episodes,
        profiles,
    })
}

fn parse_top_results_from_music_card_shelf_contents(
    mut music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<Vec<TopResult>> {
    let mut results = Vec::new();
    // Begin - first result parsing
    let result_name = music_shelf_contents.take_value_pointer(TITLE_TEXT)?;
    // NOTE: Parse this before value at SUBTITLE is taken (below).
    let result_type = music_shelf_contents
        .borrow_value_pointer::<TopResultType>(SUBTITLE)
        .ok();
    let subtitle: String = music_shelf_contents.take_value_pointer(SUBTITLE)?;
    let subtitle_2: Option<String> = music_shelf_contents.take_value_pointer(SUBTITLE2).ok();
    // Possibly artists only.
    let subscribers = subtitle_2;
    let byline = match result_type {
        Some(_) => None,
        None => Some(subtitle),
    };
    // Imperative solution, may be able to make more functional.
    let publisher = None;
    let artist = None;
    let album = None;
    let duration = None;
    let year = None;
    let plays = None;
    let thumbnails: Vec<Thumbnail> = music_shelf_contents.take_value_pointer(THUMBNAILS)?;
    let first_result = TopResult {
        // Assuming that in non-card case top result always has a result type.
        result_type,
        subscribers,
        thumbnails,
        result_name,
        publisher,
        artist,
        album,
        duration,
        year,
        plays,
        byline,
    };
    // End - first result parsing.
    results.push(first_result);
    // Other results may not exist.
    if let Ok(mut contents) = music_shelf_contents.navigate_pointer("/contents") {
        contents
            .try_iter_mut()?
            .filter_map(|r| parse_top_result_from_music_shelf_contents(r).transpose())
            .try_for_each(|r| -> Result<()> {
                results.push(r?);
                Ok(())
            })?;
    }
    Ok(results)
}
// TODO: Tests
fn parse_top_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<Option<TopResult>> {
    // This is the "More from YouTube" seperator
    if music_shelf_contents.path_exists("/messageRenderer") {
        return Ok(None);
    };
    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    let result_name = parse_flex_column_item(&mut mrlir, 0, 0)?;
    // It's possible to have artist name in the first position instead of a
    // TopResultType. There may be a way to differentiate this even further.
    let flex_1_0: String = parse_flex_column_item(&mut mrlir, 1, 0)?;
    // Deserialize without taking ownership of flex_1_0 - not possible with
    // JsonCrawler::take_value_pointer().
    // TODO: add methods like borrow_value_pointer() to JsonCrawler.
    let result_type_result: std::result::Result<_, serde::de::value::Error> =
        TopResultType::deserialize(flex_1_0.as_str().into_deserializer());
    let result_type = result_type_result.ok();
    // Imperative solution, may be able to make more functional.
    let mut subscribers = None;
    let mut publisher = None;
    let mut artist = None;
    let mut album = None;
    let mut duration = None;
    let mut year = None;
    let mut plays = None;
    match result_type {
        // XXX: Perhaps also populate Artist field.
        Some(TopResultType::Artist) => {
            subscribers = parse_flex_column_item(&mut mrlir, 1, 2).ok();
        }
        Some(TopResultType::Album(_)) => {
            // XXX: Perhaps also populate Album field.
            artist = parse_flex_column_item(&mut mrlir, 1, 2).ok();
            year = parse_flex_column_item(&mut mrlir, 1, 4).ok();
        }
        Some(TopResultType::Playlist) => {
            // Playlist, Video, and Station top result parsing not yet implemented.
            // Return None so the caller skips this entry gracefully.
            return Ok(None);
        }
        Some(TopResultType::Song) => {
            artist = parse_flex_column_item(&mut mrlir, 1, 2).ok();
            album = parse_flex_column_item(&mut mrlir, 1, 4).ok();
            duration = parse_flex_column_item(&mut mrlir, 1, 6).ok();
            plays = parse_flex_column_item(&mut mrlir, 1, 8).ok();
        }
        Some(TopResultType::Video) => {
            return Ok(None);
        }
        Some(TopResultType::Station) => {
            return Ok(None);
        }
        Some(TopResultType::Podcast) => publisher = parse_flex_column_item(&mut mrlir, 1, 2).ok(),
        None => {
            artist = Some(flex_1_0);
            let flex_1_2 = parse_flex_column_item(&mut mrlir, 1, 2);
            // If this does not show up, album isn't included in the results.
            if let Ok(flex_1_4) = parse_flex_column_item(&mut mrlir, 1, 4) {
                album = flex_1_2.ok();
                duration = Some(flex_1_4);
            } else {
                duration = flex_1_2.ok();
            }
            plays = parse_flex_column_item(&mut mrlir, 1, 6).ok();
        }
    }
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    Ok(Some(TopResult {
        result_type,
        subscribers,
        thumbnails,
        result_name,
        publisher,
        artist,
        album,
        duration,
        year,
        plays,
        byline: None,
    }))
}
// TODO: Type safety
// TODO: Tests
fn parse_artist_search_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<SearchResultArtist> {
    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    let artist = parse_flex_column_item(&mut mrlir, 0, 0)?;
    let subscribers = parse_flex_column_item(&mut mrlir, 1, 2).ok();
    let browse_id = mrlir.take_value_pointer(NAVIGATION_BROWSE_ID)?;
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    Ok(SearchResultArtist {
        artist,
        subscribers,
        thumbnails,
        browse_id,
    })
}
// TODO: Type safety
// TODO: Tests
fn parse_profile_search_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<SearchResultProfile> {
    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    let title = parse_flex_column_item(&mut mrlir, 0, 0)?;
    let username = parse_flex_column_item(&mut mrlir, 1, 2)?;
    let profile_id = mrlir.take_value_pointer(NAVIGATION_BROWSE_ID)?;
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    Ok(SearchResultProfile {
        title,
        username,
        profile_id,
        thumbnails,
    })
}
// TODO: Type safety
// TODO: Tests
fn parse_album_search_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<SearchResultAlbum> {
    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    let title = parse_flex_column_item(&mut mrlir, 0, 0)?;
    let album_type = parse_flex_column_item(&mut mrlir, 1, 0)?;

    // Artist can comprise of multiple runs, delimited by " • ".
    // See https://github.com/nick42d/youtui/issues/171
    let (artist, year) = mrlir
        .borrow_pointer(format!("{}/text/runs", flex_column_item_pointer(1)))?
        .try_expect(
            "album result should contain 3 string fields delimited by ' • '",
            |flex_column_1| {
                Ok(flex_column_1
                    .try_iter_mut()?
                    // First field is album_type which we parsed above, so skip it and the
                    // delimiter.
                    .skip(2)
                    .map(|mut field| field.take_value_pointer::<String>("/text"))
                    .collect::<json_crawler::CrawlerResult<String>>()?
                    .split(" • ")
                    .map(ToString::to_string)
                    .collect_tuple::<(String, String)>())
            },
        )?;

    let explicit = if mrlir.path_exists(BADGE_LABEL) {
        Explicit::IsExplicit
    } else {
        Explicit::NotExplicit
    };
    let browse_id = mrlir.take_value_pointer(NAVIGATION_BROWSE_ID)?;
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    Ok(SearchResultAlbum {
        artist,
        thumbnails,
        album_id: browse_id,
        title,
        year,
        album_type,
        explicit,
    })
}
fn parse_song_search_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<SearchResultSong> {
    // The byline comprises multiple fields delimited by " • ".
    // See https://github.com/nick42d/youtui/issues/171.
    // Album field is optional. See https://github.com/nick42d/youtui/issues/174
    /// Tuple makeup: (artist, album, duration)
    fn parse_song_fields(
        mrlir: &mut impl JsonCrawler,
    ) -> json_crawler::CrawlerResult<Option<(String, Option<ParsedSongAlbum>, String)>> {
        // NOTE: We are looping twice here, may be able to be improved.
        let num_runs = mrlir.try_iter_mut()?.count();
        let mut fields_vec = mrlir
            .try_iter_mut()?
            .map(|mut field| field.take_value_pointer::<String>("/text"))
            .collect::<json_crawler::CrawlerResult<String>>()?
            .rsplit(" • ")
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let Some(artist) = fields_vec.pop() else {
            return Ok(None);
        };
        let Some(album_or_duration) = fields_vec.pop() else {
            return Ok(None);
        };
        if let Some(duration) = fields_vec.pop() {
            let album_idx = num_runs - 3;
            let album = ParsedSongAlbum {
                name: album_or_duration,
                id: mrlir.take_value_pointer(format!("/{album_idx}{NAVIGATION_BROWSE_ID}"))?,
            };
            return Ok(Some((artist, Some(album), duration)));
        }
        Ok(Some((artist, None, album_or_duration)))
    }

    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    let title = parse_flex_column_item(&mut mrlir, 0, 0)?;

    let (artist, album, duration) = mrlir
        .borrow_pointer(format!("{}/text/runs", flex_column_item_pointer(1)))?
        .try_expect(
            "Song result should contain 2 or 3 string fields delimited by ' • '",
            parse_song_fields,
        )?;

    let plays = parse_flex_column_item(&mut mrlir, 2, 0)?;

    let explicit = if mrlir.path_exists(BADGE_LABEL) {
        Explicit::IsExplicit
    } else {
        Explicit::NotExplicit
    };
    let video_id = mrlir.take_value_pointer(PLAYLIST_ITEM_VIDEO_ID)?;
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    let video_type_path = concatcp!(
        PLAY_BUTTON,
        "/playNavigationEndpoint",
        NAVIGATION_VIDEO_TYPE
    );
    let music_video_type: Option<YoutubeMusicVideoType> =
        mrlir.take_value_pointer(video_type_path).ok();
    Ok(SearchResultSong {
        artist,
        thumbnails,
        title,
        explicit,
        plays,
        album,
        video_id,
        duration,
        music_video_type,
    })
}
// TODO: Type safety
// TODO: Tests
fn parse_video_search_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<Option<SearchResultVideo>> {
    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    // Handle not available case
    if let Ok("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT") = mrlir
        .take_value_pointer::<String>(DISPLAY_POLICY)
        .as_deref()
    {
        return Ok(None);
    };
    let title = parse_flex_column_item(&mut mrlir, 0, 0)?;
    let first_field: String = parse_flex_column_item(&mut mrlir, 1, 0)?;
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    match first_field.as_str() {
        // Old API format: flex column run 0 contains "Video" or "Episode" label
        "Video" => {
            let channel_name = parse_flex_column_item(&mut mrlir, 1, 2)?;
            let views = parse_flex_column_item(&mut mrlir, 1, 4)?;
            let length = parse_flex_column_item(&mut mrlir, 1, 6)?;
            let video_id = mrlir.take_value_pointer(PLAYLIST_ITEM_VIDEO_ID)?;
            Ok(Some(SearchResultVideo::Video {
                title,
                channel_name,
                views,
                length,
                thumbnails,
                video_id,
            }))
        }
        "Episode" => {
            let date = EpisodeDate::Recorded {
                date: parse_flex_column_item(&mut mrlir, 1, 2)?,
            };
            let channel_name = parse_flex_column_item(&mut mrlir, 1, 4)?;
            let episode_id = mrlir.take_value_pointer(PLAYLIST_ITEM_VIDEO_ID)?;
            Ok(Some(SearchResultVideo::VideoEpisode {
                title,
                channel_name,
                date,
                thumbnails,
                episode_id,
            }))
        }
        // New API format: YT Music removed type labels.
        // Detect video via watchEndpoint on the title run.
        _ => {
            if mrlir.path_exists("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/watchEndpoint") {
                let views = parse_flex_column_item(&mut mrlir, 1, 2).unwrap_or_default();
                let length = parse_flex_column_item(&mut mrlir, 1, 4).unwrap_or_default();
                let video_id = mrlir.take_value_pointer(PLAYLIST_ITEM_VIDEO_ID)?;
                Ok(Some(SearchResultVideo::Video {
                    title,
                    channel_name: first_field,
                    views,
                    length,
                    thumbnails,
                    video_id,
                }))
            } else {
                let channel_name = parse_flex_column_item(&mut mrlir, 1, 2).unwrap_or_default();
                let episode_id = mrlir.take_value_pointer(PLAYLIST_ITEM_VIDEO_ID)?;
                Ok(Some(SearchResultVideo::VideoEpisode {
                    title,
                    channel_name,
                    date: EpisodeDate::Recorded { date: first_field },
                    thumbnails,
                    episode_id,
                }))
            }
        }
    }
}
// TODO: Type safety
// TODO: Tests
fn parse_podcast_search_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<SearchResultPodcast> {
    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    let title = parse_flex_column_item(&mut mrlir, 0, 0)?;
    let publisher = parse_flex_column_item(&mut mrlir, 1, 0)?;
    let podcast_id = mrlir.take_value_pointer(NAVIGATION_BROWSE_ID)?;
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    Ok(SearchResultPodcast {
        title,
        publisher,
        podcast_id,
        thumbnails,
    })
}
// TODO: Type safety
// TODO: Tests
fn parse_episode_search_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<SearchResultEpisode> {
    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    let title = parse_flex_column_item(&mut mrlir, 0, 0)?;
    let first_run: String = parse_flex_column_item(&mut mrlir, 1, 0).unwrap_or_default();
    let second_run: Option<String> = parse_flex_column_item(&mut mrlir, 1, 2).ok();
    // Continuation items may have variable flex column layouts.
    // Check if a separator run exists at index 1 to detect 3-run layout.
    let (date, channel_name) = if mrlir
        .path_exists("/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs/1/text")
    {
        // 3+ runs: date, separator, channel
        (
            EpisodeDate::Recorded { date: first_run },
            second_run.unwrap_or_default(),
        )
    } else {
        // 1-2 runs: date at run 0 (or could be channel name directly)
        (
            EpisodeDate::Recorded {
                date: first_run.clone(),
            },
            first_run,
        )
    };
    let video_id = mrlir.take_value_pointer(PLAYLIST_ITEM_VIDEO_ID)?;
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    Ok(SearchResultEpisode {
        title,
        date,
        episode_id: video_id,
        channel_name,
        thumbnails,
    })
}
// TODO: Type safety
// TODO: Tests
fn parse_featured_playlist_search_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<SearchResultFeaturedPlaylist> {
    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    let title = parse_flex_column_item(&mut mrlir, 0, 0)?;
    let author = parse_flex_column_item(&mut mrlir, 1, 0)?;
    let songs = parse_flex_column_item(&mut mrlir, 1, 2)?;
    let playlist_id = mrlir.take_value_pointer(NAVIGATION_BROWSE_ID)?;
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    Ok(SearchResultFeaturedPlaylist {
        title,
        author,
        playlist_id,
        songs,
        thumbnails,
    })
}
fn parse_community_playlist_search_result_from_music_shelf_contents(
    music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<SearchResultCommunityPlaylist> {
    let mut mrlir = music_shelf_contents.navigate_pointer("/musicResponsiveListItemRenderer")?;
    let title = parse_flex_column_item(&mut mrlir, 0, 0)?;
    let author = parse_flex_column_item(&mut mrlir, 1, 0)?;
    let views = parse_flex_column_item(&mut mrlir, 1, 2)?;
    let playlist_id = mrlir.take_value_pointer(NAVIGATION_BROWSE_ID)?;
    let thumbnails: Vec<Thumbnail> = mrlir.take_value_pointer(THUMBNAILS)?;
    Ok(SearchResultCommunityPlaylist {
        title,
        author,
        playlist_id,
        views,
        thumbnails,
    })
}

fn parse_playlist_search_result_from_music_shelf_contents(
    mut music_shelf_contents: JsonCrawlerBorrowed<'_>,
) -> Result<SearchResultPlaylist> {
    let result_type: YoutubeMusicPageType = music_shelf_contents
        .borrow_value_pointer(concatcp!(MRLIR, NAVIGATION_BROWSE, PAGE_TYPE))?;

    // Search result for this query can be Podcast or Playlist.
    match result_type {
        YoutubeMusicPageType::Podcast => {
            let res = parse_podcast_search_result_from_music_shelf_contents(music_shelf_contents)?;
            Ok(SearchResultPlaylist::Podcast(res))
        }
        YoutubeMusicPageType::Playlist => {
            // The playlist search contains a mix of Community and Featured playlists.
            let playlist_params: PlaylistEndpointParams =
                music_shelf_contents.take_value_pointer(concatcp!(
                    MRLIR,
                    PLAY_BUTTON,
                    "/playNavigationEndpoint/watchPlaylistEndpoint/params"
                ))?;
            let playlist = match playlist_params {
                PlaylistEndpointParams::Featured => {
                    let res = parse_featured_playlist_search_result_from_music_shelf_contents(
                        music_shelf_contents,
                    )?;
                    SearchResultPlaylist::Featured(res)
                }
                PlaylistEndpointParams::Community => {
                    let res = parse_community_playlist_search_result_from_music_shelf_contents(
                        music_shelf_contents,
                    )?;
                    SearchResultPlaylist::Community(res)
                }
            };
            Ok(playlist)
        }
    }
}

struct FilteredSearchSectionContents(JsonCrawlerOwned);
struct FilteredSearchMusicShelfContents(JsonCrawlerOwned);
struct BasicSearchSectionListContents(JsonCrawlerOwned);
// In this case, we've searched and had no results found.
// We are being quite explicit here to avoid a false positive.
// See tests for an example.
// TODO: Test this function itself.
fn section_contents_is_empty(section_contents: &mut FilteredSearchSectionContents) -> Result<bool> {
    Ok(!section_contents
        .0
        .try_iter_mut()?
        .any(|item| item.path_exists(MUSIC_SHELF)))
}

fn take_continuation_params_from_section_contents(
    section_contents: &mut FilteredSearchSectionContents,
) -> Result<Option<ContinuationParams<'static>>> {
    section_contents
        .0
        .try_iter_mut()
        .and_then(|contents| contents.find_path(concatcp!(MUSIC_SHELF, CONTINUATION_PARAMS)))
        .map(|mut continuation_params| continuation_params.take_value())
        .ok()
        .transpose()
        .map_err(Into::into)
}
fn get_filtered_search_continuation_music_shelf_contents_and_params(
    crawler: JsonCrawlerOwned,
) -> Result<(
    FilteredSearchMusicShelfContents,
    Option<ContinuationParams<'static>>,
)> {
    let mut music_shelf = crawler.navigate_pointer(MUSIC_SHELF_CONTINUATION)?;
    let continuation_params = music_shelf.take_value_pointer(CONTINUATION_PARAMS).ok();
    let contents = music_shelf.navigate_pointer("/contents")?;
    Ok((
        FilteredSearchMusicShelfContents(contents),
        continuation_params,
    ))
}
fn section_list_contents_is_empty(
    section_contents: &mut BasicSearchSectionListContents,
) -> Result<bool> {
    Ok(!section_contents
        .0
        .try_iter_mut()?
        .any(|item| item.path_exists(MUSIC_CARD_SHELF) || item.path_exists(MUSIC_SHELF)))
}
impl<'a, S: UnfilteredSearchType> TryFrom<ProcessedResult<'a, SearchQuery<'a, S>>>
    for BasicSearchSectionListContents
{
    type Error = Error;
    fn try_from(value: ProcessedResult<SearchQuery<'a, S>>) -> Result<Self> {
        let json_crawler: JsonCrawlerOwned = value.into();
        let section_list_contents = json_crawler.navigate_pointer(concatcp!(
            "/contents/tabbedSearchResultsRenderer",
            TAB_CONTENT,
            SECTION_LIST
        ))?;
        Ok(BasicSearchSectionListContents(section_list_contents))
    }
}
impl<'a, F: FilteredSearchType> TryFrom<ProcessedResult<'a, SearchQuery<'a, FilteredSearch<F>>>>
    for FilteredSearchSectionContents
{
    type Error = Error;
    fn try_from(value: ProcessedResult<SearchQuery<'a, FilteredSearch<F>>>) -> Result<Self> {
        let json_crawler: JsonCrawlerOwned = value.into();
        let section_contents = json_crawler.navigate_pointer(concatcp!(
            "/contents/tabbedSearchResultsRenderer",
            TAB_CONTENT,
            SECTION_LIST,
        ))?;
        Ok(FilteredSearchSectionContents(section_contents))
    }
}
impl TryFrom<FilteredSearchSectionContents> for FilteredSearchMusicShelfContents {
    type Error = Error;
    fn try_from(
        value: FilteredSearchSectionContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        let music_shelf_contents = value
            .0
            .try_into_iter()?
            .find_path(concatcp!(MUSIC_SHELF, "/contents"))?;
        Ok(FilteredSearchMusicShelfContents(music_shelf_contents))
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultAlbum> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .map(|a| parse_album_search_result_from_music_shelf_contents(a))
            .collect()
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultProfile> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .map(|a| parse_profile_search_result_from_music_shelf_contents(a))
            .collect()
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultArtist> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .map(|a| parse_artist_search_result_from_music_shelf_contents(a))
            .collect()
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultSong> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .map(|a| parse_song_search_result_from_music_shelf_contents(a))
            .collect()
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultVideo> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .filter_map(|a| parse_video_search_result_from_music_shelf_contents(a).transpose())
            .collect()
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultEpisode> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .map(|a| parse_episode_search_result_from_music_shelf_contents(a))
            .collect()
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultPodcast> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .map(|a| parse_podcast_search_result_from_music_shelf_contents(a))
            .collect()
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultPlaylist> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .map(|a| parse_playlist_search_result_from_music_shelf_contents(a))
            .collect()
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultCommunityPlaylist> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .map(|a| parse_community_playlist_search_result_from_music_shelf_contents(a))
            .collect()
    }
}
impl TryFrom<FilteredSearchMusicShelfContents> for Vec<SearchResultFeaturedPlaylist> {
    type Error = Error;
    fn try_from(
        mut value: FilteredSearchMusicShelfContents,
    ) -> std::prelude::v1::Result<Self, Self::Error> {
        // TODO: Make this a From method.
        value
            .0
            .try_iter_mut()?
            .map(|a| parse_featured_playlist_search_result_from_music_shelf_contents(a))
            .collect()
    }
}
impl<'a, S: UnfilteredSearchType> ParseFrom<SearchQuery<'a, S>> for SearchResults {
    fn parse_from(p: ProcessedResult<SearchQuery<'a, S>>) -> crate::Result<Self> {
        let mut section_list_contents = BasicSearchSectionListContents::try_from(p)?;
        if section_list_contents_is_empty(&mut section_list_contents)? {
            return Ok(Self::default());
        }
        parse_basic_search_result_from_section_list_contents(section_list_contents)
    }
}

impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<ArtistsFilter>>>
    for Vec<SearchResultArtist>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<ArtistsFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<
            GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<ArtistsFilter>>>,
        >,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}
impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<ProfilesFilter>>>
    for Vec<SearchResultProfile>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<ProfilesFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<
            GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<ProfilesFilter>>>,
        >,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}
impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<AlbumsFilter>>>
    for Vec<SearchResultAlbum>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<AlbumsFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<
            GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<AlbumsFilter>>>,
        >,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}
impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<SongsFilter>>>
    for Vec<SearchResultSong>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<SongsFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<SongsFilter>>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}
impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<VideosFilter>>>
    for Vec<SearchResultVideo>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<VideosFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<
            GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<VideosFilter>>>,
        >,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}
impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<EpisodesFilter>>>
    for Vec<SearchResultEpisode>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<EpisodesFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<
            GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<EpisodesFilter>>>,
        >,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}
impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<PodcastsFilter>>>
    for Vec<SearchResultPodcast>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<PodcastsFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<
            GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<PodcastsFilter>>>,
        >,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}
impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<CommunityPlaylistsFilter>>>
    for Vec<SearchResultPlaylist>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<CommunityPlaylistsFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<
            GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<CommunityPlaylistsFilter>>>,
        >,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}
impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<FeaturedPlaylistsFilter>>>
    for Vec<SearchResultFeaturedPlaylist>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<FeaturedPlaylistsFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<
            GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<FeaturedPlaylistsFilter>>>,
        >,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}
impl<'a> ParseFromContinuable<SearchQuery<'a, FilteredSearch<PlaylistsFilter>>>
    for Vec<SearchResultPlaylist>
{
    fn parse_from_continuable(
        p: ProcessedResult<SearchQuery<'a, FilteredSearch<PlaylistsFilter>>>,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let mut section_contents = FilteredSearchSectionContents::try_from(p)?;
        if section_contents_is_empty(&mut section_contents)? {
            return Ok((Vec::new(), None));
        }
        let continuation_params =
            take_continuation_params_from_section_contents(&mut section_contents)?;
        let results = FilteredSearchMusicShelfContents::try_from(section_contents)?.try_into()?;
        Ok((results, continuation_params))
    }
    fn parse_continuation(
        p: ProcessedResult<
            GetContinuationsQuery<'_, SearchQuery<'a, FilteredSearch<PlaylistsFilter>>>,
        >,
    ) -> crate::Result<(Self, Option<crate::common::ContinuationParams<'static>>)> {
        let crawler: JsonCrawlerOwned = p.into();
        let (contents, continuation_params) =
            get_filtered_search_continuation_music_shelf_contents_and_params(crawler)?;
        let results = contents.try_into()?;
        Ok((results, continuation_params))
    }
}

impl<'a> ParseFrom<GetSearchSuggestionsQuery<'a>> for Vec<SearchSuggestion> {
    fn parse_from(p: ProcessedResult<GetSearchSuggestionsQuery<'a>>) -> crate::Result<Self> {
        let json_crawler: JsonCrawlerOwned = p.into();
        let mut suggestions = json_crawler
            .navigate_pointer("/contents/0/searchSuggestionsSectionRenderer/contents")?;
        let mut results = Vec::new();
        for mut s in suggestions.try_iter_mut()? {
            let mut runs = Vec::new();
            if let Ok(mut search_suggestion) =
                s.borrow_pointer("/searchSuggestionRenderer/suggestion/runs")
            {
                for mut r in search_suggestion.try_iter_mut()? {
                    if let Ok(true) = r.take_value_pointer("/bold") {
                        runs.push(r.take_value_pointer("/text").map(TextRun::Bold)?)
                    } else {
                        runs.push(r.take_value_pointer("/text").map(TextRun::Normal)?)
                    }
                }
                results.push(SearchSuggestion::new(SuggestionType::Prediction, runs))
            } else {
                for mut r in s
                    .borrow_pointer("/historySuggestionRenderer/suggestion/runs")?
                    .try_iter_mut()?
                {
                    if let Ok(true) = r.take_value_pointer("/bold") {
                        runs.push(r.take_value_pointer("/text").map(TextRun::Bold)?)
                    } else {
                        runs.push(r.take_value_pointer("/text").map(TextRun::Normal)?)
                    }
                }
                results.push(SearchSuggestion::new(SuggestionType::History, runs))
            }
        }
        Ok(results)
    }
}
