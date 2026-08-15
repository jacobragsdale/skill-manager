//! Content digests for skill directories, remembered against directory shape.
//!
//! A digest is the identity Agent Plugins records in an install marker, so the
//! algorithm itself is frozen: the same directory has to keep hashing to the
//! same bytes forever. How often it runs is not frozen. A refresh asks for the
//! digest of an installed skill more than once — to decide whether an automatic
//! update applies, and again to derive the skill's status — and a refresh
//! happens on launch, on window focus, and every fifteen minutes. Each answer
//! costs one open and one full read of every file in the skill, which on
//! Windows is one antivirus scan per file.
//!
//! So a digest is remembered against the *shape* of the directory: the sorted
//! tree of names, sizes, and modification times. Reading a shape costs one
//! directory listing per directory and opens nothing, because Windows returns
//! sizes and timestamps with the listing itself. Two guards keep the memo from
//! ever answering for content it has not seen:
//!
//! * The shape is read again after hashing, and the entry is kept only when it
//!   still matches — so a file rewritten while the hash was in flight is never
//!   remembered.
//! * A directory touched within the last few seconds is neither remembered nor
//!   answered from the memo. Modification times come from a system clock that
//!   is coarser than the timestamps it writes: Windows advances it about every
//!   15 ms by default, and FAT-family volumes round to two seconds. Inside that
//!   window a file can be rewritten to the same length without its recorded
//!   time moving, so recently touched directories are always re-read.

use crate::catalog_v1::relative_path;
use sha2::{Digest as _, Sha256};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::{self, File};
use std::hash::{Hash as _, Hasher as _};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a directory has to have been still before its digest is trusted
/// from the memo. Comfortably longer than the coarsest timestamp granularity
/// Agent Plugins can meet, and far shorter than the interval between refreshes.
const SETTLE_PERIOD: Duration = Duration::from_secs(3);
/// Large enough for every catalog and installed skill on a real machine. When
/// it is reached the memo is emptied rather than evicted one entry at a time:
/// the next refresh simply repopulates what it still needs.
const MAX_REMEMBERED_DIRECTORIES: usize = 4_096;
const READ_BUFFER_BYTES: usize = 64 * 1024;

struct RememberedDigest {
    shape: u64,
    digest: String,
}

fn memo() -> &'static Mutex<HashMap<PathBuf, RememberedDigest>> {
    static MEMO: OnceLock<Mutex<HashMap<PathBuf, RememberedDigest>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The SHA-256 identity of a directory: every path, every file length, and
/// every byte of content.
pub(crate) fn directory_digest(root: &Path) -> Result<String, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }

    let shape = settled_shape(root);
    if let Some(shape) = shape {
        if let Some(digest) = remembered(root, shape) {
            return Ok(digest);
        }
    }

    let digest = hash_directory(root)?;
    if let Some(shape) = shape {
        if settled_shape(root) == Some(shape) {
            remember(root, shape, &digest);
        }
    }
    Ok(digest)
}

fn remembered(root: &Path, shape: u64) -> Option<String> {
    let memo = memo().lock().unwrap_or_else(|error| error.into_inner());
    memo.get(root)
        .filter(|remembered| remembered.shape == shape)
        .map(|remembered| remembered.digest.clone())
}

fn remember(root: &Path, shape: u64, digest: &str) {
    let mut memo = memo().lock().unwrap_or_else(|error| error.into_inner());
    if memo.len() >= MAX_REMEMBERED_DIRECTORIES && !memo.contains_key(root) {
        memo.clear();
    }
    memo.insert(
        root.to_path_buf(),
        RememberedDigest {
            shape,
            digest: digest.to_string(),
        },
    );
}

/// A fingerprint of the directory tree, or `None` when the tree cannot be
/// listed or was touched too recently to fingerprint safely. `None` sends the
/// caller straight to a full hash, which surfaces any underlying error.
fn settled_shape(root: &Path) -> Option<u64> {
    let mut hasher = DefaultHasher::new();
    let mut newest = UNIX_EPOCH;
    visit_shape(root, &mut hasher, &mut newest)?;
    if SystemTime::now().duration_since(newest).ok()? < SETTLE_PERIOD {
        return None;
    }
    Some(hasher.finish())
}

fn visit_shape(current: &Path, hasher: &mut DefaultHasher, newest: &mut SystemTime) -> Option<()> {
    let mut entries = fs::read_dir(current)
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let file_type = entry.file_type().ok()?;
        entry.file_name().as_encoded_bytes().hash(hasher);
        if file_type.is_dir() {
            b'd'.hash(hasher);
            visit_shape(&entry.path(), hasher, newest)?;
            // Closing marker, so that nesting cannot be confused with a sibling.
            b'.'.hash(hasher);
        } else if file_type.is_file() {
            b'f'.hash(hasher);
            let metadata = entry.metadata().ok()?;
            metadata.len().hash(hasher);
            let modified = metadata.modified().ok()?;
            modified
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_nanos()
                .hash(hasher);
            *newest = (*newest).max(modified);
        } else {
            b'?'.hash(hasher);
        }
    }

    Some(())
}

