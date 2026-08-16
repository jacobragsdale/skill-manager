//! Detected agent profiles. Detection is the configuration set.

use crate::fs_retry;
use crate::paths::SystemPaths;
use crate::process;
use crate::sources::{sync_directory, temporary_path};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PROFILES_FILE: &str = "agent-profiles.json";
const PROFILES_BACKUP_FILE: &str = "agent-profiles.json.previous";
const PROFILES_VERSION: u8 = 1;
const DETECTION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TargetId {
    Cursor,
    ClaudeCode,
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
    GrokBuild,
    GithubCopilot,
}

impl TargetId {
    pub(crate) const ALL: [Self; 6] = [
        Self::Cursor,
        Self::ClaudeCode,
        Self::Codex,
        Self::OpenCode,
        Self::GrokBuild,
        Self::GithubCopilot,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::GrokBuild => "grok-build",
            Self::GithubCopilot => "github-copilot",
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::GrokBuild => "Grok Build",
            Self::GithubCopilot => "GitHub Copilot",
        }
    }

    fn command(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::GrokBuild => "grok",
            Self::GithubCopilot => "copilot",
        }
    }

    pub(crate) fn current_dialect(self) -> String {
        format!("{}-2026-08", self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct AgentProfile {
    pub(crate) target_id: TargetId,
    pub(crate) enabled: bool,
    pub(crate) scopes: Vec<String>,
    pub(crate) dialect_id: String,
}

impl AgentProfile {
    fn disabled(target_id: TargetId) -> Self {
        Self {
            target_id,
            enabled: false,
            scopes: vec!["user".to_string()],
            dialect_id: target_id.current_dialect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentProfileState {
    pub(crate) target_id: TargetId,
    pub(crate) display_name: String,
    pub(crate) enabled: bool,
    pub(crate) scopes: Vec<String>,
    pub(crate) dialect_id: String,
    pub(crate) detected: bool,
    pub(crate) detected_version: Option<String>,
    pub(crate) detection_message: Option<String>,
    pub(crate) verification_guidance: String,
    pub(crate) reload_guidance: String,
    pub(crate) skill_directory: String,
    pub(crate) skill_directory_shared: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProfilesFile {
    version: u8,
    profiles: Vec<AgentProfile>,
}

pub(crate) fn read(paths: &SystemPaths) -> Result<Vec<AgentProfile>, String> {
    let configured = read_configured(paths)?;
    Ok(materialize(&configured))
}

pub(crate) fn apply_detected_defaults(paths: &SystemPaths) -> Result<Vec<AgentProfile>, String> {
    let configured = read_configured(paths)?;
    let detected = TargetId::ALL
        .into_iter()
        .filter(|target| detect(*target).detected)
        .collect::<Vec<_>>();
    let (next, changed) = merge_detected_defaults(configured, &detected);
    if changed {
        write(
            paths,
            &next.values().cloned().collect::<Vec<AgentProfile>>(),
        )?;
    }
    Ok(materialize(&next))
}

pub(crate) fn set_enabled(
    paths: &SystemPaths,
    target_id: TargetId,
    enabled: bool,
) -> Result<Vec<AgentProfile>, String> {
    let mut configured = read_configured(paths)?;
    configured
        .entry(target_id)
        .or_insert_with(|| AgentProfile::disabled(target_id))
        .enabled = enabled;
    write(
        paths,
        &configured.values().cloned().collect::<Vec<AgentProfile>>(),
    )?;
    Ok(materialize(&configured))
}

pub(crate) fn states(paths: &SystemPaths) -> Result<Vec<AgentProfileState>, String> {
    read(paths)?
        .into_iter()
        .map(|profile| {
            let detection = detect(profile.target_id);
            Ok(AgentProfileState {
                target_id: profile.target_id,
                display_name: profile.target_id.display_name().to_string(),
                enabled: profile.enabled,
                scopes: profile.scopes,
                dialect_id: profile.dialect_id,
                detected: detection.detected,
                detected_version: detection.version,
                detection_message: detection.message,
                verification_guidance: verification_guidance(profile.target_id).to_string(),
                reload_guidance: reload_guidance(profile.target_id).to_string(),
                skill_directory: crate::adapters::skill_display_root(profile.target_id).to_string(),
                skill_directory_shared: crate::adapters::reads_shared_agents(profile.target_id),
            })
        })
        .collect()
}

fn verification_guidance(target: TargetId) -> &'static str {
    match target {
        TargetId::Cursor => "Inspect the Plugins and MCP settings surfaces.",
        TargetId::ClaudeCode => {
            "Run `claude mcp list` and inspect the effective user instructions."
        }
        TargetId::Codex => "Inspect configured MCP servers and the effective instruction chain.",
        TargetId::OpenCode => "Run `opencode mcp list` and inspect loaded skills and instructions.",
        TargetId::GrokBuild => "Inspect the configured skills and MCP servers in Grok Build.",
        TargetId::GithubCopilot => {
            "Inspect Copilot skills in VS Code, Visual Studio, JetBrains, or Copilot CLI."
        }
    }
}

fn reload_guidance(target: TargetId) -> &'static str {
    match target {
        TargetId::Cursor => "Reload the Cursor window after configuration changes.",
        TargetId::GithubCopilot => {
            "Reload the IDE window or start a fresh Copilot session after configuration changes."
        }
        TargetId::ClaudeCode | TargetId::Codex | TargetId::OpenCode | TargetId::GrokBuild => {
            "Start a fresh client session after configuration changes."
        }
    }
}

struct Detection {
    detected: bool,
    version: Option<String>,
    message: Option<String>,
}

fn detect(target: TargetId) -> Detection {
    if let Some(detection) = detect_application(target) {
        return detection;
    }
    detect_command(target)
}

fn detect_application(target: TargetId) -> Option<Detection> {
    match target {
        TargetId::Cursor => detect_cursor_application(),
        TargetId::GithubCopilot => detect_copilot_application(),
        _ => None,
    }
}

fn detect_cursor_application() -> Option<Detection> {
    cursor_install_roots().into_iter().find_map(|root| {
        let product = cursor_product_json(&root);
        if !product.is_file() && !root.exists() {
            return None;
        }
        Some(Detection {
            detected: true,
            version: read_product_version(&product),
            message: None,
        })
    })
}

fn cursor_install_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(target_os = "macos")]
    {
        roots.push(PathBuf::from("/Applications/Cursor.app"));
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join("Applications/Cursor.app"));
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = dirs::data_local_dir() {
            roots.push(local.join("Programs").join("cursor"));
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            roots.push(PathBuf::from(program_files).join("Cursor"));
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles(x86)") {
            roots.push(PathBuf::from(program_files).join("Cursor"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        roots.push(PathBuf::from("/usr/share/cursor"));
        roots.push(PathBuf::from("/opt/Cursor"));
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".local/share/cursor"));
        }
    }
    roots
}

fn cursor_product_json(root: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        root.join("Contents/Resources/app/product.json")
    }
    #[cfg(not(target_os = "macos"))]
    {
        root.join("resources/app/product.json")
    }
}

