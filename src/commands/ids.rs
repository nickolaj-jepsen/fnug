//! Command and group ids: defaults, uniqueness, `depends_on` resolution, and looking up a
//! command by id or name.
//!
//! A node's *local id* is its explicit `id`, or its name with `/` replaced by `-`. Ids are
//! assigned per *scope*: the root config is one scope and each workspace package is another. A
//! defaulted local id that another node in the scope shares becomes the `/`-joined path of local
//! ids below the scope root (e.g. `backend/test`). Ids in a package are prefixed with the
//! package's local id (e.g. `api/build`), so ids are unique and the same on every load.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::config_file::{ConfigCommandGroup, ConfigError};

/// Joins the parts of a qualified id. Explicit ids can't contain it, so they never collide
/// with qualified ones.
const SEPARATOR: &str = "/";

/// The id a node named `name` gets when it has no explicit `id` and no other node shares it.
pub(crate) fn local_id(name: &str) -> String {
    name.replace('/', "-")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Group,
    Command,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Group => "group",
            Kind::Command => "command",
        }
    }
}

/// A group or command, flattened in config order (a group, its commands, then its children).
struct Node {
    kind: Kind,
    name: String,
    local: String,
    explicit: bool,
    /// Index into the scope namespaces. A package's root group belongs to the enclosing scope.
    scope: usize,
    /// Local ids from below the scope root down to this node; empty for the root.
    path: Vec<String>,
    /// Names from the root down to this node, joined with ` > `.
    entry: String,
    file: Option<PathBuf>,
    /// Index of the enclosing group.
    parent: Option<usize>,
    depends_on: Vec<String>,
    /// Unique within the scope.
    scoped: String,
    /// Unique within the whole config.
    id: String,
}

impl Node {
    fn label(&self) -> String {
        format!("{} '{}'", self.kind.label(), self.entry)
    }

    fn location(&self) -> String {
        format!("{}{}", self.label(), in_file(self.file.as_deref()))
    }
}

fn in_file(file: Option<&Path>) -> String {
    file.map(|f| format!(" in {}", f.display()))
        .unwrap_or_default()
}

/// Validate names and explicit ids, give every group and command its final id, and rewrite
/// each `depends_on` entry to the id of the command it refers to.
///
/// A `depends_on` entry resolves to the first match of, within the command's scope: an id or a
/// local id among the command's siblings; a path of local ids (`backend/test`); a unique local
/// id anywhere. Failing those, it may name any command's full id (`api/build`). An entry that
/// is one command's id and a different sibling's local id is ambiguous.
///
/// # Errors
///
/// Returns `ConfigError::Validation` for an empty name, an empty explicit id, an id containing
/// `/`, an unknown or ambiguous `depends_on` entry, or a dependency cycle, and
/// `ConfigError::DuplicateId` when two nodes end up with the same id.
pub(crate) fn assign_ids(root: &mut ConfigCommandGroup) -> Result<(), ConfigError> {
    let mut tree = Tree {
        nodes: Vec::new(),
        namespaces: vec![None],
    };
    let place = Place {
        parent: None,
        scope: 0,
        path: &[],
        entry: None,
        file: None,
    };
    tree.collect(root, &place)?;
    let Tree {
        mut nodes,
        namespaces,
    } = tree;
    assign(&mut nodes, &namespaces)?;
    let edges = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            node.depends_on
                .iter()
                .map(|reference| resolve_dependency(&nodes, i, reference))
                .collect::<Result<Vec<usize>, ConfigError>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    check_cycles(&nodes, &edges)?;

    let mut resolved = nodes.iter().zip(&edges).map(|(node, deps)| {
        let deps = deps.iter().map(|&j| nodes[j].id.clone()).collect();
        (node.id.clone(), deps)
    });
    write_back(root, &mut resolved);
    Ok(())
}

/// Where a group is collected: its parent, and the scope and path it continues.
struct Place<'a> {
    parent: Option<usize>,
    scope: usize,
    /// Local ids from below the scope root down to the parent.
    path: &'a [String],
    entry: Option<&'a str>,
    file: Option<&'a Path>,
}

struct Tree {
    nodes: Vec<Node>,
    /// Id prefix of each scope; `None` for the root config.
    namespaces: Vec<Option<String>>,
}

