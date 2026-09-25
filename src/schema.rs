//! JSON Schema for fnug config files.

use schemars::generate::SchemaSettings;

use crate::config_file::Config;

/// Schema URL that tracks the `main` branch.
pub const SCHEMA_URL_LATEST: &str =
    "https://raw.githubusercontent.com/nickolaj-jepsen/fnug/main/schema/fnug.schema.json";

/// The config file's JSON Schema (draft-07), pretty-printed with a trailing newline.
///
/// # Panics
///
/// Panics if the schema cannot be serialized, which would be a bug in the schema types.
#[must_use]
pub fn config_schema_json() -> String {
    let schema = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<Config>();
    let mut json = serde_json::to_string_pretty(&schema).expect("schema serializes to JSON");
    json.push('\n');
    json
}

/// Schema URL pinned to this fnug release's tag.
#[must_use]
pub fn schema_url_for_version() -> String {
    format!(
        "https://raw.githubusercontent.com/nickolaj-jepsen/fnug/v{}/schema/fnug.schema.json",
        env!("CARGO_PKG_VERSION")
    )
}
