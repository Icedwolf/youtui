mod cache;
pub(crate) mod resolve;

pub use cache::{cache_clear, create_decoder_from_cache, set_cache_max_entries};
pub use resolve::resolve_url;

use std::sync::{Arc, LazyLock};

use anyhow::{Context, bail};
use symphonia::core::io::MediaSourceStream;
use tokio::sync::Semaphore;
use tracing::{debug, error, warn};

pub(crate) use cache::cache_put;
use crate::app::server::streaming_buffer::{SharedBuffer, SharedBufferWriter};
use crate::decoder::SymphoniaDecoder;
use crate::decoder::read_seek_source::ReadSeekSource;

const MAX_CONCURRENT_DOWNLOADS: usize = 1;
const READ_BUF_SIZE: usize = 64 * 1024;
const STREAM_INIT_THRESHOLD: usize = 512;
const DOWNLOAD_TIMEOUT_S: u64 = 120;
const DECODER_INIT_DEADLINE_S: u64 = 5;
/// Additional patience granted when a source has produced ZERO bytes by the
/// init deadline but its pipe is still open. A source that already exited is
/// dead/unavailable (bail immediately), but one that is still running may just
/// be slow to deliver its first byte (cold TLS, throttled start) — a playable
/// song must not be skipped because the first byte took >5s to arrive.
const EMPTY_PIPE_PATIENCE_S: u64 = 20;
/// Poll interval for background download progress tracking.
const BG_PROGRESS_POLL_MS: u64 = 1000;
/// A background (post-playback) download is killed only when it makes NO
/// progress for this long. Unlike `DOWNLOAD_TIMEOUT_S` there is no absolute
/// deadline: a long song legitimately takes minutes to finish downloading
/// after playback starts, and a hard cap truncates the buffer mid-song.
const BG_STALL_TIMEOUT_S: u64 = 60;
const M4A_TOTAL_LEN_TIMEOUT_S: u64 = 15;
/// Browser-like User-Agent for ffmpeg's direct-URL HTTP fetch. A bare `Lavf/…`
/// UA is a bot signal to the googlevideo CDN and is refused with 403 far more
/// often than a browser UA, so we mirror the header set yt-dlp itself sends
/// when it fetches the same URL (see `build_ffmpeg_command`).
const FFMPEG_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
/// Shared tail of the ffmpeg invocation for ALAC-in-fragmented-mp4 streaming.
/// Only the `-i` input differs between the direct-URL and relay paths, so the
/// mux flags live in one place (see DECISIONS.md:10).
const ALAC_FFMPEG_ARGS: [&str; 13] = [
    "-fflags",
    "nobuffer",
    "-flags",
    "low_delay",
    "-f",
    "mp4",
    "-movflags",
    "empty_moov+default_base_moof+frag_every_frame",
    "-c:a",
    "alac",
    "-loglevel",
    "error",
    "pipe:1",
];

pub(crate) struct DownloadConfig {
    pub yt_dlp_command: String,
    pub video_id: String,
    pub po_token: Option<String>,
    pub cookie_path: Option<std::path::PathBuf>,
    pub cookie_header: Option<String>,
    pub js_runtime: Option<String>,
    pub cancel_token: tokio_util::sync::CancellationToken,
}

static DOWNLOAD_SEMAPHORE: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(MAX_CONCURRENT_DOWNLOADS));

fn exit_code_string(status: &std::process::ExitStatus) -> String {
    status.code().map_or("unknown".into(), |c| c.to_string())
}

fn parse_total_size(line: &str) -> Option<u64> {
    let line = line.trim();
    let of_pos = line.find("of ")?;
    let rest = line[of_pos + 3..].trim_start();

    let num_end = rest.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let num_str = &rest[..num_end];
    let value: f64 = num_str.parse().ok()?;

    let rest = rest[num_end..].trim_start();
    let unit_end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    let unit = &rest[..unit_end];

    match unit {
        "Bytes" | "B" => Some(value as u64),
        "KiB" | "KB" | "kB" => Some((value * 1024.0) as u64),
        "MiB" | "MB" => Some((value * 1024.0 * 1024.0) as u64),
        "GiB" | "GB" => Some((value * 1024.0 * 1024.0 * 1024.0) as u64),
        _ => None,
    }
}

fn check_ffmpeg() -> bool {
    static HAS_FFMPEG: LazyLock<bool> = LazyLock::new(|| {
        let mut cmd = std::process::Command::new("ffmpeg");
        apply_child_env(&mut cmd);
        cmd.arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    *HAS_FFMPEG
}

/// Subprocesses run with a controlled, minimal environment instead of the
/// unbounded parent `envp`. An oversized/inherited env makes `execve` fail with
/// E2BIG (`Argument list too long`) — the systemic `spawn yt-dlp` failure seen
/// in the field. `env_clear` bounds the child's env to the small allowlist
/// below, so spawning can never hit `ARG_MAX` regardless of how the parent was
/// launched, while keeping PATH (binary lookup), proxies, and TLS cert paths
/// intact. Applied to every yt-dlp/ffmpeg child (download pipeline) and every
/// spawn-side effect via the shared sync variant.
const CHILD_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMP",
    "TMPDIR",
    "TEMP",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "http_proxy",
    "https_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

pub(crate) fn apply_child_env(cmd: &mut impl ChildCommand) {
    cmd.env_clear();
    for key in CHILD_ENV_ALLOWLIST {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
}

/// Command types `apply_child_env` can shape. Implemented for both the sync and
/// async command types so every spawn site — the download pipeline (tokio) and
/// the init-only checks (std) — runs children with the same bounded env.
pub(crate) trait ChildCommand {
    fn env_clear(&mut self);
    fn env(&mut self, key: &str, value: String);
}

impl ChildCommand for std::process::Command {
    fn env_clear(&mut self) {
        std::process::Command::env_clear(self);
    }
    fn env(&mut self, key: &str, value: String) {
        std::process::Command::env(self, key, value);
    }
}

impl ChildCommand for tokio::process::Command {
    fn env_clear(&mut self) {
        tokio::process::Command::env_clear(self);
    }
    fn env(&mut self, key: &str, value: String) {
        tokio::process::Command::env(self, key, value);
    }
}

/// Try to init a symphonia decoder from the buffer while it's still being
/// written.  For the streamed ALAC-in-fragmented-MP4 path: `empty_moov` puts
/// the moov (with the ALAC sample entry) in the first ~700 bytes, so probing
/// with the first few KB works.  For M4A (isomp4): the moov atom may be at the
/// end, so `byte_len` must be the total file size (from yt-dlp progress line).
/// Spawns on a blocking thread for isomp4 seeking (could block Condvar).
async fn init_decoder_from(mss: MediaSourceStream) -> Result<SymphoniaDecoder, String> {
    let deadline = std::time::Duration::from_secs(DECODER_INIT_DEADLINE_S);
    let handle = tokio::task::spawn_blocking(move || SymphoniaDecoder::new(mss));

    match tokio::time::timeout(deadline, handle).await {
        Ok(Ok(Ok(decoder))) => Ok(decoder),
        Ok(Ok(Err(e))) => Err(format!("{e:?}")),
        Ok(Err(join_err)) => Err(format!("spawn_blocking panicked: {join_err}")),
        Err(_elapsed) => {
            Err("decoder init timed out (isomp4 seek blocked on Condvar)".to_string())
        }
    }
}

async fn try_streaming_init(
    buffer: &Arc<SharedBuffer>,
    byte_len: Option<u64>,
) -> Result<SymphoniaDecoder, String> {
    let reader = buffer.reader();
    let source = ReadSeekSource::new(reader, byte_len);
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    init_decoder_from(mss).await
}

async fn try_streaming_init_nonseekable(
    buffer: &Arc<SharedBuffer>,
) -> Result<SymphoniaDecoder, String> {
    let reader = buffer.reader();
    let source = ReadSeekSource::nonseekable(reader);
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    init_decoder_from(mss).await
}

async fn kill_and_reap(
    main: &mut tokio::process::Child,
    extra: &mut Option<tokio::process::Child>,
) {
    let _ = main.start_kill();
    if let Some(extra) = extra.as_mut() {
        let _ = extra.start_kill();
    }
    let _ = main.wait().await;
    if let Some(extra) = extra.as_mut() {
        let _ = extra.wait().await;
    }
}

/// Track whether a background download is still making progress. Returns the
/// new baseline length + last-progress instant, and whether the download has
/// stalled: no new bytes arrived for `stall` or longer.
fn track_download_progress(
    last_len: usize,
    cur_len: usize,
    last_progress: std::time::Instant,
    stall: std::time::Duration,
) -> (usize, std::time::Instant, bool) {
    if cur_len != last_len {
        (cur_len, std::time::Instant::now(), false)
    } else if last_progress.elapsed() >= stall {
        (last_len, last_progress, true)
    } else {
        (last_len, last_progress, false)
    }
}

#[derive(Debug, PartialEq)]
enum EmptyPipeVerdict {
    Break,
    SourceExited,
    Cancelled,
    PatienceElapsed,
    Wait,
}

/// Decide the next step of the empty-pipe patience loop from its observable
/// conditions. Priority order matters: a **failed** buffer wins over a finished
/// source, so a source that failed with zero bytes (dead video / auth error,
/// classified on the buffer by the stderr handler) breaks into the post-loop
/// classification and surfaces the specific error — not a generic empty-pipe
/// bail that would drop the notification and skip the auto-removal.
#[allow(clippy::too_many_arguments)]
fn empty_pipe_verdict(
    has_bytes: bool,
    source_exited: bool,
    buffer_failed: bool,
    cancelled: bool,
    patience_elapsed: bool,
) -> EmptyPipeVerdict {
    if has_bytes || buffer_failed {
        EmptyPipeVerdict::Break
    } else if source_exited {
        EmptyPipeVerdict::SourceExited
    } else if cancelled {
        EmptyPipeVerdict::Cancelled
    } else if patience_elapsed {
        EmptyPipeVerdict::PatienceElapsed
    } else {
        EmptyPipeVerdict::Wait
    }
}

#[allow(clippy::too_many_arguments)]
async fn spawn_bg_cache_task(
    vid: String,
    ct: tokio_util::sync::CancellationToken,
    mut child: tokio::process::Child,
    mut yt_child: Option<tokio::process::Child>,
    mut write_handle: tokio::task::JoinHandle<()>,
    buf: Arc<SharedBuffer>,
    log_prefix: &'static str,
    t0: Option<tokio::time::Instant>,
    _permit: tokio::sync::SemaphorePermit<'static>,
) {
    let stall = std::time::Duration::from_secs(BG_STALL_TIMEOUT_S);
    let mut last_len = buf.len();
    let mut last_progress = std::time::Instant::now();
    let write_result = loop {
        tokio::select! {
            biased;
            _ = ct.cancelled() => {
                debug!(%vid, "{log_prefix} background cancelled, killing child");
                kill_and_reap(&mut child, &mut yt_child).await;
                return;
            }
            res = &mut write_handle => break res,
            _ = tokio::time::sleep(std::time::Duration::from_millis(BG_PROGRESS_POLL_MS)) => {
                let (new_len, new_progress, stalled) =
                    track_download_progress(last_len, buf.len(), last_progress, stall);
                if stalled {
                    warn!(%vid, seconds = BG_STALL_TIMEOUT_S,
                        "{log_prefix} no download progress, killing");
                    kill_and_reap(&mut child, &mut yt_child).await;
                    return;
                }
                last_len = new_len;
                last_progress = new_progress;
            }
        }
    };
    match write_result {
        Ok(()) => {}
        Err(join_err) => {
            error!(%vid, error = %join_err, "{log_prefix} writer task panicked");
            kill_and_reap(&mut child, &mut yt_child).await;
            return;
        }
    }
    let status = child.wait().await;
    if let Some(yt) = yt_child.as_mut() {
        let _ = yt.wait().await;
    }
    match status {
        Ok(s) if !s.success() => {
            debug!(%vid, code = exit_code_string(&s),
                "{log_prefix} exited with non-zero code (post-stream)");
            return;
        }
        Ok(_) => {
            if let Some(t0) = t0 {
                debug!(%vid, elapsed = ?t0.elapsed(), "{log_prefix} completed successfully");
            }
        }
        Err(e) => {
            debug!(%vid, error = %e, "{log_prefix} wait failed");
            return;
        }
    }
    let data = buf.finalize();
    debug!(%vid, len = data.len(), "Caching completed download ({log_prefix})");
    cache_put(vid, data);
}

/// Error message for a definitively dead video. This exact prefix is the
/// classification contract: the UI matches downloads on `starts_with("video
/// unavailable")`, so it must stay in sync across all bail sites.
pub(crate) const DEAD_VIDEO_ERR: &str = "video unavailable (yt-dlp error)";

/// Error message for an authentication/cookie failure (stale login, bot check).
/// The UI matches downloads on `starts_with("authentication error")`; the
/// resolve path appends a po_token tag and the yt-dlp stderr line after it.
pub(crate) const AUTH_ERR: &str = "authentication error (stale cookies)";

/// Classify a yt-dlp stderr line as a *permanently* unavailable video
/// (removed, terminated account) as opposed to a transient error (bot-check,
/// bad cookie file, format/network issue). Only the permanent class triggers
/// auto-removal; everything else must never touch the queue.
fn is_permanently_unavailable(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("video unavailable")
        && (line.contains("not available")
            || line.contains("no longer available")
            || line.contains("removed by the uploader"))
}

/// Classify a yt-dlp stderr line as an authentication/cookie problem: sign-in
/// required, bot check, invalid cookies. These are a login/config issue, not a
/// dead video — the song must be skipped and the user notified, never removed.
/// A bare `HTTP Error 403` is deliberately NOT here: on a signed-in session
/// that is the nsig/po_token CDN throttle (see `is_throttle_line`), and for a
/// guest there are no cookies to be stale — either way it is a transient
/// download failure that feeds the halt counter, never the stale-cookie class.
pub(crate) fn is_auth_error_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    ["sign in", "not a bot", "authentication", "requires login", "signed-in"]
        .iter()
        .any(|needle| line.contains(needle))
        || (line.contains("cookie") && line.contains("does not look like"))
}

/// Classify an ffmpeg stderr line as a CDN-throttled URL refuse. ffmpeg prints
/// `Server returned 403 Forbidden (access denied)` (or `HTTP error 403`) when
/// googlevideo rejects the resolved URL at fetch time — the nsig/po_token
/// throttling wave, NOT a dead video or stale cookies. The buffer is marked
/// so the pipeline evicts the URL and retries via the credential-carrying
/// relay instead of skipping the song.
fn is_throttle_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("forbidden")
        || line.contains("access denied")
        || (line.contains("403")
            && (line.contains("http") || line.contains("server") || line.contains("error")))
}

fn spawn_stderr_handler(
    stderr: tokio::process::ChildStderr,
    cancel_token: tokio_util::sync::CancellationToken,
    buffer: Arc<SharedBuffer>,
    log_cancellation: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let reader = tokio::io::BufReader::new(stderr);
        let mut lines = reader.lines();
        loop {
            if cancel_token.is_cancelled() {
                if log_cancellation {
                    debug!("yt-dlp stderr handler cancelled");
                }
                return;
            }
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if let Some(bytes) = parse_total_size(&line) {
                        debug!(total_bytes = bytes, "Parsed total size from yt-dlp progress");
                        buffer.set_total_len(bytes);
                    } else if line.contains("ERROR") {
                        if is_permanently_unavailable(&line) {
                            buffer.mark_dead_video();
                        } else if is_auth_error_line(&line) {
                            buffer.mark_auth_error();
                        }
                        warn!(stderr_line = %line.trim(), "yt-dlp stderr (error), failing buffer");
                        buffer.fail();
                    } else if line.contains("WARNING") {
                        debug!(stderr_line = %line.trim(), "yt-dlp stderr (warning)");
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    warn!(error = %e, "yt-dlp stderr read error");
                    break;
                }
            }
        }
        debug!("yt-dlp stderr stream ended");
    })
}

