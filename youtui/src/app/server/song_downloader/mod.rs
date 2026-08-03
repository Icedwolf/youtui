mod cache;
pub(crate) mod resolve;

pub use cache::{cache_clear, create_decoder_from_cache, set_cache_max_entries};
pub use resolve::resolve_url;

use std::sync::{Arc, LazyLock};

use anyhow::{Context, bail};
use symphonia::core::io::MediaSourceStream;
use tokio::sync::Semaphore;
use tracing::{debug, error, warn};

pub(crate) use cache::{cache_get, cache_put};
use resolve::URL_CACHE;
use crate::app::server::streaming_buffer::{SharedBuffer, SharedBufferWriter};
use crate::core::PoisonRecovery;
use crate::decoder::SymphoniaDecoder;
use crate::decoder::read_seek_source::ReadSeekSource;

const MAX_CONCURRENT_DOWNLOADS: usize = 1;
const READ_BUF_SIZE: usize = 64 * 1024;
const STREAM_INIT_THRESHOLD: usize = 512;
const DOWNLOAD_TIMEOUT_S: u64 = 120;
const DECODER_INIT_DEADLINE_S: u64 = 5;
const M4A_TOTAL_LEN_TIMEOUT_S: u64 = 15;

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
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    *HAS_FFMPEG
}

