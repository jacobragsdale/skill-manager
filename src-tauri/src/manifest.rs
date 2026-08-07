//! Strict source manifest for explicit one-to-one installations.

use schemars::{generate::SchemaSettings, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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
    pub destination: Destination,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Destination {
    pub anchor: DestinationAnchor,
    #[schemars(length(min = 1))]
    pub path: String,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "camelCase")]
pub enum DestinationAnchor {
    Home,
    Config,
    Data,
    LocalData,
    Cache,
}

impl SourceManifest {
    pub fn from_slice(contents: &[u8]) -> Result<Self, String> {
        if contents.len() > MAX_MANIFEST_BYTES {
            return Err("skill-manager.json is larger than the 1 MB limit.".to_string());
        }
        let manifest = serde_json::from_slice::<Self>(contents)
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
        "destination": { "anchor": "home", "path": ".agents/skills/fiqit-review" }
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
            "[{\n        \"id\": \"review\",\n        \"source\": \"skills/review\",\n        \"destination\": { \"anchor\": \"home\", \"path\": \".agents/skills/fiqit-review\" }\n      }]",
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
