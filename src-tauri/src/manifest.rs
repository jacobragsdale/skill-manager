//! Strict source manifest for explicit one-to-one installations.

use schemars::{generate::SchemaSettings, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

pub const SOURCE_MANIFEST_FILE: &str = "skill-manager.json";
pub const SOURCE_MANIFEST_VERSION: u8 = 1;
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SourceManifest {
    #[schemars(with = "i64", range(min = 1, max = 1))]
    pub version: u8,
    pub source: ManifestSource,
    #[schemars(length(min = 1))]
    pub installs: Vec<ManifestInstall>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManifestSource {
    #[schemars(
        length(min = 2, max = 16),
        regex(pattern = r"^[a-z](?:[a-z0-9]|-(?=[a-z0-9])){1,15}$")
    )]
    pub id: String,
    #[schemars(length(min = 1, max = 120))]
    pub name: String,
    #[schemars(length(min = 1, max = 1024))]
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManifestInstall {
    #[schemars(
        length(min = 1, max = 64),
        regex(pattern = r"^[a-z0-9](?:[a-z0-9]|-(?=[a-z0-9])){0,63}$")
    )]
    pub id: String,
    #[schemars(length(min = 1))]
    pub source: String,
    #[schemars(length(min = 1))]
    pub destination: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum LegacyDestinationAnchor {
    Home,
    Config,
    Data,
    LocalData,
    Cache,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDestination {
    anchor: LegacyDestinationAnchor,
    path: String,
}

impl SourceManifest {
    pub fn from_slice(contents: &[u8]) -> Result<Self, String> {
        if contents.len() > MAX_MANIFEST_BYTES {
            return Err("skill-manager.json is larger than the 1 MB limit.".to_string());
        }
        let mut value = serde_json::from_slice::<serde_json::Value>(contents)
            .map_err(|error| format!("Could not parse skill-manager.json: {error}"))?;
        migrate_legacy_destinations(&mut value)?;
        let manifest = serde_json::from_value::<Self>(value)
            .map_err(|error| format!("Could not parse skill-manager.json: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != SOURCE_MANIFEST_VERSION {
            return Err(format!(
                "skill-manager.json uses unsupported version {}.",
                self.version
            ));
        }
        validate_source_id(&self.source.id)?;
        validate_text(&self.source.name, "source.name", 1, 120)?;
        validate_text(&self.source.description, "source.description", 1, 1024)?;
        if self.installs.is_empty() {
            return Err("skill-manager.json does not publish any installs.".to_string());
        }
        Ok(())
    }

    pub fn referenced_repository_paths(&self) -> BTreeSet<String> {
        self.installs
            .iter()
            .map(|install| install.source.clone())
            .chain(std::iter::once(SOURCE_MANIFEST_FILE.to_string()))
            .collect()
    }
}

fn migrate_legacy_destinations(value: &mut serde_json::Value) -> Result<(), String> {
    let Some(installs) = value
        .get_mut("installs")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Ok(());
    };
    for install in installs {
        let Some(destination) = install.get_mut("destination") else {
            continue;
        };
        if !destination.is_object() {
            continue;
        }
        let legacy = serde_json::from_value::<LegacyDestination>(destination.take())
            .map_err(|error| format!("Could not parse skill-manager.json: {error}"))?;
        let relative = Path::new(&legacy.path);
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(format!(
                "Could not parse skill-manager.json: invalid legacy destination path {:?}.",
                legacy.path
            ));
        }
        let root = legacy_destination_root(legacy.anchor)?;
        *destination = serde_json::Value::String(root.join(relative).display().to_string());
    }
    Ok(())
}

