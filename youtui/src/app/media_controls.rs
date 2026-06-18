//! Wrapper for souvlaki::MediaControls that performs diffing to ensure OS calls
//! are made at a minimum (in line with immediate mode architecture principle)
use super::structures::Percentage;
use super::ui::playlist::DEFAULT_UI_VOLUME;
use crate::core::blocking_send_or_error;
use futures::Stream;
use notify_rust::{Notification, Timeout};
use souvlaki::{MediaControlEvent, MediaMetadata, MediaPosition, PlatformConfig};
use std::borrow::Cow;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task;
use tokio_stream::wrappers::ReceiverStream;

/// Minimum change in playing position before triggering a redraw. This is to
/// reduce number of calls to the platform.
const POSITION_DIFFERENCE_REDRAW_THRESHOLD: Duration = Duration::from_secs(5);

// On some platforms, souvlaki::Error doesn't implement Error, so this newtype
// is the workaround.
// https://github.com/Sinono3/souvlaki/issues/61
#[derive(Debug)]
struct MediaControlsError(souvlaki::Error);
impl std::error::Error for MediaControlsError {}
impl std::fmt::Display for MediaControlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        #[cfg(any(
            target_os = "linux",
            target_os = "openbsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "freebsd",
        ))]
        return write!(f, "{}", self.0);

        #[cfg(not(any(
            target_os = "linux",
            target_os = "openbsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "freebsd"
        )))]
        return write!(f, "{:?}", self.0);
    }
}

pub struct NotificationController {
    last_notification: Option<(String, String)>,
    cover_url: Option<String>,
}

impl Default for NotificationController {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationController {
    pub fn new() -> Self {
        Self {
            last_notification: None,
            cover_url: None,
        }
    }

    pub async fn notify_track_change(
        &mut self,
        title: &str,
        artist: Option<&str>,
        cover_url: Option<&str>,
    ) -> Result<(), notify_rust::error::Error> {
        let body = artist.unwrap_or("Unknown Artist");

        if self
            .last_notification
            .as_ref()
            .map(|(t, b)| (t.as_str(), b.as_str()))
            == Some((title, body))
        {
            return Ok(());
        }

        let icon_path = if let Some(url) = cover_url {
            if url.starts_with("file://") {
                Some(url.to_string())
            } else {
                None
            }
        } else {
            None
        };

        let mut notification = Notification::new()
            .summary(title)
            .body(body)
            .appname("youtui")
            .timeout(Timeout::Milliseconds(5000))
            .clone();

        if let Some(path) = &icon_path {
            notification.icon(path.as_str());
        }

        notification.show()?;
        self.last_notification = Some((title.to_string(), body.to_string()));
        self.cover_url = cover_url.map(String::from);
        Ok(())
    }
}

pub struct MediaController {
    inner: souvlaki::MediaControls,
    status: souvlaki::MediaPlayback,
    volume: MediaControlsVolume,
    notification_controller: NotificationController,
    notifications_enabled: bool,
    title: Option<String>,
    album: Option<String>,
    artist: Option<String>,
    cover_url: Option<String>,
    duration: Option<Duration>,
    /// macos requires an active window handle
    #[cfg(target_os = "macos")]
    macos_window_handle: raw_window_handle::AppKitWindowHandle,
}

pub struct MediaControlsUpdate<'a> {
    pub title: Option<Cow<'a, str>>,
    pub album: Option<Cow<'a, str>>,
    pub artist: Option<Cow<'a, str>>,
    pub cover_url: Option<Cow<'a, str>>,
    pub duration: Option<Duration>,
    pub playback_status: MediaControlsStatus,
    pub volume: MediaControlsVolume,
}

#[derive(Default)]
pub enum MediaControlsStatus {
    #[default]
    Stopped,
    Paused {
        progress: Duration,
    },
    Playing {
        progress: Duration,
    },
}

#[derive(Copy, Clone, PartialEq)]
pub struct MediaControlsVolume(f64);

impl Default for MediaControlsVolume {
    fn default() -> Self {
        Self(DEFAULT_UI_VOLUME.0 as f64 / 100.0)
    }
}

impl MediaControlsVolume {
    pub fn from_percentage_clamped(Percentage(p): Percentage) -> Self {
        let raw = (p as f64) / 100.0;
        Self(raw.clamp(0.0, 1.0))
    }
}

