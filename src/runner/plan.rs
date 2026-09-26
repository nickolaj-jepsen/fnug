//! Which commands a run executes, and in what order.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use thiserror::Error;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::commands::ids::{ResolveError, resolve_command};
use crate::selectors::{self, SelectOptions, SelectedBy, SelectionIssue};

/// The commands a run starts from, before their dependencies are added.
#[derive(Debug, Clone)]
pub enum Selection {
    /// Commands selected by their `auto` rules: `always`, or `git` with a matching change.
    /// Commands with `auto.check: false` are left out unless `include_manual` is set.
    Auto {
        options: SelectOptions,
        include_manual: bool,
    },
    /// Every command. Commands with `auto.check: false` are left out unless `include_manual`
    /// is set.
    All { include_manual: bool },
    /// Commands named by id or name, see [`resolve_target`]. `auto.check` doesn't apply.
    Targets(Vec<String>),
    /// Commands by exact id. `auto.check` doesn't apply.
    Ids(Vec<String>),
}

/// Why a command is in a [`Plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectReason {
    /// Named by [`Selection::Targets`] or [`Selection::Ids`].
    Requested,
    /// Part of [`Selection::All`].
    All,
    /// Selected by `auto.always`.
    Always,
    /// Selected by a changed file.
    Git,
    /// Needed by the listed commands' `depends_on`, in config order.
    Dependency { of: Vec<String> },
}

/// A command in a [`Plan`].
#[derive(Debug, Clone)]
pub struct PlannedCommand {
    pub command: Command,
    pub reason: SelectReason,
    /// For a git-enabled command chosen by [`Selection::Auto`]: the changed files matching its
    /// `auto` rules, absolute, that existed when it was selected. `None` otherwise.
    pub files: Option<Vec<PathBuf>>,
    /// The command's `depends_on` entries that are in the plan.
    pub deps: Vec<String>,
    /// Names of the groups from the root down to the command, joined with ` > `.
    pub group_path: String,
}

impl PlannedCommand {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.command.id
    }
}

/// The commands a run executes, each after its dependencies.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Topological order; commands that become ready together keep their config order.
    pub commands: Vec<PlannedCommand>,
    /// Ids of selected commands left out because of `auto.check: false`, in config order.
    pub excluded_manual: Vec<String>,
    /// Distinct changed paths that git selection found; 0 unless the selection was `Auto`.
    pub changed_files: usize,
    /// Problems that kept part of the selection from running, such as an `auto.path` outside
    /// any git repo.
    pub warnings: Vec<String>,
}

impl Plan {
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&PlannedCommand> {
        self.commands.iter().find(|c| c.id() == id)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.commands.iter().map(PlannedCommand::id)
    }
}

/// How [`plan`] adds dependencies.
#[derive(Default, Clone, Copy)]
pub struct PlanOptions<'a> {
    /// True for a dependency that need not run again, such as one that already passed in the
    /// TUI. It is left out along with the dependencies only it needs, unless the selection
    /// itself includes it.
    pub reuse_dep: Option<&'a dyn Fn(&Command) -> bool>,
}

/// Why [`plan`] could not build a plan.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    /// A target or id names no command. `suggestions` holds similar ids.
    #[error("{}", ResolveError::NotFound { reference: target.clone(), suggestions: suggestions.clone() })]
    NotFound {
        target: String,
        suggestions: Vec<String>,
    },
    /// A target is the name of several commands. `candidates` holds `(id, group path)` of each.
    #[error("{}", ResolveError::Ambiguous { reference: target.clone(), candidates: candidates.clone() })]
    Ambiguous {
        target: String,
        candidates: Vec<(String, String)>,
    },
    /// Git selection is unusable as a whole, see [`SelectionIssue::is_fatal`].
    #[error("{}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Selection(Vec<SelectionIssue>),
}

impl From<ResolveError> for PlanError {
    fn from(e: ResolveError) -> Self {
        match e {
            ResolveError::NotFound {
                reference,
                suggestions,
            } => PlanError::NotFound {
                target: reference,
                suggestions,
            },
            ResolveError::Ambiguous {
                reference,
                candidates,
            } => PlanError::Ambiguous {
                target: reference,
                candidates,
            },
        }
    }
}

/// Find the command `target` names: the command with that exact id, or else the only command
/// whose name matches it case-insensitively.
///
/// # Errors
///
/// Returns `PlanError::Ambiguous` if several commands have that name, and
/// `PlanError::NotFound` if none has that id or name.
pub fn resolve_target<'a>(
    config: &'a CommandGroup,
    target: &str,
) -> Result<&'a Command, PlanError> {
    resolve_command(config, target).map_err(PlanError::from)
}