fn legacy_destination_root(anchor: LegacyDestinationAnchor) -> Result<PathBuf, String> {
    if let Some(root) = crate::qa_paths::root()? {
        return Ok(match anchor {
            LegacyDestinationAnchor::Home => root.join("home"),
            LegacyDestinationAnchor::Config => root.join("config"),
            LegacyDestinationAnchor::Data => root.join("data"),
            LegacyDestinationAnchor::LocalData => root.join("local-data"),
            LegacyDestinationAnchor::Cache => root.join("cache"),
        });
    }
    let path = match anchor {
        LegacyDestinationAnchor::Home => dirs::home_dir(),
        LegacyDestinationAnchor::Config => dirs::config_dir(),
        LegacyDestinationAnchor::Data => dirs::data_dir(),
        LegacyDestinationAnchor::LocalData => dirs::data_local_dir(),
        LegacyDestinationAnchor::Cache => dirs::cache_dir(),
    };
    path.ok_or_else(|| "Could not resolve a legacy destination directory.".to_string())
}

pub fn source_manifest_schema_json() -> Result<String, String> {
    let generator = SchemaSettings::draft2020_12().into_generator();
    let schema = generator.into_root_schema_for::<SourceManifest>();
    let mut schema = serde_json::to_value(schema)
        .map_err(|error| format!("Could not prepare the source manifest schema: {error}"))?;
    if let Some(version) = schema.pointer_mut("/properties/version") {
        version
            .as_object_mut()
            .expect("the generated version schema is an object")
            .remove("format");
    }
    let mut output = serde_json::to_string_pretty(&schema)
        .map_err(|error| format!("Could not serialize the source manifest schema: {error}"))?;
    output.push('\n');
    Ok(output)
}

fn validate_source_id(value: &str) -> Result<(), String> {
    let valid = (2..=16).contains(&value.len())
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Invalid source.id {value:?}; use 2-16 lowercase ASCII letters, digits, and single hyphens, beginning with a letter."
        ))
    }
}

fn validate_text(value: &str, label: &str, min: usize, max: usize) -> Result<(), String> {
    if (min..=max).contains(&value.chars().count()) {
        Ok(())
    } else {
        Err(format!("{label} must contain {min}-{max} characters."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{
      "version": 1,
      "source": {
        "id": "fiqit",
        "name": "Fiqit",
        "description": "Shared agent configuration."
      },
      "installs": [{
        "id": "review",
        "source": "skills/review",
        "destination": "~/.agents/skills/fiqit-review"
      }]
    }"#;

    #[test]
    fn explicit_install_contract_is_accepted() {
        let manifest = SourceManifest::from_slice(EXAMPLE.as_bytes()).expect("valid manifest");
        assert_eq!(manifest.source.id, "fiqit");
        assert_eq!(manifest.installs.len(), 1);
    }

    #[test]
    fn source_ids_unknown_fields_and_empty_installs_are_rejected() {
        let invalid_id = EXAMPLE.replace("\"fiqit\"", "\"Fiqit\"");
        assert!(SourceManifest::from_slice(invalid_id.as_bytes())
            .expect_err("invalid source id")
            .contains("Invalid source.id"));
        let unknown = EXAMPLE.replace("\"version\": 1,", "\"version\": 1, \"extra\": true,");
        assert!(SourceManifest::from_slice(unknown.as_bytes())
            .expect_err("unknown field")
            .contains("unknown field"));
        let empty = EXAMPLE.replace(
            "[{\n        \"id\": \"review\",\n        \"source\": \"skills/review\",\n        \"destination\": \"~/.agents/skills/fiqit-review\"\n      }]",
            "[]",
        );
        assert!(SourceManifest::from_slice(empty.as_bytes())
            .expect_err("empty installs")
            .contains("does not publish"));
    }

    #[test]
    fn schema_matches_the_published_golden_file() {
        let generated = source_manifest_schema_json().expect("generated schema");
        let published = include_str!("../../schemas/v1/source-manifest.schema.json");
        let generated =
            serde_json::from_str::<serde_json::Value>(&generated).expect("generated schema JSON");
        let published =
            serde_json::from_str::<serde_json::Value>(published).expect("published schema JSON");
        assert_eq!(
            generated, published,
            "regenerate the source manifest schema"
        );
    }
}
