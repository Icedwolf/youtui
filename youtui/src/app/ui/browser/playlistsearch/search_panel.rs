// Thin re-export wrapper for the unified SearchPanel.
use crate::app::ui::browser::search_panel::PlaylistSearchConfig;
pub use crate::app::ui::browser::search_panel::{
    BrowserPlaylistsAction, NonPodcastSearchResultPlaylist,
};

pub type PlaylistSearchPanel =
    crate::app::ui::browser::search_panel::SearchPanel<PlaylistSearchConfig>;
