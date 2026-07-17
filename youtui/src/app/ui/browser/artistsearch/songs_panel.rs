// Thin re-export wrapper for the unified SongsPanel.
use crate::app::ui::browser::songs_panel::ArtistSongsConfig;
pub use crate::app::ui::browser::songs_panel::BrowserArtistSongsAction;

pub type AlbumSongsPanel = crate::app::ui::browser::songs_panel::SongsPanel<ArtistSongsConfig>;
