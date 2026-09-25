//! Project MCP config files for editors. They are JSONC, and usually committed, so they are
//! edited in place: only fnug's entry changes, and comments, key order and indentation stay.

use std::fmt;
use std::path::{Path, PathBuf};

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstNode, CstObject, CstRootNode};
use thiserror::Error;

use super::fsutil::write_atomic;

#[derive(Error, Debug)]
pub enum McpError {
    #[error("failed to read/write MCP config: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to parse {}: {message}", path.display())]
    Parse { path: PathBuf, message: String },

    #[error("invalid MCP config {}: {what} is not a JSON object", path.display())]
    NotAnObject { path: PathBuf, what: String },
}

/// A change to an editor's config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    Write(String),
    /// Nothing but empty objects and arrays would be left.
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editor {
    ClaudeCode,
    VsCode,
    Cursor,
}

impl Editor {
    pub const ALL: [Self; 3] = [Self::ClaudeCode, Self::VsCode, Self::Cursor];

    /// Relative path from project root to the MCP config file.
    fn config_rel_path(self) -> &'static str {
        match self {
            Self::ClaudeCode => ".mcp.json",
            Self::VsCode => ".vscode/mcp.json",
            Self::Cursor => ".cursor/mcp.json",
        }
    }

    /// The top-level JSON key that holds server entries.
    fn servers_key(self) -> &'static str {
        match self {
            Self::ClaudeCode | Self::Cursor => "mcpServers",
            Self::VsCode => "servers",
        }
    }

    /// Key that fnug 0.1.0-alpha.11 to alpha.13 wrongly wrote this editor's entry under.
    fn legacy_servers_key(self) -> Option<&'static str> {
        match self {
            Self::Cursor => Some("servers"),
            Self::ClaudeCode | Self::VsCode => None,
        }
    }

    /// Full path to the MCP config file for this editor.
    #[must_use]
    pub fn config_path(self, cwd: &Path) -> PathBuf {
        cwd.join(self.config_rel_path())
    }

    /// Whether the config in `cwd` has a fnug entry.
    ///
    /// # Errors
    ///
    /// Returns `McpError::Io` if the file can't be read, `McpError::Parse` if it isn't valid
    /// JSONC, and `McpError::NotAnObject` if it doesn't hold an object.
    pub fn status(self, cwd: &Path) -> Result<bool, McpError> {
        let path = self.config_path(cwd);
        let Some(root) = parse(&path)? else {
            return Ok(false);
        };
        let Some(value) = root.value() else {
            return Ok(false);
        };
        let object = value
            .as_object()
            .ok_or_else(|| not_an_object(&path, None))?;
        Ok(object
            .object_value(self.servers_key())
            .is_some_and(|servers| servers.get("fnug").is_some()))
    }

    /// Like [`Editor::status`], but a file that can't be read counts as not configured.
    #[must_use]
    pub fn is_installed(self, cwd: &Path) -> bool {
        self.status(cwd).unwrap_or(false)
    }

    /// The config file's new contents with a fnug entry running `fnug <args>`, or `None` if it
    /// already has one. An entry under the key older fnug versions wrote for this editor is
    /// removed.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Editor::status`], and `McpError::NotAnObject` if the servers key
    /// holds something other than an object.
    pub fn plan_install(self, cwd: &Path, args: &[String]) -> Result<Option<String>, McpError> {
        let path = self.config_path(cwd);
        let (root, new_file) = match parse(&path)? {
            Some(root) => (root, false),
            None => (parse_text(&path, "")?, true),
        };
        let object = root
            .object_value_or_create()
            .ok_or_else(|| not_an_object(&path, None))?;

        let mut changed = self
            .legacy_servers_key()
            .is_some_and(|legacy| remove_fnug_entry(&object, legacy));
        let key = self.servers_key();
        let servers = object
            .object_value_or_create(key)
            .ok_or_else(|| not_an_object(&path, Some(key)))?;
        if servers.get("fnug").is_none() {
            servers.append("fnug", server_entry(args));
            changed = true;
        }
        if !changed {
            return Ok(None);
        }
        let mut content = root.to_string();
        if new_file && !content.ends_with('\n') {
            content.push('\n');
        }
        Ok(Some(content))
    }

    /// The change that removes fnug's entry from the config, or `None` if it has none.
    ///
    /// # Errors
    ///
    /// Returns `McpError::Io` if the file can't be read and `McpError::Parse` if it isn't valid
    /// JSONC.
    pub fn plan_remove(self, cwd: &Path) -> Result<Option<FileChange>, McpError> {
        let path = self.config_path(cwd);
        let Some(root) = parse(&path)? else {
            return Ok(None);
        };
        let Some(object) = root.value().and_then(|v| v.as_object()) else {
            return Ok(None);
        };
        let mut changed = remove_fnug_entry(&object, self.servers_key());
        if let Some(legacy) = self.legacy_servers_key() {
            changed |= remove_fnug_entry(&object, legacy);
        }
        if !changed {
            return Ok(None);
        }
        let only_scaffolding = object
            .properties()
            .iter()
            .all(|prop| prop.value().is_some_and(|v| is_empty_collection(&v)));
        Ok(Some(if only_scaffolding {
            FileChange::Delete
        } else {
            FileChange::Write(root.to_string())
        }))
    }

    /// Make `change` to the config file. Writes are atomic, and deleting the file also removes
    /// its directory (`.vscode/`, `.cursor/`) if that is left empty.
    ///
    /// # Errors
    ///
    /// Returns `McpError::Io` if the file can't be written or deleted.
    pub fn apply(self, cwd: &Path, change: &FileChange) -> Result<(), McpError> {
        let path = self.config_path(cwd);
        match change {
            FileChange::Write(content) => write_atomic(&path, content, None)?,
            FileChange::Delete => {
                std::fs::remove_file(&path)?;
                if let Some(parent) = path.parent()
                    && parent != cwd
                {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }
        Ok(())
    }

    /// Add a fnug entry to this editor's config, creating the file if needed.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Editor::plan_install`] and [`Editor::apply`].
    pub fn install(self, cwd: &Path) -> Result<(), McpError> {
        match self.plan_install(cwd, &["mcp".to_string()])? {
            Some(content) => self.apply(cwd, &FileChange::Write(content)),
            None => Ok(()),
        }
    }

    /// Remove fnug's entry from this editor's config, deleting the file if nothing else is left.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Editor::plan_remove`] and [`Editor::apply`].
    pub fn remove(self, cwd: &Path) -> Result<(), McpError> {
        match self.plan_remove(cwd)? {
            Some(change) => self.apply(cwd, &change),
            None => Ok(()),
        }
    }
}

impl fmt::Display for Editor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaudeCode => write!(f, "Claude Code"),
            Self::VsCode => write!(f, "VS Code"),
            Self::Cursor => write!(f, "Cursor"),
        }
    }
}

