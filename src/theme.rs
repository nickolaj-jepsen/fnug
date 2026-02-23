use ratatui::style::Color;

pub const ACCENT: Color = Color::Rgb(207, 106, 76);
pub const SUCCESS: Color = Color::Green;
pub const RUNNING: Color = Color::Yellow;
pub const FAILURE: Color = Color::Red;
pub const DIM: Color = Color::DarkGray;
pub const TREE: Color = Color::Gray;

pub const TOOLBAR_BG: Color = Color::Rgb(207, 106, 76);
pub const TOOLBAR_KEY_BG: Color = Color::Rgb(40, 40, 40);
pub const TOOLBAR_KEY_FG: Color = Color::Rgb(207, 106, 76);
pub const TOOLBAR_DESC: Color = Color::Black;

/// Raw RGB tuple for use with anstyle (PTY messages)
pub const ACCENT_RGB: (u8, u8, u8) = (207, 106, 76);
