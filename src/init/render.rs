//! Write proposed groups out as a config file.

use crate::config_file::{Config, ConfigAuto, ConfigCommand, ConfigCommandGroup, WorkspaceConfig};
use crate::schema::schema_url_for_version;

use super::InitError;

/// A YAML config named `name` with `groups`, for this fnug version, headed by a comment that
/// points the YAML language server at the schema. With neither groups nor `ws`, it has an
/// example command to replace.
///
/// # Errors
///
/// Returns `InitError::Yaml` if the config can't be serialized.
pub fn render(
    name: &str,
    groups: Vec<ConfigCommandGroup>,
    ws: Option<WorkspaceConfig>,
) -> Result<String, InitError> {
    let example = groups.is_empty() && ws.is_none();
    let yaml = serde_yaml::to_string(&config(name, groups, ws))?;
    let mut out = format!(
        "# yaml-language-server: $schema={}\n",
        schema_url_for_version()
    );
    if example {
        out.push_str(EXAMPLE_COMMENT);
    }
    out.push_str(&yaml);
    Ok(out)
}

/// [`render`] as JSON, with the schema in the `$schema` key.
pub(super) fn render_json(
    name: &str,
    groups: Vec<ConfigCommandGroup>,
    ws: Option<WorkspaceConfig>,
) -> Result<String, InitError> {
    let config = Config {
        schema: Some(schema_url_for_version()),
        ..config(name, groups, ws)
    };
    let mut json = serde_json::to_string_pretty(&config)?;
    json.push('\n');
    Ok(json)
}

const EXAMPLE_COMMENT: &str = "# Replace the example command with your project's checks, see\n\
# https://github.com/nickolaj-jepsen/fnug#configuration\n";

fn config(name: &str, groups: Vec<ConfigCommandGroup>, ws: Option<WorkspaceConfig>) -> Config {
    let example = (groups.is_empty() && ws.is_none()).then(|| {
        vec![ConfigCommand {
            id: None,
            name: "example".to_string(),
            cwd: None,
            cmd: "echo 'Replace me with a lint or test command'".to_string(),
            auto: Some(ConfigAuto {
                git: Some(true),
                watch: Some(true),
                ..ConfigAuto::default()
            }),
            env: None,
            depends_on: None,
            scrollback: None,
            timeout: None,
            exclusive: None,
        }]
    });
    Config {
        schema: None,
        fnug_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        workspace: ws,
        id: None,
        name: name.to_string(),
        auto: None,
        cwd: None,
        commands: example,
        children: (!groups.is_empty()).then_some(groups),
        env: None,
        timeout: None,
        exclusive: None,
    }
}
