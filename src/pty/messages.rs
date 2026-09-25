use crate::process::ExitInfo;
use crate::theme;
use anstyle::{AnsiColor, Reset, RgbColor, Style};

const PRIMARY_COLOR: Style = Style::new().fg_color(Some(anstyle::Color::Rgb(RgbColor(
    theme::ACCENT_RGB.0,
    theme::ACCENT_RGB.1,
    theme::ACCENT_RGB.2,
))));
const SUCCESS_COLOR: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
const ERROR_COLOR: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)));
const STOPPED_COLOR: Style =
    Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::BrightBlack)));

/// Prefix of every line fnug writes into a command's terminal
const BANNER_PREFIX: &str = "❱ ";

fn render_arrow() -> String {
    format!("{PRIMARY_COLOR}❱{Reset}")
}

fn render_success() -> String {
    format!("{SUCCESS_COLOR}✓{Reset}")
}

fn render_error() -> String {
    format!("{ERROR_COLOR}✘{Reset}")
}

fn render_stopped() -> String {
    format!("{STOPPED_COLOR}■{Reset}")
}

#[must_use]
pub fn format_start_message(command: &str) -> Vec<u8> {
    format!("{} {}\r\n\r\n", render_arrow(), command).into()
}

/// Banner written after a command exits: succeeded, stopped, or failed with its exit code or signal.
#[must_use]
pub fn format_exit_message(exit: &ExitInfo) -> Vec<u8> {
    let arrow = render_arrow();
    if exit.stop_requested {
        format!("\r\n{arrow} Command stopped {}\r\n", render_stopped())
    } else if exit.success() {
        format!("\r\n{arrow} Command succeeded {}\r\n", render_success())
    } else {
        format!(
            "\r\n{arrow} Command failed {} ({})\r\n",
            render_error(),
            exit.describe()
        )
    }
    .into()
}

/// Banner written when the terminal emulator crashed and was replaced by a blank one.
#[must_use]
pub fn format_emulator_reset_message() -> Vec<u8> {
    format!(
        "{} Terminal emulator crashed {}; earlier output was lost\r\n",
        render_arrow(),
        render_error()
    )
    .into()
}

/// Whether a line of screen text is one of fnug's own banners rather than command output.
#[must_use]
pub fn is_banner_line(line: &str) -> bool {
    line.starts_with(BANNER_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::{format_exit_message, format_start_message, is_banner_line};
    use crate::process::ExitInfo;

    fn screen_text(bytes: &[u8]) -> String {
        let mut parser = vt100::Parser::new(5, 80, 0);
        parser.process(bytes);
        parser.screen().contents().trim().to_string()
    }

    fn exit(code: Option<i32>, signal: Option<i32>, stop_requested: bool) -> ExitInfo {
        ExitInfo {
            code,
            signal,
            stop_requested,
        }
    }

    #[test]
    fn format_exit_message_variants() {
        let text = |e: ExitInfo| screen_text(&format_exit_message(&e));
        assert_eq!(text(exit(Some(0), None, false)), "❱ Command succeeded ✓");
        assert_eq!(
            text(exit(Some(3), None, false)),
            "❱ Command failed ✘ (exit code 3)"
        );
        assert_eq!(
            text(exit(None, Some(libc::SIGSEGV), false)),
            "❱ Command failed ✘ (terminated by SIGSEGV)"
        );
        assert_eq!(
            text(exit(None, Some(libc::SIGINT), true)),
            "❱ Command stopped ■"
        );
    }

    #[test]
    fn banners_are_recognised() {
        let start = screen_text(&format_start_message("cargo test"));
        let exit = screen_text(&format_exit_message(&exit(Some(1), None, false)));
        assert!(is_banner_line(&start));
        assert!(is_banner_line(&exit));
        assert!(!is_banner_line("test result: ok ❱ 3 passed"));
    }
}