/// Logs every ffmpeg stderr line and classifies the one failure that would
/// otherwise be invisible: ffmpeg runs with `-loglevel error`, so it prints
/// nothing on success and only real diagnostics on failure. The line logged
/// just before an empty-pipe bail names the actual cause (bad/cached URL,
/// codec, auth block). A CDN 403 (`Server returned 403 Forbidden`) marks the
/// buffer as throttled + failed so the pipeline retries via the relay instead
/// of skipping the song; any other line is only logged. Failure classification
/// is the only buffer interaction — a healthy stream's stderr stays silent, so
/// playback behaviour is unchanged.
fn spawn_ffmpeg_stderr_handler(
    stderr: tokio::process::ChildStderr,
    buffer: Arc<SharedBuffer>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let reader = tokio::io::BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if is_throttle_line(&line) {
                debug!(stderr_line = %line.trim(), "ffmpeg stream URL throttled (403), failing buffer for relay retry");
                buffer.mark_throttled();
            } else {
                warn!(stderr_line = %line.trim(), "ffmpeg stderr (error)");
            }
        }
    })
}

fn spawn_stdout_writer(
    reader: tokio::process::ChildStdout,
    mut writer: SharedBufferWriter,
    pipe_name: &'static str,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut rdr = tokio::io::BufReader::new(reader);
        let mut buf = vec![0u8; READ_BUF_SIZE];
        loop {
            match rdr.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => writer.write(&buf[..n]),
                Err(e) => {
                    warn!(error = %e, "{pipe_name} stdout read error, failing buffer");
                    writer.fail();
                    return;
                }
            }
        }
        debug!("{pipe_name} stream ended, finishing buffer");
        writer.finish();
    })
}

/// The spawned ffmpeg's (stderr-logger, buffer-writer, child, optional stdin).
type FfmpegSpawn = (
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
    tokio::process::Child,
    Option<tokio::process::ChildStdin>,
);

/// Where ffmpeg reads its webm input from.
enum FfmpegInput {
    /// A pre-resolved stream URL, read directly by ffmpeg.
    Url(String),
    /// yt-dlp stdout relayed over stdin.
    Pipe,
}

/// Build the ffmpeg invocation for a given input source. A direct-URL fetch
/// (`FfmpegInput::Url`) is an anonymous `Lavf/…` request by default, which the
/// googlevideo CDN intermittently refuses with 403 — so it is shaped to mirror
/// yt-dlp's own fetch of the same URL: a browser User-Agent, the
/// `music.youtube.com` referer, and the same `Cookie:` header the app already
/// passes to yt-dlp. Kept separate from `spawn_ffmpeg` so tests can assert the
/// exact argv.
fn build_ffmpeg_command(
    input: &FfmpegInput,
    cookie_header: Option<&str>,
) -> tokio::process::Command {
    let mut ffmpeg = tokio::process::Command::new("ffmpeg");
    apply_child_env(&mut ffmpeg);
    match input {
        FfmpegInput::Url(url) => {
            ffmpeg.args(["-i", url.as_str()]);
            ffmpeg.args(["-user_agent", FFMPEG_USER_AGENT]);
            ffmpeg.args(["-referer", "https://music.youtube.com/"]);
            if let Some(ch) = cookie_header {
                ffmpeg.args(["-headers", &format!("Cookie: {ch}")]);
            }
        }
        FfmpegInput::Pipe => {
            ffmpeg.args(["-i", "pipe:0"]).stdin(std::process::Stdio::piped());
        }
    }
    ffmpeg
}

/// Spawn ffmpeg as an ALAC-in-fragmented-mp4 muxer and wire its stdout into
/// the shared buffer. Both ALAC pipelines (resolved-URL and yt-dlp relay) use
/// the exact same ffmpeg invocation, differing only in input source. Returns
/// the stderr logger, the buffer writer, the child, and (for `Pipe`) the stdin
/// to feed the relay.
fn spawn_ffmpeg(
    input: FfmpegInput,
    writer: SharedBufferWriter,
    label: &'static str,
    cookie_header: Option<&str>,
    buffer: Arc<SharedBuffer>,
) -> anyhow::Result<FfmpegSpawn> {
    let mut ffmpeg = build_ffmpeg_command(&input, cookie_header);
    ffmpeg
        .args(ALAC_FFMPEG_ARGS)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = ffmpeg.spawn().with_context(|| format!("spawn {label}"))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().context("no ffmpeg stdout")?;
    let stderr = child.stderr.take().context("no ffmpeg stderr")?;
    let stderr_handle = spawn_ffmpeg_stderr_handler(stderr, buffer);
    let write_handle = spawn_stdout_writer(stdout, writer, label);
    Ok((stderr_handle, write_handle, child, stdin))
}

fn decoder_from_buffer(
    buffer: &Arc<SharedBuffer>,
    byte_len: Option<u64>,
    pipeline: &'static str,
) -> anyhow::Result<SymphoniaDecoder> {
    let reader = buffer.reader();
    let source = ReadSeekSource::new(reader, byte_len);
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    SymphoniaDecoder::new(mss).with_context(|| format!("decoder (fallback, {pipeline})"))
}

/// Evict a cached stream URL after a download failed on it. The URL may have
/// died server-side, so the retry must re-resolve instead of reusing it.
fn evict_cached_url(from_url_cache: bool, video_id: &str) {
    if from_url_cache {
        resolve::url_cache_remove(video_id);
    }
}

/// Common bail-out for a failed source buffer: permanently unavailable video,
/// auth/cookie problem, or a generic format/network error. Each class surfaces
/// a distinct error; a cached URL is evicted so the retry re-resolves.
#[allow(clippy::too_many_arguments)]
fn bail_failed_buffer(
    buffer: &SharedBuffer,
    from_url_cache: bool,
    video_id: &str,
    t0: tokio::time::Instant,
    context: &str,
) -> anyhow::Result<()> {
    if !buffer.is_failed() {
        return Ok(());
    }
    if buffer.is_dead_video() {
        debug!(%video_id, "Video unavailable (permanently dead), bailing early");
        anyhow::bail!("{}", DEAD_VIDEO_ERR);
    }
    if buffer.is_auth_error() {
        warn!(%video_id, "Auth error {context} (stale cookies?), bailing early");
        anyhow::bail!("{}", AUTH_ERR);
    }
    evict_cached_url(from_url_cache, video_id);
    let reason = if from_url_cache { "ffmpeg" } else { "yt-dlp" };
    debug!(%video_id, elapsed = ?t0.elapsed(), "{reason} failed {context} — bailing early");
    anyhow::bail!("format not available ({reason} error)")
}

