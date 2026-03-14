use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::theme;

/// Background color for overlay panels (context menu, help dialog, etc.)
pub const OVERLAY_BG: Color = Color::Rgb(30, 30, 30);

/// Dim the entire area by overwriting fg colors with `DarkGray` and removing bold/underline.
pub fn dim_background(buf: &mut Buffer, area: Rect) {
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .remove_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    for row in area.y..area.bottom() {
        for col in area.x..area.right() {
            buf[(col, row)].set_style(dim_style);
        }
    }
}

/// Clear a rectangular area with the overlay background and draw a border around it.
pub fn draw_bordered_panel(buf: &mut Buffer, rect: Rect) {
    // Clear background
    let reset_style = Style::reset().bg(OVERLAY_BG).fg(Color::White);
    for row in rect.y..rect.bottom() {
        for col in rect.x..rect.right() {
            buf[(col, row)].set_style(reset_style).set_symbol(" ");
        }
    }

    // Border
    let border_style = Style::reset().fg(theme::ACCENT).bg(OVERLAY_BG);
    for col in rect.x..rect.right() {
        buf[(col, rect.y)].set_style(border_style).set_symbol("─");
        buf[(col, rect.bottom() - 1)]
            .set_style(border_style)
            .set_symbol("─");
    }
    for row in rect.y..rect.bottom() {
        buf[(rect.x, row)].set_style(border_style).set_symbol("│");
        buf[(rect.right() - 1, row)]
            .set_style(border_style)
            .set_symbol("│");
    }
    buf[(rect.x, rect.y)]
        .set_style(border_style)
        .set_symbol("┌");
    buf[(rect.right() - 1, rect.y)]
        .set_style(border_style)
        .set_symbol("┐");
    buf[(rect.x, rect.bottom() - 1)]
        .set_style(border_style)
        .set_symbol("└");
    buf[(rect.right() - 1, rect.bottom() - 1)]
        .set_style(border_style)
        .set_symbol("┘");
}
