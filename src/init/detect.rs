//! Find the tooling a project uses from its marker files, such as `Cargo.toml`.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use log::info;

use crate::config_file::{ConfigAuto, ConfigCommand, ConfigCommandGroup};
use crate::setup::fsutil;

/// A command group proposed for a project, and the files that suggested it.
#[derive(Debug)]
pub struct Proposal {
    /// The detector's name, such as `rust`; unique among proposals, and the group's name.
    pub key: &'static str,
    /// The tooling's display name, such as `Rust`.
    pub label: String,
    /// The files that suggested the group, relative to the project directory.
    pub evidence: Vec<PathBuf>,
    /// The group to add to the config.
    pub group: ConfigCommandGroup,
}

impl fmt::Display for Proposal {
    /// `Rust (Cargo.toml): fmt, clippy, test`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let evidence: Vec<_> = self
            .evidence
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let names: Vec<_> = self
            .group
            .commands
            .iter()
            .flatten()
            .map(|c| c.name.as_str())
            .collect();
        write!(
            f,
            "{} ({}): {}",
            self.label,
            evidence.join(", "),
            names.join(", ")
        )
    }
}

/// Propose a command group for each kind of tooling found in `dir`: Rust, Python, Node, Go and
/// Nix, in that order. Each group's commands are selected by uncommitted changes and watched
/// edits to the files it checks. Tooling without commands to run is not proposed: a
/// `package.json` without a `format:check`, `lint`, `typecheck` or `test` script (npm's
/// placeholder test script doesn't count), or a `flake.nix` when none of alejandra, statix and
/// deadnix is on `PATH`.
#[must_use]
pub fn detect(dir: &Path) -> Vec<Proposal> {
    let project = Project { dir };
    [rust, python, node, go, nix]
        .into_iter()
        .filter_map(|detector| detector(&project))
        .collect()
}

/// The directory being detected.
struct Project<'a> {
    dir: &'a Path,
}

impl Project<'_> {
    fn has(&self, file: &str) -> bool {
        self.dir.join(file).is_file()
    }

    /// The first of `files` that exists.
    fn first<'f>(&self, files: &[&'f str]) -> Option<&'f str> {
        files.iter().copied().find(|file| self.has(file))
    }

    fn read(&self, file: &str) -> Option<String> {
        let path = self.dir.join(file);
        std::fs::read_to_string(&path)
            .inspect_err(|e| {
                if e.kind() != std::io::ErrorKind::NotFound {
                    info!("can't read {}: {e}", path.display());
                }
            })
            .ok()
    }
}

fn rust(project: &Project) -> Option<Proposal> {
    project.has("Cargo.toml").then(|| {
        proposal(
            "rust",
            "Rust",
            &["Cargo.toml"],
            &[r"\.rs$", r"(^|/)Cargo\.(toml|lock)$"],
            vec![
                command("fmt", "cargo fmt --check"),
                command("clippy", "cargo clippy --all-targets -- -D warnings"),
                command("test", "cargo test"),
            ],
        )
    })
}

/// Lockfiles and the command that runs a tool in the project's environment.
const PYTHON_RUNNERS: [(&str, &str); 3] = [
    ("uv.lock", "uv run "),
    ("poetry.lock", "poetry run "),
    ("pdm.lock", "pdm run "),
];

fn python(project: &Project) -> Option<Proposal> {
    let configs = ["pyproject.toml", "ruff.toml", ".ruff.toml"];
    let mut evidence: Vec<&str> = configs.into_iter().filter(|f| project.has(f)).collect();
    if evidence.is_empty() {
        return None;
    }
    // `uv run` and friends put the environment first on PATH, so a global tool still works
    let lock = PYTHON_RUNNERS.iter().find(|(lock, _)| project.has(lock));
    let prefix = lock.map_or("", |(_, prefix)| prefix);
    evidence.extend(lock.map(|(lock, _)| *lock));
    let pyproject = project.read("pyproject.toml").unwrap_or_default();

    let mut commands = vec![
        command("ruff check", format!("{prefix}ruff check .")),
        command("ruff format", format!("{prefix}ruff format --check .")),
    ];
    if uses_python_tool(&pyproject, "mypy") || project.first(&["mypy.ini", ".mypy.ini"]).is_some() {
        commands.push(command("mypy", format!("{prefix}mypy .")));
    }
    if uses_python_tool(&pyproject, "pytest")
        || project.first(&["pytest.ini", "conftest.py"]).is_some()
    {
        commands.push(command("pytest", format!("{prefix}pytest")));
    }
    Some(proposal(
        "python",
        "Python",
        &evidence,
        &[r"\.pyi?$", r"(^|/)(pyproject|\.?ruff)\.toml$"],
        commands,
    ))
}