/// The file's syntax tree, or `None` if it doesn't exist.
fn parse(path: &Path) -> Result<Option<CstRootNode>, McpError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_text(path, &text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn parse_text(path: &Path, text: &str) -> Result<CstRootNode, McpError> {
    // What editors accept: JSON plus comments and trailing commas
    let options = ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    };
    CstRootNode::parse(text, &options).map_err(|e| McpError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

fn not_an_object(path: &Path, key: Option<&str>) -> McpError {
    McpError::NotAnObject {
        path: path.to_path_buf(),
        what: key.map_or_else(|| "the file".to_string(), |key| format!("`{key}`")),
    }
}

fn server_entry(args: &[String]) -> CstInputValue {
    CstInputValue::Object(vec![
        ("type".to_string(), "stdio".into()),
        ("command".to_string(), "fnug".into()),
        (
            "args".to_string(),
            CstInputValue::Array(args.iter().map(|arg| arg.as_str().into()).collect()),
        ),
    ])
}

/// Remove `root[key].fnug`, and `root[key]` too if that leaves it empty.
fn remove_fnug_entry(root: &CstObject, key: &str) -> bool {
    let Some(servers) = root.object_value(key) else {
        return false;
    };
    let Some(entry) = servers.get("fnug") else {
        return false;
    };
    entry.remove();
    if servers.properties().is_empty()
        && let Some(prop) = root.get(key)
    {
        prop.remove();
    }
    true
}

fn is_empty_collection(node: &CstNode) -> bool {
    node.as_object().is_some_and(|o| o.properties().is_empty())
        || node.as_array().is_some_and(|a| a.elements().is_empty())
}