impl Tree {
    fn collect(&mut self, group: &ConfigCommandGroup, place: &Place) -> Result<(), ConfigError> {
        let file = group.source.as_deref().or(place.file);
        let entry = check_name(&group.name, Kind::Group, place.entry, file)?;
        let (local, explicit) =
            local_of(group.id.as_deref(), &group.name, Kind::Group, &entry, file)?;
        let path = if place.parent.is_some() {
            [place.path, std::slice::from_ref(&local)].concat()
        } else {
            Vec::new()
        };
        let index = self.nodes.len();

        // A package's contents form a new scope, prefixed with the package's local id.
        let (scope, inner_path) = if group.source.is_some() && place.parent.is_some() {
            self.namespaces.push(Some(local.clone()));
            (self.namespaces.len() - 1, Vec::new())
        } else {
            (place.scope, path.clone())
        };
        self.nodes.push(Node {
            kind: Kind::Group,
            name: group.name.clone(),
            local,
            explicit,
            scope: place.scope,
            path,
            entry: entry.clone(),
            file: file.map(Path::to_path_buf),
            parent: place.parent,
            depends_on: Vec::new(),
            scoped: String::new(),
            id: String::new(),
        });

        for cmd in group.commands.iter().flatten() {
            let cmd_entry = check_name(&cmd.name, Kind::Command, Some(&entry), file)?;
            let (local, explicit) = local_of(
                cmd.id.as_deref(),
                &cmd.name,
                Kind::Command,
                &cmd_entry,
                file,
            )?;
            self.nodes.push(Node {
                kind: Kind::Command,
                name: cmd.name.clone(),
                path: [inner_path.as_slice(), std::slice::from_ref(&local)].concat(),
                local,
                explicit,
                scope,
                entry: cmd_entry,
                file: file.map(Path::to_path_buf),
                parent: Some(index),
                depends_on: cmd.depends_on.clone().unwrap_or_default(),
                scoped: String::new(),
                id: String::new(),
            });
        }
        for child in group.children.iter().flatten() {
            let child_place = Place {
                parent: Some(index),
                scope,
                path: &inner_path,
                entry: Some(&entry),
                file,
            };
            self.collect(child, &child_place)?;
        }
        Ok(())
    }
}

/// The node's display entry path, or an error if its name is empty.
fn check_name(
    name: &str,
    kind: Kind,
    parent_entry: Option<&str>,
    file: Option<&Path>,
) -> Result<String, ConfigError> {
    if name.trim().is_empty() {
        let place = parent_entry.map_or_else(
            || "at the root".to_string(),
            |entry| format!("in '{entry}'"),
        );
        return Err(ConfigError::Validation(format!(
            "A {} {place}{} has an empty name",
            kind.label(),
            in_file(file)
        )));
    }
    Ok(match parent_entry {
        Some(parent) => format!("{parent} > {name}"),
        None => name.to_string(),
    })
}

fn local_of(
    id: Option<&str>,
    name: &str,
    kind: Kind,
    entry: &str,
    file: Option<&Path>,
) -> Result<(String, bool), ConfigError> {
    let Some(id) = id else {
        return Ok((local_id(name), false));
    };
    let location = format!("{} '{entry}'{}", kind.label(), in_file(file));
    if id.trim().is_empty() {
        return Err(ConfigError::Validation(format!(
            "The {location} has an empty id"
        )));
    }
    if id.contains(SEPARATOR) {
        return Err(ConfigError::Validation(format!(
            "The {location} has id '{id}', but ids must not contain '{SEPARATOR}'"
        )));
    }
    Ok((id.to_string(), true))
}

fn assign(nodes: &mut [Node], namespaces: &[Option<String>]) -> Result<(), ConfigError> {
    // The root group's id only names the TUI's top node, so it never displaces another id.
    let mut counts: HashMap<(usize, &str), usize> = HashMap::new();
    for node in nodes.iter().skip(1) {
        *counts.entry((node.scope, &node.local)).or_default() += 1;
    }
    let scoped: Vec<String> = nodes
        .iter()
        .map(|node| {
            if node.explicit || node.path.is_empty() || counts[&(node.scope, &*node.local)] == 1 {
                node.local.clone()
            } else {
                node.path.join(SEPARATOR)
            }
        })
        .collect();
    let mut ids: Vec<String> = nodes
        .iter()
        .zip(&scoped)
        .map(|(node, scoped)| match &namespaces[node.scope] {
            Some(namespace) => format!("{namespace}{SEPARATOR}{scoped}"),
            None => scoped.clone(),
        })
        .collect();
    if !nodes[0].explicit && ids[1..].contains(&ids[0]) {
        // No other id ends with the separator.
        ids[0].push_str(SEPARATOR);
    }

    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (i, id) in ids.iter().enumerate() {
        if let Some(&first) = seen.get(id.as_str()) {
            let (a, b) = (&nodes[first], &nodes[i]);
            // Name a shared file once, after the second location.
            let first = if a.file == b.file {
                a.label()
            } else {
                a.location()
            };
            return Err(ConfigError::DuplicateId {
                id: id.clone(),
                first,
                second: b.location(),
            });
        }
        seen.insert(id, i);
    }
    for ((node, scoped), id) in nodes.iter_mut().zip(scoped).zip(ids) {
        node.scoped = scoped;
        node.id = id;
    }
    Ok(())
}

