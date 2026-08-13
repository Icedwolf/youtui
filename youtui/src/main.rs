use anyhow::{Context, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use cli::handle_cli_command;
use config::{ApiKey, AuthType, Config};
use directories::ProjectDirs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
mod api;
mod app;
mod appevent;
mod async_rodio_sink;
mod cli;
mod config;
mod core;
mod decoder;
mod drawutils;
mod keyaction;
mod keybind;
mod widgets;

pub(crate) const POTOKEN_FILENAME: &str = "po_token.txt";
pub(crate) const COOKIE_FILENAME: &str = "cookie.txt";
pub(crate) const COOKIE_NETSCAPE_FILENAME: &str = "cookies_netscape.txt";

/// Detect a browser with YouTube cookies for yt-dlp `--cookies-from-browser`.
/// Returns `None` if no supported browser profile is found.
pub(crate) fn detect_browser_source() -> Option<String> {
    let home = std::path::PathBuf::from(std::env::var("HOME").ok()?);
    let floorp_profiles = home.join(".floorp").join("profiles.ini");
    if let Ok(content) = std::fs::read_to_string(&floorp_profiles) {
        for line in content.lines() {
            if let Some(path) = line.strip_prefix("Path=") {
                let profile_dir = home.join(".floorp").join(path.trim());
                return Some(format!("firefox:{}", profile_dir.display()));
            }
        }
    }
    let ff_paths = [
        home.join(".mozilla").join("firefox").join("profiles.ini"),
        home.join(".config").join("mozilla").join("firefox").join("profiles.ini"),
    ];
    for ff_path in &ff_paths {
        if ff_path.exists() {
            return Some("firefox".to_string());
        }
    }
    if home.join(".config").join("chromium").join("Default").join("Cookies").exists() {
        return Some("chromium".to_string());
    }
    None
}

const BROWSER_AUTH_SETUP_STEPS_URL: &str =
    "https://github.com/Icedwolf/youtui?tab=readme-ov-file#browser-auth-setup-steps";
const POTOKEN_INFORMATION_URL: &str =
    "https://github.com/Icedwolf/youtui?tab=readme-ov-file#po-token-information";
const RUNNING_YOUTUI_GUIDE_URL: &str =
    "https://github.com/Icedwolf/youtui?tab=readme-ov-file#running-youtui";
const DIRECTORY_NAME_ERROR_MESSAGE: &str = "Error generating application directory for your host system. See README.md for more information about application directories.";

#[derive(Parser, Debug)]
#[command(author,version,about,long_about=None)]
/// A text-based user interface for YouTube Music.
struct Arguments {
    /// Display and log additional debug information.
    #[arg(short, long, default_value_t = false)]
    debug: bool,
    /// Disable media controls
    #[arg(long, default_value_t = false)]
    disable_media_controls: bool,
    #[command(flatten)]
    cli: Cli,
    /// Generate shell completions for the specified shell
    #[arg(short, long, id = "SHELL", value_enum)]
    generate_completions: Option<Shell>,
    /// Force the use of an auth type.
    #[arg(value_enum, short, long)]
    auth_type: Option<AuthType>,
}

#[derive(Args, Debug, Clone)]
struct Cli {
    /// Print the source output Json from YouTube Music's API instead of the
    /// processed value.
    #[arg(short, long, default_value_t = false)]
    show_source: bool,
    /// Process the passed Json file(s) as if received from YouTube Music. This
    /// parameter can be passed multiple times, processing multiple files if
    /// the endpoint supports continuations. If multiple files are
    /// passed but the endpoint doesn't support continuations, only the
    /// first one is processed.
    #[arg(short, long, id = "PATH")]
    input_json: Option<Vec<PathBuf>>,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand, Debug, Clone)]
