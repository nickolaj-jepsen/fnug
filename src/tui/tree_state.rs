use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use super::app::CommandStatus;
use super::run_summary::RunSummary;
use super::tree_widget::{NodeKind, VisibleNode};

/// Shared state passed through recursive tree flattening
pub(super) struct TreeContext<'a> {
    pub expanded: &'a HashMap<String, bool>,
    pub selected: &'a HashSet<String>,
    /// Latest run of each command; a command without one never ran
    pub runs: &'a HashMap<String, RunSummary>,
    /// Running commands show how long they have run at this instant
    pub now: Instant,
    pub nodes: &'a mut Vec<VisibleNode>,
    pub filter: Option<&'a str>,
}

/// Check if a group or any of its descendants match the filter query (case-insensitive)
fn matches_filter(group: &CommandGroup, query: &str) -> bool {
    let q = query.to_lowercase();
    if group.name.to_lowercase().contains(&q) || group.id.to_lowercase().contains(&q) {
        return true;
    }
    if group.commands.iter().any(|c| command_matches_filter(c, &q)) {
        return true;
    }
    group.children.iter().any(|c| matches_filter(c, &q))
}

fn command_matches_filter(cmd: &Command, query: &str) -> bool {
    cmd.name.to_lowercase().contains(query)
        || cmd.id.to_lowercase().contains(query)
        || cmd.cmd.to_lowercase().contains(query)
}

pub(super) fn flatten_group(
    group: &CommandGroup,
    depth: usize,
    is_last: bool,
    ancestor_is_last: &[bool],
    ctx: &mut TreeContext<'_>,
) {
    // When filter is active, skip groups that don't match
    if let Some(query) = ctx.filter
        && !query.is_empty()
        && !matches_filter(group, query)
    {
        return;
    }

    // Force expand groups when filter is active
    let expanded = if ctx.filter.is_some_and(|q| !q.is_empty()) {
        true
    } else {
        *ctx.expanded.get(&group.id).unwrap_or(&true)
    };

    // Compute summary for group
    let total_count = count_commands(group);
    let counts = count_status(group, ctx);

    ctx.nodes.push(VisibleNode {
        id: group.id.clone(),
        depth,
        is_last_sibling: is_last,
        ancestor_is_last: ancestor_is_last.to_vec(),
        kind: NodeKind::Group {
            name: group.name.clone(),
            expanded,
            success: counts.success,
            running: counts.running,
            failure: counts.failure,
            selected: counts.selected,
            total: total_count,
        },
    });

    if expanded {
        // Build ancestor trail for children at depth+1.
        // Skip the root level (depth 0) since it's always the only node
        // and never needs a continuation line — this also removes the
        // wasted 2-char indent that would otherwise push everything right.
        let mut child_ancestors = ancestor_is_last.to_vec();
        if depth > 0 {
            child_ancestors.push(is_last);
        }

        let has_filter = ctx.filter.is_some_and(|q| !q.is_empty());
        let filter_lower = ctx.filter.unwrap_or("").to_lowercase();

        // Filter children and commands based on search query
        let visible_children: Vec<&CommandGroup> = if has_filter {
            group
                .children
                .iter()
                .filter(|c| matches_filter(c, &filter_lower))
                .collect()
        } else {
            group.children.iter().collect()
        };

        let visible_commands: Vec<&Command> = if has_filter {
            group
                .commands
                .iter()
                .filter(|c| command_matches_filter(c, &filter_lower))
                .collect()
        } else {
            group.commands.iter().collect()
        };

        let children_count = visible_children.len();
        let total = children_count + visible_commands.len();

        for (i, child) in visible_children.iter().enumerate() {
            let is_last = i == total.saturating_sub(1);
            flatten_group(child, depth + 1, is_last, &child_ancestors, ctx);
        }

        for (i, cmd) in visible_commands.iter().enumerate() {
            let is_selected = ctx.selected.contains(&cmd.id);
            let run = ctx.runs.get(&cmd.id);

            ctx.nodes.push(VisibleNode {
                id: cmd.id.clone(),
                depth: depth + 1,
                is_last_sibling: children_count + i == total.saturating_sub(1),
                ancestor_is_last: child_ancestors.clone(),
                kind: NodeKind::Command {
                    name: cmd.name.clone(),
                    selected: is_selected,
                    status: run.map_or(CommandStatus::Pending, |r| r.status.clone()),
                    duration: run.and_then(|r| r.elapsed(ctx.now)),
                    detail: run.and_then(RunSummary::detail),
                },
            });
        }
    }
}

#[derive(Default)]
struct StatusCounts {
    success: u16,
    running: u16,
    failure: u16,
    selected: u16,
}

impl StatusCounts {
    fn merge(&mut self, other: &StatusCounts) {
        self.success += other.success;
        self.running += other.running;
        self.failure += other.failure;
        self.selected += other.selected;
    }
}

fn count_status(group: &CommandGroup, ctx: &TreeContext<'_>) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for cmd in &group.commands {
        if ctx.selected.contains(&cmd.id) {
            counts.selected += 1;
        }
        match ctx.runs.get(&cmd.id).map(|r| &r.status) {
            Some(CommandStatus::Success) => counts.success += 1,
            Some(CommandStatus::Running | CommandStatus::WaitingForDeps) => counts.running += 1,
            Some(CommandStatus::Failure(_) | CommandStatus::Error(_)) => counts.failure += 1,
            Some(CommandStatus::Pending | CommandStatus::Stopped) | None => {}
        }
    }
    for child in &group.children {
        counts.merge(&count_status(child, ctx));
    }
    counts
}

pub(super) fn count_commands(group: &CommandGroup) -> u16 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "command count never exceeds u16"
    )]
    let own = group.commands.len() as u16;
    let children: u16 = group.children.iter().map(count_commands).sum();
    own + children
}

