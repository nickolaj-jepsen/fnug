use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// Map a vt100 color to a ratatui color
fn map_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Widget that renders a `vt100::Screen` into a ratatui buffer
pub struct PseudoTerminal<'a> {
    screen: &'a vt100::Screen,
}

impl<'a> PseudoTerminal<'a> {
    #[must_use]
    pub fn new(screen: &'a vt100::Screen) -> Self {
        Self { screen }
    }
}

impl Widget for PseudoTerminal<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rows = area.height.min(self.screen.size().0);
        let cols = area.width.min(self.screen.size().1);

        for row in 0..rows {
            for col in 0..cols {
                let cell = self.screen.cell(row, col);
                if let Some(cell) = cell {
                    let x = area.x + col;
                    let y = area.y + row;

                    if x >= area.right() || y >= area.bottom() {
                        continue;
                    }

                    let Some(buf_cell) = buf.cell_mut((x, y)) else {
                        continue;
                    };

                    let ch = cell.contents();
                    if ch.is_empty() {
                        buf_cell.set_char(' ');
                    } else {
                        // Set the first char; for wide chars this handles the main cell
                        let mut chars = ch.chars();
                        if let Some(c) = chars.next() {
                            buf_cell.set_char(c);
                        }
                    }

                    let mut modifier = Modifier::empty();
                    if cell.bold() {
                        modifier |= Modifier::BOLD;
                    }
                    if cell.italic() {
                        modifier |= Modifier::ITALIC;
                    }
                    if cell.underline() {
                        modifier |= Modifier::UNDERLINED;
                    }
                    if cell.inverse() {
                        modifier |= Modifier::REVERSED;
                    }

                    buf_cell.set_style(
                        Style::default()
                            .fg(map_color(cell.fgcolor()))
                            .bg(map_color(cell.bgcolor()))
                            .add_modifier(modifier),
                    );
                }
            }
        }

        // Render cursor
        if !self.screen.hide_cursor() {
            let (cursor_row, cursor_col) = self.screen.cursor_position();
            let cx = area.x + cursor_col;
            let cy = area.y + cursor_row;
            if cx < area.right()
                && cy < area.bottom()
                && let Some(cell) = buf.cell_mut((cx, cy))
            {
                cell.set_style(Style::default().fg(Color::Black).bg(Color::White));
            }
        }
    }
}
