use rat_text::HasScreenCursor;
use rat_text::text_input::TextInput;
use rat_text::text_input::TextInputState;
use ratatui::Frame;
use ratatui::prelude::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};

// Standard app colour scheme
pub const SELECTED_BORDER_COLOUR: Color = Color::Cyan;
pub const DESELECTED_BORDER_COLOUR: Color = Color::Reset;
// TODO: Implement in all locations.
pub const TEXT_COLOUR: Color = Color::Reset;
pub const BUTTON_BG_COLOUR: Color = Color::Gray;
pub const BUTTON_FG_COLOUR: Color = Color::Black;
pub const PROGRESS_BG_COLOUR: Color = Color::DarkGray;
pub const PROGRESS_FG_COLOUR: Color = Color::LightGreen;
pub const TABLE_HEADINGS_COLOUR: Color = Color::LightGreen;
pub const ROW_HIGHLIGHT_COLOUR: Color = Color::Blue;

/// Draw a text input box
pub fn draw_text_box(
    f: &mut Frame,
    title: impl AsRef<str>,
    contents: &mut TextInputState,
    chunk: Rect,
) {
    let block_widget = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(SELECTED_BORDER_COLOUR))
        .title(title.as_ref());
    let text_chunk = block_widget.inner(chunk);
    let text_chunk = Rect {
        x: text_chunk.x,
        y: text_chunk.y,
        width: text_chunk.width.saturating_sub(1),
        height: text_chunk.height,
    };
    let text_widget = TextInput::new();
    f.render_widget(block_widget, chunk);
    f.render_stateful_widget(text_widget, text_chunk, contents);
    if let Some(cursor_pos) = contents.screen_cursor() {
        f.set_cursor_position(cursor_pos)
    };
}

/// Helper function to create a popup at bottom corner of chunk.
pub fn left_bottom_corner_rect(height: u16, width: u16, r: Rect) -> Rect {
    let r_x2 = r.x.saturating_add(r.width);
    let r_y2 = r.y.saturating_add(r.height);
    let x = r_x2.saturating_sub(width).max(r.x);
    let y = r_y2.saturating_sub(height).max(r.y);
    Rect {
        x,
        y,
        width: width.min(r_x2.saturating_sub(x)),
        height: height.min(r_y2.saturating_sub(y)),
    }
}
/// Helper function to create a popup below a chunk.
//  We pass in the max bounds that can be rendered by the application,
//  to avoid returning a Rect that is not drawable.
// TODO: Add a test to ensure this is returning correct area
pub fn below_left_rect(height: u16, width: u16, r: Rect, max_bounds: Rect) -> Rect {
    let y = r.y.saturating_add(r.height.saturating_sub(1));
    Rect {
        x: r.x,
        y,
        width: width.min(max_bounds.right().saturating_sub(r.x)),
        height: (height.saturating_add(1)).min(max_bounds.bottom().saturating_sub(y)),
    }
}
/// Helper function to create a popup in the center of a chunk.
pub fn centered_rect(height: u16, width: u16, r: Rect) -> Rect {
    Rect {
        x: (r.x + r.width / 2).saturating_sub(width / 2).max(r.x),
        y: (r.y + r.height / 2).saturating_sub(height / 2).max(r.y),
        width: width.min(r.width),
        height: height.min(r.height),
    }
}
/// Helper function to get the bottom line of a chunk, ignoring side borders.
pub fn bottom_of_rect(r: Rect) -> Rect {
    Rect {
        x: r.x.saturating_add(1),
        y: r.y.saturating_add(r.height.saturating_sub(1)),
        width: r.width.saturating_sub(2),
        height: 1,
    }
}
pub fn get_offset_after_list_resize(
    prev_offset: usize,
    prev_cur: usize,
    prev_max_cur: usize,
    new_cur: usize,
    new_max_cur: usize,
) -> usize {
    // Calculate previous offset relative to the previous cur (as a signed int),
    // defaulting to zero if any issues with cast identified.
    let prev_offset_rel_cur = isize::try_from(prev_offset)
        .map(|prev_offset| prev_offset.saturating_sub_unsigned(prev_cur))
        .unwrap_or(0);
    // Calculate previous offset relative to the previous max cur (as a signed int),
    // defaulting to zero if any issues with cast required.
    let prev_offset_rel_max = isize::try_from(prev_offset)
        .map(|prev_offset| prev_offset.saturating_sub_unsigned(prev_max_cur))
        .unwrap_or(0);
    // Adjust offset accordingly to ensure the offset relative to cur is the same as
    // it was previously.
    let Ok(new_cur_isize) = isize::try_from(new_cur) else {
        return 0;
    };
    let Ok(new_max_cur_isize) = isize::try_from(new_max_cur) else {
        return 0;
    };
    let new_offset_using_rel_cur = new_cur_isize + prev_offset_rel_cur;
    let new_offset_using_rel_max = new_max_cur_isize + prev_offset_rel_max;
    let new_offset: usize = ((new_offset_using_rel_max + new_offset_using_rel_cur) / 2)
        .try_into()
        .unwrap_or(0);
    new_offset
}

