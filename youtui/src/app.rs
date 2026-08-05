use super::appevent::{AppEvent, EventHandler};
use crate::config::ApiKey;
use crate::core::get_limited_sequential_file;
use crate::{RuntimeInfo, get_data_dir, detect_browser_source};
use crate::{COOKIE_NETSCAPE_FILENAME, get_config_dir};
use anyhow::{Context, Result, bail};
use component::actionhandler::YoutuiEffect;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use effect::TaskResult;
use media_controls::MediaController;
use queue_persistence::auto_save;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use server::Server;
use std::borrow::Cow;
use std::fmt::Display;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use server::song_downloader;
use structures::ListSong;
use tracing::{debug, error, info, warn};
use tracing_subscriber::prelude::*;
use ytmapi_rs::common::YoutubeID;
use ui::{WindowContext, YoutuiWindow};

#[macro_use]
pub mod component;
pub mod effect;
mod media_controls;
pub mod queue_persistence;
mod server;
pub(crate) mod structures;
pub mod ui;
pub mod view;

// We need this thread_local to ensure we know which is the main thread. Panic
// hook that destructs terminal should only run on the main thread.
thread_local! {
    static IS_MAIN_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

const CALLBACK_CHANNEL_SIZE: usize = 64;
const EVENT_CHANNEL_SIZE: usize = 256;
const LOG_FILE_NAME: &str = "debug";
const LOG_FILE_EXT: &str = "log";
const MAX_LOG_FILES: u16 = 5;

pub struct Youtui {
    status: AppStatus,
    event_handler: EventHandler,
    window_state: YoutuiWindow,
    task_manager: effect::TaskManager<YoutuiWindow>,
    server: Arc<Server>,
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    // Optional as may be disabled at runtime.
    media_controls: Option<MediaController>,
}

#[derive(PartialEq)]
pub enum AppStatus {
    Running,
    // Cow: Message
    Exiting(Cow<'static, str>),
}

// A callback from one of the application components to the top level.
#[derive(Debug)]
#[must_use]
pub enum AppCallback {
    Quit,
    ChangeContext(WindowContext),
    AddSongsToPlaylist(Vec<ListSong>),
    AddSongsToPlaylistAndPlay(Vec<ListSong>),
}

impl Youtui {
    pub async fn new(rt: RuntimeInfo) -> Result<Youtui> {
        let RuntimeInfo {
            api_key,
            debug,
            po_token,
            config,
            disable_media_controls,
        } = rt;
        // Setup tracing and link to tui_logger.
        // NOTE: File logging is always enabled for now - I can't think of a use case
        // where we wouldn't want this.
        init_tracing(debug, true).await?;
        match debug {
            true => info!("Starting in debug mode"),
            false => info!("Starting"),
        }
        // Youtui is not designed to try to bypass youtube music advertising.
        // Authentication is required to use it.
        if let ApiKey::None = api_key {
            bail!("Authentication is required to run youtui");
        }
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture,)?;
        // By only performing panic cleanup from the main thread, this largely prevents
        // exits that occur part-way through a redraw.
        IS_MAIN_THREAD.with(|flag| flag.set(true));
        std::panic::set_hook(Box::new(|panic_info| {
            if IS_MAIN_THREAD.with(|flag| flag.get()) {
                error!(
                    "Panic detected on main thread. \
                     Message: {panic_info}"
                );
                // If we fail to exit cleanly, ignore the error as panicking anyway.
                let _ = cleanup_tui_and_print_panic_message(&panic_info);
            } else {
                warn!(
                    "Panic detected outside main thread - \
                     this is not necessarily an error but may indicate one. \
                     Message: {panic_info}"
                );
            }
        }));
        let js_runtime = if std::process::Command::new("node").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok() {
            Some("node".to_string())
        } else {
            None
        };
        let (cookie_path, did_export) = if let Some(browser) = detect_browser_source() {
            let mut cp = get_config_dir()?;
            cp.push(COOKIE_NETSCAPE_FILENAME);
            let exported = if cookie_export_needed(&cp) {
                debug!(browser = %browser, path = %cp.display(), "Exporting cookies from browser");
                export_browser_cookies(&browser, &cp, &config.yt_dlp_command)
            } else {
                false
            };
            (Some(cp), exported)
        } else {
            (None, false)
        };
        match &cookie_path {
            Some(cp) if server::song_downloader::resolve::file_has_auth_cookie(cp) => {
                info!(path = %cp.display(), "auth: signed-in (browser cookies)")
            }
            Some(cp) => info!(
                path = %cp.display(),
                "auth: browser cookies are guest-only (no signed-in session)"
            ),
            None => info!("auth: no browser detected — guest"),
        }

        // Setup components
        let task_manager = effect::TaskManager::<YoutuiWindow>::new();
        let t_server = std::time::Instant::now();
        let server = Arc::new(server::Server::new(api_key, po_token, &config, cookie_path, js_runtime)?);
        debug!(
            "startup_timing: Server::new() = {}ms",
            t_server.elapsed().as_millis()
        );
        if did_export {
            debug!("startup_timing: cookie export included");
        }
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        let (media_controls, media_control_event_stream) = if disable_media_controls {
            (None, None)
        } else {
            let (media_controls, media_control_event_stream) = MediaController::new(
                config.notifications_enabled,
            )
            .context("Unable to initialise media controls - is the application already running?")?;
            (Some(media_controls), Some(media_control_event_stream))
        };
        let event_handler = EventHandler::new(EVENT_CHANNEL_SIZE, media_control_event_stream)?;
        server::song_downloader::set_cache_max_entries(config.download_cache_size);
        let (mut window_state, effect) = YoutuiWindow::new(config);
        // Even the creation of a YoutuiWindow causes an effect. We'll spawn it straight
        // away.
        task_manager.spawn(&server, effect);

        // Auto-load playlist from previous session (if any)
        let t_load = std::time::Instant::now();
        match queue_persistence::auto_load(&mut window_state.playlist) {
            Ok(load_effect) => {
                let song_count = window_state.playlist.list.get_list_iter().count();
                info!(
                    "Auto-loaded {} songs from __autosave.json in {}ms",
                    song_count,
                    t_load.elapsed().as_millis()
                );
                // If a saved queue was loaded, open on the playlist view
                if window_state.playlist.loaded_from_autosave() {
                    window_state.handle_change_context(WindowContext::Playlist);
                }
                task_manager.spawn(
                    &server,
                    load_effect.map(|w: &mut YoutuiWindow| &mut w.playlist),
                );
            }
            Err(e) => {
                debug!("Auto-load failed ({}). Starting with empty playlist.", e);
            }
        }

        // Pre-resolve the first song's URL to avoid cold 2-4s latency on first play.
        if let Some(first_song) = window_state.playlist.list.get_list_iter().next() {
            let vid = first_song.video_id.get_raw().to_string();
            let yt_cmd = server.config.yt_dlp_command.clone();
            let pt = server.po_token.clone();
            let cp = server.cookie_path.clone();
            let jr = server.js_runtime.clone();
            let ch = server.cookie_header.clone();
            tokio::spawn(async move {
                song_downloader::resolve_url(&vid, &yt_cmd, pt.as_deref(), cp.as_deref(), ch.as_deref(), jr.as_deref(), None).await;
            });
        }

        Ok(Youtui {
            status: AppStatus::Running,
            event_handler,
            window_state,
            task_manager,
            server,
            terminal,
            media_controls,
        })
    }
    async fn render_and_process_events(&mut self) -> Result<()> {
        #[cfg(feature = "profile-render")]
        let _render_start = std::time::Instant::now();
        self.terminal.draw(|f| {
            ui::draw::draw_app(f, &mut self.window_state);
        })?;
        #[cfg(feature = "profile-render")]
        {
            let elapsed = _render_start.elapsed();
            if elapsed.as_millis() > 8 {
                warn!(
                    "profile-render: draw took {}ms (>8ms threshold)",
                    elapsed.as_millis()
                );
            }
        }
        if let Some(media_controls) = &mut self.media_controls {
            media_controls.update_controls(ui::draw_media_controls::draw_app_media_controls(
                &self.window_state,
            ))?;
        }
        tokio::select! {
            Some(event) = self.event_handler.next() =>
                self.handle_event(event).await,
            Some(outcome) = self.task_manager.get_next_response() =>
                self.handle_effect(outcome),
        }
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            match &self.status {
                AppStatus::Running => {
                    self.render_and_process_events().await?;
                }
                AppStatus::Exiting(s) => {
                    auto_save(&self.window_state.playlist)
                        .unwrap_or_else(|e| warn!("Failed to auto-save queue on exit: {e}"));
                    destruct_terminal()?;
                    println!("{s}");
                    break;
                }
            }
        }
        Ok(())
    }
    fn handle_effect(&mut self, result: TaskResult<YoutuiWindow>) {
        match result {
            TaskResult::Mutation(mutation) => {
                let next = mutation(&mut self.window_state);
                self.task_manager.spawn(&self.server, next);
            }
            TaskResult::StreamFinished => {
                debug!("Stream task finished");
            }
            TaskResult::Panic(msg) => {
                error!("Task panicked: {msg}");
                let _ = cleanup_tui_and_print_panic_message(&msg);
            }
        }
    }
    async fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Tick => self.window_state.handle_tick().await,
            AppEvent::Crossterm(e) => {
                let YoutuiEffect { effect, callback } =
                    self.window_state.handle_crossterm_event(e).await;
                self.task_manager.spawn(&self.server, effect);
                if let Some(callback) = callback {
                    self.handle_callback(callback);
                }
            }
            AppEvent::MediaControls(e) => {
                let YoutuiEffect { effect, callback } =
                    self.window_state.handle_media_controls_event(e).await;
                self.task_manager.spawn(&self.server, effect);
                if let Some(callback) = callback {
                    self.handle_callback(callback);
                }
            }
            AppEvent::QuitSignal => self.status = AppStatus::Exiting("Quit signal received".into()),
        }
    }
    pub fn handle_callback(&mut self, callback: AppCallback) {
        match callback {
            AppCallback::Quit => self.status = AppStatus::Exiting("Quitting".into()),
            AppCallback::ChangeContext(context) => self.window_state.handle_change_context(context),
            AppCallback::AddSongsToPlaylist(song_list) => self.task_manager.spawn(
                &self.server,
                self.window_state.handle_add_songs_to_playlist(song_list),
            ),
            AppCallback::AddSongsToPlaylistAndPlay(song_list) => self.task_manager.spawn(
                &self.server,
                self.window_state
                    .handle_add_songs_to_playlist_and_play(song_list),
            ),
        }
    }
}

