//! HTTPS artifact download, digest, and safe archive extraction.

use crate::catalog_v1::validate_portable_component;
use crate::locator::{self, sha256_hex};
use crate::sources::{temporary_path, validate_catalog_tree, MAX_SOURCE_BYTES, MAX_SOURCE_FILES};
use flate2::read::GzDecoder;
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderValue, ETAG, LAST_MODIFIED};
use reqwest::redirect::{Action, Attempt, Policy};
use std::fs::{self, File};
use std::io::{self, Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const FETCH_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 5;
const MAX_DOWNLOAD_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ArtifactValidators {
    pub(crate) etag: Option<String>,
    pub(crate) last_modified: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DownloadedBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) digest: String,
    pub(crate) validators: ArtifactValidators,
}

pub(crate) fn validators_match(stored: &ArtifactValidators, remote: &ArtifactValidators) -> bool {
    match (
        &stored.etag,
        &stored.last_modified,
        &remote.etag,
        &remote.last_modified,
    ) {
        (Some(stored_etag), Some(stored_modified), Some(remote_etag), Some(remote_modified)) => {
            stored_etag == remote_etag && stored_modified == remote_modified
        }
        _ => false,
    }
}

pub(crate) fn head_artifact(url: &str) -> Result<ArtifactValidators, String> {
    let response = client()?
        .head(fetch_url(url)?)
        .send()
        .map_err(fetch_error)?;
    let response = require_success(response, "Could not inspect the artifact")?;
    Ok(validators_from_headers(response.headers()))
}

pub(crate) fn download_artifact(url: &str) -> Result<DownloadedBytes, String> {
    let response = client()?.get(fetch_url(url)?).send().map_err(fetch_error)?;
    let response = require_success(response, "Could not download the artifact")?;
    let validators = validators_from_headers(response.headers());
    if let Some(length) = response.content_length() {
        if length > MAX_DOWNLOAD_BYTES {
            return Err("The artifact is larger than the 50 MB download limit.".to_string());
        }
    }
    let mut bytes = Vec::new();
    let mut reader = response;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("Could not download the artifact: {error}"))?;
        if read == 0 {
            break;
        }
        let next = bytes
            .len()
            .checked_add(read)
            .ok_or_else(|| "The artifact is larger than the 50 MB download limit.".to_string())?;
        if next as u64 > MAX_DOWNLOAD_BYTES {
            return Err("The artifact is larger than the 50 MB download limit.".to_string());
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let digest = sha256_hex(&bytes);
    Ok(DownloadedBytes {
        bytes,
        digest,
        validators,
    })
}

pub(crate) fn extract_source_archive(bytes: &[u8], destination: &Path) -> Result<(), String> {
    if looks_like_json(bytes) {
        return Err(
            "This artifact is a JSON document, not a source archive. Add it as a source repository."
                .to_string(),
        );
    }
    fs::create_dir_all(destination)
        .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;
    match detect_archive(bytes)? {
        ArchiveKind::Zip => extract_zip(bytes, destination)?,
        ArchiveKind::Tar => extract_tar(Cursor::new(bytes), destination)?,
        ArchiveKind::TarGz => extract_tar(GzDecoder::new(Cursor::new(bytes)), destination)?,
    }
    unwrap_single_directory(destination)?;
    validate_catalog_tree(destination)
}

pub(crate) fn require_repository_json(bytes: &[u8]) -> Result<(), String> {
    if detect_archive(bytes).is_ok() {
        return Err(
            "This artifact is a source archive, not a source-repository catalog. A source repository artifact must be a JSON document."
                .to_string(),
        );
    }
    if !looks_like_json(bytes) {
        return Err(
            "A source repository artifact must be a JSON document at the HTTPS URL.".to_string(),
        );
    }
    Ok(())
}

fn looks_like_json(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'{' || *byte == b'[')
}

#[derive(Clone, Copy)]
enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
}

fn detect_archive(bytes: &[u8]) -> Result<ArchiveKind, String> {
    if bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
    {
        return Ok(ArchiveKind::Zip);
    }
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(ArchiveKind::TarGz);
    }
    if bytes.len() > 262 && bytes[257..262] == *b"ustar" {
        return Ok(ArchiveKind::Tar);
    }
    Err("The artifact is not a zip, tar, or tar.gz source archive.".to_string())
}

