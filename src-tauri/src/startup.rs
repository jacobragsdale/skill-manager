//! Host environment preparation that runs once at process start.
//!
//! This is the only place that repairs the login environment so skills and MCP
//! servers work on the user's machine. GUI-launched apps inherit a PATH without
//! Homebrew, `uv`, or Node, and they miss proxy variables set in a shell
//! profile. Corporate TLS interception then breaks `uv` unless it uses the
//! platform certificate store.
//!
//! Add new startup host checks in [`prepare_with`]. Do not scatter PATH or
//! proxy mutations through the rest of the crate.
//!
//! MCP stdio servers are installed as bare commands (`npx`, `uvx`, `node`).
//! Those processes inherit the *agent's* environment, not this process, so the
//! same PATH and proxy values are published to the macOS session via
//! `launchctl setenv` for newly launched GUI agents.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::Duration;

const TOOLS: [&str; 4] = ["uv", "uvx", "node", "npx"];
const LOOPBACK_NO_PROXY: [&str; 3] = ["localhost", "127.0.0.1", "::1"];
const PROXY_PAIRS: [[&str; 2]; 4] = [
    ["HTTP_PROXY", "http_proxy"],
    ["HTTPS_PROXY", "https_proxy"],
    ["ALL_PROXY", "all_proxy"],
    ["NO_PROXY", "no_proxy"],
];
const UV_NATIVE_TLS: &str = "UV_NATIVE_TLS";
const SESSION_KEYS: [&str; 5] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    UV_NATIVE_TLS,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolStatus {
    pub(crate) name: &'static str,
    pub(crate) path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProxyStatus {
    Unset,
    FromEnvironment { http: String, https: String },
    FromSystem { http: String, https: String },
    Socks { url: String },
    PacOnly { url: String },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StartupReport {
    pub(crate) tools: Vec<ToolStatus>,
    pub(crate) proxy: ProxyStatus,
    pub(crate) notes: Vec<String>,
    pub(crate) prepended_path_dirs: Vec<PathBuf>,
}

impl Default for ProxyStatus {
    fn default() -> Self {
        Self::Unset
    }
}

impl StartupReport {
    pub(crate) fn log(&self) {
        for tool in &self.tools {
            match &tool.path {
                Some(path) => eprintln!(
                    "Agent Plugins startup: found {} at {}.",
                    tool.name,
                    path.display()
                ),
                None => eprintln!(
                    "Agent Plugins startup: {} was not found. Skill scripts and MCP servers that invoke it will fail until it is installed.",
                    tool.name
                ),
            }
        }
        if !self.prepended_path_dirs.is_empty() {
            eprintln!(
                "Agent Plugins startup: put {} on PATH.",
                self.prepended_path_dirs
                    .iter()
                    .map(|dir| dir.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        match &self.proxy {
            ProxyStatus::Unset => {
                eprintln!("Agent Plugins startup: no HTTP proxy is configured.");
            }
            ProxyStatus::FromEnvironment { http, https } => eprintln!(
                "Agent Plugins startup: using proxy from the environment (http {}, https {}).",
                display_proxy(http),
                display_proxy(https)
            ),
            ProxyStatus::FromSystem { http, https } => eprintln!(
                "Agent Plugins startup: using the system proxy (http {}, https {}).",
                display_proxy(http),
                display_proxy(https)
            ),
            ProxyStatus::Socks { url } => eprintln!(
                "Agent Plugins startup: using the system SOCKS proxy ({}).",
                display_proxy(url)
            ),
            ProxyStatus::PacOnly { url } => eprintln!(
                "Agent Plugins startup: a proxy auto-config URL is set ({url}); HTTP_PROXY was left unset because PAC files are not evaluated."
            ),
        }
        for note in &self.notes {
            eprintln!("Agent Plugins startup: {note}");
        }
    }
}

pub(crate) trait Host {
    fn extra_search_roots(&self) -> Vec<PathBuf>;
    fn env(&self, key: &str) -> Option<OsString>;
    fn set_env(&mut self, key: &str, value: &OsStr);
    fn is_executable(&self, path: &Path) -> bool;
    fn path_separator(&self) -> char;
    fn env_keys_are_case_insensitive(&self) -> bool;
    fn system_proxy(&self) -> Option<SystemProxy>;
    fn session_path(&self) -> Option<OsString>;
    fn persist_enabled(&self) -> bool;
    fn persist_session(&mut self, key: &str, value: &OsStr) -> Result<(), String>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SystemProxy {
    pub(crate) http: Option<String>,
    pub(crate) https: Option<String>,
    pub(crate) socks: Option<String>,
    pub(crate) no_proxy: Option<String>,
    pub(crate) pac_url: Option<String>,
}

impl SystemProxy {
    fn has_explicit_proxy(&self) -> bool {
        self.http.is_some() || self.https.is_some() || self.socks.is_some()
    }
}

struct LiveHost;

impl Host for LiveHost {
    fn extra_search_roots(&self) -> Vec<PathBuf> {
        live_search_roots()
    }

    fn env(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }

    fn set_env(&mut self, key: &str, value: &OsStr) {
        // SAFETY: `prepare` runs on the main thread in `run` before Tauri
        // starts the async runtime or any worker that reads the environment.
        unsafe {
            std::env::set_var(key, value);
        }
    }

    fn is_executable(&self, path: &Path) -> bool {
        is_executable_file(path)
    }

    fn path_separator(&self) -> char {
        if cfg!(windows) {
            ';'
        } else {
            ':'
        }
    }

    fn env_keys_are_case_insensitive(&self) -> bool {
        cfg!(windows)
    }

    fn system_proxy(&self) -> Option<SystemProxy> {
        live_system_proxy()
    }

    fn session_path(&self) -> Option<OsString> {
        live_session_path()
    }

    fn persist_enabled(&self) -> bool {
        crate::qa_paths::root().ok().flatten().is_none()
    }

    fn persist_session(&mut self, key: &str, value: &OsStr) -> Result<(), String> {
        persist_session_env(key, value)
    }
}

/// Repair PATH, proxy variables, and `uv` TLS settings for this process and
/// the user session. Safe to call once at startup; missing tools are reported
/// and do not stop the application.
pub(crate) fn prepare() -> StartupReport {
    prepare_with(&mut LiveHost)
}

pub(crate) fn prepare_with(host: &mut impl Host) -> StartupReport {
    let mut report = StartupReport::default();
    let tool_dirs = ensure_toolchain_path(host, &mut report);
    ensure_proxy(host, &mut report);
    ensure_uv_trust(host, &mut report);
    persist_session(host, &tool_dirs, &mut report);
    report
}

fn ensure_toolchain_path(host: &mut impl Host, report: &mut StartupReport) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for name in TOOLS {
        match find_tool(host, name) {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    push_unique_dir(&mut dirs, parent);
                }
                report.tools.push(ToolStatus {
                    name,
                    path: Some(path),
                });
            }
            None => report.tools.push(ToolStatus { name, path: None }),
        }
    }
    prepend_dirs(host, &dirs, &current_path_dirs(host));
    report.prepended_path_dirs.clone_from(&dirs);
    dirs
}

fn find_tool(host: &impl Host, name: &str) -> Option<PathBuf> {
    let file_name = tool_file_name(name);
    current_path_dirs(host)
        .into_iter()
        .chain(host.extra_search_roots())
        .map(|dir| dir.join(&file_name))
        .find(|candidate| host.is_executable(candidate))
}

fn ensure_proxy(host: &mut impl Host, report: &mut StartupReport) {
    mirror_proxy_cases(host);
    if let Some((http, https)) = explicit_http_proxies(host) {
        apply_proxy_var(host, "HTTP_PROXY", &http);
        apply_proxy_var(host, "HTTPS_PROXY", &https);
        report.proxy = ProxyStatus::FromEnvironment { http, https };
    } else if let Some(system) = host.system_proxy() {
        apply_system_proxy(host, system, report);
    } else {
        report.proxy = ProxyStatus::Unset;
    }
    ensure_loopback_no_proxy(host);
}

fn apply_system_proxy(host: &mut impl Host, system: SystemProxy, report: &mut StartupReport) {
    if let Some(no_proxy) = system.no_proxy.as_deref() {
        if env_utf8(host, "NO_PROXY").is_none() {
            apply_proxy_var(host, "NO_PROXY", no_proxy);
        }
    }
    let http = system.http.clone().or_else(|| system.https.clone());
    let https = system.https.clone().or_else(|| system.http.clone());
    let has_explicit_proxy = system.has_explicit_proxy();
    match (http, https, system.socks, system.pac_url) {
        (Some(http), Some(https), _, _) => {
            apply_proxy_var(host, "HTTP_PROXY", &http);
            apply_proxy_var(host, "HTTPS_PROXY", &https);
            report.proxy = ProxyStatus::FromSystem { http, https };
        }
        (_, _, Some(url), _) => {
            apply_proxy_var(host, "ALL_PROXY", &url);
            report.proxy = ProxyStatus::Socks { url };
        }
        (_, _, _, Some(url)) => {
            report.proxy = ProxyStatus::PacOnly { url };
        }
        _ => {
            report.proxy = ProxyStatus::Unset;
            if !has_explicit_proxy {
                report.notes.push(
                    "The system proxy settings did not include an explicit HTTP or SOCKS proxy."
                        .to_string(),
                );
            }
        }
    }
}

fn ensure_uv_trust(host: &mut impl Host, report: &mut StartupReport) {
    if env_utf8(host, UV_NATIVE_TLS).is_some() {
        return;
    }
    host.set_env(UV_NATIVE_TLS, OsStr::new("1"));
    report.notes.push(
        "Set UV_NATIVE_TLS=1 so uv uses the platform certificate store behind a corporate proxy."
            .to_string(),
    );
}

fn persist_session(host: &mut impl Host, tool_dirs: &[PathBuf], report: &mut StartupReport) {
    if !host.persist_enabled() {
        return;
    }
    if let Err(error) = persist_session_path(host, tool_dirs) {
        report.notes.push(format!(
            "Could not publish PATH to the user session: {error}"
        ));
    }
    let extra_keys = if host.env_keys_are_case_insensitive() {
        Vec::new()
    } else {
        SESSION_KEYS
            .iter()
            .map(|key| key.to_ascii_lowercase())
            .collect::<Vec<_>>()
    };
    for key in SESSION_KEYS
        .iter()
        .copied()
        .chain(extra_keys.iter().map(String::as_str))
    {
        let Some(value) = host.env(key) else {
            continue;
        };
        if let Err(error) = host.persist_session(key, &value) {
            report.notes.push(format!(
                "Could not publish {key} to the user session: {error}"
            ));
        }
    }
}

fn persist_session_path(host: &mut impl Host, tool_dirs: &[PathBuf]) -> Result<(), String> {
    if tool_dirs.is_empty() {
        return Ok(());
    }
    let sep = host.path_separator();
    let base = host
        .session_path()
        .unwrap_or_else(|| OsString::from(default_gui_path(sep)));
    let published = prepended_path(&split_paths(&base, sep), tool_dirs);
    host.persist_session("PATH", &join_paths(&published, sep))
}

fn prepend_dirs(host: &mut impl Host, dirs: &[PathBuf], existing: &[PathBuf]) {
    if dirs.is_empty() {
        return;
    }
    let published = prepended_path(existing, dirs);
    if published != existing {
        host.set_env("PATH", &join_paths(&published, host.path_separator()));
    }
}

fn prepended_path(existing: &[PathBuf], dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut entries = existing.to_vec();
    for dir in dirs.iter().rev() {
        if !entries.iter().any(|entry| paths_match(entry, dir)) {
            entries.insert(0, dir.clone());
        }
    }
    entries
}

fn mirror_proxy_cases(host: &mut impl Host) {
    for pair in PROXY_PAIRS {
        let upper = env_utf8(host, pair[0]);
        let lower = env_utf8(host, pair[1]);
        match (upper, lower) {
            (Some(value), None) => apply_proxy_var(host, pair[0], &value),
            (None, Some(value)) => apply_proxy_var(host, pair[0], &value),
            (Some(_), Some(_)) | (None, None) => {}
        }
    }
}

fn explicit_http_proxies(host: &impl Host) -> Option<(String, String)> {
    let http = env_utf8(host, "HTTP_PROXY").or_else(|| env_utf8(host, "http_proxy"))?;
    let https = env_utf8(host, "HTTPS_PROXY")
        .or_else(|| env_utf8(host, "https_proxy"))
        .unwrap_or_else(|| http.clone());
    Some((http, https))
}

fn apply_proxy_var(host: &mut impl Host, canonical: &str, value: &str) {
    host.set_env(canonical, OsStr::new(value));
    if !host.env_keys_are_case_insensitive() {
        host.set_env(&canonical.to_ascii_lowercase(), OsStr::new(value));
    }
}

fn ensure_loopback_no_proxy(host: &mut impl Host) {
    let existing = env_utf8(host, "NO_PROXY").unwrap_or_default();
    let mut entries = split_no_proxy(&existing);
    let mut changed = existing.is_empty();
    for host_name in LOOPBACK_NO_PROXY {
        if !entries
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(host_name))
        {
            entries.push(host_name.to_string());
            changed = true;
        }
    }
    if changed {
        apply_proxy_var(host, "NO_PROXY", &entries.join(","));
    }
}

fn split_no_proxy(value: &str) -> Vec<String> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn current_path_dirs(host: &impl Host) -> Vec<PathBuf> {
    host.env("PATH")
        .map(|path| split_paths(&path, host.path_separator()))
        .unwrap_or_default()
}

fn split_paths(path: &OsStr, sep: char) -> Vec<PathBuf> {
    path.to_string_lossy()
        .split(sep)
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn join_paths(entries: &[PathBuf], sep: char) -> OsString {
    let mut joined = OsString::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            joined.push(sep.to_string());
        }
        joined.push(entry);
    }
    joined
}

fn push_unique_dir(dirs: &mut Vec<PathBuf>, dir: &Path) {
    if !dirs.iter().any(|existing| paths_match(existing, dir)) {
        dirs.push(dir.to_path_buf());
    }
}

fn paths_match(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
    } else {
        left == right
    }
}

