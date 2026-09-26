//! Tests for `fnug init`: tooling detection and the config it writes.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use fnug::config_file::ConfigCommand;
use fnug::init::{Proposal, detect};

fn write(dir: &Path, file: &str, content: &str) {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn commands(proposal: &Proposal) -> &[ConfigCommand] {
    proposal.group.commands.as_deref().unwrap_or_default()
}

/// The `(name, cmd)` of each proposed command.
fn cmds(proposal: &Proposal) -> Vec<(&str, &str)> {
    commands(proposal)
        .iter()
        .map(|c| (c.name.as_str(), c.cmd.as_str()))
        .collect()
}

/// The only proposal for `dir`.
fn only(dir: &Path) -> Proposal {
    let mut proposals = detect(dir);
    assert_eq!(proposals.len(), 1, "{proposals:?}");
    proposals.remove(0)
}

#[test]
fn detect_rust() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "Cargo.toml", "[package]\nname = \"demo\"\n");

    let rust = only(dir.path());

    assert_eq!(rust.key, "rust");
    assert_eq!(rust.group.name, "rust");
    assert_eq!(rust.evidence, [Path::new("Cargo.toml")]);
    assert_eq!(
        cmds(&rust),
        [
            ("fmt", "cargo fmt --check"),
            ("clippy", "cargo clippy --all-targets -- -D warnings"),
            ("test", "cargo test"),
        ]
    );
    let auto = rust.group.auto.as_ref().unwrap();
    assert_eq!((auto.git, auto.watch), (Some(true), Some(true)));
    let regex = auto.regex.as_ref().unwrap();
    assert!(regex.iter().any(|r| r == r"\.rs$"), "{regex:?}");
    assert_eq!(rust.to_string(), "Rust (Cargo.toml): fmt, clippy, test");
}

#[test]
fn detect_node_lockfile_runner() {
    let manifest = r#"{
        "scripts": {"build": "tsc", "lint": "eslint .", "test": "vitest", "typecheck": "tsc --noEmit"}
    }"#;
    for (lockfile, runner) in [
        (Some("pnpm-lock.yaml"), "pnpm"),
        (Some("yarn.lock"), "yarn"),
        (Some("bun.lockb"), "bun"),
        (Some("bun.lock"), "bun"),
        (Some("package-lock.json"), "npm"),
        (None, "npm"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "package.json", manifest);
        if let Some(lockfile) = lockfile {
            write(dir.path(), lockfile, "");
        }

        let node = only(dir.path());

        assert_eq!(node.key, "node");
        let expected: Vec<_> = ["lint", "typecheck", "test"]
            .map(|script| (script, format!("{runner} run {script}")))
            .into();
        let found: Vec<_> = cmds(&node)
            .into_iter()
            .map(|(name, cmd)| (name, cmd.to_string()))
            .collect();
        assert_eq!(found, expected, "{lockfile:?}");
        let test = &commands(&node)[2];
        let env = test.env.as_ref().unwrap();
        assert_eq!(env.get("CI").map(String::as_str), Some("true"));
        assert!(commands(&node)[0].env.is_none());
        let evidence: Vec<_> = std::iter::once("package.json").chain(lockfile).collect();
        assert_eq!(
            node.evidence,
            evidence.iter().map(Path::new).collect::<Vec<_>>()
        );
    }
}

#[test]
fn detect_node_package_manager_field_and_missing_scripts() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"packageManager": "pnpm@9.1.0", "scripts": {"format:check": "prettier -c ."}}"#,
    );
    assert_eq!(
        cmds(&only(dir.path())),
        [("format:check", "pnpm run format:check")]
    );

    // No script to run, or a manifest that doesn't parse: nothing to propose
    write(
        dir.path(),
        "package.json",
        r#"{"scripts": {"dev": "vite"}}"#,
    );
    assert!(detect(dir.path()).is_empty());
    write(dir.path(), "package.json", "{ not json");
    assert!(detect(dir.path()).is_empty());
}