#[cfg(test)]
mod tests {
    use super::{below_left_rect, bottom_of_rect, centered_rect, left_bottom_corner_rect};
    use crate::drawutils::get_offset_after_list_resize;
    use ratatui::layout::Rect;

    #[test]
    fn test_get_offset_after_list_resize_prev_upper_list() {
        let new_offset = get_offset_after_list_resize(30, 40, 50, 10, 10);
        assert_eq!(new_offset, 0);
    }
    #[test]
    fn test_get_offset_after_list_resize_prev_lower_list() {
        let new_offset = get_offset_after_list_resize(20, 40, 40, 10, 10);
        assert_eq!(new_offset, 0);
    }
    #[test]
    fn test_get_offset_after_list_resize_prev_no_change() {
        let prev_offset = 30;
        let new_offset = get_offset_after_list_resize(prev_offset, 40, 50, 40, 50);
        assert_eq!(prev_offset, new_offset);
    }
    fn bounds_check_rect(r: Rect, max_bounds: Rect) {
        assert!(r.left() >= max_bounds.left());
        assert!(r.right() <= max_bounds.right());
        assert!(r.bottom() <= max_bounds.bottom());
        assert!(r.top() >= max_bounds.top());
    }
    #[test]
    #[should_panic]
    fn test_bounds_check_rect() {
        // TODO: Rect constructor may make this neater.
        let r1 = Rect {
            x: 0,
            y: 0,
            height: 50,
            width: 50,
        };
        let m1 = Rect {
            x: 0,
            y: 50,
            height: 50,
            width: 50,
        };
        let r2 = Rect {
            x: 30,
            y: 30,
            height: 50,
            width: 50,
        };
        let m2 = Rect {
            x: 30,
            y: 30,
            height: 51,
            width: 51,
        };
        let r3 = Rect {
            x: 30,
            y: 30,
            height: 50,
            width: 50,
        };
        let m3 = Rect {
            x: 30,
            y: 30,
            height: 51,
            width: 50,
        };
        let r4 = Rect {
            x: 30,
            y: 30,
            height: 50,
            width: 50,
        };
        let m4 = Rect {
            x: 30,
            y: 30,
            height: 50,
            width: 51,
        };
        let r5 = Rect {
            x: 30,
            y: 30,
            height: 50,
            width: 50,
        };
        let m5 = Rect {
            x: 31,
            y: 31,
            height: 50,
            width: 50,
        };
        bounds_check_rect(r1, m1);
        bounds_check_rect(r2, m2);
        bounds_check_rect(r3, m3);
        bounds_check_rect(r4, m4);
        bounds_check_rect(r5, m5);
    }
    // These don't actually do anything as they don't try to draw...
    #[test]
    fn test_centered_rect_zero_height() {
        let chunk = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let c = centered_rect(0, 5, chunk);
        assert_eq!(c.width, 5);
        assert_eq!(c.height, 0);
    }
    #[test]
    fn test_centered_rect_larger_than_chunk() {
        let chunk = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let c = centered_rect(100, 100, chunk);
        assert_eq!(c.width, 10);
        assert_eq!(c.height, 10);
    }
    #[test]
    fn test_left_bottom_corner_rect_larger_than_chunk() {
        let chunk = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let c = left_bottom_corner_rect(100, 100, chunk);
        assert_eq!(c.width, 10);
        assert_eq!(c.height, 10);
    }
    #[test]
    fn bounds_check_left_bottom_corner_rect() {
        left_bottom_corner_rect(
            u16::MAX,
            u16::MAX,
            Rect {
                x: 0,
                y: 0,
                height: 50,
                width: 50,
            },
        );
        left_bottom_corner_rect(
            u16::MAX,
            u16::MAX,
            Rect {
                x: 0,
                y: 50,
                height: 50,
                width: 50,
            },
        );
        left_bottom_corner_rect(
            u16::MAX,
            u16::MAX,
            Rect {
                x: 50,
                y: 0,
                height: 50,
                width: 50,
            },
        );
        left_bottom_corner_rect(
            u16::MAX,
            u16::MAX,
            Rect {
                x: 50,
                y: 50,
                height: 50,
                width: 50,
            },
        );
    }