fn tool_file_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn env_utf8(host: &impl Host, key: &str) -> Option<String> {
    let value = host.env(key)?;
    let value = value.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn default_gui_path(sep: char) -> String {
    if sep == ';' {
        r"C:\Windows\system32;C:\Windows".to_string()
    } else {
        "/usr/bin:/bin:/usr/sbin:/sbin".to_string()
    }
}

fn display_proxy(value: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(value) else {
        return value.to_string();
    };
    if parsed.username().is_empty() && parsed.password().is_none() {
        return value.to_string();
    }
    let _ = parsed.set_username("***");
    let _ = parsed.set_password(None);
    parsed.to_string()
}

fn live_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local/bin"));
        roots.push(home.join(".cargo/bin"));
        roots.push(home.join(".volta/bin"));
        roots.push(home.join(".local/share/fnm/aliases/default/bin"));
        roots.push(home.join(".fnm/aliases/default/bin"));
        if let Some(nvm) = nvm_bin_dir(&home) {
            roots.push(nvm);
        }
    }
    #[cfg(unix)]
    {
        roots.push(PathBuf::from("/opt/homebrew/bin"));
        roots.push(PathBuf::from("/usr/local/bin"));
    }
    #[cfg(windows)]
    {
        roots.push(PathBuf::from(r"C:\Program Files\nodejs"));
        roots.push(PathBuf::from(r"C:\Program Files (x86)\nodejs"));
        if let Some(local) = dirs::data_local_dir() {
            roots.push(local.join("Programs").join("nodejs"));
        }
    }
    roots
}

