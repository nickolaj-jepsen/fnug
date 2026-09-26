//! Tests for `fnug check --staged --stash`, which sets unstaged changes aside while commands run.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::Duration;

use common::git::git;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `fnug check` in `dir` with `args`, isolated from the user's git config.
fn fnug(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fnug"));
    common::git::isolate(&mut command)
        .current_dir(dir)
        .args(["--no-workspace", "check", "--no-tui"])
        .args(args)
        .env_remove("FNUG_LOG")
        .stdin(Stdio::null());
    command
}

/// `fnug check --staged --stash` with `args`.
fn stash_command(dir: &Path, args: &[&str]) -> Command {
    let mut command = fnug(dir, &["--staged", "--stash"]);
    command.args(args);
    command
}

fn check(dir: &Path, args: &[&str]) -> Output {
    stash_command(dir, args).output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Everything fnug and its commands printed.
fn printed(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr(output)
    )
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn write(dir: &Path, path: &str, content: impl AsRef<[u8]>) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Wait for `child` to exit, killing it after [`TIMEOUT`].
fn wait(child: &mut Child) -> ExitStatus {
    let mut status = None;
    let exited = common::wait_until(TIMEOUT, || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    if !exited {
        let _ = child.kill();
        panic!("fnug did not exit within {TIMEOUT:?}");
    }
    status.unwrap()
}

fn kill(pid: u32, signal: i32) {
    // SAFETY: plain syscall on one of the test's own descendants.
    unsafe { libc::kill(i32::try_from(pid).unwrap(), signal) };
}

/// A repo in `dir` with `config` and `files` committed. Returns false when git isn't available,
/// and the test should skip.
fn repo(dir: &Path, config: &str, files: &[(&str, &str)]) -> bool {
    if !common::git::available() {
        return false;
    }
    common::git::init(dir);
    common::write_config(dir, config);
    for (path, content) in files {
        write(dir, path, content);
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
    true
}

/// Everything git reports about the work tree and the index, to compare before and after.
fn state(dir: &Path) -> String {
    [
        git(dir, &["status", "--porcelain=v1", "--untracked-files=all"]),
        git(dir, &["diff", "--binary"]),
        git(dir, &["diff", "--cached", "--binary"]),
    ]
    .join("\n---\n")
}

/// Neither a lock nor a saved patch is left behind.
fn assert_clean_git_dir(dir: &Path) {
    assert!(!dir.join(".git/fnug-stash.lock").exists());
    let patches: Vec<_> = std::fs::read_dir(dir.join(".git/fnug"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(patches.is_empty(), "{patches:?}");
}

const LINT: &str = r"
name: root
commands:
  - name: lint
    cmd: '! grep -rn BAD src/'
    auto:
      git: true
      path: [src]
";

#[test]
fn stash_runs_on_index_content() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    if !repo(dir, LINT, &[("src/a.py", "ok\n")]) {
        return;
    }
    // BAD is staged, and gone from the work tree
    write(dir, "src/a.py", "ok\nBAD\n");
    git(dir, &["add", "src/a.py"]);
    write(dir, "src/a.py", "ok\n");

    // Selection alone checks the work tree, so BAD gets through
    let output = fnug(dir, &["--staged"]).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));

    let output = check(dir, &[]);
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("src/a.py:2:BAD"),
        "{}",
        printed(&output)
    );
    assert_eq!(read(&dir.join("src/a.py")), "ok\n");
    assert_eq!(git(dir, &["show", ":src/a.py"]), "ok\nBAD\n");
    assert_clean_git_dir(dir);
}

/// Records whether the work tree matched the index while it ran.
const RECORD: &str = r"
name: root
commands:
  - name: record
    cmd: 'git diff --stat > .git/during; git ls-files -m >> .git/during'
    auto:
      always: true
";

#[test]
fn stash_restores_unstaged_byte_exact() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let lines = (1..=12)
        .map(|i| format!("line {i}\n"))
        .collect::<Vec<_>>()
        .concat();
    let files = [
        ("partial.txt", lines.as_str()),
        ("gone.txt", "deleted in the work tree\n"),
        ("nonl.txt", "a\nb"),
        ("mode.sh", "echo hi\n"),
        ("staged.txt", "old\n"),
        ("odd [x]*\tname.txt", "odd\n"),
    ];
    if !repo(dir, RECORD, &files) {
        return;
    }
    write(dir, "bin.dat", [0u8, 1, 2, 255, 0, 10, 13]);
    git(dir, &["add", "bin.dat"]);
    git(dir, &["commit", "-qm", "binary"]);

    // Staged: the first hunk of partial.txt and staged.txt
    write(dir, "partial.txt", lines.replace("line 1\n", "line one\n"));
    write(dir, "staged.txt", "new\n");
    git(dir, &["add", "partial.txt", "staged.txt"]);
    // Unstaged: the last hunk of partial.txt, a binary edit, a deletion, a missing final newline,
    // a mode change and a name that is also a glob
    write(dir, "odd [x]*\tname.txt", "odd\nchanged\n");
    write(
        dir,
        "partial.txt",
        lines
            .replace("line 1\n", "line one\n")
            .replace("line 12\n", "line twelve\n"),
    );
    write(dir, "bin.dat", [0u8, 1, 2, 254, 0, 10, 13, 0]);
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    write(dir, "nonl.txt", "a\nb\nc");
    std::fs::set_permissions(dir.join("mode.sh"), PermissionsExt::from_mode(0o755)).unwrap();
    let before = state(dir);
    let partial = read(&dir.join("partial.txt"));

    let output = check(dir, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        read(&dir.join(".git/during")),
        "",
        "the work tree had unstaged changes during the run"
    );
    assert_eq!(state(dir), before);
    assert_eq!(read(&dir.join("partial.txt")), partial);
    assert_eq!(
        std::fs::read(dir.join("bin.dat")).unwrap(),
        [0u8, 1, 2, 254, 0, 10, 13, 0]
    );
    assert!(!dir.join("gone.txt").exists());
    assert_eq!(read(&dir.join("nonl.txt")), "a\nb\nc");
    let mode = std::fs::metadata(dir.join("mode.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
    assert_clean_git_dir(dir);
}

#[test]
fn stash_intent_to_add_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let config = r"
name: root
commands:
  - name: record
    cmd: 'cat new.txt > .git/during'
    auto:
      always: true
";
    if !repo(dir, config, &[("a.txt", "a\n"), ("b.txt", "b\n")]) {
        return;
    }
    write(dir, "a.txt", "staged\n");
    git(dir, &["add", "a.txt"]);
    write(dir, "b.txt", "unstaged\n");
    write(dir, "new.txt", "intent to add\n");
    git(dir, &["add", "-N", "new.txt"]);
    let before = state(dir);
    assert!(before.contains(" A new.txt"), "{before}");

    let output = check(dir, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(read(&dir.join(".git/during")), "intent to add\n");
    assert_eq!(state(dir), before);
    assert_eq!(read(&dir.join("new.txt")), "intent to add\n");
    assert_clean_git_dir(dir);
}

#[test]
fn stash_untracked_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let config = r"
name: root
commands:
  - name: record
    cmd: 'cat notes/untracked.txt > .git/during'
    auto:
      always: true
";
    if !repo(dir, config, &[("a.txt", "a\n")]) {
        return;
    }
    write(dir, "a.txt", "staged\n");
    git(dir, &["add", "a.txt"]);
    write(dir, "a.txt", "staged\nunstaged\n");
    write(dir, "notes/untracked.txt", "mine\n");
    let before = state(dir);

    let output = check(dir, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(read(&dir.join(".git/during")), "mine\n");
    assert_eq!(read(&dir.join("notes/untracked.txt")), "mine\n");
    assert_eq!(state(dir), before);
    assert_clean_git_dir(dir);
}

#[test]
fn stash_fixer_conflict_keeps_user_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let config = r"
name: root
commands:
  - name: fmt
    cmd: sed -i.bak 's/x=1/x = 1/' a.py && rm a.py.bak
    auto:
      always: true
";
    if !repo(dir, config, &[("a.py", "x=0\n")]) {
        return;
    }
    write(dir, "a.py", "x=1\n");
    git(dir, &["add", "a.py"]);
    // The fixer rewrites the line the unstaged change touches
    write(dir, "a.py", "x=1  # wip\n");
    let before = state(dir);

    let output = check(dir, &[]);
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(err.contains("fmt FAIL (modified: a.py"), "{err}");
    assert!(
        err.contains("changes to those files were discarded"),
        "{err}"
    );
    assert_eq!(read(&dir.join("a.py")), "x=1  # wip\n");
    assert_eq!(state(dir), before);
    assert_clean_git_dir(dir);
}

/// Two blocks alike but for one line, so a hunk in the second also fits the first.
const REPEATED: &str = "1\n2\n3\nNEW\n4\n5\n6\n---\n1\n2\n3\n4\n5\n6\n";
/// [`REPEATED`] with the line added to the second block too.
const REPEATED_SYNCED: &str = "1\n2\n3\nNEW\n4\n5\n6\n---\n1\n2\n3\nNEW\n4\n5\n6\n";

#[test]
fn stash_fixer_on_repeated_context_keeps_user_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // Rewrites a context line of the unstaged hunk in the second block
    let config = r"
name: root
commands:
  - name: fixer
    cmd: sed -i.bak '13s/^5$/five/' f.txt && rm f.txt.bak
    auto:
      always: true
";
    if !repo(dir, config, &[("f.txt", REPEATED)]) {
        return;
    }
    write(dir, "f.txt", REPEATED_SYNCED);
    let before = state(dir);

    let output = check(dir, &[]);
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(err.contains("fixer FAIL (modified: f.txt"), "{err}");
    assert!(err.contains("discarded"), "{err}");
    assert_eq!(read(&dir.join("f.txt")), REPEATED_SYNCED, "{err}");
    assert_eq!(state(dir), before);
    assert_clean_git_dir(dir);
}

