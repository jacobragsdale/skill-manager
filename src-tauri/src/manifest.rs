//! Versioned, strict source-manifest contract and semantic validation.

use schemars::{generate::SchemaSettings, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path};

pub const SOURCE_MANIFEST_FILE: &str = "skill-manager.json";
pub const SOURCE_MANIFEST_VERSION: u8 = 1;
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_COMMAND_TIMEOUT_SECONDS: u32 = 60 * 60;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SourceManifest {
    #[schemars(range(min = 1, max = 1))]
    pub version: u8,
    pub source: ManifestSource,
    #[serde(default)]
    pub agent_skills: Vec<AgentSkillCollection>,
    #[serde(default)]
    pub items: Vec<ManifestItem>,
    #[serde(default)]
    pub collections: Vec<GenericCollection>,
    #[serde(default)]
    pub actions: Vec<ManifestAction>,
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
pub struct AgentSkillCollection {
    pub include: Vec<String>,
    pub destinations: Vec<DestinationTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<PlatformSelector>,
    #[serde(default)]
    pub hooks: LifecycleHooks,
    #[serde(default)]
    pub actions: Vec<ManifestAction>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManifestItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub files: Vec<FileMapping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<PlatformSelector>,
    #[serde(default)]
    pub hooks: LifecycleHooks,
    #[serde(default)]
    pub actions: Vec<ManifestAction>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenericCollection {
    pub include: Vec<String>,
    pub item: ManifestItem,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FileMapping {
    pub source: String,
    pub destination: DestinationTemplate,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DestinationTemplate {
    pub anchor: DestinationAnchor,
    pub path: String,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DestinationAnchor {
    Home,
    Config,
    Data,
    LocalData,
    Cache,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlatformSelector {
    #[serde(default)]
    pub os: Vec<OperatingSystem>,
    #[serde(default)]
    pub arch: Vec<Architecture>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OperatingSystem {
    Macos,
    Linux,
    Windows,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
pub enum Architecture {
    #[serde(rename = "x86_64")]
    #[schemars(rename = "x86_64")]
    X86_64,
    #[serde(rename = "aarch64")]
    #[schemars(rename = "aarch64")]
    Aarch64,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LifecycleHooks {
    #[serde(default)]
    pub pre_install: Vec<CommandStep>,
    #[serde(default)]
    pub post_install: Vec<CommandStep>,
    #[serde(default)]
    pub pre_update: Vec<CommandStep>,
    #[serde(default)]
    pub post_update: Vec<CommandStep>,
    #[serde(default)]
    pub pre_uninstall: Vec<CommandStep>,
    #[serde(default)]
    pub post_uninstall: Vec<CommandStep>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManifestAction {
    #[schemars(length(min = 1, max = 64))]
    pub id: String,
    #[schemars(length(min = 1, max = 120))]
    pub name: String,
    #[schemars(length(min = 1, max = 1024))]
    pub description: String,
    pub steps: Vec<CommandStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<PlatformSelector>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CommandStep {
    #[schemars(length(min = 1, max = 64))]
    pub id: String,
    pub program: Program,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout_seconds")]
    #[schemars(range(min = 1, max = 3600))]
    pub timeout_seconds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<PlatformSelector>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(untagged)]
pub enum Program {
    Source(SourceProgram),
    System(SystemProgram),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceProgram {
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SystemProgram {
    pub system: String,
}

const fn default_timeout_seconds() -> u32 {
    300
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
        if self.agent_skills.is_empty() && self.items.is_empty() && self.collections.is_empty() {
            return Err("skill-manager.json does not publish any items.".to_string());
        }

        for (index, collection) in self.agent_skills.iter().enumerate() {
            validate_include(
                &collection.include,
                &format!("agentSkills[{index}].include"),
            )?;
            if collection.destinations.is_empty() {
                return Err(format!(
                    "agentSkills[{index}].destinations must contain at least one destination."
                ));
            }
            for destination in &collection.destinations {
                validate_destination_template(
                    &destination.path,
                    &["skill.name", "skill.localName"],
                    "agent skill destination",
                )?;
            }
            validate_hooks(&collection.hooks)?;
            validate_actions(&collection.actions, "agent skill action")?;
        }

        let mut item_ids = BTreeSet::new();
        for item in &self.items {
            validate_item(item, &[], "item")?;
            if !item_ids.insert(item.id.as_str()) {
                return Err(format!("Duplicate item id: {}", item.id));
            }
        }
        for (index, collection) in self.collections.iter().enumerate() {
            validate_include(
                &collection.include,
                &format!("collections[{index}].include"),
            )?;
            validate_item(
                &collection.item,
                &["path", "basename", "stem"],
                "collection item",
            )?;
        }
        validate_actions(&self.actions, "source action")
    }

    pub fn has_executable_behavior(&self) -> bool {
        !self.actions.is_empty()
            || self
                .agent_skills
                .iter()
                .any(|group| group.hooks.has_commands() || !group.actions.is_empty())
            || self
                .items
                .iter()
                .any(|item| item.hooks.has_commands() || !item.actions.is_empty())
            || self.collections.iter().any(|collection| {
                collection.item.hooks.has_commands() || !collection.item.actions.is_empty()
            })
    }
}

impl LifecycleHooks {
    pub fn has_commands(&self) -> bool {
        !self.pre_install.is_empty()
            || !self.post_install.is_empty()
            || !self.pre_update.is_empty()
            || !self.post_update.is_empty()
            || !self.pre_uninstall.is_empty()
            || !self.post_uninstall.is_empty()
    }

    fn phases(&self) -> [&[CommandStep]; 6] {
        [
            &self.pre_install,
            &self.post_install,
            &self.pre_update,
            &self.post_update,
            &self.pre_uninstall,
            &self.post_uninstall,
        ]
    }
}

pub fn source_manifest_schema_json() -> Result<String, String> {
    let generator = SchemaSettings::draft2020_12().into_generator();
    let schema = generator.into_root_schema_for::<SourceManifest>();
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

fn validate_local_id(value: &str, label: &str) -> Result<(), String> {
    let valid = (1..=64).contains(&value.len())
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(format!("Invalid {label} id: {value}"))
    }
}

fn validate_text(value: &str, label: &str, min: usize, max: usize) -> Result<(), String> {
    if (min..=max).contains(&value.chars().count()) {
        Ok(())
    } else {
        Err(format!("{label} must contain {min}-{max} characters."))
    }
}

fn validate_include(patterns: &[String], label: &str) -> Result<(), String> {
    if patterns.is_empty() {
        return Err(format!("{label} must contain at least one pattern."));
    }
    for pattern in patterns {
        validate_repository_template(pattern, &[], label)?;
    }
    Ok(())
}

fn validate_item(item: &ManifestItem, variables: &[&str], label: &str) -> Result<(), String> {
    validate_template_id(&item.id, variables, &format!("{label}.id"))?;
    validate_template_text(&item.name, variables, &format!("{label}.name"), 120)?;
    validate_template_text(
        &item.description,
        variables,
        &format!("{label}.description"),
        1024,
    )?;
    validate_template_id(&item.kind, variables, &format!("{label}.kind"))?;
    if item.files.is_empty() {
        return Err(format!("{label}.files must contain at least one mapping."));
    }
    for mapping in &item.files {
        validate_repository_template(&mapping.source, variables, &format!("{label} source"))?;
        validate_destination_template(
            &mapping.destination.path,
            variables,
            &format!("{label} destination"),
        )?;
    }
    validate_hooks(&item.hooks)?;
    validate_actions(&item.actions, &format!("{label} action"))
}

fn validate_template_id(value: &str, variables: &[&str], label: &str) -> Result<(), String> {
    let expanded = expand_validation_template(value, variables, label)?;
    validate_local_id(&expanded, label)
}

fn validate_template_text(
    value: &str,
    variables: &[&str],
    label: &str,
    max: usize,
) -> Result<(), String> {
    let expanded = expand_validation_template(value, variables, label)?;
    validate_text(&expanded, label, 1, max)
}

fn validate_repository_template(
    value: &str,
    variables: &[&str],
    label: &str,
) -> Result<(), String> {
    let expanded = expand_validation_template(value, variables, label)?;
    validate_relative_path(&expanded, true).map_err(|message| format!("Invalid {label}: {message}"))
}

fn validate_destination_template(
    value: &str,
    variables: &[&str],
    label: &str,
) -> Result<(), String> {
    let expanded = expand_validation_template(value, variables, label)?;
    validate_relative_path(&expanded, false)
        .map_err(|message| format!("Invalid {label}: {message}"))
}

fn expand_validation_template(
    value: &str,
    variables: &[&str],
    label: &str,
) -> Result<String, String> {
    let mut expanded = value.to_string();
    while let Some(start) = expanded.find("${") {
        let rest = &expanded[start + 2..];
        let Some(relative_end) = rest.find('}') else {
            return Err(format!(
                "{label} contains an unterminated template variable."
            ));
        };
        let end = start + 2 + relative_end;
        let variable = &expanded[start + 2..end];
        if !variables.contains(&variable) {
            return Err(format!(
                "{label} contains unsupported template variable ${{{variable}}}."
            ));
        }
        expanded.replace_range(start..=end, "value");
    }
    if expanded.contains(['{', '}']) {
        return Err(format!("{label} contains an invalid template expression."));
    }
    Ok(expanded)
}

fn validate_relative_path(value: &str, allow_globs: bool) -> Result<(), String> {
    if value.is_empty() || value.contains('\\') || Path::new(value).is_absolute() {
        return Err("paths must be non-empty, relative, and use forward slashes".to_string());
    }
    for component in Path::new(value).components() {
        let Component::Normal(part) = component else {
            return Err("paths may not contain . or .. components".to_string());
        };
        let Some(component) = part.to_str() else {
            return Err("paths must be UTF-8".to_string());
        };
        if component.is_empty()
            || (!allow_globs && component.contains(['*', '?', '[', ']']))
            || component.chars().any(char::is_control)
        {
            return Err("paths contain an unsupported component".to_string());
        }
    }
    Ok(())
}

fn validate_hooks(hooks: &LifecycleHooks) -> Result<(), String> {
    for phase in hooks.phases() {
        validate_steps(phase, "hook")?;
    }
    Ok(())
}

fn validate_actions(actions: &[ManifestAction], label: &str) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for action in actions {
        validate_local_id(&action.id, label)?;
        if !ids.insert(action.id.as_str()) {
            return Err(format!("Duplicate {label} id: {}", action.id));
        }
        validate_text(&action.name, &format!("{label}.name"), 1, 120)?;
        validate_text(
            &action.description,
            &format!("{label}.description"),
            1,
            1024,
        )?;
        validate_steps(&action.steps, label)?;
    }
    Ok(())
}

fn validate_steps(steps: &[CommandStep], label: &str) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for step in steps {
        validate_local_id(&step.id, &format!("{label} step"))?;
        if !ids.insert(step.id.as_str()) {
            return Err(format!("Duplicate {label} step id: {}", step.id));
        }
        if !(1..=MAX_COMMAND_TIMEOUT_SECONDS).contains(&step.timeout_seconds) {
            return Err(format!(
                "{}.timeoutSeconds must be between 1 and {}.",
                step.id, MAX_COMMAND_TIMEOUT_SECONDS
            ));
        }
        match &step.program {
            Program::Source(program) => {
                validate_relative_path(&program.source, false)
                    .map_err(|message| format!("Invalid source program: {message}"))?;
            }
            Program::System(program) => {
                if program.system.is_empty()
                    || program.system.contains(['/', '\\'])
                    || program.system.chars().any(char::is_whitespace)
                {
                    return Err(format!(
                        "System program {:?} must be a single PATH executable name.",
                        program.system
                    ));
                }
            }
        }
        if step.args.iter().any(|argument| argument.contains('\0')) {
            return Err(format!("{} contains an argument with a NUL byte.", step.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{
      "version": 1,
      "source": {
        "id": "fiqit",
        "name": "Fiqit agent configuration",
        "description": "Shared skills, rules, prompts, and maintenance actions."
      },
      "agentSkills": [{
        "include": ["skills/*"],
        "destinations": [{ "anchor": "home", "path": ".agents/skills/${skill.name}" }]
      }],
      "items": [{
        "id": "cursor-review-rule",
        "name": "Cursor review rule",
        "description": "Installs shared review instructions.",
        "kind": "cursor-rule",
        "files": [{
          "source": "rules/review.mdc",
          "destination": { "anchor": "home", "path": ".cursor/rules/review.mdc" }
        }],
        "hooks": {
          "postInstall": [{
            "id": "register-rule",
            "program": { "source": "scripts/register-rule.sh" },
            "timeoutSeconds": 120
          }]
        }
      }],
      "collections": [{
        "include": ["prompts/*.prompt.md"],
        "item": {
          "id": "${stem}",
          "name": "${stem}",
          "description": "Installs ${basename}.",
          "kind": "prompt",
          "files": [{
            "source": "${path}",
            "destination": { "anchor": "config", "path": "fiqit/prompts/${basename}" }
          }]
        }
      }],
      "actions": [{
        "id": "doctor",
        "name": "Check source setup",
        "description": "Runs source diagnostics.",
        "steps": [{ "id": "doctor", "program": { "system": "git" }, "args": ["--version"] }]
      }]
    }"#;

    #[test]
    fn example_contract_is_accepted_and_executable() {
        let manifest = SourceManifest::from_slice(EXAMPLE.as_bytes()).expect("valid manifest");
        assert_eq!(manifest.source.id, "fiqit");
        assert!(manifest.has_executable_behavior());
    }

    #[test]
    fn source_ids_and_unknown_fields_are_rejected() {
        let invalid_id = EXAMPLE.replace("\"fiqit\"", "\"Fiqit\"");
        assert!(SourceManifest::from_slice(invalid_id.as_bytes())
            .expect_err("invalid source id")
            .contains("Invalid source.id"));
        let unknown = EXAMPLE.replace("\"version\": 1,", "\"version\": 1, \"extra\": true,");
        assert!(SourceManifest::from_slice(unknown.as_bytes())
            .expect_err("unknown field")
            .contains("unknown field"));
    }

    #[test]
    fn program_objects_are_exclusive_and_timeouts_are_bounded() {
        let ambiguous = EXAMPLE.replace(
            "{ \"system\": \"git\" }",
            "{ \"system\": \"git\", \"source\": \"scripts/doctor.sh\" }",
        );
        assert!(SourceManifest::from_slice(ambiguous.as_bytes()).is_err());
        let too_long = EXAMPLE.replace("\"args\": [\"--version\"]", "\"timeoutSeconds\": 3601");
        assert!(SourceManifest::from_slice(too_long.as_bytes())
            .expect_err("bounded timeout")
            .contains("timeoutSeconds"));
    }

    #[test]
    fn schema_matches_the_published_golden_file() {
        let generated = source_manifest_schema_json().expect("generated schema");
        let published = include_str!("../../schemas/v1/source-manifest.schema.json");
        assert_eq!(
            generated, published,
            "regenerate the v1 source manifest schema"
        );
    }
}