fn nvm_bin_dir(home: &Path) -> Option<PathBuf> {
    let alias = std::fs::read_to_string(home.join(".nvm/alias/default")).ok()?;
    let version = alias.trim();
    if version.is_empty() {
        return None;
    }
    Some(home.join(".nvm/versions/node").join(version).join("bin"))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn live_system_proxy() -> Option<SystemProxy> {
    #[cfg(target_os = "macos")]
    {
        macos_system_proxy()
    }
    #[cfg(windows)]
    {
        windows_system_proxy()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_system_proxy() -> Option<SystemProxy> {
    let mut command = crate::process::command(Path::new("/usr/sbin/scutil"));
    command.arg("--proxy");
    let output =
        crate::process::run(command, "system proxy lookup", Duration::from_secs(3)).ok()?;
    if !output.status.success() {
        return None;
    }
    let parsed = parse_scutil_proxy(&String::from_utf8_lossy(&output.stdout));
    if parsed.has_explicit_proxy() || parsed.pac_url.is_some() {
        Some(parsed)
    } else {
        None
    }
}

#[cfg(windows)]
fn windows_system_proxy() -> Option<SystemProxy> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let settings = hkcu
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    let pac_url = settings
        .get_value::<String, _>("AutoConfigURL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let enabled = settings.get_value::<u32, _>("ProxyEnable").unwrap_or(0) == 1;
    let server = settings
        .get_value::<String, _>("ProxyServer")
        .ok()
        .unwrap_or_default();
    let override_list = settings
        .get_value::<String, _>("ProxyOverride")
        .ok()
        .unwrap_or_default();
    let no_proxy_entries = parse_windows_proxy_override(&override_list);
    let mut proxy = SystemProxy {
        pac_url,
        no_proxy: (!no_proxy_entries.is_empty()).then(|| no_proxy_entries.join(",")),
        ..SystemProxy::default()
    };
    if enabled {
        let (http, https) = parse_windows_proxy_server(&server);
        proxy.http = http;
        proxy.https = https;
    }
    if proxy.has_explicit_proxy() || proxy.pac_url.is_some() {
        Some(proxy)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn live_session_path() -> Option<OsString> {
    let mut command = crate::process::command(Path::new("/bin/launchctl"));
    command.args(["getenv", "PATH"]);
    let output = crate::process::run(command, "session PATH", Duration::from_secs(2)).ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout);
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(OsString::from(trimmed))
    }
}

#[cfg(not(target_os = "macos"))]
fn live_session_path() -> Option<OsString> {
    None
}

#[cfg(target_os = "macos")]
fn persist_session_env(key: &str, value: &OsStr) -> Result<(), String> {
    let mut command = crate::process::command(Path::new("/bin/launchctl"));
    command.arg("setenv").arg(key).arg(value);
    let output = crate::process::run(command, "session environment", Duration::from_secs(2))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "launchctl setenv {key} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn persist_session_env(_key: &str, _value: &OsStr) -> Result<(), String> {
    Ok(())
}

pub(crate) fn parse_scutil_proxy(text: &str) -> SystemProxy {
    let mut values = BTreeMap::new();
    let mut exceptions = Vec::new();
    let mut in_exceptions = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if in_exceptions {
            if trimmed == "}" {
                in_exceptions = false;
                continue;
            }
            if let Some((_, value)) = split_scutil_field(trimmed) {
                if !value.is_empty() {
                    exceptions.push(value.to_string());
                }
            }
            continue;
        }
        let Some((key, value)) = split_scutil_field(trimmed) else {
            continue;
        };
        if key == "ExceptionsList" {
            in_exceptions = value.contains("<array>");
            continue;
        }
        values.insert(key.to_string(), value.to_string());
    }
    let http = enabled_proxy_url(&values, "HTTPEnable", "HTTPProxy", "HTTPPort");
    let https = enabled_proxy_url(&values, "HTTPSEnable", "HTTPSProxy", "HTTPSPort");
    let socks = if values.get("SOCKSEnable").is_some_and(|value| value == "1") {
        socks_proxy_url(values.get("SOCKSProxy"), values.get("SOCKSPort"))
    } else {
        None
    };
    let pac_url = values
        .get("ProxyAutoConfigEnable")
        .is_some_and(|value| value == "1")
        .then(|| values.get("ProxyAutoConfigURLString").cloned())
        .flatten()
        .filter(|value| !value.is_empty());
    SystemProxy {
        http,
        https,
        socks,
        no_proxy: (!exceptions.is_empty()).then(|| exceptions.join(",")),
        pac_url,
    }
}

fn split_scutil_field(line: &str) -> Option<(&str, &str)> {
    line.split_once(" : ")
        .map(|(key, value)| (key.trim(), value.trim()))
}

fn enabled_proxy_url(
    values: &BTreeMap<String, String>,
    enable_key: &str,
    host_key: &str,
    port_key: &str,
) -> Option<String> {
    if values.get(enable_key).map(String::as_str) != Some("1") {
        return None;
    }
    http_proxy_url(values.get(host_key), values.get(port_key))
}

fn http_proxy_url(host: Option<&String>, port: Option<&String>) -> Option<String> {
    proxy_url("http", host.map(String::as_str), port.map(String::as_str))
}

fn socks_proxy_url(host: Option<&String>, port: Option<&String>) -> Option<String> {
    proxy_url("socks5", host.map(String::as_str), port.map(String::as_str))
}

fn proxy_url(scheme: &str, host: Option<&str>, port: Option<&str>) -> Option<String> {
    let host = host.map(str::trim).filter(|value| !value.is_empty())?;
    if host.contains("://") {
        return Some(host.to_string());
    }
    match port
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "0")
    {
        Some(port) => Some(format!("{scheme}://{host}:{port}")),
        None => Some(format!("{scheme}://{host}")),
    }
}

#[cfg(any(test, windows))]
fn parse_windows_proxy_server(server: &str) -> (Option<String>, Option<String>) {
    let server = server.trim();
    if server.is_empty() {
        return (None, None);
    }
    if !server.contains('=') {
        let url = normalize_proxy_url(server);
        return (url.clone(), url);
    }
    let mut http = None;
    let mut https = None;
    for part in server.split(';') {
        let Some((scheme, rest)) = part.split_once('=') else {
            continue;
        };
        let url = normalize_proxy_url(rest);
        match scheme.trim().to_ascii_lowercase().as_str() {
            "http" => http = url,
            "https" => https = url,
            _ => {}
        }
    }
    (http, https)
}

#[cfg(any(test, windows))]
fn parse_windows_proxy_override(override_list: &str) -> Vec<String> {
    let mut entries = Vec::new();
    for entry in override_list.split([';', ',']) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if entry.eq_ignore_ascii_case("<local>") {
            entries.push("localhost".to_string());
            entries.push("127.0.0.1".to_string());
        } else {
            entries.push(entry.to_string());
        }
    }
    entries
}

