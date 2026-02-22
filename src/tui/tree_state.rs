use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use std::collections::HashMap;
use std::time::Instant;

use super::app::{CommandStatus, ProcessInstance};
use super::tree_widget::{NodeKind, VisibleNode};

/// Shared state passed through recursive tree flattening
pub(super) struct TreeContext<'a> {
    pub expanded: &'a HashMap<String, bool>,
    pub selected: &'a HashMap<String, bool>,
    pub processes: &'a HashMap<String, ProcessInstance>,
    pub error_messages: &'a HashMap<String, String>,
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
    let counts = count_status(group, ctx.selected, ctx.processes, ctx.error_messages);

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
            let is_selected = *ctx.selected.get(&cmd.id).unwrap_or(&false);
            let (status, duration) = if let Some(msg) = ctx.error_messages.get(&cmd.id) {
                (CommandStatus::Error(msg.clone()), None)
            } else if let Some(proc) = ctx.processes.get(&cmd.id) {
                let dur = Some(
                    proc.finished_at
                        .unwrap_or_else(Instant::now)
                        .duration_since(proc.started_at),
                );
                (proc.status.clone(), dur)
            } else {
                (CommandStatus::Pending, None)
            };

            ctx.nodes.push(VisibleNode {
                id: cmd.id.clone(),
                depth: depth + 1,
                is_last_sibling: children_count + i == total.saturating_sub(1),
                ancestor_is_last: child_ancestors.clone(),
                kind: NodeKind::Command {
                    name: cmd.name.clone(),
                    selected: is_selected,
                    status,
                    duration,
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

fn count_status(
    group: &CommandGroup,
    selected_map: &HashMap<String, bool>,
    processes: &HashMap<String, ProcessInstance>,
    error_messages: &HashMap<String, String>,
) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for cmd in &group.commands {
        if *selected_map.get(&cmd.id).unwrap_or(&false) {
            counts.selected += 1;
        }
        if error_messages.contains_key(&cmd.id) {
            counts.failure += 1;
        } else if let Some(proc) = processes.get(&cmd.id) {
            match proc.status {
                CommandStatus::Success => counts.success += 1,
                CommandStatus::Running | CommandStatus::WaitingForDeps => counts.running += 1,
                CommandStatus::Failure(_) | CommandStatus::Error(_) => counts.failure += 1,
                CommandStatus::Pending => {}
            }
        }
    }
    for child in &group.children {
        counts.merge(&count_status(
            child,
            selected_map,
            processes,
            error_messages,
        ));
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