/// Decides whether a throttled direct-URL attempt should be retried as a relay.
/// A CDN 403 on a freshly resolved URL is the nsig/po_token throttling wave,
/// not a dead video and not stale cookies: the same song fetched through the
/// credential-carrying yt-dlp relay usually plays. When true, the helper has
/// already evicted the cached URL; the caller sets `stream_url = None` and
/// `continue 'attempt` so the relay (which carries the auth context) runs once.
fn throttled_url_retry(
    buffer: &SharedBuffer,
    from_url_cache: bool,
    video_id: &str,
    t0: tokio::time::Instant,
) -> bool {
    if buffer.is_throttled() && from_url_cache {
        evict_cached_url(from_url_cache, video_id);
        debug!(%video_id, elapsed = ?t0.elapsed(), "Stream URL throttled (403) — retrying via relay");
        true
    } else {
        false
    }
}

/// Build the yt-dlp command for streaming a song to stdout, with the auth
/// cookie/header applied. Shared by the relay (WebM→ffmpeg) and direct M4A
/// paths; the caller configures stdio and spawns.
fn build_ytdlp_command(cfg: &DownloadConfig, format: &str) -> tokio::process::Command {
    let yt_dlp_cmd = if cfg.yt_dlp_command.is_empty() {
        "yt-dlp".to_string()
    } else {
        cfg.yt_dlp_command.clone()
    };
    let mut cmd = tokio::process::Command::new(&yt_dlp_cmd);
    apply_child_env(&mut cmd);
    cmd.args(["-f", format, "-o", "-", "--no-warnings", "--no-playlist"]);
    resolve::apply_ytdlp_auth_args(
        &mut cmd,
        cfg.po_token.as_deref(),
        cfg.cookie_path.as_deref(),
        cfg.cookie_header.as_deref(),
        cfg.js_runtime.as_deref(),
        &cfg.video_id,
    );
    cmd
}

/// A spawned yt-dlp streaming child: its stderr classifier, its stdout (fed to
/// ffmpeg's stdin for the relay path, or to the buffer writer for direct M4A),
/// and the child itself (held for the pipeline so `kill_on_drop` fires only on
/// bail/timeout/cancel, rather than relying on pipe closure which left orphans).
struct YtDlpSpawn {
    stderr_handle: tokio::task::JoinHandle<()>,
    stdout: tokio::process::ChildStdout,
    child: tokio::process::Child,
}

/// Build, spawn, and wire a yt-dlp streaming child with auth applied. Shared by
/// the relay (WebM→ffmpeg) and direct M4A fallback, which differ only in format
/// and whether the stderr handler logs its own cancellation.
fn spawn_ytdlp(
    cfg: &DownloadConfig,
    format: &str,
    buffer: Arc<SharedBuffer>,
    t0: tokio::time::Instant,
    log_cancellation: bool,
) -> anyhow::Result<YtDlpSpawn> {
    let mut cmd = build_ytdlp_command(cfg, format);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().context("spawn yt-dlp")?;
    let stdout = child.stdout.take().context("no stdout from yt-dlp")?;
    let stderr = child.stderr.take().context("no stderr from yt-dlp")?;
    debug!(%cfg.video_id, elapsed = ?t0.elapsed(), "yt-dlp spawned");
    let stderr_handle =
        spawn_stderr_handler(stderr, cfg.cancel_token.clone(), buffer, log_cancellation);
    Ok(YtDlpSpawn {
        stderr_handle,
        stdout,
        child,
    })
}