/// Whether `pyproject` configures `tool` (a `[tool.<tool>...]` table) or lists it as a
/// dependency: `"pytest>=8"` in a PEP 621 or PEP 735 array, or `pytest = "^8"` in a Poetry table.
fn uses_python_tool(pyproject: &str, tool: &str) -> bool {
    let ends_name = |rest: &str| {
        !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    pyproject.lines().map(str::trim).any(|line| {
        if let Some(table) = line.strip_prefix('[') {
            let table = table.trim_start_matches('[').trim_start();
            return table
                .strip_prefix("tool.")
                .and_then(|t| t.strip_prefix(tool))
                .is_some_and(|rest| rest.starts_with(['.', ']']));
        }
        let poetry = line
            .strip_prefix(tool)
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        let listed = ["\"", "'"].iter().any(|quote| {
            line.match_indices(&format!("{quote}{tool}"))
                .any(|(i, found)| ends_name(&line[i + found.len()..]))
        });
        poetry || listed
    })
}

/// Lockfiles and the package manager that wrote them, in the order they are looked for.
const NODE_LOCKFILES: [(&str, &str); 6] = [
    ("pnpm-lock.yaml", "pnpm"),
    ("yarn.lock", "yarn"),
    ("bun.lock", "bun"),
    ("bun.lockb", "bun"),
    ("package-lock.json", "npm"),
    ("npm-shrinkwrap.json", "npm"),
];

/// The `package.json` scripts that check a project, in the order they run.
const NODE_SCRIPTS: [&str; 4] = ["format:check", "lint", "typecheck", "test"];

/// Part of the test script `npm init` writes: `echo "Error: no test specified" && exit 1`.
const NPM_PLACEHOLDER: &str = "Error: no test specified";

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageJson {
    #[serde(default)]
    scripts: HashMap<String, serde_json::Value>,
    package_manager: Option<String>,
}

fn node(project: &Project) -> Option<Proposal> {
    let manifest: PackageJson = serde_json::from_str(&project.read("package.json")?)
        .inspect_err(|e| info!("no Node commands: can't parse package.json: {e}"))
        .ok()?;
    let lock = NODE_LOCKFILES.iter().find(|(lock, _)| project.has(lock));
    // Without a lockfile, the `packageManager` field (`pnpm@9.1.0`) names it
    let runner = lock.map_or_else(
        || {
            let named = manifest.package_manager.as_deref().unwrap_or_default();
            let name = named.split('@').next().unwrap_or_default();
            ["pnpm", "yarn", "bun"]
                .into_iter()
                .find(|runner| *runner == name)
                .unwrap_or("npm")
        },
        |(_, runner)| *runner,
    );
    let commands: Vec<_> = NODE_SCRIPTS
        .into_iter()
        .filter(|script| {
            manifest.scripts.get(*script).is_some_and(|body| {
                // `npm init` writes a test script that only fails
                let placeholder = body
                    .as_str()
                    .is_some_and(|body| body.contains(NPM_PLACEHOLDER));
                if placeholder {
                    info!("not proposing the {script} script: it is npm's placeholder");
                }
                !placeholder
            })
        })
        .map(|script| {
            let mut command = command(script, format!("{runner} run {script}"));
            if script == "test" {
                // Test runners such as vitest start in watch mode unless they run in CI
                command.env = Some(HashMap::from([("CI".to_string(), "true".to_string())]));
            }
            command
        })
        .collect();
    if commands.is_empty() {
        info!(
            "no Node commands: package.json has none of the scripts {}",
            NODE_SCRIPTS.join(", ")
        );
        return None;
    }
    let mut evidence = vec!["package.json"];
    evidence.extend(lock.map(|(lock, _)| *lock));
    Some(proposal(
        "node",
        "Node",
        &evidence,
        &[
            r"\.([cm]?[jt]sx?|vue|svelte)$",
            r"(^|/)(package|tsconfig)\.json$",
        ],
        commands,
    ))
}

fn go(project: &Project) -> Option<Proposal> {
    project.has("go.mod").then(|| {
        proposal(
            "go",
            "Go",
            &["go.mod"],
            &[r"\.go$", r"(^|/)go\.(mod|sum)$"],
            vec![
                // gofmt -l exits 0 when it lists unformatted files
                command(
                    "gofmt",
                    r#"files=$(gofmt -l .) && test -z "$files" || { echo "$files"; exit 1; }"#,
                ),
                command("vet", "go vet ./..."),
                command("test", "go test ./..."),
            ],
        )
    })
}

fn nix(project: &Project) -> Option<Proposal> {
    if !project.has("flake.nix") {
        return None;
    }
    let tools = [
        ("alejandra", "alejandra --check ."),
        ("statix", "statix check ."),
        ("deadnix", "deadnix --fail ."),
    ];
    let commands: Vec<_> = tools
        .into_iter()
        .filter(|(tool, _)| fsutil::find_on_path(tool).is_some())
        .map(|(tool, cmd)| command(tool, cmd))
        .collect();
    if commands.is_empty() {
        info!("no Nix commands: none of alejandra, statix and deadnix is on PATH");
        return None;
    }
    Some(proposal(
        "nix",
        "Nix",
        &["flake.nix"],
        &[r"\.nix$"],
        commands,
    ))
}

fn proposal(
    key: &'static str,
    label: &str,
    evidence: &[&str],
    regex: &[&str],
    commands: Vec<ConfigCommand>,
) -> Proposal {
    Proposal {
        key,
        label: label.to_string(),
        evidence: evidence.iter().map(PathBuf::from).collect(),
        group: ConfigCommandGroup {
            id: None,
            name: key.to_string(),
            auto: Some(ConfigAuto {
                git: Some(true),
                watch: Some(true),
                regex: Some(regex.iter().map(ToString::to_string).collect()),
                ..ConfigAuto::default()
            }),
            cwd: None,
            commands: Some(commands),
            children: None,
            env: None,
            timeout: None,
            exclusive: None,
            source: None,
        },
    }
}

fn command(name: &str, cmd: impl Into<String>) -> ConfigCommand {
    ConfigCommand {
        id: None,
        name: name.to_string(),
        cwd: None,
        cmd: cmd.into(),
        auto: None,
        env: None,
        depends_on: None,
        scrollback: None,
        timeout: None,
        exclusive: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_tool_detection() {
        let cases = [
            ("[tool.mypy]\nstrict = true", "mypy", true),
            ("[[tool.mypy.overrides]]\nmodule = 'x'", "mypy", true),
            ("[tool.mypyc]", "mypy", false),
            ("[tool.pytest.ini_options]", "pytest", true),
            (
                "[dependency-groups]\ndev = [\"pytest>=8\", \"ruff\"]",
                "pytest",
                true,
            ),
            ("dev = ['pytest']", "pytest", true),
            (
                "[tool.poetry.group.dev.dependencies]\npytest = \"^8\"",
                "pytest",
                true,
            ),
            ("dev = [\"pytest-cov\"]", "pytest", false),
            ("dependencies = [\"mypy-extensions\"]", "mypy", false),
            ("[tool.ruff]\nline-length = 100", "pytest", false),
        ];
        for (pyproject, tool, expected) in cases {
            assert_eq!(
                uses_python_tool(pyproject, tool),
                expected,
                "{tool} in {pyproject:?}"
            );
        }
    }
}
