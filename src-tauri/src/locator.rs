//! Fetch locators for sources and source repositories.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "camelCase")]
pub enum LocatorKind {
    Git,
    Artifact,
}

impl LocatorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Artifact => "artifact",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Locator {
    Git { url: String },
    Artifact { url: String },
}

impl Locator {
    pub(crate) fn parse(kind: LocatorKind, url: &str) -> Result<Self, String> {
        match kind {
            LocatorKind::Git => Ok(Self::Git {
                url: canonicalize_git_url(url)?,
            }),
            LocatorKind::Artifact => Ok(Self::Artifact {
                url: canonicalize_artifact_url(url)?,
            }),
        }
    }

    pub(crate) fn kind(&self) -> LocatorKind {
        match self {
            Self::Git { .. } => LocatorKind::Git,
            Self::Artifact { .. } => LocatorKind::Artifact,
        }
    }

    pub(crate) fn url(&self) -> &str {
        match self {
            Self::Git { url } | Self::Artifact { url } => url,
        }
    }

    pub(crate) fn identity_key(&self) -> &str {
        match self {
            Self::Git { url } => git_identity_key(url),
            Self::Artifact { url } => url.as_str(),
        }
    }

    pub(crate) fn source_key(&self) -> String {
        match self {
            Self::Git { url } => prefixed_key("source-", git_identity_key(url).as_bytes()),
            Self::Artifact { url } => prefixed_key("source-", format!("artifact:{url}").as_bytes()),
        }
    }

    pub(crate) fn repository_key(&self) -> String {
        let mut material = String::new();
        material.push_str(self.kind().as_str());
        material.push('\0');
        material.push_str(self.identity_key());
        prefixed_key("repo-", material.as_bytes())
    }

    pub(crate) fn same_identity(&self, other: &Self) -> bool {
        self.kind() == other.kind() && self.identity_key() == other.identity_key()
    }
}

pub(crate) fn git_identity_key(url: &str) -> &str {
    url.strip_suffix(".git").unwrap_or(url)
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn prefixed_key(prefix: &str, material: &[u8]) -> String {
    let digest = Sha256::digest(material);
    let mut key = prefix.to_string();
    for byte in &digest[..8] {
        write!(&mut key, "{byte:02x}").expect("writing to a String cannot fail");
    }
    key
}

pub(crate) fn canonicalize_git_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    let (scheme, remainder) = input
        .split_once("://")
        .ok_or_else(|| git_url_error("Use an https:// or ssh:// URL."))?;
    let scheme = if scheme.eq_ignore_ascii_case("https") {
        "https"
    } else if scheme.eq_ignore_ascii_case("ssh") {
        "ssh"
    } else {
        return Err(git_url_error(
            "Only https:// and ssh:// URLs are supported.",
        ));
    };
    if remainder.is_empty()
        || remainder
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || remainder.contains('\\')
    {
        return Err(git_url_error("The URL contains an invalid character."));
    }
    let (authority, path) = remainder
        .split_once('/')
        .ok_or_else(|| git_url_error("The URL must include a repository path."))?;
    let path = path.trim_end_matches('/');
    if authority.is_empty() || path.is_empty() || path.contains(['?', '#']) {
        return Err(git_url_error(
            "The URL must contain a host and repository path without a query or fragment.",
        ));
    }
    let (username, host_port) = match authority.rsplit_once('@') {
        Some((userinfo, host_port)) => {
            if scheme == "https"
                || userinfo.is_empty()
                || host_port.is_empty()
                || userinfo.contains(['@', ':'])
                || userinfo.to_ascii_lowercase().contains("%3a")
            {
                return Err(git_url_error(
                    "Repository URLs may not contain credentials.",
                ));
            }
            (Some(userinfo), host_port)
        }
        None => (None, authority),
    };
    let (host, port) = canonical_host_and_port(host_port, scheme, UrlClass::Git)?;
    let mut canonical = format!("{scheme}://");
    if let Some(username) = username {
        canonical.push_str(username);
        canonical.push('@');
    }
    canonical.push_str(&host);
    if let Some(port) = port {
        canonical.push(':');
        canonical.push_str(port);
    }
    canonical.push('/');
    canonical.push_str(path);
    Ok(canonical)
}

pub(crate) fn canonicalize_artifact_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    let (scheme, remainder) = input
        .split_once("://")
        .ok_or_else(|| artifact_url_error("Use an https:// URL."))?;
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(artifact_url_error(
            "Only https:// URLs are supported. HTTP, including LAN Nexus, is not accepted.",
        ));
    }
    if remainder.is_empty()
        || remainder
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || remainder.contains('\\')
    {
        return Err(artifact_url_error("The URL contains an invalid character."));
    }
    let remainder = remainder
        .split_once('#')
        .map_or(remainder, |(without_fragment, _)| without_fragment);
    let (authority, path_and_query) = remainder
        .split_once('/')
        .ok_or_else(|| artifact_url_error("The URL must include a path."))?;
    if authority.is_empty() || authority.contains('@') {
        return Err(artifact_url_error(
            "Artifact URLs may not contain credentials or an empty host.",
        ));
    }
    if path_and_query.is_empty() {
        return Err(artifact_url_error("The URL must include a path."));
    }
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return Err(artifact_url_error("The URL must include a path."));
    }
    let (host, port) = canonical_host_and_port(authority, "https", UrlClass::Artifact)?;
    let mut canonical = format!("https://{host}");
    if let Some(port) = port {
        canonical.push(':');
        canonical.push_str(port);
    }
    canonical.push('/');
    canonical.push_str(path);
    if let Some(query) = query {
        if query.is_empty() {
            return Err(artifact_url_error("The URL has an empty query string."));
        }
        canonical.push('?');
        canonical.push_str(query);
    }
    Ok(canonical)
}