/// When panicking in the tui, terminal cleanup and error message must be in the
/// correct order.
fn cleanup_tui_and_print_panic_message(panic: &impl Display) -> Result<()> {
    destruct_terminal()?;
    println!("{panic}");
    Ok(())
}

/// Cleanly exit the tui
fn destruct_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        crossterm::cursor::Show
    )?;
    Ok(())
}

/// Initialise tracing and subscribers such as tuilogger and file logging.
/// # Panics
/// If tracing fails to initialise, function will panic
async fn init_tracing(debug: bool, logging: bool) -> Result<()> {
    let tracing_log_level = if debug {
        tracing::Level::DEBUG
    } else {
        tracing::Level::WARN
    };
    if logging {
        let context_layer =
            tracing_subscriber::filter::Targets::new().with_target("youtui", tracing_log_level);
        let (log_file, log_file_name) = get_limited_sequential_file(
            &get_data_dir()?,
            LOG_FILE_NAME,
            LOG_FILE_EXT,
            MAX_LOG_FILES,
        )
        .await?;
        let log_file = log_file
            .try_into_std()
            .map_err(|_| anyhow::anyhow!("log file busy, cannot convert to std handle"))?;
        let log_file_layer = tracing_subscriber::fmt::layer().with_writer(Arc::new(log_file));
        tracing_subscriber::registry()
            .with(log_file_layer)
            .with(context_layer)
            .init();
        info!("Logging to {:?}.", log_file_name);
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::filter::Targets::new().with_target("youtui", tracing_log_level),
            )
            .init();
    }
    Ok(())
}

