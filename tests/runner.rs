//! Tests for the shared runner: planning with git selection, and executing plans with real
//! processes.

mod common;

use std::ffi::CString;
use std::num::NonZeroUsize;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::time::{Duration, Instant};

use fnug::commands::group::CommandGroup;
use fnug::process::StopSignal;
use fnug::runner::{
    CaptureLimits, Counts, ExecHook, ExecOptions, Failure, NoHook, Outcome, OutputMode, Plan,
    PlanError, PlanOptions, PlannedCommand, RunEvent, RunReport, SelectReason, Selection, execute,
    plan,
};
use fnug::selectors::{GitScope, SelectOptions, SelectionIssue};

const TIMEOUT: Duration = Duration::from_secs(10);

// ─── planning ───

const GIT_CONFIG: &str = r"
name: root
commands:
  - name: rust
    cmd: 'true'
    auto:
      git: true
      regex: ['\.rs$']
  - name: docs
    cmd: 'true'
    auto:
      git: true
      regex: ['\.md$']
  - name: always
    cmd: 'true'
    auto:
      always: true
";

fn auto(scope: GitScope) -> Selection {
    Selection::Auto {
        options: SelectOptions {
            scope,
            ..SelectOptions::default()
        },
        include_manual: false,
    }
}

#[test]
fn auto_plan_carries_matched_files() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let (config, _) = common::load(dir.path(), GIT_CONFIG);
    common::commit_all(&repo);
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

    let plan = plan(
        &config,
        &auto(GitScope::WorkingTree),
        &PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.ids().collect::<Vec<_>>(), ["rust", "always"]);
    let rust = plan.get("rust").unwrap();
    assert_eq!(rust.reason, SelectReason::Git);
    let file = dir.path().canonicalize().unwrap().join("main.rs");
    assert_eq!(rust.files.as_deref(), Some(&[file][..]));
    let always = plan.get("always").unwrap();
    assert_eq!(
        (&always.reason, &always.files),
        (&SelectReason::Always, &None)
    );
    assert_eq!(plan.changed_files, 1);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
}

#[test]
fn unknown_base_is_a_plan_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let (config, _) = common::load(dir.path(), GIT_CONFIG);
    common::commit_all(&repo);

    let err = plan(
        &config,
        &auto(GitScope::Since("no-such-ref".into())),
        &PlanOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(&err, PlanError::Selection(issues)
            if matches!(issues.as_slice(), [SelectionIssue::BaseRefNotFound { .. }])),
        "{err:?}"
    );
    assert!(err.to_string().contains("no-such-ref"), "{err}");
}

// ─── execution ───

fn capture() -> ExecOptions {
    ExecOptions {
        output: OutputMode::Capture(CaptureLimits::DEFAULT),
        ..ExecOptions::default()
    }
}

fn all() -> Selection {
    Selection::All {
        include_manual: false,
    }
}

async fn run(config: &CommandGroup, cwd: &Path, opts: &ExecOptions) -> RunReport {
    run_with(config, cwd, opts, &NoHook).await
}

async fn run_with<H: ExecHook>(
    config: &CommandGroup,
    cwd: &Path,
    opts: &ExecOptions,
    hook: &H,
) -> RunReport {
    let plan = plan(config, &all(), &PlanOptions::default()).unwrap();
    execute(&plan, cwd, opts, hook, &mut |_| {}).await
}

fn outcome<'a>(report: &'a RunReport, id: &str) -> &'a Outcome {
    &report.get(id).unwrap().outcome
}

fn output(report: &RunReport, id: &str) -> String {
    report.get(id).unwrap().output.as_ref().unwrap().text()
}

const BUILD_CHAIN: &str = r"
name: root
commands:
  - name: build
    cmd: 'exit 1'
  - name: test
    cmd: 'true'
    depends_on: [build]
  - name: lint
    cmd: 'true'
    depends_on: [test]
  - name: other
    cmd: 'true'
";