fn read_product_version(path: &Path) -> Option<String> {
    let value = serde_json::from_slice::<serde_json::Value>(&fs::read(path).ok()?).ok()?;
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .map(str::to_string)
}

fn detect_copilot_application() -> Option<Detection> {
    detect_vscode_copilot().or_else(detect_jetbrains_copilot)
}

fn detect_vscode_copilot() -> Option<Detection> {
    vscode_extension_roots().into_iter().find_map(|root| {
        first_dir_with_prefix(&root, "github.copilot").map(|dir| Detection {
            detected: true,
            version: read_product_version(&dir.join("package.json")),
            message: None,
        })
    })
}

fn vscode_extension_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".vscode/extensions"),
        home.join(".vscode-insiders/extensions"),
    ]
}

fn detect_jetbrains_copilot() -> Option<Detection> {
    jetbrains_config_roots().into_iter().find_map(|root| {
        let ides = fs::read_dir(root).ok()?;
        ides.flatten().find_map(|ide| {
            let plugins = ide.path().join("plugins");
            first_dir_with_prefix(&plugins, "github-copilot")
                .or_else(|| first_dir_with_prefix(&plugins, "copilot-intellij"))
                .map(|_| Detection {
                    detected: true,
                    version: None,
                    message: None,
                })
        })
    })
}

fn jetbrains_config_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        roots.push(home.join("Library/Application Support/JetBrains"));
        #[cfg(target_os = "windows")]
        if let Some(config) = dirs::config_dir() {
            roots.push(config.join("JetBrains"));
        }
        #[cfg(target_os = "linux")]
        {
            roots.push(home.join(".config/JetBrains"));
            roots.push(home.join(".local/share/JetBrains"));
        }
    }
    roots
}

fn first_dir_with_prefix(parent: &Path, prefix: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(parent).ok()?;
    let prefix = prefix.to_ascii_lowercase();
    entries.flatten().map(|entry| entry.path()).find(|path| {
        path.is_dir()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().starts_with(&prefix))
    })
}

