//! Dependency-gated scheduling state, shared by the headless executor and the TUI.
//!
//! [`DagState`] does no I/O: a driver asks it which command may start next, starts it, and
//! reports back how it ended.

use std::collections::{BTreeMap, HashMap, VecDeque};

use super::plan::Plan;

/// Where a command stands in the schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeState {
    /// Blocked on these dependencies, in `depends_on` order.
    Waiting(Vec<String>),
    /// Every dependency passed; queued to start.
    Ready,
    Running,
    Passed,
    Failed,
    /// Never started because a dependency failed or was aborted. `cause` is the id of the
    /// command that did, not of an intermediate skipped one.
    Skipped {
        cause: String,
    },
    /// Never started, or stopped, without a failure to blame.
    NotRun,
}

impl NodeState {
    /// Whether the command still has to start or finish.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            NodeState::Waiting(_) | NodeState::Ready | NodeState::Running
        )
    }
}

/// A command as [`DagState`] schedules it.
#[derive(Debug, Clone, Copy)]
pub struct DagNode<'a> {
    pub id: &'a str,
    /// Every `depends_on` entry, including dependencies outside the submitted batch.
    pub depends_on: &'a [String],
    /// Never runs alongside another command.
    pub exclusive: bool,
}

#[derive(Debug)]
struct Node {
    state: NodeState,
    exclusive: bool,
    /// Submission order; the ready queue starts commands in this order.
    seq: u64,
}

/// Scheduling state for commands and their dependencies.
///
/// Commands start in submission order once every dependency they wait on has passed. A failed
/// command's waiting dependents are skipped, transitively.
#[derive(Debug, Default)]
pub struct DagState {
    nodes: HashMap<String, Node>,
    ready: BTreeMap<u64, String>,
    next_seq: u64,
}