fn extract_zip(bytes: &[u8], destination: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("Could not read the zip artifact: {error}"))?;
    let mut file_count = 0;
    let mut total_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Could not read a zip entry: {error}"))?;
        if entry.is_symlink() {
            return Err("Source archives may not contain symbolic links.".to_string());
        }
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| "Source archives may not contain unsafe paths.".to_string())?;
        let relative = sanitize_archive_path(&enclosed)?;
        let path = destination.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&path)
                .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        account_extracted_file(entry.size(), &mut file_count, &mut total_bytes)?;
        let mut file = File::create(&path)
            .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
        io::copy(&mut entry, &mut file)
            .map_err(|error| format!("Could not extract {}: {error}", path.display()))?;
    }
    Ok(())
}

fn extract_tar<R: Read>(reader: R, destination: &Path) -> Result<(), String> {
    let mut archive = tar::Archive::new(reader);
    let mut file_count = 0;
    let mut total_bytes = 0_u64;
    for entry in archive
        .entries()
        .map_err(|error| format!("Could not read the tar artifact: {error}"))?
    {
        let mut entry = entry.map_err(|error| format!("Could not read a tar entry: {error}"))?;
        let header = entry.header();
        let entry_type = header.entry_type();
        if entry_type.is_symlink()
            || entry_type.is_hard_link()
            || entry_type.is_fifo()
            || entry_type.is_character_special()
            || entry_type.is_block_special()
        {
            return Err("Source archives may not contain links or special files.".to_string());
        }
        let entry_path = entry
            .path()
            .map_err(|error| format!("Could not read a tar path: {error}"))?;
        let relative = sanitize_archive_path(&entry_path)?;
        let path = destination.join(&relative);
        if entry_type.is_dir() {
            fs::create_dir_all(&path)
                .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
            continue;
        }
        if !entry_type.is_file()
            && !entry_type.is_gnu_longname()
            && !entry_type.is_pax_local_extensions()
        {
            return Err(format!(
                "Source archives may not contain special entries: {}",
                relative.display()
            ));
        }
        if !entry_type.is_file() {
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        account_extracted_file(
            header.size().unwrap_or(0),
            &mut file_count,
            &mut total_bytes,
        )?;
        let mut file = File::create(&path)
            .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
        io::copy(&mut entry, &mut file)
            .map_err(|error| format!("Could not extract {}: {error}", path.display()))?;
    }
    Ok(())
}

fn sanitize_archive_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Err("Source archives may not contain absolute paths.".to_string());
    }
    let mut sanitized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => {
                validate_portable_component(name, path)?;
                sanitized.push(name);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("Source archives may not contain parent-directory paths.".to_string());
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err("Source archives may not contain absolute paths.".to_string());
            }
        }
    }
    if sanitized.as_os_str().is_empty() {
        return Err("Source archives may not contain empty paths.".to_string());
    }
    Ok(sanitized)
}

fn account_extracted_file(
    size: u64,
    file_count: &mut usize,
    total_bytes: &mut u64,
) -> Result<(), String> {
    *file_count = file_count
        .checked_add(1)
        .ok_or_else(|| "The source contains too many files.".to_string())?;
    if *file_count > MAX_SOURCE_FILES {
        return Err(format!(
            "The source contains more than {MAX_SOURCE_FILES} files."
        ));
    }
    *total_bytes = total_bytes
        .checked_add(size)
        .ok_or_else(|| "The source is too large.".to_string())?;
    if *total_bytes > MAX_SOURCE_BYTES {
        return Err("The source expands beyond 50 MB.".to_string());
    }
    Ok(())
}

