//! Strict, locally pinned source manifests for legacy installs and portable packages.

use schemars::{generate::SchemaSettings, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

pub const SOURCE_MANIFEST_FILE: &str = "skill-manager.json";
pub const SOURCE_MANIFEST_VERSION: u8 = 2;
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(untagged)]
pub enum SourceManifest {
    V1(ManifestV1),
    V2(ManifestV2),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManifestV1 {
    #[schemars(with = "i64", range(min = 1, max = 1))]
    pub version: u8,
    pub source: ManifestSource,
    #[schemars(length(min = 1))]
    pub installs: Vec<ManifestInstall>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManifestV2 {
    #[schemars(with = "i64", range(min = 2, max = 2))]
    pub version: u8,
    pub source: ManifestSource,
    #[schemars(length(min = 1))]
    pub packages: Vec<ManifestPackage>,
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

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManifestPackage {
    #[schemars(
        length(min = 1, max = 64),
        regex(pattern = r"^[a-z0-9](?:[a-z0-9]|-(?=[a-z0-9])){0,63}$")
    )]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 120))]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 1024))]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ManifestComponent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts_with: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", tag = "kind")]
pub enum ManifestComponent {
    Skill {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        path: String,
    },
    McpServer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        path: String,
    },
}

impl ManifestComponent {
    pub(crate) fn id(&self) -> Option<&str> {
        match self {
            Self::Skill { id, .. } | Self::McpServer { id, .. } => id.as_deref(),
        }
    }

