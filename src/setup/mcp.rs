use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum McpError {
    #[error("failed to read/write MCP config: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to parse MCP config: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid MCP config: root is not a JSON object")]
    NotAnObject,
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
            Self::ClaudeCode => "mcpServers",
            Self::VsCode | Self::Cursor => "servers",
        }
    }

    /// Full path to the MCP config file for this editor.
    #[must_use]
    pub fn config_path(self, cwd: &Path) -> PathBuf {
        cwd.join(self.config_rel_path())
    }

    /// Check if fnug MCP is configured for this editor.
    #[must_use]
    pub fn is_installed(self, cwd: &Path) -> bool {
        let path = self.config_path(cwd);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .and_then(|json| json.get(self.servers_key())?.get("fnug").cloned())
            .is_some()
    }

    /// Install fnug MCP config for this editor (read-modify-write).
    ///
    /// # Errors
    ///
    /// Returns `McpError` on IO or JSON parse failures.
    pub fn install(self, cwd: &Path) -> Result<(), McpError> {
        let path = self.config_path(cwd);
        let mut json = read_json(&path)?.unwrap_or_else(|| serde_json::json!({}));

        let root = json.as_object_mut().ok_or(McpError::NotAnObject)?;
        let servers = root
            .entry(self.servers_key())
            .or_insert_with(|| serde_json::json!({}));
        let servers = servers.as_object_mut().ok_or(McpError::NotAnObject)?;
        servers.insert("fnug".to_string(), fnug_server_entry());

        write_json(&path, &json)
    }

    /// Remove fnug MCP config for this editor.
    ///
    /// # Errors
    ///
    /// Returns `McpError` on IO or JSON parse failures.
    pub fn remove(self, cwd: &Path) -> Result<(), McpError> {
        let path = self.config_path(cwd);
        let Some(mut json) = read_json(&path)? else {
            return Ok(());
        };

        let root = json.as_object_mut().ok_or(McpError::NotAnObject)?;
        if let Some(servers) = root
            .get_mut(self.servers_key())
            .and_then(|s| s.as_object_mut())
        {
            servers.remove("fnug");
            if servers.is_empty() {
                root.remove(self.servers_key());
            }
        }

        // Delete file if no meaningful content remains (only empty arrays/objects).
        let dominated = root.values().all(is_empty_collection);
        if dominated {
            std::fs::remove_file(&path)?;
            // Remove parent dir if it's now empty (e.g. .vscode/, .cursor/)
            if let Some(parent) = path.parent()
                && parent != cwd
            {
                let _ = std::fs::remove_dir(parent);
            }
        } else {
            write_json(&path, &json)?;
        }

        Ok(())
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

fn fnug_server_entry() -> serde_json::Value {
    serde_json::json!({
        "type": "stdio",
        "command": "fnug",
        "args": ["mcp"]
    })
}

fn is_empty_collection(v: &serde_json::Value) -> bool {
    v.as_array().is_some_and(Vec::is_empty) || v.as_object().is_some_and(serde_json::Map::is_empty)
}

/// Read and parse a JSON file, returning `None` if it doesn't exist.
fn read_json(path: &Path) -> Result<Option<serde_json::Value>, McpError> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(serde_json::from_str(&content)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Write a JSON value to a file, creating parent directories as needed.
fn write_json(path: &Path, json: &serde_json::Value) -> Result<(), McpError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let formatted = serde_json::to_string_pretty(json)?;
    std::fs::write(path, format!("{formatted}\n"))?;
    Ok(())
}