/// How long a browser cookie export is trusted before being refreshed. 7 days
/// covers the common "works fine for a week, then songs skip" failure: the
/// export is still present and non-empty, but the tokens it holds have been
/// rotated or throttled by YouTube.
const COOKIE_EXPORT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// URL yt-dlp is pointed at so it dumps browser cookies into the Netscape
/// export file. The `.invalid` TLD is reserved (RFC 2606) and never resolves,
/// so yt-dlp fails fast — *after* extracting and writing the cookies at
/// startup. No network and no external video is required for the export.
const COOKIE_EXPORT_PROBE_URL: &str = "https://cookie-export.invalid/";

/// True when the exported Netscape cookie file should be regenerated: it is
/// missing, empty (the historical 0-byte export trap), or older than
/// `COOKIE_EXPORT_TTL`. Self-heals a stale export on every startup.
fn cookie_export_needed(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    if meta.len() == 0 {
        return true;
    }
    let Ok(modified) = meta.modified() else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .map_or(true, |age| age > COOKIE_EXPORT_TTL)
}

/// Run a single yt-dlp cookie export to `out`. yt-dlp refuses to run without a
/// URL and, against a live browser profile, the probe URL's own extraction can
/// legitimately fail (storyboards-only) while the cookies were already written.
/// Success is therefore a *non-empty cookie file*, not the exit code — the
/// stderr is only surfaced when the file came out empty.
fn run_one_cookie_export(cmd: &str, browser: &str, out: &Path) -> Result<(), String> {
    let output = std::process::Command::new(cmd)
        .args([
            "--ignore-config",
            "--cookies-from-browser",
            browser,
            "--cookies",
        ])
        .arg(out)
        .args(["--simulate", COOKIE_EXPORT_PROBE_URL])
        .stdout(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if std::fs::metadata(out).map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(());
    }
    let _ = std::fs::remove_file(out);
    Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
}