#[tokio::test]
async fn counts_failed_and_skipped_separately() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(dir.path(), BUILD_CHAIN);
    let plan = plan(&config, &all(), &PlanOptions::default()).unwrap();
    let mut events = Vec::new();
    let report = execute(&plan, &cwd, &capture(), &NoHook, &mut |event| {
        events.push(match event {
            RunEvent::Started { seq, cmd, .. } => format!("start {seq} {}", cmd.id()),
            RunEvent::Finished { seq, done, cmd, .. } => {
                format!("finish {seq} {done} {}", cmd.id())
            }
        });
    })
    .await;

    assert_eq!(
        report.counts(),
        Counts {
            total: 4,
            passed: 1,
            failed: 1,
            skipped: 2,
            ..Counts::default()
        }
    );
    assert_eq!(
        *outcome(&report, "build"),
        Outcome::Failed(Failure::Exit(1))
    );
    assert_eq!(
        *outcome(&report, "lint"),
        Outcome::Skipped {
            cause: "build".into()
        }
    );
    let ids: Vec<&str> = report.commands.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["build", "other", "test", "lint"]);
    assert_eq!(
        events,
        [
            "start 1 build",
            "finish 1 1 build",
            "finish 2 2 test",
            "finish 3 3 lint",
            "start 4 other",
            "finish 4 4 other",
        ]
    );
    assert!(!report.success());
    assert_eq!(report.rerun_ids(), ["build", "test", "lint"]);
}

#[tokio::test]
async fn fail_fast_reports_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: first
    cmd: 'exit 2'
  - name: second
    cmd: 'touch second-ran'
  - name: third
    cmd: 'true'
",
    );
    let opts = ExecOptions {
        fail_fast: true,
        ..capture()
    };
    let report = run(&config, &cwd, &opts).await;
    let counts = report.counts();
    assert_eq!((counts.failed, counts.not_run), (1, 2), "{counts:?}");
    assert_eq!(*outcome(&report, "second"), Outcome::NotRun);
    assert!(!dir.path().join("second-ran").exists());
    assert!(!report.cancelled);
}

/// Each command marks itself started, then waits until all three have.
const OVERLAP: &str = r"
name: root
commands:
  - name: a
    cmd: &wait 'touch $MARK; i=0; until [ -e a ] && [ -e b ] && [ -e c ]; do i=$((i+1)); [ $i -gt 250 ] && exit 1; sleep 0.02; done'
    env: {MARK: a}
  - name: b
    cmd: *wait
    env: {MARK: b}
  - name: c
    cmd: *wait
    env: {MARK: c}
";

#[tokio::test]
async fn parallel_independent_commands_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(dir.path(), OVERLAP);
    let opts = ExecOptions {
        jobs: NonZeroUsize::new(3).unwrap(),
        ..capture()
    };
    let report = run(&config, &cwd, &opts).await;
    assert!(report.success(), "{report:#?}");
}

#[tokio::test]
async fn inherit_mode_runs_one_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: a
    cmd: &alone 'test ! -e running && touch running && sleep 0.2 && rm running'
  - name: b
    cmd: *alone
",
    );
    let opts = ExecOptions {
        jobs: NonZeroUsize::new(4).unwrap(),
        ..ExecOptions::default()
    };
    let report = run(&config, &cwd, &opts).await;
    assert!(report.success(), "{report:#?}");
    assert!(report.get("a").unwrap().output.is_none());
}

#[test]
fn exclusive_is_inherited() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = common::load(
        dir.path(),
        r"
name: root
exclusive: true
commands:
  - name: top
    cmd: 'true'
children:
  - name: group
    exclusive: false
    commands:
      - name: plain
        cmd: 'true'
      - name: own
        cmd: 'true'
        exclusive: true
",
    );
    let exclusive: Vec<&str> = config
        .all_commands()
        .into_iter()
        .filter(|c| c.is_exclusive())
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(exclusive, ["top", "own"]);
}

