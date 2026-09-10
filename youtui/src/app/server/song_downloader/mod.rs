mod cache;
pub(crate) mod resolve;

pub use cache::{cache_clear, create_decoder_from_cache, set_cache_max_entries};

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
/// A held next/prev key queues one download per press, and each download would
/// spawn its own yt-dlp extraction. The primary path is token-free since the
/// client flip (DECISIONS.md:42) — no GVS mint — but a burst still fires a
/// burst of extractions at YouTube, the original bot-signal/throttle trigger.
/// This settle window (in `download_and_decode`, before the semaphore) lets a
/// burst coalesce to its final song before any yt-dlp spawns. 100ms still
/// covers key autorepeat (~30-40ms cadence) and any superseding press within
/// it; a leak at a sub-100ms deliberate-mash cadence is a mint-free extraction
/// killed on the next press — cheap next to the pre-flip mint flood.
const RESOLVE_SETTLE_MS: u64 = 100;
/// Shared tail of the ffmpeg invocation for ALAC-in-fragmented-mp4 streaming.
/// The `-i pipe:0` input precedes these mux flags (see DECISIONS.md:10).
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
    pub pot_provider: Option<resolve::PotProvider>,
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

pub(crate) fn check_ffmpeg() -> bool {
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
/// resolve path appends a POT-provider tag and the yt-dlp stderr line after it.
pub(crate) const AUTH_ERR: &str = "authentication error (stale cookies)";

/// Classify a yt-dlp stderr line as a *permanently* unavailable video
/// (removed, terminated account, region-blocked) as opposed to a transient
/// error (bot-check, bad cookie file, format/network issue). The bare
/// `Video unavailable` line is yt-dlp's own label for the permanently-dead
/// class on the music extractor; the variant suffixes it once used to print
/// (`... not available`, `... removed by the uploader`, `... in your country`)
/// are all contained in it, so matching the bare phrase covers every form.
/// Only the permanent class session-flags the song (skip + notify; never fed
/// to the transient-failure halt counter); everything else stays transient.
fn is_permanently_unavailable(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("video unavailable")
}

/// Classify a yt-dlp stderr line as an authentication/cookie problem: sign-in
/// required, bot check, invalid cookies. These are a login/config issue, not a
/// dead video — the song must be skipped and the user notified, never removed.
/// A bare `HTTP Error 403` is deliberately NOT here: on a signed-in session
/// that is the nsig/GVS-token CDN throttle (see `is_throttle_line`), and for a
/// guest there are no cookies to be stale — either way it is a transient
/// download failure that feeds the halt counter, never the stale-cookie class.
pub(crate) fn is_auth_error_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    ["sign in", "not a bot", "authentication", "requires login", "signed-in"]
        .iter()
        .any(|needle| line.contains(needle))
        || (line.contains("cookie") && line.contains("does not look like"))
}

/// Classify a yt-dlp stderr line as the CDN throttle (`HTTP Error 403`). The
/// relay's yt-dlp reports this when googlevideo refuses the fetch — the
/// nsig/GVS-token throttling wave, NOT a dead video or stale cookies. The
/// buffer is marked so the pipeline retries the relay once instead of skipping
/// the song.
fn is_throttle_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("forbidden")
        || line.contains("access denied")
        || (line.contains("403")
            && (line.contains("http") || line.contains("server") || line.contains("error")))
}

/// Classify a yt-dlp stderr line as a refused-format error on the forced
/// player client (`Requested format is not available`). This is a client
/// problem, not a dead video or a throttle: google stripped/SABR'd/abandoned
/// the client's format set (GVS token not provided, SABR experiment active,
/// embedded-only client). The song is playable on a token-free client, so the
/// buffer is marked for a bounded client-fallback retry.
fn is_format_unavailable_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("requested format is not available")
}

fn spawn_stderr_handler(
    stderr: tokio::process::ChildStderr,
    cancel_token: tokio_util::sync::CancellationToken,
    buffer: Arc<SharedBuffer>,
    log_cancellation: bool,
    video_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let reader = tokio::io::BufReader::new(stderr);
        let mut lines = reader.lines();
        loop {
            if cancel_token.is_cancelled() {
                if log_cancellation {
                    debug!(%video_id, "yt-dlp stderr handler cancelled");
                }
                return;
            }
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if let Some(bytes) = parse_total_size(&line) {
                        debug!(%video_id, total_bytes = bytes, "Parsed total size from yt-dlp progress");
                        buffer.set_total_len(bytes);
                    } else if line.contains("ERROR") {
                        if is_permanently_unavailable(&line) {
                            buffer.mark_dead_video();
                        } else if is_auth_error_line(&line) {
                            buffer.mark_auth_error();
                        } else if is_throttle_line(&line) {
                            // A relay `HTTP Error 403: Forbidden` is the nsig/GVS-token
                            // CDN throttle — mark it so the pipeline may retry the
                            // relay once. warn! (not debug!) so a throttle wave is
                            // visible in the default-level log and a recovery is
                            // explainable.
                            warn!(%video_id, stderr_line = %line.trim(),
                                "yt-dlp 403 (throttled), marking buffer for relay retry");
                            buffer.mark_throttled();
                        } else if is_format_unavailable_line(&line) {
                            // The default clients have no playable formats
                            // (SABR/abandoned client), not a dead video. Mark
                            // it so the pipeline retries once through
                            // `web_music` with the GVS-token provider.
                            warn!(%video_id, stderr_line = %line.trim(),
                                "yt-dlp format unavailable (client/scrape), marking buffer for client fallback");
                            buffer.mark_format_unavailable();
                        }
                        warn!(%video_id, stderr_line = %line.trim(), "yt-dlp stderr (error), failing buffer");
                        buffer.fail();
                    } else if line.contains("WARNING") {
                        debug!(%video_id, stderr_line = %line.trim(), "yt-dlp stderr (warning)");
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    warn!(%video_id, error = %e, "yt-dlp stderr read error");
                    break;
                }
            }
        }
        debug!(%video_id, "yt-dlp stderr stream ended");
    })
}

