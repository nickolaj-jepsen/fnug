use crate::commands::auto::Auto;
use crate::commands::command::Command;
use crate::commands::env;
use crate::commands::group::CommandGroup;
use crate::config_file::ConfigError;
use log::warn;
use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};

#[must_use]
pub fn inherit_path(parent: &Path, child: PathBuf) -> PathBuf {
    if child.as_os_str().is_empty() {
        parent.to_path_buf()
    } else if child.is_relative() {
        parent.join(child)
    } else {
        child
    }
}

#[derive(Default, Clone)]
pub struct Inheritance {
    cwd: PathBuf,
    auto: Auto,
    entry_path: Vec<String>,
    env: HashMap<String, String>,
}

/// A configured path that could not be resolved.
struct PathError {
    field: String,
    path: PathBuf,
    source: io::Error,
}

/// Canonicalize `path`, allowing its tail to be missing: the deepest existing ancestor is
/// canonicalized and the missing components are appended. This keeps missing paths comparable
/// with canonical paths (e.g. git's realpath'd work tree).
fn canonicalize_lenient(path: &Path) -> io::Result<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut resolved) => {
                // Missing components can't be symlinks, so `..` among them is resolved lexically.
                for component in missing.into_iter().rev() {
                    match component {
                        Component::ParentDir => {
                            resolved.pop();
                        }
                        Component::Normal(name) => resolved.push(name),
                        _ => {}
                    }
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match (existing.parent(), existing.components().next_back()) {
                    (Some(parent), Some(last)) => {
                        missing.push(last);
                        existing = parent;
                    }
                    _ => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
}

impl Inheritance {
    /// Resolve `cwd`, which must exist, and `auto.path`, which may name missing paths.
    fn canonicalize(&mut self) -> Result<(), PathError> {
        if !self.cwd.as_os_str().is_empty() {
            self.cwd = self.cwd.canonicalize().map_err(|source| PathError {
                field: "cwd".to_string(),
                path: self.cwd.clone(),
                source,
            })?;
        }
        if let Some(paths) = &self.auto.path {
            let canonical = paths
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let path = inherit_path(&self.cwd, p.clone());
                    canonicalize_lenient(&path).map_err(|source| PathError {
                        field: format!("auto.path[{i}]"),
                        path,
                        source,
                    })
                })
                .collect::<Result<Vec<PathBuf>, PathError>>()?;
            self.auto.path = Some(canonical);
        }
        Ok(())
    }

    fn merge_entry_path(&self, entry: &str) -> Vec<String> {
        let mut new_entry_path = self.entry_path.clone();
        new_entry_path.push(entry.to_string());
        new_entry_path
    }

    /// A fresh start for a workspace package: nothing is inherited, but errors keep naming
    /// the package's place in the tree.
    fn scope_root(entry_path: Vec<String>) -> Self {
        Inheritance {
            entry_path,
            ..Default::default()
        }
    }
}

impl From<PathBuf> for Inheritance {
    fn from(cwd: PathBuf) -> Self {
        Inheritance {
            cwd,
            ..Default::default()
        }
    }
}

/// A trait for types that can inherit settings from another instance, or another type, eg command from command group
pub trait Inheritable: Sized {
    /// Calculate the inheritance state for this item.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` if the inheritance calculation fails.
    fn calculate_inheritance(&self, inheritance: &Inheritance) -> Result<Inheritance, ConfigError>;