#[tokio::test]
async fn exclusive_command_runs_alone() {
    let dir = tempfile::tempdir().unwrap();
    // Run together, `fix` would find `a.running`, and `b` and `c` would miss `fix.done`
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: a
    cmd: 'touch a.running; sleep 0.3; rm a.running'
  - name: fix
    cmd: 'sleep 0.1; test ! -e a.running && touch fix.done'
    exclusive: true
  - name: b
    cmd: 'test -e fix.done'
  - name: c
    cmd: 'test -e fix.done'
",
    );
    let opts = ExecOptions {
        jobs: NonZeroUsize::new(4).unwrap(),
        ..capture()
    };
    let report = run(&config, &cwd, &opts).await;
    assert!(report.success(), "{report:#?}");
}

#[tokio::test]
async fn parallel_respects_deps() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: test
    cmd: 'test -e built'
    depends_on: [build]
  - name: build
    cmd: 'sleep 0.3 && touch built'
  - name: unrelated
    cmd: 'true'
",
    );
    let opts = ExecOptions {
        jobs: NonZeroUsize::new(4).unwrap(),
        ..capture()
    };
    let report = run(&config, &cwd, &opts).await;
    assert!(report.success(), "{report:#?}");
}

const BACKGROUND_SLEEP: &str = r"
name: root
commands:
  - name: hang
    cmd: 'sleep 30 & echo $! > pid; wait'
  - name: after
    cmd: 'true'
    depends_on: [hang]
";

#[tokio::test]
async fn timeout_kills_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(dir.path(), BACKGROUND_SLEEP);
    let timeout = Duration::from_millis(300);
    let opts = ExecOptions {
        default_timeout: Some(timeout),
        ..capture()
    };
    let started = Instant::now();
    let report = run(&config, &cwd, &opts).await;
    assert!(started.elapsed() < TIMEOUT, "took {:?}", started.elapsed());
    assert_eq!(*outcome(&report, "hang"), Outcome::TimedOut(timeout));
    assert_eq!(
        *outcome(&report, "after"),
        Outcome::Skipped {
            cause: "hang".into()
        }
    );
    let pid = common::read_pid(&dir.path().join("pid"));
    assert!(common::wait_until(TIMEOUT, || !common::process_alive(pid)));
}

#[test]
fn timeout_is_inherited_and_zero_disables_it() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = common::load(
        dir.path(),
        r"
name: root
timeout: 5m
commands:
  - name: top
    cmd: 'true'
children:
  - name: group
    timeout: 30
    commands:
      - name: plain
        cmd: 'true'
      - name: own
        cmd: 'true'
        timeout: 1m 30s
      - name: off
        cmd: 'true'
        timeout: 0
",
    );
    let timeout = |id: &str| {
        let cmd = config.all_commands().into_iter().find(|c| c.id == id);
        cmd.unwrap().timeout
    };
    assert_eq!(timeout("top"), Some(Duration::from_secs(300)));
    assert_eq!(timeout("plain"), Some(Duration::from_secs(30)));
    assert_eq!(timeout("own"), Some(Duration::from_secs(90)));
    assert_eq!(timeout("off"), Some(Duration::ZERO));
}

#[tokio::test]
async fn command_timeout_overrides_default() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: default
    cmd: 'exec sleep 30'
  - name: short
    cmd: 'exec sleep 30'
    timeout: 300ms
  - name: long
    cmd: 'sleep 0.5'
    timeout: 10
  - name: off
    cmd: 'sleep 0.5'
    timeout: 0
",
    );
    let default = Duration::from_millis(200);
    let opts = ExecOptions {
        jobs: NonZeroUsize::new(4).unwrap(),
        default_timeout: Some(default),
        ..capture()
    };
    let report = run(&config, &cwd, &opts).await;
    assert_eq!(*outcome(&report, "default"), Outcome::TimedOut(default));
    assert_eq!(
        *outcome(&report, "short"),
        Outcome::TimedOut(Duration::from_millis(300))
    );
    assert_eq!(*outcome(&report, "long"), Outcome::Passed);
    assert_eq!(*outcome(&report, "off"), Outcome::Passed);
}