fn unwrap_single_directory(root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    for entry in fs::read_dir(root)
        .map_err(|error| format!("Could not inspect {}: {error}", root.display()))?
    {
        let entry =
            entry.map_err(|error| format!("Could not inspect {}: {error}", root.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        if file_type.is_dir() {
            directories.push(entry.path());
        } else if file_type.is_file() {
            files.push(entry.path());
        } else {
            return Err(format!(
                "Source entry is not a regular file or directory: {}",
                entry.path().display()
            ));
        }
    }
    if files.is_empty() && directories.len() == 1 {
        let only = directories
            .into_iter()
            .next()
            .expect("one top-level directory");
        let staging = temporary_path(root, "unwrap");
        fs::rename(&only, &staging)
            .map_err(|error| format!("Could not unwrap {}: {error}", only.display()))?;
        for entry in fs::read_dir(&staging)
            .map_err(|error| format!("Could not unwrap {}: {error}", staging.display()))?
        {
            let entry = entry
                .map_err(|error| format!("Could not unwrap {}: {error}", staging.display()))?;
            let destination = root.join(entry.file_name());
            fs::rename(entry.path(), &destination).map_err(|error| {
                format!(
                    "Could not unwrap {} to {}: {error}",
                    entry.path().display(),
                    destination.display()
                )
            })?;
        }
        fs::remove_dir(&staging)
            .map_err(|error| format!("Could not unwrap {}: {error}", staging.display()))?;
    }
    Ok(())
}

fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(Policy::custom(redirect_policy))
        .build()
        .map_err(|error| format!("Could not create the HTTPS client: {error}"))
}

fn redirect_policy(attempt: Attempt<'_>) -> Action {
    if attempt.previous().len() >= MAX_REDIRECTS {
        return attempt.error("The artifact URL redirected more than 5 times.");
    }
    match allowed_redirect_url(attempt.url().as_str()) {
        Ok(()) => attempt.follow(),
        Err(error) => attempt.error(error),
    }
}

fn allowed_redirect_url(url: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        if is_loopback_http(url) {
            return Ok(());
        }
    }
    locator::canonicalize_artifact_url(url).map(|_| ())
}

fn fetch_url(url: &str) -> Result<String, String> {
    #[cfg(test)]
    {
        if is_loopback_http(url) {
            return Ok(url.to_string());
        }
    }
    locator::canonicalize_artifact_url(url)
}

#[cfg(test)]
fn is_loopback_http(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|parsed| {
        parsed.scheme() == "http"
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed
                .host_str()
                .is_some_and(|host| host == "127.0.0.1" || host == "localhost" || host == "[::1]")
    })
}

fn require_success(response: Response, operation: &str) -> Result<Response, String> {
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else {
        Err(format!("{operation}: HTTP {status}."))
    }
}

fn validators_from_headers(headers: &HeaderMap) -> ArtifactValidators {
    ArtifactValidators {
        etag: header_text(headers.get(ETAG)),
        last_modified: header_text(headers.get(LAST_MODIFIED)),
    }
}