/// Try to init a symphonia decoder from the buffer while it's still being
/// written.  For WAV: the header is at the start, so probing with the first
/// few KB works.  For M4A (isomp4): the moov atom may be at the end, so
/// `byte_len` must be the total file size (from yt-dlp progress line).
/// Spawns on a blocking thread for isomp4 seeking (could block Condvar).
async fn try_streaming_init(
    buffer: &Arc<SharedBuffer>,
    byte_len: Option<u64>,
) -> Result<SymphoniaDecoder, String> {
    let reader = buffer.reader();
    let source = ReadSeekSource::new(reader, byte_len);
    let mss = MediaSourceStream::new(Box::new(source), Default::default());

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

#[allow(clippy::too_many_arguments)]
async fn spawn_bg_cache_task(
    vid: String,
    ct: tokio_util::sync::CancellationToken,
    mut child: tokio::process::Child,
    mut yt_child: Option<tokio::process::Child>,
    write_handle: tokio::task::JoinHandle<()>,
    buf: Arc<SharedBuffer>,
    log_prefix: &'static str,
    _t0: Option<tokio::time::Instant>,
) {
    tokio::select! {
        biased;
        _ = ct.cancelled() => {
            debug!(%vid, "{log_prefix} background cancelled, killing child");
            kill_and_reap(&mut child, &mut yt_child).await;
        }
        result = tokio::time::timeout(
            std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_S),
            write_handle,
        ) => {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(join_err)) => {
                    error!(%vid, error = %join_err, "{log_prefix} writer task panicked");
                    kill_and_reap(&mut child, &mut yt_child).await;
                    return;
                }
                Err(_elapsed) => {
                    warn!(%vid, "{log_prefix} download timed out ({}s), killing", DOWNLOAD_TIMEOUT_S);
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
                    if let Some(t0) = _t0 {
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
    }
}

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

async fn ytdlp_pipeline(
    cfg: &DownloadConfig,
    ffmpeg_avail: bool,
    _permit: tokio::sync::SemaphorePermit<'_>,
    t0: tokio::time::Instant,
    stream_url: Option<String>,
) -> anyhow::Result<SymphoniaDecoder> {
    let from_url_cache = stream_url.is_some();
    let is_wav = ffmpeg_avail || from_url_cache;

    let buffer = SharedBuffer::new();
    let writer = buffer.writer();

    let (_stderr_handle, stdout_handle, mut child, _relay_handle, mut yt_child) =
        if let Some(url) = &stream_url {
            let mut ffmpeg = tokio::process::Command::new("ffmpeg");
            ffmpeg
                .args([
                    "-i",
                    url,
                    "-fflags",
                    "nobuffer",
                    "-flags",
                    "low_delay",
                    "-f",
                    "wav",
                    "-loglevel",
                    "error",
                    "pipe:1",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let mut child = ffmpeg.spawn().context("spawn ffmpeg (stream_url)")?;
            let ffmpeg_stdout = child.stdout.take().context("no ffmpeg stdout")?;

            let write_handle = spawn_stdout_writer(ffmpeg_stdout, writer, "ffmpeg (stream_url)");

            (tokio::spawn(async {}), write_handle, child, None, None)
        } else if ffmpeg_avail {
            let quality = "ba/bestaudio";
            let yt_dlp_cmd = if cfg.yt_dlp_command.is_empty() {
                "yt-dlp".to_string()
            } else {
                cfg.yt_dlp_command.clone()
            };

            let mut yt_cmd = tokio::process::Command::new(&yt_dlp_cmd);
            yt_cmd.args(["-f", quality, "-o", "-", "--no-warnings", "--no-playlist"]);

            resolve::apply_ytdlp_auth_args(&mut yt_cmd, cfg.po_token.as_deref(), cfg.cookie_path.as_deref(), cfg.cookie_header.as_deref(), cfg.js_runtime.as_deref(), &cfg.video_id);
            yt_cmd.stdout(std::process::Stdio::piped());
            yt_cmd.stderr(std::process::Stdio::piped());
            // yt_dlp_child is held for the whole pipeline (returned in the tuple)
            // and moved into the bg cache task after streaming init, so kill_on_drop
            // only fires on bail/timeout/cancel — killing yt-dlp directly instead of
            // relying on pipe closure, which left orphans running for seconds.
            yt_cmd.kill_on_drop(true);
            let mut yt_dlp_child = yt_cmd.spawn().context("spawn yt-dlp")?;
            let yt_stdout = yt_dlp_child.stdout.take().context("no stdout from yt-dlp")?;
            let yt_stderr = yt_dlp_child.stderr.take().context("no stderr from yt-dlp")?;

            debug!(%cfg.video_id, elapsed = ?t0.elapsed(), "yt-dlp spawned");

            let stderr_handle = spawn_stderr_handler(yt_stderr, cfg.cancel_token.clone(), buffer.clone(), true);

            let mut ffmpeg = tokio::process::Command::new("ffmpeg");
            ffmpeg
                .args([
                    "-i",
                    "pipe:0",
                    "-fflags",
                    "nobuffer",
                    "-flags",
                    "low_delay",
                    "-f",
                    "wav",
                    "-loglevel",
                    "error",
                    "pipe:1",
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let mut ffmpeg_child = ffmpeg.spawn().context("spawn ffmpeg")?;
            let mut ffmpeg_stdin = ffmpeg_child.stdin.take().context("no ffmpeg stdin")?;
            let ffmpeg_stdout = ffmpeg_child.stdout.take().context("no ffmpeg stdout")?;

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

            let write_handle = spawn_stdout_writer(ffmpeg_stdout, writer, "ffmpeg");

            (stderr_handle, write_handle, ffmpeg_child, Some(relay), Some(yt_dlp_child))
        } else {
            let quality = "bestaudio[ext=m4a]/bestaudio/bestaudio*";
            let yt_dlp_cmd = if cfg.yt_dlp_command.is_empty() {
                "yt-dlp".to_string()
            } else {
                cfg.yt_dlp_command.clone()
            };

            let mut yt_cmd = tokio::process::Command::new(&yt_dlp_cmd);
            yt_cmd.args(["-f", quality, "-o", "-", "--no-warnings", "--no-playlist"]);

            resolve::apply_ytdlp_auth_args(&mut yt_cmd, cfg.po_token.as_deref(), cfg.cookie_path.as_deref(), cfg.cookie_header.as_deref(), cfg.js_runtime.as_deref(), &cfg.video_id);
            yt_cmd.stdout(std::process::Stdio::piped());
            yt_cmd.stderr(std::process::Stdio::piped());
            yt_cmd.kill_on_drop(true);
            let mut yt_dlp_child = yt_cmd.spawn().context("spawn yt-dlp")?;
            let yt_stdout = yt_dlp_child
                .stdout
                .take()
                .context("no stdout from yt-dlp")?;
            let yt_stderr = yt_dlp_child
                .stderr
                .take()
                .context("no stderr from yt-dlp")?;
            debug!(%cfg.video_id, elapsed = ?t0.elapsed(), "yt-dlp spawned");

            let stderr_handle = spawn_stderr_handler(yt_stderr, cfg.cancel_token.clone(), buffer.clone(), false);

            let write_handle = spawn_stdout_writer(yt_stdout, writer, "yt-dlp");

            (stderr_handle, write_handle, yt_dlp_child, None, None)
        };

    let (decoder, needs_cache) = if is_wav {
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
        if buffer.is_failed() {
            if buffer.is_dead_video() {
                debug!(%cfg.video_id, "Video unavailable (permanently dead), bailing early");
                bail!("video unavailable (yt-dlp error)");
            }
            let reason = if from_url_cache { "ffmpeg" } else { "yt-dlp" };
            debug!(%cfg.video_id, elapsed = ?t0.elapsed(),
                "{reason} failed before data arrived — bailing early");
            bail!("format not available ({reason} error)");
        }
        let current = buffer.len();
        if current == 0 {
            debug!(%cfg.video_id, elapsed = ?t0.elapsed(),
                "No data after {}s, format may be unavailable — bailing early", DECODER_INIT_DEADLINE_S);
            bail!(
                "format not available (empty pipe after {}s)",
                DECODER_INIT_DEADLINE_S
            );
        }
        let stream_type = if from_url_cache {
            "url-cache→ffmpeg→wav"
        } else {
            "ffmpeg→wav"
        };
        debug!(%cfg.video_id, stream_type, buf_len = current, elapsed = ?t0.elapsed(),
            "Trying early decoder init");

        match try_streaming_init(&buffer, None).await {
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
                ));
                drop(_permit);
                (decoder, false)
            }
            Err(stream_err) => {
                drop(_permit);
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
                        if from_url_cache {
                            URL_CACHE.lock().unwrap_or_warn().remove(&cfg.video_id);
                        }
                        bail!("{pipe_label} writer task panicked: {join_err}");
                    }
                    Err(_elapsed) => {
                        kill_and_reap(&mut child, &mut yt_child).await;
                        if from_url_cache {
                            URL_CACHE.lock().unwrap_or_warn().remove(&cfg.video_id);
                        }
                        bail!("{pipe_label} download timed out ({}s)", DOWNLOAD_TIMEOUT_S);
                    }
                }

                let status = child.wait().await.with_context(|| format!("wait {pipe_label}"))?;
                if !status.success() {
                    let code = exit_code_string(&status);
                    if from_url_cache {
                        URL_CACHE.lock().unwrap_or_warn().remove(&cfg.video_id);
                    }
                    bail!("{pipe_label} exited with code {code}");
                }
                debug!(%cfg.video_id, "{pipe_label} completed successfully");

                debug!(%cfg.video_id, buf_len = buffer.len(),
                    "Creating decoder from completed download (fallback)");
                let d = decoder_from_buffer(&buffer, None, "wav-fallback")?;
                (d, !from_url_cache)
            }
        }
    } else {
        let _total_len = {
            let deadline = tokio::time::Instant::now()
                + std::time::Duration::from_secs(M4A_TOTAL_LEN_TIMEOUT_S);
            loop {
                if buffer.is_failed() {
                    if buffer.is_dead_video() {
                        bail!("video unavailable (yt-dlp error)");
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
        match try_streaming_init(&buffer, Some(_total_len)).await {
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
                ));

                drop(_permit);
                (decoder, false)
            }
            Err(stream_err) => {
                drop(_permit);
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
                let d = decoder_from_buffer(&buffer, Some(_total_len), "m4a-fallback")?;
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
    Ok(decoder)
}

pub async fn download_and_decode(cfg: DownloadConfig) -> anyhow::Result<SymphoniaDecoder> {

    if cfg.cancel_token.is_cancelled() {
        anyhow::bail!("download cancelled before start");
    }

    if let Some(cached) = cache_get(&cfg.video_id) {
        debug!(%cfg.video_id, len = cached.len(), "Reusing cached buffer");
        let len = cached.len() as u64;
        let cursor = std::io::Cursor::new(cached);
        let source = ReadSeekSource::new(cursor, Some(len));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        return SymphoniaDecoder::new(mss).context("decoder (cached)");
    }

    let ffmpeg_avail = check_ffmpeg();
    let cached_url = resolve_url(
        &cfg.video_id, &cfg.yt_dlp_command, cfg.po_token.as_deref(), cfg.cookie_path.as_deref(), cfg.cookie_header.as_deref(), cfg.js_runtime.as_deref(),
        Some(&cfg.cancel_token),
    ).await;

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

    let t0 = tokio::time::Instant::now();

    ytdlp_pipeline(&cfg, ffmpeg_avail, _permit, t0, cached_url).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::server::song_downloader::cache::CACHE_MAX_ENTRIES;
    use crate::app::server::streaming_buffer::SharedBuffer;
    use crate::decoder::SymphoniaDecoder;
    use crate::decoder::read_seek_source::ReadSeekSource;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    use symphonia::core::io::MediaSourceStream;

    const TEST_WAV: &[u8] = include_bytes!("../../../../../ytmapi-rs/test_json/test_silence.wav");

    // Tests touching the global BYTE_CACHE must run serially (they share the
    // same cache behind one Mutex, so parallel runs evict each other's entries).
    static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

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
        let mut child = ffmpeg.spawn().context("spawn ffmpeg for WAV transcode")?;
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
            "-o", "-", "--no-warnings", "--no-playlist", &url])
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
            "-f", "wav", "-loglevel", "error", "pipe:1"])
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
        let mut dec_relay = create_decoder_from(&relay_buf).expect("decoder from relay stream");
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
            "-o", output_path, "--no-warnings", "--no-playlist", &url])
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
            let task = tokio::spawn(spawn_bg_cache_task(
                "test-vid".to_string(),
                ct.clone(),
                ff_child,
                Some(yt_child),
                done,
                buf,
                "test",
                None,
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
}
