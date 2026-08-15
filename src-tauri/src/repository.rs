//! Source-repository catalog documents.

use crate::locator::Locator;
use crate::manifest::{validate_source_id, validate_text};
use schemars::{generate::SchemaSettings, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub const REPOSITORY_MANIFEST_FILE: &str = "skill-manager-repository.json";
pub const REPOSITORY_MANIFEST_VERSION: u8 = 1;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_LISTED_SOURCES: usize = 200;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RepositoryManifest {
    #[schemars(with = "i64", range(min = 1, max = 1))]
    pub version: u8,
    pub repository: RepositoryIdentity,
    #[schemars(length(min = 1, max = 200))]
    pub sources: Vec<ListedSource>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RepositoryIdentity {
    #[schemars(
        length(min = 2, max = 32),
        regex(pattern = r"^[a-z](?:[a-z0-9]|-(?=[a-z0-9])){1,31}$")
    )]
    pub id: String,
    #[schemars(length(min = 1, max = 120))]
    pub name: String,
    #[schemars(length(min = 1, max = 1024))]
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ListedSource {
    #[schemars(length(min = 1, max = 120))]
    pub name: String,
    #[schemars(length(min = 1, max = 1024))]
    pub description: String,
    pub locator: Locator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 2, max = 16))]
    pub source_id: Option<String>,
}

impl RepositoryManifest {
    pub fn from_slice(contents: &[u8]) -> Result<Self, String> {
        if contents.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "{REPOSITORY_MANIFEST_FILE} is larger than the 1 MB limit."
            ));
        }
        let value = serde_json::from_slice::<serde_json::Value>(contents)
            .map_err(|error| format!("Could not parse {REPOSITORY_MANIFEST_FILE}: {error}"))?;
        let version = value.get("version").and_then(serde_json::Value::as_u64);
        let manifest = match version {
            Some(1) => serde_json::from_value::<Self>(value)
                .map_err(|error| format!("Could not parse {REPOSITORY_MANIFEST_FILE}: {error}"))?,
            Some(version) => {
                return Err(format!(
                    "{REPOSITORY_MANIFEST_FILE} uses unsupported version {version}."
                ));
            }
            None => {
                return Err(format!("{REPOSITORY_MANIFEST_FILE} has no valid version."));
            }
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn from_path(path: &Path) -> Result<Self, String> {
        let file = if path.is_dir() {
            path.join(REPOSITORY_MANIFEST_FILE)
        } else {
            path.to_path_buf()
        };
        let contents = std::fs::read(&file)
            .map_err(|error| format!("Could not read {}: {error}", file.display()))?;
        Self::from_slice(&contents)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != REPOSITORY_MANIFEST_VERSION {
            return Err(format!(
                "{REPOSITORY_MANIFEST_FILE} uses unsupported version {}.",
                self.version
            ));
        }
        validate_repository_id(&self.repository.id)?;
        validate_text(&self.repository.name, "repository.name", 1, 120)?;
        validate_text(
            &self.repository.description,
            "repository.description",
            1,
            1024,
        )?;
        if self.sources.is_empty() {
            return Err(format!(
                "{REPOSITORY_MANIFEST_FILE} does not list any sources."
            ));
        }
        if self.sources.len() > MAX_LISTED_SOURCES {
            return Err(format!(
                "{REPOSITORY_MANIFEST_FILE} lists more than {MAX_LISTED_SOURCES} sources."
            ));
        }
        let mut locators = BTreeSet::new();
        for (index, source) in self.sources.iter().enumerate() {
            validate_text(&source.name, "sources[].name", 1, 120)?;
            validate_text(&source.description, "sources[].description", 1, 1024)?;
            let locator =
                Locator::parse(source.locator.kind(), source.locator.url()).map_err(|error| {
                    format!(
                        "Listed source {} has an invalid locator: {error}",
                        index + 1
                    )
                })?;
            if !locators.insert((locator.kind(), locator.identity_key().to_string())) {
                return Err(
                    "Source repository listings contain a duplicate locator after canonicalization."
                        .to_string(),
                );
            }
            if let Some(source_id) = &source.source_id {
                validate_source_id(source_id).map_err(|error| {
                    format!(
                        "Listed source {} has an invalid sourceId: {error}",
                        index + 1
                    )
                })?;
            }
        }
        Ok(())
    }

    pub fn canonical_sources(&self) -> Result<Vec<ListedSource>, String> {
        self.sources
            .iter()
            .map(|source| {
                Ok(ListedSource {
                    name: source.name.clone(),
                    description: source.description.clone(),
                    locator: Locator::parse(source.locator.kind(), source.locator.url())?,
                    source_id: source.source_id.clone(),
                })
            })
            .collect()
    }
}

