// Thin re-export wrapper for the unified SearchPanel.
use crate::app::ui::browser::search_panel::ArtistSearchConfig;
pub use crate::app::ui::browser::search_panel::BrowserArtistsAction;

pub type ArtistSearchPanel = crate::app::ui::browser::search_panel::SearchPanel<ArtistSearchConfig>;