#[tokio::test]
async fn cancel_kills_group() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(dir.path(), BACKGROUND_SLEEP);
    let opts = capture();
    let cancel = opts.cancel.clone();
    let pid_file = dir.path().join("pid");
    let canceller = tokio::task::spawn_blocking(move || {
        let pid = common::read_pid(&pid_file);
        cancel.cancel();
        pid
    });
    let started = Instant::now();
    let report = run(&config, &cwd, &opts).await;
    assert!(started.elapsed() < TIMEOUT, "took {:?}", started.elapsed());
    assert!(report.cancelled);
    assert_eq!(*outcome(&report, "hang"), Outcome::Cancelled);
    assert_eq!(*outcome(&report, "after"), Outcome::NotRun);
    let pid = canceller.await.unwrap();
    assert!(common::wait_until(TIMEOUT, || !common::process_alive(pid)));
}

/// Writes the name of the signal it gets to `caught` a moment later, and exits. Short sleeps,
/// since a trap waits for the command that runs when its signal arrives.
const GRACEFUL: &str = r#"
name: root
commands:
  - name: graceful
    cmd: 'for s in INT HUP TERM; do trap "sleep 0.2; echo $s > caught; exit 1" $s; done; touch started; i=0; while [ $i -lt 600 ]; do i=$((i+1)); sleep 0.05; done'
"#;

