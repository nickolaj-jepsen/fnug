use crate::theme;
use anstyle::{AnsiColor, Reset, RgbColor, Style};

const PRIMARY_COLOR: Style = Style::new().fg_color(Some(anstyle::Color::Rgb(RgbColor(
    theme::ACCENT_RGB.0,
    theme::ACCENT_RGB.1,
    theme::ACCENT_RGB.2,
))));
const SUCCESS_COLOR: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
const ERROR_COLOR: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)));

fn render_arrow() -> String {
    format!("{PRIMARY_COLOR}❱{Reset}")
}

fn render_success() -> String {
    format!("{SUCCESS_COLOR}✓{Reset}")
}

fn render_error() -> String {
    format!("{ERROR_COLOR}✘{Reset}")
}

#[must_use]
pub fn format_start_message(command: &str) -> Vec<u8> {
    format!("{} {}\r\n\r\n", render_arrow(), command).into()
}

#[must_use]
pub fn format_success_message() -> Vec<u8> {
    format!(
        "\r\n{} Command succeeded {}\r\n",
        render_arrow(),
        render_success()
    )
    .into()
}

#[must_use]
pub fn format_failure_message(exit_code: u32) -> Vec<u8> {
    format!(
        "\r\n{} Command failed {} (exit code {})\r\n",
        render_arrow(),
        render_error(),
        exit_code
    )
    .into()
}