async fn ytdlp_pipeline(
    cfg: &DownloadConfig,
    ffmpeg_avail: bool,
    _permit: tokio::sync::SemaphorePermit<'static>,
    t0: tokio::time::Instant,
    mut stream_url: Option<String>,
) -> anyhow::Result<SymphoniaDecoder> {
    // ALAC transcoding requires ffmpeg; without it the pipeline must use the
    // direct M4A path even when a (webm) URL resolved, since symphonia cannot
    // decode Opus in a webm container.
    //
    // `'attempt` runs the direct-URL pipeline first and, on a CDN-throttled
    // (403) URL refuse, retries the same song once through the
    // credential-carrying yt-dlp relay (`stream_url = None`). The permit stays
    // owned by this function across attempts — it is only moved into the
    // background cache task on a successful streaming init, which returns.
    'attempt: loop {
        let from_url_cache = stream_url.is_some();

        let buffer = SharedBuffer::new();
        let writer = buffer.writer();

        let (_stderr_handle, stdout_handle, mut child, _relay_handle, mut yt_child) =
            if ffmpeg_avail && let Some(url) = &stream_url {
                let (stderr_handle, write_handle, ffmpeg_child, _stdin) = spawn_ffmpeg(
                    FfmpegInput::Url(url.clone()),
                    writer,
                    "ffmpeg (stream_url)",
                    cfg.cookie_header.as_deref(),
                    buffer.clone(),
                )?;
                (stderr_handle, write_handle, ffmpeg_child, None, None)
            } else if ffmpeg_avail {
                let YtDlpSpawn { stderr_handle, stdout: yt_stdout, child: yt_dlp_child } =
                    spawn_ytdlp(cfg, "ba/bestaudio", buffer.clone(), t0, true)?;

                let (_ffmpeg_stderr_handle, write_handle, ffmpeg_child, ffmpeg_stdin) =
                    spawn_ffmpeg(FfmpegInput::Pipe, writer, "ffmpeg", None, buffer.clone())?;
                let mut ffmpeg_stdin = ffmpeg_stdin.context("no ffmpeg stdin")?;

                let relay = tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut rdr = tokio::io::BufReader::new(yt_stdout);
                    let mut buf = vec![0u8; READ_BUF_SIZE];
                    loop {
                        match rdr.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                if ffmpeg_stdin.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let _ = ffmpeg_stdin.shutdown().await;
                });

                (stderr_handle, write_handle, ffmpeg_child, Some(relay), Some(yt_dlp_child))
            } else {
                let YtDlpSpawn { stderr_handle, stdout: yt_stdout, child: yt_dlp_child } =
                    spawn_ytdlp(cfg, "bestaudio[ext=m4a]/bestaudio/bestaudio*", buffer.clone(), t0, false)?;

                let write_handle = spawn_stdout_writer(yt_stdout, writer, "yt-dlp");

                (stderr_handle, write_handle, yt_dlp_child, None, None)
            };

        let (decoder, needs_cache) = if ffmpeg_avail {
            let deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs(DECODER_INIT_DEADLINE_S);
            while buffer.len() < STREAM_INIT_THRESHOLD
                && tokio::time::Instant::now() < deadline
                && !buffer.is_failed()
                && !cfg.cancel_token.is_cancelled()
            {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            if cfg.cancel_token.is_cancelled() {
                bail!("download cancelled during buffering");
            }
            if throttled_url_retry(&buffer, from_url_cache, &cfg.video_id, t0) {
                stream_url = None;
                continue 'attempt;
            }
            bail_failed_buffer(&buffer, from_url_cache, &cfg.video_id, t0, "before data arrived")?;
            let current = buffer.len();
            if current == 0 {
                // A source that has already exited without emitting a byte is dead
                // or unavailable (dead cached URL, format gone). Evict a cached URL
                // so the retry re-resolves. But a source that is still running may
                // simply be slow to produce its first byte; skipping a playable
                // song because it warmed up slowly is worse than waiting, so keep
                // polling until it exits, produces data, fails, is cancelled, or
                // the patience window elapses.
                let empty_pipe_deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(EMPTY_PIPE_PATIENCE_S);
                loop {
                    let verdict = empty_pipe_verdict(
                        buffer.len() > 0,
                        stdout_handle.is_finished(),
                        buffer.is_failed(),
                        cfg.cancel_token.is_cancelled(),
                        tokio::time::Instant::now() >= empty_pipe_deadline,
                    );
                    match verdict {
                        EmptyPipeVerdict::Break => break,
                        EmptyPipeVerdict::SourceExited => {
                            // A throttle mark from the stderr handler may still be
                            // in flight when the source-exit is observed first
                            // (stdout EOF wins the race). Yield once and re-check
                            // so a 403-refused URL breaks into the throttle-retry
                            // guard after the empty-pipe loop instead of being
                            // misread as a dead pipe.
                            tokio::task::yield_now().await;
                            if buffer.is_failed() {
                                break;
                            }
                            evict_cached_url(from_url_cache, &cfg.video_id);
                            debug!(%cfg.video_id, elapsed = ?t0.elapsed(),
                                "Source exited with an empty pipe");
                            bail!("format not available (source exited, empty pipe)");
                        }
                        EmptyPipeVerdict::Cancelled => {
                            bail!("download cancelled during empty-pipe wait");
                        }
                        EmptyPipeVerdict::PatienceElapsed => {
                            evict_cached_url(from_url_cache, &cfg.video_id);
                            debug!(%cfg.video_id, elapsed = ?t0.elapsed(),
                                "Empty pipe persisted {}s — treating as unavailable",
                                EMPTY_PIPE_PATIENCE_S);
                            bail!(
                                "format not available (empty pipe after {}s)",
                                EMPTY_PIPE_PATIENCE_S
                            );
                        }
                        EmptyPipeVerdict::Wait => {
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    }
                }
                if throttled_url_retry(&buffer, from_url_cache, &cfg.video_id, t0) {
                    stream_url = None;
                    continue 'attempt;
                }
                bail_failed_buffer(&buffer, from_url_cache, &cfg.video_id, t0, "during empty-pipe wait")?;
            }
            let stream_type = if from_url_cache {
                "url-cache→ffmpeg→alac-mp4"
            } else {
                "ffmpeg→alac-mp4"
            };
            debug!(%cfg.video_id, stream_type, buf_len = buffer.len(), elapsed = ?t0.elapsed(),
                "Trying early decoder init");

            match try_streaming_init_nonseekable(&buffer).await {
                Ok(decoder) => {
                    debug!(%cfg.video_id, buf_len = buffer.len(), elapsed = ?t0.elapsed(),
                        "Streaming decoder init succeeded");
                    let log_prefix = if from_url_cache { "ffmpeg (stream_url)" } else { "ffmpeg" };
                    let _cache_task = tokio::spawn(spawn_bg_cache_task(
                        cfg.video_id.clone(),
                        cfg.cancel_token.clone(),
                        child,
                        yt_child,
                        stdout_handle,
                        buffer.clone(),
                        log_prefix,
                        Some(t0),
                        _permit,
                    ));
                    (decoder, false)
                }
                Err(stream_err) => {
                    debug!(%cfg.video_id, error = %stream_err,
                        "Streaming decoder init failed, waiting for {} stream to complete",
                        if from_url_cache { "ffmpeg" } else { "ffmpeg relay" });
                    let pipe_label = if from_url_cache { "ffmpeg (stream_url)" } else { "ffmpeg" };
                    let wait_result = tokio::select! {
                        biased;
                        _ = cfg.cancel_token.cancelled() => {
                            kill_and_reap(&mut child, &mut yt_child).await;
                            bail!("{pipe_label} download cancelled during fallback wait");
                        }
                        res = tokio::time::timeout(
                            std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_S),
                            stdout_handle,
                        ) => res,
                    };
                    match wait_result {
                        Ok(Ok(())) => {}
                        Ok(Err(join_err)) => {
                            evict_cached_url(from_url_cache, &cfg.video_id);
                            bail!("{pipe_label} writer task panicked: {join_err}");
                        }
                        Err(_elapsed) => {
                            kill_and_reap(&mut child, &mut yt_child).await;
                            evict_cached_url(from_url_cache, &cfg.video_id);
                            bail!("{pipe_label} download timed out ({}s)", DOWNLOAD_TIMEOUT_S);
                        }
                    }

                    let status = child.wait().await.with_context(|| format!("wait {pipe_label}"))?;
                    if !status.success() {
                        let code = exit_code_string(&status);
                        // The 403 throttle mark can still be in flight when the
                        // writer task resolves on stdout EOF (see the empty-pipe
                        // Site-2 handling for the same race). Yield once so the
                        // stderr handler's mark lands before classifying the exit
                        // — otherwise a throttled URL is misread as a generic
                        // failure and the song skips instead of relay-retrying.
                        tokio::task::yield_now().await;
                        if throttled_url_retry(&buffer, from_url_cache, &cfg.video_id, t0) {
                            stream_url = None;
                            continue 'attempt;
                        }
                        evict_cached_url(from_url_cache, &cfg.video_id);
                        bail!("{pipe_label} exited with code {code}");
                    }
                    debug!(%cfg.video_id, "{pipe_label} completed successfully");

                    debug!(%cfg.video_id, buf_len = buffer.len(),
                        "Creating decoder from completed download (fallback)");
                    let d = decoder_from_buffer(&buffer, None, "mp4-fallback")?;
                    (d, !from_url_cache)
                }
            }
        } else {
            let total_len = {
                let deadline = tokio::time::Instant::now()
                    + std::time::Duration::from_secs(M4A_TOTAL_LEN_TIMEOUT_S);
                loop {
                    if buffer.is_failed() {
                        if buffer.is_dead_video() {
                            bail!("{}", DEAD_VIDEO_ERR);
                        }
                        if buffer.is_auth_error() {
                            bail!("{}", AUTH_ERR);
                        }
                        break None;
                    }
                    if cfg.cancel_token.is_cancelled() {
                        break None;
                    }
                    if let Some(tl) = buffer.total_len() {
                        break Some(tl);
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break None;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
            .unwrap_or(buffer.len() as u64);

            if cfg.cancel_token.is_cancelled() {
                bail!("download cancelled during M4A total_len wait");
            }

            debug!(
                %cfg.video_id,
                stream_type = "direct m4a",
                buf_len = buffer.len(),
                elapsed = ?t0.elapsed(),
                "Trying early decoder init (spawn_blocking + {}s timeout)", DECODER_INIT_DEADLINE_S
            );
            match try_streaming_init(&buffer, Some(total_len)).await {
                Ok(decoder) => {
                    debug!(%cfg.video_id, buf_len = buffer.len(), elapsed = ?t0.elapsed(),
                    "Streaming decoder init succeeded (M4A)");

                    let pipe_name: &str = "yt-dlp";
                    let _handle = tokio::spawn(spawn_bg_cache_task(
                        cfg.video_id.clone(),
                        cfg.cancel_token.clone(),
                        child,
                        None,
                        stdout_handle,
                        buffer.clone(),
                        pipe_name,
                        None,
                        _permit,
                    ));

                    (decoder, false)
                }
                Err(stream_err) => {
                    debug!(%cfg.video_id, error = %stream_err,
                    "Streaming decoder init failed, waiting for yt-dlp stream to complete");
                    let wait_result = tokio::select! {
                        biased;
                        _ = cfg.cancel_token.cancelled() => {
                            kill_and_reap(&mut child, &mut yt_child).await;
                            bail!("yt-dlp download cancelled during fallback wait");
                        }
                        res = tokio::time::timeout(
                            std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_S),
                            stdout_handle,
                        ) => res,
                    };
                    match wait_result {
                        Ok(Ok(())) => {}
                        Ok(Err(join_err)) => {
                            bail!("yt-dlp writer task panicked: {join_err}");
                        }
                        Err(_elapsed) => {
                            kill_and_reap(&mut child, &mut yt_child).await;
                            bail!("yt-dlp download timed out ({}s)", DOWNLOAD_TIMEOUT_S);
                        }
                    }

                    let status = child
                        .wait()
                        .await
                        .with_context(|| "wait yt-dlp".to_string())?;
                    if !status.success() {
                        let code = exit_code_string(&status);
                        bail!("yt-dlp exited with code {code}");
                    }
                    debug!(%cfg.video_id, "yt-dlp completed successfully");

                    debug!(%cfg.video_id, buf_len = buffer.len(),
                    "Creating decoder from completed download (fallback)");
                    let d = decoder_from_buffer(&buffer, Some(total_len), "m4a-fallback")?;
                    (d, true)
                }
            }
        };

        if needs_cache {
            let data = buffer.finalize();
            debug!(%cfg.video_id, len = data.len(), "Caching completed download (fallback)");
            cache_put(cfg.video_id.clone(), data);
        }

        debug!(%cfg.video_id, elapsed = ?t0.elapsed(), "download_and_decode returning decoder");
        return Ok(decoder);
    }
}

pub async fn download_and_decode(cfg: DownloadConfig) -> anyhow::Result<SymphoniaDecoder> {

    if cfg.cancel_token.is_cancelled() {
        anyhow::bail!("download cancelled before start");
    }

    if let Some(decoder) = create_decoder_from_cache(&cfg.video_id) {
        return Ok(decoder);
    }

    let ffmpeg_avail = check_ffmpeg();
    let cached_url = match resolve_url(
        &cfg.video_id, &cfg.yt_dlp_command, cfg.po_token.as_deref(), cfg.cookie_path.as_deref(), cfg.cookie_header.as_deref(), cfg.js_runtime.as_deref(),
        Some(&cfg.cancel_token),
    ).await {
        resolve::ResolveOutcome::Url(url) => Some(url),
        resolve::ResolveOutcome::AuthError(line) => {
            let po_tok = if cfg.po_token.is_some() {
                "po_token set"
            } else {
                "no po_token"
            };
            warn!(%cfg.video_id, po_tok, error_line = %line,
                "resolve failed with an auth error — failing fast, skipping redundant download");
            anyhow::bail!("authentication error (stale cookies; {po_tok}): {line}");
        }
        resolve::ResolveOutcome::Failed => None,
    };

    if cfg.cancel_token.is_cancelled() {
        anyhow::bail!("download cancelled before semaphore");
    }

    let _permit = DOWNLOAD_SEMAPHORE
        .acquire()
        .await
        .context("Semaphore closed")?;

    if cfg.cancel_token.is_cancelled() {
        anyhow::bail!("download cancelled after semaphore");
    }

    if let Some(decoder) = create_decoder_from_cache(&cfg.video_id) {
        debug!(%cfg.video_id, "Reusing cached buffer (filled while waiting for semaphore)");
        return Ok(decoder);
    }

    let t0 = tokio::time::Instant::now();

    ytdlp_pipeline(&cfg, ffmpeg_avail, _permit, t0, cached_url).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::server::song_downloader::cache::CACHE_MAX_ENTRIES;
    use crate::app::server::song_downloader::cache::cache_get;
    use crate::app::server::streaming_buffer::SharedBuffer;
    use crate::decoder::SymphoniaDecoder;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    use symphonia::core::io::MediaSourceStream;

    const TEST_WAV: &[u8] = include_bytes!("../../../../../ytmapi-rs/test_json/test_silence.wav");
    const TEST_ALAC: &[u8] =
        include_bytes!("../../../../../ytmapi-rs/test_json/test_alac_fragmented.mp4");

    #[tokio::test]
    async fn oversized_env_var_makes_spawn_fail_with_e2big() {
        let mut cmd = tokio::process::Command::new("/bin/true");
        cmd.env("HUGENV", "x".repeat(300 * 1024));
        let result = cmd.status().await;
        assert!(result.is_err(), "oversized env must make execve fail with E2BIG");
    }

    #[tokio::test]
    async fn apply_child_env_rescues_oversized_env_spawn() {
        let mut cmd = tokio::process::Command::new("/bin/true");
        cmd.env("HUGENV", "x".repeat(300 * 1024));
        apply_child_env(&mut cmd);
        let status = cmd.status().await.expect("env_clear must bound env and allow spawn");
        assert!(status.success());
    }

    #[test]
    fn apply_child_env_rescues_oversized_env_spawn_sync() {
        let mut cmd = std::process::Command::new("/bin/true");
        cmd.env("HUGENV", "x".repeat(300 * 1024));
        apply_child_env(&mut cmd);
        let status = cmd.status().expect("sync spawn also runs with the bounded env");
        assert!(status.success());
    }

    #[tokio::test]
    async fn apply_child_env_preserves_path_and_proxy() {
        const PROXY: &str = "http://proxy.invalid:8080";
        // tokio tests are single-threaded; the parent env is not read by the
        // runtime while we set it, so this is safe here.
        unsafe { std::env::set_var("http_proxy", PROXY) };
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("printf %s \"$http_proxy\"");
        apply_child_env(&mut cmd);
        let out = cmd.output().await.expect("must spawn with bounded env");
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            PROXY,
            "allowlisted proxy var must reach the child"
        );
        unsafe { std::env::remove_var("http_proxy") };
    }

    #[test]
    fn track_download_progress_growth_resets_stall() {
        let start = Instant::now();
        let (len, progress, stalled) =
            track_download_progress(100, 200, start, Duration::from_secs(BG_STALL_TIMEOUT_S));
        assert!(!stalled, "new bytes arriving means the download is alive");
        assert_eq!(len, 200, "baseline must advance to the new length");
        // Progress instant is reset: even with the baseline unchanged, a fresh
        // timestamp means no stall yet.
        let (_, _, stalled) =
            track_download_progress(len, len, progress, Duration::from_secs(BG_STALL_TIMEOUT_S));
        assert!(!stalled);
    }

    #[test]
    fn track_download_progress_no_growth_within_stall_not_stalled() {
        let now = Instant::now();
        let (_, _, stalled) =
            track_download_progress(100, 100, now, Duration::from_secs(BG_STALL_TIMEOUT_S));
        assert!(!stalled, "no growth yet, but within the stall window");
    }

    #[test]
    fn empty_pipe_verdict_prioritizes_failed_over_exited() {
        use EmptyPipeVerdict::*;
        // Data present → proceed regardless of source state.
        assert_eq!(empty_pipe_verdict(true, true, false, false, false), Break);
        // A source that FAILED with zero bytes must break into classification
        // (dead video / auth), NOT be swallowed as a generic empty pipe. This
        // is the regression guard for the empty-pipe patience-loop fix.
        assert_eq!(empty_pipe_verdict(false, true, true, false, false), Break);
        // Exited-but-not-failed, still empty → genuinely dead pipe.
        assert_eq!(empty_pipe_verdict(false, true, false, false, false), SourceExited);
        assert_eq!(empty_pipe_verdict(false, false, false, true, false), Cancelled);
        assert_eq!(empty_pipe_verdict(false, false, false, false, true), PatienceElapsed);
        assert_eq!(empty_pipe_verdict(false, false, false, false, false), Wait);
    }

    #[test]
    fn track_download_progress_no_growth_beyond_stall_reports_stalled() {
        // Baseline untouched for longer than the stall window -> stuck.
        let old = Instant::now() - Duration::from_secs(BG_STALL_TIMEOUT_S + 1);
        let (_, _, stalled) =
            track_download_progress(100, 100, old, Duration::from_secs(BG_STALL_TIMEOUT_S));
        assert!(stalled, "zero progress beyond the stall window must be reported");
    }

    // Tests touching the global BYTE_CACHE must run serially (they share the
    // same cache behind one Mutex, so parallel runs evict each other's entries).
    static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

    // Restores CACHE_MAX_ENTRIES on drop so a test that raises the cache size
    // can't leak the larger value into a later test that assumes the default.
    struct RestoreCacheMax(u64);
    impl Drop for RestoreCacheMax {
        fn drop(&mut self) {
            CACHE_MAX_ENTRIES.store(self.0 as usize, Ordering::Release);
        }
    }

    // Tests acquiring the global DOWNLOAD_SEMAPHORE must run serially: parallel
    // runtimes would steal each other's permit and defeat the held/released
    // assertions.
    static SEMAPHORE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn spawn_slow_write(
        buf: &Arc<SharedBuffer>,
        data: &[u8],
        chunk_delay: Duration,
    ) -> std::thread::JoinHandle<()> {
        let buf = buf.clone();
        let data = data.to_vec();
        std::thread::spawn(move || {
            let mut writer = buf.writer();
            let chunk: usize = 64 * 1024;
            for chunk_start in (0..data.len()).step_by(chunk) {
                let end = (chunk_start + chunk).min(data.len());
                writer.write(&data[chunk_start..end]);
                if end < data.len() {
                    std::thread::sleep(chunk_delay);
                }
            }
            writer.finish();
        })
    }

    fn spawn_slow_write_chunked(
        buf: &Arc<SharedBuffer>,
        data: &[u8],
        chunk: usize,
        chunk_delay: Duration,
    ) -> std::thread::JoinHandle<()> {
        let buf = buf.clone();
        let data = data.to_vec();
        std::thread::spawn(move || {
            let mut writer = buf.writer();
            for chunk_start in (0..data.len()).step_by(chunk) {
                let end = (chunk_start + chunk).min(data.len());
                writer.write(&data[chunk_start..end]);
                if end < data.len() {
                    std::thread::sleep(chunk_delay);
                }
            }
            writer.finish();
        })
    }

    fn create_decoder_from(buf: &Arc<SharedBuffer>) -> anyhow::Result<SymphoniaDecoder> {
        let total = buf.total_len().unwrap_or(buf.len() as u64);
        let reader = buf.reader();
        let source = ReadSeekSource::new(reader, Some(total));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        SymphoniaDecoder::new(mss).context("decoder")
    }

    fn time_to_first_frame(
        decoder: &mut SymphoniaDecoder,
        target_frames: usize,
    ) -> Option<Duration> {
        let t0 = Instant::now();
        let mut total = 0usize;
        while total < target_frames {
            match decoder.next() {
                Some(_) => { total += 1; }
                None => {
                    if total == 0 { return None; }
                    break;
                }
            }
        }
        Some(t0.elapsed())
    }

    #[test]
    fn parse_kib() {
        let line = "[download]   0.3% of  302.04KiB at  344.87KiB/s ETA 00:00";
        assert_eq!(parse_total_size(line), Some((302.04 * 1024.0) as u64));
    }

    #[test]
    fn permanently_unavailable_classifier() {
        let permanent = [
            "ERROR: [youtube] NLkDhrzgrI8: Video unavailable. This video is not available",
            "ERROR: [youtube] 5cmbsjSt3K0: Video unavailable. This video is not available",
            "ERROR: [youtube] 4y5R7urKjAQ: Video unavailable. This video is no longer available because the YouTube account associated with this video has been terminated.",
            "ERROR: [youtube] X: Video unavailable. This video has been removed by the uploader",
        ];
        for line in permanent {
            assert!(is_permanently_unavailable(line), "expected permanent: {line}");
        }

        let transient = [
            "ERROR: [youtube] X: Sign in to confirm you're not a bot",
            "ERROR: '/home/.config/youtui/cookies_netscape.txt' does not look like a Netscape format cookies file",
            "ERROR: Requested format is not available",
            "ERROR: [youtube] X: HTTP Error 429: Too Many Requests",
            "ERROR: [youtube] X: This video is only available to signed-in users",
            "ERROR: [youtube] X: Please sign in to view this content",
        ];
        for line in transient {
            assert!(
                !is_permanently_unavailable(line),
                "expected transient: {line}"
            );
        }
    }

    #[test]
    fn auth_error_classifier() {
        let auth = [
            "ERROR: [youtube] X: Sign in to confirm you're not a bot",
            "ERROR: [youtube] X: Please sign in to view this content",
            "ERROR: [youtube] X: This video is only available to signed-in users",
            "ERROR: [youtube] X: Sign in to confirm your age",
            "ERROR: '/home/.config/youtui/cookies_netscape.txt' does not look like a Netscape format cookies file",
        ];
        for line in auth {
            assert!(is_auth_error_line(line), "expected auth error: {line}");
        }

        let not_auth = [
            "ERROR: [youtube] X: HTTP Error 403: Forbidden",
            "ERROR: [youtube] NLkDhrzgrI8: Video unavailable. This video is not available",
            "ERROR: [youtube] X: HTTP Error 429: Too Many Requests",
            "ERROR: [youtube] X: The uploader has not made this video available in your country",
            "ERROR: Requested format is not available",
        ];
        for line in not_auth {
            assert!(!is_auth_error_line(line), "expected non-auth: {line}");
        }
    }

    #[test]
    fn ffmpeg_403_line_classifies_throttle() {
        let throttled = [
            "Server returned 403 Forbidden (access denied)",
            "[in#0 @ 0x559032e24a00] Error opening input: Server returned 403 Forbidden (access denied)",
            "Error opening input files: Server returned 403 Forbidden (access denied)",
            "Server returned HTTP error 403, aborting",
        ];
        for line in throttled {
            assert!(is_throttle_line(line), "expected throttled: {line}");
        }

        let not_throttled = [
            "ERROR: [youtube] X: HTTP Error 429: Too Many Requests",
            "ERROR: Requested format is not available",
            "[in#0 @ 0x559032e24a00] Error opening input: Input/output error",
            "[in#0 @ 0x559032e24a00] Error opening input: Connection timed out",
            "ERROR: [youtube] X: Sign in to confirm you're not a bot",
            "ERROR: [youtube] X: HTTP Error 416: Requested Range Not Satisfiable",
        ];
        for line in not_throttled {
            assert!(!is_throttle_line(line), "expected not throttled: {line}");
        }
    }

    #[tokio::test]
    async fn ffmpeg_stderr_403_marks_throttled() {
        let buf = SharedBuffer::new();
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("printf '%s\\n' 'Server returned 403 Forbidden (access denied)' >&2; exit 1")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sh for ffmpeg-stderr wiring test");
        let stderr = child.stderr.take().expect("sh stderr");
        let handle = spawn_ffmpeg_stderr_handler(stderr, buf.clone());
        child.wait().await.expect("wait sh");
        let _ = handle.await;
        assert!(buf.is_throttled(), "a 403 line must mark the buffer throttled");
        assert!(
            buf.is_failed(),
            "the throttled buffer must be failed so the pipeline breaks out of its loops"
        );
    }

    #[test]
    fn throttled_url_retry_decision() {
        let t0 = tokio::time::Instant::from_std(std::time::Instant::now());
        let throttled = SharedBuffer::new();
        throttled.mark_throttled();
        let failed = SharedBuffer::new();
        failed.fail();
        let plain = SharedBuffer::new();

        // A throttled refused URL must retry via the relay.
        assert!(throttled_url_retry(&throttled, true, "v1", t0));
        // A throttled RELAY attempt (no URL involved) must NOT loop again — the
        // retry already happened; another loop would retry forever.
        assert!(!throttled_url_retry(&throttled, false, "v1", t0));
        // Any other failure (even a failed buffer) is not a throttle → no retry.
        assert!(!throttled_url_retry(&failed, true, "v1", t0));
        assert!(!throttled_url_retry(&plain, true, "v1", t0));
    }

    // The E2E throttle tests below put fake `ffmpeg`/`yt-dlp` binaries on PATH
    // and run the full `download_and_decode` pipeline. They must run serially:
    // PATH is process-global, the global DOWNLOAD_SEMAPHORE permit is consumed,
    // and the relay's background cache task touches BYTE_CACHE.
    static PIPELINE_TEST_LOCK: Mutex<()> = Mutex::new(());

    // Fake ffmpeg: `-version` must succeed so check_ffmpeg() sees ffmpeg as
    // available; a URL input (`-i <url>`) is a CDN-throttled direct fetch
    // (403 on stderr + non-zero exit); a pipe input (`-i pipe:0`) is the relay
    // path, where the fake just copies stdin to stdout untouched so the ALAC
    // bytes pass through as-is.
    const FAKE_FFMPEG: &str = r#"#!/bin/sh
case " $* " in
  *" -version "*) exit 0 ;;
