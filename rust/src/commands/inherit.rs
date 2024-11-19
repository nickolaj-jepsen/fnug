use crate::commands::auto::Auto;
use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::config_file::ConfigError;
use std::io;
use std::path::PathBuf;

pub fn inherit_path(parent: &PathBuf, child: PathBuf) -> PathBuf {
    if child == PathBuf::new() {
        parent.clone()
    } else if child.is_relative() {
        parent.join(child)
    } else {
        child.clone()
    }
}

#[derive(Default, Clone)]
pub struct Inheritance {
    cwd: PathBuf,
    auto: Auto,
    entry_path: Vec<String>,
}

impl Inheritance {
    fn canonicalize(&mut self) -> Result<(), io::Error> {
        if self.cwd != PathBuf::new() {
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
    fn calculate_inheritance(&self, inheritance: &Inheritance) -> Result<Inheritance, ConfigError>;
    fn apply_inheritance(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError>;
    fn inherit(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError> {
        let mut inherited = self.calculate_inheritance(inheritance)?;
        inherited
            .canonicalize()
            .map_err(|_| ConfigError::DirectoryNotFound {
                path: inherited.cwd.clone(),
                entry: inherited.entry_path.join("."),
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
        let mut auto = self.merge(&inheritance.auto);
        auto.path = auto
            .path
            .iter()
            .map(|p| inherit_path(&inheritance.cwd, p.clone()))
            .collect();
        Ok(Inheritance {
            cwd: PathBuf::new(),
            auto,
            entry_path: inheritance.merge_entry_path("auto"),
        })
    }

    fn apply_inheritance(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError> {
        self.watch = inheritance.auto.watch;
        self.git = inheritance.auto.git;
        self.path = inheritance.auto.path.clone();
        self.regex = inheritance.auto.regex.clone();
        self.always = inheritance.auto.always;

        Ok(())
    }
}

impl Inheritable for Command {
    fn calculate_inheritance(&self, inheritance: &Inheritance) -> Result<Inheritance, ConfigError> {
        Ok(Inheritance {
            cwd: inherit_path(&inheritance.cwd, self.cwd.clone()),
            auto: self.auto.merge(&inheritance.auto),
            entry_path: inheritance.merge_entry_path(&self.name),
        })
    }

    fn apply_inheritance(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError> {
        self.cwd = inheritance.cwd.clone();
        self.auto.inherit(inheritance)?;
        Ok(())
    }
}

impl Inheritable for CommandGroup {
    fn calculate_inheritance(&self, inheritance: &Inheritance) -> Result<Inheritance, ConfigError> {
        Ok(Inheritance {
            cwd: inherit_path(&inheritance.cwd, self.cwd.clone()),
            auto: self.auto.merge(&inheritance.auto),
            entry_path: inheritance.merge_entry_path(&self.name),
        })
    }

    fn apply_inheritance(&mut self, inheritance: &Inheritance) -> Result<(), ConfigError> {
        self.cwd = inheritance.cwd.clone();
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
                interactive: false,
                cwd: PathBuf::new(),
                auto: Auto::default(),
            }],
            children: vec![],
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
                interactive: false,
                cwd: PathBuf::from("subdir"),
                auto: Auto::default(),
            }],
            children: vec![],
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
                interactive: false,
                cwd: PathBuf::new(),
                auto: Auto::default(),
            }],
            children: vec![],
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
                interactive: false,
                cwd: PathBuf::new(),
                auto: Auto::default(),
            }],
            children: vec![],
        };

        let mut parent_group = CommandGroup {
            id: "1".to_string(),
            name: "parent".to_string(),
            auto: parent_auto,
            cwd: root.clone(),
            commands: vec![],
            children: vec![child_group],
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
                interactive: false,
                cwd: PathBuf::from("./subdir"),
                auto: Auto::default(),
            }],
            children: vec![],
        };

        group.inherit(&Inheritance::from(root.clone())).unwrap();
        assert_eq!(group.commands[0].cwd, root.join("subdir"));
    }

    #[test]
    fn test_auto_invalid_regex() {
        let result = Auto::new(
            Some(true),
            Some(false),
            vec![],
            vec!["[invalid".to_string()],
            Some(false),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_valid_regex() {
        let result = Auto::new(
            Some(true),
            Some(false),
            vec![],
            vec![".*\\.rs$".to_string()],
            Some(false),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_auto_path_validation() {
        let auto = Auto::new(
            Some(true),
            Some(false),
            vec![PathBuf::from("/nonexistent/path")],
            vec![],
            Some(false),
        );
        assert!(auto.is_ok());

        let auto = Auto::new(
            Some(true),
            Some(false),
            vec![PathBuf::new()],
            vec![],
            Some(false),
        );
        assert!(auto.is_ok());
    }

    #[test]
    fn test_nested_invalid_auto_inheritance() {
        let parent_auto = Auto::new(
            Some(true),
            Some(true),
            vec![],
            vec![".*\\.rs$".to_string()],
            Some(false),
        )
        .unwrap();
        let child_auto = Auto::new(
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
            cwd: PathBuf::from("/root"),
            commands: vec![],
            children: vec![],
        };

        // Parent group with valid Auto should still work
        parent_group
            .inherit(&Inheritance::from(PathBuf::from("/root")))
            .unwrap();
        assert_eq!(parent_group.cwd, PathBuf::from("/root"));
    }
}
