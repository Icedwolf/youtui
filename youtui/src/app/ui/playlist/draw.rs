use super::*;

impl TableView for Playlist {
    fn get_selected_item(&self) -> usize {
        self.cur_selected
    }

    fn get_state(&self) -> &ScrollingTableState {
        &self.widget_state
    }

    fn get_layout(&self) -> &[BasicConstraint] {
        &[
            BasicConstraint::Length(1),
            BasicConstraint::Percentage(Percentage(33)),
            BasicConstraint::Percentage(Percentage(33)),
            BasicConstraint::Percentage(Percentage(33)),
            BasicConstraint::Length(4),
            BasicConstraint::Length(9),
        ]
    }

    fn get_items(&self) -> impl ExactSizeIterator<Item = impl Iterator<Item = Cow<'_, str>> + '_> {
        let count: usize = if !self.search_text.is_empty() {
            self.search_indices.len()
        } else if self.shuffle_enabled {
            self.shuffle_indices.len()
        } else {
            self.list.get_list_iter().count()
        };

        let cur_playing_visual = self
            .get_cur_playing_index()
            .and_then(|idx| self.actual_to_visual_index(idx));

        (0..count).map(move |visual_i| {
            let actual_i = self.visual_to_actual_index(visual_i);

            let playing_indicator: Cow<'_, str> =
                if Some(visual_i) == cur_playing_visual { ">".into() } else { "".into() };

            let fields = [
                ListSongDisplayableField::Song,
                ListSongDisplayableField::Artists,
                ListSongDisplayableField::Album,
                ListSongDisplayableField::Year,
                ListSongDisplayableField::Duration,
            ];

            match self.list.get_song_from_idx(actual_i) {
                // Normal row.
                Some(ls) => iter::once(playing_indicator).chain(ls.get_fields(fields)),
                // Inverse-map desync: never panic the whole TUI on a render
                // frame. Emit an empty row and log instead.
                None => {
                    debug!(
                        visual_i,
                        actual_i,
                        "draw: visual_to_actual_index desync, rendering empty row"
                    );
                    let empty = std::array::from_fn(|_| Cow::Borrowed(""));
                    iter::once(playing_indicator).chain(empty)
                }
            }
        })
    }

    fn get_headings(&self) -> impl Iterator<Item = &'static str> {
        ["", "Song", "Artists", "Album", "Year", "Duration"].into_iter()
    }

    fn get_highlighted_row(&self) -> Option<usize> {
        self.get_cur_playing_index()
            .and_then(|idx| self.actual_to_visual_index(idx))
    }

    fn get_mut_state(&mut self) -> &mut ScrollingTableState {
        &mut self.widget_state
    }
}

impl HasTitle for Playlist {
    fn get_title(&self) -> Line<'static> {
        {
            let cached = self.cached_title.borrow();
            if let Some(title) = cached.as_ref() {
                return title.clone();
            }
        }
        let shuffle_indicator = if self.shuffle_enabled {
            " [SHUFFLE]"
        } else {
            ""
        };

        let resolve_indicator = if self.resolving_audio {
            " [RESOLVING]"
        } else {
            ""
        };

        let next_indicator = if !self.play_next_queue.is_empty() {
            format!(" [NEXT: {}]", self.play_next_queue.len())
        } else {
            String::new()
        };

        let song_count = self.list.get_list_iter().len();
        let base = format!(
            "Local playlist - {song_count} songs{shuffle_indicator}{resolve_indicator}{next_indicator}"
        );

        let title = if !self.search_text.is_empty() {
            let search_indicator = format!(" [SEARCH: {}]", self.search_text);
            if self.search_indices.is_empty() {
                Line::from(vec![
                    Span::raw(base),
                    Span::styled(search_indicator, Style::new().fg(Color::Red)),
                ])
            } else {
                Line::from(base + &search_indicator)
            }
        } else if self.search_enabled {
            Line::from(base + " [SEARCH]")
        } else {
            Line::from(base)
        };

        *self.cached_title.borrow_mut() = Some(title.clone());
        title
    }
}