fn is_missing_program_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("no such file")
        || error.contains("not found")
        || error.contains("cannot find the file")
        || error.contains("cannot find the path")
        || error.contains("the system cannot find")
}

fn detect_command(target: TargetId) -> Detection {
    let mut command = process::command(Path::new(target.command()));
    command.arg("--version");
    match process::run(
        command,
        &format!("{} detection", target.display_name()),
        DETECTION_TIMEOUT,
    ) {
        Ok(output) if output.status.success() => {
            let version =
                first_nonempty_line(&output.stdout).or_else(|| first_nonempty_line(&output.stderr));
            Detection {
                detected: true,
                version,
                message: None,
            }
        }
        Ok(_) => Detection {
            detected: false,
            version: None,
            message: None,
        },
        Err(error) => Detection {
            detected: false,
            version: None,
            message: (!is_missing_program_error(&error)).then_some(error),
        },
    }
}

fn read_configured(paths: &SystemPaths) -> Result<BTreeMap<TargetId, AgentProfile>, String> {
    recover(&paths.app_data())?;
    let path = paths.app_data().join(PROFILES_FILE);
    let configured = match fs::read(&path) {
        Ok(contents) => {
            let file = serde_json::from_slice::<ProfilesFile>(&contents)
                .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
            if file.version != PROFILES_VERSION {
                return Err(format!(
                    "{} uses an unsupported profile version.",
                    path.display()
                ));
            }
            file.profiles
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    let mut profiles = BTreeMap::new();
    for profile in configured {
        if profile.scopes != ["user"] || profile.dialect_id.is_empty() {
            return Err(format!(
                "{} contains an invalid profile for {}.",
                path.display(),
                profile.target_id.as_str()
            ));
        }
        if profiles.insert(profile.target_id, profile).is_some() {
            return Err(format!(
                "{} contains a duplicate agent profile.",
                path.display()
            ));
        }
    }
    Ok(profiles)
}

fn materialize(configured: &BTreeMap<TargetId, AgentProfile>) -> Vec<AgentProfile> {
    TargetId::ALL
        .into_iter()
        .map(|target| {
            configured
                .get(&target)
                .cloned()
                .unwrap_or_else(|| AgentProfile::disabled(target))
        })
        .collect()
}

fn merge_detected_defaults(
    mut configured: BTreeMap<TargetId, AgentProfile>,
    detected: &[TargetId],
) -> (BTreeMap<TargetId, AgentProfile>, bool) {
    let mut changed = false;
    for target in TargetId::ALL {
        let should_enable = detected.contains(&target);
        match configured.entry(target) {
            std::collections::btree_map::Entry::Vacant(entry) if should_enable => {
                entry.insert(AgentProfile {
                    target_id: target,
                    enabled: true,
                    scopes: vec!["user".to_string()],
                    dialect_id: target.current_dialect(),
                });
                changed = true;
            }
            std::collections::btree_map::Entry::Occupied(mut entry)
                if entry.get().enabled != should_enable =>
            {
                entry.get_mut().enabled = should_enable;
                changed = true;
            }
            _ => {}
        }
    }
    (configured, changed)
}

fn first_nonempty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn write(paths: &SystemPaths, profiles: &[AgentProfile]) -> Result<(), String> {
    let data_base = paths.app_data();
    fs::create_dir_all(&data_base)
        .map_err(|error| format!("Could not create {}: {error}", data_base.display()))?;
    recover(&data_base)?;
    let file = ProfilesFile {
        version: PROFILES_VERSION,
        profiles: profiles.to_vec(),
    };
    let mut contents = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("Could not serialize agent profiles: {error}"))?;
    contents.push(b'\n');
    atomic_write(
        &data_base,
        &data_base.join(PROFILES_FILE),
        &data_base.join(PROFILES_BACKUP_FILE),
        &contents,
    )
}

fn atomic_write(
    directory: &Path,
    path: &Path,
    backup: &Path,
    contents: &[u8],
) -> Result<(), String> {
    let staging = temporary_path(directory, "agent-profiles-writing");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| format!("Could not create {}: {error}", staging.display()))?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Could not write {}: {error}", staging.display()))?;
    if path.exists() {
        if backup.exists() {
            fs_retry::remove_file(backup)
                .map_err(|error| format!("Could not remove {}: {error}", backup.display()))?;
        }
        fs_retry::rename(path, backup)
            .map_err(|error| format!("Could not stage {}: {error}", path.display()))?;
    }
    if let Err(error) = fs_retry::rename(&staging, path) {
        if backup.exists() {
            let _ = fs_retry::rename(backup, path);
        }
        return Err(format!("Could not activate {}: {error}", path.display()));
    }
    sync_directory(directory)?;
    if backup.exists() {
        fs_retry::remove_file(backup)
            .map_err(|error| format!("Could not remove {}: {error}", backup.display()))?;
    }
    Ok(())
}