#[test]
fn stash_keeps_fixes_to_files_without_unstaged_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let config = r#"
name: root
commands:
  - name: fixer
    cmd: 'for f in $FILES; do echo fixed > "$f"; done'
    auto:
      always: true
"#;
    let files = [("a.txt", "a\n"), ("b.txt", "b\n")];
    if !repo(dir, config, &files) {
        return;
    }
    write(dir, "a.txt", "a\nunstaged\n");

    // Only files without unstaged changes: the fix stays, and so do the unstaged changes
    let output = stash_command(dir, &[])
        .env("FILES", "b.txt")
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(!err.contains("discarded"), "{err}");
    assert_eq!(read(&dir.join("a.txt")), "a\nunstaged\n");
    assert_eq!(read(&dir.join("b.txt")), "fixed\n");
    assert_clean_git_dir(dir);

    // A file with unstaged changes too: only the fix to it is discarded
    git(dir, &["checkout", "--", "b.txt"]);
    let output = stash_command(dir, &[])
        .env("FILES", "a.txt b.txt")
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(err.contains("discarded"), "{err}");
    assert_eq!(read(&dir.join("a.txt")), "a\nunstaged\n");
    assert_eq!(read(&dir.join("b.txt")), "fixed\n");
    assert_clean_git_dir(dir);
}

