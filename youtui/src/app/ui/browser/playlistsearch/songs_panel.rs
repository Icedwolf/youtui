// Thin re-export wrapper for the unified SongsPanel.
pub use crate::app::ui::browser::songs_panel::BrowserPlaylistSongsAction;
use crate::app::ui::browser::songs_panel::PlaylistSongsConfig;

pub type PlaylistSongsPanel = crate::app::ui::browser::songs_panel::SongsPanel<PlaylistSongsConfig>;
