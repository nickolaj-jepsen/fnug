//! The help overlay, laid out from the keymap: two columns where it fits, else one that
//! scrolls.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::keymap::{KEYMAP, KeyHelp};
use super::overlay::{OVERLAY_BG, dim_background, draw_bordered_panel};
use crate::theme;

const KEY_WIDTH: usize = 12;
const DESC_WIDTH: usize = 24;
const COLUMN_GAP: usize = 2;
/// Narrower popups get one column
const MIN_TWO_COLUMN_WIDTH: u16 = 60;

enum Row {
    Heading(&'static str),
    Key(&'static KeyHelp),
}

/// The keymap as rows: each context's heading, then its keys.
fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    let mut context = None;
    for key in KEYMAP {
        if context != Some(key.context) {
            context = Some(key.context);
            rows.push(Row::Heading(key.context.title()));
        }
        rows.push(Row::Key(key));
    }
    rows
}

/// Where the second column starts: the heading that balances the columns best.
fn column_split(rows: &[Row]) -> usize {
    let len = rows.len();
    rows.iter()
        .enumerate()
        .filter(|(i, row)| *i > 0 && matches!(row, Row::Heading(_)))
        .map(|(i, _)| i)
        .min_by_key(|&i| i.max(len - i))
        .unwrap_or(len.div_ceil(2))
}

/// `text` cut to `width` characters and padded to it
fn fit(text: &str, width: usize) -> String {
    let cut: String = text.chars().take(width).collect();
    format!("{cut:<width$}")
}

/// A row in a column `width` wide
fn cell(row: Option<&Row>, width: usize) -> Vec<Span<'static>> {
    let key_style = Style::reset()
        .fg(theme::ACCENT)
        .bg(OVERLAY_BG)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::reset().fg(Color::White).bg(OVERLAY_BG);
    let heading_style = key_style.add_modifier(Modifier::UNDERLINED);
    let key_width = KEY_WIDTH.min(width);
    match row {
        Some(Row::Heading(title)) => vec![
            Span::styled(fit(title, title.chars().count().min(width)), heading_style),
            Span::styled(
                " ".repeat(width.saturating_sub(title.chars().count())),
                desc_style,
            ),
        ],
        Some(Row::Key(key)) => vec![
            Span::styled(fit(key.keys, key_width), key_style),
            Span::styled(fit(key.desc, width - key_width), desc_style),
        ],
        None => vec![Span::styled(" ".repeat(width), desc_style)],
    }
}

/// The overlay's lines for an inner area `width` columns wide.
fn lines(width: usize, two_columns: bool) -> Vec<Line<'static>> {
    let rows = rows();
    if !two_columns {
        return rows
            .iter()
            .map(|row| Line::from(cell(Some(row), width)))
            .collect();
    }
    let split = column_split(&rows);
    let (left, right) = rows.split_at(split);
    let column = (width - COLUMN_GAP) / 2;
    (0..left.len().max(right.len()))
        .map(|i| {
            let mut spans = cell(left.get(i), column);
            spans.push(Span::styled(
                " ".repeat(width - 2 * column),
                Style::reset().bg(OVERLAY_BG),
            ));
            spans.extend(cell(right.get(i), column));
            Line::from(spans)
        })
        .collect()
}

/// Draw the help over the whole of `area`, scrolled down `scroll` lines. Returns `scroll`
/// clamped to what can scroll, which is 0 when everything fits.
pub(super) fn render_help(buf: &mut Buffer, area: Rect, scroll: usize) -> usize {
    dim_background(buf, area);
    let max_width = area.width.saturating_sub(4);
    let max_height = area.height.saturating_sub(2);
    let two_columns = max_width >= MIN_TWO_COLUMN_WIDTH;
    let content_width = if two_columns {
        2 * (KEY_WIDTH + DESC_WIDTH) + COLUMN_GAP
    } else {
        KEY_WIDTH + DESC_WIDTH
    };
    let width = u16::try_from(content_width + 2)
        .unwrap_or(u16::MAX)
        .min(max_width);
    let inner_width = usize::from(width.saturating_sub(2));
    let lines = lines(inner_width, two_columns);
    let height = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(max_height);
    if width < 4 || height < 3 {
        return 0;
    }
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    draw_bordered_panel(buf, popup);

    let inner_height = usize::from(height - 2);
    let scroll = scroll.min(lines.len().saturating_sub(inner_height));
    let title_style = Style::reset()
        .fg(theme::ACCENT)
        .bg(OVERLAY_BG)
        .add_modifier(Modifier::BOLD);
    border_label(buf, popup, popup.y, " Keybindings ", title_style);
    if lines.len() > inner_height {
        let hint_style = Style::reset().fg(Color::DarkGray).bg(OVERLAY_BG);
        border_label(buf, popup, popup.bottom() - 1, " j/k scroll ", hint_style);
    }
    for (i, line) in lines.iter().skip(scroll).take(inner_height).enumerate() {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "bounded by the popup height"
        )]
        let y = popup.y + 1 + i as u16;
        buf.set_line(popup.x + 1, y, line, width - 2);
    }
    scroll
}

/// `label` centred on the border row `y` of `popup`, if it fits between the corners.
fn border_label(buf: &mut Buffer, popup: Rect, y: u16, label: &str, style: Style) {
    let len = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
    if len + 2 > popup.width {
        return;
    }
    let x = popup.x + (popup.width - len) / 2;
    buf.set_string(x, y, label, style);
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use crate::tui::keymap::KEYMAP;
    use crate::tui::test_util::{draw, press, two_groups};

    #[test]
    fn help_80x24() {
        let mut app = two_groups();
        press(&mut app, KeyCode::Char('?'));
        let (rows, _) = draw(&mut app, 80, 24);
        let screen = rows.join("\n");

        for key in KEYMAP {
            assert!(
                screen.contains(key.desc),
                "{:?} missing:\n{screen}",
                key.desc
            );
        }
        assert!(!screen.contains("j/k scroll"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn help_tiny_no_panic() {
        for (width, height) in [(5, 4), (40, 3), (1, 1), (6, 5)] {
            let mut app = two_groups();
            press(&mut app, KeyCode::Char('?'));
            draw(&mut app, width, height);
        }
    }

    #[test]
    fn narrow_help_scrolls() {
        let mut app = two_groups();
        press(&mut app, KeyCode::Char('?'));
        let (rows, _) = draw(&mut app, 50, 16);
        let screen = rows.join("\n");
        assert!(screen.contains("Move down"), "{screen}");
        assert!(screen.contains("j/k scroll"), "{screen}");
        assert!(!screen.contains("Exit fullscreen"), "{screen}");

        for _ in 0..100 {
            press(&mut app, KeyCode::Char('j'));
        }
        let (rows, _) = draw(&mut app, 50, 16);
        let screen = rows.join("\n");
        assert!(screen.contains("Exit fullscreen"), "{screen}");
        assert!(!screen.contains("Move down"), "{screen}");
        assert!(app.show_help);

        // Scrolled no further than the end, so one press moves back up at once
        press(&mut app, KeyCode::Char('k'));
        let (rows, _) = draw(&mut app, 50, 16);
        assert!(!rows.join("\n").contains("Exit fullscreen"));
    }
}