#[tokio::test]
async fn cancel_passes_on_the_signal_fnug_got() {
    for (signal, expected) in [
        (None, "TERM"),
        (Some(StopSignal::Interrupt), "INT"),
        (Some(StopSignal::Terminate), "TERM"),
        (Some(StopSignal::Hangup), "HUP"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (config, cwd) = common::load(dir.path(), GRACEFUL);
        let opts = capture();
        if let Some(signal) = signal {
            opts.cancel_cause.set_signal(signal);
        }
        let cancel = opts.cancel.clone();
        let started = dir.path().join("started");
        tokio::task::spawn_blocking(move || {
            assert!(common::wait_until(TIMEOUT, || started.exists()));
            cancel.cancel();
        });
        let report = run(&config, &cwd, &opts).await;
        assert_eq!(
            *outcome(&report, "graceful"),
            Outcome::Cancelled,
            "{signal:?}"
        );
        let caught = std::fs::read_to_string(dir.path().join("caught")).unwrap_or_default();
        assert_eq!(caught.trim(), expected, "{signal:?}: {report:#?}");
    }
}

#[tokio::test]
async fn failure_ending_during_fail_fast_keeps_its_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = CString::new(dir.path().join("exited").into_os_string().into_vec()).unwrap();
    // SAFETY: plain syscall with a valid, NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    // `own` exits 3 by itself once its TERM-proof background process is up, and its run
    // lingers on the pipe that process holds. `fail` fails once `own`'s shell has exited and
    // closed the fifo, so fail-fast stops the run before `own`'s outcome is known.
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: own
    cmd: 'exec 3> exited; (trap '''' TERM; exec 3>&-; touch ready; exec sleep 5) & i=0; until [ -e ready ]; do i=$((i+1)); [ $i -gt 500 ] && exit 9; sleep 0.01; done; exit 3'
  - name: fail
    cmd: 'cat exited; exit 1'
",
    );
    let opts = ExecOptions {
        fail_fast: true,
        jobs: NonZeroUsize::new(2).unwrap(),
        ..capture()
    };
    let report = tokio::time::timeout(TIMEOUT, run(&config, &cwd, &opts))
        .await
        .unwrap();
    assert_eq!(*outcome(&report, "fail"), Outcome::Failed(Failure::Exit(1)));
    assert_eq!(*outcome(&report, "own"), Outcome::Failed(Failure::Exit(3)));
    assert!(!report.cancelled);
}

#[tokio::test]
async fn cancelled_before_start_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(dir.path(), BUILD_CHAIN);
    let opts = capture();
    opts.cancel.cancel();
    let report = run(&config, &cwd, &opts).await;
    assert_eq!(report.counts().not_run, 4);
    assert!(report.cancelled && !report.success());
}

#[tokio::test]
async fn leftover_background_process_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: spawner
    cmd: 'sleep 30 & echo $! > pid; echo done'
",
    );
    let started = Instant::now();
    let report = run(&config, &cwd, &capture()).await;
    assert!(started.elapsed() < TIMEOUT, "took {:?}", started.elapsed());
    assert_eq!(*outcome(&report, "spawner"), Outcome::Passed);
    assert_eq!(output(&report, "spawner"), "done\n");
    let pid = common::read_pid(&dir.path().join("pid"));
    assert!(common::wait_until(TIMEOUT, || !common::process_alive(pid)));
}

#[tokio::test]
async fn merged_output_preserves_order() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: mixed
    cmd: 'echo o1; echo e1 >&2; echo o2; printf no-newline >&2'
",
    );
    let report = run(&config, &cwd, &capture()).await;
    assert_eq!(output(&report, "mixed"), "o1\ne1\no2\nno-newline");
}

#[tokio::test]
async fn exit_code_and_signal_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: code
    cmd: 'exit 3'
  - name: killed
    cmd: 'kill -9 $$'
",
    );
    for opts in [capture(), ExecOptions::default()] {
        let report = run(&config, &cwd, &opts).await;
        assert_eq!(*outcome(&report, "code"), Outcome::Failed(Failure::Exit(3)));
        assert_eq!(
            *outcome(&report, "killed"),
            Outcome::Failed(Failure::Signal(9))
        );
    }
}

#[tokio::test]
async fn spawn_error_reported() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: gone
    cmd: 'true'
    cwd: sub
  - name: after
    cmd: 'true'
    depends_on: [gone]
",
    );
    std::fs::remove_dir(dir.path().join("sub")).unwrap();
    for opts in [capture(), ExecOptions::default()] {
        let report = run(&config, &cwd, &opts).await;
        let Outcome::Failed(Failure::Spawn(message)) = outcome(&report, "gone") else {
            panic!("{report:#?}");
        };
        assert!(
            message.contains("sub") && message.contains("does not exist"),
            "{message}"
        );
        assert_eq!(report.counts().skipped, 1);
    }
}

#[tokio::test]
async fn capture_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: flood
    cmd: 'head -c 10000000 /dev/zero | tr ''\0'' a; echo; echo last'
",
    );
    let opts = ExecOptions {
        output: OutputMode::Capture(CaptureLimits {
            head: 1024,
            tail: 1024,
        }),
        ..ExecOptions::default()
    };
    let report = run(&config, &cwd, &opts).await;
    assert_eq!(*outcome(&report, "flood"), Outcome::Passed);
    let captured = report.get("flood").unwrap().output.as_ref().unwrap();
    assert_eq!(captured.total_bytes(), 10_000_006);
    assert_eq!(captured.omitted_bytes(), 10_000_006 - 2048);
    let text = captured.text();
    assert!(text.contains("bytes omitted"));
    assert!(text.ends_with("a\nlast\n"), "{}", &text[text.len() - 20..]);
}

/// Fails every command that passed, as a check for modified files would.
struct FailPassing;

impl ExecHook for FailPassing {
    type Token = String;

    fn before(&self, cmd: &PlannedCommand) -> String {
        cmd.id().to_string()
    }

    fn after(&self, cmd: &PlannedCommand, token: String, outcome: &mut Outcome) {
        assert_eq!(token, cmd.id());
        if *outcome == Outcome::Passed {
            *outcome = Outcome::Failed(Failure::Modified(vec![token.into()]));
        }
    }
}

#[tokio::test]
async fn hook_can_fail_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let (config, cwd) = common::load(
        dir.path(),
        r"
name: root
commands:
  - name: fixer
    cmd: 'true'
  - name: after
    cmd: 'true'
    depends_on: [fixer]
",
    );
    let report = run_with(&config, &cwd, &capture(), &FailPassing).await;
    assert_eq!(
        *outcome(&report, "fixer"),
        Outcome::Failed(Failure::Modified(vec!["fixer".into()]))
    );
    assert_eq!(report.counts().skipped, 1);
}

#[test]
fn execute_future_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let plan = Plan::default();
    let opts = ExecOptions::default();
    let mut on_event = |_: RunEvent<'_>| {};
    let future = execute(&plan, Path::new("."), &opts, &NoHook, &mut on_event);
    assert_send(&future);
}
