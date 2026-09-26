//! Tests for the shared runner: planning with git selection.

mod common;

use fnug::runner::{PlanError, PlanOptions, SelectReason, Selection, plan};
use fnug::selectors::{GitScope, SelectOptions, SelectionIssue};

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