fn resolve_dependency(nodes: &[Node], from: usize, reference: &str) -> Result<usize, ConfigError> {
    let commands = || {
        nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == Kind::Command)
    };
    let (scope, parent) = (nodes[from].scope, nodes[from].parent);
    let own_id = &nodes[from].id;
    let steps: [&dyn Fn(&Node) -> bool; 4] = [
        // An id must not silently shadow a sibling's local id, so either matches here and both
        // together are ambiguous. Local ids never contain the separator.
        &|node| {
            node.scope == scope
                && (node.scoped == reference
                    || node.parent == parent && node.local == reference && node.id != *own_id)
        },
        &|node| {
            node.scope == scope
                && reference.contains(SEPARATOR)
                && node.path.join(SEPARATOR) == reference
        },
        &|node| node.scope == scope && node.local == reference,
        &|node| node.id == reference,
    ];
    for matches in steps {
        let found: Vec<usize> = commands()
            .filter(|(_, node)| matches(node))
            .map(|(j, _)| j)
            .collect();
        match found.as_slice() {
            [] => {}
            [j] => return Ok(*j),
            _ => return Err(ambiguous_dependency(nodes, from, reference, &found)),
        }
    }

    let mut message = format!(
        "The {} depends on '{reference}', which matches no command",
        nodes[from].location()
    );
    let named: Vec<String> = commands()
        .filter(|(_, node)| node.name == reference)
        .map(|(_, node)| format!("'{}'", node.id))
        .collect();
    if named.is_empty() {
        let suggestions = similar(
            reference,
            commands().map(|(_, node)| (&*node.id, &*node.local)),
        );
        message.push_str(&fmt_suggestions(&suggestions));
    } else {
        let _ = write!(
            message,
            "; the command named '{reference}' has id {}, use that",
            named.join(" or ")
        );
    }
    Err(ConfigError::Validation(message))
}

fn ambiguous_dependency(
    nodes: &[Node],
    from: usize,
    reference: &str,
    found: &[usize],
) -> ConfigError {
    let listed = |j: usize| format!("'{}' ({})", nodes[j].id, nodes[j].entry);
    let owner = found.iter().find(|&&j| nodes[j].scoped == reference);
    let sibling = found.iter().find(|&&j| nodes[j].scoped != reference);
    let message = if let (Some(&owner), Some(&sibling)) = (owner, sibling) {
        // Repeating the reference can't pick the owner, so point at renaming instead.
        format!(
            "The {} depends on '{reference}', which is the id of {} but also the name of its \
             sibling {}; use '{}' for the sibling, or give '{}' another id",
            nodes[from].location(),
            listed(owner),
            listed(sibling),
            nodes[sibling].id,
            nodes[owner].entry,
        )
    } else {
        let candidates: Vec<String> = found.iter().map(|&j| listed(j)).collect();
        format!(
            "The {} depends on '{reference}', which matches several commands: {}; use one of \
             these ids",
            nodes[from].location(),
            candidates.join(", ")
        )
    };
    ConfigError::Validation(message)
}

fn check_cycles(nodes: &[Node], edges: &[Vec<usize>]) -> Result<(), ConfigError> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        New,
        Visiting,
        Done,
    }

    fn visit(
        i: usize,
        edges: &[Vec<usize>],
        state: &mut [State],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        state[i] = State::Visiting;
        stack.push(i);
        for &next in &edges[i] {
            match state[next] {
                State::New => {
                    if let Some(cycle) = visit(next, edges, state, stack) {
                        return Some(cycle);
                    }
                }
                State::Visiting => {
                    let start = stack.iter().position(|&s| s == next).unwrap_or_default();
                    let mut cycle = stack[start..].to_vec();
                    cycle.push(next);
                    return Some(cycle);
                }
                State::Done => {}
            }
        }
        stack.pop();
        state[i] = State::Done;
        None
    }

    let mut state = vec![State::New; nodes.len()];
    for i in 0..nodes.len() {
        if state[i] == State::New
            && let Some(cycle) = visit(i, edges, &mut state, &mut Vec::new())
        {
            let ids: Vec<&str> = cycle.iter().map(|&j| nodes[j].id.as_str()).collect();
            return Err(ConfigError::Validation(format!(
                "Circular dependency: {}",
                ids.join(" -> ")
            )));
        }
    }
    Ok(())
}