/// Waits to be stopped, after writing its pid, when `SLOW` is set.
const SLOW: &str = r#"
name: root
commands:
  - name: slow
    cmd: 'if [ -n "$SLOW" ]; then echo $$ > .git/pid; exec sleep 30; fi'
    auto:
      always: true
"#;

/// A repo with [`SLOW`], where `a.txt` has staged and unstaged changes.
fn slow_repo(dir: &Path) -> bool {
    if !repo(dir, SLOW, &[("a.txt", "a\n")]) {
        return false;
    }
    write(dir, "a.txt", "staged\n");
    git(dir, &["add", "a.txt"]);
    write(dir, "a.txt", "staged\nunstaged\n");
    true
}

#[test]
fn stash_sigterm_restores() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    if !slow_repo(dir) {
        return;
    }
    let before = state(dir);

    let mut child = stash_command(dir, &["--mute-success"])
        .env("SLOW", "1")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    common::read_pid(&dir.join(".git/pid"));
    assert_eq!(read(&dir.join("a.txt")), "staged\n", "not set aside");
    kill(child.id(), libc::SIGTERM);
    let status = wait(&mut child);
    let err = stderr(&child.wait_with_output().unwrap());
    assert_eq!(status.code(), Some(143), "{err}");
    assert!(err.contains("Restoring unstaged changes"), "{err}");
    assert_eq!(read(&dir.join("a.txt")), "staged\nunstaged\n", "{err}");
    assert_eq!(state(dir), before);
    assert_clean_git_dir(dir);
}

#[test]
fn stash_sigkill_then_next_run_recovers() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    if !slow_repo(dir) {
        return;
    }
    let before = state(dir);

    kill_while_running(dir);
    assert_eq!(read(&dir.join("a.txt")), "staged\n");

    let output = check(dir, &[]);
    let err = stderr(&output);
    assert!(output.status.success(), "{err}");
    assert!(err.contains("Put back unstaged changes"), "{err}");
    assert_eq!(read(&dir.join("a.txt")), "staged\nunstaged\n");
    assert_eq!(state(dir), before);
    assert_clean_git_dir(dir);
}

/// Run `--stash` with [`SLOW`] in `dir`, and SIGKILL fnug and its command while it runs.
fn kill_while_running(dir: &Path) {
    let mut child = stash_command(dir, &["--mute-success"])
        .env("SLOW", "1")
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let sleep = common::read_pid(&dir.join(".git/pid"));
    kill(child.id(), libc::SIGKILL);
    wait(&mut child);
    // In a session of its own, so it outlives fnug
    kill(u32::try_from(sleep).unwrap(), libc::SIGKILL);
    assert!(dir.join(".git/fnug-stash.lock").exists());
}