/// Resolve the on-disk profile directory for a browser spec like
/// `firefox:/home/u/.floorp/PROFILE`. Bare `firefox` (no path) resolves to the
/// default profile via profiles.ini, mirroring `detect_browser_source`. Returns
/// `None` when the profile cannot be located.
fn resolve_profile_dir(browser: &str) -> Option<(String, PathBuf)> {
    if let Some((name, path)) = browser.split_once(':') {
        let dir = PathBuf::from(path);
        return dir.join("cookies.sqlite").exists().then(|| (name.to_string(), dir));
    }
    if browser == "firefox" {
        let home = PathBuf::from(std::env::var("HOME").ok()?);
        for ini in [
            home.join(".mozilla").join("firefox").join("profiles.ini"),
            home.join(".config").join("mozilla").join("firefox").join("profiles.ini"),
        ] {
            if let Ok(content) = std::fs::read_to_string(&ini)
                && let Some(parent) = ini.parent()
            {
                for line in content.lines() {
                    if let Some(p) = line.strip_prefix("Path=") {
                        let dir = parent.join(p.trim());
                        if dir.join("cookies.sqlite").exists() {
                            return Some((browser.to_string(), dir));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Export cookies from a copy of the profile's cookie database instead of the
/// live one. The live DB is locked while the browser is running, but a copy is
/// always readable, so yt-dlp can export signed-in cookies regardless of
/// whether the browser is open. Returns the copy spec + its parent dir (to be
/// removed by the caller), or `None` when the profile cannot be copied.
fn copy_cookie_db_for_export(browser: &str) -> Option<(String, PathBuf)> {
    let (name, profile_dir) = resolve_profile_dir(browser)?;
    let src = profile_dir.join("cookies.sqlite");
    if !src.exists() {
        return None;
    }
    let task_dir = std::env::temp_dir().join(format!("youtui-cookies-{}", std::process::id()));
    let db_dir = task_dir.join("profile");
    if std::fs::create_dir_all(&db_dir).is_err() {
        return None;
    }
    let mut copied = false;
    for f in ["cookies.sqlite", "cookies.sqlite-wal", "cookies.sqlite-shm"] {
        let p = profile_dir.join(f);
        if p.exists() && std::fs::copy(&p, db_dir.join(f)).is_ok() {
            copied = true;
        }
    }
    if !copied {
        let _ = std::fs::remove_dir_all(&task_dir);
        return None;
    }
    Some((format!("{name}:{}", db_dir.display()), task_dir))
}

/// Run yt-dlp's cookie export. Returns `Err(stderr)` on a non-zero exit so the
/// caller can report *why* the export failed (locked browser profile, wrong
/// browser, unsupported DB, ...) instead of a bare "Failed". Writes to a
/// sibling temp file so a failed/partial export can never corrupt the live
/// cookie file. When the live profile DB is locked (browser running) or yields
/// nothing, retries from a copy of the cookie database, which is never locked.
fn run_cookie_export(cmd: &str, browser: &str, dest: &Path) -> Result<(), String> {
    let tmp = dest.with_extension("tmp");
    let direct_err = match run_one_cookie_export(cmd, browser, &tmp) {
        Ok(()) if std::fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false) => return Ok(()),
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            "export produced an empty file".to_string()
        }
        Err(e) => e,
    };
    if let Some((copied_browser, task_dir)) = copy_cookie_db_for_export(browser) {
        let res = run_one_cookie_export(cmd, &copied_browser, &tmp);
        let _ = std::fs::remove_dir_all(&task_dir);
        match res {
            Ok(()) if std::fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false) => return Ok(()),
            _ => {}
        }
    }
    Err(direct_err)
}

/// Export the browser's cookies to a Netscape file, reusing yt-dlp. Returns
/// false (and warns with the actual failure reason) on any failure — a silent
/// failed export is what left a 0-byte file that disabled downloads for weeks.
/// Honors the configured `yt_dlp_command` so the export always uses the same
/// binary as downloads. Publish is atomic: the live file is only replaced by a
/// non-empty export, so a failed re-export preserves the last good cookies.
fn export_browser_cookies(browser: &str, dest: &Path, yt_dlp_cmd: &str) -> bool {
    // Mirror the download path's empty-string fallback (song_downloader).
    let cmd = if yt_dlp_cmd.is_empty() { "yt-dlp" } else { yt_dlp_cmd };
    let tmp = dest.with_extension("tmp");
    let reason = match run_cookie_export(cmd, browser, dest) {
        Ok(()) => {
            if std::fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false) {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&tmp)
                    .map(|m| m.permissions())
                    .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o600));
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&tmp, perms);
                if std::fs::rename(&tmp, dest).is_ok() {
                    info!("Exported cookies to {}", dest.display());
                    return true;
                }
                let _ = std::fs::remove_file(&tmp);
                return false;
            }
            let _ = std::fs::remove_file(&tmp);
            "export succeeded but produced an empty file".to_string()
        }
        Err(err) => err,
    };
    warn!(
        "Failed to export cookies from {browser} to {}: {reason}",
        dest.display()
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_export_needed_missing_is_true() {
        let missing = std::env::temp_dir().join("youtui_export_missing.txt");
        let _ = std::fs::remove_file(&missing);
        assert!(cookie_export_needed(&missing));
    }

    #[test]
    fn cookie_export_needed_empty_is_true() {
        let path = std::env::temp_dir().join("youtui_export_empty.txt");
        std::fs::write(&path, b"").unwrap();
        assert!(cookie_export_needed(&path));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cookie_export_needed_fresh_is_false() {
        let path = std::env::temp_dir().join("youtui_export_fresh.txt");
        std::fs::write(&path, b".youtube.com\tTRUE\t/\tTRUE\t1735689600\tSAPISID\tabc\n").unwrap();
        assert!(!cookie_export_needed(&path));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cookie_export_needed_old_mtime_is_true() {
        let path = std::env::temp_dir().join("youtui_export_old.txt");
        std::fs::write(&path, b".youtube.com\tTRUE\t/\tTRUE\t1735689600\tSAPISID\tabc\n").unwrap();
        let old = std::time::SystemTime::now()
            - Duration::from_secs(COOKIE_EXPORT_TTL.as_secs() + 3600);
        let f = std::fs::File::open(&path).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
        assert!(cookie_export_needed(&path));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn export_browser_cookies_uses_configured_command() {
        // A fake "yt-dlp" whose only job is to write the cookie destination file.
        let script = std::env::temp_dir().join(format!("youtui_fake_ytdlp_{}.sh", std::process::id()));
        std::fs::write(&script, "#!/bin/sh\necho 'SID=abc' > \"$5\"\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let dest = std::env::temp_dir().join(format!("youtui_export_cfg_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        // Config path must be honored: the fake binary runs, not the real yt-dlp.
        assert!(export_browser_cookies("fake-browser", &dest, script.to_str().unwrap()));
        assert!(std::fs::metadata(&dest).map(|m| m.len() > 0).unwrap_or(false));

        std::fs::remove_file(&script).ok();
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn export_browser_cookies_empty_command_falls_back() {
        // Empty configured command must resolve to "yt-dlp" (like the download
        // path) rather than attempting `Command::new("")`.
        let dest = std::env::temp_dir().join(format!("youtui_export_empty_cmd_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        // No such browser -> real yt-dlp (or missing binary) fails -> false.
        // Deterministic: the test only asserts no panic + bool return.
        let ok = export_browser_cookies("youtui-no-such-browser", &dest, "");
        assert!(!ok);
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn run_cookie_export_reports_failure_stderr() {
        // A fake yt-dlp that fails loudly. The exported failure reason must
        // surface so users can tell "browser locked" from "no cookies found".
        let script = std::env::temp_dir().join(format!("youtui_fail_export_{}.sh", std::process::id()));
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'LockedProfileException: profile is locked' >&2\nexit 1\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let dest = std::env::temp_dir().join(format!("youtui_export_fail_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        let err = run_cookie_export(script.to_str().unwrap(), "fake-browser", &dest)
            .expect_err("failing export must produce an Err");
        assert!(
            err.contains("LockedProfileException"),
            "stderr must be surfaced, got: {err:?}"
        );
        assert!(!dest.exists(), "failed export must not leave a file");
        assert!(
            !dest.with_extension("tmp").exists(),
            "failed export must clean up its temp file"
        );

        std::fs::remove_file(&script).ok();
    }

    #[test]
    fn run_cookie_export_falls_back_to_copied_profile_when_locked() {
        use std::os::unix::fs::PermissionsExt;
        // A fake Firefox profile whose cookies.sqlite exists but whose live DB
        // "export" fails (simulates the locked-profile case: browser running).
        let srcdir = std::env::temp_dir().join(format!("youtui_fake_profile_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&srcdir);
        std::fs::create_dir_all(&srcdir).unwrap();
        std::fs::write(srcdir.join("cookies.sqlite"), b"cookie-db-bytes").unwrap();

        // Fake yt-dlp: fails when pointed at the original profile (locked), but
        // succeeds when pointed at a copied profile (which the fallback makes).
        let script = std::env::temp_dir().join(format!("youtui_copy_export_{}.sh", std::process::id()));
        let orig = srcdir.to_str().unwrap();
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nbr='' out='' nxt=''\nfor a in \"$@\"; do\n  if [ \"$nxt\" = browser ]; then br=\"$a\"; nxt=''; fi\n  if [ \"$nxt\" = cookies ]; then out=\"$a\"; nxt=''; fi\n  if [ \"$a\" = --cookies-from-browser ]; then nxt=browser; fi\n  if [ \"$a\" = --cookies ]; then nxt=cookies; fi\ndone\ncase \"$br\" in *{orig}*) exit 1;; esac\nprintf '.youtube.com\\tTRUE\\t/\\tTRUE\\t1819286662\\tSID\\tabc\\n' > \"$out\"\nexit 0\n",
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let dest = std::env::temp_dir().join(format!("youtui_export_copy_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        let browser = format!("firefox:{}", srcdir.display());

        let res = run_cookie_export(script.to_str().unwrap(), &browser, &dest);
        assert!(res.is_ok(), "copy fallback must recover from locked profile, got: {res:?}");
        assert!(
            std::fs::metadata(dest.with_extension("tmp"))
                .map(|m| m.len() > 0)
                .unwrap_or(false),
            "fallback export must write a non-empty cookie file"
        );

        // The temp copy of the profile must be cleaned up afterwards.
        let task_dir = std::env::temp_dir().join(format!("youtui-cookies-{}", std::process::id()));
        assert!(!task_dir.exists(), "temporary profile copy must be removed");

        std::fs::remove_file(&script).ok();
        std::fs::remove_dir_all(&srcdir).ok();
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn export_browser_cookies_failure_preserves_existing_dest() {
        use std::os::unix::fs::PermissionsExt;
        // A pre-existing valid export must survive a subsequent failed
        // re-export (the historical 0-byte-trap). The fake yt-dlp writes an
        // empty output then fails — under the new success rule (non-empty
        // file) that is a true failure, proving the atomic publish.
        let script = std::env::temp_dir().join(format!("youtui_corrupt_export_{}.sh", std::process::id()));
        std::fs::write(
            &script,
            "#!/bin/sh\nout='' nxt=''\nfor a in \"$@\"; do\n  if [ \"$nxt\" = cookies ]; then out=\"$a\"; nxt=''; fi\n  if [ \"$a\" = --cookies ]; then nxt=cookies; fi\ndone\n: > \"$out\"\nexit 1\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let dest = std::env::temp_dir().join(format!("youtui_export_atomic_{}.txt", std::process::id()));
        std::fs::write(&dest, b".youtube.com\tTRUE\t/\tTRUE\t1735689600\tSID\tabc\n").unwrap();

        let ok = export_browser_cookies("fake-browser", &dest, script.to_str().unwrap());
        assert!(!ok, "failing export must report failure");
        let content = std::fs::read_to_string(&dest).unwrap();
        assert!(
            content.contains("SID"),
            "prior good export must be preserved, got: {content:?}"
        );
        assert!(
            !dest.with_extension("tmp").exists(),
            "failed export must not leave a temp file"
        );

        std::fs::remove_file(&script).ok();
        std::fs::remove_file(&dest).ok();
    }
}
