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
        if area.is_empty() {
            return;
        }
        let (screen_rows, screen_cols) = self.screen.size();
        let rows = area.height.min(screen_rows);
        let cols = area.width.min(screen_cols);
        let (cursor_row, cursor_col) = self.screen.cursor_position();
        // A screen taller than the pane shows the rows up to the cursor, where output goes
        let row_offset = (cursor_row + 1)
            .saturating_sub(area.height)
            .min(screen_rows.saturating_sub(area.height));

        for row in 0..rows {
            for col in 0..cols {
                let cell = self.screen.cell(row + row_offset, col);
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
            let cx = area.x + cursor_col;
            let cy = area.y + (cursor_row - row_offset);
            if cx < area.right()
                && cy < area.bottom()
                && let Some(cell) = buf.cell_mut((cx, cy))
            {
                cell.set_style(Style::default().fg(Color::Black).bg(Color::White));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    use super::PseudoTerminal;

    /// The first `rows` rows drawn from a 10-row screen that received `output`
    fn draw(output: &str, rows: u16) -> Vec<String> {
        let mut parser = vt100::Parser::new(10, 20, 0);
        parser.process(output.as_bytes());
        let area = Rect::new(0, 0, 20, rows);
        let mut buf = Buffer::empty(area);
        PseudoTerminal::new(parser.screen()).render(area, &mut buf);
        (0..rows)
            .map(|y| {
                let row: String = (0..20).map(|x| buf[(x, y)].symbol()).collect();
                row.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn short_pane_shows_newest_rows() {
        let output = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\r\n");
        assert_eq!(
            draw(&output, 5),
            ["line6", "line7", "line8", "line9", "line10"]
        );
    }

    #[test]
    fn short_pane_keeps_short_output_at_top() {
        assert_eq!(
            draw("line1\r\nline2\r\n", 5),
            ["line1", "line2", "", "", ""]
        );
    }
}
