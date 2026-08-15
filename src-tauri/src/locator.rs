//! HTTPS artifact locators for sources and source repositories.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Company catalog JSON URL. Leave empty until the Nexus catalog is published.
pub(crate) const DEFAULT_CATALOG_URL: &str = "";

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Locator {
    pub url: String,
}

impl Locator {
    pub(crate) fn parse(url: &str) -> Result<Self, String> {
        Ok(Self {
            url: canonicalize_artifact_url(url)?,
        })
    }

    pub(crate) fn display_url(url: String) -> Self {
        Self { url }
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn source_key(&self) -> String {
        prefixed_key("source-", format!("artifact:{}", self.url).as_bytes())
    }

    pub(crate) fn repository_key(&self) -> String {
        prefixed_key("repo-", format!("artifact\0{}", self.url).as_bytes())
    }

    pub(crate) fn same_identity(&self, other: &Self) -> bool {
        self.url == other.url
    }
}

pub(crate) fn default_catalog_locator() -> Result<Option<Locator>, String> {
    let url = DEFAULT_CATALOG_URL.trim();
    if url.is_empty() {
        return Ok(None);
    }
    Ok(Some(Locator::parse(url)?))
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
    let (host, port) = canonical_host_and_port(authority)?;
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

fn canonical_host_and_port(host_port: &str) -> Result<(String, Option<&str>), String> {
    let (host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let closing = bracketed
            .find(']')
            .ok_or_else(|| artifact_url_error("The URL has an invalid IPv6 host."))?;
        let host_end = closing + 1;
        let host = &host_port[..=host_end];
        let suffix = &host_port[host_end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or_else(|| artifact_url_error("The URL has an invalid host."))?,
            )
        };
        (host, port)
    } else {
        if host_port.matches(':').count() > 1 {
            return Err(artifact_url_error(
                "IPv6 hosts must be enclosed in brackets.",
            ));
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
        return Err(artifact_url_error("The URL has an invalid host."));
    }
    let port = match port {
        Some(port) => {
            let parsed = port
                .parse::<u16>()
                .map_err(|_| artifact_url_error("The URL has an invalid port."))?;
            if parsed == 0 {
                return Err(artifact_url_error("The URL has an invalid port."));
            }
            (parsed != 443).then_some(port)
        }
        None => None,
    };
    Ok((host.to_ascii_lowercase(), port))
}

fn artifact_url_error(detail: &str) -> String {
    format!("Invalid artifact URL. {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_source_key_uses_prefixed_canonical_url() {
        let locator = Locator::parse(
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
    }

    #[test]
    fn artifact_urls_reject_credentials_and_http() {
        assert!(
            Locator::parse("http://nexus.example.com/repository/raw/latest.zip")
                .expect_err("http")
                .contains("https://")
        );
        assert!(
            Locator::parse("https://user:token@nexus.example.com/repository/raw/latest.zip")
                .expect_err("userinfo")
                .contains("credentials")
        );
    }

    #[test]
    fn repository_key_is_stable_for_canonical_url() {
        let locator = Locator::parse("https://nexus.example.com/repository/raw/catalogs/acme.json")
            .expect("locator");
        assert!(locator.repository_key().starts_with("repo-"));
        assert_ne!(locator.repository_key(), locator.source_key());
        assert_eq!(
            locator.repository_key(),
            Locator::parse("HTTPS://Nexus.Example.com:443/repository/raw/catalogs/acme.json")
                .expect("canonical")
                .repository_key()
        );
    }

    #[test]
    fn empty_default_catalog_url_is_unset() {
        assert_eq!(default_catalog_locator().expect("default"), None);
    }
}
