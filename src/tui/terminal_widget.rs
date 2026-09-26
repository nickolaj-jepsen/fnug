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

/// The text attributes of a vt100 cell as ratatui modifiers
fn modifiers(cell: &vt100::Cell) -> Modifier {
    [
        (cell.bold(), Modifier::BOLD),
        (cell.dim(), Modifier::DIM),
        (cell.italic(), Modifier::ITALIC),
        (cell.underline(), Modifier::UNDERLINED),
        (cell.inverse(), Modifier::REVERSED),
        (cell.strikethrough(), Modifier::CROSSED_OUT),
    ]
    .into_iter()
    .filter(|(set, _)| *set)
    .fold(Modifier::empty(), |all, (_, modifier)| all | modifier)
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

        // Reused for every cell, so a grapheme with combining marks needs no allocation
        let mut symbol = String::new();
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = self.screen.cell(row + row_offset, col) else {
                    continue;
                };
                // The wide character in the cell before covers it
                if cell.is_wide_continuation() {
                    continue;
                }
                let Some(buf_cell) = buf.cell_mut((area.x + col, area.y + row)) else {
                    continue;
                };

                symbol.clear();
                symbol.extend(cell.chars());
                if symbol.is_empty() {
                    buf_cell.set_char(' ');
                } else {
                    buf_cell.set_symbol(&symbol);
                }
                buf_cell.set_style(
                    Style::default()
                        .fg(map_color(cell.fgcolor()))
                        .bg(map_color(cell.bgcolor()))
                        .add_modifier(modifiers(cell)),
                );
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
    fn widget_renders_dim_strike_grapheme() {
        use ratatui::style::Modifier;

        let mut parser = vt100::Parser::new(2, 10, 0);
        parser.process("\x1b[2mD\x1b[0m\x1b[9mS\x1b[0me\u{301}中x".as_bytes());
        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::empty(area);
        PseudoTerminal::new(parser.screen()).render(area, &mut buf);

        assert!(buf[(0, 0)].modifier.contains(Modifier::DIM));
        assert!(!buf[(0, 0)].modifier.contains(Modifier::CROSSED_OUT));
        assert!(buf[(1, 0)].modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(buf[(2, 0)].symbol(), "e\u{301}");
        assert_eq!(buf[(3, 0)].symbol(), "中");
        assert_eq!(buf[(5, 0)].symbol(), "x");
    }

    #[test]
    fn short_pane_keeps_short_output_at_top() {
        assert_eq!(
            draw("line1\r\nline2\r\n", 5),
            ["line1", "line2", "", "", ""]
        );
    }
}