impl DagState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// State with `plan` submitted.
    #[must_use]
    pub fn from_plan(plan: &Plan) -> Self {
        let mut dag = Self::new();
        dag.submit(plan);
        dag
    }

    /// Queue the plan's commands; see [`submit_nodes`](Self::submit_nodes).
    pub fn submit(&mut self, plan: &Plan) -> Vec<String> {
        self.submit_nodes(plan.commands.iter().map(|c| DagNode {
            id: c.id(),
            depends_on: &c.command.depends_on,
            exclusive: false,
        }))
    }

    /// Queue a batch of commands to run, in order, whatever their previous state.
    ///
    /// A command waits for a dependency that is in the batch, even one that passed before, or
    /// that is still waiting, ready or running from an earlier batch. Any other dependency
    /// counts as satisfied.
    ///
    /// Returns the ids that were running: the driver must stop those runs, and no
    /// [`finish`](Self::finish) is expected for them.
    pub fn submit_nodes<'a>(
        &mut self,
        nodes: impl IntoIterator<Item = DagNode<'a>>,
    ) -> Vec<String> {
        let nodes: Vec<DagNode> = nodes.into_iter().collect();
        let waits: Vec<Vec<String>> = nodes
            .iter()
            .map(|node| {
                node.depends_on
                    .iter()
                    .filter(|dep| {
                        nodes.iter().any(|other| other.id == dep.as_str()) || self.is_active(dep)
                    })
                    .cloned()
                    .collect()
            })
            .collect();

        let mut restarted = Vec::new();
        for (node, waiting) in nodes.iter().zip(waits) {
            if let Some(old) = self.nodes.remove(node.id) {
                match old.state {
                    NodeState::Running => restarted.push(node.id.to_string()),
                    NodeState::Ready => {
                        self.ready.remove(&old.seq);
                    }
                    _ => {}
                }
            }
            let seq = self.next_seq;
            self.next_seq += 1;
            let state = if waiting.is_empty() {
                self.ready.insert(seq, node.id.to_string());
                NodeState::Ready
            } else {
                NodeState::Waiting(waiting)
            };
            self.nodes.insert(
                node.id.to_string(),
                Node {
                    state,
                    exclusive: node.exclusive,
                    seq,
                },
            );
        }
        restarted
    }

    /// Start the next ready command, if fewer than `max_running` are running.
    ///
    /// The queue is first in, first out: while its head is exclusive and anything runs, or an
    /// exclusive command runs, nothing starts, so a later command can't starve an exclusive
    /// one.
    pub fn pop_ready(&mut self, max_running: usize) -> Option<String> {
        let (running, exclusive_running) = self
            .nodes
            .values()
            .filter(|n| n.state == NodeState::Running)
            .fold((0, false), |(count, exclusive), n| {
                (count + 1, exclusive || n.exclusive)
            });
        if running >= max_running || exclusive_running {
            return None;
        }
        let (&seq, id) = self.ready.first_key_value()?;
        let node = self.nodes.get_mut(id)?;
        if node.exclusive && running > 0 {
            return None;
        }
        node.state = NodeState::Running;
        self.ready.remove(&seq)
    }

    /// Record that a running command ended. Its dependents become ready once nothing else
    /// holds them back; if it failed, they are skipped instead.
    ///
    /// Returns the newly skipped commands as `(id, cause)`, in submission order. A command that
    /// isn't running is left alone.
    pub fn finish(&mut self, id: &str, passed: bool) -> Vec<(String, String)> {
        match self.nodes.get_mut(id) {
            Some(node) if node.state == NodeState::Running => {
                node.state = if passed {
                    NodeState::Passed
                } else {
                    NodeState::Failed
                };
            }
            _ => return Vec::new(),
        }
        if passed {
            self.release(id);
            Vec::new()
        } else {
            self.skip_dependents(id)
        }
    }

    /// Drop a command from the schedule, such as when it is cleared, whatever its state; the
    /// driver stops it if it runs. Its waiting dependents are skipped, as if it had failed.
    ///
    /// Returns the newly skipped commands as `(id, cause)`, in submission order.
    pub fn abort(&mut self, id: &str) -> Vec<(String, String)> {
        if !self.set_not_run(id) {
            return Vec::new();
        }
        self.skip_dependents(id)
    }

    /// Record that the user stopped a command: it and its waiting dependents, transitively,
    /// become [`NodeState::NotRun`], with no failure to report.
    ///
    /// Returns the dependents that were cancelled, in submission order.
    pub fn stop(&mut self, id: &str) -> Vec<String> {
        if !self.set_not_run(id) {
            return Vec::new();
        }
        self.cascade(id, |_| NodeState::NotRun)
            .into_iter()
            .map(|(dependent, _)| dependent)
            .collect()
    }

    /// Mark every waiting and ready command [`NodeState::NotRun`], leaving running ones alone.
    ///
    /// Returns their ids in submission order.
    pub fn cancel_pending(&mut self) -> Vec<String> {
        self.ready.clear();
        let mut cancelled: Vec<(u64, String)> = self
            .nodes
            .iter_mut()
            .filter(|(_, node)| matches!(node.state, NodeState::Waiting(_) | NodeState::Ready))
            .map(|(id, node)| {
                node.state = NodeState::NotRun;
                (node.seq, id.clone())
            })
            .collect();
        cancelled.sort_unstable();
        cancelled.into_iter().map(|(_, id)| id).collect()
    }

    #[must_use]
    pub fn state(&self, id: &str) -> Option<&NodeState> {
        self.nodes.get(id).map(|n| &n.state)
    }

    /// The dependencies a waiting command still waits on; empty otherwise.
    #[must_use]
    pub fn waiting_on(&self, id: &str) -> &[String] {
        match self.state(id) {
            Some(NodeState::Waiting(deps)) => deps,
            _ => &[],
        }
    }

    /// How many commands are running.
    #[must_use]
    pub fn running(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| n.state == NodeState::Running)
            .count()
    }

    /// Whether `id` still has to start or finish.
    #[must_use]
    pub fn is_active(&self, id: &str) -> bool {
        self.state(id).is_some_and(NodeState::is_active)
    }

    /// Whether no command is waiting, ready or running.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        !self.nodes.values().any(|n| n.state.is_active())
    }

    /// Set a known command to `NotRun`, taking it off the ready queue. Returns false if unknown.
    fn set_not_run(&mut self, id: &str) -> bool {
        let Some(node) = self.nodes.get_mut(id) else {
            return false;
        };
        if node.state == NodeState::Ready {
            self.ready.remove(&node.seq);
        }
        node.state = NodeState::NotRun;
        true
    }

    /// Stop waiting on `id`, which passed; commands with nothing left to wait on become ready.
    fn release(&mut self, id: &str) {
        for (dependent, node) in &mut self.nodes {
            let NodeState::Waiting(deps) = &mut node.state else {
                continue;
            };
            deps.retain(|d| d != id);
            if deps.is_empty() {
                node.state = NodeState::Ready;
                self.ready.insert(node.seq, dependent.clone());
            }
        }
    }

    fn skip_dependents(&mut self, id: &str) -> Vec<(String, String)> {
        self.cascade(id, |cause| NodeState::Skipped {
            cause: cause.to_string(),
        })
    }

    /// Give every command waiting on `root`, directly or through another such command, the
    /// state `make(root)`. Returns `(id, root)` for each, in submission order.
    fn cascade(&mut self, root: &str, make: impl Fn(&str) -> NodeState) -> Vec<(String, String)> {
        let mut hit: Vec<(u64, String)> = Vec::new();
        let mut queue = VecDeque::from([root.to_string()]);
        while let Some(done) = queue.pop_front() {
            for (dependent, node) in &mut self.nodes {
                if matches!(&node.state, NodeState::Waiting(deps) if deps.contains(&done)) {
                    node.state = make(root);
                    hit.push((node.seq, dependent.clone()));
                    queue.push_back(dependent.clone());
                }
            }
        }
        hit.sort_unstable();
        hit.into_iter()
            .map(|(_, id)| (id, root.to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(id, depends_on, exclusive)`
    type Spec<'a> = (&'a str, &'a [&'a str], bool);

    fn submit(dag: &mut DagState, specs: &[Spec]) -> Vec<String> {
        let deps: Vec<Vec<String>> = specs
            .iter()
            .map(|(_, deps, _)| deps.iter().map(ToString::to_string).collect())
            .collect();
        dag.submit_nodes(
            specs
                .iter()
                .zip(&deps)
                .map(|((id, _, exclusive), depends_on)| DagNode {
                    id,
                    depends_on,
                    exclusive: *exclusive,
                }),
        )
    }

    fn drain(dag: &mut DagState, max_running: usize) -> Vec<String> {
        std::iter::from_fn(|| dag.pop_ready(max_running)).collect()
    }

    fn run_to_completion(dag: &mut DagState) {
        while let Some(id) = dag.pop_ready(usize::MAX) {
            dag.finish(&id, true);
        }
    }

    const CHAIN: &[Spec] = &[
        ("a", &[], false),
        ("b", &["a"], false),
        ("c", &["b"], false),
    ];

    #[test]
    fn rerun_chain_waits_despite_previous_pass() {
        let mut dag = DagState::new();
        submit(&mut dag, CHAIN);
        run_to_completion(&mut dag);
        assert_eq!(dag.state("c"), Some(&NodeState::Passed));

        submit(&mut dag, CHAIN);
        assert_eq!(drain(&mut dag, usize::MAX), ["a"]);
        assert_eq!(dag.waiting_on("b"), ["a"]);
        assert_eq!(dag.waiting_on("c"), ["b"]);

        dag.finish("a", true);
        assert_eq!(drain(&mut dag, usize::MAX), ["b"]);
        assert_eq!(dag.state("c"), Some(&NodeState::Waiting(vec!["b".into()])));
    }

    #[test]
    fn dependency_outside_batch_counts_only_while_active() {
        let mut dag = DagState::new();
        submit(&mut dag, &[("build", &[], false)]);
        submit(&mut dag, &[("test", &["build"], false)]);
        assert_eq!(dag.waiting_on("test"), ["build"]);
        assert_eq!(drain(&mut dag, usize::MAX), ["build"]);
        dag.finish("build", true);
        assert_eq!(drain(&mut dag, usize::MAX), ["test"]);
        dag.finish("test", true);

        // A passed dependency outside the batch is satisfied
        submit(&mut dag, &[("test", &["build"], false)]);
        assert_eq!(dag.state("test"), Some(&NodeState::Ready));
    }

    #[test]
    fn failure_skips_transitive_with_root_cause() {
        let mut dag = DagState::new();
        submit(
            &mut dag,
            &[
                ("build", &[], false),
                ("other", &[], false),
                ("test", &["build"], false),
                ("lint", &["test"], false),
            ],
        );
        assert_eq!(drain(&mut dag, usize::MAX), ["build", "other"]);
        let skipped = dag.finish("build", false);
        assert_eq!(
            skipped,
            [
                ("test".to_string(), "build".to_string()),
                ("lint".to_string(), "build".to_string())
            ]
        );
        assert_eq!(dag.state("build"), Some(&NodeState::Failed));
        assert_eq!(
            dag.state("lint"),
            Some(&NodeState::Skipped {
                cause: "build".into()
            })
        );
        assert!(dag.finish("other", true).is_empty());
        assert!(dag.is_idle());
    }

    #[test]
    fn fail_fast_marks_pending_not_run() {
        let mut dag = DagState::new();
        submit(
            &mut dag,
            &[("a", &[], false), ("b", &[], false), ("c", &["a"], false)],
        );
        assert_eq!(dag.pop_ready(1).as_deref(), Some("a"));
        dag.finish("a", false);
        assert_eq!(dag.cancel_pending(), ["b"]);
        assert_eq!(dag.state("b"), Some(&NodeState::NotRun));
        assert_eq!(
            dag.state("c"),
            Some(&NodeState::Skipped { cause: "a".into() })
        );
        assert_eq!(dag.pop_ready(usize::MAX), None);
        assert!(dag.is_idle());
    }

    #[test]
    fn cancel_pending_leaves_running_alone() {
        let mut dag = DagState::new();
        submit(&mut dag, &[("a", &[], false), ("b", &["a"], false)]);
        assert_eq!(drain(&mut dag, usize::MAX), ["a"]);
        assert_eq!(dag.cancel_pending(), ["b"]);
        assert_eq!(dag.state("a"), Some(&NodeState::Running));
        assert!(!dag.is_idle());
    }

    #[test]
    fn jobs_cap_respected() {
        let mut dag = DagState::new();
        submit(
            &mut dag,
            &[("a", &[], false), ("b", &[], false), ("c", &[], false)],
        );
        assert_eq!(drain(&mut dag, 2), ["a", "b"]);
        assert_eq!(dag.running(), 2);
        dag.finish("b", true);
        assert_eq!(drain(&mut dag, 2), ["c"]);
    }

    #[test]
    fn exclusive_runs_alone_fifo() {
        let mut dag = DagState::new();
        submit(
            &mut dag,
            &[
                ("a", &[], false),
                ("fix", &[], true),
                ("b", &[], false),
                ("c", &[], false),
            ],
        );
        // `fix` heads the queue after `a`, so `b` may not overtake it
        assert_eq!(drain(&mut dag, usize::MAX), ["a"]);
        dag.finish("a", true);
        assert_eq!(drain(&mut dag, usize::MAX), ["fix"]);
        dag.finish("fix", true);
        assert_eq!(drain(&mut dag, usize::MAX), ["b", "c"]);
    }

    #[test]
    fn submit_returns_running_for_restart() {
        let mut dag = DagState::new();
        submit(&mut dag, &[("a", &[], false), ("b", &["a"], false)]);
        assert_eq!(drain(&mut dag, usize::MAX), ["a"]);

        let restarted = submit(&mut dag, &[("a", &[], false), ("b", &["a"], false)]);
        assert_eq!(restarted, ["a"]);
        assert_eq!(dag.state("a"), Some(&NodeState::Ready));
        // The replaced run's exit is not this run's
        assert!(dag.finish("a", false).is_empty());
        assert_eq!(dag.state("b"), Some(&NodeState::Waiting(vec!["a".into()])));
        assert_eq!(drain(&mut dag, usize::MAX), ["a"]);
    }

    #[test]
    fn abort_queued_dependency_skips_dependents() {
        let mut dag = DagState::new();
        submit(
            &mut dag,
            &[
                ("slow", &[], false),
                ("build", &[], false),
                ("test", &["build"], false),
            ],
        );
        assert_eq!(dag.pop_ready(1).as_deref(), Some("slow"));
        assert_eq!(dag.state("build"), Some(&NodeState::Ready));

        let skipped = dag.abort("build");
        assert_eq!(skipped, [("test".to_string(), "build".to_string())]);
        assert_eq!(dag.state("build"), Some(&NodeState::NotRun));
        dag.finish("slow", true);
        assert_eq!(dag.pop_ready(usize::MAX), None);
        assert!(dag.is_idle());
    }

    #[test]
    fn abort_running_dependency_skips_dependents() {
        let mut dag = DagState::new();
        submit(&mut dag, CHAIN);
        assert_eq!(drain(&mut dag, usize::MAX), ["a"]);

        let skipped = dag.abort("a");
        assert_eq!(
            skipped,
            [
                ("b".to_string(), "a".to_string()),
                ("c".to_string(), "a".to_string())
            ]
        );
        assert_eq!(dag.running(), 0);
        assert!(dag.is_idle());
    }

    #[test]
    fn stop_cancels_dependents_without_cause() {
        let mut dag = DagState::new();
        submit(&mut dag, CHAIN);
        assert_eq!(drain(&mut dag, usize::MAX), ["a"]);

        assert_eq!(dag.stop("a"), ["b", "c"]);
        assert_eq!(dag.state("a"), Some(&NodeState::NotRun));
        assert_eq!(dag.state("c"), Some(&NodeState::NotRun));
        // The stopped run's exit arrives later and changes nothing
        assert!(dag.finish("a", false).is_empty());
        assert!(dag.is_idle());
    }
}