fn recover(data_base: &Path) -> Result<(), String> {
    let path = data_base.join(PROFILES_FILE);
    if path.exists() {
        return Ok(());
    }
    let backup = data_base.join(PROFILES_BACKUP_FILE);
    match fs_retry::rename(&backup, &path) {
        Ok(()) => sync_directory(data_base),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not recover {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;

    fn paths(root: &Path) -> SystemPaths {
        SystemPaths {
            home: root.join("home"),
            config: root.join("config"),
            data: root.join("data"),
            local_data: root.join("local-data"),
            cache: root.join("cache"),
        }
    }

    #[test]
    fn profiles_default_disabled_and_persist_explicit_selection() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let initial = read(&paths).expect("initial");
        assert_eq!(initial.len(), TargetId::ALL.len());
        assert!(initial.iter().all(|profile| !profile.enabled));
        set_enabled(&paths, TargetId::Codex, true).expect("enable");
        let reloaded = read(&paths).expect("reloaded");
        assert!(reloaded
            .iter()
            .any(|profile| profile.target_id == TargetId::Codex && profile.enabled));
        assert!(reloaded
            .iter()
            .filter(|profile| profile.target_id != TargetId::Codex)
            .all(|profile| !profile.enabled));
    }

    #[test]
    fn detected_agents_are_configured_and_undetected_agents_are_not() {
        let configured = BTreeMap::from([(
            TargetId::Codex,
            AgentProfile {
                target_id: TargetId::Codex,
                enabled: false,
                scopes: vec!["user".to_string()],
                dialect_id: TargetId::Codex.current_dialect(),
            },
        )]);
        let (next, changed) =
            merge_detected_defaults(configured, &[TargetId::Cursor, TargetId::Codex]);
        assert!(changed);
        assert!(next[&TargetId::Cursor].enabled);
        assert!(next[&TargetId::Codex].enabled);
        let (after_loss, lost) = merge_detected_defaults(next, &[TargetId::Cursor]);
        assert!(lost);
        assert!(after_loss[&TargetId::Cursor].enabled);
        assert!(!after_loss[&TargetId::Codex].enabled);
    }

    #[test]
    fn first_dir_with_prefix_matches_vscode_style_extension_folders() {
        let root = tempfile::tempdir().expect("root");
        fs::create_dir_all(root.path().join("github.copilot-1.372.0")).expect("extension");
        fs::create_dir_all(root.path().join("other.ext-1.0.0")).expect("other");
        assert_eq!(
            first_dir_with_prefix(root.path(), "github.copilot")
                .expect("found")
                .file_name()
                .and_then(|name| name.to_str()),
            Some("github.copilot-1.372.0")
        );
        assert_eq!(first_dir_with_prefix(root.path(), "missing"), None);
    }

    #[test]
    fn product_json_version_is_read_from_the_version_field() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("product.json");
        fs::write(&path, r#"{"nameShort":"Cursor","version":"3.15.6"}"#).expect("product");
        assert_eq!(read_product_version(&path).as_deref(), Some("3.15.6"));
        fs::write(&path, r#"{"nameShort":"Cursor"}"#).expect("product");
        assert_eq!(read_product_version(&path), None);
    }

    #[test]
    fn version_output_uses_the_first_nonempty_line() {
        assert_eq!(
            first_nonempty_line(b"\n  3.15.6\na1f686\narm64\n").as_deref(),
            Some("3.15.6")
        );
        assert_eq!(first_nonempty_line(b"\n\n"), None);
    }

    #[test]
    fn target_ids_serialize_to_the_stable_wire_contract() {
        let serialized = TargetId::ALL
            .into_iter()
            .map(|target| serde_json::to_value(target).expect("serialize target"))
            .collect::<Vec<_>>();
        assert_eq!(
            serialized,
            [
                "cursor",
                "claude-code",
                "codex",
                "opencode",
                "grok-build",
                "github-copilot"
            ]
            .into_iter()
            .map(serde_json::Value::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn windows_missing_program_errors_are_not_surfaced_as_detection_failures() {
        assert!(is_missing_program_error(
            "codex detection: could not start the process: The system cannot find the file specified. (os error 2)"
        ));
        assert!(is_missing_program_error("No such file or directory"));
        assert!(!is_missing_program_error("timed out after 3 seconds"));
    }
}
