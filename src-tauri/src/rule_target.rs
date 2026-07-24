use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MARKER_VERSION: u8 = 1;
const TARGET: &str = "codex";
const SCOPE: &str = "user";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RuleStatus {
    Available,
    Installed,
    UpdateAvailable,
    Removed,
    Modified,
    UnmanagedMatch,
    Conflict,
    SourceConflict,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuleTargetState {
    pub(crate) target: String,
    pub(crate) scope: String,
    pub(crate) path: String,
    pub(crate) active: bool,
    pub(crate) reload_required: String,
    pub(crate) message: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct RuleSource {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) commit: String,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnedRule {
    pub(crate) source_id: String,
    pub(crate) source_url: String,
    pub(crate) name: String,
    pub(crate) status: RuleStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RuleMarker {
    version: u8,
    source_id: String,
    source: String,
    source_commit: String,
    name: String,
    content_digest: String,
    installed_digest: String,
    target: String,
    scope: String,
    created_file: bool,
    inserted_prefix: String,
}

fn codex_root(home: &Path) -> PathBuf {
    home.join(".codex")
}

pub(crate) fn rule_install_root(home: &Path) -> PathBuf {
    codex_root(home)
}

fn instructions_path(home: &Path) -> PathBuf {
    codex_root(home).join("AGENTS.md")
}

fn override_path(home: &Path) -> PathBuf {
    codex_root(home).join("AGENTS.override.md")
}

fn marker_root(home: &Path) -> PathBuf {
    codex_root(home).join(".skill-manager").join("rules")
}

fn marker_path(home: &Path, source_id: &str, name: &str) -> PathBuf {
    marker_root(home).join(format!("{source_id}--{name}.json"))
}

fn backup_root(home: &Path, name: &str) -> PathBuf {
    codex_root(home)
        .join(".skill-manager-backups")
        .join("rules")
        .join(name)
}

fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn temporary_path(parent: &Path, label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    parent.join(format!(".{label}-{}-{nonce}", std::process::id()))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn valid_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest_text(value: &str) -> String {
    digest_bytes(value.as_bytes())
}

fn digest_bytes(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn start_marker(source_id: &str, name: &str) -> String {
    format!("<!-- skill-manager:rule:{source_id}:{name}:begin -->")
}

fn end_marker(source_id: &str, name: &str) -> String {
    format!("<!-- skill-manager:rule:{source_id}:{name}:end -->")
}

fn normalized_rule_body(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let contents = String::from_utf8(bytes)
        .map_err(|error| format!("{} must be valid UTF-8: {error}", path.display()))?;
    let normalized = contents
        .strip_prefix('\u{feff}')
        .unwrap_or(&contents)
        .replace("\r\n", "\n");
    let frontmatter = normalized
        .strip_prefix("---\n")
        .ok_or_else(|| format!("{} is missing YAML frontmatter.", path.display()))?;
    let (_, body) = frontmatter
        .split_once("\n---\n")
        .ok_or_else(|| format!("{} has unterminated YAML frontmatter.", path.display()))?;
    let body = body.trim_matches('\n');
    if body.trim().is_empty() {
        return Err(format!(
            "{} does not contain rule instructions.",
            path.display()
        ));
    }
    Ok(format!("{body}\n"))
}

fn rendered_block(path: &Path, source_id: &str, name: &str) -> Result<String, String> {
    let body = normalized_rule_body(path)?;
    Ok(format!(
        "{}\n{body}{}\n",
        start_marker(source_id, name),
        end_marker(source_id, name)
    ))
}

fn read_instructions(home: &Path) -> Result<Option<String>, String> {
    let path = instructions_path(home);
    match fs::read(&path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|error| format!("{} must be valid UTF-8: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

fn valid_marker(marker: &RuleMarker) -> bool {
    marker.version == MARKER_VERSION
        && marker.target == TARGET
        && marker.scope == SCOPE
        && valid_identifier(&marker.source_id)
        && !marker.source.is_empty()
        && valid_commit(&marker.source_commit)
        && valid_identifier(&marker.name)
        && valid_digest(&marker.content_digest)
        && valid_digest(&marker.installed_digest)
        && matches!(marker.inserted_prefix.as_str(), "" | "\n" | "\n\n")
}

fn read_marker_at(path: &Path) -> Result<RuleMarker, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let marker = serde_json::from_slice::<RuleMarker>(&bytes)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
    if !valid_marker(&marker) {
        return Err(format!(
            "{} is not a valid rule ownership marker.",
            path.display()
        ));
    }
    Ok(marker)
}

fn exact_marker(home: &Path, source_id: &str, name: &str) -> Result<Option<RuleMarker>, String> {
    let path = marker_path(home, source_id, name);
    match read_marker_at(&path) {
        Ok(marker) if marker.source_id == source_id && marker.name == name => Ok(Some(marker)),
        Ok(_) => Err(format!("{} identifies a different rule.", path.display())),
        Err(_) if !path.exists() => Ok(None),
        Err(error) => Err(error),
    }
}

fn owned_markers(home: &Path) -> Result<Vec<RuleMarker>, String> {
    let root = marker_root(home);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut markers = Vec::new();
    for entry in fs::read_dir(&root)
        .map_err(|error| format!("Could not read {}: {error}", root.display()))?
    {
        let entry = entry.map_err(|error| format!("Could not read {}: {error}", root.display()))?;
        if entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?
            .is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        {
            if let Ok(marker) = read_marker_at(&entry.path()) {
                markers.push(marker);
            }
        }
    }
    Ok(markers)
}

fn block_range(
    contents: &str,
    source_id: &str,
    name: &str,
) -> Result<Option<(usize, usize)>, String> {
    let start_text = start_marker(source_id, name);
    let end_text = end_marker(source_id, name);
    let Some(start) = contents.find(&start_text) else {
        return Ok(None);
    };
    if contents[start + start_text.len()..].contains(&start_text) {
        return Err(format!(
            "Codex instructions contain duplicate managed blocks for rule:{name}."
        ));
    }
    let after_start = start + start_text.len();
    let relative_end = contents[after_start..].find(&end_text).ok_or_else(|| {
        format!("The managed Codex block for rule:{name} is missing its end marker.")
    })?;
    let mut end = after_start + relative_end + end_text.len();
    if contents.as_bytes().get(end) == Some(&b'\n') {
        end += 1;
    }
    Ok(Some((start, end)))
}

fn installed_block<'a>(
    contents: &'a str,
    source_id: &str,
    name: &str,
) -> Result<Option<&'a str>, String> {
    Ok(block_range(contents, source_id, name)?.map(|(start, end)| &contents[start..end]))
}

fn has_other_named_block(contents: &str, source_id: &str, name: &str) -> bool {
    let expected = start_marker(source_id, name);
    let suffix = format!(":{name}:begin -->");
    contents.lines().any(|line| {
        line.starts_with("<!-- skill-manager:rule:") && line.ends_with(&suffix) && line != expected
    })
}

pub(crate) fn status(
    home: &Path,
    source_id: &str,
    name: &str,
    catalog_path: Option<&Path>,
    catalog_digest: Option<&str>,
) -> RuleStatus {
    match exact_marker(home, source_id, name) {
        Ok(Some(marker)) => {
            let Ok(Some(contents)) = read_instructions(home) else {
                return RuleStatus::Modified;
            };
            let Ok(Some(block)) = installed_block(&contents, source_id, name) else {
                return RuleStatus::Modified;
            };
            if digest_text(block) != marker.installed_digest {
                return RuleStatus::Modified;
            }
            match catalog_digest {
                Some(digest) if digest == marker.content_digest => RuleStatus::Installed,
                Some(_) => RuleStatus::UpdateAvailable,
                None => RuleStatus::Removed,
            }
        }
        Err(_) => RuleStatus::Conflict,
        Ok(None) => {
            if owned_markers(home)
                .is_ok_and(|markers| markers.iter().any(|marker| marker.name == name))
            {
                return RuleStatus::SourceConflict;
            }
            let Ok(Some(contents)) = read_instructions(home) else {
                return if instructions_path(home).exists() {
                    RuleStatus::Conflict
                } else {
                    RuleStatus::Available
                };
            };
            if has_other_named_block(&contents, source_id, name) {
                return RuleStatus::SourceConflict;
            }
            let Ok(Some(block)) = installed_block(&contents, source_id, name) else {
                return RuleStatus::Available;
            };
            match catalog_path.and_then(|path| rendered_block(path, source_id, name).ok()) {
                Some(expected) if expected == block => RuleStatus::UnmanagedMatch,
                _ => RuleStatus::Conflict,
            }
        }
    }
}

fn append_block(existing: Option<&str>, block: &str) -> (String, bool, String) {
    match existing {
        None | Some("") => (block.to_string(), true, String::new()),
        Some(contents) => {
            let separator = if contents.ends_with("\n\n") {
                ""
            } else if contents.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            (
                format!("{contents}{separator}{block}"),
                false,
                separator.to_string(),
            )
        }
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory.", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    let staging = temporary_path(parent, "rule-writing");
    let previous = temporary_path(parent, "rule-previous");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| format!("Could not create {}: {error}", staging.display()))?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Could not write {}: {error}", staging.display()))?;
    drop(file);
    if path.exists() {
        fs::rename(path, &previous).map_err(|error| {
            format!(
                "Could not stage {} for replacement: {error}",
                path.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(&staging, path) {
        if previous.exists() {
            let _ = fs::rename(&previous, path);
        }
        let _ = fs::remove_file(&staging);
        return Err(format!("Could not activate {}: {error}", path.display()));
    }
    if previous.exists() {
        fs::remove_file(&previous).map_err(|error| {
            format!(
                "{} was updated, but its previous staged copy could not be removed: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn write_marker(home: &Path, marker: &RuleMarker) -> Result<(), String> {
    let root = marker_root(home);
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create {}: {error}", root.display()))?;
    let path = marker_path(home, &marker.source_id, &marker.name);
    let mut contents = serde_json::to_string_pretty(marker)
        .map_err(|error| format!("Could not create the rule ownership marker: {error}"))?;
    contents.push('\n');
    write_atomic(&path, &contents)
}

fn backup_instructions(home: &Path, name: &str) -> Result<Option<PathBuf>, String> {
    let target = instructions_path(home);
    if !target.is_file() {
        return Ok(None);
    }
    let root = backup_root(home, name);
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create {}: {error}", root.display()))?;
    for suffix in 0..10_000_u16 {
        let filename = if suffix == 0 {
            format!("{}-AGENTS.md", current_epoch_seconds())
        } else {
            format!("{}-{suffix}-AGENTS.md", current_epoch_seconds())
        };
        let candidate = root.join(filename);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut output) => {
                let contents = fs::read(&target)
                    .map_err(|error| format!("Could not read {}: {error}", target.display()))?;
                output
                    .write_all(&contents)
                    .and_then(|()| output.sync_all())
                    .map_err(|error| format!("Could not write {}: {error}", candidate.display()))?;
                return Ok(Some(candidate));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!("Could not create {}: {error}", candidate.display()));
            }
        }
    }
    Err(format!(
        "Could not choose a backup path in {}.",
        root.display()
    ))
}

fn marker_for(
    source: &RuleSource,
    name: &str,
    content_digest: &str,
    block: &str,
    created_file: bool,
    inserted_prefix: String,
) -> RuleMarker {
    RuleMarker {
        version: MARKER_VERSION,
        source_id: source.id.clone(),
        source: source.url.clone(),
        source_commit: source.commit.clone(),
        name: name.to_string(),
        content_digest: content_digest.to_string(),
        installed_digest: digest_text(block),
        target: TARGET.to_string(),
        scope: SCOPE.to_string(),
        created_file,
        inserted_prefix,
    }
}

pub(crate) fn install(
    home: &Path,
    source: &RuleSource,
    name: &str,
    catalog_path: &Path,
    content_digest: &str,
) -> Result<(), String> {
    let current_status = status(
        home,
        &source.id,
        name,
        Some(catalog_path),
        Some(content_digest),
    );
    let block = rendered_block(catalog_path, &source.id, name)?;
    match current_status {
        RuleStatus::Available => {
            let existing = read_instructions(home)?;
            let (updated, created_file, inserted_prefix) =
                append_block(existing.as_deref(), &block);
            write_atomic(&instructions_path(home), &updated)?;
            if let Err(error) = write_marker(
                home,
                &marker_for(
                    source,
                    name,
                    content_digest,
                    &block,
                    created_file,
                    inserted_prefix,
                ),
            ) {
                if created_file {
                    let _ = fs::remove_file(instructions_path(home));
                } else if let Some(existing) = existing {
                    let _ = write_atomic(&instructions_path(home), &existing);
                }
                return Err(error);
            }
            Ok(())
        }
        RuleStatus::UpdateAvailable => {
            let marker = exact_marker(home, &source.id, name)?
                .ok_or_else(|| format!("rule:{name} has no ownership marker."))?;
            let contents = read_instructions(home)?
                .ok_or_else(|| format!("{} is missing.", instructions_path(home).display()))?;
            let (start, end) = block_range(&contents, &source.id, name)?
                .ok_or_else(|| format!("The managed Codex block for rule:{name} is missing."))?;
            let installed = &contents[start..end];
            if digest_text(installed) != marker.installed_digest {
                return Err(format!(
                    "rule:{name} contains local changes. It was not updated."
                ));
            }
            backup_instructions(home, name)?;
            let previous_contents = contents.clone();
            let mut updated = contents;
            updated.replace_range(start..end, &block);
            write_atomic(&instructions_path(home), &updated)?;
            if let Err(error) = write_marker(
                home,
                &marker_for(
                    source,
                    name,
                    content_digest,
                    &block,
                    marker.created_file,
                    marker.inserted_prefix.clone(),
                ),
            ) {
                let _ = write_atomic(&instructions_path(home), &previous_contents);
                let _ = write_marker(home, &marker);
                return Err(error);
            }
            Ok(())
        }
        RuleStatus::Installed => Err(format!("rule:{name} is already installed.")),
        RuleStatus::UnmanagedMatch => Err(format!(
            "rule:{name} already matches the catalog. Use Manage instead."
        )),
        RuleStatus::Removed
        | RuleStatus::Modified
        | RuleStatus::Conflict
        | RuleStatus::SourceConflict => Err(format!(
            "rule:{name} needs manual attention before it can be installed."
        )),
    }
}

pub(crate) fn adopt(
    home: &Path,
    source: &RuleSource,
    name: &str,
    catalog_path: &Path,
    content_digest: &str,
) -> Result<(), String> {
    if status(
        home,
        &source.id,
        name,
        Some(catalog_path),
        Some(content_digest),
    ) != RuleStatus::UnmanagedMatch
    {
        return Err(format!(
            "rule:{name} does not exactly match the catalog copy."
        ));
    }
    let block = rendered_block(catalog_path, &source.id, name)?;
    write_marker(
        home,
        &marker_for(source, name, content_digest, &block, false, String::new()),
    )
}

pub(crate) fn replace_unmanaged(
    home: &Path,
    source: &RuleSource,
    name: &str,
    catalog_path: &Path,
    content_digest: &str,
) -> Result<PathBuf, String> {
    if status(
        home,
        &source.id,
        name,
        Some(catalog_path),
        Some(content_digest),
    ) != RuleStatus::Conflict
    {
        return Err(format!("rule:{name} is not an unmanaged differing block."));
    }
    let contents = read_instructions(home)?
        .ok_or_else(|| format!("{} is missing.", instructions_path(home).display()))?;
    let (start, end) = block_range(&contents, &source.id, name)?
        .ok_or_else(|| format!("The unmanaged Codex block for rule:{name} is missing."))?;
    let block = rendered_block(catalog_path, &source.id, name)?;
    let backup = backup_instructions(home, name)?
        .ok_or_else(|| "Could not back up the existing Codex instructions.".to_string())?;
    let previous_contents = contents.clone();
    let mut updated = contents;
    updated.replace_range(start..end, &block);
    write_atomic(&instructions_path(home), &updated)?;
    if let Err(error) = write_marker(
        home,
        &marker_for(source, name, content_digest, &block, false, String::new()),
    ) {
        let _ = write_atomic(&instructions_path(home), &previous_contents);
        return Err(error);
    }
    Ok(backup)
}

pub(crate) fn uninstall(home: &Path, source_id: &str, name: &str) -> Result<(), String> {
    let marker = exact_marker(home, source_id, name)?
        .ok_or_else(|| format!("rule:{name} is not managed by Skill Manager."))?;
    let contents = read_instructions(home)?
        .ok_or_else(|| format!("{} is missing.", instructions_path(home).display()))?;
    let (start, end) = block_range(&contents, source_id, name)?
        .ok_or_else(|| format!("The managed Codex block for rule:{name} is missing."))?;
    if digest_text(&contents[start..end]) != marker.installed_digest {
        return Err(format!(
            "rule:{name} contains local changes. It was not removed."
        ));
    }
    let removal_start = start
        .checked_sub(marker.inserted_prefix.len())
        .filter(|prefix_start| contents[*prefix_start..start] == marker.inserted_prefix)
        .unwrap_or(start);
    let previous_contents = contents.clone();
    let mut updated = contents;
    updated.replace_range(removal_start..end, "");
    let target = instructions_path(home);
    if marker.created_file && updated.trim().is_empty() {
        fs::remove_file(&target)
            .map_err(|error| format!("Could not remove {}: {error}", target.display()))?;
    } else {
        write_atomic(&target, &updated)?;
    }
    if let Err(error) = fs::remove_file(marker_path(home, source_id, name)) {
        let _ = write_atomic(&target, &previous_contents);
        return Err(format!(
            "The rule could not be removed because its ownership marker could not be deleted: {error}"
        ));
    }
    Ok(())
}

pub(crate) fn owned_rules(home: &Path) -> Result<Vec<OwnedRule>, String> {
    owned_markers(home).map(|markers| {
        markers
            .into_iter()
            .map(|marker| OwnedRule {
                status: status(home, &marker.source_id, &marker.name, None, None),
                source_id: marker.source_id,
                source_url: marker.source,
                name: marker.name,
            })
            .collect()
    })
}

pub(crate) fn target_state(home: &Path) -> RuleTargetState {
    let override_file = override_path(home);
    let override_active =
        fs::read_to_string(&override_file).is_ok_and(|contents| !contents.trim().is_empty());
    RuleTargetState {
        target: "Codex".to_string(),
        scope: "User-wide".to_string(),
        path: instructions_path(home).display().to_string(),
        active: !override_active,
        reload_required: "Start a new Codex run or session after rule changes.".to_string(),
        message: override_active.then(|| {
            format!(
                "{} is non-empty, so Codex loads it instead of the managed AGENTS.md file.",
                override_file.display()
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_rule(path: &Path, body: &str) {
        fs::write(
            path,
            format!("---\nname: python\ndescription: Python rules\n---\n\n{body}\n"),
        )
        .expect("rule");
    }

    fn source() -> RuleSource {
        RuleSource {
            id: "skillbook".to_string(),
            url: "https://example.com/skillbook".to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
        }
    }

    #[test]
    fn preserves_unrelated_instructions_through_install_update_and_uninstall() {
        let home = tempfile::tempdir().expect("home");
        let catalog = tempfile::tempdir().expect("catalog");
        let rule = catalog.path().join("python.md");
        write_rule(&rule, "# Python\n\nUse strict typing.");
        fs::create_dir_all(codex_root(home.path())).expect("Codex root");
        let original = "# Personal\n\nKeep this exactly.\n";
        fs::write(instructions_path(home.path()), original).expect("instructions");
        let digest = digest_bytes(&fs::read(&rule).expect("rule bytes"));

        install(home.path(), &source(), "python", &rule, &digest).expect("install");
        assert_eq!(
            status(
                home.path(),
                "skillbook",
                "python",
                Some(&rule),
                Some(&digest)
            ),
            RuleStatus::Installed
        );

        write_rule(&rule, "# Python\n\nUse strict typing and validation.");
        let updated_digest = digest_bytes(&fs::read(&rule).expect("rule bytes"));
        install(home.path(), &source(), "python", &rule, &updated_digest).expect("update");
        uninstall(home.path(), "skillbook", "python").expect("uninstall");
        assert_eq!(
            fs::read_to_string(instructions_path(home.path())).expect("instructions"),
            original
        );
    }

    #[test]
    fn protects_locally_modified_managed_blocks() {
        let home = tempfile::tempdir().expect("home");
        let catalog = tempfile::tempdir().expect("catalog");
        let rule = catalog.path().join("python.md");
        write_rule(&rule, "# Python\n\nUse strict typing.");
        let digest = digest_bytes(&fs::read(&rule).expect("rule bytes"));
        install(home.path(), &source(), "python", &rule, &digest).expect("install");
        let path = instructions_path(home.path());
        let modified = fs::read_to_string(&path)
            .expect("instructions")
            .replace("strict typing", "loose typing");
        fs::write(&path, modified).expect("modify");

        assert_eq!(
            status(
                home.path(),
                "skillbook",
                "python",
                Some(&rule),
                Some(&digest)
            ),
            RuleStatus::Modified
        );
        assert!(uninstall(home.path(), "skillbook", "python").is_err());
    }

    #[test]
    fn unmanaged_matching_block_can_be_adopted() {
        let home = tempfile::tempdir().expect("home");
        let catalog = tempfile::tempdir().expect("catalog");
        let rule = catalog.path().join("python.md");
        write_rule(&rule, "# Python\n\nUse strict typing.");
        let block = rendered_block(&rule, "skillbook", "python").expect("rendered block");
        fs::create_dir_all(codex_root(home.path())).expect("Codex root");
        fs::write(instructions_path(home.path()), &block).expect("unmanaged block");
        let digest = digest_bytes(&fs::read(&rule).expect("rule bytes"));

        assert_eq!(
            status(
                home.path(),
                "skillbook",
                "python",
                Some(&rule),
                Some(&digest)
            ),
            RuleStatus::UnmanagedMatch
        );
        adopt(home.path(), &source(), "python", &rule, &digest).expect("adopt");
        assert_eq!(
            status(
                home.path(),
                "skillbook",
                "python",
                Some(&rule),
                Some(&digest)
            ),
            RuleStatus::Installed
        );
    }

    #[test]
    fn differing_unmanaged_block_is_backed_up_before_replacement() {
        let home = tempfile::tempdir().expect("home");
        let catalog = tempfile::tempdir().expect("catalog");
        let rule = catalog.path().join("python.md");
        write_rule(&rule, "# Python\n\nUse strict typing.");
        fs::create_dir_all(codex_root(home.path())).expect("Codex root");
        fs::write(
            instructions_path(home.path()),
            format!(
                "{}\n# Python\n\nUse loose typing.\n{}\n",
                start_marker("skillbook", "python"),
                end_marker("skillbook", "python")
            ),
        )
        .expect("unmanaged block");
        let digest = digest_bytes(&fs::read(&rule).expect("rule bytes"));

        let backup = replace_unmanaged(home.path(), &source(), "python", &rule, &digest)
            .expect("replace unmanaged");
        assert!(backup.is_file());
        assert!(fs::read_to_string(backup)
            .expect("backup")
            .contains("loose typing"));
        assert_eq!(
            status(
                home.path(),
                "skillbook",
                "python",
                Some(&rule),
                Some(&digest)
            ),
            RuleStatus::Installed
        );
    }

    #[test]
    fn failed_marker_write_restores_existing_instructions() {
        let home = tempfile::tempdir().expect("home");
        let catalog = tempfile::tempdir().expect("catalog");
        let rule = catalog.path().join("python.md");
        write_rule(&rule, "# Python\n\nUse strict typing.");
        fs::create_dir_all(codex_root(home.path())).expect("Codex root");
        let original = "# Personal\n\nKeep this.\n";
        fs::write(instructions_path(home.path()), original).expect("instructions");
        fs::create_dir_all(codex_root(home.path()).join(".skill-manager")).expect("marker parent");
        fs::write(marker_root(home.path()), "blocks marker directory")
            .expect("blocking marker file");
        let digest = digest_bytes(&fs::read(&rule).expect("rule bytes"));

        assert!(install(home.path(), &source(), "python", &rule, &digest).is_err());
        assert_eq!(
            fs::read_to_string(instructions_path(home.path())).expect("instructions"),
            original
        );
    }

    #[test]
    fn target_reports_nonempty_global_override() {
        let home = tempfile::tempdir().expect("home");
        fs::create_dir_all(codex_root(home.path())).expect("Codex root");
        fs::write(override_path(home.path()), "# Temporary override\n").expect("override");
        let state = target_state(home.path());
        assert!(!state.active);
        assert!(state.message.is_some());
    }

    #[test]
    fn same_named_unmanaged_block_from_another_source_is_a_source_conflict() {
        let home = tempfile::tempdir().expect("home");
        let catalog = tempfile::tempdir().expect("catalog");
        let rule = catalog.path().join("python.md");
        write_rule(&rule, "# Python\n\nUse strict typing.");
        fs::create_dir_all(codex_root(home.path())).expect("Codex root");
        fs::write(
            instructions_path(home.path()),
            rendered_block(&rule, "source-other", "python").expect("other block"),
        )
        .expect("instructions");
        let digest = digest_bytes(&fs::read(&rule).expect("rule bytes"));

        assert_eq!(
            status(
                home.path(),
                "skillbook",
                "python",
                Some(&rule),
                Some(&digest)
            ),
            RuleStatus::SourceConflict
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires an authenticated Codex CLI and network access"]
    fn live_codex_loads_an_installed_user_rule() {
        use std::os::unix::fs::symlink;
        use std::process::Command;

        let home = tempfile::tempdir().expect("home");
        let catalog = tempfile::tempdir().expect("catalog");
        let rule = catalog.path().join("verification.md");
        fs::write(
            &rule,
            "---\nname: verification\ndescription: Live Codex rule loading verification.\n---\n\n# Verification\n\nThe active verification token is SKILL_MANAGER_RULE_LOADED_7F3A.\n",
        )
        .expect("rule");
        let digest = digest_bytes(&fs::read(&rule).expect("rule bytes"));
        let mut verification_source = source();
        verification_source.id = "live-verification".to_string();
        install(
            home.path(),
            &verification_source,
            "verification",
            &rule,
            &digest,
        )
        .expect("install verification rule");

        let real_home = dirs::home_dir().expect("real home");
        let auth = real_home.join(".codex/auth.json");
        assert!(auth.is_file(), "Codex auth file is unavailable");
        symlink(auth, codex_root(home.path()).join("auth.json")).expect("link Codex auth");
        let output = Command::new("codex")
            .args([
                "exec",
                "--ephemeral",
                "--ignore-user-config",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "What exact verification token do your active global instructions specify? Reply with the token only.",
            ])
            .current_dir(home.path())
            .env("CODEX_HOME", codex_root(home.path()))
            .output()
            .expect("run Codex");
        assert!(
            output.status.success(),
            "Codex failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("SKILL_MANAGER_RULE_LOADED_7F3A"),
            "Codex did not report the installed rule token: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