#[derive(Clone, Copy)]
enum UrlClass {
    Git,
    Artifact,
}

fn canonical_host_and_port<'a>(
    host_port: &'a str,
    scheme: &str,
    class: UrlClass,
) -> Result<(String, Option<&'a str>), String> {
    let error = |detail: &str| match class {
        UrlClass::Git => git_url_error(detail),
        UrlClass::Artifact => artifact_url_error(detail),
    };
    let (host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let closing = bracketed
            .find(']')
            .ok_or_else(|| error("The URL has an invalid IPv6 host."))?;
        let host_end = closing + 1;
        let host = &host_port[..=host_end];
        let suffix = &host_port[host_end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or_else(|| error("The URL has an invalid host."))?,
            )
        };
        (host, port)
    } else {
        if host_port.matches(':').count() > 1 {
            return Err(error("IPv6 hosts must be enclosed in brackets."));
        }
        host_port
            .rsplit_once(':')
            .map_or((host_port, None), |(host, port)| (host, Some(port)))
    };
    if host.is_empty()
        || host == "[]"
        || host
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || host.contains(['/', '@', '%'])
    {
        return Err(error("The URL has an invalid host."));
    }
    let port = match port {
        Some(port) => {
            let parsed = port
                .parse::<u16>()
                .map_err(|_| error("The URL has an invalid port."))?;
            if parsed == 0 {
                return Err(error("The URL has an invalid port."));
            }
            let is_default =
                (scheme == "https" && parsed == 443) || (scheme == "ssh" && parsed == 22);
            (!is_default).then_some(port)
        }
        None => None,
    };
    Ok((host.to_ascii_lowercase(), port))
}

fn git_url_error(detail: &str) -> String {
    format!("Invalid repository URL. {detail}")
}

fn artifact_url_error(detail: &str) -> String {
    format!("Invalid artifact URL. {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_source_key_is_stable_for_skillbook() {
        let locator = Locator::parse(
            LocatorKind::Git,
            "https://github.com/jacobragsdale/skillbook",
        )
        .expect("locator");
        assert_eq!(locator.source_key(), "source-41d130b3115ae73a");
        let with_git = Locator::parse(
            LocatorKind::Git,
            "HTTPS://GitHub.COM:443/jacobragsdale/skillbook.git",
        )
        .expect("locator");
        assert_eq!(with_git.source_key(), locator.source_key());
        assert_eq!(
            with_git.url(),
            "https://github.com/jacobragsdale/skillbook.git"
        );
    }

    #[test]
    fn git_identity_ignores_default_ports_and_dot_git() {
        let one = Locator::parse(LocatorKind::Git, "HTTPS://GitHub.COM:443/acme/example.git")
            .expect("locator");
        let two =
            Locator::parse(LocatorKind::Git, "https://github.com/acme/example").expect("locator");
        assert_eq!(one.source_key(), two.source_key());
        assert_eq!(one.url(), "https://github.com/acme/example.git");
        assert!(one.same_identity(&two));
    }

    #[test]
    fn artifact_source_key_uses_prefixed_canonical_url() {
        let locator = Locator::parse(
            LocatorKind::Artifact,
            "HTTPS://Nexus.Example.com:443/repository/raw/sources/data-latest.zip?download=1#ignored",
        )
        .expect("locator");
        assert_eq!(
            locator.url(),
            "https://nexus.example.com/repository/raw/sources/data-latest.zip?download=1"
        );
        assert_eq!(
            locator.source_key(),
            prefixed_key(
                "source-",
                b"artifact:https://nexus.example.com/repository/raw/sources/data-latest.zip?download=1"
            )
        );
        assert_ne!(
            locator.source_key(),
            Locator::parse(
                LocatorKind::Git,
                "https://nexus.example.com/repository/raw/sources/data-latest.zip"
            )
            .expect("git")
            .source_key()
        );
    }

    #[test]
    fn artifact_urls_reject_credentials_and_http() {
        assert!(Locator::parse(
            LocatorKind::Artifact,
            "http://nexus.example.com/repository/raw/latest.zip"
        )
        .expect_err("http")
        .contains("https://"));
        assert!(Locator::parse(
            LocatorKind::Artifact,
            "https://user:token@nexus.example.com/repository/raw/latest.zip"
        )
        .expect_err("userinfo")
        .contains("credentials"));
    }

    #[test]
    fn repository_key_includes_kind() {
        let git =
            Locator::parse(LocatorKind::Git, "https://github.com/acme/catalog.git").expect("git");
        let artifact = Locator::parse(LocatorKind::Artifact, "https://github.com/acme/catalog.git")
            .expect("artifact");
        assert!(git.repository_key().starts_with("repo-"));
        assert_ne!(git.repository_key(), artifact.repository_key());
        assert_ne!(git.repository_key(), git.source_key());
    }
}
