use log::Level;
use ratatui::style::Color;

pub use crate::logger::{LogBuffer, LogEntry};

/// Map a log level to a ratatui color for display.
#[must_use]
pub fn level_color(level: Level) -> Color {
    match level {
        Level::Error => crate::theme::FAILURE,
        Level::Warn => Color::Yellow,
        Level::Info => Color::Blue,
        Level::Debug | Level::Trace => Color::DarkGray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_color_mapping() {
        assert_eq!(level_color(Level::Error), crate::theme::FAILURE);
        assert_eq!(level_color(Level::Warn), Color::Yellow);
        assert_eq!(level_color(Level::Info), Color::Blue);
        assert_eq!(level_color(Level::Debug), Color::DarkGray);
        assert_eq!(level_color(Level::Trace), Color::DarkGray);
    }
}
