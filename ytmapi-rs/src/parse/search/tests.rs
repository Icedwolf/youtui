#![allow(deprecated)]
use crate::auth::BrowserToken;
use crate::parse::SearchResults;
use crate::process_json;
use crate::query::search::{
    AlbumsFilter, ArtistsFilter, CommunityPlaylistsFilter, EpisodesFilter, FeaturedPlaylistsFilter,
    PlaylistsFilter, PodcastsFilter, ProfilesFilter, SearchQuery, SongsFilter, VideosFilter,
};
use pretty_assertions::assert_eq;
use std::path::Path;

#[tokio::test]
async fn test_search_basic_top_result_no_type() {
    // Case where topmost result doesn't contain a type.
    parse_test!(
        "./test_json/search_basic_top_result_no_type_20240720.json",
        "./test_json/search_basic_top_result_no_type_20240720_output.txt",
        SearchQuery::new(""),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_basic_radio() {
    // Case where topmost result is a special 'radio' playlist. Doesn't contain a
    // type and only has a single subtitle. Seems to show up when searching for
    // genres like classical and metal.
    parse_test!(
        "./test_json/search_basic_radio_20240830.json",
        "./test_json/search_basic_radio_20240830_output.txt",
        SearchQuery::new(""),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_basic_top_result_card() {
    // Case where there is only a 'card' top result, with no children.
    parse_test!(
        "./test_json/search_basic_top_result_card_20240721.json",
        "./test_json/search_basic_top_result_card_20240721_output.txt",
        SearchQuery::new(""),
        BrowserToken
    );
}
#[tokio::test]
async fn test_basic_search_no_results_suggestions() {
    // Case where there are no results, but there are 'Did You Mean' suggestions.
    parse_test_value!(
        "./test_json/search_basic_no_results_suggestions_20240104.json",
        SearchResults::default(),
        SearchQuery::new(""),
        BrowserToken
    );
}

#[tokio::test]
async fn test_search_basic_no_results() {
    // Case where there are no results, and there are not 'Did You Mean'
    // suggestions.
    parse_test!(
        "./test_json/search_basic_no_results_20240721.json",
        "./test_json/search_basic_no_results_20240721_output.txt",
        SearchQuery::new(""),
        BrowserToken
    );
}

#[tokio::test]
async fn test_search_artists_empty() {
    let source_path = Path::new("./test_json/search_artists_no_results_20231226.json");
    let source = tokio::fs::read_to_string(source_path)
        .await
        .expect("Expect file read to pass during tests");
    // Blank query has no bearing on function
    let query = SearchQuery::new_filtered("", ArtistsFilter);
    let output = process_json::<_, BrowserToken>(source, query).unwrap();
    assert_eq!(output, Vec::new());
}
#[tokio::test]
// Test results appear for the correct categories.
async fn test_basic_search_has_simple_top_result() {
    let source_path = Path::new("./test_json/search_basic_top_result_20231228.json");
    let source = tokio::fs::read_to_string(source_path)
        .await
        .expect("Expect file read to pass during tests");
    // Blank query has no bearing on function
    let query = SearchQuery::new("");
    let output = process_json::<_, BrowserToken>(source, query).unwrap();
    assert!(!output.top_results.is_empty());
}
#[tokio::test]
// Test results appear for the correct categories.
async fn test_basic_search_has_card_top_result() {
    let source_path = Path::new("./test_json/search_highlighted_top_result_20240107.json");
    let source = tokio::fs::read_to_string(source_path)
        .await
        .expect("Expect file read to pass during tests");
    // Blank query has no bearing on function
    let query = SearchQuery::new("");
    let output = process_json::<_, BrowserToken>(source, query).unwrap();
    assert!(!output.top_results.is_empty());
}
#[tokio::test]
// Test results appear for the correct categories.
async fn test_basic_search_no_top_results_has_results() {
    let source_path = Path::new("./test_json/search_basic_no_top_result_20231228.json");
    let source = tokio::fs::read_to_string(source_path)
        .await
        .expect("Expect file read to pass during tests");
    // Blank query has no bearing on function
    let query = SearchQuery::new("");
    let output = process_json::<_, BrowserToken>(source, query).unwrap();
    assert!(!output.songs.is_empty());
    assert!(!output.featured_playlists.is_empty());
    assert!(output.videos.is_empty());
    assert!(output.community_playlists.is_empty());
    assert!(output.episodes.is_empty());
    assert!(!output.artists.is_empty());
    assert!(output.podcasts.is_empty());
    assert!(output.profiles.is_empty());
    assert!(output.top_results.is_empty());
}

#[tokio::test]
async fn test_basic_search_highlighted_top_result() {
    parse_test!(
        "./test_json/search_highlighted_top_result_20240107.json",
        "./test_json/search_highlighted_top_result_20240107_output.txt",
        SearchQuery::new(""),
        BrowserToken
    );
}
#[tokio::test]
async fn test_basic_search_with_vodcasts_type_not_specified() {
    parse_test!(
        "./test_json/search_basic_with_vodcasts_type_not_specified_20240612.json",
        "./test_json/search_basic_with_vodcasts_type_not_specified_20240612_output.txt",
        SearchQuery::new(""),
        BrowserToken
    );
}
#[tokio::test]
async fn test_basic_search_with_vodcasts_type_specified() {
    parse_test!(
        "./test_json/search_basic_with_vodcasts_type_specified_20240612.json",
        "./test_json/search_basic_with_vodcasts_type_specified_20240612_output.txt",
        SearchQuery::new(""),
        BrowserToken
    );
}
#[tokio::test]
async fn test_basic_search_with_about_message() {
    parse_test!(
        "./test_json/search_basic_with_about_message_20240809.json",
        "./test_json/search_basic_with_about_message_20240809_output.txt",
        SearchQuery::new(""),
        BrowserToken
    );
}
#[tokio::test]
async fn test_basic_search_with_podcast_community_playlists() {
    parse_test!(
        "./test_json/search_basic_with_podcast_community_playlists_20250605.json",
        "./test_json/search_basic_with_podcast_community_playlists_20250605_output.txt",
        SearchQuery::new(""),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_artists() {
    parse_with_matching_continuation_test!(
        "./test_json/search_artists_20231226.json",
        "./test_json/search_artists_continuation_20231226.json",
        "./test_json/search_artists_20231226_output.txt",
        SearchQuery::new_filtered("", ArtistsFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_artists_with_about_message() {
    parse_test!(
        "./test_json/search_artists_with_about_message_20240824.json",
        "./test_json/search_artists_with_about_message_20240824_output.txt",
        SearchQuery::new_filtered("", ArtistsFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_artists_drops_junk_entries() {
    // YouTube Music has begun leaking non-artist entries (videos, playlists,
    // unrelated songs) into the Artists-filtered shelf — items with no
    // navigationEndpoint/browseEndpoint/browseId. The parse must keep the real
    // artists and skip the junk instead of failing the whole query: a single
    // junk entry currently aborts the artist search.
    // (Fixture: live "american football" Artists-filter response, 2026-08-13.)
    let source_path = Path::new("./test_json/search_artists_with_junk_20260813.json");
    let source = tokio::fs::read_to_string(source_path).await.unwrap();
    let parsed: Vec<crate::parse::SearchResultArtist> =
        process_json::<_, BrowserToken>(source, SearchQuery::new_filtered("", ArtistsFilter)).unwrap();
    let names: Vec<&str> = parsed.iter().map(|a| a.artist.as_str()).collect();
    assert_eq!(names, ["American Football", "Mike Kinsella"]);
}
#[tokio::test]
async fn test_search_songs_drops_junk_entries() {
    // Same leak into the Songs-filtered shelf: entries that look like songs
    // (they carry a videoId) but have no album browseEndpoint in the subtitle.
    // Keep the playable songs, skip the junk, never fail the whole query.
    // (Fixture: live "american football" Songs-filter response, 2026-08-13.)
    let source_path = Path::new("./test_json/search_songs_with_junk_20260813.json");
    let source = tokio::fs::read_to_string(source_path).await.unwrap();
    let parsed: Vec<crate::parse::SearchResultSong> =
        process_json::<_, BrowserToken>(source, SearchQuery::new_filtered("", SongsFilter)).unwrap();
    // 24 shelf entries = 20 real songs + 3 malformed leaks (no album browse)
    // + 1 well-formed unrelated entry (kept, structurally valid). The 3
    // malformed leaks drop; every retained song has an album.
    assert_eq!(parsed.len(), 21);
    assert!(parsed.iter().any(|s| s.title == "Never Meant"));
    assert!(parsed.iter().all(|s| s.album.is_some()));
    assert!(
        parsed
            .iter()
            .all(|s| !s.title.contains("This Is America") && s.title != "Dai Dai")
    );
}
#[tokio::test]
async fn test_search_albums() {
    parse_with_matching_continuation_test!(
        "./test_json/search_albums_20231226.json",
        "./test_json/search_albums_continuation_20231226.json",
        "./test_json/search_albums_20231226_output.txt",
        SearchQuery::new_filtered("", AlbumsFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_songs() {
    parse_with_matching_continuation_test!(
        "./test_json/search_songs_20231226.json",
        "./test_json/search_songs_continuation_20231226.json",
        "./test_json/search_songs_20231226_output.txt",
        SearchQuery::new_filtered("", SongsFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_videos() {
    parse_test!(
        "./test_json/search_videos_20231226.json",
        "./test_json/search_videos_20231226_output.txt",
        SearchQuery::new_filtered("", VideosFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_videos_2024() {
    // Vodcasts were added for this version
    parse_with_matching_continuation_test!(
        "./test_json/search_videos_20240612.json",
        "./test_json/search_videos_continuation_20240612.json",
        "./test_json/search_videos_20240612_output.txt",
        SearchQuery::new_filtered("", VideosFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_playlists() {
    parse_with_matching_continuation_test!(
        "./test_json/search_playlists_20231228.json",
        "./test_json/search_playlists_continuation_20231228.json",
        "./test_json/search_playlists_20231228_output.txt",
        SearchQuery::new_filtered("", PlaylistsFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_featured_playlists() {
    parse_with_matching_continuation_test!(
        "./test_json/search_featured_playlists_20231226.json",
        "./test_json/search_featured_playlists_continuation_20231226.json",
        "./test_json/search_featured_playlists_20231226_output.txt",
        SearchQuery::new_filtered("", FeaturedPlaylistsFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_community_playlists() {
    parse_with_matching_continuation_test!(
        "./test_json/search_community_playlists_20231226.json",
        "./test_json/search_community_playlists_continuation_20231226.json",
        "./test_json/search_community_playlists_20231226_output.txt",
        SearchQuery::new_filtered("", CommunityPlaylistsFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_episodes() {
    parse_with_matching_continuation_test!(
        "./test_json/search_episodes_20231226.json",
        "./test_json/search_episodes_continuation_20231226.json",
        "./test_json/search_episodes_20231226_output.txt",
        SearchQuery::new_filtered("", EpisodesFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_podcasts() {
    parse_with_matching_continuation_test!(
        "./test_json/search_podcasts_20231226.json",
        "./test_json/search_podcasts_continuation_20231226.json",
        "./test_json/search_podcasts_20231226_output.txt",
        SearchQuery::new_filtered("", PodcastsFilter),
        BrowserToken
    );
}
#[tokio::test]
async fn test_search_profiles() {
    parse_with_matching_continuation_test!(
        "./test_json/search_profiles_20231226.json",
        "./test_json/search_profiles_continuation_20231226.json",
        "./test_json/search_profiles_20231226_output.txt",
        SearchQuery::new_filtered("", ProfilesFilter),
        BrowserToken
    );
}

// Push a junk entry (a partial musicResponsiveListItemRenderer, like the
// videos/playlists YouTube Music leaks into search shelves) into every
// `renderer_key.<array_key>` list in the response. `serde_json` re-serializes
// the Value, which JsonCrawler re-parses on the other side.
fn inject_junk_into_lists(source: &str, targets: &[(&str, &str)]) -> String {
    fn walk(value: &mut serde_json::Value, targets: &[(&str, &str)]) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    if let Some(list) = targets
                        .iter()
                        .find(|(rk, _)| *rk == key)
                        .and_then(|(_, array_key)| {
                            child
                                .get_mut(*array_key)
                                .and_then(serde_json::Value::as_array_mut)
                        })
                    {
                        list.push(serde_json::json!({
                            "musicResponsiveListItemRenderer": {}
                        }));
                    }
                    walk(child, targets);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, targets);
                }
            }
            _ => {}
        }
    }
    let mut json: serde_json::Value = serde_json::from_str(source).unwrap();
    walk(&mut json, targets);
    json.to_string()
}

#[tokio::test]
async fn test_search_basic_shelves_drop_junk_entries() {
    // The 2026-08-13 junk leak hits every basic-search shelf, not just the
    // filtered Artists/Songs ones: Albums, Featured playlists and Songs each
    // aborted the whole query on one leaked entry. A junk entry in any shelf
    // must drop; the valid entries survive.
    let source = tokio::fs::read_to_string(Path::new(
        "./test_json/search_basic_no_top_result_20231228.json",
    ))
    .await
    .unwrap();
    let source = inject_junk_into_lists(&source, &[("musicShelfRenderer", "contents")]);
    let parsed: SearchResults = process_json::<_, BrowserToken>(source, SearchQuery::new("")).unwrap();
    assert!(!parsed.albums.is_empty());
    assert!(!parsed.featured_playlists.is_empty());
    assert!(!parsed.songs.is_empty());
}

macro_rules! filtered_junk_tolerance_tests {
    ($( $name:ident: $fixture:literal => $filter:expr, $out:ty; )*) => {
        $(
            #[tokio::test]
            async fn $name() {
                // Same leak into the other filtered shelves: one junk entry
                // (partial musicResponsiveListItemRenderer) aborted the whole
                // query. It must drop and leave the real results intact.
                let source = tokio::fs::read_to_string(Path::new($fixture)).await.unwrap();
                let source = inject_junk_into_lists(&source, &[("musicShelfRenderer", "contents")]);
                let parsed: $out = process_json::<_, BrowserToken>(
                    source,
                    SearchQuery::new_filtered("", $filter),
                )
                .unwrap();
                assert!(!parsed.is_empty(), "expected valid entries to survive junk");
            }
        )*
    };
}

filtered_junk_tolerance_tests! {
    test_search_albums_drop_junk_entries: "./test_json/search_albums_20231226.json" => AlbumsFilter, Vec<crate::parse::SearchResultAlbum>;
    test_search_profiles_drop_junk_entries: "./test_json/search_profiles_20231226.json" => ProfilesFilter, Vec<crate::parse::SearchResultProfile>;
    test_search_episodes_drop_junk_entries: "./test_json/search_episodes_20231226.json" => EpisodesFilter, Vec<crate::parse::SearchResultEpisode>;
    test_search_podcasts_drop_junk_entries: "./test_json/search_podcasts_20231226.json" => PodcastsFilter, Vec<crate::parse::SearchResultPodcast>;
    test_search_playlists_drop_junk_entries: "./test_json/search_playlists_20231228.json" => PlaylistsFilter, Vec<crate::parse::SearchResultPlaylist>;
    test_search_community_playlists_drop_junk_entries: "./test_json/search_community_playlists_20231226.json" => CommunityPlaylistsFilter, Vec<crate::parse::SearchResultPlaylist>;
    test_search_featured_playlists_drop_junk_entries: "./test_json/search_featured_playlists_20231226.json" => FeaturedPlaylistsFilter, Vec<crate::parse::SearchResultFeaturedPlaylist>;
}