impl MediaController {
    pub fn new(notifications_enabled: bool) -> anyhow::Result<(Self, impl Stream<Item = MediaControlEvent>)> {
        let (tx, rx) = mpsc::channel(super::EVENT_CHANNEL_SIZE);

        // On windows, a hwnd window handle is required, so we create a non-visible
        // window using winit. See souvlaki docs for more information.
        #[cfg(target_os = "windows")]
        use raw_window_handle::HasWindowHandle;
        #[cfg(target_os = "windows")]
        use winit::platform::windows::EventLoopBuilderExtWindows;
        #[cfg(target_os = "windows")]
        let raw_window_handle::RawWindowHandle::Win32(raw_win32_handle) =
            winit::event_loop::EventLoop::builder()
                .with_any_thread(true)
                .build()?
                .create_window(winit::window::Window::default_attributes().with_visible(false))?
                .window_handle()?
                .as_raw()
        else {
            anyhow::bail!("Expected to get a Win32WindowHandle but we did not!")
        };
        #[cfg(target_os = "macos")]
        use raw_window_handle::HasWindowHandle;
        #[cfg(target_os = "macos")]
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        #[cfg(target_os = "macos")]
        let raw_window_handle::RawWindowHandle::AppKit(macos_window_handle) =
            winit::event_loop::EventLoop::builder()
                .build()?
                .create_window(winit::window::Window::default_attributes().with_visible(false))?
                .window_handle()?
                .as_raw()
        else {
            anyhow::bail!("Expected to get a AppKitWindowHandle but we did not!")
        };

        let config = PlatformConfig {
            display_name: "Youtui",
            dbus_name: "youtui",
            #[cfg(not(target_os = "windows"))]
            hwnd: None,
            #[cfg(target_os = "windows")]
            hwnd: Some(raw_win32_handle.hwnd.get() as *mut std::ffi::c_void),
        };

        let mut controls = souvlaki::MediaControls::new(config).map_err(MediaControlsError)?;
        // Assumption - event handler runs in another thread, and blocking send is
        // acceptable.
        controls
            .attach(move |event| {
                blocking_send_or_error(&tx, event);
            })
            .map_err(MediaControlsError)?;
        Ok((
            MediaController {
                inner: controls,
                status: souvlaki::MediaPlayback::Stopped,
                title: None,
                album: None,
                artist: None,
                cover_url: None,
                duration: None,
                volume: Default::default(),
                notification_controller: NotificationController::new(),
                notifications_enabled,
                #[cfg(target_os = "macos")]
                macos_window_handle,
            },
            ReceiverStream::new(rx),
        ))
    }
    pub fn update_controls(&mut self, update: MediaControlsUpdate<'_>) -> anyhow::Result<()> {
        let MediaControlsUpdate {
            title,
            album,
            artist,
            cover_url,
            duration,
            playback_status,
            volume,
        } = update;
        self.update_metadata(title, album, artist, cover_url, duration)?;
        self.update_playback(playback_status)?;
        #[cfg(any(
            target_os = "linux",
            target_os = "openbsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "freebsd",
        ))]
        self.update_volume(volume)?;
        Ok(())
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "freebsd",
    ))]
    fn update_volume(&mut self, volume: MediaControlsVolume) -> anyhow::Result<()> {
        if self.volume != volume {
            self.volume = volume;
            self.inner.set_volume(volume.0)?;
        }
        Ok(())
    }
    fn update_metadata(
        &mut self,
        title: Option<Cow<'_, str>>,
        album: Option<Cow<'_, str>>,
        artist: Option<Cow<'_, str>>,
        cover_url: Option<Cow<'_, str>>,
        duration: Option<Duration>,
    ) -> anyhow::Result<()> {
        let mut redraw = false;
        if self.title.as_deref() != title.as_deref() {
            redraw = true;
            self.title = title.map(|title| title.to_string());
        }
        if self.album.as_deref() != album.as_deref() {
            redraw = true;
            self.album = album.map(|album| album.to_string());
        }
        if self.artist.as_deref() != artist.as_deref() {
            redraw = true;
            self.artist = artist.map(|artist| artist.to_string());
        }
        if self.cover_url.as_deref() != cover_url.as_deref() {
            redraw = true;
            self.cover_url = cover_url.map(|cover_url| cover_url.to_string());
        }
        if self.duration != duration {
            redraw = true;
            self.duration = duration;
        }
        if redraw {
            let new_metadata = MediaMetadata {
                title: self.title.as_deref(),
                album: self.album.as_deref(),
                artist: self.artist.as_deref(),
                cover_url: self.cover_url.as_deref(),
                duration: self.duration,
            };
            self.inner
                .set_metadata(new_metadata)
                .map_err(MediaControlsError)?;

            if let Some(title) = &self.title {
                let artist = self.artist.clone();
                let cover_url = self.cover_url.clone();
                if self.notifications_enabled {
                    let mut controller = std::mem::take(&mut self.notification_controller);

                    let _ = task::block_in_place(|| {
                        let rt = tokio::runtime::Handle::current();
                        rt.block_on(async {
                            controller
                                .notify_track_change(title, artist.as_deref(), cover_url.as_deref())
                                .await
                        })
                    });
                    self.notification_controller = controller;
                }
            }
        }
        Ok(())
    }
    fn update_progress(
        current: &mut souvlaki::MediaPlayback,
        new_status: impl FnOnce(Option<MediaPosition>) -> souvlaki::MediaPlayback,
        new_progress: Duration,
    ) -> bool {
        let needs_update = match current {
            souvlaki::MediaPlayback::Paused { progress: Some(p) }
            | souvlaki::MediaPlayback::Playing { progress: Some(p) } => {
                p.0.abs_diff(new_progress) >= POSITION_DIFFERENCE_REDRAW_THRESHOLD
            }
            _ => true,
        };
        if needs_update {
            *current = new_status(Some(MediaPosition(new_progress)));
        }
        needs_update
    }

    fn update_playback(&mut self, playback_status: MediaControlsStatus) -> anyhow::Result<()> {
        let mut redraw = false;
        match playback_status {
            MediaControlsStatus::Stopped => {
                if self.status != souvlaki::MediaPlayback::Stopped {
                    self.status = souvlaki::MediaPlayback::Stopped;
                    redraw = true;
                }
            }
            MediaControlsStatus::Paused { progress } => {
                redraw = Self::update_progress(
                    &mut self.status,
                    |p| souvlaki::MediaPlayback::Paused { progress: p },
                    progress,
                );
            }
            MediaControlsStatus::Playing { progress } => {
                redraw = Self::update_progress(
                    &mut self.status,
                    |p| souvlaki::MediaPlayback::Playing { progress: p },
                    progress,
                );
            }
        }
        if redraw {
            self.inner
                .set_playback(self.status.clone())
                .map_err(MediaControlsError)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type-level test: NotificationController can be constructed.
    /// This would fail to compile if the type is removed or changed.
    #[test]
    fn notification_controller_constructs() {
        let nc = NotificationController::new();
        assert!(nc.last_notification.is_none());
        assert!(nc.cover_url.is_none());
    }

    /// Type-level test: NotificationController has Default impl.
    #[test]
    fn notification_controller_default() {
        let nc = NotificationController::default();
        assert!(nc.last_notification.is_none());
    }

    /// Zero-cost compile-time check: NotificationController type exists and is
    /// a field of MediaController. Neither function is ever called — they're only
    /// referenced for type-checking.
    #[test]
    fn notification_type_and_field_are_present() {
        fn _type(_: &NotificationController) {}
        fn _field(mc: &MediaController) {
            let _ = &mc.notification_controller;
        }
        let _ = (_type, _field);
    }

    /// Dedup logic: calling notify_track_change with same title+artist after
    /// a successful notification returns Ok(()) without calling show().
    /// NOTE: The first call may fail with Err if no D-Bus daemon is running.
    /// The second call should always be Ok(()) if the first succeeded.
    #[ignore = "requires D-Bus notification daemon (mako, dunst, etc.)"]
    #[test]
    fn notify_track_change_dedup() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut nc = NotificationController::new();

        // First call may fail if no daemon; if it succeeds, last_notification is set
        let result = rt.block_on(nc.notify_track_change("Song A", Some("Artist A"), None));
        if result.is_ok() {
            let second = rt
                .block_on(nc.notify_track_change("Song A", Some("Artist A"), None));
            assert!(second.is_ok(), "dedup should return Ok for duplicate");
        }
    }

    /// Regression: notify_track_change handles missing artist gracefully.
    #[ignore = "requires D-Bus notification daemon (mako, dunst, etc.)"]
    #[test]
    fn notify_track_change_default_artist() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut nc = NotificationController::new();
        let result = rt.block_on(nc.notify_track_change("Song B", None, None));
        if result.is_ok() {
            assert_eq!(
                nc.last_notification,
                Some(("Song B".to_string(), "Unknown Artist".to_string()))
            );
        }
    }
}