/// Logs every ffmpeg stderr line. ffmpeg runs with `-loglevel error`, so it
/// prints nothing on success and only real diagnostics on failure; the line
/// logged just before an empty-pipe bail names the actual cause (decode error,
/// codec). Pure diagnosability — the handler never touches the buffer, so a
/// healthy stream's stderr stays silent and playback is unchanged.
fn spawn_ffmpeg_stderr_handler(
    stderr: tokio::process::ChildStderr,
    video_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let reader = tokio::io::BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            warn!(%video_id, stderr_line = %line.trim(), "ffmpeg stderr (error)");
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

/// Spawn ffmpeg as an ALAC-in-fragmented-mp4 muxer reading the yt-dlp relay's
/// stdout over stdin, and wire its stdout into the shared buffer. Returns the
/// stderr logger, the buffer writer, the child, and the stdin feeding the relay.
fn spawn_ffmpeg(
    writer: SharedBufferWriter,
    label: &'static str,
    video_id: &str,
) -> anyhow::Result<FfmpegSpawn> {
    let mut ffmpeg = tokio::process::Command::new("ffmpeg");
    apply_child_env(&mut ffmpeg);
    ffmpeg
        .arg("-i")
        .arg("pipe:0")
        .stdin(std::process::Stdio::piped());
    ffmpeg
        .args(ALAC_FFMPEG_ARGS)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = ffmpeg.spawn().with_context(|| format!("spawn {label}"))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().context("no ffmpeg stdout")?;
    let stderr = child.stderr.take().context("no ffmpeg stderr")?;
    let stderr_handle = spawn_ffmpeg_stderr_handler(stderr, video_id.to_string());
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

/// Common bail-out for a failed source buffer: permanently unavailable video,
/// auth/cookie problem, or a generic format/network error. Each class surfaces
/// a distinct error.
fn bail_failed_buffer(
    buffer: &SharedBuffer,
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
    debug!(%video_id, elapsed = ?t0.elapsed(), "yt-dlp failed {context} — bailing early");
    anyhow::bail!("format not available (yt-dlp error)")
}

/// Allow a throttled relay attempt up to two retries (three capped relay
/// attempts total). The CDN's throttle wave periodically beats two consecutive
/// fresh mints (debug370 xAbGAyd_-W4: both capped relays 403'd → skipped,
/// then played on a re-select seconds later), so only a third consecutive
/// throttle is definitive. Dead/auth failures never throttle, and a third
/// throttled relay is still definitive: the queue halts via the transient-
/// failure counter exactly as before, never an endless relay loop.
fn relay_throttle_retry(
    buffer: &SharedBuffer,
    relay_attempts: usize,
    video_id: &str,
    t0: tokio::time::Instant,
) -> bool {
    if buffer.is_throttled() && relay_attempts <= 2 {
        warn!(%video_id, elapsed = ?t0.elapsed(),
            "Relay throttled (403) — retrying with a fresh resolve (attempt {}/3)", relay_attempts + 1);
        true
    } else {
        false
    }
}

/// Allow exactly one client fallback per song: when the default clients report
/// no playable formats (`Requested format is not available`), retry once
/// through `web_music`, where a present POT provider mints a fresh per-video
/// GVS token (slow, ~9.4s, but the most reliably playable client). Bounded —
/// once `web_music_fallback_used`, the fallback's own refusal is definitive
/// (bail, halt counter intact). Only meaningful while a provider is installed:
/// without one there is no token source to escape to, and the retry guard
/// refuses.
fn relay_client_fallback_retry(
    buffer: &SharedBuffer,
    web_music_fallback_used: bool,
    provider_available: bool,
    video_id: &str,
    t0: tokio::time::Instant,
) -> bool {
    if buffer.is_format_unavailable() && !web_music_fallback_used && provider_available {
        warn!(%video_id, elapsed = ?t0.elapsed(),
            "default-client formats unavailable — retrying once via web_music (GVS token)");
        true
    } else {
        false
    }
}

/// Unified retry-decision ladder for the `'attempt` loop's three bail points
/// (no-data, empty-pipe, ffmpeg-exit). Runs the throttle retry (relay →
/// relay → relay, capped at three attempts) then the client fallback (default
/// clients → web_music+POT, exactly once). Returns `true` when a retry was
/// scheduled — the caller must `continue 'attempt`; `false` means no retry
/// applies and the caller bails with its site-specific message.
fn try_pipeline_retry(
    buffer: &SharedBuffer,
    relay_attempts: usize,
    web_music_fallback_used: &mut bool,
    provider_available: bool,
    video_id: &str,
    t0: tokio::time::Instant,
) -> bool {
    if relay_throttle_retry(buffer, relay_attempts, video_id, t0) {
        return true;
    }
    if relay_client_fallback_retry(
        buffer,
        *web_music_fallback_used,
        provider_available,
        video_id,
        t0,
    ) {
        *web_music_fallback_used = true;
        return true;
    }
    false
}

/// Build the yt-dlp command for streaming a song to stdout, with the auth
/// cookie/header applied. Shared by the relay (WebM→ffmpeg) and direct M4A
/// paths; the caller configures stdio and spawns.
fn build_ytdlp_command(
    cfg: &DownloadConfig,
    format: &str,
    web_music_fallback: bool,
) -> tokio::process::Command {
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
        cfg.pot_provider.as_ref(),
        cfg.cookie_header.as_deref(),
        cfg.js_runtime.as_deref(),
        &cfg.video_id,
        web_music_fallback,
    );
    debug!(%cfg.video_id, format, js_runtime = ?cfg.js_runtime, cookie_path = ?cfg.cookie_path, "build_ytdlp_command: spawning yt-dlp");
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
    web_music_fallback: bool,
) -> anyhow::Result<YtDlpSpawn> {
    let mut cmd = build_ytdlp_command(cfg, format, web_music_fallback);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().context("spawn yt-dlp")?;
    let stdout = child.stdout.take().context("no stdout from yt-dlp")?;
    let stderr = child.stderr.take().context("no stderr from yt-dlp")?;
    debug!(%cfg.video_id, elapsed = ?t0.elapsed(), "yt-dlp spawned");
    let stderr_handle = spawn_stderr_handler(
        stderr,
        cfg.cancel_token.clone(),
        buffer,
        log_cancellation,
        cfg.video_id.clone(),
    );
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
) -> anyhow::Result<SymphoniaDecoder> {
    // ALAC transcoding requires ffmpeg; without it the pipeline must use the
    // direct M4A path, since symphonia cannot decode Opus in a webm container.
    //
    // The relay is the only download path: yt-dlp streams the audio, and
    // ffmpeg transcodes it to fragmented-MP4 ALAC. The first attempt runs on
    // yt-dlp's default (token-free) playback clients — measured ~2.5s to first
    // byte vs ~9.4s for a web_music GVS mint — so the common path pays no
    // resolve penalty. If the default clients report no playable formats, one
    // bounded retry re-runs the pipeline through `web_music` + POT
    // (`relay_client_fallback_retry`). A throttled relay is retried up to
    // twice (`relay_throttle_retry`), capped at three relay attempts total: the
    // CDN's throttle wave occasionally beats two consecutive fresh mints and a
    // third (fresh mint, possibly different edge) plays. Dead/auth failures
    // never throttle, so a truly unplayable song fails fast. The
    // permit stays owned by this function across attempts — it is only moved
    // into the background cache task on a successful streaming init, which
    // returns.
    let mut relay_attempts = 0;
    let mut web_music_fallback_used = false;
    'attempt: loop {
        relay_attempts += 1;

        let buffer = SharedBuffer::new();
        let writer = buffer.writer();

        let (_stderr_handle, stdout_handle, mut child, _relay_handle, mut yt_child) = if ffmpeg_avail
        {
            // Spawn ffmpeg before yt-dlp. If the relay's yt-dlp fails the
            // buffer instantly (e.g. a throttled 403 written before ffmpeg
            // was up), the init-wait loop below bails on the first
            // `is_failed()` check and kill_on_drop would kill a just-spawned
            // ffmpeg before it produced its first byte. Starting ffmpeg
            // first guarantees it is running (and reading `pipe:0`) before
            // the relay's output can fail the buffer, and overlaps its
            // startup with the yt-dlp spawn.
            let (_ffmpeg_stderr_handle, write_handle, ffmpeg_child, ffmpeg_stdin) =
                spawn_ffmpeg(writer, "ffmpeg", &cfg.video_id)?;
            let mut ffmpeg_stdin = ffmpeg_stdin.context("no ffmpeg stdin")?;

            let YtDlpSpawn { stderr_handle, stdout: yt_stdout, child: yt_dlp_child } =
                spawn_ytdlp(cfg, "ba/bestaudio", buffer.clone(), t0, true, web_music_fallback_used)?;

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
                spawn_ytdlp(cfg, "bestaudio[ext=m4a]/bestaudio/bestaudio*", buffer.clone(), t0, false, false)?;

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
            if try_pipeline_retry(
                &buffer,
                relay_attempts,
                &mut web_music_fallback_used,
                cfg.pot_provider.is_some(),
                &cfg.video_id,
                t0,
            ) {
                continue 'attempt;
            }
            bail_failed_buffer(&buffer, &cfg.video_id, t0, "before data arrived")?;
            let current = buffer.len();
            if current == 0 {
                // A source that has already exited without emitting a byte is
                // dead or unavailable (dead video, format gone). A source that
                // is still running may simply be slow to produce its first
                // byte; skipping a playable song because it warmed up slowly is
                // worse than waiting, so keep polling until it exits, produces
                // data, fails, is cancelled, or the patience window elapses.
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
                            // so a 403-refused relay breaks into the throttle-retry
                            // guard after the empty-pipe loop instead of being
                            // misread as a dead pipe.
                            tokio::task::yield_now().await;
                            if buffer.is_failed() {
                                break;
                            }
                            debug!(%cfg.video_id, elapsed = ?t0.elapsed(),
                                "Source exited with an empty pipe");
                            bail!("format not available (source exited, empty pipe)");
                        }
                        EmptyPipeVerdict::Cancelled => {
                            bail!("download cancelled during empty-pipe wait");
                        }
                        EmptyPipeVerdict::PatienceElapsed => {
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
                if try_pipeline_retry(
                    &buffer,
                    relay_attempts,
                    &mut web_music_fallback_used,
                    cfg.pot_provider.is_some(),
                    &cfg.video_id,
                    t0,
                ) {
                    continue 'attempt;
                }
                bail_failed_buffer(&buffer, &cfg.video_id, t0, "during empty-pipe wait")?;
            }
            debug!(%cfg.video_id, stream_type = "ffmpeg→alac-mp4", buf_len = buffer.len(), elapsed = ?t0.elapsed(),
                "Trying early decoder init");

            match try_streaming_init_nonseekable(&buffer).await {
                Ok(decoder) => {
                    debug!(%cfg.video_id, buf_len = buffer.len(), elapsed = ?t0.elapsed(),
                        "Streaming decoder init succeeded");
                    let _cache_task = tokio::spawn(spawn_bg_cache_task(
                        cfg.video_id.clone(),
                        cfg.cancel_token.clone(),
                        child,
                        yt_child,
                        stdout_handle,
                        buffer.clone(),
                        "ffmpeg",
                        Some(t0),
                        _permit,
                    ));
                    (decoder, false)
                }
                Err(stream_err) => {
                    debug!(%cfg.video_id, error = %stream_err,
                        "Streaming decoder init failed, waiting for ffmpeg relay stream to complete");
                    let wait_result = tokio::select! {
                        biased;
                        _ = cfg.cancel_token.cancelled() => {
                            kill_and_reap(&mut child, &mut yt_child).await;
                            bail!("ffmpeg download cancelled during fallback wait");
                        }
                        res = tokio::time::timeout(
                            std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_S),
                            stdout_handle,
                        ) => res,
                    };
                    match wait_result {
                        Ok(Ok(())) => {}
                        Ok(Err(join_err)) => {
                            bail!("ffmpeg writer task panicked: {join_err}");
                        }
                        Err(_elapsed) => {
                            kill_and_reap(&mut child, &mut yt_child).await;
                            bail!("ffmpeg download timed out ({}s)", DOWNLOAD_TIMEOUT_S);
                        }
                    }

                    let status = child.wait().await.with_context(|| "wait ffmpeg".to_string())?;
                    if !status.success() {
                        let code = exit_code_string(&status);
                        // The 403 throttle mark can still be in flight when the
                        // writer task resolves on stdout EOF (see the empty-pipe
                        // Site-2 handling for the same race). Yield once so the
                        // stderr handler's mark lands before classifying the exit
                        // — otherwise a throttled relay is misread as a generic
                        // failure and the song skips instead of relay-retrying.
                        tokio::task::yield_now().await;
                        if try_pipeline_retry(
                            &buffer,
                            relay_attempts,
                            &mut web_music_fallback_used,
                            cfg.pot_provider.is_some(),
                            &cfg.video_id,
                            t0,
                        ) {
                            continue 'attempt;
                        }
                        bail!("ffmpeg exited with code {code}");
                    }
                    debug!(%cfg.video_id, "ffmpeg completed successfully");

                    debug!(%cfg.video_id, buf_len = buffer.len(),
                        "Creating decoder from completed download (fallback)");
                    let d = decoder_from_buffer(&buffer, None, "mp4-fallback")?;
                    (d, true)
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

    // Rapid song switches (a held next/prev key) enqueue one download per
    // press, and each download spawns a yt-dlp that mints a fresh PO token — a
    // token-mint flood the CDN reads as a bot signal and answers with 403
    // throttles. Settle briefly before the semaphore so a burst coalesces to
    // its final song: presses superseded within this window cancel here and
    // never spawn yt-dlp. The single-song hot path pays it once (a small slice
    // of the ~10s relay), and decoder-cache hits return above this point.
    tokio::select! {
        biased;
        _ = cfg.cancel_token.cancelled() => {
            debug!(%cfg.video_id, "download cancelled during settle");
            anyhow::bail!("download cancelled during settle");
        }
        _ = tokio::time::sleep(std::time::Duration::from_millis(RESOLVE_SETTLE_MS)) => {}
    }

    let ffmpeg_avail = check_ffmpeg();

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

    ytdlp_pipeline(&cfg, ffmpeg_avail, _permit, t0).await
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
            "ERROR: [youtube] gPBZjyavHwc: Video unavailable",
            "ERROR: [youtube] X: Video unavailable in your country",
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
            "ERROR: unable to download video data: HTTP Error 403: Forbidden",
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

    #[test]
    fn relay_throttle_retry_decision() {
        let t0 = tokio::time::Instant::from_std(std::time::Instant::now());
        let throttled = SharedBuffer::new();
        throttled.mark_throttled();
        let failed = SharedBuffer::new();
        failed.fail();
        let plain = SharedBuffer::new();

// A throttled first relay (one relay so far) must get its retry.
        assert!(relay_throttle_retry(&throttled, 1, "v1", t0));
        // A throttled second relay (two relays so far) must get one more retry:
        // the CDN wave beats a second consecutive fresh mint often enough that
        // the song plays on a third (debug370 xAbGAyd_-W4: skipped at 2,
        // played on re-select). Two throttles are not definitive.
        assert!(relay_throttle_retry(&throttled, 2, "v1", t0));
        // A throttled third relay is definitive: three capped attempts total.
        assert!(!relay_throttle_retry(&throttled, 3, "v1", t0));
        // A non-throttle relay failure (dead/auth/format, whatever the mark)
        // is definitive per-song and must not consume a retry slot.
        assert!(!relay_throttle_retry(&failed, 1, "v1", t0));
        assert!(!relay_throttle_retry(&plain, 1, "v1", t0));
    }

    #[test]
    fn format_unavailable_line_classifies() {
        let unavailable = [
            "ERROR: [youtube] xyz: Requested format is not available",
            "ERROR: Requested format is not available",
            "ERROR: [youtube] xyz: Requested format is not available. Use --list-formats for a list of available formats",
            "WARNING: Requested format is not available; retrying with another client",
        ];
        for line in unavailable {
            assert!(
                is_format_unavailable_line(line),
                "expected format unavailable: {line}"
            );
        }

        let not_unavailable = [
            "ERROR: [youtube] xyz: HTTP Error 403: Forbidden",
            "ERROR: [youtube] NLkDhrzgrI8: Video unavailable. This video is not available",
            "ERROR: [youtube] xyz: Sign in to confirm you're not a bot",
            "ERROR: [youtube] xyz: HTTP Error 429: Too Many Requests",
            "ERROR: unable to download video data: HTTP Error 403: Forbidden",
        ];
        for line in not_unavailable {
            assert!(
                !is_format_unavailable_line(line),
                "expected not format-unavailable: {line}"
            );
        }
    }

    #[test]
    fn relay_client_fallback_retry_decision() {
        let t0 = tokio::time::Instant::from_std(std::time::Instant::now());
        let formats_gone = SharedBuffer::new();
        formats_gone.mark_format_unavailable();
        let failed = SharedBuffer::new();
        failed.fail();
        let throttled = SharedBuffer::new();
        throttled.mark_throttled();

        // A fresh default-client attempt with formats unavailable gets exactly
        // one web_music fallback (once per song).
        assert!(relay_client_fallback_retry(&formats_gone, false, true, "v1", t0));
        // An already-fallen-back song never falls back again — the web_music
        // attempt's own failure is definitive.
        assert!(!relay_client_fallback_retry(&formats_gone, true, true, "v1", t0));
        // No fallback without a provider: there is no GVS token source to
        // escape to, so the default-clients refusal is definitive.
        assert!(!relay_client_fallback_retry(&formats_gone, false, false, "v1", t0));
        // Classifiers stay exclusive: a throttle/plain failure is NOT a
        // reason to switch clients.
        assert!(!relay_client_fallback_retry(&throttled, false, true, "v1", t0));
        assert!(!relay_client_fallback_retry(&failed, false, true, "v1", t0));
        assert!(!relay_client_fallback_retry(&SharedBuffer::new(), false, true, "v1", t0));
    }

    #[test]
    fn try_pipeline_retry_decision() {
        let t0 = tokio::time::Instant::from_std(std::time::Instant::now());
        let throttled = SharedBuffer::new();
        throttled.mark_throttled();
        let formats_gone = SharedBuffer::new();
        formats_gone.mark_format_unavailable();
        let plain = SharedBuffer::new();

        // Throttle ladder wins: a throttled relay under the cap retries, and
        // the web_music flag is NOT touched by a throttle retry.
        let mut used = false;
        assert!(try_pipeline_retry(&throttled, 1, &mut used, true, "v1", t0));
        assert!(!used, "a throttle retry must not consume the client-fallback slot");
        assert!(try_pipeline_retry(&throttled, 2, &mut used, true, "v1", t0));
        // At the third throttled attempt the ladder falls through to the
        // fallback check, which does not apply to a throttled buffer.
        assert!(!try_pipeline_retry(&throttled, 3, &mut used, true, "v1", t0));

        // Client fallback: a fresh format-unavailable refusal with a provider
        // retries once AND commits the slot. A second use refuses.
        let mut used = false;
        assert!(try_pipeline_retry(&formats_gone, 1, &mut used, true, "v1", t0));
        assert!(used, "the client fallback must commit its one-shot slot");
        assert!(!try_pipeline_retry(&formats_gone, 1, &mut used, true, "v1", t0));
        // No provider → nothing to escape to → refuse.
        assert!(!try_pipeline_retry(&formats_gone, 1, &mut false, false, "v1", t0));
        // Plain failure → no retry of any kind.
        assert!(!try_pipeline_retry(&plain, 1, &mut false, true, "v1", t0));
    }

    // The E2E throttle tests below put fake `ffmpeg`/`yt-dlp` binaries on PATH
    // and run the full `download_and_decode` pipeline. They must run serially:
    // PATH is process-global, the global DOWNLOAD_SEMAPHORE permit is consumed,
    // and the relay's background cache task touches BYTE_CACHE.
    static PIPELINE_TEST_LOCK: Mutex<()> = Mutex::new(());

    // Fake ffmpeg: `-version` must succeed so check_ffmpeg() sees ffmpeg as
    // available; any real invocation reads `-i pipe:0` (the relay), where the
    // fake just copies stdin to stdout untouched so the ALAC bytes pass through
    // as-is. Every invocation logs its argv to `log`, so the E2E tests can
    // assert exactly how many relay passes ran.
    fn fake_ffmpeg_body(log: &std::path::Path) -> String {
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n\
case \" $* \" in\n\
  *\" -version \"*) exit 0 ;;\n\
esac\n\
cat\n",
            log.display()
        )
    }

    // Fake yt-dlp: the relay streams the ALAC fixture to stdout. A test body
    // appends its own 403/stream logic after this header.
    const YTDLP_FAKE_HEAD: &str = "#!/bin/sh\n";

    struct FakePath {
        dir: std::path::PathBuf,
    }

    impl FakePath {
        fn new() -> Self {
            // A per-process monotonic counter guarantees a unique dir even when
            // two tests construct a FakePath within the same clock tick (the
            // nanosecond timestamp alone collided under parallel runs, making
            // sibling tests share a counter file and corrupt the relay count).
            static FAKE_DIR_SEQ: std::sync::atomic::AtomicUsize =
                std::sync::atomic::AtomicUsize::new(0);
            let seq = FAKE_DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "youtui_fakebin_{}_{}",
                std::process::id(),
                seq
            ));
            std::fs::create_dir_all(&dir).expect("create fake binary dir");
            let old_path = std::env::var("PATH").unwrap_or_default();
            let new_path = format!("{}:{}", dir.display(), old_path);
            // PATH is process-global; only these E2E tests mutate it
            // (serialized by PIPELINE_TEST_LOCK). The sibling by-name spawns
            // (ffmpeg_relay_ttf_from_webm_file, m4a_decoder_ttf_from_full_download,
            // download_pipeline_comparison) can collide with the ~10ms window:
            // the common case degrades to a skip or a garbage bench, but a
            // real-yt-dlp + fake-ffmpeg pairing can panic download_pipeline_comparison
            // (webm cat'd through untranscoded) and a stray fake-ffmpeg spawn can
            // inflate the E2E pipe:0 count. Both need a rare temporal overlap and
            // are accepted: serializing the 30s bench against the lock would tax
            // every networked suite run, and the flake self-heals on re-run.
            // glibc setenv/getenv are mutex-protected, so concurrent reads by
            // other test threads are benign.
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

    fn ffmpeg_relay_invocations(log: &std::path::Path) -> usize {
        std::fs::read_to_string(log)
            .unwrap_or_else(|e| panic!("read fake ffmpeg invocation log {}: {e}", log.display()))
            .matches("pipe:0")
            .count()
    }

    fn alac_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ytmapi-rs/test_json/test_alac_fragmented.mp4")
            .canonicalize()
            .expect("resolve ALAC fixture path")
    }

    #[test]
    fn relay_streams_alac_end_to_end() {
        // The relay is the only download path: yt-dlp streams the audio, ffmpeg
        // transcodes it to fragmented-MP4 ALAC, and the pipeline decodes it.
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sem = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cache = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let fakebin = FakePath::new();
            let ffmpeg_log = fakebin.dir.join("ffmpeg.log");
            write_fake_bin(&fakebin.dir, "ffmpeg", &fake_ffmpeg_body(&ffmpeg_log));
            let fixture = alac_fixture_path();
            write_fake_bin(
                &fakebin.dir,
                "yt-dlp",
                &format!("{}cat '{}'\n", YTDLP_FAKE_HEAD, fixture.display()),
            );

            let video_id = format!("relay-e2e-{}", std::process::id());
            let cfg = DownloadConfig {
                yt_dlp_command: "yt-dlp".to_string(),
                video_id,
                pot_provider: None,
                cookie_path: None,
                cookie_header: None,
                js_runtime: None,
                cancel_token: tokio_util::sync::CancellationToken::new(),
            };

            let mut decoder = download_and_decode(cfg)
                .await
                .expect("the relay must stream and decode");
            assert!(
                time_to_first_frame(&mut decoder, 1024).is_some(),
                "the relay download must produce audio frames"
            );
            assert_eq!(
                ffmpeg_relay_invocations(&ffmpeg_log),
                1,
                "the ALAC relay (ffmpeg pipe:0) must run exactly once; \
                 a no-ffmpeg host silently taking the M4A path would log nothing"
            );
        });
    }

    #[test]
    fn throttle_relay_twice_then_third_relay_recovers_and_plays() {
        // The "fail then play" repro (debug370 xAbGAyd_-W4): the CDN wave
        // beats *two* consecutive fresh relays (two throttles), and the third
        // capped relay streams — the song must play on the first pipeline pass
        // instead of being skipped for a manual re-select.
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sem = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cache = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let fakebin = FakePath::new();
            let ffmpeg_log = fakebin.dir.join("ffmpeg.log");
            write_fake_bin(&fakebin.dir, "ffmpeg", &fake_ffmpeg_body(&ffmpeg_log));
            let fixture = alac_fixture_path();
            // First two relay invocations 403 (touch a counter file), the
            // third streams the fixture.
            let count_file = fakebin.dir.join("relay-count");
            write_fake_bin(
                &fakebin.dir,
                "yt-dlp",
                &format!(
                    "{}n=0; [ -e '{}' ] && n=$(cat '{}')\n\
                     n=$((n+1)); echo $n > '{}'\n\
                     if [ \"$n\" -ge 3 ]; then cat '{}'; exit 0; fi\n\
                     echo 'ERROR: [youtube] xyz: HTTP Error 403: Forbidden' >&2\n\
                     exit 1\n",
                    YTDLP_FAKE_HEAD,
                    count_file.display(),
                    count_file.display(),
                    count_file.display(),
                    fixture.display(),
                ),
            );

            let video_id = format!("throttle-e2e-third-relay-{}", std::process::id());
            let cfg = DownloadConfig {
                yt_dlp_command: "yt-dlp".to_string(),
                video_id,
                pot_provider: None,
                cookie_path: None,
                cookie_header: None,
                js_runtime: None,
                cancel_token: tokio_util::sync::CancellationToken::new(),
            };

            let mut decoder = download_and_decode(cfg)
                .await
                .expect("a doubly-throttled relay must be retried twice and recover");
            assert!(
                time_to_first_frame(&mut decoder, 1024).is_some(),
                "the third-relay-recovered download must produce audio frames"
            );
            assert_eq!(
                ffmpeg_relay_invocations(&ffmpeg_log),
                3,
                "the throttled relay must be retried twice (relay + relay + relay)"
            );
        });
    }

    #[test]
    fn throttled_relay_failure_bails_after_capped_relay_retries() {
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sem = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cache = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let fakebin = FakePath::new();
            let ffmpeg_log = fakebin.dir.join("ffmpeg.log");
            write_fake_bin(&fakebin.dir, "ffmpeg", &fake_ffmpeg_body(&ffmpeg_log));
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
                video_id,
                pot_provider: None,
                cookie_path: None,
                cookie_header: None,
                js_runtime: None,
                cancel_token: tokio_util::sync::CancellationToken::new(),
            };

            let err = match download_and_decode(cfg).await {
                Ok(_) => panic!(
                    "a relay throttled on its final (capped) attempt must not spawn a fourth relay"
                ),
                Err(e) => e,
            };
            assert!(
                err.to_string().starts_with("format not available"),
                "a throttled relay must bail as a generic transient failure, got: {err}"
            );
            assert_eq!(
                ffmpeg_relay_invocations(&ffmpeg_log),
                3,
                "a throttled relay must be retried twice and then bail; \
                 a fourth relay would spawn ffmpeg pipe:0 a fourth time"
            );
        });
    }

    #[test]
    fn cancel_download_during_settle_never_spawns_ytdlp() {
        // A press superseded within the settle window must abort before any
        // yt-dlp spawn: the settle exists so a held next/prev burst coalesces to
        // its final song instead of spawning one extraction per press. Cancel
        // 60ms in (inside the 100ms window) and assert no yt-dlp ever touched
        // the counter file — a settle removed/shortened below the cancel point
        // would let the pipeline reach the spawn and fail this.
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sem = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cache = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let fakebin = FakePath::new();
            let ffmpeg_log = fakebin.dir.join("ffmpeg.log");
            write_fake_bin(&fakebin.dir, "ffmpeg", &fake_ffmpeg_body(&ffmpeg_log));
            let count_file = fakebin.dir.join("yt-dlp_count");
            write_fake_bin(
                &fakebin.dir,
                "yt-dlp",
                &format!(
                    "{}echo 1 >> '{}'\ncat '{}'\n",
                    YTDLP_FAKE_HEAD,
                    count_file.display(),
                    alac_fixture_path().display(),
                ),
            );

            let token = tokio_util::sync::CancellationToken::new();
            let cfg = DownloadConfig {
                yt_dlp_command: "yt-dlp".to_string(),
                video_id: format!("settle-e2e-{}", std::process::id()),
                pot_provider: None,
                cookie_path: None,
                cookie_header: None,
                js_runtime: None,
                cancel_token: token.clone(),
            };

            let handle = tokio::spawn(download_and_decode(cfg));
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            token.cancel();
            let result = handle.await.expect("download task must not panic");
            let err = match result {
                Ok(_) => panic!("a cancel within the settle window must abort the download"),
                Err(e) => e,
            };
            assert!(
                err.to_string().contains("settle"),
                "cancel inside the settle window must bail at the settle, got: {err}"
            );
            assert!(
                !count_file.exists(),
                "no yt-dlp may spawn while the settle window is open; \
                 a removed/shortened settle lets the pipeline reach the spawn"
            );
        });
    }

    #[test]
    fn download_and_decode_pre_cancelled_bails_without_spawning() {
        // A token already cancelled before the download begins must bail at the
        // pre-start check, never reaching the settle or the yt-dlp spawn.
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sem = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cache = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let fakebin = FakePath::new();
            let ffmpeg_log = fakebin.dir.join("ffmpeg.log");
            write_fake_bin(&fakebin.dir, "ffmpeg", &fake_ffmpeg_body(&ffmpeg_log));
            let count_file = fakebin.dir.join("yt-dlp_count");
            write_fake_bin(
                &fakebin.dir,
                "yt-dlp",
                &format!("{}echo 1 >> '{}'\nexit 1\n", YTDLP_FAKE_HEAD, count_file.display()),
            );

            let token = tokio_util::sync::CancellationToken::new();
            token.cancel();
            let cfg = DownloadConfig {
                yt_dlp_command: "yt-dlp".to_string(),
                video_id: format!("precancel-e2e-{}", std::process::id()),
                pot_provider: None,
                cookie_path: None,
                cookie_header: None,
                js_runtime: None,
                cancel_token: token,
            };

            let err = match download_and_decode(cfg).await {
                Ok(_) => panic!("a pre-cancelled token must abort the download"),
                Err(e) => e,
            };
            assert!(
                err.to_string().contains("before start"),
                "pre-cancelled download must bail at the pre-start check, got: {err}"
            );
            assert!(
                !count_file.exists(),
                "a pre-cancelled download must never spawn yt-dlp"
            );
        });
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // intentionally a compile-time contract guard
    fn settle_window_still_covers_key_autorepeat() {
        // X11/Wayland key autorepeat cadence is ~30-40ms. The settle window
        // must stay above the worst-case repeat gap, or a held key leaks a
        // spawn per press and the burst-coalescing purpose (and the throttle
        // protection it buys) silently degrades.
        assert!(
            RESOLVE_SETTLE_MS > 50,
            "settle window {RESOLVE_SETTLE_MS}ms must stay above the ~40ms key-repeat cadence"
        );
    }

    #[test]
    fn default_client_format_unavailable_falls_back_to_web_music_and_plays() {
        // The client-fallback repro after the primary/fallback flip: the common
        // path runs yt-dlp's default (token-free) clients; if they report
        // "Requested format is not available" (SABR experiment stripping
        // formats, abandoned default), the video is not dead — the client is.
        // The pipeline must retry exactly once through `web_music` + the POT
        // provider, which streams the fixture, instead of skipping the song.
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sem = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cache = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let fakebin = FakePath::new();
            let ffmpeg_log = fakebin.dir.join("ffmpeg.log");
            write_fake_bin(&fakebin.dir, "ffmpeg", &fake_ffmpeg_body(&ffmpeg_log));
            let fixture = alac_fixture_path();
            // The web_music fallback attempt (recognized by its player_client
            // arg) streams the fixture; the default-clients attempt dies with
            // the format-not-available refusal.
            write_fake_bin(
                &fakebin.dir,
                "yt-dlp",
                &format!(
                    "{}case \" $* \" in\n\
                     *web_music*) cat '{}'; exit 0 ;;\n\
                     esac\n\
                     echo 'ERROR: [youtube] xyz: Requested format is not available' >&2\n\
                     exit 1\n",
                    YTDLP_FAKE_HEAD,
                    fixture.display(),
                ),
            );

            let video_id = format!("fmtul-e2e-fallback-{}", std::process::id());
            let cfg = DownloadConfig {
                yt_dlp_command: "yt-dlp".to_string(),
                video_id,
                pot_provider: Some(resolve::PotProvider {
                    plugin_dir: fakebin.dir.clone(),
                    cli: fakebin.dir.join("bgutil-pot"),
                }),
                cookie_path: None,
                cookie_header: None,
                js_runtime: None,
                cancel_token: tokio_util::sync::CancellationToken::new(),
            };

            let mut decoder = download_and_decode(cfg)
                .await
                .expect("a default-client format refusal must fall back to web_music and play");
            assert!(
                time_to_first_frame(&mut decoder, 1024).is_some(),
                "the web_music fallback download must produce audio frames"
            );
            assert_eq!(
                ffmpeg_relay_invocations(&ffmpeg_log),
                2,
                "exactly one fallback attempt: default clients (rejected) + web_music (streamed) \
                 = two pipe:0 spawns"
            );
        });
    }

    #[test]
    fn format_unavailable_falls_back_only_once_then_bails() {
        // Boundedness: when the fallback client ALSO refuses the format, the
        // song bails as a generic transient failure after exactly one fallback
        // — no endless client-toggling loop, no extra retries.
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sem = SEMAPHORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cache = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let fakebin = FakePath::new();
            let ffmpeg_log = fakebin.dir.join("ffmpeg.log");
            write_fake_bin(&fakebin.dir, "ffmpeg", &fake_ffmpeg_body(&ffmpeg_log));
            write_fake_bin(
                &fakebin.dir,
                "yt-dlp",
                &format!(
                    "{}echo 'ERROR: [youtube] xyz: Requested format is not available' >&2\nexit 1\n",
                    YTDLP_FAKE_HEAD
                ),
            );

            let video_id = format!("fmtul-e2e-once-{}", std::process::id());
            let cfg = DownloadConfig {
                yt_dlp_command: "yt-dlp".to_string(),
                video_id,
                pot_provider: Some(resolve::PotProvider {
                    plugin_dir: fakebin.dir.clone(),
                    cli: fakebin.dir.join("bgutil-pot"),
                }),
                cookie_path: None,
                cookie_header: None,
                js_runtime: None,
                cancel_token: tokio_util::sync::CancellationToken::new(),
            };

            let err = match download_and_decode(cfg).await {
                Ok(_) => panic!(
                    "a web_music fallback that also refuses the format must not play"
                ),
                Err(e) => e,
            };
            assert!(
                err.to_string().starts_with("format not available"),
                "a doubly format-unavailable song must bail as a generic transient \
                 failure, got: {err}"
            );
            assert_eq!(
                ffmpeg_relay_invocations(&ffmpeg_log),
                2,
                "exactly one fallback attempt (default clients + web_music) then bail; \
                 no third attempt"
            );
        });
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
        // Spawns ffmpeg by name, so it must not run concurrently with the E2E
        // tests that inject a fake ffmpeg onto PATH (serialized by the lock).
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        // Spawns yt-dlp by name, so it must not run concurrently with the E2E
        // tests that inject a fake yt-dlp onto PATH (serialized by the lock).
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        // Spawns ffmpeg/yt-dlp by name, so it must not run concurrently with
        // the E2E tests that inject fakes onto PATH (serialized by the lock).
        let _pipe = PIPELINE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