fn header_text(value: Option<&HeaderValue>) -> Option<String> {
    value
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn fetch_error(error: reqwest::Error) -> String {
    format!("Could not fetch the artifact: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn source_tree() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("tree");
        let skill = root.path().join("skills/review");
        fs::create_dir_all(&skill).expect("skill");
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: review\ndescription: Reviews code\n---\nBody\n",
        )
        .expect("skill");
        fs::write(
            root.path().join("skill-manager.json"),
            r#"{
              "version": 2,
              "source": { "id": "acme", "name": "Acme", "description": "Test source" },
              "packages": [{
                "id": "review",
                "components": [{"kind": "skill", "path": "skills/review"}]
              }]
            }"#,
        )
        .expect("manifest");
        (root, skill)
    }

    fn zip_bytes(root: &Path, prefix: Option<&str>) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            add_zip_tree(&mut zip, root, root, prefix, options);
            zip.finish().expect("zip");
        }
        cursor.into_inner()
    }

    fn add_zip_tree(
        zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>,
        root: &Path,
        directory: &Path,
        prefix: Option<&str>,
        options: SimpleFileOptions,
    ) {
        let mut entries = fs::read_dir(directory)
            .expect("read")
            .collect::<Result<Vec<_>, _>>()
            .expect("entries");
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("relative");
            let name = match prefix {
                Some(prefix) => Path::new(prefix).join(relative),
                None => relative.to_path_buf(),
            };
            let name = name.to_str().expect("utf-8").replace('\\', "/");
            if path.is_dir() {
                zip.add_directory(format!("{name}/"), options).expect("dir");
                add_zip_tree(zip, root, &path, prefix, options);
            } else {
                zip.start_file(name, options).expect("file");
                zip.write_all(&fs::read(&path).expect("read file"))
                    .expect("write");
            }
        }
    }

    fn zip_with_name(name: &str) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            zip.start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .expect("start");
            zip.write_all(b"nope").expect("write");
            zip.finish().expect("finish");
        }
        cursor.into_inner()
    }

    #[test]
    fn zip_slip_is_rejected() {
        let destination = tempfile::tempdir().expect("dest");
        let error = extract_source_archive(&zip_with_name("../evil.txt"), destination.path())
            .expect_err("zip-slip");
        assert!(
            error.contains("unsafe") || error.contains("parent-directory"),
            "{error}"
        );
        assert!(!destination.path().join("evil.txt").exists());
    }

    #[test]
    fn zip_parent_paths_are_rejected() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            zip.start_file(
                "ok/../../evil.txt",
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .expect("start");
            zip.write_all(b"nope").expect("write");
            zip.finish().expect("finish");
        }
        let destination = tempfile::tempdir().expect("dest");
        let error =
            extract_source_archive(&cursor.into_inner(), destination.path()).expect_err("parent");
        assert!(
            error.contains("unsafe") || error.contains("parent-directory"),
            "{error}"
        );
        assert!(!destination.path().join("evil.txt").exists());
    }

    #[test]
    fn single_directory_archives_unwrap() {
        let (tree, _) = source_tree();
        let bytes = zip_bytes(tree.path(), Some("repo-main"));
        let destination = tempfile::tempdir().expect("dest");
        extract_source_archive(&bytes, destination.path()).expect("extract");
        assert!(destination.path().join("skill-manager.json").is_file());
        assert!(destination.path().join("skills/review/SKILL.md").is_file());
        assert!(!destination.path().join("repo-main").exists());
    }

    #[test]
    fn json_payload_is_rejected_as_a_source_archive() {
        let destination = tempfile::tempdir().expect("dest");
        assert!(
            extract_source_archive(br#"{"version":1}"#, destination.path())
                .expect_err("json")
                .contains("source repository")
        );
    }

    struct Recorded {
        gets: usize,
        heads: usize,
    }

    fn serve_fixture(
        body: Vec<u8>,
        etag: Option<&'static str>,
        last_modified: Option<&'static str>,
    ) -> (String, Arc<Mutex<Recorded>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        let recorded = Arc::new(Mutex::new(Recorded { gets: 0, heads: 0 }));
        let counts = Arc::clone(&recorded);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(false).expect("blocking");
            for _ in 0..8 {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let mut reader = BufReader::new(stream);
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok() {
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    line.clear();
                }
                let method = request.split_whitespace().next().unwrap_or("");
                {
                    let mut counts = counts.lock().expect("lock");
                    if method == "HEAD" {
                        counts.heads += 1;
                    } else if method == "GET" {
                        counts.gets += 1;
                    }
                }
                let mut stream = reader.into_inner();
                let mut headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n",
                    body.len()
                );
                if let Some(etag) = etag {
                    headers.push_str(&format!("ETag: {etag}\r\n"));
                }
                if let Some(last_modified) = last_modified {
                    headers.push_str(&format!("Last-Modified: {last_modified}\r\n"));
                }
                headers.push_str("\r\n");
                let _ = stream.write_all(headers.as_bytes());
                if method == "GET" {
                    let _ = stream.write_all(&body);
                }
                let _ = stream.flush();
            }
        });
        (
            format!("http://127.0.0.1:{}/source.zip", address.port()),
            recorded,
            handle,
        )
    }

    #[test]
    fn digest_changes_when_payload_changes() {
        let (tree, _) = source_tree();
        let first = zip_bytes(tree.path(), None);
        fs::write(tree.path().join("extra.txt"), "changed").expect("change");
        let second = zip_bytes(tree.path(), None);
        let (url, _, _) = serve_fixture(first.clone(), None, None);
        let downloaded = download_artifact(&url).expect("download");
        assert_eq!(downloaded.digest, sha256_hex(&first));
        assert_ne!(downloaded.digest, sha256_hex(&second));
    }

    #[test]
    fn etag_and_last_modified_short_circuit() {
        let (tree, _) = source_tree();
        let body = zip_bytes(tree.path(), None);
        let (url, counts, _) =
            serve_fixture(body, Some("\"abc\""), Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        let first = download_artifact(&url).expect("get");
        let head = head_artifact(&url).expect("head");
        assert!(validators_match(&first.validators, &head));
        let recorded = counts.lock().expect("lock");
        assert_eq!(recorded.gets, 1);
        assert_eq!(recorded.heads, 1);
    }
}