fn validate_repository_id(value: &str) -> Result<(), String> {
    let valid = (2..=32).contains(&value.len())
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
            "Invalid repository.id {value:?}; use 2-32 lowercase ASCII letters, digits, and single hyphens, beginning with a letter."
        ))
    }
}

pub fn source_repository_schema_json() -> Result<String, String> {
    let generator = SchemaSettings::draft2020_12().into_generator();
    let schema = generator.into_root_schema_for::<RepositoryManifest>();
    let mut schema = serde_json::to_value(schema)
        .map_err(|error| format!("Could not prepare the source-repository schema: {error}"))?;
    if let Some(version) = schema.pointer_mut("/properties/version") {
        version
            .as_object_mut()
            .expect("the generated version schema is an object")
            .remove("format");
    }
    let mut output = serde_json::to_string_pretty(&schema)
        .map_err(|error| format!("Could not serialize the source-repository schema: {error}"))?;
    output.push('\n');
    Ok(output)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryValidationError {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryValidationReport {
    pub repository_id: String,
    pub listed_sources: usize,
    pub errors: Vec<RepositoryValidationError>,
}

pub fn validate_source_repository(input: &str) -> Result<RepositoryValidationReport, String> {
    report_manifest(&RepositoryManifest::from_path(Path::new(input))?)
}

pub(crate) fn report_manifest(
    manifest: &RepositoryManifest,
) -> Result<RepositoryValidationReport, String> {
    Ok(RepositoryValidationReport {
        repository_id: manifest.repository.id.clone(),
        listed_sources: manifest.sources.len(),
        errors: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{
      "version": 1,
      "repository": {
        "id": "acme",
        "name": "Acme sources",
        "description": "Official portable sources."
      },
      "sources": [
        {
          "name": "Review workflows",
          "description": "Review skill and database MCP server.",
          "locator": {
            "kind": "git",
            "url": "https://github.com/acme/review-source.git"
          }
        },
        {
          "name": "Data tools",
          "description": "Published from Nexus as a zip.",
          "locator": {
            "kind": "artifact",
            "url": "https://nexus.example.com/repository/raw/sources/data-latest.zip"
          }
        }
      ]
    }"#;

    #[test]
    fn valid_catalog_is_accepted() {
        let manifest = RepositoryManifest::from_slice(EXAMPLE.as_bytes()).expect("manifest");
        assert_eq!(manifest.repository.id, "acme");
        assert_eq!(manifest.canonical_sources().expect("canonical").len(), 2);
    }

    #[test]
    fn duplicate_locators_are_rejected() {
        let duplicate = r#"{
          "version": 1,
          "repository": {"id":"acme","name":"Acme","description":"Sources"},
          "sources": [
            {"name":"One","description":"One","locator":{"kind":"git","url":"https://github.com/acme/one.git"}},
            {"name":"Two","description":"Two","locator":{"kind":"git","url":"https://github.com/acme/one"}}
          ]
        }"#;
        assert!(RepositoryManifest::from_slice(duplicate.as_bytes())
            .expect_err("duplicate")
            .contains("duplicate locator"));
    }

    #[test]
    fn unknown_fields_and_bad_ids_are_rejected() {
        let unknown = EXAMPLE.replace("\"version\": 1,", "\"version\": 1, \"extra\": true,");
        assert!(RepositoryManifest::from_slice(unknown.as_bytes())
            .expect_err("unknown")
            .contains("unknown field"));
        let bad_id = EXAMPLE.replace("\"acme\"", "\"Acme\"");
        assert!(RepositoryManifest::from_slice(bad_id.as_bytes())
            .expect_err("id")
            .contains("repository.id"));
    }

    #[test]
    fn schema_matches_the_published_golden_file() {
        let generated = source_repository_schema_json().expect("generated schema");
        let published = include_str!("../../schemas/v1/source-repository.schema.json");
        let generated =
            serde_json::from_str::<serde_json::Value>(&generated).expect("generated schema JSON");
        let published =
            serde_json::from_str::<serde_json::Value>(published).expect("published schema JSON");
        assert_eq!(
            generated, published,
            "regenerate the source-repository schema"
        );
    }
}
