// LIFECYCLE SAFETY INVARIANT (do not regress):
// Background download tasks (`_handle = tokio::spawn(...)`) are deliberately
// fire-and-forget.  They capture the child process (ffmpeg or yt-dlp), which
// has `kill_on_drop(true)`.  Dropping a JoinHandle does NOT cancel the task —
// the child stays alive, the buffer fills, the decoder reads.
//
// NEVER store these handles for later cancellation inside download_and_decode.
// The prefetch mechanism (download_upcoming_from_id) calls this function while
// the CURRENT song is still playing.  Cancelling the previous song's background
// task drops its `Child` with `kill_on_drop(true)`, which KILLS ffmpeg mid-
// stream — starving the current song's SharedBuffer of data and causing it to
// EOF after ~1.5 seconds of playback.  Previous background tasks complete
// naturally: ffmpeg finishes -> writer.finish() -> finalize() -> cache_put().
//
// The only legitimate cancellation path is `cancel_all_downloads` at the
// playlist layer, which cancels all CancellationTokens and clears the song
// queue.  Each download task checks its token at key points and bails.  No
// individual download task is ever aborted externally — they self-terminate
// on the next cancellation check.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use anyhow::{Context, bail};
use symphonia::core::io::MediaSourceStream;
use tokio::sync::Semaphore;
use tracing::{debug, error, warn};

use crate::app::server::streaming_buffer::SharedBuffer;
use crate::core::PoisonRecovery;
use crate::decoder::SymphoniaDecoder;
use crate::decoder::read_seek_source::ReadSeekSource;

const MAX_CONCURRENT_DOWNLOADS: usize = 1;
static CACHE_MAX_ENTRIES: AtomicUsize = AtomicUsize::new(1);

pub fn set_cache_max_entries(val: usize) {
    CACHE_MAX_ENTRIES.store(val.clamp(0, 100), Ordering::Release);
}

fn exit_code_string(status: &std::process::ExitStatus) -> String {
    status.code().map_or("unknown".into(), |c| c.to_string())
}
const READ_BUF_SIZE: usize = 64 * 1024;
const STREAM_INIT_THRESHOLD: usize = 512;
const DOWNLOAD_TIMEOUT_S: u64 = 120;
const DECODER_INIT_DEADLINE_S: u64 = 5;
const M4A_TOTAL_LEN_TIMEOUT_S: u64 = 15;

static DOWNLOAD_SEMAPHORE: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(MAX_CONCURRENT_DOWNLOADS));

// Bounded cache of raw audio bytes for recently completed downloads.
// Keyed by video_id, evicts oldest entry when full.
// Enables instant re-play of recently played songs without re-download.
// Stores Arc<[u8]> so cache hits avoid cloning 50MB+ buffers —
// the Arc<[u8]> refcount bump is ~8 bytes vs a full Vec clone.
struct AudioCache {
    data: HashMap<String, Arc<[u8]>>,
    order: VecDeque<String>,
}

impl AudioCache {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn put(&mut self, key: String, data: Arc<[u8]>) {
        let max = CACHE_MAX_ENTRIES.load(Ordering::Acquire);
        if max > 0
            && self.data.len() >= max
            && let Some(old) = self.order.pop_front()
        {
            self.data.remove(&old);
        }
        if max > 0 {
            self.data.insert(key.clone(), data);
            self.order.push_back(key);
        }
    }

    fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        let hit = self.data.get(key).cloned()?;
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
            self.order.push_back(key.to_string());
        }
        Some(hit)
    }
}

static BYTE_CACHE: LazyLock<Mutex<AudioCache>> = LazyLock::new(|| Mutex::new(AudioCache::new()));

fn cache_put(key: String, data: Arc<[u8]>) {
    BYTE_CACHE.lock().unwrap_or_warn().put(key, data);
}

fn cache_get(key: &str) -> Option<Arc<[u8]>> {
    BYTE_CACHE.lock().unwrap_or_warn().get(key)
}

pub fn cache_clear() {
    let mut cache = BYTE_CACHE.lock().unwrap_or_warn();
    cache.data.clear();
    cache.order.clear();
}

/// Create a decoder from cached audio data. Returns None if not cached.
pub fn create_decoder_from_cache(video_id: &str) -> Option<SymphoniaDecoder> {
    let cached = cache_get(video_id)?;
    let len = cached.len() as u64;
    let cursor = std::io::Cursor::new(cached);
    let source = ReadSeekSource::new(cursor, Some(len));
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    SymphoniaDecoder::new(mss).ok()
}