#[test]
fn stash_sigkill_recovers_on_repeated_context() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    if !repo(dir, SLOW, &[("f.txt", REPEATED)]) {
        return;
    }
    write(dir, "f.txt", REPEATED_SYNCED);
    let before = state(dir);

    kill_while_running(dir);
    assert_eq!(read(&dir.join("f.txt")), REPEATED);

    let output = check(dir, &[]);
    let err = stderr(&output);
    assert!(output.status.success(), "{err}");
    assert!(err.contains("Put back unstaged changes"), "{err}");
    assert_eq!(read(&dir.join("f.txt")), REPEATED_SYNCED);
    assert_eq!(state(dir), before);
    assert_clean_git_dir(dir);
}

#[test]
fn stash_sigkill_then_edit_is_left_to_the_user() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    if !slow_repo(dir) {
        return;
    }
    kill_while_running(dir);
    // Edited after the kill, so fnug can't tell what the patch still has to add
    write(dir, "a.txt", "staged\nretyped\n");

    let output = check(dir, &[]);
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(2), "{err}");
    assert!(err.contains("git apply"), "{err}");
    assert_eq!(read(&dir.join("a.txt")), "staged\nretyped\n");
    assert!(dir.join(".git/fnug-stash.lock").exists());
    let patches: Vec<_> = std::fs::read_dir(dir.join(".git/fnug"))
        .unwrap()
        .flatten()
        .map(|e| read(&e.path()))
        .collect();
    assert!(
        matches!(&patches[..], [patch] if patch.contains("+unstaged")),
        "{patches:?}"
    );

    // Plain check leaves it alone, but says so
    let output = fnug(dir, &[]).output().unwrap();
    let err = stderr(&output);
    assert!(err.contains("fnug-stash.lock"), "{err}");
    assert!(dir.join(".git/fnug-stash.lock").exists());
}

#[test]
fn stash_concurrent_lock_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    if !slow_repo(dir) {
        return;
    }
    let before = state(dir);
    let lock = dir.join(".git/fnug-stash.lock");
    // Held by a live process: this test
    let held = format!("fnug-stash-lock 1\0pid\0{}\0", std::process::id());
    std::fs::write(&lock, held).unwrap();

    let output = check(dir, &[]);
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(2), "{err}");
    assert!(err.contains("another `fnug check --stash`"), "{err}");
    assert_eq!(state(dir), before);
    assert!(lock.exists());

    std::fs::write(&lock, "garbage").unwrap();
    let output = check(dir, &[]);
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(2), "{err}");
    assert!(err.contains("is not a lock fnug can read"), "{err}");
    assert_eq!(state(dir), before);
}

#[test]
fn stash_unborn_head() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    if !common::git::available() {
        return;
    }
    common::git::init(dir);
    common::write_config(dir, LINT);
    write(dir, "src/a.py", "BAD\n");
    git(dir, &["add", "src/a.py"]);
    write(dir, "src/a.py", "ok\n");
    let before = state(dir);

    let output = check(dir, &[]);
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("src/a.py:1:BAD"),
        "{}",
        printed(&output)
    );
    assert_eq!(read(&dir.join("src/a.py")), "ok\n");
    assert_eq!(state(dir), before);
    assert_clean_git_dir(dir);
}