/// Every command in config order, with the names of its groups joined by ` > `.
pub(crate) fn commands_with_group_path(config: &CommandGroup) -> Vec<(&Command, String)> {
    fn walk<'a>(group: &'a CommandGroup, path: &str, out: &mut Vec<(&'a Command, String)>) {
        out.extend(group.commands.iter().map(|cmd| (cmd, path.to_string())));
        for child in &group.children {
            walk(child, &format!("{path} > {}", child.name), out);
        }
    }
    let mut out = Vec::new();
    walk(config, &config.name, &mut out);
    out
}

/// Choose the commands `selection` names, add everything they depend on, and order the result
/// so each command comes after its dependencies.
///
/// # Errors
///
/// Returns `PlanError::NotFound` or `PlanError::Ambiguous` for a target or id that names no
/// single command, and `PlanError::Selection` when git selection fails as a whole. Other
/// selection problems become [`Plan::warnings`].
pub fn plan(
    config: &CommandGroup,
    selection: &Selection,
    opts: &PlanOptions,
) -> Result<Plan, PlanError> {
    let entries = commands_with_group_path(config);
    let index: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, (cmd, _))| (cmd.id.as_str(), i))
        .collect();
    let mut plan = Plan::default();
    let mut chosen: HashMap<usize, (SelectReason, Option<Vec<PathBuf>>)> = HashMap::new();
    let manual = |cmd: &Command| cmd.auto.check == Some(false);

    match selection {
        Selection::Auto {
            options,
            include_manual,
        } => {
            let commands: Vec<&Command> = entries.iter().map(|(cmd, _)| *cmd).collect();
            let output = selectors::select(&commands, options);
            let (fatal, other): (Vec<_>, Vec<_>) = output
                .issues
                .into_iter()
                .partition(SelectionIssue::is_fatal);
            if !fatal.is_empty() {
                return Err(PlanError::Selection(fatal));
            }
            plan.warnings = other.iter().map(ToString::to_string).collect();
            plan.changed_files = output.changed_files;
            for selected in output.commands {
                let Some(&i) = index.get(selected.id.as_str()) else {
                    continue;
                };
                let cmd = entries[i].0;
                if manual(cmd) && !include_manual {
                    plan.excluded_manual.push(selected.id);
                    continue;
                }
                let reason = match selected.by {
                    SelectedBy::Always => SelectReason::Always,
                    SelectedBy::Git => SelectReason::Git,
                };
                let files = (cmd.auto.git == Some(true)).then_some(selected.files);
                chosen.insert(i, (reason, files));
            }
        }
        Selection::All { include_manual } => {
            for (i, (cmd, _)) in entries.iter().enumerate() {
                if manual(cmd) && !include_manual {
                    plan.excluded_manual.push(cmd.id.clone());
                } else {
                    chosen.insert(i, (SelectReason::All, None));
                }
            }
        }
        Selection::Targets(targets) => {
            for target in targets {
                let cmd = resolve_target(config, target)?;
                chosen.insert(index[cmd.id.as_str()], (SelectReason::Requested, None));
            }
        }
        Selection::Ids(ids) => {
            for id in ids {
                let i = *index.get(id.as_str()).ok_or_else(|| PlanError::NotFound {
                    target: id.clone(),
                    suggestions: Vec::new(),
                })?;
                chosen.insert(i, (SelectReason::Requested, None));
            }
        }
    }

    let included = expand_dependencies(&entries, &index, &chosen, opts);
    let ordered = topo_sort(&entries, &index, &included);
    let in_plan: HashSet<&str> = ordered.iter().map(|&i| entries[i].0.id.as_str()).collect();

    plan.commands = ordered
        .iter()
        .map(|&i| {
            let (cmd, group_path) = &entries[i];
            let (reason, files) = chosen.remove(&i).unwrap_or_else(|| {
                let of = entries
                    .iter()
                    .enumerate()
                    .filter(|(j, (other, _))| {
                        included.contains(j) && other.depends_on.contains(&cmd.id)
                    })
                    .map(|(_, (other, _))| other.id.clone())
                    .collect();
                (SelectReason::Dependency { of }, None)
            });
            PlannedCommand {
                command: (*cmd).clone(),
                reason,
                files,
                deps: cmd
                    .depends_on
                    .iter()
                    .filter(|d| in_plan.contains(d.as_str()))
                    .cloned()
                    .collect(),
                group_path: group_path.clone(),
            }
        })
        .collect();
    Ok(plan)
}