/// Cache of pre-resolved stream URLs (video_id → (url, timestamp)).
/// URLs expire after ~6 hours (YouTube signature expiry), but for a
/// single session they remain valid. Filled by background `yt-dlp --print url`.
const URL_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(6 * 3600);
static URL_CACHE: LazyLock<Mutex<HashMap<String, (String, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn url_cache_get(video_id: &str) -> Option<String> {
    let mut cache = URL_CACHE.lock().unwrap_or_warn();
    if let Some((url, ts)) = cache.get(video_id) {
        if ts.elapsed() < URL_CACHE_TTL {
            return Some(url.clone());
        }
        // Expired — remove
        cache.remove(video_id);
    }
    None
}

fn url_cache_put(video_id: String, url: String) {
    let mut cache = URL_CACHE.lock().unwrap_or_warn();
    cache.insert(video_id, (url, Instant::now()));
}

/// Resolve a stream URL for the given YouTube Music video_id using
/// `yt-dlp --print url`.  The resolved URL is cached so that the next
/// time the same song plays, we can skip the 2+ second yt-dlp negotiation
/// and stream directly via ffmpeg.
/// Checks `cancel_token` to abort mid-resolution when the task is cancelled.
pub async fn resolve_url(
    video_id: &str,
    yt_dlp_cmd: &str,
    po_token: Option<&str>,
    cancel_token: Option<&tokio_util::sync::CancellationToken>,
) -> Option<String> {
    if let Some(url) = url_cache_get(video_id) {
        return Some(url);
    }

    if let Some(ct) = cancel_token && ct.is_cancelled() {
        return None;
    }

    let cmd = if yt_dlp_cmd.is_empty() {
        "yt-dlp"
    } else {
        yt_dlp_cmd
    };
    let url = format!("https://music.youtube.com/watch?v={video_id}");
    let skip = "hls,translated_subs";
    let extractor_args = match po_token {
        Some(pt) => format!("youtube:po_token={pt};skip={skip}"),
        None => format!("youtube:skip={skip}"),
    };

    let mut resolve_cmd = tokio::process::Command::new(cmd);
    resolve_cmd
        .args([
            "--print",
            "url",
            "-f",
            "bestaudio[ext=webm]/bestaudio[ext=m4a]/bestaudio/bestaudio*",
            "--no-warnings",
            "--no-playlist",
            "--extractor-args",
            &extractor_args,
            &url,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    resolve_cmd.kill_on_drop(true);
    let mut child = resolve_cmd.spawn().ok()?;
    let mut stdout_pipe = child.stdout.take()?;

    let output_fut = async move {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        stdout_pipe.read_to_end(&mut buf).await?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>(std::process::Output {
            status,
            stdout: buf,
            stderr: Vec::new(),
        })
    };

    let output = if let Some(ct) = cancel_token {
        tokio::select! {
            biased;
            _ = ct.cancelled() => return None,
            output = output_fut => output.ok()?,
        }
    } else {
        output_fut.await.ok()?
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let url = stdout.lines().next()?.trim().to_string();
    if url.is_empty() || !url.starts_with("http") {
        return None;
    }

    url_cache_put(video_id.to_string(), url.clone());
    Some(url)
}

/// yt-dlp pipeline: spawn yt-dlp (+ optional ffmpeg relay) or use a
/// pre-resolved stream URL directly with ffmpeg.  Tries streaming decoder
/// init, falls back to full download on failure.  Returns a decoder and
/// caches the completed buffer (unless `stream_url` is Some — URL might be
/// stale, so don't cache the full download).
///
/// `stream_url` = pre-resolved stream URL (URL-cache hit).  When provided,
/// skips yt-dlp entirely and feeds the URL directly to ffmpeg.  When None,
/// spawns yt-dlp (+ optional ffmpeg relay if available, or direct M4A).
#[allow(clippy::too_many_arguments)]
async fn ytdlp_pipeline(
    yt_dlp_command: &str,
    video_id: &str,
    po_token: Option<&str>,
    cookie_path: Option<PathBuf>,
    js_runtime: Option<&str>,
    ffmpeg_avail: bool,
    cancel_token: tokio_util::sync::CancellationToken,
    _permit: tokio::sync::SemaphorePermit<'_>,
    t0: tokio::time::Instant,
    stream_url: Option<String>,
) -> anyhow::Result<SymphoniaDecoder> {
    let from_url_cache = stream_url.is_some();
    let is_wav = ffmpeg_avail || from_url_cache;

    let buffer = SharedBuffer::new();
    let mut writer = buffer.writer();

    // ── Data source ──────────────────────────────────────────────────
    let (_stderr_handle, stdout_handle, mut child, _relay_handle) =
        if let Some(url) = &stream_url {
            // URL-cache hit: ffmpeg reads directly from pre-resolved URL.
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

            let write_handle = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut rdr = tokio::io::BufReader::new(ffmpeg_stdout);
                let mut buf = vec![0u8; READ_BUF_SIZE];
                loop {
                    match rdr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => writer.write(&buf[..n]),
                        Err(e) => {
                            warn!(error = %e, "ffmpeg stdout read error (stream_url), failing buffer");
                            writer.fail();
                            return;
                        }
                    }
                }
                writer.finish();
            });

            (tokio::spawn(async {}), write_handle, child, None)
        } else if ffmpeg_avail {
            // yt-dlp → relay → ffmpeg pipe:0
            let quality = "ba/bestaudio";
            let yt_dlp_cmd = if yt_dlp_command.is_empty() {
                "yt-dlp".to_string()
            } else {
                yt_dlp_command.to_string()
            };

            let mut yt_cmd = tokio::process::Command::new(&yt_dlp_cmd);
            yt_cmd.args(["-f", quality, "-o", "-", "--no-warnings", "--no-playlist"]);

            let skip = "hls,translated_subs";
            let extractor_args = match po_token {
                Some(pt) => format!("youtube:po_token={pt};skip={skip}"),
                None => format!("youtube:skip={skip}"),
            };
            yt_cmd.arg("--extractor-args");
            yt_cmd.arg(&extractor_args);
            if let Some(ref cp) = cookie_path {
                yt_cmd.arg("--cookies");
                yt_cmd.arg(cp.to_string_lossy().into_owned());
            }
            if let Some(js) = js_runtime {
                yt_cmd.arg("--js-runtimes");
                yt_cmd.arg(js);
            }

            yt_cmd.arg(format!("https://music.youtube.com/watch?v={video_id}"));
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
            debug!(%video_id, elapsed = ?t0.elapsed(), "yt-dlp spawned");

            let stderr_ct = cancel_token.clone();
            let buf_for_stderr = buffer.clone();
            let stderr_handle = tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let reader = tokio::io::BufReader::new(yt_stderr);
                let mut lines = reader.lines();
                loop {
                    if stderr_ct.is_cancelled() {
                        debug!("yt-dlp stderr handler cancelled (relay)");
                        return;
                    }
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if let Some(bytes) = parse_total_size(&line) {
                                debug!(total_bytes = bytes, "Parsed total size from yt-dlp progress");
                                buf_for_stderr.set_total_len(bytes);
                            } else if line.contains("ERROR") {
                                warn!(stderr_line = %line.trim(), "yt-dlp stderr (error), failing buffer");
                                buf_for_stderr.fail();
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
            });

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

            let write_handle = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut rdr = tokio::io::BufReader::new(ffmpeg_stdout);
                let mut buf = vec![0u8; READ_BUF_SIZE];
                loop {
                    match rdr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => writer.write(&buf[..n]),
                        Err(e) => {
                            warn!(error = %e, "ffmpeg stdout read error, failing buffer");
                            writer.fail();
                            return;
                        }
                    }
                }
                debug!("ffmpeg stream ended, finishing buffer");
                writer.finish();
            });

            (stderr_handle, write_handle, ffmpeg_child, Some(relay))
        } else {
            // yt-dlp direct → M4A
            let quality = "bestaudio[ext=m4a]/bestaudio/bestaudio*";
            let yt_dlp_cmd = if yt_dlp_command.is_empty() {
                "yt-dlp".to_string()
            } else {
                yt_dlp_command.to_string()
            };

            let mut yt_cmd = tokio::process::Command::new(&yt_dlp_cmd);
            yt_cmd.args(["-f", quality, "-o", "-", "--no-warnings", "--no-playlist"]);

            let skip = "hls,translated_subs";
            let extractor_args = match po_token {
                Some(pt) => format!("youtube:po_token={pt};skip={skip}"),
                None => format!("youtube:skip={skip}"),
            };
            yt_cmd.arg("--extractor-args");
            yt_cmd.arg(&extractor_args);
            if let Some(ref cp) = cookie_path {
                yt_cmd.arg("--cookies");
                yt_cmd.arg(cp.to_string_lossy().into_owned());
            }
            if let Some(js) = js_runtime {
                yt_cmd.arg("--js-runtimes");
                yt_cmd.arg(js);
            }

            yt_cmd.arg(format!("https://music.youtube.com/watch?v={video_id}"));
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
            debug!(%video_id, elapsed = ?t0.elapsed(), "yt-dlp spawned");

            let stderr_ct = cancel_token.clone();
            let buf_for_stderr = buffer.clone();
            let stderr_handle = tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let reader = tokio::io::BufReader::new(yt_stderr);
                let mut lines = reader.lines();
                loop {
                    if stderr_ct.is_cancelled() {
                        return;
                    }
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if let Some(bytes) = parse_total_size(&line) {
                                debug!(total_bytes = bytes, "Parsed total size from yt-dlp progress");
                                buf_for_stderr.set_total_len(bytes);
                            } else if line.contains("ERROR") {
                                warn!(stderr_line = %line.trim(), "yt-dlp stderr (error), failing buffer");
                                buf_for_stderr.fail();
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
            });

            let write_handle = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut rdr = tokio::io::BufReader::new(yt_stdout);
                let mut buf = vec![0u8; READ_BUF_SIZE];
                loop {
                    match rdr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => writer.write(&buf[..n]),
                        Err(e) => {
                            warn!(error = %e, "yt-dlp stdout read error, failing buffer");
                            writer.fail();
                            return;
                        }
                    }
                }
                debug!("yt-dlp stdout stream ended, finishing buffer");
                writer.finish();
            });

            (stderr_handle, write_handle, yt_dlp_child, None)
        };

    // ── Streaming init ──────────────────────────────────────────────
    let (decoder, needs_cache) = if is_wav {
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(DECODER_INIT_DEADLINE_S);
        while buffer.len() < STREAM_INIT_THRESHOLD
            && tokio::time::Instant::now() < deadline
            && !buffer.is_failed()
            && !cancel_token.is_cancelled()
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        if cancel_token.is_cancelled() {
            bail!("download cancelled during buffering");
        }
        if buffer.is_failed() {
            let reason = if from_url_cache { "ffmpeg" } else { "yt-dlp" };
            debug!(%video_id, elapsed = ?t0.elapsed(),
                "{reason} failed before data arrived — bailing early");
            bail!("format not available ({reason} error)");
        }
        let current = buffer.len();
        if current == 0 {
            debug!(%video_id, elapsed = ?t0.elapsed(),
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
        debug!(%video_id, stream_type, buf_len = current, elapsed = ?t0.elapsed(),
            "Trying early decoder init");

        match try_streaming_init(&buffer, None).await {
            Ok(decoder) => {
                debug!(%video_id, buf_len = buffer.len(), elapsed = ?t0.elapsed(),
                    "Streaming decoder init succeeded");
                let log_prefix = if from_url_cache { "ffmpeg (stream_url)" } else { "ffmpeg" };
                let _cache_task = tokio::spawn(spawn_bg_cache_task(
                    video_id.to_string(),
                    cancel_token.clone(),
                    child,
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
                debug!(%video_id, error = %stream_err,
                    "Streaming decoder init failed, waiting for {} stream to complete",
                    if from_url_cache { "ffmpeg" } else { "ffmpeg relay" });
                let pipe_label = if from_url_cache { "ffmpeg (stream_url)" } else { "ffmpeg" };
                match tokio::time::timeout(
                    std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_S),
                    stdout_handle,
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(join_err)) => {
                        if from_url_cache {
                            URL_CACHE.lock().unwrap_or_warn().remove(video_id);
                        }
                        bail!("{pipe_label} writer task panicked: {join_err}");
                    }
                    Err(_elapsed) => {
                        let _ = child.start_kill();
                        if from_url_cache {
                            URL_CACHE.lock().unwrap_or_warn().remove(video_id);
                        }
                        bail!("{pipe_label} download timed out ({}s)", DOWNLOAD_TIMEOUT_S);
                    }
                }

                let status = child.wait().await.with_context(|| format!("wait {pipe_label}"))?;
                if !status.success() {
                    let code = exit_code_string(&status);
                    if from_url_cache {
                        URL_CACHE.lock().unwrap_or_warn().remove(video_id);
                    }
                    bail!("{pipe_label} exited with code {code}");
                }
                debug!(%video_id, "{pipe_label} completed successfully");

                debug!(%video_id, buf_len = buffer.len(),
                    "Creating decoder from completed download (fallback)");
                let reader = buffer.reader();
                let source = ReadSeekSource::new(reader, None);
                let mss = MediaSourceStream::new(Box::new(source), Default::default());
                let d = SymphoniaDecoder::new(mss).context("decoder (fallback)")?;
                (d, !from_url_cache)
            }
        }
    } else {
        let _total_len = {
            let deadline = tokio::time::Instant::now()
                + std::time::Duration::from_secs(M4A_TOTAL_LEN_TIMEOUT_S);
            loop {
                if buffer.is_failed() {
                    break None;
                }
                if cancel_token.is_cancelled() {
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

        if cancel_token.is_cancelled() {
            bail!("download cancelled during M4A total_len wait");
        }

        debug!(
            %video_id,
            stream_type = "direct m4a",
            buf_len = buffer.len(),
            elapsed = ?t0.elapsed(),
            "Trying early decoder init (spawn_blocking + {}s timeout)", DECODER_INIT_DEADLINE_S
        );
        match try_streaming_init(&buffer, Some(_total_len)).await {
            Ok(decoder) => {
                debug!(%video_id, buf_len = buffer.len(), elapsed = ?t0.elapsed(),
                "Streaming decoder init succeeded (M4A)");

                let pipe_name: &str = "yt-dlp";
                let _handle = tokio::spawn(spawn_bg_cache_task(
                    video_id.to_string(),
                    cancel_token.clone(),
                    child,
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
                debug!(%video_id, error = %stream_err,
                "Streaming decoder init failed, waiting for yt-dlp stream to complete");
                match tokio::time::timeout(
                    std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_S),
                    stdout_handle,
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(join_err)) => {
                        bail!("yt-dlp writer task panicked: {join_err}");
                    }
                    Err(_elapsed) => {
                        let _ = child.start_kill();
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
                debug!(%video_id, "yt-dlp completed successfully");

                debug!(%video_id, buf_len = buffer.len(),
                "Creating decoder from completed download (fallback)");
                let reader = buffer.reader();
                let source = ReadSeekSource::new(reader, Some(_total_len));
                let mss = MediaSourceStream::new(Box::new(source), Default::default());
                let d = SymphoniaDecoder::new(mss).context("decoder (fallback)")?;
                (d, true)
            }
        }
    };

    if needs_cache {
        let data = buffer.finalize();
        debug!(%video_id, len = data.len(), "Caching completed download (fallback)");
        cache_put(video_id.to_string(), data);
    }

    debug!(%video_id, elapsed = ?t0.elapsed(), "download_and_decode returning decoder");
    Ok(decoder)
}

/// Fire-and-forget background task that waits for a download to complete,
/// then stores the buffer in the shared byte cache.  Uses `tokio::select!`
/// with `biased` priority so that cancellation kills the child process
/// immediately (releasing ~42MB of SharedBuffer) rather than waiting for
/// the download timeout.  See module-level lifecycle safety invariant.
async fn spawn_bg_cache_task(
    vid: String,
    ct: tokio_util::sync::CancellationToken,
    mut child: tokio::process::Child,
    write_handle: tokio::task::JoinHandle<()>,
    buf: Arc<SharedBuffer>,
    log_prefix: &'static str,
    _t0: Option<tokio::time::Instant>,
) {
    tokio::select! {
        biased;
        _ = ct.cancelled() => {
            debug!(%vid, "{log_prefix} background cancelled, killing child");
            let _ = child.start_kill();
        }
        result = tokio::time::timeout(
            std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_S),
            write_handle,
        ) => {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(join_err)) => {
                    error!(%vid, error = %join_err, "{log_prefix} writer task panicked");
                    return;
                }
                Err(_elapsed) => {
                    warn!(%vid, "{log_prefix} download timed out ({}s), killing", DOWNLOAD_TIMEOUT_S);
                    let _ = child.start_kill();
                    return;
                }
            }
            let status = child.wait().await;
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

pub async fn download_and_decode(
    yt_dlp_command: &str,
    video_id: &str,
    po_token: Option<&str>,
    cookie_path: Option<PathBuf>,
    js_runtime: Option<&str>,
    cancel_token: tokio_util::sync::CancellationToken,
) -> anyhow::Result<SymphoniaDecoder> {

    if cancel_token.is_cancelled() {
        anyhow::bail!("download cancelled before start");
    }

    // Check cache first — instant replay without re-download.
    if let Some(cached) = cache_get(video_id) {
        debug!(%video_id, len = cached.len(), "Reusing cached buffer");
        let len = cached.len() as u64;
        let cursor = std::io::Cursor::new(cached);
        let source = ReadSeekSource::new(cursor, Some(len));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        return SymphoniaDecoder::new(mss).context("decoder (cached)");
    }

    // ── URL pre-resolution (outside semaphore) ──────────────────────────
    // Resolve the stream URL before acquiring the semaphore so that
    // cancellation (e.g. user skips to another song) does not block on a
    // 2-4s yt-dlp negotiation while holding the semaphore.
    // Once cached, resolve_url returns instantly.
    // Multiple songs can resolve in parallel — no semaphore contention.
    let ffmpeg_avail = check_ffmpeg();
    let cached_url =
        resolve_url(video_id, yt_dlp_command, po_token, Some(&cancel_token)).await;

    if cancel_token.is_cancelled() {
        anyhow::bail!("download cancelled before semaphore");
    }

    let _permit = DOWNLOAD_SEMAPHORE
        .acquire()
        .await
        .context("Semaphore closed")?;

    if cancel_token.is_cancelled() {
        anyhow::bail!("download cancelled after semaphore");
    }

    let t0 = tokio::time::Instant::now();

    ytdlp_pipeline(
        yt_dlp_command,
        video_id,
        po_token,
        cookie_path,
        js_runtime,
        ffmpeg_avail,
        cancel_token,
        _permit,
        t0,
        cached_url,
    )
    .await
}

/// Parse the total download size from a yt-dlp progress line like:
/// `[download]   0.3% of  302.04KiB at  344.87KiB/s ETA 00:00`
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

/// Check whether `ffmpeg` is available on the system.
/// Used to decide between the ffmpeg relay path (WebM → MP3 stream) and
/// the direct download path (M4A, full-download then decode).
/// Result is cached after first call — ffmpeg presence never changes during a session.
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

/// Attempt to create a SymphoniaDecoder from the streaming buffer.
///
/// `byte_len` is passed as-is to ReadSeekSource:
/// - `Some(total_size)` for M4A (isomp4 reader needs it for SeekFrom::End)
/// - `None` for WAV (RIFF reader handles unknown data_chunk size during
///   streaming — no SeekFrom::End needed)
///
/// For WAV (`byte_len = None`), the probe is done inline because the RIFF
/// header is always in the first 78 bytes, guaranteed buffered by the
/// caller's threshold wait.  No blocking thread needed.
///
/// For M4A (`byte_len = Some`), `spawn_blocking` is used because isomp4's
/// `SeekFrom::End` blocks on the SharedBuffer Condvar until the full file
/// arrives — must stay off the async runtime.
async fn try_streaming_init(
    buffer: &Arc<SharedBuffer>,
    byte_len: Option<u64>,
) -> anyhow::Result<SymphoniaDecoder> {
    let buf = buffer.clone();
    let reader = buf.reader();

    if byte_len.is_none() {
        // WAV fast path: RIFF header + fmt chunk are always buffered.
        // Do the probe inline — saves ~100µs spawn_blocking overhead.
        let source = ReadSeekSource::new(reader, None);
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        return SymphoniaDecoder::new(mss).context("decoder (inline wav)");
    }

    // M4A path: isomp4 probe seeks to byte_len from End, which blocks
    // on the SharedBuffer Condvar until full download.  Must use
    // spawn_blocking to keep the async runtime free.
    let handle = tokio::task::spawn_blocking(move || {
        let source = ReadSeekSource::new(reader, byte_len);
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        SymphoniaDecoder::new(mss)
    });

    match tokio::time::timeout(
        std::time::Duration::from_secs(DECODER_INIT_DEADLINE_S),
        handle,
    )
    .await
    {
        Ok(join_result) => match join_result {
            Ok(Ok(decoder)) => Ok(decoder),
            Ok(Err(e)) => Err(e).context("decoder (m4a blocking)"),
            Err(join_err) => Err(join_err).context("blocking task panicked"),
        },
        Err(_elapsed) => {
            bail!("decoder init timed out (isomp4 seek blocked on Condvar)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kib() {
        let line = "[download]   0.3% of  302.04KiB at  344.87KiB/s ETA 00:00";
        assert_eq!(parse_total_size(line), Some((302.04 * 1024.0) as u64));
    }

    #[test]
    fn parse_mib() {
        let line = "[download]   5.0% of    4.06MiB at    2.14MiB/s ETA 00:00";
        assert_eq!(
            parse_total_size(line),
            Some((4.06 * 1024.0 * 1024.0) as u64)
        );
    }

    #[test]
    fn parse_gib() {
        let line = "[download]   1.0% of    1.50GiB at  100.00MiB/s ETA 00:10";
        assert_eq!(
            parse_total_size(line),
            Some((1.50 * 1024.0 * 1024.0 * 1024.0) as u64)
        );
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
        assert_eq!(
            parse_total_size(line),
            Some((2.50 * 1024.0 * 1024.0) as u64)
        );
    }

    #[test]
    fn parse_gb_si_unit() {
        let line = "[download]   1.0% of    1.20GB at   50.00MB/s ETA 00:30";
        assert_eq!(
            parse_total_size(line),
            Some((1.20 * 1024.0 * 1024.0 * 1024.0) as u64)
        );
    }

    #[test]
    fn parse_kb_lowercase() {
        let line = "[download]   5.0% of  100.00kB at  200.00kB/s ETA 00:01";
        assert_eq!(parse_total_size(line), Some((100.0 * 1024.0) as u64));
    }

    // ── Streaming vs Full-Download Benchmarks ──────────────────────────

    use std::sync::Arc;
    use std::time::{Duration, Instant};

    // Embed a real MP3 from the ytmapi-rs test data (~1 MiB, 256 kbps, 44.1 kHz stereo).
    const TEST_MP3: &[u8] = include_bytes!("../../../../ytmapi-rs/test_json/test_upload.mp3");

    /// Spawn a background thread that writes `data` to a SharedBuffer in
    /// 64 KiB chunks with an inter-chunk `chunk_delay`, then finishes.
    /// Returns a JoinHandle — the caller can `join()` to wait for completion.
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

    /// Helper: create a SymphoniaDecoder from a SharedBuffer.
    fn create_decoder_from(buf: &Arc<SharedBuffer>) -> anyhow::Result<SymphoniaDecoder> {
        let total = buf.total_len().unwrap_or(buf.len() as u64);
        let reader = buf.reader();
        let source = ReadSeekSource::new(reader, Some(total));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        SymphoniaDecoder::new(mss).context("decoder")
    }

    /// Consume audio frames from the decoder until at least `target_frames`
    /// have been produced.  Returns the time from call to the first non-zero
    /// frame count (i.e. the first decoded packet).
    fn time_to_first_frame(
        decoder: &mut SymphoniaDecoder,
        target_frames: usize,
    ) -> Option<Duration> {
        let t0 = Instant::now();
        let mut total = 0usize;
        while total < target_frames {
            match decoder.next() {
                Some(_) => {
                    total += 1;
                }
                None => {
                    if total == 0 {
                        return None;
                    }
                    break;
                }
            }
        }
        Some(t0.elapsed())
    }

    #[test]
    fn streaming_creates_decoder_with_full_data_incomplete_writer() {
        // Write ALL data to the buffer but keep the writer alive (simulating
        // download in progress).  The decoder should be creatable because
        // all the bytes are already there, and should produce frames.
        let buf = SharedBuffer::new();
        buf.set_total_len(TEST_MP3.len() as u64);
        let mut keep_alive = buf.writer();
        keep_alive.write(TEST_MP3);

        let mut dec =
            create_decoder_from(&buf).expect("Decoder with all data written but writer alive");
        let ttf = time_to_first_frame(&mut dec, 1024);
        assert!(
            ttf.is_some(),
            "Decoder should produce frames when all data is in the buffer"
        );
        // Keep alive until after assertion so the buffer stays "incomplete".
        drop(keep_alive);
    }

    #[test]
    fn streaming_produces_frames_before_full_write_completes() {
        // Simulate the real download_and_decode flow:
        //  1. Buffer starts EMPTY
        //  2. total_len is set (simulating yt-dlp progress line)
        //  3. Data arrives in 64 KiB chunks (simulating stdout_handle)
        //  4. Decoder is created immediately (before all data arrives)
        //  5. Decoder blocks on Condvar until first chunk arrives
        //  6. First audio frame is produced while download continues
        let chunk_delay = Duration::from_millis(15);

        let buf = SharedBuffer::new();
        buf.set_total_len(TEST_MP3.len() as u64);

        // Background: write ALL data in 64 KiB chunks with delay.
        let handle = spawn_slow_write(&buf, TEST_MP3, chunk_delay);

        // Create decoder immediately — buffer may be empty or have only
        // the first chunk.  Symphonia's probe will block on Condvar
        // until the first bytes arrive, then detect the format.
        let mut dec = create_decoder_from(&buf)
            .expect("Decoder created from streaming buffer (may block briefly)");

        let streaming_ttf = time_to_first_frame(&mut dec, 44100);
        assert!(
            streaming_ttf.is_some(),
            "Streaming decoder must produce frames while download is in progress"
        );

        // Wait for the full write to know how long the *full* path would take.
        handle.join().unwrap();

        let full_write_estimate =
            (TEST_MP3.len().div_ceil(64 * 1024) as u64).saturating_sub(1) * 15;
        println!(
            "streaming: first ~1s of audio in {:?} (full write would take ~{full_write_estimate} ms)",
            streaming_ttf.unwrap(),
        );
        // TTF must be less than the full write time (otherwise there's no
        // benefit to streaming over full-download-then-decode).
        assert!(
            streaming_ttf.unwrap() < Duration::from_millis(full_write_estimate),
            "Streaming TTF {:?} must be < {full_write_estimate} ms (full write time)",
            streaming_ttf.unwrap(),
        );
    }

    #[test]
    fn full_download_requires_write_completion() {
        let buf = SharedBuffer::new();
        buf.set_total_len(TEST_MP3.len() as u64);

        // Full download writes ALL data before decoder is created.
        let mut w = buf.writer();
        w.write(TEST_MP3);
        w.finish();

        let mut dec = create_decoder_from(&buf).expect("Decoder after full download");
        let ttf = time_to_first_frame(&mut dec, 44100);
        assert!(ttf.is_some(), "Full-download decoder should produce frames");
    }

    #[test]
    fn streaming_vs_full_timing_comparison() {
        // Proper benchmark simulating the real yt-dlp pipeline:
        //   Streaming: create decoder immediately (buffer may be empty),
        //              measure TTF as data streams in.
        //   Full:      wait for ALL data to arrive, then create decoder,
        //              measure TTF.
        let chunk_delay = Duration::from_millis(5);
        let num_chunks = TEST_MP3.len().div_ceil(64 * 1024);
        let full_write_ms = (num_chunks as u64).saturating_sub(1) * 5;

        // ── Streaming approach ────────────────────────────────────────
        // Mirror the real download_and_decode: buffer empty, total_len set,
        // data flows in background, decoder created immediately.
        let buf_stream = SharedBuffer::new();
        buf_stream.set_total_len(TEST_MP3.len() as u64);
        let _stream_handle = spawn_slow_write(&buf_stream, TEST_MP3, chunk_delay);

        // Create decoder from potentially-empty buffer, time first frame.
        let t0 = Instant::now();
        let mut dec_stream = create_decoder_from(&buf_stream).unwrap();
        let streaming_ttf = t0.elapsed();
        let _streaming_frames = dec_stream.by_ref().take(1024).count();
        // Keep the decoder alive long enough to consume frames (the actual
        // TTF measurement was taken right after decoder creation above).

        // ── Full-download approach ────────────────────────────────────
        let buf_full = SharedBuffer::new();
        buf_full.set_total_len(TEST_MP3.len() as u64);
        let full_handle = spawn_slow_write(&buf_full, TEST_MP3, chunk_delay);
        full_handle.join().unwrap(); // Wait for full download.

        let t0 = Instant::now();
        let mut dec_full = create_decoder_from(&buf_full).unwrap();
        let full_ttf = t0.elapsed();
        let _full_frames = dec_full.by_ref().take(1024).count();

        // Wait for streaming background write to finish (cleanup).
        _stream_handle.join().unwrap();

        println!(
            "BENCHMARK: streaming TTF = {:?} (decoder created from flowing buffer), \
             full TTF = {:?} (decoder created after {:?} write)",
            streaming_ttf,
            full_ttf,
            Duration::from_millis(full_write_ms),
        );
        // Verify both approaches work.
        assert!(
            streaming_ttf < Duration::from_millis(full_write_ms),
            "Streaming TTF {:?} must be < full write time {:?}",
            streaming_ttf,
            Duration::from_millis(full_write_ms),
        );
    }

    #[test]
    fn cache_hit_returns_instantly() {
        let key = "test_cache_video_id".to_string();
        let data: Arc<[u8]> = Arc::from(vec![0u8; 64 * 1024]);
        cache_put(key.clone(), data);

        let t0 = Instant::now();
        let cached = cache_get(&key);
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_millis(1),
            "Cache get {:?}",
            elapsed
        );

        // Decoder creation from cached bytes should not block on I/O
        // even though the data is invalid audio.
        let cached = cached.unwrap();
        let len = cached.len() as u64;
        let cursor = std::io::Cursor::new(cached);
        let source = ReadSeekSource::new(cursor, Some(len));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let t0 = Instant::now();
        let _result = SymphoniaDecoder::new(mss);
        let elapsed = t0.elapsed();
        println!("cache decoder creation: {:?}", elapsed);
        assert!(
            elapsed < Duration::from_millis(100),
            "Cached decoder creation must not block (>100ms: {:?})",
            elapsed
        );
    }

    #[test]
    fn streaming_creates_decoder_from_empty_buffer_then_gets_frames() {
        // Most realistic simulation: buffer is EMPTY at decoder creation
        // time (no data arrived yet), total_len is set from yt-dlp progress.
        // Data streams in afterwards.  Symphonia's probe blocks on Condvar,
        // wakes up when the first chunk arrives, and the decoder eventually
        // produces frames.  This mirrors download_and_decode exactly.
        let chunk_delay = Duration::from_millis(2);

        let buf = SharedBuffer::new();
        buf.set_total_len(TEST_MP3.len() as u64);

        // Spawn writer FIRST (like real download_and_decode spawns
        // stdout_handle before creating the decoder).  The writer runs
        // in the background — by the time we create the decoder, the
        // first chunk may or may not have arrived.
        let handle = spawn_slow_write(&buf, TEST_MP3, chunk_delay);

        let mut dec = create_decoder_from(&buf)
            .expect("Decoder from streaming buffer (data may not have arrived yet)");

        let ttf = time_to_first_frame(&mut dec, 44100);
        assert!(
            ttf.is_some(),
            "Decoder should produce frames after data starts arriving"
        );
        handle.join().unwrap();
    }

    #[test]
    fn streaming_cache_eviction_works() {
        CACHE_MAX_ENTRIES.store(3, Ordering::Release);
        for i in 0..5 {
            cache_put(format!("evict_test_{}", i), Arc::from(vec![0u8; 1024]));
        }
        // With max=3: entries 0,1 evicted; 2,3,4 remain.
        assert!(
            cache_get("evict_test_0").is_none(),
            "Oldest entry (0) should be evicted"
        );
        assert!(
            cache_get("evict_test_1").is_none(),
            "Oldest entry (1) should be evicted"
        );
        assert!(
            cache_get("evict_test_2").is_some(),
            "Entry 2 should still be in cache"
        );
        assert!(
            cache_get("evict_test_3").is_some(),
            "Entry 3 should still be in cache"
        );
        assert!(
            cache_get("evict_test_4").is_some(),
            "Entry 4 should still be in cache"
        );
    }

    #[test]
    fn streaming_cache_lru_hit_updates_order() {
        CACHE_MAX_ENTRIES.store(3, Ordering::Release);
        for i in 0..3 {
            cache_put(format!("lru_test_{}", i), Arc::from(vec![0u8; 1024]));
        }
        // Hit entry 0 (oldest) — LRU promotes it to most-recent
        assert!(
            cache_get("lru_test_0").is_some(),
            "Entry 0 should be in cache"
        );
        // Now add 2 more entries (3→4).  With max=3, evicts the LRU tail
        // which should be entry 1 (oldest after 0 was promoted).
        for i in 3..5 {
            cache_put(format!("lru_test_{}", i), Arc::from(vec![0u8; 1024]));
        }
        assert!(
            cache_get("lru_test_1").is_none(),
            "Entry 1 (now oldest) should be evicted"
        );
        assert!(
            cache_get("lru_test_0").is_some(),
            "Entry 0 (promoted by hit) should survive"
        );
    }

    // ── Performance benchmarks ─────────────────────────────────────────
    //
    // These measure time-to-first-frame (TTF) comparing the ffmpeg relay
    // approach against pure decoder creation.  Run with:
    //   cargo test ffmpeg_relay -- --nocapture
    //   cargo test download_pipeline -- --nocapture

    /// Spawn ffmpeg, read MP3 from stdout into a SharedBuffer.
    /// Returns (buffer, writer_handle, child_with_stdin).
    /// Caller must write data to child.stdin, then drop stdin so ffmpeg
    /// sees EOF, then wait for the child to exit.
    fn start_ffmpeg_relay() -> anyhow::Result<(
        Arc<SharedBuffer>,
        std::thread::JoinHandle<()>,
        std::process::Child,
    )> {
        let buf = SharedBuffer::new();
        let mut wtr = buf.writer();

        let mut ffmpeg = std::process::Command::new("ffmpeg");
        ffmpeg
            .args([
                "-i",
                "pipe:0",
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-f",
                "mp3",
                "-compression_level",
                "5",
                "-ab",
                "128k",
                "-loglevel",
                "error",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = ffmpeg.spawn().context("spawn ffmpeg for WAV transcode")?;
        let ffmpeg_stdout = child.stdout.take().context("ffmpeg stdout not captured")?;

        // Writer thread: read ffmpeg stdout → SharedBuffer
        let writer = std::thread::spawn(move || {
            use std::io::Read;
            let mut rdr = std::io::BufReader::new(ffmpeg_stdout);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match rdr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => wtr.write(&buf[..n]),
                    Err(_) => {
                        wtr.fail();
                        return;
                    }
                }
            }
            wtr.finish();
        });

        Ok((buf, writer, child))
    }

    #[test]
    fn ffmpeg_relay_ttf_from_webm_file() {
        // Load the saved WebM file, pipe through ffmpeg, measure TTF.
        let webm_data = match std::fs::read("/tmp/test_streaming.webm") {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP: /tmp/test_streaming.webm not available: {e}");
                return;
            }
        };

        // Track absolute time from the start of the pipeline.
        let t0 = Instant::now();

        let (buf, writer, mut child) = start_ffmpeg_relay().expect("ffmpeg relay setup");

        // Write WebM data to ffmpeg stdin, then close stdin (EOF).
        let mut stdin = child.stdin.take().unwrap();
        use std::io::Write;
        stdin.write_all(&webm_data).unwrap();
        drop(stdin);
        let write_finished = t0.elapsed();

        // Wait for ffmpeg to finish, then writer thread to drain stdout.
        let exit_status = child.wait();
        let ffmpeg_finished = t0.elapsed();
        let _ = writer.join();
        let all_data_written = t0.elapsed();

        // Time decoder creation from streaming buffer.
        let t_dec = Instant::now();
        let mut dec = create_decoder_from(&buf).expect("decoder from ffmpeg relay stream");
        let decoder_dur = t_dec.elapsed();

        let ttf = time_to_first_frame(&mut dec, 44100);

        match ttf {
            Some(dur) => println!(
                "BENCH ffmpeg_relay: \
                 write_data={write_finished:?}  \
                 ffmpeg_exit={ffmpeg_finished:?}  \
                 buf_ready={all_data_written:?}  \
                 decoder_init={decoder_dur:?}  \
                 ttf={dur:?}  \
                 total_from_start={:?}  \
                 buf_len={}",
                t0.elapsed(),
                buf.len()
            ),
            None => println!("BENCH ffmpeg_relay: DECODER FAILED (no frames produced)"),
        }

        assert!(ttf.is_some(), "ffmpeg relay must produce frames");
        assert!(exit_status.is_ok(), "ffmpeg exit status = {exit_status:?}");
    }

    #[test]
    fn m4a_decoder_ttf_from_full_download() {
        // Download a 10s M4A clip and measure full-download-then-decode TTF.
        // This is the "old approach" baseline.
        use std::io::Read;

        let output_path = "/tmp/yt_bench_m4a.m4a";
        let status = std::process::Command::new("yt-dlp")
            .args([
                "-f",
                "bestaudio[ext=m4a]",
                "--download-sections",
                "*0-10",
                "-o",
                output_path,
                "--no-warnings",
                "--no-playlist",
                "--print",
                "after_move:",
                "https://music.youtube.com/watch?v=jNQXAC9IVRw",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match status {
            Ok(s) if !s.success() => {
                eprintln!("SKIP m4a_decoder_ttf: yt-dlp exit code {:?}", s.code());
                return;
            }
            Err(e) => {
                eprintln!("SKIP m4a_decoder_ttf: yt-dlp not available: {e}");
                return;
            }
            _ => {}
        }

        let data = match std::fs::read(output_path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP m4a_decoder_ttf: failed to read {output_path}: {e}");
                return;
            }
        };

        let dl_dur: Duration = {
            let t0 = Instant::now();
            let mut f = std::fs::File::open(output_path).unwrap();
            let mut buf = Vec::with_capacity(data.len());
            f.read_to_end(&mut buf).unwrap();
            t0.elapsed()
        };

        // Time full-download decoder creation + first frame.
        let len = data.len() as u64;
        let cursor = std::io::Cursor::new(data);
        let source = ReadSeekSource::new(cursor, Some(len));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let t0 = Instant::now();
        let mut dec = SymphoniaDecoder::new(mss).expect("decoder from full M4A");
        let decoder_dur = t0.elapsed();

        let ttf = time_to_first_frame(&mut dec, 44100);

        let _ = std::fs::remove_file(output_path);

        match ttf {
            Some(dur) => println!(
                "BENCH m4a_full: download_io={:?}  decoder_init={:?}  ttf={:?}  total_from_ytdlp_start={:?}  file_len={}",
                dl_dur,
                decoder_dur,
                dur,
                dl_dur + decoder_dur + dur,
                len,
            ),
            None => println!("BENCH m4a_full: DECODER FAILED"),
        }

        // Store for reference in the comparison test.
        assert!(ttf.is_some(), "M4A decoder must produce frames");
    }

    #[test]
    fn download_pipeline_comparison() {
        // End-to-end: run both the ffmpeg relay and the M4A direct approach
        // using the same video, and report timing breakdowns.
        //
        // This test REQUIRES yt-dlp and network access.  Skips silently if
        // either is unavailable.
        use std::io::Read;
        use std::io::Write;

        let video_id = "jNQXAC9IVRw";
        let url = format!("https://music.youtube.com/watch?v={video_id}");

        // ── ffmpeg relay path ───────────────────────────────────────
        println!("--- ffmpeg relay ---");

        let t0 = Instant::now();
        let mut yt = std::process::Command::new("yt-dlp");
        yt.args([
            "-f",
            "bestaudio[ext=webm]",
            "--download-sections",
            "*0-10",
            "-o",
            "-",
            "--no-warnings",
            "--no-playlist",
            &url,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
        let mut yt_child = match yt.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("SKIP: cannot spawn yt-dlp: {e}");
                return;
            }
        };
        let yt_stdout = yt_child.stdout.take().unwrap();
        let yt_spawn_dur = t0.elapsed();

        let mut ffmpeg = std::process::Command::new("ffmpeg");
        ffmpeg
            .args([
                "-i",
                "pipe:0",
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-f",
                "mp3",
                "-compression_level",
                "5",
                "-ab",
                "128k",
                "-loglevel",
                "error",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut ff_child = ffmpeg.spawn().expect("spawn ffmpeg for relay test");
        let mut ff_stdin = ff_child.stdin.take().expect("ffmpeg stdin");
        let ff_stdout = ff_child.stdout.take().expect("ffmpeg stdout");
        let ff_spawn_dur = t0.elapsed();

        let relay_buf = Arc::new(SharedBuffer::new());
        let mut relay_wtr = relay_buf.writer();

        // Relay thread: yt-dlp stdout → ffmpeg stdin
        let relay_handle = std::thread::spawn(move || {
            let mut rdr = std::io::BufReader::new(yt_stdout);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match rdr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = ff_stdin.write_all(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        });

        // Writer thread: ffmpeg stdout → SharedBuffer
        let relay_writer = std::thread::spawn(move || {
            let mut rdr = std::io::BufReader::new(ff_stdout);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match rdr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => relay_wtr.write(&buf[..n]),
                    Err(_) => {
                        relay_wtr.fail();
                        return;
                    }
                }
            }
            relay_wtr.finish();
        });

        // Wait for first data, then create decoder (simulating real flow).
        while relay_buf.len() < 2048 {
            std::thread::sleep(Duration::from_millis(1));
        }
        let relay_data_arrival = t0.elapsed();

        let t_dec = Instant::now();
        let mut dec_relay = create_decoder_from(&relay_buf).expect("decoder from relay stream");
        let relay_decoder_dur = t_dec.elapsed();

        let relay_ttf = time_to_first_frame(&mut dec_relay, 44100);

        relay_handle.join().unwrap();
        relay_writer.join().unwrap();
        let _ = yt_child.wait();
        let _ = ff_child.wait();
        let relay_total_dur = t0.elapsed();

        // ── M4A direct path ──────────────────────────────────────────
        println!("--- M4A full download ---");

        let t0 = Instant::now();
        let output_path = "/tmp/yt_bench_m4a_comparison.m4a";
        let mut yt2 = std::process::Command::new("yt-dlp");
        yt2.args([
            "-f",
            "bestaudio[ext=m4a]",
            "--download-sections",
            "*0-10",
            "-o",
            output_path,
            "--no-warnings",
            "--no-playlist",
            &url,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

        match yt2.status() {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("SKIP: yt-dlp exit code {:?}", s.code());
                return;
            }
            Err(e) => {
                eprintln!("SKIP: yt-dlp not available: {e}");
                return;
            }
        }
        let yt2_spawn_dur = t0.elapsed();

        let m4a_data = match std::fs::read(output_path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP: failed to read {output_path}: {e}");
                return;
            }
        };
        let _ = std::fs::remove_file(output_path);
        let m4a_dl_dur = t0.elapsed();

        let m4a_buf = SharedBuffer::new();
        let mut m4a_wtr = m4a_buf.writer();
        m4a_wtr.write(&m4a_data);
        m4a_wtr.finish();
        drop(m4a_wtr);

        let t_dec = Instant::now();
        let mut dec_m4a = create_decoder_from(&m4a_buf).expect("decoder from full M4A");
        let m4a_decoder_dur = t_dec.elapsed();

        let m4a_ttf = time_to_first_frame(&mut dec_m4a, 44100);
        let m4a_total_dur = t0.elapsed();

        // ── Results ──────────────────────────────────────────────────
        let relay_playable = relay_data_arrival + relay_decoder_dur + relay_ttf.unwrap_or_default();
        let m4a_playable = yt2_spawn_dur + m4a_decoder_dur + m4a_ttf.unwrap_or_default();
        let relay_dl_fraction =
            relay_data_arrival.as_secs_f64() / relay_total_dur.as_secs_f64().max(0.001);
        let m4a_dl_fraction = m4a_dl_dur.as_secs_f64() / m4a_total_dur.as_secs_f64().max(0.001);

        let playable_delta = relay_playable.as_secs_f64() - m4a_playable.as_secs_f64();
        println!(
            "===== PIPELINE COMPARISON (video: {video_id}) =====\n\
             \n  ffmpeg relay:\
             \n    yt-dlp spawn:     {:>8.1?}\
             \n    ffmpeg spawn:     {:>8.1?}\
             \n    first data @{:>5} bytes: {:>8.1?}\
             \n    decoder init:     {:>8.1?}\
             \n    time-to-first-frame: {:>8.1?}\
             \n    playable @:       {:>8.1?}\
             \n    relay total (incl. cleanup): {:>8.1?}\
             \n    final mp3 buf:    {} bytes  ({}% of download time)\
             \n  \n  M4A direct:\
             \n    yt-dlp download:  {:>8.1?}\
             \n    decoder init:     {:>8.1?}\
             \n    time-to-first-frame: {:>8.1?}\
             \n    playable @:       {:>8.1?}\
             \n    total (cleanup):  {:>8.1?}\
             \n    final m4a buf:    {} bytes  ({}% of total)\
             \n  \n  PLAYABLE delta: relay @ {:>8.1?} vs M4A @ {:>8.1?} = {:+.3}s",
            yt_spawn_dur,
            ff_spawn_dur,
            relay_buf.len(),
            relay_data_arrival,
            relay_decoder_dur,
            relay_ttf.unwrap_or_default(),
            relay_playable,
            relay_total_dur,
            relay_buf.len(),
            relay_dl_fraction * 100.0,
            yt2_spawn_dur,
            m4a_decoder_dur,
            m4a_ttf.unwrap_or_default(),
            m4a_playable,
            m4a_total_dur,
            m4a_buf.len(),
            m4a_dl_fraction * 100.0,
            relay_playable,
            m4a_playable,
            playable_delta,
        );

        assert!(relay_ttf.is_some(), "ffmpeg relay must produce frames");
        assert!(m4a_ttf.is_some(), "M4A decoder must produce frames");
    }

    #[test]
    fn wav_decoder_from_file() {
        // Test if symphonia's WAV reader handles ffmpeg's generated WAV
        // (RIFF header with 0xFFFFFFFF size, embedded LIST metadata).
        let wav_path = std::path::Path::new("/tmp/test_output.wav");
        if !wav_path.exists() {
            eprintln!(
                "SKIP: /tmp/test_output.wav not found (run: ffmpeg -i test.mp3 -f wav /tmp/test_output.wav)"
            );
            return;
        }

        let t0 = Instant::now();
        let wav_data = std::fs::read(wav_path).expect("read WAV file");
        let read_dur = t0.elapsed();
        eprintln!("WAV file: {} bytes, read={:?}", wav_data.len(), read_dur);

        // Full-data decoder (Cursor with owned data)
        let t_dec = Instant::now();
        let cursor = std::io::Cursor::new(wav_data.clone());
        let source = ReadSeekSource::new(cursor, Some(wav_data.len() as u64));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let mut dec = match SymphoniaDecoder::new(mss) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("FAIL: symphonia rejected WAV: {e:?}");
                return;
            }
        };
        let decoder_dur = t_dec.elapsed();

        let ttf = time_to_first_frame(&mut dec, 44100);
        eprintln!("WAV decoder (full): init={:?}, ttf={:?}", decoder_dur, ttf);
        assert!(ttf.is_some(), "full WAV must produce frames");

        // Now test STREAMING: SharedBuffer with partial data.
        // Use spawn_blocking + timeout to simulate try_streaming_init.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            let buf = Arc::new(SharedBuffer::new());
            let mut writer = buf.writer();

            // Write only first 8KB (simulating streaming).
            let header_size = 8192.min(wav_data.len());
            writer.write(&wav_data[..header_size]);

            let t = tokio::time::Instant::now();
            let r = try_streaming_init(&buf, None).await;
            (t.elapsed(), r)
        });

        match result.1 {
            Ok(mut dec) => {
                eprintln!("WAV stream: init={:?}", result.0);
                // Write remaining data so decoder can read frames
                let mut writer = SharedBuffer::new().writer();
                writer.write(&wav_data[8192..]);
                writer.finish();

                let ttf2 = time_to_first_frame(&mut dec, 44100);
                eprintln!("WAV stream: ttf={:?}", ttf2);
                assert!(ttf2.is_some(), "streaming WAV must produce frames");
            }
            Err(e) => {
                eprintln!("WAV stream init FAILED: {e:?}");
            }
        }
    }

    #[test]
    fn dropped_joinhandle_does_not_cancel_kill_on_drop_child() {
        // Regression guard for Session 2026-07-16 bug:
        // download_and_decode used cancel_prev_bg_task() which called
        // AbortHandle::abort() on a spawned task.  The task owned a
        // Child with kill_on_drop(true), so aborting the task dropped
        // the Child — killing ffmpeg mid-stream, starving the buffer.
        //
        // This test proves:
        //   1) Dropping JoinHandle does NOT cancel the task (child survives)
        //   2) AbortHandle::abort() DOES cancel the task (child dies)
        //
        // Future code MUST use fire-and-forget JoinHandles (_cache_task)
        // and NEVER store abortable handles for download background tasks.
        use std::time::Duration;

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // ── Positive: Drop JoinHandle, child survives ──────────────
            let mut child = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn sleep");
            let pid = child.id().expect("child pid");

            let handle = tokio::spawn(async move {
                let _ = child.wait().await;
            });
            drop(handle); // ← this is what _cache_task does

            tokio::time::sleep(Duration::from_millis(200)).await;

            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .expect("kill -0");
            assert!(
                alive.success(),
                "process {pid} must survive JoinHandle drop (kill_on_drop should NOT fire)"
            );
            eprintln!("PASS: JoinHandle drop → child {pid} alive (as expected)");

            // Clean up this process before the next sub-test.
            std::process::Command::new("kill")
                .arg(pid.to_string())
                .status()
                .ok();

            // ── Negative: AbortHandle::abort kills the child ───────────
            let mut child2 = tokio::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn sleep 2");
            let pid2 = child2.id().expect("child2 pid");

            let abort_handle = tokio::spawn(async move {
                let _ = child2.wait().await;
            });
            let ab = abort_handle.abort_handle();
            ab.abort(); // ← this is what cancel_prev_bg_task did — BUG
            // abort is async — give it time to cancel the task + drop child2
            tokio::time::sleep(Duration::from_millis(200)).await;

            let alive2 = std::process::Command::new("kill")
                .args(["-0", &pid2.to_string()])
                .status()
                .expect("kill -0 2");
            assert!(
                !alive2.success(),
                "process {pid2} must be DEAD after AbortHandle::abort (kill_on_drop fires)"
            );
            eprintln!("PASS: AbortHandle::abort → child {pid2} dead (kill_on_drop fired)");
        });
    }
}
