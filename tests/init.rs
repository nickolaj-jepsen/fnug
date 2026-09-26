//! Tests for `fnug init`: tooling detection and the config it writes.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use fnug::commands::group::CommandGroup;
use fnug::config_file::{Config, ConfigCommand, WorkspaceConfig};
use fnug::init::{InitError, InitOptions, Proposal, detect};

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
fn detect_node_skips_npm_placeholder_test() {
    let dir = tempfile::tempdir().unwrap();
    // What `npm init -y` writes, which always fails
    let placeholder = r#""test": "echo \"Error: no test specified\" && exit 1""#;
    write(
        dir.path(),
        "package.json",
        &format!(r#"{{"scripts": {{"lint": "eslint .", {placeholder}}}}}"#),
    );
    assert_eq!(cmds(&only(dir.path())), [("lint", "npm run lint")]);

    write(
        dir.path(),
        "package.json",
        &format!(r#"{{"scripts": {{{placeholder}}}}}"#),
    );
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

fn fnug(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fnug"))
        .current_dir(dir)
        .args(args)
        .env_remove("FNUG_LOG")
        .output()
        .unwrap()
}

#[test]
fn not_found_suggests_init() {
    let dir = tempfile::tempdir().unwrap();
    let output = fnug(dir.path(), &["check", "--no-tui"]);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No config file found"), "{stderr}");
    assert!(stderr.contains("run `fnug init`"), "{stderr}");
}

fn init(dir: &Path, force: bool) -> Result<PathBuf, InitError> {
    fnug::init::run(
        &InitOptions {
            dir: dir.to_path_buf(),
            file: None,
            force,
            yes: true,
        },
        |_| panic!("--yes doesn't ask"),
    )
}

/// Every command id in `group` and below.
fn command_ids(group: &CommandGroup) -> Vec<String> {
    let mut ids: Vec<_> = group.commands.iter().map(|c| c.id.clone()).collect();
    ids.extend(group.children.iter().flat_map(command_ids));
    ids
}

/// Load the config at `path` on its own, and return its command ids.
fn load_ids(path: &Path) -> Vec<String> {
    let (root, _) = fnug::load_config(path.to_str(), true).unwrap();
    command_ids(&root)
}

fn assert_header(content: &str) {
    let header = format!(
        "# yaml-language-server: $schema={}",
        fnug::schema::schema_url_for_version()
    );
    assert_eq!(content.lines().next(), Some(header.as_str()), "{content}");
}

#[test]
fn init_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("my-app");
    write(&dir, "Cargo.toml", "");
    write(&dir, "pyproject.toml", "[tool.pytest.ini_options]\n");
    write(&dir, "uv.lock", "");
    write(
        &dir,
        "package.json",
        r#"{"scripts": {"lint": "eslint .", "test": "vitest"}}"#,
    );
    write(&dir, "pnpm-lock.yaml", "");
    write(&dir, "go.mod", "module demo\n");

    let path = init(&dir, false).unwrap();

    assert_eq!(path, dir.canonicalize().unwrap().join(".fnug.yaml"));
    let content = std::fs::read_to_string(&path).unwrap();
    assert_header(&content);
    assert!(!content.contains("null"), "{content}");
    let config = Config::from_file(&path).unwrap();
    assert_eq!(config.name, "my-app");
    assert_eq!(
        config.fnug_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    let ids = load_ids(&path);
    let unique: HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
    for id in ["rust/test", "node/test", "go/test", "fmt", "pytest", "lint"] {
        assert!(ids.iter().any(|i| i == id), "{id} not in {ids:?}");
    }
}

#[test]
fn init_includes_what_choose_picks() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "Cargo.toml", "");
    write(dir.path(), "go.mod", "module demo\n");
    let run = |choice: Vec<usize>| {
        let opts = InitOptions {
            dir: dir.path().to_path_buf(),
            force: true,
            ..InitOptions::default()
        };
        fnug::init::run(&opts, |proposals| {
            let keys: Vec<_> = proposals.iter().map(|p| p.key).collect();
            assert_eq!(keys, ["rust", "go"]);
            Ok(choice)
        })
        .unwrap()
    };

    let path = run(vec![1]);
    assert_eq!(load_ids(&path), ["gofmt", "vet", "test"]);

    // Nothing picked: an example command to replace
    let path = run(vec![]);
    let content = std::fs::read_to_string(&path).unwrap();
    assert_header(&content);
    assert!(content.contains("Replace the example command"), "{content}");
    assert_eq!(load_ids(&path), ["example"]);
}

#[test]
fn prepare_writes_a_workspace_root_and_packages() {
    let dir = tempfile::tempdir().unwrap();
    let api = dir.path().join("api");
    write(&api, "go.mod", "module api\n");
    let groups = detect(&api).into_iter().map(|p| p.group).collect();
    fnug::init::prepare(&api, false, groups, None)
        .unwrap()
        .write()
        .unwrap();
    let root = fnug::init::prepare(
        dir.path(),
        false,
        Vec::new(),
        Some(WorkspaceConfig::Enabled(true)),
    )
    .unwrap();

    // No example command in a root that only gathers its packages
    assert!(!root.content.contains("example"), "{}", root.content);
    root.write().unwrap();
    let (loaded, _) = fnug::load_config(root.path.to_str(), false).unwrap();
    assert_eq!(command_ids(&loaded), ["api/gofmt", "api/vet", "api/test"]);
}

#[test]
fn init_refuses_existing() {
    for file in [".fnug.yaml", ".fnug.yml", ".fnug.json"] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Cargo.toml", "");
        write(dir.path(), file, "keep me");

        let err = fnug::init::run(
            &InitOptions {
                dir: dir.path().to_path_buf(),
                ..InitOptions::default()
            },
            |_| panic!("asked before refusing"),
        )
        .unwrap_err();

        let InitError::Exists(existing) = &err else {
            panic!("{err:?}");
        };
        assert_eq!(existing.file_name().unwrap(), file);
        assert!(err.to_string().ends_with("already exists"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(file)).unwrap(),
            "keep me"
        );
    }
}