/// Indices of the chosen commands plus everything they transitively depend on, minus
/// dependencies `opts.reuse_dep` lets be skipped.
fn expand_dependencies(
    entries: &[(&Command, String)],
    index: &HashMap<&str, usize>,
    chosen: &HashMap<usize, (SelectReason, Option<Vec<PathBuf>>)>,
    opts: &PlanOptions,
) -> HashSet<usize> {
    let mut included: HashSet<usize> = chosen.keys().copied().collect();
    let mut queue: Vec<usize> = included.iter().copied().collect();
    queue.sort_unstable();
    let mut queue = VecDeque::from(queue);
    while let Some(i) = queue.pop_front() {
        for dep in &entries[i].0.depends_on {
            let Some(&j) = index.get(dep.as_str()) else {
                continue;
            };
            if included.contains(&j) || opts.reuse_dep.is_some_and(|reuse| reuse(entries[j].0)) {
                continue;
            }
            included.insert(j);
            queue.push_back(j);
        }
    }
    included
}

/// Kahn's algorithm over the included commands, seeded in config order.
fn topo_sort(
    entries: &[(&Command, String)],
    index: &HashMap<&str, usize>,
    included: &HashSet<usize>,
) -> Vec<usize> {
    let members: Vec<usize> = (0..entries.len())
        .filter(|i| included.contains(i))
        .collect();
    let mut in_degree: HashMap<usize, usize> = HashMap::new();
    let mut dependents: HashMap<usize, Vec<usize>> = HashMap::new();
    for &i in &members {
        let deps: Vec<usize> = entries[i]
            .0
            .depends_on
            .iter()
            .filter_map(|d| index.get(d.as_str()).copied())
            .filter(|j| included.contains(j))
            .collect();
        in_degree.insert(i, deps.len());
        for j in deps {
            dependents.entry(j).or_default().push(i);
        }
    }

    let mut queue: VecDeque<usize> = members
        .iter()
        .copied()
        .filter(|i| in_degree[i] == 0)
        .collect();
    let mut result = Vec::with_capacity(members.len());
    while let Some(i) = queue.pop_front() {
        result.push(i);
        for &dependent in dependents.get(&i).map(Vec::as_slice).unwrap_or_default() {
            let degree = in_degree
                .get_mut(&dependent)
                .expect("dependent is a member");
            *degree -= 1;
            if *degree == 0 {
                queue.push_back(dependent);
            }
        }
    }

    // Config validation rejects cycles, so every member is emitted.
    debug_assert_eq!(result.len(), members.len(), "dependency cycle in plan");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(yaml: &str) -> CommandGroup {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".fnug.yaml");
        std::fs::write(&path, yaml).unwrap();
        crate::load_config(path.to_str(), true).unwrap().0
    }

    fn ids(plan: &Plan) -> Vec<&str> {
        plan.ids().collect()
    }

    fn auto(include_manual: bool) -> Selection {
        Selection::Auto {
            options: SelectOptions::default(),
            include_manual,
        }
    }

    fn targets(names: &[&str]) -> Selection {
        Selection::Targets(names.iter().map(ToString::to_string).collect())
    }

    const CHAIN: &str = r"
name: root
commands:
  - name: lint
    cmd: 'true'
    depends_on: [test]
    auto:
      always: true
  - name: unrelated
    cmd: 'true'
  - name: test
    cmd: 'true'
    depends_on: [build]
  - name: build
    cmd: 'true'
";

    #[test]
    fn plan_expands_transitive_deps_in_config_order() {
        let config = load(CHAIN);
        let chain = plan(&config, &auto(false), &PlanOptions::default()).unwrap();
        assert_eq!(ids(&chain), ["build", "test", "lint"]);
        assert_eq!(chain.get("lint").unwrap().reason, SelectReason::Always);
        assert_eq!(chain.get("test").unwrap().deps, ["build"]);

        let config = load(
            r"
name: root
commands:
  - name: reader
    cmd: 'true'
    depends_on: [writer]
  - name: writer
    cmd: 'true'
  - name: other
    cmd: 'true'
",
        );
        let all = Selection::All {
            include_manual: false,
        };
        let plan = plan(&config, &all, &PlanOptions::default()).unwrap();
        assert_eq!(ids(&plan), ["writer", "other", "reader"]);
        assert!(plan.commands.iter().all(|c| c.reason == SelectReason::All));
    }

    #[test]
    fn reason_dependency_of() {
        let config = load(
            r"
name: root
commands:
  - name: build
    cmd: 'true'
  - name: test
    cmd: 'true'
    depends_on: [build]
  - name: bench
    cmd: 'true'
    depends_on: [build]
",
        );
        let plan = plan(
            &config,
            &targets(&["bench", "test"]),
            &PlanOptions::default(),
        )
        .unwrap();
        assert_eq!(ids(&plan), ["build", "test", "bench"]);
        assert_eq!(
            plan.get("build").unwrap().reason,
            SelectReason::Dependency {
                of: vec!["test".into(), "bench".into()]
            }
        );
        assert_eq!(plan.get("test").unwrap().reason, SelectReason::Requested);
        assert_eq!(plan.get("test").unwrap().group_path, "root");
    }

    const MANUAL: &str = r"