#[test]
fn detect_python_ruff_uv() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pyproject.toml",
        "[project]\nname = \"demo\"\n\n[dependency-groups]\ndev = [\"pytest>=8\"]\n\n[tool.ruff]\nline-length = 100\n",
    );
    write(dir.path(), "uv.lock", "");

    let python = only(dir.path());

    assert_eq!(python.key, "python");
    assert_eq!(
        cmds(&python),
        [
            ("ruff check", "uv run ruff check ."),
            ("ruff format", "uv run ruff format --check ."),
            ("pytest", "uv run pytest"),
        ]
    );
    assert_eq!(
        python.evidence,
        [Path::new("pyproject.toml"), Path::new("uv.lock")]
    );
}

#[test]
fn detect_python_runners_and_tools() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "ruff.toml", "line-length = 100\n");
    assert_eq!(
        cmds(&only(dir.path())),
        [
            ("ruff check", "ruff check ."),
            ("ruff format", "ruff format --check ."),
        ]
    );

    write(dir.path(), "pyproject.toml", "[tool.mypy]\nstrict = true\n");
    write(dir.path(), "conftest.py", "");
    write(dir.path(), "poetry.lock", "");
    assert_eq!(
        cmds(&only(dir.path())),
        [
            ("ruff check", "poetry run ruff check ."),
            ("ruff format", "poetry run ruff format --check ."),
            ("mypy", "poetry run mypy ."),
            ("pytest", "poetry run pytest"),
        ]
    );
}

/// Run `cmd` with `sh -c` in `dir`, with only `bin` on `PATH`.
fn run_sh(dir: &Path, bin: &Path, cmd: &str) -> std::process::Output {
    Command::new("/bin/sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .env("PATH", bin)
        .output()
        .unwrap()
}

fn write_script(path: &Path, body: &str) {
    write(
        path.parent().unwrap(),
        path.file_name().unwrap().to_str().unwrap(),
        body,
    );
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn detect_go_gofmt_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "go.mod", "module example.com/demo\n");

    let go = only(dir.path());

    assert_eq!(go.key, "go");
    let names: Vec<_> = cmds(&go).into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["gofmt", "vet", "test"]);
    assert_eq!(cmds(&go)[1].1, "go vet ./...");
    assert_eq!(cmds(&go)[2].1, "go test ./...");

    // `gofmt -l` exits 0 even when it lists unformatted files; the command must not
    let gofmt = cmds(&go)[0].1;
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let missing = run_sh(dir.path(), &bin, gofmt);
    assert!(!missing.status.success(), "{missing:?}");

    write_script(&bin.join("gofmt"), "#!/bin/sh\necho main.go\n");
    let unformatted = run_sh(dir.path(), &bin, gofmt);
    assert!(!unformatted.status.success(), "{unformatted:?}");
    assert_eq!(String::from_utf8_lossy(&unformatted.stdout), "main.go\n");

    write_script(
        &bin.join("gofmt"),
        "#!/bin/sh\necho 'main.go:1:1: expected' >&2\nexit 2\n",
    );
    let broken = run_sh(dir.path(), &bin, gofmt);
    assert!(!broken.status.success(), "{broken:?}");

    write_script(&bin.join("gofmt"), "#!/bin/sh\n");
    let formatted = run_sh(dir.path(), &bin, gofmt);
    assert!(formatted.status.success(), "{formatted:?}");
}

#[test]
fn detect_orders_groups_and_ignores_unknown_projects() {
    let dir = tempfile::tempdir().unwrap();
    assert!(detect(dir.path()).is_empty());

    write(dir.path(), "go.mod", "module demo\n");
    write(dir.path(), "Cargo.toml", "");
    write(dir.path(), "pyproject.toml", "");
    write(
        dir.path(),
        "package.json",
        r#"{"scripts": {"test": "jest"}}"#,
    );
    let keys: Vec<_> = detect(dir.path()).iter().map(|p| p.key).collect();
    assert_eq!(keys, ["rust", "python", "node", "go"]);
}