    pub(crate) fn path(&self) -> &str {
        match self {
            Self::Skill { path, .. } | Self::McpServer { path, .. } => path,
        }
    }
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
        let version = value.get("version").and_then(serde_json::Value::as_u64);
        let manifest = match version {
            Some(1) => serde_json::from_value::<ManifestV1>(value)
                .map(Self::V1)
                .map_err(|error| format!("Could not parse skill-manager.json: {error}"))?,
            Some(2) => serde_json::from_value::<ManifestV2>(value)
                .map(Self::V2)
                .map_err(|error| format!("Could not parse skill-manager.json: {error}"))?,
            Some(version) => {
                return Err(format!(
                    "skill-manager.json uses unsupported version {version}."
                ));
            }
            None => return Err("skill-manager.json has no valid version.".to_string()),
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        let source = self.source();
        validate_source_id(&source.id)?;
        validate_text(&source.name, "source.name", 1, 120)?;
        validate_text(&source.description, "source.description", 1, 1024)?;
        match self {
            Self::V1(manifest) => {
                if manifest.version != 1 || manifest.installs.is_empty() {
                    return Err("skill-manager.json does not publish any installs.".to_string());
                }
                let mut ids = BTreeSet::new();
                for install in &manifest.installs {
                    validate_package_id(&install.id, "install")?;
                    if !ids.insert(&install.id) {
                        return Err(format!("Duplicate install id: {}", install.id));
                    }
                }
            }
            Self::V2(manifest) => {
                if manifest.version != SOURCE_MANIFEST_VERSION || manifest.packages.is_empty() {
                    return Err("skill-manager.json does not publish any packages.".to_string());
                }
                let mut ids = BTreeSet::new();
                for package in &manifest.packages {
                    validate_package(package)?;
                    if !ids.insert(&package.id) {
                        return Err(format!("Duplicate package id: {}", package.id));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn referenced_repository_paths(&self) -> BTreeSet<String> {
        let paths = match self {
            Self::V1(manifest) => manifest
                .installs
                .iter()
                .map(|install| install.source.clone())
                .collect::<BTreeSet<_>>(),
            Self::V2(manifest) => manifest
                .packages
                .iter()
                .flat_map(|package| {
                    package
                        .components
                        .iter()
                        .map(|component| component.path().to_string())
                })
                .collect::<BTreeSet<_>>(),
        };
        paths
            .into_iter()
            .chain(std::iter::once(SOURCE_MANIFEST_FILE.to_string()))
            .collect()
    }

    pub fn source(&self) -> &ManifestSource {
        match self {
            Self::V1(manifest) => &manifest.source,
            Self::V2(manifest) => &manifest.source,
        }
    }

    pub(crate) fn installs(&self) -> &[ManifestInstall] {
        match self {
            Self::V1(manifest) => &manifest.installs,
            Self::V2(_) => &[],
        }
    }

    pub(crate) fn packages(&self) -> &[ManifestPackage] {
        match self {
            Self::V1(_) => &[],
            Self::V2(manifest) => &manifest.packages,
        }
    }
}

fn validate_package(package: &ManifestPackage) -> Result<(), String> {
    validate_package_id(&package.id, "package")?;
    if let Some(name) = &package.name {
        validate_text(name, "package.name", 1, 120)?;
    }
    if let Some(description) = &package.description {
        validate_text(description, "package.description", 1, 1024)?;
    }
    if package.components.is_empty() {
        return Err(format!(
            "Package {} must declare one or more skill or mcpServer components.",
            package.id
        ));
    }
    let mut component_ids = BTreeSet::new();
    for (index, component) in package.components.iter().enumerate() {
        let id = component.id().unwrap_or_else(|| {
            if package.components.len() == 1 {
                &package.id
            } else {
                ""
            }
        });
        if id.is_empty() {
            return Err(format!(
                "Package {} component {} needs an id because the package has several components.",
                package.id,
                index + 1
            ));
        }
        validate_package_id(id, "component")?;
        if !component_ids.insert(id) {
            return Err(format!(
                "Package {} has duplicate component id {id}.",
                package.id
            ));
        }
        if component.path().is_empty() {
            return Err(format!(
                "Package {} has an empty component path.",
                package.id
            ));
        }
    }
    for conflict in &package.conflicts_with {
        let valid = conflict
            .split_once('/')
            .filter(|(_, package_id)| !package_id.contains('/'))
            .is_some_and(|(source_id, package_id)| {
                validate_source_id(source_id).is_ok()
                    && validate_package_id(package_id, "conflicting package").is_ok()
            });
        if !valid {
            return Err(format!(
                "Package {} has invalid conflictsWith id {conflict:?}; use source-id/package-id.",
                package.id
            ));
        }
    }
    Ok(())
}

fn validate_package_id(value: &str, label: &str) -> Result<(), String> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    valid
        .then_some(())
        .ok_or_else(|| format!("Invalid {label} id: {value}"))
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
        assert_eq!(manifest.source().id, "fiqit");
        assert_eq!(manifest.installs().len(), 1);
    }

    #[test]
    fn portable_package_contract_is_accepted() {
        let manifest = SourceManifest::from_slice(
            br#"{
              "version": 2,
              "source": {"id":"acme","name":"Acme","description":"Shared configuration."},
              "packages": [{
                "id":"review",
                "components":[
                  {"kind":"skill","id":"review-skill","path":"skills/review"},
                  {"kind":"mcpServer","id":"review-db","path":"mcp/database.json"}
                ]
              }]
            }"#,
        )
        .expect("valid v2 manifest");
        assert_eq!(manifest.source().id, "acme");
        assert_eq!(manifest.packages().len(), 1);
        assert_eq!(manifest.referenced_repository_paths().len(), 3);
    }

    #[test]
    fn instruction_sets_and_plugin_packages_are_rejected() {
        let instruction = br#"{
          "version": 2,
          "source": {"id":"acme","name":"Acme","description":"Shared configuration."},
          "packages": [{
            "id":"review",
            "components":[
              {"kind":"instructionSet","id":"review-rules","path":"rules/review.md","activation":"always"}
            ]
          }]
        }"#;
        let plugin = br#"{
          "version": 2,
          "source": {"id":"acme","name":"Acme","description":"Shared configuration."},
          "packages": [{
            "id":"data-tools",
            "format":"agent-plugin@1.0.0",
            "path":"plugins/data-tools"
          }]
        }"#;
        assert!(SourceManifest::from_slice(instruction)
            .expect_err("instruction set")
            .contains("unknown variant"));
        assert!(SourceManifest::from_slice(plugin)
            .expect_err("plugin package")
            .contains("unknown field"));
    }

    #[test]
    fn portable_package_conflicts_require_canonical_ids() {
        for conflict in ["missing-slash", "Bad/tools", "acme/", "acme/tools/extra"] {
            let manifest = format!(
                r#"{{
                  "version":2,
                  "source":{{"id":"acme","name":"Acme","description":"Test"}},
                  "packages":[{{
                    "id":"review",
                    "components":[{{"kind":"skill","path":"skills/review"}}],
                    "conflictsWith":["{conflict}"]
                  }}]
                }}"#
            );
            assert!(SourceManifest::from_slice(manifest.as_bytes())
                .expect_err("invalid conflict")
                .contains("invalid conflictsWith"));
        }
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
        let published = include_str!("../../schemas/v2/source-manifest.schema.json");
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