    /// Apply previously calculated inheritance to this item.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` if applying inheritance fails.
    fn apply_inheritance(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError>;

    /// Calculate and apply inheritance in one step.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::DirectoryNotFound` if the working directory does not exist, or an
    /// `auto.path` can't be resolved for another reason than being missing.
    fn inherit(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError> {
        let mut inherited = self.calculate_inheritance(inheritance)?;
        inherited
            .canonicalize()
            .map_err(|e| ConfigError::DirectoryNotFound {
                entry: inherited.entry_path.join(" > "),
                field: e.field,
                path: e.path,
                source: e.source,
            })?;
        self.apply_inheritance(&inherited)
    }
}

impl Auto {
    fn merge(&self, other: &Auto) -> Auto {
        Auto {
            watch: self.watch.or(other.watch),
            git: self.git.or(other.git),
            path: self.path.clone().or_else(|| other.path.clone()),
            regex: self.regex.clone().or_else(|| other.regex.clone()),
            always: self.always.or(other.always),
            check: self.check.or(other.check),
        }
    }
}

impl Inheritable for Auto {
    fn calculate_inheritance(&self, inheritance: &Inheritance) -> Result<Inheritance, ConfigError> {
        let mut auto = self.merge(&inheritance.auto);

        if auto.paths().is_empty() {
            auto.path = Some(vec![inheritance.cwd.clone()]);
        }

        Ok(Inheritance {
            cwd: inheritance.cwd.clone(),
            auto,
            entry_path: inheritance.merge_entry_path("auto"),
            env: inheritance.env.clone(),
        })
    }

    fn apply_inheritance(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError> {
        self.watch = inheritance.auto.watch;
        self.git = inheritance.auto.git;
        self.path.clone_from(&inheritance.auto.path);
        self.regex.clone_from(&inheritance.auto.regex);
        self.always = inheritance.auto.always;
        self.check = inheritance.auto.check;

        Ok(())
    }
}

fn calculate_common_inheritance(
    name: &str,
    cwd: &Path,
    auto: &Auto,
    env: &HashMap<String, String>,
    inheritance: &Inheritance,
) -> Inheritance {
    let entry_path = inheritance.merge_entry_path(name);
    // Values see the parent's env, then the process env, but not their siblings.
    let lookup = |var: &str| {
        inheritance
            .env
            .get(var)
            .cloned()
            .or_else(|| std::env::var_os(var).map(|value| value.to_string_lossy().into_owned()))
    };
    let mut merged_env = inheritance.env.clone();
    for (key, value) in env {
        let (expanded, undefined) = env::expand(value, lookup);
        for var in undefined {
            warn!(
                "{}: env {key} uses ${var}, which is not set, so it expands to nothing",
                entry_path.join(" > ")
            );
        }
        merged_env.insert(key.clone(), expanded);
    }
    Inheritance {
        cwd: inherit_path(&inheritance.cwd, cwd.to_path_buf()),
        auto: auto.merge(&inheritance.auto),
        entry_path,
        env: merged_env,
    }
}

impl Inheritable for Command {
    fn calculate_inheritance(&self, inheritance: &Inheritance) -> Result<Inheritance, ConfigError> {
        Ok(calculate_common_inheritance(
            &self.name,
            &self.cwd,
            &self.auto,
            &self.env,
            inheritance,
        ))
    }

    fn apply_inheritance(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError> {
        self.cwd.clone_from(&inheritance.cwd);
        self.env.clone_from(&inheritance.env);
        self.auto.inherit(inheritance)?;
        Ok(())
    }
}

impl Inheritable for CommandGroup {
    fn calculate_inheritance(&self, inheritance: &Inheritance) -> Result<Inheritance, ConfigError> {
        Ok(calculate_common_inheritance(
            &self.name,
            &self.cwd,
            &self.auto,
            &self.env,
            inheritance,
        ))
    }

    fn apply_inheritance(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError> {
        self.cwd.clone_from(&inheritance.cwd);
        self.env.clone_from(&inheritance.env);
        self.auto.inherit(inheritance)?;
        for command in &mut self.commands {
            command.inherit(inheritance)?;
        }
        for child in &mut self.children {
            if child.source.is_some() {
                child.inherit(&Inheritance::scope_root(inheritance.entry_path.clone()))?;
            } else {
                child.inherit(inheritance)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::auto::Auto;
    use crate::commands::command::Command;
    use crate::commands::group::CommandGroup;
    use crate::commands::inherit::{Inheritable, Inheritance};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_dir_all(path: &std::path::Path) {
        fs::create_dir_all(path)
            .unwrap_or_else(|e| panic!("Failed to create directory {}: {}", path.display(), e));
    }

    #[test]
    fn test_basic_cwd_inheritance() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        create_dir_all(&root);

        let mut group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: Auto::default(),
            cwd: root.clone(),
            commands: vec![Command {
                id: "2".to_string(),
                name: "child".to_string(),
                cmd: "echo test".to_string(),

                cwd: PathBuf::new(),
                auto: Auto::default(),
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        group.inherit(&Inheritance::from(root.clone())).unwrap();
        assert_eq!(group.commands[0].cwd, root);
    }

    #[test]
    fn test_relative_path_resolution() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let subdir = root.join("subdir");
        create_dir_all(&subdir);

        let mut group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: Auto::default(),
            cwd: root.clone(),
            commands: vec![Command {
                id: "2".to_string(),
                name: "child".to_string(),
                cmd: "echo test".to_string(),

                cwd: PathBuf::from("subdir"),
                auto: Auto::default(),
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        group.inherit(&Inheritance::from(root.clone())).unwrap();
        assert_eq!(group.commands[0].cwd, root.join("subdir"));
    }

    #[test]
    fn test_auto_settings_inheritance() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        create_dir_all(&root);

        let parent_auto = Auto {
            watch: Some(true),
            git: Some(true),
            path: None,
            regex: None,
            always: Some(false),
            check: None,
        };
        let mut group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: parent_auto,
            cwd: root.clone(),
            commands: vec![Command {
                id: "2".to_string(),
                name: "child".to_string(),
                cmd: "echo test".to_string(),

                cwd: PathBuf::new(),
                auto: Auto::default(),
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        group.inherit(&Inheritance::default()).unwrap();
        assert!(group.commands[0].auto.watch.unwrap());
        assert!(group.commands[0].auto.git.unwrap());
    }

    #[test]
    fn test_nested_inheritance() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let subdir = root.join("subdir");
        create_dir_all(&subdir);

        let parent_auto = Auto {
            watch: Some(true),
            git: Some(true),
            path: Some(vec![root.clone()]),
            regex: None,
            always: Some(false),
            check: None,
        };

        let child_group = CommandGroup {
            id: "2".to_string(),
            name: "child_group".to_string(),
            auto: Auto::default(),
            cwd: PathBuf::from("subdir"),
            commands: vec![Command {
                id: "3".to_string(),
                name: "command".to_string(),
                cmd: "echo test".to_string(),

                cwd: PathBuf::new(),
                auto: Auto::default(),
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        let mut parent_group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: parent_auto,
            cwd: root.clone(),
            commands: vec![],
            children: vec![child_group],
            ..Default::default()
        };

        parent_group
            .inherit(&Inheritance::from(root.clone()))
            .unwrap();
        assert_eq!(
            parent_group.children[0].commands[0].cwd,
            root.join("subdir")
        );
        assert!(parent_group.children[0].commands[0].auto.watch.unwrap());
        assert!(parent_group.children[0].commands[0].auto.git.unwrap());
    }

    #[test]
    fn test_no_base_path() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let subdir = root.join("subdir");
        create_dir_all(&subdir);

        let parent_auto = Auto {
            watch: Some(true),
            git: Some(true),
            path: None,
            regex: None,
            always: Some(false),
            check: None,
        };

        let mut group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: parent_auto,
            cwd: PathBuf::new(),
            commands: vec![Command {
                id: "2".to_string(),
                name: "child".to_string(),
                cmd: "echo test".to_string(),

                cwd: PathBuf::from("./subdir"),
                auto: Auto::default(),
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        group.inherit(&Inheritance::from(root.clone())).unwrap();
        assert_eq!(group.commands[0].cwd, root.join("subdir"));
    }

    #[test]
    fn test_nested_invalid_auto_inheritance() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();

        let parent_auto = Auto::create(
            Some(true),
            Some(true),
            vec![],
            vec![".*\\.rs$".to_string()],
            Some(false),
            None,
        )
        .unwrap();
        let child_auto = Auto::create(
            Some(true),
            Some(true),
            vec![],
            vec!["[invalid".to_string()],
            Some(false),
            None,
        );
        assert!(child_auto.is_err());

        let mut parent_group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: parent_auto,
            cwd: root.clone(),
            commands: vec![],
            children: vec![],
            ..Default::default()
        };

        // Parent group with valid Auto should still work
        parent_group
            .inherit(&Inheritance::from(root.clone()))
            .unwrap();
        assert_eq!(parent_group.cwd, root);
    }

    #[test]
    fn test_empty_auto_path_should_inherit_parent_command_path() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let subdir = root.join("subdir");
        create_dir_all(&subdir);

        let mut command = Command {
            id: "2".to_string(),
            name: "child".to_string(),
            cmd: "echo test".to_string(),
            cwd: PathBuf::from("subdir"),
            auto: Auto {
                watch: Some(true),
                git: Some(true),
                path: None,
                regex: None,
                always: Some(false),
                check: None,
            },
            ..Default::default()
        };

        command.inherit(&Inheritance::from(root.clone())).unwrap();

        assert_eq!(command.auto.paths(), [root.join("subdir")]);
    }

    #[test]
    fn test_empty_auto_path_should_inherit_parent_command_path_unless_parent_auto_has_path() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let subdir = root.join("subdir");
        create_dir_all(&subdir);

        let mut group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: Auto {
                watch: Some(true),
                git: Some(true),
                path: None,
                regex: None,
                always: Some(false),
                check: None,
            },
            cwd: root.clone(),
            commands: vec![Command {
                id: "2".to_string(),
                name: "child".to_string(),
                cmd: "echo test".to_string(),

                cwd: PathBuf::from("subdir"),
                auto: Auto {
                    watch: Some(true),
                    git: Some(true),
                    path: None,
                    regex: None,
                    always: Some(false),
                    check: None,
                },
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        group.inherit(&Inheritance::from(root.clone())).unwrap();
        assert_eq!(group.commands[0].auto.paths(), [root.join("subdir")]);

        let mut group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: Auto {
                watch: Some(true),
                git: Some(true),
                path: Some(vec![root.clone()]),
                regex: None,
                always: Some(false),
                check: None,
            },
            cwd: root.clone(),
            commands: vec![Command {
                id: "2".to_string(),
                name: "child".to_string(),
                cmd: "echo test".to_string(),

                cwd: PathBuf::from("subdir"),
                auto: Auto {
                    watch: Some(true),
                    git: Some(true),
                    path: None,
                    regex: None,
                    always: Some(false),
                    check: None,
                },
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        group.inherit(&Inheritance::from(root.clone())).unwrap();
        assert_eq!(group.commands[0].auto.paths(), [root]);
    }

    #[test]
    fn test_always_inherits_without_watch_or_git() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();

        let mut group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: Auto {
                always: Some(true),
                ..Default::default()
            },
            cwd: root.clone(),
            commands: vec![Command {
                id: "2".to_string(),
                name: "child".to_string(),
                cmd: "echo test".to_string(),
                cwd: PathBuf::new(),
                auto: Auto::default(),
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        group.inherit(&Inheritance::from(root)).unwrap();
        assert_eq!(
            group.commands[0].auto.always,
            Some(true),
            "always: true should be inherited from parent group even without watch/git"
        );
    }
}