#[cfg(any(test, windows))]
fn normalize_proxy_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.contains("://") {
        Some(value.to_string())
    } else {
        Some(format!("http://{value}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    struct FakeHost {
        extra_roots: Vec<PathBuf>,
        vars: BTreeMap<String, OsString>,
        executables: BTreeSet<PathBuf>,
        path_separator: char,
        case_insensitive: bool,
        system_proxy: Option<SystemProxy>,
        session_path: Option<OsString>,
        persist_enabled: bool,
        persisted: BTreeMap<String, OsString>,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                extra_roots: Vec::new(),
                vars: BTreeMap::new(),
                executables: BTreeSet::new(),
                path_separator: ':',
                case_insensitive: false,
                system_proxy: None,
                session_path: None,
                persist_enabled: true,
                persisted: BTreeMap::new(),
            }
        }

        fn with_path(mut self, path: &str) -> Self {
            self.vars.insert("PATH".to_string(), OsString::from(path));
            self
        }

        fn with_executable(mut self, path: &str) -> Self {
            self.executables.insert(PathBuf::from(path));
            self
        }

        fn with_root(mut self, path: &str) -> Self {
            self.extra_roots.push(PathBuf::from(path));
            self
        }

        fn with_env(mut self, key: &str, value: &str) -> Self {
            self.vars.insert(key.to_string(), OsString::from(value));
            self
        }

        fn with_system_proxy(mut self, proxy: SystemProxy) -> Self {
            self.system_proxy = Some(proxy);
            self
        }

        fn env_str(&self, key: &str) -> Option<String> {
            self.env(key)
                .map(|value| value.to_string_lossy().into_owned())
        }
    }

    impl Host for FakeHost {
        fn extra_search_roots(&self) -> Vec<PathBuf> {
            self.extra_roots.clone()
        }

        fn env(&self, key: &str) -> Option<OsString> {
            if self.case_insensitive {
                let needle = key.to_ascii_lowercase();
                return self.vars.iter().find_map(|(existing, value)| {
                    existing
                        .eq_ignore_ascii_case(&needle)
                        .then(|| value.clone())
                });
            }
            self.vars.get(key).cloned()
        }

        fn set_env(&mut self, key: &str, value: &OsStr) {
            if self.case_insensitive {
                if let Some(existing) = self
                    .vars
                    .keys()
                    .find(|existing| existing.eq_ignore_ascii_case(key))
                    .cloned()
                {
                    self.vars.insert(existing, value.to_os_string());
                    return;
                }
            }
            self.vars.insert(key.to_string(), value.to_os_string());
        }

        fn is_executable(&self, path: &Path) -> bool {
            self.executables.contains(path)
        }

        fn path_separator(&self) -> char {
            self.path_separator
        }

        fn env_keys_are_case_insensitive(&self) -> bool {
            self.case_insensitive
        }

        fn system_proxy(&self) -> Option<SystemProxy> {
            self.system_proxy.clone()
        }

        fn session_path(&self) -> Option<OsString> {
            self.session_path.clone()
        }

        fn persist_enabled(&self) -> bool {
            self.persist_enabled
        }

        fn persist_session(&mut self, key: &str, value: &OsStr) -> Result<(), String> {
            self.persisted.insert(key.to_string(), value.to_os_string());
            Ok(())
        }
    }

    fn uv_path() -> PathBuf {
        PathBuf::from("/home/user/.local/bin").join(tool_file_name("uv"))
    }

    fn npx_path() -> PathBuf {
        PathBuf::from("/opt/homebrew/bin").join(tool_file_name("npx"))
    }

    #[test]
    fn finds_uv_outside_path_and_prepends_its_directory() {
        let mut host = FakeHost::new()
            .with_path("/usr/bin:/bin")
            .with_root("/home/user/.local/bin")
            .with_executable(uv_path().to_str().expect("utf8"));
        let report = prepare_with(&mut host);
        assert_eq!(
            report
                .tools
                .iter()
                .find(|tool| tool.name == "uv")
                .and_then(|tool| tool.path.as_ref()),
            Some(&uv_path())
        );
        assert_eq!(
            host.env_str("PATH").as_deref(),
            Some("/home/user/.local/bin:/usr/bin:/bin")
        );
    }

    #[test]
    fn does_not_duplicate_a_directory_already_on_path() {
        let mut host = FakeHost::new()
            .with_path("/home/user/.local/bin:/usr/bin")
            .with_root("/home/user/.local/bin")
            .with_executable(uv_path().to_str().expect("utf8"));
        prepare_with(&mut host);
        assert_eq!(
            host.env_str("PATH").as_deref(),
            Some("/home/user/.local/bin:/usr/bin")
        );
    }

    #[test]
    fn reports_missing_tools_without_changing_path() {
        let mut host = FakeHost::new().with_path("/usr/bin");
        let report = prepare_with(&mut host);
        assert!(report.tools.iter().all(|tool| tool.path.is_none()));
        assert_eq!(host.env_str("PATH").as_deref(), Some("/usr/bin"));
        assert!(report.prepended_path_dirs.is_empty());
    }

    #[test]
    fn copies_lowercase_proxy_to_uppercase_and_fills_https() {
        let mut host = FakeHost::new().with_env("http_proxy", "http://proxy.corp:8080");
        let report = prepare_with(&mut host);
        assert_eq!(
            host.env_str("HTTP_PROXY").as_deref(),
            Some("http://proxy.corp:8080")
        );
        assert_eq!(
            host.env_str("HTTPS_PROXY").as_deref(),
            Some("http://proxy.corp:8080")
        );
        assert_eq!(
            host.env_str("https_proxy").as_deref(),
            Some("http://proxy.corp:8080")
        );
        assert_eq!(
            report.proxy,
            ProxyStatus::FromEnvironment {
                http: "http://proxy.corp:8080".to_string(),
                https: "http://proxy.corp:8080".to_string(),
            }
        );
    }

    #[test]
    fn does_not_overwrite_an_existing_https_proxy() {
        let mut host = FakeHost::new()
            .with_env("HTTP_PROXY", "http://http-proxy:8080")
            .with_env("HTTPS_PROXY", "http://https-proxy:8443");
        prepare_with(&mut host);
        assert_eq!(
            host.env_str("HTTPS_PROXY").as_deref(),
            Some("http://https-proxy:8443")
        );
    }

    #[test]
    fn applies_system_proxy_when_environment_is_empty() {
        let mut host = FakeHost::new().with_system_proxy(SystemProxy {
            http: Some("http://proxy.corp:8080".to_string()),
            https: Some("http://proxy.corp:8443".to_string()),
            no_proxy: Some("*.corp".to_string()),
            ..SystemProxy::default()
        });
        let report = prepare_with(&mut host);
        assert_eq!(
            host.env_str("HTTPS_PROXY").as_deref(),
            Some("http://proxy.corp:8443")
        );
        assert!(host
            .env_str("NO_PROXY")
            .is_some_and(|value| value.contains("*.corp") && value.contains("localhost")));
        assert_eq!(
            report.proxy,
            ProxyStatus::FromSystem {
                http: "http://proxy.corp:8080".to_string(),
                https: "http://proxy.corp:8443".to_string(),
            }
        );
    }

    #[test]
    fn reports_pac_only_without_inventing_a_proxy() {
        let mut host = FakeHost::new().with_system_proxy(SystemProxy {
            pac_url: Some("http://pac.corp/proxy.pac".to_string()),
            ..SystemProxy::default()
        });
        let report = prepare_with(&mut host);
        assert!(host.env_str("HTTP_PROXY").is_none());
        assert_eq!(
            report.proxy,
            ProxyStatus::PacOnly {
                url: "http://pac.corp/proxy.pac".to_string(),
            }
        );
    }

    #[test]
    fn socks_only_system_proxy_becomes_all_proxy() {
        let mut host = FakeHost::new().with_system_proxy(SystemProxy {
            socks: Some("socks5://proxy.corp:1080".to_string()),
            ..SystemProxy::default()
        });
        let report = prepare_with(&mut host);
        assert_eq!(
            host.env_str("ALL_PROXY").as_deref(),
            Some("socks5://proxy.corp:1080")
        );
        assert_eq!(
            report.proxy,
            ProxyStatus::Socks {
                url: "socks5://proxy.corp:1080".to_string(),
            }
        );
    }

    #[test]
    fn sets_uv_native_tls_when_absent_and_preserves_an_existing_value() {
        let mut host = FakeHost::new();
        prepare_with(&mut host);
        assert_eq!(host.env_str(UV_NATIVE_TLS).as_deref(), Some("1"));

        let mut host = FakeHost::new().with_env(UV_NATIVE_TLS, "false");
        prepare_with(&mut host);
        assert_eq!(host.env_str(UV_NATIVE_TLS).as_deref(), Some("false"));
    }

    #[test]
    fn adds_loopback_hosts_to_no_proxy_without_duplicating() {
        let mut host = FakeHost::new().with_env("NO_PROXY", "localhost,*.corp");
        prepare_with(&mut host);
        let value = host.env_str("NO_PROXY").expect("NO_PROXY");
        assert!(value.contains("localhost"));
        assert!(value.contains("127.0.0.1"));
        assert!(value.contains("::1"));
        assert_eq!(value.matches("localhost").count(), 1);
    }

    #[test]
    fn finds_npx_for_mcp_and_prepends_its_directory() {
        let mut host = FakeHost::new()
            .with_path("/usr/bin")
            .with_root("/opt/homebrew/bin")
            .with_executable(npx_path().to_str().expect("utf8"));
        let report = prepare_with(&mut host);
        assert_eq!(
            report
                .tools
                .iter()
                .find(|tool| tool.name == "npx")
                .and_then(|tool| tool.path.as_ref()),
            Some(&npx_path())
        );
        assert!(host
            .env_str("PATH")
            .is_some_and(|path| path.starts_with("/opt/homebrew/bin:")));
    }

    #[test]
    fn publishes_tool_dirs_to_the_session_path_without_using_process_path() {
        let mut host = FakeHost::new()
            .with_path("/opt/conda/bin:/usr/bin")
            .with_root("/home/user/.local/bin")
            .with_executable(uv_path().to_str().expect("utf8"));
        host.session_path = Some(OsString::from("/usr/bin:/bin"));
        prepare_with(&mut host);
        assert_eq!(
            host.persisted
                .get("PATH")
                .map(|value| value.to_string_lossy().into_owned())
                .as_deref(),
            Some("/home/user/.local/bin:/usr/bin:/bin")
        );
        assert_eq!(
            host.persisted
                .get(UV_NATIVE_TLS)
                .map(|value| value.to_string_lossy().into_owned())
                .as_deref(),
            Some("1")
        );
    }

    #[test]
    fn case_insensitive_hosts_only_write_uppercase_proxy_keys() {
        let mut host = FakeHost::new()
            .with_env("http_proxy", "http://proxy.corp:8080")
            .with_system_proxy(SystemProxy::default());
        host.case_insensitive = true;
        prepare_with(&mut host);
        assert!(host.vars.keys().all(|key| {
            !key.chars().any(char::is_lowercase) || key == "http_proxy" || key == "PATH"
        }));
        assert_eq!(
            host.env_str("HTTP_PROXY").as_deref(),
            Some("http://proxy.corp:8080")
        );
    }

    #[test]
    fn second_prepare_is_idempotent() {
        let mut host = FakeHost::new()
            .with_path("/usr/bin")
            .with_root("/home/user/.local/bin")
            .with_executable(uv_path().to_str().expect("utf8"))
            .with_env("NO_PROXY", "localhost");
        prepare_with(&mut host);
        let first_path = host.env_str("PATH");
        let first_no_proxy = host.env_str("NO_PROXY");
        prepare_with(&mut host);
        assert_eq!(host.env_str("PATH"), first_path);
        assert_eq!(host.env_str("NO_PROXY"), first_no_proxy);
    }

    #[test]
    fn parse_scutil_proxy_reads_explicit_proxy_exceptions_and_pac() {
        let parsed = parse_scutil_proxy(
            r#"
<dictionary> {
  ExceptionsList : <array> {
    0 : *.local
    1 : 169.254/16
  }
  HTTPEnable : 1
  HTTPPort : 8080
  HTTPProxy : proxy.corp.example
  HTTPSEnable : 1
  HTTPSPort : 8443
  HTTPSProxy : proxy.corp.example
  ProxyAutoConfigEnable : 0
  SOCKSEnable : 0
}
"#,
        );
        assert_eq!(
            parsed.http.as_deref(),
            Some("http://proxy.corp.example:8080")
        );
        assert_eq!(
            parsed.https.as_deref(),
            Some("http://proxy.corp.example:8443")
        );
        assert_eq!(parsed.no_proxy.as_deref(), Some("*.local,169.254/16"));
        assert!(parsed.pac_url.is_none());

        let pac = parse_scutil_proxy(
            r#"
  HTTPEnable : 0
  ProxyAutoConfigEnable : 1
  ProxyAutoConfigURLString : http://pac.corp/proxy.pac
"#,
        );
        assert_eq!(pac.pac_url.as_deref(), Some("http://pac.corp/proxy.pac"));
        assert!(pac.http.is_none());
    }

    #[test]
    fn parse_windows_proxy_server_handles_shared_and_per_scheme_hosts() {
        assert_eq!(
            parse_windows_proxy_server("proxy.corp:8080"),
            (
                Some("http://proxy.corp:8080".to_string()),
                Some("http://proxy.corp:8080".to_string())
            )
        );
        assert_eq!(
            parse_windows_proxy_server("http=http-proxy:8080;https=https-proxy:8443"),
            (
                Some("http://http-proxy:8080".to_string()),
                Some("http://https-proxy:8443".to_string())
            )
        );
        assert_eq!(
            parse_windows_proxy_override("localhost;<local>;*.corp"),
            ["localhost", "localhost", "127.0.0.1", "*.corp"]
        );
    }

    #[test]
    fn display_proxy_redacts_userinfo() {
        assert_eq!(
            display_proxy("http://user:secret@proxy.corp:8080"),
            "http://***@proxy.corp:8080/"
        );
    }
}
