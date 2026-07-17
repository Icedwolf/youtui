use super::{WindowContext, YoutuiWindow, footer, header};
use crate::app::view::draw::{DrawTableConfig, draw_panel_mut_impl, draw_table_impl};
use crate::app::view::{BasicConstraint, DrawableMut};
use crate::drawutils::{SELECTED_BORDER_COLOUR, TEXT_COLOUR, left_bottom_corner_rect};
use crate::keyaction::{DisplayableKeyAction, DisplayableMode};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear, Row, Table};
pub fn draw_app(f: &mut Frame, w: &mut YoutuiWindow) {
    let [header_chunk, window_chunk, footer_chunk] = Layout::default()
        .direction(Direction::Vertical)
        .margin(0)
        .constraints([
            Constraint::Length(header::header_required_height(w)),
            Constraint::Min(2),
            Constraint::Length(5),
        ])
        .areas(f.area());
    header::draw_header(f, w, header_chunk);
    let context_selected = !w.help.shown && !w.key_pending();
    match w.context {
        WindowContext::Browser => {
            w.browser
                .draw_mut_chunk(f, window_chunk, context_selected, w.tick);
        }
        WindowContext::Playlist => {
            w.playlist
                .draw_mut_chunk(f, window_chunk, context_selected, w.tick);
        }
    }
    if w.help.shown {
        draw_help(f, w, window_chunk);
    }
    if w.key_pending() {
        draw_popup(f, w, window_chunk);
    }
    footer::draw_footer(f, w, footer_chunk);
}

fn draw_popup(f: &mut Frame, w: &YoutuiWindow, chunk: Rect) {
    // NOTE: if there are more commands than we can fit on the screen, some will be
    // cut off. If there are no commands, no need to draw anything.
    let Some(DisplayableMode {
        displayable_commands: commands,
        description: title,
    }) = w.get_cur_displayable_mode()
    else {
        return;
    };
    let shortcuts_descriptions = commands.collect::<Vec<_>>();
    // TODO: Make commands_vec an iterator instead of a vec
    let (shortcut_len, description_len, commands_vec) = shortcuts_descriptions.iter().fold(
        (0, 0, Vec::new()),
        |(acc1, acc2, mut commands_vec),
         DisplayableKeyAction {
             keybinds,
             context: _,
             description,
         }| {
            commands_vec.push(
                Row::new(vec![format!("{}", keybinds), format!("{}", description)])
                    .style(Style::new().fg(TEXT_COLOUR)),
            );
            (
                keybinds.len().max(acc1),
                description.len().max(acc2),
                commands_vec,
            )
        },
    );
    let width = shortcut_len
        .saturating_add(description_len)
        .saturating_add(3);
    let height = commands_vec.len().saturating_add(2);
    let table_constraints = [
        Constraint::Min(shortcut_len.try_into().unwrap_or(u16::MAX)),
        Constraint::Min(description_len.try_into().unwrap_or(u16::MAX)),
    ];
    let block = Table::new(commands_vec, table_constraints).block(
        Block::default()
            .title(title.as_ref())
            .borders(Borders::ALL)
            .style(Style::new().fg(SELECTED_BORDER_COLOUR)),
    );
    let area = left_bottom_corner_rect(
        height.try_into().unwrap_or(u16::MAX),
        width.try_into().unwrap_or(u16::MAX),
        chunk,
    );
    f.render_widget(Clear, area);
    f.render_widget(block, area);
}

/// Draw the help page. The help page should show all visible commands for the
/// current page.
/// Draw the help page. The help page should show all visible commands for the
/// current page.
fn draw_help(f: &mut Frame, w: &mut YoutuiWindow, chunk: Rect) {
    let mut s_len = 0usize;
    let mut c_len = 0usize;
    let mut d_len = 0usize;
    let mut items = 0usize;
    for action in w.get_help_list_items() {
        items = items.saturating_add(1);
        s_len = s_len.max(action.keybinds.len());
        c_len = c_len.max(action.context.len());
        d_len = d_len.max(action.description.len());
    }
    // Ensure the width of each column is at least as wide as header.
    (s_len, c_len, d_len) = (s_len.max(3), c_len.max(7), d_len.max(7));
    // Total block width required, including padding and borders.
    let width = s_len
        .saturating_add(c_len)
        .saturating_add(d_len)
        .saturating_add(4);
    // Total block height required, including header and borders.
    let height = items.saturating_add(3);
    let table_constraints = [
        BasicConstraint::Length(s_len.try_into().unwrap_or(u16::MAX)),
        BasicConstraint::Length(c_len.try_into().unwrap_or(u16::MAX)),
        BasicConstraint::Length(d_len.try_into().unwrap_or(u16::MAX)),
    ];
    let headings = ["Key", "Context", "Command"].into_iter();
    let area = left_bottom_corner_rect(
        height.try_into().unwrap_or(u16::MAX),
        width.try_into().unwrap_or(u16::MAX),
        chunk,
    );
    f.render_widget(Clear, area);
    let cur_tick = w.tick;
    draw_panel_mut_impl(
        f,
        w,
        area,
        true,
        |_| "Help".into(),
        |t, f, chunk| {
            let commands_table = t.get_help_list_items().map(
                |DisplayableKeyAction {
                     keybinds,
                     context,
                     description,
                 }| { [keybinds, context, description].into_iter() },
            );
            let (new_state, effect) = draw_table_impl(
                f,
                chunk,
                DrawTableConfig {
                    cur: t.help.cur,
                    secondary_highlighted_row: None,
                    state: &t.help.widget_state,
                    len: items,
                    layout: &table_constraints,
                    footer: None,
                    cur_tick,
                },
                commands_table,
                headings,
            );
            t.help.widget_state = new_state;
            Some(effect)
        },
    );
}