#[test]
fn init_force_overwrites() {
    // The file fnug loads is replaced, in its own format
    for (file, other, json) in [
        (".fnug.yaml", None, false),
        (".fnug.json", Some(".fnug.yml"), true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Cargo.toml", "");
        write(dir.path(), file, "old: [");
        if let Some(other) = other {
            write(dir.path(), other, "also old");
        }

        let path = init(dir.path(), true).unwrap();

        assert_eq!(path.file_name().unwrap(), file);
        assert_eq!(load_ids(&path), ["fmt", "clippy", "test"]);
        if json {
            let config = Config::from_file(&path).unwrap();
            let schema = fnug::schema::schema_url_for_version();
            assert_eq!(config.schema, Some(schema));
        }
        if let Some(other) = other {
            let untouched = std::fs::read_to_string(dir.path().join(other)).unwrap();
            assert_eq!(untouched, "also old");
        }
    }
}

#[test]
fn init_cli_proposes_nix_tools_on_path() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    write(&project, "flake.nix", "{}");
    let bin = dir.path().join("bin");
    for tool in ["alejandra", "deadnix"] {
        write_script(&bin.join(tool), "#!/bin/sh\n");
    }

    // Without a terminal, `fnug init` includes everything it found
    let output = Command::new(env!("CARGO_BIN_EXE_fnug"))
        .current_dir(dir.path())
        .args(["init", "project"])
        .env("PATH", &bin)
        .env_remove("FNUG_LOG")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let path = project.canonicalize().unwrap().join(".fnug.yaml");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Created {}", path.display())),
        "{stdout}"
    );
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("alejandra --check ."), "{content}");
    assert!(content.contains("deadnix --fail ."), "{content}");
    assert!(!content.contains("statix"), "{content}");

    let again = fnug(dir.path(), &["init", "project"]);
    assert!(!again.status.success(), "{again:?}");
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(stderr.contains("already exists"), "{stderr}");
    // The hint replaces the same directory's config
    assert!(
        stderr.contains("run `fnug init --force project` to replace it"),
        "{stderr}"
    );
}

