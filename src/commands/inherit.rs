use crate::commands::auto::Auto;
use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::config_file::ConfigError;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

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

impl Inheritance {
    fn canonicalize(&mut self) -> Result<(), io::Error> {
        if !self.cwd.as_os_str().is_empty() {
            self.cwd.canonicalize()?;
        }
        self.auto.path = self
            .auto
            .path
            .iter()
            .map(|p| inherit_path(&self.cwd, p.clone()).canonicalize())
            .collect::<Result<Vec<PathBuf>, io::Error>>()?;
        Ok(())
    }

    fn merge_entry_path(&self, entry: &str) -> Vec<String> {
        let mut new_entry_path = self.entry_path.clone();
        new_entry_path.push(entry.to_string());
        new_entry_path
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
    /// Returns `ConfigError::DirectoryNotFound` if a referenced directory does not exist.
    fn inherit(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError> {
        let mut inherited = self.calculate_inheritance(inheritance)?;
        inherited
            .canonicalize()
            .map_err(|e| ConfigError::DirectoryNotFound {
                path: inherited.cwd.clone(),
                entry: inherited.entry_path.join("."),
                source: e,
            })?;
        self.apply_inheritance(&inherited)
    }
}

impl Auto {
    fn merge(&self, other: &Auto) -> Auto {
        let path = if self.path.is_empty() {
            other.path.clone()
        } else {
            self.path.clone()
        };
        let regex = if self.regex.is_empty() {
            other.regex.clone()
        } else {
            self.regex.clone()
        };
        Auto {
            watch: self.watch.or(other.watch),
            git: self.git.or(other.git),
            path,
            regex,
            always: self.always.or(other.always),
        }
    }
}

impl Inheritable for Auto {
    fn calculate_inheritance(&self, inheritance: &Inheritance) -> Result<Inheritance, ConfigError> {
        // Only inherit auto settings if the parent has watch, git, or always enabled
        let mut auto = if inheritance.auto.watch.unwrap_or(false)
            || inheritance.auto.git.unwrap_or(false)
            || inheritance.auto.always.unwrap_or(false)
        {
            self.merge(&inheritance.auto)
        } else {
            self.clone()
        };

        // If the path is empty, inherit the cwd from the parent
        if auto.path.is_empty() {
            auto.path.push(inheritance.cwd.clone());
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
    let mut merged_env = inheritance.env.clone();
    merged_env.extend(env.clone());
    Inheritance {
        cwd: inherit_path(&inheritance.cwd, cwd.to_path_buf()),
        auto: auto.merge(&inheritance.auto),
        entry_path: inheritance.merge_entry_path(name),
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
            child.inherit(inheritance)?;
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
            path: vec![],
            regex: vec![],
            always: Some(false),
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
            path: vec![root.clone()],
            regex: vec![],
            always: Some(false),
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
            path: vec![],
            regex: vec![],
            always: Some(false),
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
        )
        .unwrap();
        let child_auto = Auto::create(
            Some(true),
            Some(true),
            vec![],
            vec!["[invalid".to_string()],
            Some(false),
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
                path: vec![],
                regex: vec![],
                always: Some(false),
            },
            ..Default::default()
        };

        command.inherit(&Inheritance::from(root.clone())).unwrap();

        assert_eq!(command.auto.path, vec![root.join("subdir")]);
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
                path: vec![],
                regex: vec![],
                always: Some(false),
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
                    path: vec![],
                    regex: vec![],
                    always: Some(false),
                },
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        group.inherit(&Inheritance::from(root.clone())).unwrap();
        assert_eq!(group.commands[0].auto.path, vec![root.join("subdir")]);

        let mut group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: Auto {
                watch: Some(true),
                git: Some(true),
                path: vec![root.clone()],
                regex: vec![],
                always: Some(false),
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
                    path: vec![],
                    regex: vec![],
                    always: Some(false),
                },
                ..Default::default()
            }],
            children: vec![],
            ..Default::default()
        };

        group.inherit(&Inheritance::from(root.clone())).unwrap();
        assert_eq!(group.commands[0].auto.path, vec![root]);
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