fn write_back(
    group: &mut ConfigCommandGroup,
    resolved: &mut impl Iterator<Item = (String, Vec<String>)>,
) {
    if let Some((id, _)) = resolved.next() {
        group.id = Some(id);
    }
    for cmd in group.commands.iter_mut().flatten() {
        if let Some((id, deps)) = resolved.next() {
            cmd.id = Some(id);
            cmd.depends_on = Some(deps);
        }
    }
    for child in group.children.iter_mut().flatten() {
        write_back(child, resolved);
    }
}

/// Ids of candidates whose id or name is within a small edit distance of `reference`,
/// closest first. Case is ignored.
fn similar<'a>(
    reference: &str,
    candidates: impl Iterator<Item = (&'a str, &'a str)>,
) -> Vec<String> {
    let wanted = reference.to_lowercase();
    let mut scored: Vec<(usize, &str)> = candidates
        .filter_map(|(id, name)| {
            let distance = [id, name]
                .iter()
                .map(|c| strsim::damerau_levenshtein(&wanted, &c.to_lowercase()))
                .min()?;
            (distance <= 2 && distance < wanted.chars().count()).then_some((distance, id))
        })
        .collect();
    scored.sort_by_key(|&(distance, _)| distance);
    let mut ids: Vec<String> = Vec::new();
    for (_, id) in scored {
        if !ids.iter().any(|seen| seen == id) {
            ids.push(id.to_string());
        }
    }
    ids.truncate(5);
    ids
}

fn fmt_suggestions(suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        return String::new();
    }
    let quoted: Vec<String> = suggestions.iter().map(|s| format!("'{s}'")).collect();
    format!("; did you mean {}?", quoted.join(" or "))
}

/// Why [`resolve_command`] found no single command.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    #[error(
        "No command has the id or name '{reference}'{}",
        fmt_suggestions(suggestions)
    )]
    NotFound {
        reference: String,
        /// Ids of commands with a similar id or name.
        suggestions: Vec<String>,
    },
    #[error(
        "'{reference}' matches several commands: {}; use an id",
        candidates.iter().map(|(id, group)| format!("{id} ({group})")).collect::<Vec<_>>().join(", ")
    )]
    Ambiguous {
        reference: String,
        /// `(id, group path)` of each match, in config order. The group path joins group names
        /// with ` > `.
        candidates: Vec<(String, String)>,
    },
}

/// Find the command `reference` names: the command with that exact id, or else the only
/// command whose name matches it case-insensitively.
///
/// An exact id always wins, so a reference that is one command's id is never `Ambiguous`, even
/// when other commands have it as their name (a root-level `lint` keeps the id `lint` while a
/// nested one becomes `backend/lint`).
///
/// # Errors
///
/// Returns `ResolveError::Ambiguous` if several commands have that name, and
/// `ResolveError::NotFound` if none has that id or name.
pub fn resolve_command<'a>(
    root: &'a CommandGroup,
    reference: &str,
) -> Result<&'a Command, ResolveError> {
    let mut commands = Vec::new();
    with_group_path(root, &root.name, &mut commands);
    if let Some((cmd, _)) = commands.iter().find(|(cmd, _)| cmd.id == reference) {
        return Ok(cmd);
    }
    let wanted = reference.to_lowercase();
    let named: Vec<&(&Command, String)> = commands
        .iter()
        .filter(|(cmd, _)| cmd.name.to_lowercase() == wanted)
        .collect();
    match named.as_slice() {
        [(cmd, _)] => Ok(cmd),
        [] => Err(ResolveError::NotFound {
            reference: reference.to_string(),
            suggestions: similar(
                reference,
                commands
                    .iter()
                    .map(|(cmd, _)| (cmd.id.as_str(), cmd.name.as_str())),
            ),
        }),
        _ => Err(ResolveError::Ambiguous {
            reference: reference.to_string(),
            candidates: named
                .iter()
                .map(|(cmd, group)| (cmd.id.clone(), group.clone()))
                .collect(),
        }),
    }
}

fn with_group_path<'a>(group: &'a CommandGroup, path: &str, out: &mut Vec<(&'a Command, String)>) {
    out.extend(group.commands.iter().map(|cmd| (cmd, path.to_string())));
    for child in &group.children {
        with_group_path(child, &format!("{path} > {}", child.name), out);
    }
}