#[test]
fn init_cli_creates_in_root() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "sub/go.mod", "module demo\n");

    let output = fnug(dir.path(), &["--root", "sub", "init", "--yes"]);

    assert!(output.status.success(), "{output:?}");
    assert!(!dir.path().join(".fnug.yaml").exists());
    assert_eq!(
        load_ids(&dir.path().join("sub/.fnug.yaml")),
        ["gofmt", "vet", "test"]
    );

    // DIR names the same directory, or else it's a contradiction
    let output = fnug(dir.path(), &["--root", "sub", "init", "--force", "sub"]);
    assert!(output.status.success(), "{output:?}");
    std::fs::create_dir(dir.path().join("other")).unwrap();
    let output = fnug(dir.path(), &["--root", "sub", "init", "other"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--root"), "{stderr}");
    assert!(!dir.path().join("other/.fnug.yaml").exists());
}

#[test]
fn init_cli_writes_the_config_file() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "sub/go.mod", "module demo\n");

    // The file's directory is the project, as it is when fnug loads the file
    let output = fnug(dir.path(), &["-c", "sub/ci.yaml", "init", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    let path = dir.path().join("sub/ci.yaml");
    assert_header(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(load_ids(&path), ["gofmt", "vet", "test"]);
    assert!(!dir.path().join("sub/.fnug.yaml").exists());

    let again = fnug(dir.path(), &["-c", "sub/ci.yaml", "init", "--yes"]);
    assert_eq!(again.status.code(), Some(2), "{again:?}");
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(
        stderr.contains("already exists; run `fnug -c sub/ci.yaml init --force --yes`"),
        "{stderr}"
    );

    let output = fnug(dir.path(), &["-c", "sub/ci.json", "init", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    let json = Config::from_file(&dir.path().join("sub/ci.json")).unwrap();
    assert_eq!(json.schema, Some(fnug::schema::schema_url_for_version()));

    // With --root, the commands run there instead, so the file can be elsewhere
    let output = fnug(
        dir.path(),
        &["-c", "ci.yaml", "--root", "sub", "init", "--yes"],
    );
    assert!(output.status.success(), "{output:?}");
    let loaded = fnug::load(&fnug::LoadOptions {
        config: Some(dir.path().join("ci.yaml")),
        root_dir: Some(dir.path().join("sub")),
        ..fnug::LoadOptions::default()
    })
    .unwrap();
    assert_eq!(command_ids(&loaded.root), ["gofmt", "vet", "test"]);

    // Without it, a file outside DIR would run DIR's commands in the wrong place
    let output = fnug(dir.path(), &["-c", "elsewhere.yaml", "init", "sub"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!dir.path().join("elsewhere.yaml").exists());
}

#[test]
fn init_warns_when_it_shadows_a_parent_config() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        ".fnug.yaml",
        "name: root\ncommands:\n  - {name: a, cmd: 'true'}\n",
    );
    write(dir.path(), "sub/go.mod", "module demo\n");

    let output = fnug(dir.path(), &["init", "--yes", "sub"]);

    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parent = dir.path().canonicalize().unwrap().join(".fnug.yaml");
    assert!(
        stderr.contains(&format!("{} isn't a workspace root", parent.display())),
        "{stderr}"
    );
    assert!(stderr.contains("workspace: true"), "{stderr}");

    // A workspace root takes the new config in as a package instead
    write(
        dir.path(),
        ".fnug.yaml",
        "name: root\nworkspace: true\ncommands:\n  - {name: a, cmd: 'true'}\n",
    );
    let output = fnug(dir.path(), &["init", "--yes", "--force", "sub"]);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("workspace root"), "{stderr}");
}

#[test]
fn init_warns_below_the_repository_root() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("docs")).unwrap();

    let output = fnug(dir.path(), &["init", "--yes", "docs"]);

    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("isn't the top of its git repository"),
        "{stderr}"
    );
    let root = dir.path().canonicalize().unwrap();
    assert!(
        stderr.contains(&format!("fnug init {}", root.display())),
        "{stderr}"
    );
    assert!(dir.path().join("docs/.fnug.yaml").is_file());

    // At the root, it only says nothing was found
    let output = fnug(dir.path(), &["init"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("found no tooling"), "{stderr}");
    assert!(!stderr.contains("isn't the top"), "{stderr}");
}