fn update_hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn hash_directory(root: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
    hash_directory_entries(root, root, &mut hasher, &mut buffer)?;
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(encoded)
}

fn hash_directory_entries(
    root: &Path,
    current: &Path,
    hasher: &mut Sha256,
    buffer: &mut [u8],
) -> Result<(), String> {
    let mut entries = fs::read_dir(current)
        .map_err(|error| format!("Could not read {}: {error}", current.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read {}: {error}", current.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
        let relative = relative_path(root, &path)?;

        if file_type.is_dir() {
            hasher.update(b"directory");
            update_hash_field(hasher, relative.as_bytes());
            hash_directory_entries(root, &path, hasher, buffer)?;
        } else if file_type.is_file() {
            hasher.update(b"file");
            update_hash_field(hasher, relative.as_bytes());
            let mut file = File::open(&path)
                .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
            let size = file
                .metadata()
                .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?
                .len();
            hasher.update(size.to_le_bytes());

            loop {
                let read = file
                    .read(buffer)
                    .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
        } else {
            return Err(format!(
                "{} is not a regular file or directory",
                path.display()
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write(path: &Path, contents: &str) {
        fs::write(path, contents).expect("test file");
    }

    /// A fixed instant well in the past, so that backdating a file twice records
    /// the same time rather than two times a few milliseconds apart. `sequence`
    /// picks a distinct instant for each round of edits a test wants noticed.
    fn settled_instant(sequence: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000 + sequence)
    }

    /// Backdates every file in the tree so the directory reads as settled, the
    /// way a skill installed minutes or days ago does.
    fn settle(root: &Path, at: SystemTime) {
        for entry in fs::read_dir(root).expect("listing") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                settle(&path, at);
            } else {
                let file = File::options().write(true).open(&path).expect("open");
                file.set_modified(at).expect("backdate");
            }
        }
    }

    #[test]
    fn remembers_settled_directories_and_notices_every_change() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let skill = temporary.path().join("example");
        fs::create_dir_all(skill.join("nested")).expect("skill directory");
        write(&skill.join("SKILL.md"), "---\nname: example\n---\n");
        write(&skill.join("nested").join("note.md"), "first");
        settle(&skill, settled_instant(0));

        let original = directory_digest(&skill).expect("digest");
        assert_eq!(directory_digest(&skill).expect("memo hit"), original);

        // A just-written file is never answered from the memo, even when the
        // rewrite kept the length.
        write(&skill.join("nested").join("note.md"), "FIRST");
        let rewritten = directory_digest(&skill).expect("digest");
        assert_ne!(rewritten, original);
        settle(&skill, settled_instant(1));
        assert_eq!(directory_digest(&skill).expect("digest"), rewritten);

        // A new file changes the shape and therefore the digest.
        write(&skill.join("extra.md"), "extra");
        settle(&skill, settled_instant(2));
        assert_ne!(directory_digest(&skill).expect("digest"), rewritten);
    }

    /// The memo is only as sharp as the shape it is keyed on: a rewrite that
    /// keeps the length and puts the recorded modification time back is
    /// indistinguishable from no rewrite at all. This pins that boundary
    /// deliberately, and is also proof that a settled directory really is
    /// answered from the memo rather than read again.
    #[test]
    fn answers_a_settled_directory_without_reading_it_again() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let skill = temporary.path().join("example");
        fs::create_dir(&skill).expect("skill directory");
        let contents = skill.join("SKILL.md");
        write(&contents, "---\nname: example\n---\n");
        settle(&skill, settled_instant(0));
        let original = directory_digest(&skill).expect("digest");

        write(&contents, "---\nname: EXAMPLE\n---\n");
        settle(&skill, settled_instant(0));
        assert_eq!(directory_digest(&skill).expect("memo hit"), original);
    }

    #[test]
    fn hashes_content_larger_than_the_read_buffer() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let skill = temporary.path().join("large");
        fs::create_dir(&skill).expect("skill directory");
        let mut file = File::create(skill.join("SKILL.md")).expect("large file");
        for index in 0..(READ_BUFFER_BYTES / 8 + 3) {
            write!(file, "{index:07} ").expect("write chunk");
        }
        drop(file);

        assert_eq!(directory_digest(&skill).expect("digest").len(), 64);
        assert!(directory_digest(&temporary.path().join("missing")).is_err());
    }
}
