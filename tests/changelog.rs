//! Keeps `CHANGELOG.md` in the Keep a Changelog 1.1 shape that the release workflow reads.

use std::collections::HashSet;

use regex::Regex;

const CATEGORIES: [&str; 6] = [
    "Added",
    "Changed",
    "Deprecated",
    "Removed",
    "Fixed",
    "Security",
];

/// Entries are read by people skimming a release, so each one has to fit on a line.
const MAX_ENTRY_CHARS: usize = 160;

#[derive(Default)]
struct Release {
    name: String,
    categories: Vec<String>,
    empty_category: Option<String>,
}

/// Every rule `CHANGELOG.md` breaks, one message per offending line.
fn changelog_errors(text: &str) -> Vec<String> {
    let release_re = Regex::new(r"^## \[(.+?)\](.*)$").unwrap();
    let version_re = Regex::new(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$").unwrap();
    let date_re = Regex::new(r"^ - \d{4}-\d{2}-\d{2}$").unwrap();
    let link_re = Regex::new(r"^\[(.+?)\]: https://\S+$").unwrap();

    let mut errors = Vec::new();
    let mut releases: Vec<Release> = Vec::new();
    let mut links = HashSet::new();

    if text.lines().next() != Some("# Changelog") {
        errors.push("the first line must be `# Changelog`".to_string());
    }

    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if let Some(caps) = release_re.captures(line) {
            let (name, rest) = (&caps[1], &caps[2]);
            if releases.is_empty() {
                if name != "Unreleased" || !rest.is_empty() {
                    errors.push(format!(
                        "line {n}: the first release must be `## [Unreleased]`"
                    ));
                }
            } else if !version_re.is_match(name) || !date_re.is_match(rest) {
                errors.push(format!(
                    "line {n}: expected `## [X.Y.Z] - YYYY-MM-DD`, got `{line}`"
                ));
            }
            if releases.iter().any(|r| r.name == name) {
                errors.push(format!("line {n}: `{name}` is listed twice"));
            }
            releases.push(Release {
                name: name.to_string(),
                ..Release::default()
            });
        } else if line.starts_with("## ") {
            errors.push(format!(
                "line {n}: release headings are `## [version]`, got `{line}`"
            ));
        } else if let Some(category) = line.strip_prefix("### ") {
            let Some(release) = releases.last_mut() else {
                errors.push(format!("line {n}: `{line}` comes before any release"));
                continue;
            };
            if let Some(empty) = release.empty_category.take() {
                errors.push(format!(
                    "line {n}: `### {empty}` in {} has no entries",
                    release.name
                ));
            }
            match CATEGORIES.iter().position(|c| *c == category) {
                None => errors.push(format!(
                    "line {n}: `{category}` is not one of {}",
                    CATEGORIES.join(", ")
                )),
                Some(pos) => {
                    let last = release
                        .categories
                        .last()
                        .and_then(|c| CATEGORIES.iter().position(|k| k == c));
                    if last.is_some_and(|last| last >= pos) {
                        errors.push(format!(
                            "line {n}: `### {category}` in {} is out of order or repeated (order: {})",
                            release.name,
                            CATEGORIES.join(", ")
                        ));
                    }
                }
            }
            release.categories.push(category.to_string());
            release.empty_category = Some(category.to_string());
        } else if let Some(caps) = link_re.captures(line) {
            links.insert(caps[1].to_string());
        } else if let Some(release) = releases.last_mut().filter(|r| !r.categories.is_empty()) {
            if line.is_empty() {
                continue;
            }
            if !line.starts_with("- ") {
                errors.push(format!(
                    "line {n}: entries are single `- ` lines; join or shorten `{line}`"
                ));
            }
            let chars = line.chars().count();
            if chars > MAX_ENTRY_CHARS {
                errors.push(format!(
                    "line {n}: entry is {chars} characters, keep it within {MAX_ENTRY_CHARS}"
                ));
            }
            release.empty_category = None;
        }
    }

    for release in &releases {
        if let Some(empty) = &release.empty_category {
            errors.push(format!("`### {empty}` in {} has no entries", release.name));
        }
        if !links.contains(&release.name) {
            errors.push(format!(
                "`[{}]` has no link reference at the end",
                release.name
            ));
        }
    }
    errors
}

#[test]
fn changelog_follows_keep_a_changelog() {
    let text =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/CHANGELOG.md")).unwrap();
    let errors = changelog_errors(&text);
    assert!(errors.is_empty(), "CHANGELOG.md:\n{}", errors.join("\n"));
}

#[test]
fn changelog_rules_catch_mistakes() {
    let ok = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- A thing\n\n### Fixed\n\n- B\n\n\
              ## [1.0.0] - 2026-01-02\n\n### Removed\n\n- C\n\n\
              [Unreleased]: https://example.com/a\n[1.0.0]: https://example.com/b\n";
    assert_eq!(changelog_errors(ok), Vec::<String>::new());

    let long = format!("- {}", "x".repeat(MAX_ENTRY_CHARS));
    let cases = [
        ("## [1.0.0] - 2026-01-02\n", "first release must be"),
        (
            "## [Unreleased]\n### Fixed\n- a\n### Added\n- b\n",
            "out of order",
        ),
        ("## [Unreleased]\n### Fixes\n- a\n", "is not one of"),
        (
            "## [Unreleased]\n### Added\n### Fixed\n- a\n",
            "has no entries",
        ),
        (
            "## [Unreleased]\n### Added\n- a\n  wrapped\n",
            "single `- ` lines",
        ),
        (
            &format!("## [Unreleased]\n### Added\n{long}\n"),
            "keep it within",
        ),
        (
            "## [Unreleased]\n## [1.0] - 2026-01-02\n",
            "expected `## [X.Y.Z]",
        ),
    ];
    for (body, expected) in cases {
        let text =
            format!("# Changelog\n{body}[Unreleased]: https://example.com\n[1.0]: https://e.com\n");
        let errors = changelog_errors(&text);
        assert!(
            errors.iter().any(|e| e.contains(expected)),
            "{body:?} should report {expected:?}, got {errors:?}"
        );
    }
    let unlinked = changelog_errors("# Changelog\n## [Unreleased]\n");
    assert!(
        unlinked.iter().any(|e| e.contains("no link reference")),
        "{unlinked:?}"
    );
}