#[test]
fn stash_in_commit_hook_checks_what_is_committed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    if !repo(dir, LINT, &[("src/a.py", "ok\n")]) {
        return;
    }
    let hook = dir.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    // With the arguments `fnug setup` installs
    let args = fnug::setup::hooks::hook_args(true).join(" ");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\nexec '{}' {args}\n", env!("CARGO_BIN_EXE_fnug")),
    )
    .unwrap();
    std::fs::set_permissions(&hook, PermissionsExt::from_mode(0o755)).unwrap();
    let commit = |args: &[&str]| {
        common::git::command(dir)
            .arg("commit")
            .args(args)
            .env_remove("FNUG_LOG")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };

    // BAD only in the work tree doesn't block, and stays unstaged
    write(dir, "src/a.py", "ok\nfine\n");
    git(dir, &["add", "src/a.py"]);
    write(dir, "src/a.py", "ok\nfine\nBAD\n");
    let output = commit(&["-qm", "fine"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(git(dir, &["show", "HEAD:src/a.py"]), "ok\nfine\n");
    assert_eq!(read(&dir.join("src/a.py")), "ok\nfine\nBAD\n");

    // BAD staged blocks, even when the work tree no longer has it
    git(dir, &["add", "src/a.py"]);
    write(dir, "src/a.py", "ok\nfine\n");
    let output = commit(&["-qm", "bad"]);
    assert!(!output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("src/a.py:3:BAD"),
        "{}",
        stderr(&output)
    );
    assert_eq!(read(&dir.join("src/a.py")), "ok\nfine\n");
    assert_eq!(git(dir, &["show", ":src/a.py"]), "ok\nfine\nBAD\n");

    // `commit -a` commits the work tree, which is fine
    write(dir, "src/a.py", "ok\nfine\nmore\n");
    let output = commit(&["-qam", "all"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(git(dir, &["show", "HEAD:src/a.py"]), "ok\nfine\nmore\n");
    assert_clean_git_dir(dir);
}

/// Records, in `$SEEN`, what the command sees as `a.txt` and which files its directory holds.
const SUB_RECORD: &str = r#"
name: sub
commands:
  - name: record
    cmd: 'cat a.txt > "$SEEN"; ls >> "$SEEN"'
    auto:
      always: true
"#;

/// Files for [`commit_in_sub_sees_staged_content`]: an `a.txt` at the top too, which a
/// checkout relative to `sub/` would put there.
const SUB_FILES: &[(&str, &str)] = &[("a.txt", "top\n"), ("sub/a.txt", "sub\n")];

/// Commit through the pre-commit hook `fnug setup` installs for a config in `top/sub`, where
/// `sub/a.txt` has staged and unstaged changes, and check that the command saw exactly the
/// staged `sub/a.txt`. `git_dir` is the work tree's own git dir.
fn commit_in_sub_sees_staged_content(top: &Path, git_dir: &Path) {
    use fnug::setup::hooks;

    write(top, "sub/a.txt", "sub staged\n");
    git(top, &["add", "sub/a.txt"]);
    write(top, "sub/a.txt", "sub staged\nunstaged\n");

    // The hook runs whichever `fnug` is first on PATH
    let bin = top.parent().unwrap().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fnug"), bin.join("fnug")).unwrap();
    let opts = hooks::InstallOptions {
        no_workspace: true,
        ..hooks::InstallOptions::default()
    };
    hooks::install_with(&hooks::resolve(&top.join("sub")).unwrap(), &opts).unwrap();

    let seen = top.parent().unwrap().join("seen");
    let path = std::env::var("PATH").unwrap_or_default();
    let output = common::git::command(top)
        .args(["commit", "-qm", "sub"])
        .env("PATH", format!("{}:{path}", bin.display()))
        .env("SEEN", &seen)
        .env_remove("FNUG_LOG")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    // Not the top's a.txt, nor a copy of the index under sub/
    assert_eq!(read(&seen), "sub staged\na.txt\n", "{}", printed(&output));
    assert_eq!(git(top, &["show", "HEAD:sub/a.txt"]), "sub staged\n");
    assert_eq!(read(&top.join("sub/a.txt")), "sub staged\nunstaged\n");
    assert_eq!(read(&top.join("a.txt")), "top\n");
    assert!(!top.join("sub/sub").exists());
    assert!(!git_dir.join("fnug-stash.lock").exists());
}

#[test]
fn stash_hook_in_linked_worktree_with_config_in_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir(&main).unwrap();
    if !repo(&main, "name: root\n", SUB_FILES) {
        return;
    }
    common::write_config(&main.join("sub"), SUB_RECORD);
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-qm", "sub config"]);
    // Absolute, since a linked worktree's `.git` is a file
    let hooks = main.join(".git/hooks");
    git(
        &main,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let wt = tmp.path().join("wt");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);

    commit_in_sub_sees_staged_content(&wt, &main.join(".git/worktrees/wt"));
}

#[test]
fn stash_hook_in_separate_git_dir_with_config_in_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    if !common::git::available() {
        return;
    }
    let top = tmp.path().join("wt");
    let git_dir = tmp.path().join("gd");
    let (top_arg, git_dir_arg) = (top.to_str().unwrap(), git_dir.to_str().unwrap());
    git(
        tmp.path(),
        &["init", "-q", "--separate-git-dir", git_dir_arg, top_arg],
    );
    let hooks = git_dir.join("hooks");
    git(&top, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    for (path, content) in SUB_FILES {
        write(&top, path, content);
    }
    common::write_config(&top.join("sub"), SUB_RECORD);
    git(&top, &["add", "-A"]);
    git(&top, &["commit", "-qm", "init"]);

    commit_in_sub_sees_staged_content(&top, &git_dir);
}
