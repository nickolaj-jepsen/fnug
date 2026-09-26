//! Every keybinding the help overlay, the toolbar and the README list, defined once. The
//! dispatch lives in `key_handler`; tests keep the two in step.

use std::borrow::Cow;
use std::fmt::Write;

/// Where a key works, which also groups the help overlay
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyContext {
    Navigate,
    Run,
    Output,
    Other,
    /// While typing a search
    Search,
    /// While a command has the keyboard
    Terminal,
    Fullscreen,
}

impl KeyContext {
    /// Heading in the help overlay
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            KeyContext::Navigate => "Navigate",
            KeyContext::Run => "Run",
            KeyContext::Output => "Output",
            KeyContext::Other => "Other",
            KeyContext::Search => "Searching",
            KeyContext::Terminal => "Typing into a command",
            KeyContext::Fullscreen => "Fullscreen",
        }
    }

    /// The README's "Context" column
    #[must_use]
    pub fn applies_in(self) -> &'static str {
        match self {
            KeyContext::Navigate | KeyContext::Run | KeyContext::Output | KeyContext::Other => {
                "Tree"
            }
            KeyContext::Search => "Search",
            KeyContext::Terminal => "Terminal",
            KeyContext::Fullscreen => "Fullscreen",
        }
    }
}

/// One line of help: the keys, alternatives separated by ` / `, and what they do
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyHelp {
    pub keys: &'static str,
    pub context: KeyContext,
    pub desc: &'static str,
}

const fn key(keys: &'static str, context: KeyContext, desc: &'static str) -> KeyHelp {
    KeyHelp {
        keys,
        context,
        desc,
    }
}

/// Every keybinding, grouped by context. Descriptions fit the help overlay's 24 columns.
pub const KEYMAP: &[KeyHelp] = &[
    key("j / ↓", KeyContext::Navigate, "Move down"),
    key("k / ↑", KeyContext::Navigate, "Move up"),
    key("h / ←", KeyContext::Navigate, "Collapse / deselect"),
    key("l / →", KeyContext::Navigate, "Expand / select"),
    key("Space", KeyContext::Navigate, "Toggle selection"),
    key("E", KeyContext::Navigate, "Expand all groups"),
    key("W", KeyContext::Navigate, "Collapse all groups"),
    key("/", KeyContext::Navigate, "Search and filter"),
    key("Enter", KeyContext::Run, "Run selected commands"),
    key("r", KeyContext::Run, "Run command or group"),
    key("s", KeyContext::Run, "Stop command"),
    key("x", KeyContext::Run, "Clear command"),
    key("g", KeyContext::Run, "Select by git changes"),
    key("c", KeyContext::Output, "Copy output"),
    key("Shift+↑/↓", KeyContext::Output, "Scroll output"),
    key("{ / }", KeyContext::Output, "Output top / bottom"),
    key("Tab", KeyContext::Output, "Type into the command"),
    key("Ctrl+R", KeyContext::Output, "Toggle fullscreen"),
    key("L", KeyContext::Output, "Toggle log panel"),
    key("?", KeyContext::Other, "Toggle this help"),
    key("q / Ctrl+C", KeyContext::Other, "Quit"),
    key("Enter", KeyContext::Search, "Keep the filter"),
    key("Esc", KeyContext::Search, "Clear the search"),
    key("Ctrl+]", KeyContext::Terminal, "Back to the tree"),
    key("Esc", KeyContext::Terminal, "Back, unless full-screen"),
    key("Esc", KeyContext::Fullscreen, "Exit fullscreen"),
];

/// A key as a toolbar badge: `Ctrl+R` becomes `^R`, named keys are upper case, and a single
/// character stays as it is, so `r` and `L` keep their case.
#[must_use]
pub fn badge(key: &'static str) -> Cow<'static, str> {
    if let Some(rest) = key.strip_prefix("Ctrl+") {
        Cow::Owned(format!("^{rest}"))
    } else if key.chars().count() > 1 {
        Cow::Owned(key.to_uppercase())
    } else {
        Cow::Borrowed(key)
    }
}

/// The keybinding table in the README's "Keyboard Shortcuts" section.
#[must_use]
pub fn readme_table() -> String {
    let rows: Vec<[String; 3]> = KEYMAP
        .iter()
        .map(|k| {
            let keys = k
                .keys
                .split(" / ")
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(" / ");
            [keys, k.context.applies_in().to_string(), k.desc.to_string()]
        })
        .collect();
    let header = ["Key", "Context", "Action"].map(String::from);
    let width = |col: usize| {
        rows.iter()
            .chain(std::iter::once(&header))
            .map(|row| row[col].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2)];
    let line = |cells: &[String; 3]| {
        let mut line = String::from("|");
        for (cell, width) in cells.iter().zip(widths) {
            let pad = width - cell.chars().count();
            let _ = write!(line, " {cell}{} |", " ".repeat(pad));
        }
        line
    };
    let rule = widths.map(|w| "-".repeat(w));
    std::iter::once(line(&header))
        .chain(std::iter::once(line(&rule)))
        .chain(rows.iter().map(line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_fit_the_help_overlay() {
        for k in KEYMAP {
            assert!(k.desc.chars().count() <= 24, "{k:?}");
            assert!(k.keys.chars().count() <= 11, "{k:?}");
        }
    }

    #[test]
    fn badges() {
        assert_eq!(badge("Ctrl+]"), "^]");
        assert_eq!(badge("Enter"), "ENTER");
        assert_eq!(badge("r"), "r");
        assert_eq!(badge("L"), "L");
    }

    #[test]
    fn keymap_matches_readme() {
        let readme = include_str!("../../README.md");
        let section = readme
            .split("## Keyboard Shortcuts\n")
            .nth(1)
            .expect("README has no Keyboard Shortcuts section");
        let table: Vec<&str> = section
            .lines()
            .skip_while(|line| !line.starts_with('|'))
            .take_while(|line| line.starts_with('|'))
            .collect();
        let expected = readme_table();
        assert_eq!(
            table.join("\n"),
            expected,
            "Update the README's keybinding table to:\n{expected}"
        );
    }
}