name: root
commands:
  - name: lint
    cmd: 'true'
    auto:
      always: true
  - name: demo
    cmd: 'true'
    depends_on: [lint]
    auto:
      always: true
      check: false
";

    #[test]
    fn auto_drops_check_false_unless_include_manual() {
        let config = load(MANUAL);
        let plan_default = plan(&config, &auto(false), &PlanOptions::default()).unwrap();
        assert_eq!(ids(&plan_default), ["lint"]);
        assert_eq!(plan_default.excluded_manual, ["demo"]);

        let with_manual = plan(&config, &auto(true), &PlanOptions::default()).unwrap();
        assert_eq!(ids(&with_manual), ["lint", "demo"]);
        assert!(with_manual.excluded_manual.is_empty());
    }

    #[test]
    fn all_drops_check_false_unless_include_manual() {
        let config = load(MANUAL);
        for (include_manual, expected) in [(false, &["lint"][..]), (true, &["lint", "demo"])] {
            let plan = plan(
                &config,
                &Selection::All { include_manual },
                &PlanOptions::default(),
            )
            .unwrap();
            assert_eq!(ids(&plan), expected);
        }
    }

    #[test]
    fn targets_include_check_false() {
        let config = load(MANUAL);
        let by_name = plan(&config, &targets(&["demo"]), &PlanOptions::default()).unwrap();
        assert_eq!(ids(&by_name), ["lint", "demo"]);
        assert!(by_name.excluded_manual.is_empty());

        let plan = plan(
            &config,
            &Selection::Ids(vec!["demo".into()]),
            &PlanOptions::default(),
        )
        .unwrap();
        assert_eq!(ids(&plan), ["lint", "demo"]);
    }

    #[test]
    fn reuse_dep_prunes_satisfied_subtree() {
        let config = load(CHAIN);
        let reuse = |cmd: &Command| cmd.id == "test";
        let opts = PlanOptions {
            reuse_dep: Some(&reuse),
        };
        let pruned = plan(&config, &targets(&["lint"]), &opts).unwrap();
        assert_eq!(ids(&pruned), ["lint"]);
        assert!(pruned.get("lint").unwrap().deps.is_empty());

        // A requested command runs even when it could be reused.
        let requested = plan(&config, &targets(&["lint", "test"]), &opts).unwrap();
        assert_eq!(ids(&requested), ["build", "test", "lint"]);
    }

    const SAME_NAMES: &str = r"
name: root
commands:
  - name: lint
    cmd: 'true'
children:
  - name: backend
    commands:
      - name: lint
        cmd: 'true'
      - name: test
        cmd: 'true'
  - name: frontend
    commands:
      - name: test
        cmd: 'true'
";

    #[test]
    fn resolve_exact_id_beats_name() {
        let config = load(SAME_NAMES);
        assert_eq!(resolve_target(&config, "lint").unwrap().id, "lint");
        let backend_lint = resolve_target(&config, "backend/lint").unwrap();
        assert_eq!(backend_lint.id, "backend/lint");
    }

    #[test]
    fn resolve_ambiguous_name_lists_candidates() {
        let config = load(SAME_NAMES);
        let err = plan(&config, &targets(&["test"]), &PlanOptions::default()).unwrap_err();
        assert_eq!(
            err,
            PlanError::Ambiguous {
                target: "test".into(),
                candidates: vec![
                    ("backend/test".into(), "root > backend".into()),
                    ("frontend/test".into(), "root > frontend".into()),
                ],
            }
        );
        assert!(
            err.to_string().contains("backend/test (root > backend)"),
            "{err}"
        );
    }

    #[test]
    fn unknown_target_or_id_is_not_found() {
        let config = load(CHAIN);
        let err = plan(&config, &targets(&["buld"]), &PlanOptions::default()).unwrap_err();
        assert!(
            matches!(&err, PlanError::NotFound { target, suggestions }
                if target == "buld" && suggestions.contains(&"build".to_string())),
            "{err:?}"
        );
        let err = plan(
            &config,
            &Selection::Ids(vec!["nope".into()]),
            &PlanOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::NotFound { .. }), "{err:?}");
    }
}