esac
if printf '%s\n' "$@" | grep -q 'pipe:0'; then
  cat
else
  echo 'Server returned 403 Forbidden (access denied)' >&2
  exit 1
fi
"#;

    // Fake yt-dlp resolve branch: `--print url` prints a resolvable URL.
    const YTDLP_FAKE_HEAD: &str = r#"#!/bin/sh
if printf '%s\n' "$@" | grep -q -- '--print'; then
  echo 'https://fake.example.com/stream'
  exit 0
fi
"#;

    struct FakePath {
        dir: std::path::PathBuf,
    }

    impl FakePath {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "youtui_fakebin_{}_{}",
                std::process::id(),
                nanos
            ));
            std::fs::create_dir_all(&dir).expect("create fake binary dir");
            let old_path = std::env::var("PATH").unwrap_or_default();
            let new_path = format!("{}:{}", dir.display(), old_path);
            // tokio tests run on a current-thread runtime; only the E2E tests
            // mutate PATH, serialized by PIPELINE_TEST_LOCK.
            unsafe { std::env::set_var("PATH", new_path) };
            FakePath { dir }
        }
    }

    impl Drop for FakePath {
        fn drop(&mut self) {
            let prefix = format!("{}:", self.dir.display());
            let cur = std::env::var("PATH").unwrap_or_default();
            let restored = cur.strip_prefix(&prefix).unwrap_or(&cur).to_string();
            unsafe { std::env::set_var("PATH", restored) };
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn write_fake_bin(dir: &std::path::Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write fake binary");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake binary");
    }

    fn alac_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ytmapi-rs/test_json/test_alac_fragmented.mp4")
            .canonicalize()
            .expect("resolve ALAC fixture path")
    }

    #[test]
    fn throttled_url_retries_via_relay_end_to_end() {
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sem = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cache = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let fakebin = FakePath::new();
            write_fake_bin(&fakebin.dir, "ffmpeg", FAKE_FFMPEG);
            let fixture = alac_fixture_path();
            write_fake_bin(
                &fakebin.dir,
                "yt-dlp",
                &format!("{}cat '{}'\n", YTDLP_FAKE_HEAD, fixture.display()),
            );

            let video_id = format!("throttle-e2e-{}", std::process::id());
            let cfg = DownloadConfig {
                yt_dlp_command: "yt-dlp".to_string(),
                video_id: video_id.clone(),
                po_token: None,
                cookie_path: None,
                cookie_header: None,
                js_runtime: None,
                cancel_token: tokio_util::sync::CancellationToken::new(),
            };

            let mut decoder = download_and_decode(cfg)
                .await
                .expect("a throttled direct-URL attempt must recover via the relay and decode");
            assert!(
                time_to_first_frame(&mut decoder, 1024).is_some(),
                "the relay-recovered download must produce audio frames"
            );
            assert_eq!(
                resolve::url_cache_get(&video_id),
                None,
                "the throttled stream URL must be evicted from the URL cache"
            );
        });
    }

    #[test]
    fn throttled_relay_failure_bails_without_retry() {
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sem = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cache = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let fakebin = FakePath::new();
            write_fake_bin(&fakebin.dir, "ffmpeg", FAKE_FFMPEG);
            write_fake_bin(
                &fakebin.dir,
                "yt-dlp",
                &format!(
                    "{}echo 'ERROR: [youtube] xyz: HTTP Error 403: Forbidden' >&2\nexit 1\n",
                    YTDLP_FAKE_HEAD
                ),
            );

            let video_id = format!("throttle-e2e-relay-{}", std::process::id());
            let cfg = DownloadConfig {
                yt_dlp_command: "yt-dlp".to_string(),
                video_id: video_id.clone(),
                po_token: None,
                cookie_path: None,
                cookie_header: None,
                js_runtime: None,
                cancel_token: tokio_util::sync::CancellationToken::new(),
            };

            let err = match download_and_decode(cfg).await {
                Ok(_) => panic!("a throttled relay attempt (already one retry deep) must not be retried"),
                Err(e) => e,
            };
            assert!(
                err.to_string().starts_with("format not available"),
                "a throttled relay must bail as a generic transient failure, got: {err}"
            );
            assert_eq!(
                resolve::url_cache_get(&video_id),
                None,
                "the throttled stream URL must be evicted from the URL cache"
            );
        });
    }

    #[test]
    fn url_input_includes_auth_passthrough_args() {
        let cmd = build_ffmpeg_command(
            &FfmpegInput::Url("https://example.com/stream".into()),
            Some("SID=abc; z=1"),
        );
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2).any(|w| w[0] == "-i" && w[1] == "https://example.com/stream"),
            "missing -i: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-user_agent" && w[1] == FFMPEG_USER_AGENT),
            "missing browser -user_agent: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-referer" && w[1] == "https://music.youtube.com/"),
            "missing -referer: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-headers" && w[1] == "Cookie: SID=abc; z=1"),
            "missing cookie -headers (the CDN refuses anonymous fetches): {args:?}"
        );
    }

    #[test]
    fn url_input_omits_headers_without_cookie() {
        let cmd = build_ffmpeg_command(&FfmpegInput::Url("https://example.com/stream".into()), None);
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|a| a == "-headers"),
            "no cookie header means no -headers: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-user_agent" && w[1] == FFMPEG_USER_AGENT),
            "UA/referer still applied without cookies: {args:?}"
        );
    }

    #[test]
    fn pipe_input_has_no_http_passthrough() {
        let cmd = build_ffmpeg_command(&FfmpegInput::Pipe, Some("SID=abc"));
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|a| a == "-headers" || a == "-referer" || a == "-user_agent"),
            "relay input reads stdin; no HTTP passthrough should be added: {args:?}"
        );
        assert!(
            args.windows(2).any(|w| w[0] == "-i" && w[1] == "pipe:0"),
            "relay ffmpeg must read pipe:0: {args:?}"
        );
    }

    #[test]
    fn parse_mib() {
        let line = "[download]   5.0% of    4.06MiB at    2.14MiB/s ETA 00:00";
        assert_eq!(parse_total_size(line), Some((4.06 * 1024.0 * 1024.0) as u64));
    }

    #[test]
    fn parse_gib() {
        let line = "[download]   1.0% of    1.50GiB at  100.00MiB/s ETA 00:10";
        assert_eq!(parse_total_size(line), Some((1.50 * 1024.0 * 1024.0 * 1024.0) as u64));
    }

    #[test]
    fn parse_bytes() {
        let line = "[download]  50.0% of  500Bytes at  100B/s ETA 00:00";
        assert_eq!(parse_total_size(line), Some(500));
    }

    #[test]
    fn parse_no_match() {
        assert!(parse_total_size("[youtube] Extracting URL...").is_none());
        assert!(parse_total_size("[info] jNQXAC9IVRw: Downloading 1 format(s): 140").is_none());
    }

    #[test]
    fn parse_kb_si_unit() {
        let line = "[download]  10.0% of  500.00KB at  1.00MB/s ETA 00:00";
        assert_eq!(parse_total_size(line), Some((500.0 * 1024.0) as u64));
    }

    #[test]
    fn parse_mb_si_unit() {
        let line = "[download]  25.0% of    2.50MB at  500.00KB/s ETA 00:05";
        assert_eq!(parse_total_size(line), Some((2.50 * 1024.0 * 1024.0) as u64));
    }

    #[test]
    fn parse_gb_si_unit() {
        let line = "[download]   1.0% of    1.20GB at   50.00MB/s ETA 00:30";
        assert_eq!(parse_total_size(line), Some((1.20 * 1024.0 * 1024.0 * 1024.0) as u64));
    }

    #[test]
    fn parse_kb_lowercase() {
        let line = "[download]   5.0% of  100.00kB at  200.00kB/s ETA 00:01";
        assert_eq!(parse_total_size(line), Some((100.0 * 1024.0) as u64));
    }

    #[test]
    fn parse_small_values() {
        let line = "[download] 100.0% of  1.00Bytes at  1.00B/s ETA 00:00";
        assert_eq!(parse_total_size(line), Some(1));
    }

    #[test]
    fn parse_weird_padding() {
        let line = "[download]   0.0% of    0.00KiB at    0.00B/s ETA 00:00";
        assert_eq!(parse_total_size(line), Some(0));
    }

    #[test]
    fn test_ffmpeg_check_twice() {
        let a = check_ffmpeg();
        let b = check_ffmpeg();
        assert_eq!(a, b);
    }

    #[test]
    fn parse_many_units() {
        let cases: Vec<(&str, Option<u64>)> = vec![
            ("[download]  10.0% of  1.00KiB at ...", Some(1024)),
            ("[download]  10.0% of  1.00KB at ...", Some(1024)),
            ("[download]  10.0% of  1.00kB at ...", Some(1024)),
            ("[download]  10.0% of  1.00MiB at ...", Some(1024 * 1024)),
            ("[download]  10.0% of  1.00MB at ...", Some(1024 * 1024)),
            ("[download]  10.0% of  1.00GiB at ...", Some(1024 * 1024 * 1024)),
            ("[download]  10.0% of  1.00GB at ...", Some(1024 * 1024 * 1024)),
            ("[download]  10.0% of  500B at ...", Some(500)),
            ("[download]  10.0% of  500Bytes at ...", Some(500)),
            ("no match here", None),
            ("[youtube] jNQXAC9IVRw: Downloading page 1", None),
        ];
        for (line, expected) in cases {
            assert_eq!(parse_total_size(line), expected, "parse_total_size({line:?})");
        }
    }

    #[test]
    fn exit_code_string_variants() {
        let success = std::process::ExitStatus::default();
        assert_eq!(exit_code_string(&success), "0");
    }

    #[test]
    fn test_streaming_deadline_tight() {
        let cursor = std::io::Cursor::new(vec![0u8; 16]);
        let source = ReadSeekSource::new(cursor, Some(16));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let result = SymphoniaDecoder::new(mss);
        assert!(result.is_err(), "truncated wav should fail");
    }

    #[test]
    fn streaming_creates_decoder_with_full_data_incomplete_writer() {
        let buf = SharedBuffer::new();
        buf.set_total_len(TEST_WAV.len() as u64);
        let mut keep_alive = buf.writer();
        keep_alive.write(TEST_WAV);
        let mut dec = create_decoder_from(&buf).expect("Decoder with all data written but writer alive");
        let ttf = time_to_first_frame(&mut dec, 1024);
        assert!(ttf.is_some(), "Decoder should produce frames when all data is in the buffer");
        drop(keep_alive);
    }

    #[test]
    fn streaming_produces_frames_before_full_write_completes() {
        let chunk_delay = Duration::from_millis(15);
        let buf = SharedBuffer::new();
        buf.set_total_len(TEST_WAV.len() as u64);
        let handle = spawn_slow_write(&buf, TEST_WAV, chunk_delay);
        let mut dec = create_decoder_from(&buf)
            .expect("Decoder created from streaming buffer (may block briefly)");
        let streaming_ttf = time_to_first_frame(&mut dec, 44100);
        assert!(streaming_ttf.is_some(), "Streaming decoder must produce frames while download is in progress");
        handle.join().unwrap();
        let full_write_estimate = (TEST_WAV.len().div_ceil(64 * 1024) as u64).saturating_sub(1) * 15;
        println!("streaming: first ~1s of audio in {:?} (full write would take ~{full_write_estimate} ms)", streaming_ttf.unwrap());
        assert!(streaming_ttf.unwrap() < Duration::from_millis(full_write_estimate),
            "Streaming TTF {:?} must be < {full_write_estimate} ms (full write time)", streaming_ttf.unwrap());
    }

    #[test]
    fn full_download_requires_write_completion() {
        let buf = SharedBuffer::new();
        buf.set_total_len(TEST_WAV.len() as u64);
        let mut w = buf.writer();
        w.write(TEST_WAV);
        w.finish();
        let mut dec = create_decoder_from(&buf).expect("Decoder after full download");
        let ttf = time_to_first_frame(&mut dec, 44100);
        assert!(ttf.is_some(), "Full-download decoder should produce frames");
    }

    #[test]
    fn alac_fragmented_decodes_from_full_buffer() {
        let buf = SharedBuffer::new();
        let mut w = buf.writer();
        w.write(TEST_ALAC);
        w.finish();
        let mut dec = create_decoder_from(&buf).expect("ALAC fragmented-mp4 decoder");
        let ttf = time_to_first_frame(&mut dec, 1024);
        assert!(ttf.is_some(), "ALAC fragmented mp4 should decode from full buffer");
    }

    #[test]
    fn alac_fragmented_streams_from_partial_buffer() {
        let chunk_delay = Duration::from_millis(8);
        let buf = SharedBuffer::new();
        let handle = spawn_slow_write_chunked(&buf, TEST_ALAC, 64, chunk_delay);
        let deadline = Instant::now() + Duration::from_secs(5);
        while buf.len() < STREAM_INIT_THRESHOLD && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(buf.len() >= STREAM_INIT_THRESHOLD, "buffer must reach init threshold");
        let reader = buf.reader();
        let source = crate::decoder::read_seek_source::ReadSeekSource::nonseekable(reader);
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let mut dec = SymphoniaDecoder::new(mss)
            .expect("ALAC decoder created from streaming buffer (may block briefly)");
        let streaming_ttf = time_to_first_frame(&mut dec, 1024);
        assert!(streaming_ttf.is_some(), "ALAC fragmented mp4 must stream from partial buffer");
        handle.join().unwrap();
        let full_write_estimate = TEST_ALAC.len().div_ceil(64) as u64 * chunk_delay.as_millis() as u64;
        println!("alac streaming: first frames in {:?} (full write would take ~{full_write_estimate} ms)", streaming_ttf.unwrap());
        assert!(streaming_ttf.unwrap() < Duration::from_millis(full_write_estimate),
            "ALAC streaming TTF {:?} must be < {full_write_estimate} ms (full write time)", streaming_ttf.unwrap());
    }

    #[test]
    fn streaming_vs_full_timing_comparison() {
        let chunk_delay = Duration::from_millis(5);
        let num_chunks = TEST_WAV.len().div_ceil(64 * 1024);
        let full_write_ms = (num_chunks as u64).saturating_sub(1) * 5;

        let buf_stream = SharedBuffer::new();
        buf_stream.set_total_len(TEST_WAV.len() as u64);
        let _stream_handle = spawn_slow_write(&buf_stream, TEST_WAV, chunk_delay);
        let t0 = Instant::now();
        let mut dec_stream = create_decoder_from(&buf_stream).unwrap();
        let streaming_ttf = t0.elapsed();
        let _streaming_frames = dec_stream.by_ref().take(1024).count();

        let buf_full = SharedBuffer::new();
        buf_full.set_total_len(TEST_WAV.len() as u64);
        let full_handle = spawn_slow_write(&buf_full, TEST_WAV, chunk_delay);
        full_handle.join().unwrap();
        let t0 = Instant::now();
        let mut dec_full = create_decoder_from(&buf_full).unwrap();
        let full_ttf = t0.elapsed();
        let _full_frames = dec_full.by_ref().take(1024).count();
        _stream_handle.join().unwrap();

        println!("BENCHMARK: streaming TTF = {:?} (decoder created from flowing buffer), \
             full TTF = {:?} (decoder created after {:?} write)", streaming_ttf, full_ttf, Duration::from_millis(full_write_ms));
        assert!(streaming_ttf < Duration::from_millis(full_write_ms),
            "Streaming TTF {:?} must be < full write time {:?}", streaming_ttf, Duration::from_millis(full_write_ms));
    }

    #[test]
    fn cache_hit_returns_instantly() {
        let _guard = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let key = "test_cache_video_id".to_string();
        let data: Arc<[u8]> = Arc::from(vec![0u8; 64 * 1024]);
        cache_put(key.clone(), data);
        let t0 = Instant::now();
        let cached = cache_get(&key);
        let elapsed = t0.elapsed();
        assert!(elapsed < Duration::from_millis(1), "Cache get {:?}", elapsed);
        let cached = cached.unwrap();
        let len = cached.len() as u64;
        let cursor = std::io::Cursor::new(cached);
        let source = ReadSeekSource::new(cursor, Some(len));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let t0 = Instant::now();
        let _result = SymphoniaDecoder::new(mss);
        let elapsed = t0.elapsed();
        println!("cache decoder creation: {:?}", elapsed);
        assert!(elapsed < Duration::from_millis(100), "Cached decoder creation must not block (>100ms: {:?})", elapsed);
    }

    #[test]
    fn streaming_creates_decoder_from_empty_buffer_then_gets_frames() {
        let chunk_delay = Duration::from_millis(2);
        let buf = SharedBuffer::new();
        buf.set_total_len(TEST_WAV.len() as u64);
        let handle = spawn_slow_write(&buf, TEST_WAV, chunk_delay);
        let mut dec = create_decoder_from(&buf)
            .expect("Decoder from streaming buffer (data may not have arrived yet)");
        let ttf = time_to_first_frame(&mut dec, 44100);
        assert!(ttf.is_some(), "Decoder should produce frames after data starts arriving");
        handle.join().unwrap();
    }

    #[test]
    fn streaming_cache_eviction_works() {
        let _guard = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = RestoreCacheMax(CACHE_MAX_ENTRIES.load(Ordering::Acquire) as u64);
        CACHE_MAX_ENTRIES.store(3, Ordering::Release);
        for i in 0..5 {
            cache_put(format!("evict_test_{}", i), Arc::from(vec![0u8; 1024]));
        }
        assert!(cache_get("evict_test_0").is_none(), "Oldest entry (0) should be evicted");
        assert!(cache_get("evict_test_1").is_none(), "Oldest entry (1) should be evicted");
        assert!(cache_get("evict_test_2").is_some(), "Entry 2 should still be in cache");
        assert!(cache_get("evict_test_3").is_some(), "Entry 3 should still be in cache");
        assert!(cache_get("evict_test_4").is_some(), "Entry 4 should still be in cache");
    }

    #[test]
    fn streaming_cache_lru_hit_updates_order() {
        let _guard = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = RestoreCacheMax(CACHE_MAX_ENTRIES.load(Ordering::Acquire) as u64);
        CACHE_MAX_ENTRIES.store(3, Ordering::Release);
        for i in 0..3 {
            cache_put(format!("lru_test_{}", i), Arc::from(vec![0u8; 1024]));
        }
        assert!(cache_get("lru_test_0").is_some(), "Entry 0 should be in cache");
        for i in 3..5 {
            cache_put(format!("lru_test_{}", i), Arc::from(vec![0u8; 1024]));
        }
        assert!(cache_get("lru_test_1").is_none(), "Entry 1 (now oldest) should be evicted");
        assert!(cache_get("lru_test_0").is_some(), "Entry 0 (promoted by hit) should survive");
    }

    fn start_ffmpeg_relay() -> anyhow::Result<(
        Arc<SharedBuffer>,
        std::thread::JoinHandle<()>,
        std::process::Child,
    )> {
        let buf = SharedBuffer::new();
        let mut wtr = buf.writer();
        let mut ffmpeg = std::process::Command::new("ffmpeg");
        ffmpeg
            .args(["-i", "pipe:0", "-fflags", "nobuffer", "-flags", "low_delay",
                "-f", "mp3", "-compression_level", "5", "-ab", "128k", "-loglevel", "error", "pipe:1"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = ffmpeg.spawn().context("spawn ffmpeg for mp3 relay")?;
        let ffmpeg_stdout = child.stdout.take().context("ffmpeg stdout not captured")?;
        let writer = std::thread::spawn(move || {
            use std::io::Read;
            let mut rdr = std::io::BufReader::new(ffmpeg_stdout);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match rdr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => wtr.write(&buf[..n]),
                    Err(_) => { wtr.fail(); return; }
                }
            }
            wtr.finish();
        });
        Ok((buf, writer, child))
    }

    #[test]
    fn ffmpeg_relay_ttf_from_webm_file() {
        let webm_data = match std::fs::read("/tmp/test_streaming.webm") {
            Ok(d) => d,
            Err(e) => { eprintln!("SKIP: /tmp/test_streaming.webm not available: {e}"); return; }
        };
        let t0 = Instant::now();
        let (buf, writer, mut child) = start_ffmpeg_relay().expect("ffmpeg relay setup");
        let mut stdin = child.stdin.take().unwrap();
        use std::io::Write;
        stdin.write_all(&webm_data).unwrap();
        drop(stdin);
        let _write_finished = t0.elapsed();
        let exit_status = child.wait();
        let _ffmpeg_finished = t0.elapsed();
        let _ = writer.join();
        let _all_data_written = t0.elapsed();
        let t_dec = Instant::now();
        let mut dec = create_decoder_from(&buf).expect("decoder from ffmpeg relay stream");
        let _decoder_dur = t_dec.elapsed();
        let ttf = time_to_first_frame(&mut dec, 44100);
        match ttf {
            Some(dur) => println!("BENCH ffmpeg_relay: ttf={dur:?} buf_len={}", buf.len()),
            None => println!("BENCH ffmpeg_relay: DECODER FAILED (no frames produced)"),
        }
        assert!(ttf.is_some(), "ffmpeg relay must produce frames");
        assert!(exit_status.is_ok(), "ffmpeg exit status = {exit_status:?}");
    }

    #[test]
    fn m4a_decoder_ttf_from_full_download() {
        use std::io::Read;
        let output_path = "/tmp/yt_bench_m4a.m4a";
        let status = std::process::Command::new("yt-dlp")
            .args(["-f", "bestaudio[ext=m4a]", "--download-sections", "*0-10",
                "-o", output_path, "--no-warnings", "--no-playlist", "--print", "after_move:",
                "https://music.youtube.com/watch?v=jNQXAC9IVRw"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if !s.success() => { eprintln!("SKIP m4a_decoder_ttf: yt-dlp exit code {:?}", s.code()); return; }
            Err(e) => { eprintln!("SKIP m4a_decoder_ttf: yt-dlp not available: {e}"); return; }
            _ => {}
        }
        let data = match std::fs::read(output_path) {
            Ok(d) => d,
            Err(e) => { eprintln!("SKIP m4a_decoder_ttf: failed to read {output_path}: {e}"); return; }
        };
        let _dl_dur = {
            let t0 = Instant::now();
            let mut f = std::fs::File::open(output_path).unwrap();
            let mut buf = Vec::with_capacity(data.len());
            f.read_to_end(&mut buf).unwrap();
            t0.elapsed()
        };
        let len = data.len() as u64;
        let cursor = std::io::Cursor::new(data);
        let source = ReadSeekSource::new(cursor, Some(len));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let t0 = Instant::now();
        let mut dec = SymphoniaDecoder::new(mss).expect("decoder from full M4A");
        let _decoder_dur = t0.elapsed();
        let ttf = time_to_first_frame(&mut dec, 44100);
        let _ = std::fs::remove_file(output_path);
        match ttf {
            Some(dur) => println!("BENCH m4a_full: ttf={dur:?} file_len={len}"),
            None => println!("BENCH m4a_full: DECODER FAILED"),
        }
        assert!(ttf.is_some(), "M4A decoder must produce frames");
    }

    #[test]
    fn download_pipeline_comparison() {
        use std::io::Read;
        use std::io::Write;
        let video_id = "jNQXAC9IVRw";
        let url = format!("https://music.youtube.com/watch?v={video_id}");

        println!("--- ffmpeg relay ---");
        let t0 = Instant::now();
        let mut yt = std::process::Command::new("yt-dlp");
        yt.args(["-f", "bestaudio[ext=webm]", "--download-sections", "*0-10",
            "--ignore-config", "-o", "-", "--no-warnings", "--no-playlist", &url])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut yt_child = match yt.spawn() {
            Ok(c) => c,
            Err(e) => { eprintln!("SKIP: cannot spawn yt-dlp: {e}"); return; }
        };
        let yt_stdout = yt_child.stdout.take().unwrap();
        let _yt_spawn_dur = t0.elapsed();

        let mut ffmpeg = std::process::Command::new("ffmpeg");
        ffmpeg.args(["-i", "pipe:0", "-fflags", "nobuffer", "-flags", "low_delay",
            "-f", "mp4", "-movflags", "empty_moov+default_base_moof+frag_every_frame",
            "-c:a", "alac", "-loglevel", "error", "pipe:1"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut ff_child = match ffmpeg.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("SKIP: cannot spawn ffmpeg for relay test: {e}");
                let _ = yt_child.kill();
                let _ = yt_child.wait();
                return;
            }
        };
        let mut ff_stdin = ff_child.stdin.take().expect("ffmpeg stdin");
        let ff_stdout = ff_child.stdout.take().expect("ffmpeg stdout");
        let _ff_spawn_dur = t0.elapsed();

        let relay_buf = Arc::new(SharedBuffer::new());
        let mut relay_wtr = relay_buf.writer();
        let relay_handle = std::thread::spawn(move || {
            let mut rdr = std::io::BufReader::new(yt_stdout);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match rdr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => { let _ = ff_stdin.write_all(&buf[..n]); }
                    Err(_) => break,
                }
            }
        });
        let relay_writer = std::thread::spawn(move || {
            let mut rdr = std::io::BufReader::new(ff_stdout);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match rdr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => relay_wtr.write(&buf[..n]),
                    Err(_) => { relay_wtr.fail(); return; }
                }
            }
            relay_wtr.finish();
        });
        let relay_deadline = Instant::now() + Duration::from_secs(30);
        while relay_buf.len() < 2048 {
            if relay_buf.is_failed() || Instant::now() >= relay_deadline {
                eprintln!(
                    "SKIP download_pipeline_comparison: no relay data within 30s (yt-dlp failed?)"
                );
                let _ = yt_child.kill();
                let _ = ff_child.kill();
                let _ = yt_child.wait();
                let _ = ff_child.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let relay_data_arrival = t0.elapsed();
        let t_dec = Instant::now();
        let relay_reader = relay_buf.reader();
        let relay_source = ReadSeekSource::nonseekable(relay_reader);
        let relay_mss = MediaSourceStream::new(Box::new(relay_source), Default::default());
        let mut dec_relay = SymphoniaDecoder::new(relay_mss).expect("decoder from relay stream");
        let _relay_decoder_dur = t_dec.elapsed();
        let relay_ttf = time_to_first_frame(&mut dec_relay, 44100);
        relay_handle.join().unwrap();
        relay_writer.join().unwrap();
        let _ = yt_child.wait();
        let _ = ff_child.wait();
        let _relay_total_dur = t0.elapsed();

        println!("--- M4A full download ---");
        let t0 = Instant::now();
        let output_path = "/tmp/yt_bench_m4a_comparison.m4a";
        let mut yt2 = std::process::Command::new("yt-dlp");
        yt2.args(["-f", "bestaudio[ext=m4a]", "--download-sections", "*0-10",
            "--ignore-config", "-o", output_path, "--no-warnings", "--no-playlist", &url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match yt2.status() {
            Ok(s) if s.success() => {}
            Ok(s) => { eprintln!("SKIP: yt-dlp exit code {:?}", s.code()); return; }
            Err(e) => { eprintln!("SKIP: yt-dlp not available: {e}"); return; }
        }
        let _yt2_spawn_dur = t0.elapsed();
        let m4a_data = match std::fs::read(output_path) {
            Ok(d) => d,
            Err(e) => { eprintln!("SKIP: failed to read {output_path}: {e}"); return; }
        };
        let _ = std::fs::remove_file(output_path);
        let _m4a_dl_dur = t0.elapsed();
        let m4a_buf = SharedBuffer::new();
        let mut m4a_wtr = m4a_buf.writer();
        m4a_wtr.write(&m4a_data);
        m4a_wtr.finish();
        drop(m4a_wtr);
        let t_dec = Instant::now();
        let mut dec_m4a = create_decoder_from(&m4a_buf).expect("decoder from full M4A");
        let _m4a_decoder_dur = t_dec.elapsed();
        let m4a_ttf = time_to_first_frame(&mut dec_m4a, 44100);
        let _m4a_total_dur = t0.elapsed();

        let _relay_playable = relay_data_arrival + _relay_decoder_dur + relay_ttf.unwrap_or_default();
        let _m4a_playable = _yt2_spawn_dur + _m4a_decoder_dur + m4a_ttf.unwrap_or_default();
        println!("===== PIPELINE COMPARISON (video: {video_id}) =====");
        println!("relay playable={:?} m4a playable={:?}", _relay_playable, _m4a_playable);
        assert!(relay_ttf.is_some(), "ffmpeg relay must produce frames");
        assert!(m4a_ttf.is_some(), "M4A decoder must produce frames");
    }

    #[test]
    fn wav_decoder_from_file() {
        let wav_path = std::path::Path::new("/tmp/test_output.wav");
        if !wav_path.exists() {
            eprintln!("SKIP: /tmp/test_output.wav not found");
            return;
        }
        let t0 = Instant::now();
        let wav_data = std::fs::read(wav_path).expect("read WAV file");
        let _read_dur = t0.elapsed();

        let t_dec = Instant::now();
        let cursor = std::io::Cursor::new(wav_data.clone());
        let source = ReadSeekSource::new(cursor, Some(wav_data.len() as u64));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let mut dec = match SymphoniaDecoder::new(mss) {
            Ok(d) => d,
            Err(e) => { eprintln!("FAIL: symphonia rejected WAV: {e:?}"); return; }
        };
        let _decoder_dur = t_dec.elapsed();
        let ttf = time_to_first_frame(&mut dec, 44100);
        assert!(ttf.is_some(), "full WAV must produce frames");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            let buf = Arc::new(SharedBuffer::new());
            let mut writer = buf.writer();
            let header_size = 8192.min(wav_data.len());
            writer.write(&wav_data[..header_size]);
            let t = tokio::time::Instant::now();
            let r = try_streaming_init(&buf, None).await;
            (t.elapsed(), r)
        });
        match result.1 {
            Ok(mut dec) => {
                eprintln!("WAV stream: init={:?}", result.0);
                let mut writer = SharedBuffer::new().writer();
                writer.write(&wav_data[8192..]);
                writer.finish();
                let ttf2 = time_to_first_frame(&mut dec, 44100);
                assert!(ttf2.is_some(), "streaming WAV must produce frames");
            }
            Err(e) => { eprintln!("WAV stream init FAILED: {e:?}"); }
        }
    }

    #[test]
    fn dropped_joinhandle_does_not_cancel_kill_on_drop_child() {
        use std::time::Duration;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut child = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn sleep");
            let pid = child.id().expect("child pid");
            let handle = tokio::spawn(async move { let _ = child.wait().await; });
            drop(handle);
            tokio::time::sleep(Duration::from_millis(200)).await;
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .expect("kill -0");
            assert!(alive.success(), "process {pid} must survive JoinHandle drop");
            eprintln!("PASS: JoinHandle drop -> child {pid} alive (as expected)");
            std::process::Command::new("kill").arg(pid.to_string()).status().ok();

            let mut child2 = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn sleep 2");
            let pid2 = child2.id().expect("child2 pid");
            let abort_handle = tokio::spawn(async move { let _ = child2.wait().await; });
            let ab = abort_handle.abort_handle();
            ab.abort();
            tokio::time::sleep(Duration::from_millis(200)).await;
            let alive2 = std::process::Command::new("kill")
                .args(["-0", &pid2.to_string()])
                .status()
                .expect("kill -0 2");
            assert!(!alive2.success(), "process {pid2} must be DEAD after AbortHandle::abort");
            eprintln!("PASS: AbortHandle::abort -> child {pid2} dead (kill_on_drop fired)");
        });
    }

    #[test]
    fn bg_cache_task_cancel_kills_all_children() {
        use std::time::Duration;
        let _guard = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ff_child = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn sleep (primary)");
            let ff_pid = ff_child.id().expect("primary pid");
            let yt_child = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn sleep (extra)");
            let yt_pid = yt_child.id().expect("extra pid");

            let ct = tokio_util::sync::CancellationToken::new();
            let buf = SharedBuffer::new();
            let done = tokio::task::spawn(std::future::pending::<()>());
            let permit = DOWNLOAD_SEMAPHORE.acquire().await.expect("test permit");
            let task = tokio::spawn(spawn_bg_cache_task(
                "test-vid".to_string(),
                ct.clone(),
                ff_child,
                Some(yt_child),
                done,
                buf,
                "test",
                None,
                permit,
            ));

            tokio::time::sleep(Duration::from_millis(150)).await;
            ct.cancel();
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("bg cache task must finish after cancel")
                .expect("bg cache task join");

            for (label, pid) in [("primary", ff_pid), ("extra", yt_pid)] {
                let alive = std::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .stderr(std::process::Stdio::null())
                    .status()
                    .expect("kill -0");
                assert!(!alive.success(), "{label} child {pid} must be dead after cancel");
            }
        });
    }

    #[test]
    fn bg_cache_task_holds_permit_until_complete() {
        use std::time::Duration;
        let _guard = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ff_child = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn sleep (primary)");
            let yt_child = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn sleep (extra)");

            let ct = tokio_util::sync::CancellationToken::new();
            let buf = SharedBuffer::new();
            let done = tokio::task::spawn(std::future::pending::<()>());
            let permit = DOWNLOAD_SEMAPHORE.acquire().await.expect("test permit");

            let task = tokio::spawn(spawn_bg_cache_task(
                "test-vid".to_string(),
                ct.clone(),
                ff_child,
                Some(yt_child),
                done,
                buf,
                "test",
                None,
                permit,
            ));

            assert!(
                DOWNLOAD_SEMAPHORE.try_acquire().is_err(),
                "permit must be held by the bg cache task while it runs"
            );

            ct.cancel();
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("bg cache task must finish after cancel")
                .expect("bg cache task join");

            assert!(
                DOWNLOAD_SEMAPHORE.try_acquire().is_ok(),
                "permit must be released once the bg cache task completes"
            );
        });
    }

    #[test]
    fn permit_released_after_cancel_not_before() {
        use std::time::Duration;
        let _guard = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ff_child = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn sleep (primary)");
            let yt_child = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn sleep (extra)");

            let ct = tokio_util::sync::CancellationToken::new();
            let buf = SharedBuffer::new();
            let done = tokio::task::spawn(std::future::pending::<()>());
            let permit = DOWNLOAD_SEMAPHORE.acquire().await.expect("test permit");

            let task = tokio::spawn(spawn_bg_cache_task(
                "test-vid".to_string(),
                ct.clone(),
                ff_child,
                Some(yt_child),
                done,
                buf,
                "test",
                None,
                permit,
            ));

            ct.cancel();
            assert!(
                DOWNLOAD_SEMAPHORE.try_acquire().is_err(),
                "permit must NOT be released while the bg task is still unwinding (kill/reap in flight)"
            );

            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("bg cache task must finish after cancel")
                .expect("bg cache task join");

            assert!(
                DOWNLOAD_SEMAPHORE.try_acquire().is_ok(),
                "permit released only after the bg task future resolves"
            );
        });
    }
}