enum Command {
    GetSearchSuggestions {
        query: String,
    },
    GetArtist {
        channel_id: String,
    },
    GetArtistAlbums {
        channel_id: String,
        browse_params: String,
        #[arg(default_value_t = usize::MAX)]
        max_pages: usize,
    },
    SubscribeArtist {
        channel_id: String,
    },
    UnsubscribeArtists {
        channel_ids: Vec<String>,
    },
    GetAlbum {
        browse_id: String,
    },
    GetPlaylistDetails {
        playlist_id: String,
    },
    GetPlaylistTracks {
        playlist_id: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    GetLibraryPlaylists {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    //TODO: Allow sorting
    GetLibraryArtists {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    //TODO: Allow sorting
    GetLibrarySongs {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    //TODO: Allow sorting
    GetLibraryAlbums {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    //TODO: Allow sorting
    GetLibraryArtistSubscriptions {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    //TODO: Allow sorting
    GetLibraryPodcasts {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    //TODO: Allow sorting
    GetLibraryChannels {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    Search {
        query: String,
    },
    SearchArtists {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    SearchAlbums {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    SearchSongs {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    SearchPlaylists {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    SearchCommunityPlaylists {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    SearchFeaturedPlaylists {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    SearchVideos {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    SearchEpisodes {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    SearchProfiles {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    SearchPodcasts {
        query: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    // TODO: Privacy status, video ids, source playlist
    CreatePlaylist {
        title: String,
        description: Option<String>,
    },
    DeletePlaylist {
        playlist_id: String,
    },
    RemovePlaylistItems {
        playlist_id: String,
        video_ids: Vec<String>,
    },
    AddVideosToPlaylist {
        playlist_id: String,
        video_ids: Vec<String>,
    },
    AddPlaylistToPlaylist {
        playlist_id: String,
        from_playlist_id: String,
    },
    EditPlaylistTitle {
        playlist_id: String,
        new_title: String,
    },
    GetHistory,
    RemoveHistoryItems {
        feedback_tokens: Vec<String>,
    },
    RateSong {
        video_id: String,
        like_status: String,
    },
    RatePlaylist {
        playlist_id: String,
        like_status: String,
    },
    EditSongLibraryStatus {
        feedback_tokens: Vec<String>,
    },
    // TODO: Sorting
    GetLibraryUploadSongs {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    // TODO: Sorting
    GetLibraryUploadArtists {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    // TODO: Sorting
    GetLibraryUploadAlbums {
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    GetLibraryUploadArtist {
        upload_artist_id: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    GetLibraryUploadAlbum {
        upload_album_id: String,
    },
    DeleteUploadEntity {
        upload_entity_id: String,
    },
    GetTasteProfile,
    // Simple implementation - only allows a single set per command.
    SetTasteProfile {
        impression_token: String,
        selection_token: String,
    },
    GetMoodCategories,
    GetMoodPlaylists {
        mood_category_params: String,
    },
    AddHistoryItem {
        song_tracking_url: String,
    },
    GetSongTrackingUrl {
        video_id: String,
    },
    GetLyrics {
        lyrics_id: String,
    },
    GetLyricsID {
        video_id: String,
    },
    // TODO: Option to use playlist ID instead
    GetWatchPlaylist {
        video_id: String,
        /// Maximum number of pages that the API is allowed to return.
        #[arg(default_value_t = 1)]
        max_pages: usize,
    },
    GetChannel {
        channel_id: String,
    },
    GetChannelEpisodes {
        channel_id: String,
        podcast_channel_params: String,
    },
    GetPodcast {
        podcast_id: String,
    },
    GetEpisode {
        video_id: String,
    },
    GetNewEpisodes,
    GetUser {
        user_channel_id: String,
    },
    GetUserPlaylists {
        user_channel_id: String,
        browse_params: String,
    },
    GetUserVideos {
        user_channel_id: String,
        browse_params: String,
    },
}

pub(crate) struct RuntimeInfo {
    debug: bool,
    disable_media_controls: bool,
    config: Config,
    api_key: ApiKey,
    po_token: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    // Using try block to print error using Display instead of Debug.
    if let Err(e) = try_main().await {
        println!("{e:?}");
        return ExitCode::FAILURE;
    };
    ExitCode::SUCCESS
}

// Main function is refactored here so that we can pretty print errors.
// Regular main function returns debug errors so not as friendly.
async fn try_main() -> anyhow::Result<()> {
    let args = Arguments::parse();
    let Arguments {
        debug,
        cli,
        auth_type,
        generate_completions,
        disable_media_controls,
    } = args;
    if let Some(shell) = generate_completions {
        let mut cmd = Arguments::command();
        let bin_name = cmd.get_name().to_string();
        eprintln!("Generating completion file for {shell:?}");
        generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
        // Done here if we got this command. No need to go further.
        return Ok(());
    };
    // Config and API key files will be in OS directories.
    // Create them if they don't exist.
    initialise_directories().await?;
    let mut config = config::Config::new(debug).await?;
    // Command line flag for auth_type should override config for auth_type.
    if let Some(auth_type) = auth_type {
        config.auth_type = auth_type
    }
    // Once config has loaded, load API key to memory
    // (Which key to load depends on configuration)
    // TODO: api_key and po_token could be more lazily loaded.
    let api_key = load_api_key(&config).await?;
    // Use PoToken, if the user has supplied one (otherwise don't).
    let po_token = load_po_token().await.ok();
    let rt = RuntimeInfo {
        debug,
        config,
        api_key,
        po_token,
        disable_media_controls,
    };
    match cli.command {
        None => run_app(rt).await?,
        Some(_) => handle_cli_command(cli, rt).await?,
    };
    Ok(())
}

/// Build the ytmapi-rs API client for CLI commands. Distinct from
/// `load_api_key` (which returns the app's `ApiKey`); CLI commands need a live
/// `DynamicYtMusic` to run queries against.
async fn get_api(config: &Config) -> anyhow::Result<api::DynamicYtMusic> {
    let confdir = get_config_dir()?;
    let api = match config.auth_type {
        config::AuthType::Browser => {
            let mut cookies_loc = confdir;
            cookies_loc.push(COOKIE_FILENAME);
            let api = ytmapi_rs::builder::YtMusicBuilder::new_rustls_tls()
                .with_browser_token_cookie_file(cookies_loc)
                .build()
                .await?;
            api::DynamicYtMusic::Browser(api)
        }
        config::AuthType::Unauthenticated => {
            let api = ytmapi_rs::builder::YtMusicBuilder::new_rustls_tls()
                .build()
                .await?;
            api::DynamicYtMusic::NoAuth(api)
        }
    };
    Ok(api)
}

pub(crate) async fn run_app(rt: RuntimeInfo) -> anyhow::Result<()> {
    let mut app = app::Youtui::new(rt).await?;
    app.run().await?;
    Ok(())
}

/// Returns the data directory path. Override with `YOUTUI_DATA_DIR` env var.
/// Defaults to the OS‑specific data directory (e.g. `~/.local/share/youtui` on Linux).
pub(crate) fn get_data_dir() -> anyhow::Result<PathBuf> {
    let directory = if let Ok(s) = std::env::var("YOUTUI_DATA_DIR") {
        PathBuf::from(s)
    } else if let Some(proj_dirs) = ProjectDirs::from("com", "nick42", "youtui") {
        proj_dirs.data_local_dir().to_path_buf()
    } else {
        bail!(DIRECTORY_NAME_ERROR_MESSAGE);
    };
    Ok(directory)
}

/// Returns the config directory path. Override with `YOUTUI_CONFIG_DIR` env var.
/// Defaults to the OS‑specific config directory (e.g. `~/.config/youtui` on Linux).
pub(crate) fn get_config_dir() -> anyhow::Result<PathBuf> {
    let directory = if let Ok(s) = std::env::var("YOUTUI_CONFIG_DIR") {
        PathBuf::from(s)
    } else if let Some(proj_dirs) = ProjectDirs::from("com", "nick42", "youtui") {
        proj_dirs.config_local_dir().to_path_buf()
    } else {
        bail!(DIRECTORY_NAME_ERROR_MESSAGE);
    };
    Ok(directory)
}

async fn load_po_token() -> anyhow::Result<String> {
    let mut path = get_config_dir()?;
    path.push(POTOKEN_FILENAME);
    tokio::fs::read_to_string(&path)
        .await
        // Allocation is required here if we wish to trim within this function.
        .map(|s| s.trim().to_string())
        .with_context(|| {
            format!(
                "Error loading po_token from {}. Does the file exist? See README.md for more information on PO tokens: {}",
                path.display(),
                POTOKEN_INFORMATION_URL
            )
        })
}

async fn load_cookie_file() -> anyhow::Result<String> {
    let mut path = get_config_dir()?;
    path.push(COOKIE_FILENAME);
    tokio::fs::read_to_string(&path)
        .await
        .with_context(|| auth_token_error_message(config::AuthType::Browser, &path))
}

/// Create the Config and Data directories for the app if they do not already
/// exist. Returns an error if unsuccesful.
async fn initialise_directories() -> anyhow::Result<()> {
    let config_dir = get_config_dir()?;
    let data_dir = get_data_dir()?;
    tokio::try_join!(
        tokio::fs::create_dir_all(config_dir),
        tokio::fs::create_dir_all(data_dir),
    )?;
    Ok(())
}

async fn load_api_key(cfg: &Config) -> anyhow::Result<ApiKey> {
    let api_key = match cfg.auth_type {
        config::AuthType::Browser => ApiKey::BrowserToken(load_cookie_file().await?),
        config::AuthType::Unauthenticated => ApiKey::None,
    };
    Ok(api_key)
}

/// Return a URL to exact README guide, or information
/// to help a user find needed information without finding
/// the repo's README if they closed it in their browser.
fn auth_token_readme_link(token_type: config::AuthType) -> &'static str {
    match token_type {
        config::AuthType::Browser => BROWSER_AUTH_SETUP_STEPS_URL,
        config::AuthType::Unauthenticated => RUNNING_YOUTUI_GUIDE_URL,
    }
}

fn auth_token_error_message(token_type: config::AuthType, path: &Path) -> String {
    format!(
        "Error loading {:?} auth token from {}. Does the file exist? See README.md for more information: {}",
        token_type,
        path.display(),
        auth_token_readme_link(token_type)
    )
}