    #[test]
    fn bounds_check_centered_rect() {
        let t_r1 = Rect {
            x: 0,
            y: 0,
            height: 50,
            width: 50,
        };
        let t_r2 = Rect {
            x: 0,
            y: 50,
            height: 50,
            width: 50,
        };
        let t_r3 = Rect {
            x: 50,
            y: 0,
            height: 50,
            width: 50,
        };
        let t_r4 = Rect {
            x: 50,
            y: 50,
            height: 50,
            width: 50,
        };
        let r1 = centered_rect(u16::MAX, u16::MAX, t_r1);
        let r2 = centered_rect(u16::MAX, u16::MAX, t_r2);
        let r3 = centered_rect(u16::MAX, u16::MAX, t_r3);
        let r4 = centered_rect(u16::MAX, u16::MAX, t_r4);
        // Unsure if these are correct of there is a better way to check.
        // TODO: Add a bounds check rect function.
        bounds_check_rect(r1, t_r1);
        bounds_check_rect(r2, t_r2);
        bounds_check_rect(r3, t_r3);
        bounds_check_rect(r4, t_r4);
    }
    #[test]
    fn test_bottom_of_rect_normal() {
        let r = Rect {
            x: 5,
            y: 10,
            width: 20,
            height: 5,
        };
        let b = bottom_of_rect(r);
        assert_eq!(b.x, 6);
        assert_eq!(b.y, 14);
        assert_eq!(b.width, 18);
        assert_eq!(b.height, 1);
    }
    #[test]
    fn test_bottom_of_rect_zero_height() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 0,
        };
        let b = bottom_of_rect(r);
        assert_eq!(b.y, 0);
        assert_eq!(b.width, 8);
        assert_eq!(b.height, 1);
    }
    #[test]
    fn test_bottom_of_rect_zero_width() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 10,
        };
        let b = bottom_of_rect(r);
        assert_eq!(b.x, 1);
        assert_eq!(b.y, 9);
        assert_eq!(b.width, 0);
        assert_eq!(b.height, 1);
    }
    #[test]
    fn test_below_left_rect_zero_height() {
        let chunk = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 0,
        };
        let max = Rect {
            x: 0,
            y: 0,
            width: 50,
            height: 50,
        };
        // height 0 chunk: y = 0 + 0 - 1 = 0 (saturated)
        let b = below_left_rect(5, 10, chunk, max);
        assert_eq!(b.x, 0);
        assert_eq!(b.y, 0);
        assert_eq!(b.width, 10);
        assert!(b.height <= max.height);
    }
    #[test]
    fn test_below_left_rect_normal() {
        // below_left_rect adds 1 to height internally
        let chunk = Rect {
            x: 5,
            y: 10,
            width: 20,
            height: 5,
        };
        let max = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let r = below_left_rect(10, 15, chunk, max);
        assert_eq!(r.x, 5);
        assert_eq!(r.y, 14);
        assert_eq!(r.width, 15);
        assert_eq!(r.height, 11);
        bounds_check_rect(r, max);
    }

    #[test]
    fn test_below_left_rect_clamped_to_max() {
        let chunk = Rect {
            x: 50,
            y: 90,
            width: 20,
            height: 5,
        };
        let max = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let r = below_left_rect(20, 30, chunk, max);
        assert_eq!(r.x, 50);
        assert_eq!(r.y, 94);
        assert_eq!(r.width, 30);
        assert_eq!(r.height, 6);
        bounds_check_rect(r, max);
    }

    #[test]
    fn test_below_left_rect_width_clamped_right() {
        let chunk = Rect {
            x: 85,
            y: 10,
            width: 20,
            height: 5,
        };
        let max = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let r = below_left_rect(10, 30, chunk, max);
        assert_eq!(r.x, 85);
        assert_eq!(r.y, 14);
        assert_eq!(r.width, 15);
        assert_eq!(r.height, 11);
        bounds_check_rect(r, max);
    }

    #[test]
    fn test_below_left_rect_zero_height_chunk() {
        let chunk = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 0,
        };
        let max = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let r = below_left_rect(5, 10, chunk, max);
        assert_eq!(r.y, 0);
        assert_eq!(r.height, 6);
        bounds_check_rect(r, max);
    }

    #[test]
    fn bounds_check_below_left_rect_no_panic() {
        // Verify no panics with extreme values
        let cases = [
            (
                u16::MAX,
                u16::MAX,
                Rect {
                    x: 0,
                    y: 0,
                    height: 50,
                    width: 50,
                },
                Rect {
                    x: 100,
                    y: 100,
                    height: 1050,
                    width: 1050,
                },
            ),
            (
                u16::MAX,
                u16::MAX,
                Rect {
                    x: 0,
                    y: 50,
                    height: 50,
                    width: 50,
                },
                Rect {
                    x: 100,
                    y: 1050,
                    height: 1050,
                    width: 1050,
                },
            ),
            (
                u16::MAX,
                u16::MAX,
                Rect {
                    x: 50,
                    y: 0,
                    height: 50,
                    width: 50,
                },
                Rect {
                    x: 1050,
                    y: 100,
                    height: 1050,
                    width: 1050,
                },
            ),
            (
                u16::MAX,
                u16::MAX,
                Rect {
                    x: 50,
                    y: 50,
                    height: 50,
                    width: 50,
                },
                Rect {
                    x: 1050,
                    y: 1050,
                    height: 1050,
                    width: 1050,
                },
            ),
        ];
        for (h, w, chunk, max) in &cases {
            let r = below_left_rect(*h, *w, *chunk, *max);
            assert_eq!(r.x, chunk.x, "below_left_rect x must match chunk x");
            assert!(r.y >= chunk.y + chunk.height.saturating_sub(1));
        }
    }
}