pub(super) fn find_group_in_group<'a>(
    group: &'a CommandGroup,
    id: &str,
) -> Option<&'a CommandGroup> {
    if group.id == id {
        return Some(group);
    }
    group
        .children
        .iter()
        .find_map(|c| find_group_in_group(c, id))
}

pub(super) fn find_command_in_group(group: &CommandGroup, id: &str) -> Option<Command> {
    group
        .commands
        .iter()
        .find(|cmd| cmd.id == id)
        .cloned()
        .or_else(|| {
            group
                .children
                .iter()
                .find_map(|c| find_command_in_group(c, id))
        })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::time::Instant;

    use super::{TreeContext, flatten_group};
    use crate::commands::command::Command;
    use crate::commands::group::CommandGroup;
    use crate::tui::app::CommandStatus;
    use crate::tui::run_summary::RunSummary;
    use crate::tui::test_util::{command, group};
    use crate::tui::tree_widget::{NodeKind, VisibleNode};

    #[derive(Default)]
    struct State {
        expanded: HashMap<String, bool>,
        selected: HashSet<String>,
        runs: HashMap<String, RunSummary>,
    }

    impl State {
        fn flatten(&self, config: &CommandGroup, filter: Option<&str>) -> Vec<VisibleNode> {
            let mut nodes = Vec::new();
            let mut ctx = TreeContext {
                expanded: &self.expanded,
                selected: &self.selected,
                runs: &self.runs,
                now: Instant::now(),
                nodes: &mut nodes,
                filter,
            };
            flatten_group(config, 0, true, &[], &mut ctx);
            nodes
        }

        fn ran(&mut self, id: &str, status: CommandStatus) {
            let run = RunSummary {
                status,
                ..RunSummary::default()
            };
            self.runs.insert(id.into(), run);
        }
    }

    fn ids(nodes: &[VisibleNode]) -> Vec<&str> {
        nodes.iter().map(|n| n.id.as_str()).collect()
    }

    fn node<'a>(nodes: &'a [VisibleNode], id: &str) -> &'a VisibleNode {
        nodes.iter().find(|n| n.id == id).unwrap()
    }

    /// `(success, running, failure, selected, total)` of group `id`
    fn counts(nodes: &[VisibleNode], id: &str) -> (u16, u16, u16, u16, u16) {
        match node(nodes, id).kind {
            NodeKind::Group {
                success,
                running,
                failure,
                selected,
                total,
                ..
            } => (success, running, failure, selected, total),
            NodeKind::Command { .. } => panic!("{id} is a command"),
        }
    }

    /// `root` holding `alpha` (`a1` running `cargo test`, `a2`) and `beta` (`b1`)
    fn filter_config() -> CommandGroup {
        let a1 = Command {
            cmd: "cargo test".into(),
            ..command("a1")
        };
        group(
            "root",
            vec![
                group("alpha", vec![], vec![a1, command("a2")]),
                group("beta", vec![], vec![command("b1")]),
            ],
            vec![],
        )
    }

    #[test]
    fn filter_keeps_matches_and_the_groups_above_them() {
        let config = filter_config();
        let mut state = State::default();
        state.expanded.insert("alpha".into(), false);
        assert_eq!(
            ids(&state.flatten(&config, None)),
            ["root", "alpha", "beta", "b1"]
        );

        // By id or by cmd, ignoring case; a collapsed group opens to show its match
        for query in ["A1", "CARGO"] {
            let nodes = state.flatten(&config, Some(query));
            assert_eq!(ids(&nodes), ["root", "alpha", "a1"], "{query}");
            assert!(
                matches!(
                    node(&nodes, "alpha").kind,
                    NodeKind::Group { expanded: true, .. }
                ),
                "{query}"
            );
            assert!(node(&nodes, "a1").is_last_sibling, "{query}");
        }

        // The branch drawing follows what is shown, not the whole tree
        let nodes = state.flatten(&config, Some("b"));
        assert_eq!(ids(&nodes), ["root", "beta", "b1"]);
        assert!(node(&nodes, "beta").is_last_sibling);

        assert!(state.flatten(&config, Some("nothing")).is_empty());
        // An empty query filters nothing
        assert_eq!(ids(&state.flatten(&config, Some(""))).len(), 4);
    }

    #[test]
    fn group_counts_cover_every_command_below() {
        // root { g { sub { c3, c4 }, c1, c2 }, c5, c6, c7 }
        let sub = group("sub", vec![], vec![command("c3"), command("c4")]);
        let g = group("g", vec![sub], vec![command("c1"), command("c2")]);
        let config = group(
            "root",
            vec![g],
            vec![command("c5"), command("c6"), command("c7")],
        );
        let mut state = State::default();
        state.ran("c1", CommandStatus::Success);
        state.ran("c2", CommandStatus::Failure(1));
        state.ran("c3", CommandStatus::Running);
        state.ran("c4", CommandStatus::WaitingForDeps);
        state.ran("c5", CommandStatus::Error("no such directory".into()));
        state.ran("c6", CommandStatus::Stopped);
        state.selected.extend(["c2".to_string(), "c7".to_string()]);

        let nodes = state.flatten(&config, None);

        // Waiting counts as running, an error as a failure, and stopped or never run as neither
        assert_eq!(counts(&nodes, "root"), (1, 2, 2, 2, 7));
        assert_eq!(counts(&nodes, "g"), (1, 2, 1, 1, 4));
        assert_eq!(counts(&nodes, "sub"), (0, 2, 0, 0, 2));
        assert!(matches!(
            node(&nodes, "c7").kind,
            NodeKind::Command {
                selected: true,
                status: CommandStatus::Pending,
                ..
            }
        ));
    }
}
