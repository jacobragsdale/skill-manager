//! Host environment preparation that runs once at process start.
//!
//! This is the only place that repairs the login environment so skill scripts
//! can find `uv`. Agents launch MCP servers themselves, so Node is not
//! discovered or installed here. GUI-launched apps inherit a PATH without `uv`
//! and miss proxy variables set in a shell profile. Corporate TLS interception
//! then breaks `uv` unless it uses the platform certificate store.
//!
//! Installed copies are preferred over a download. GUI apps do not inherit the
//! user's login PATH, so discovery also walks the user and machine Path and
//! Windows App Paths. `uv` is installed only when it is still missing.
//!
//! Add new startup host checks in [`prepare_with`]. Do not scatter PATH or
//! proxy mutations through the rest of the crate.

#[cfg(any(test, target_os = "macos"))]
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const TOOLS: [&str; 2] = ["uv", "uvx"];
const MAX_TOOL_ARCHIVE_BYTES: u64 = 80 * 1024 * 1024;
const TOOL_FETCH_TIMEOUT: Duration = Duration::from_secs(180);
const TOOL_FETCH_REDIRECTS: usize = 8;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolPack {
    Uv,
}

impl ToolPack {
    fn id(self) -> &'static str {
        "uv"
    }

    fn tools(self) -> &'static [&'static str] {
        &["uv", "uvx"]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolStatus {
    pub(crate) name: &'static str,
    pub(crate) path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum ProxyStatus {
    #[default]
    Unset,
    FromEnvironment {
        http: String,
        https: String,
    },
    FromSystem {
        http: String,
        https: String,
    },
    Socks {
        url: String,
    },
    PacOnly {
        url: String,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StartupReport {
    pub(crate) tools: Vec<ToolStatus>,
    pub(crate) proxy: ProxyStatus,
    pub(crate) notes: Vec<String>,
    pub(crate) prepended_path_dirs: Vec<PathBuf>,
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
    fn additional_search_dirs(&self) -> Vec<PathBuf>;
    fn env(&self, key: &str) -> Option<OsString>;
    fn set_env(&mut self, key: &str, value: &OsStr);
    fn is_executable(&self, path: &Path) -> bool;
    fn executable_extensions(&self) -> Vec<String>;
    fn path_separator(&self) -> char;
    fn env_keys_are_case_insensitive(&self) -> bool;
    fn system_proxy(&self) -> Option<SystemProxy>;
    fn session_path(&self) -> Option<OsString>;
    fn session_path_defaults_to_gui(&self) -> bool;
    fn persist_enabled(&self) -> bool;
    fn persist_session(&mut self, key: &str, value: &OsStr) -> Result<(), String>;
    fn managed_tools_root(&self) -> Option<PathBuf>;
    fn install_tool_pack(&mut self, pack: ToolPack) -> Result<PathBuf, String>;
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

    fn additional_search_dirs(&self) -> Vec<PathBuf> {
        live_additional_search_dirs()
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

    fn executable_extensions(&self) -> Vec<String> {
        live_executable_extensions()
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

    fn session_path_defaults_to_gui(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn persist_enabled(&self) -> bool {
        crate::qa_paths::root().ok().flatten().is_none()
    }

    fn persist_session(&mut self, key: &str, value: &OsStr) -> Result<(), String> {
        persist_session_env(key, value)
    }

    fn managed_tools_root(&self) -> Option<PathBuf> {
        live_managed_tools_root()
    }

    fn install_tool_pack(&mut self, pack: ToolPack) -> Result<PathBuf, String> {
        install_official_tool_pack(self, pack)
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
    ensure_proxy(host, &mut report);
    ensure_uv_trust(host, &mut report);
    let tool_dirs = ensure_toolchain(host, &mut report);
    persist_session(host, &tool_dirs, &mut report);
    report
}

fn ensure_toolchain(host: &mut impl Host, report: &mut StartupReport) -> Vec<PathBuf> {
    scan_tools(host, report);
    for pack in missing_packs(&report.tools) {
        if host.managed_tools_root().is_none() {
            report.notes.push(format!(
                "Could not find a user-writable directory to install {}.",
                pack.id()
            ));
            continue;
        }
        match host.install_tool_pack(pack) {
            Ok(dir) => {
                report
                    .notes
                    .push(format!("Installed {} into {}.", pack.id(), dir.display()))
            }
            Err(error) => report.notes.push(format!(
                "Could not install {} without administrator rights: {error}",
                pack.id()
            )),
        }
    }
    report.tools.clear();
    scan_tools(host, report);
    let mut dirs = Vec::new();
    for tool in &report.tools {
        if let Some(parent) = tool.path.as_ref().and_then(|path| path.parent()) {
            push_unique_dir(host, &mut dirs, parent);
        }
    }
    prepend_dirs(host, &dirs, &current_path_dirs(host));
    report.prepended_path_dirs.clone_from(&dirs);
    dirs
}

fn scan_tools(host: &impl Host, report: &mut StartupReport) {
    for name in TOOLS {
        report.tools.push(ToolStatus {
            name,
            path: find_tool(host, name),
        });
    }
    complete_tool_pairs(host, &mut report.tools);
}

fn missing_packs(tools: &[ToolStatus]) -> Vec<ToolPack> {
    let missing_primary = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .is_none_or(|tool| tool.path.is_none())
    };
    let mut packs = Vec::new();
    if missing_primary("uv") {
        packs.push(ToolPack::Uv);
    }
    packs
}

fn complete_tool_pairs(host: &impl Host, tools: &mut [ToolStatus]) {
    complete_companion(host, tools, "uv", "uvx");
}

fn complete_companion(host: &impl Host, tools: &mut [ToolStatus], primary: &str, companion: &str) {
    let Some(dir) = tools
        .iter()
        .find(|tool| tool.name == primary)
        .and_then(|tool| tool.path.as_ref())
        .and_then(|path| path.parent())
        .map(Path::to_path_buf)
    else {
        return;
    };
    let Some(status) = tools.iter_mut().find(|tool| tool.name == companion) else {
        return;
    };
    if status.path.is_some() {
        return;
    }
    status.path = find_tool_in_dir(host, &dir, companion);
}

fn find_tool(host: &impl Host, name: &str) -> Option<PathBuf> {
    candidate_search_dirs(host)
        .into_iter()
        .find_map(|dir| find_tool_in_dir(host, &dir, name))
}

fn candidate_search_dirs(host: &impl Host) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let login_dirs = host
        .session_path()
        .map(|path| split_and_expand_paths(host, &path))
        .unwrap_or_default();
    for dir in current_path_dirs(host)
        .into_iter()
        .chain(login_dirs)
        .chain(host.additional_search_dirs())
        .chain(host.extra_search_roots())
    {
        if !dir.as_os_str().is_empty() {
            push_unique_dir(host, &mut dirs, &dir);
        }
    }
    dirs
}

fn find_tool_in_dir(host: &impl Host, dir: &Path, name: &str) -> Option<PathBuf> {
    tool_file_names(name, &host.executable_extensions())
        .into_iter()
        .map(|file_name| dir.join(file_name))
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
    let base = match host.session_path() {
        Some(path) => path,
        None if host.session_path_defaults_to_gui() => OsString::from(default_gui_path(sep)),
        None => OsString::new(),
    };
    let published = prepended_path(host, &split_paths(&base, sep), tool_dirs);
    host.persist_session("PATH", &join_paths(&published, sep))
}

fn prepend_dirs(host: &mut impl Host, dirs: &[PathBuf], existing: &[PathBuf]) {
    if dirs.is_empty() {
        return;
    }
    let published = prepended_path(host, existing, dirs);
    if published != existing {
        host.set_env("PATH", &join_paths(&published, host.path_separator()));
    }
}

fn prepended_path(host: &impl Host, existing: &[PathBuf], dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut entries = existing.to_vec();
    for dir in dirs.iter().rev() {
        if !entries.iter().any(|entry| paths_match(host, entry, dir)) {
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
        .map(|path| split_and_expand_paths(host, &path))
        .unwrap_or_default()
}

fn split_and_expand_paths(host: &impl Host, path: &OsStr) -> Vec<PathBuf> {
    split_paths(path, host.path_separator())
        .into_iter()
        .map(|entry| expand_path_vars(host, entry))
        .collect()
}

fn split_paths(path: &OsStr, sep: char) -> Vec<PathBuf> {
    path.to_string_lossy()
        .split(sep)
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn expand_path_vars(host: &impl Host, path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if !text.contains('%') {
        return path;
    }
    PathBuf::from(expand_percent_vars(host, &text))
}

fn expand_percent_vars(host: &impl Host, input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let Some(end) = rest.find('%') else {
            out.push('%');
            out.push_str(rest);
            return out;
        };
        let name = &rest[..end];
        rest = &rest[end + 1..];
        if name.is_empty() {
            out.push('%');
            continue;
        }
        if let Some(value) = env_utf8(host, name) {
            out.push_str(&value);
        } else {
            out.push('%');
            out.push_str(name);
            out.push('%');
        }
    }
    out.push_str(rest);
    out
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

fn push_unique_dir(host: &impl Host, dirs: &mut Vec<PathBuf>, dir: &Path) {
    if !dirs.iter().any(|existing| paths_match(host, existing, dir)) {
        dirs.push(dir.to_path_buf());
    }
}

fn paths_match(host: &impl Host, left: &Path, right: &Path) -> bool {
    if host.env_keys_are_case_insensitive() {
        left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
    } else {
        left == right
    }
}

fn tool_file_names(name: &str, extensions: &[String]) -> Vec<String> {
    let mut names = vec![name.to_string()];
    for extension in extensions {
        let extension = extension.trim().trim_start_matches('.');
        if extension.is_empty() {
            continue;
        }
        names.push(format!("{name}.{extension}"));
    }
    names
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
    if let Some(managed) = live_managed_tools_root() {
        roots.push(managed.join("uv"));
    }
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local/bin"));
        roots.push(home.join(".cargo/bin"));
        roots.push(home.join(".asdf/shims"));
        roots.push(home.join(".local/share/mise/shims"));
        roots.push(home.join("scoop/shims"));
    }
    #[cfg(unix)]
    {
        roots.push(PathBuf::from("/opt/homebrew/bin"));
        roots.push(PathBuf::from("/usr/local/bin"));
    }
    #[cfg(windows)]
    {
        roots.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
        if let Some(local) = dirs::data_local_dir() {
            roots.push(local.join("Microsoft").join("WinGet").join("Links"));
            push_python_script_dirs(&mut roots, &local.join("Programs").join("Python"));
        }
        if let Some(roaming) = dirs::data_dir() {
            push_python_script_dirs(&mut roots, &roaming.join("Python"));
        }
    }
    roots
}

fn live_additional_search_dirs() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut dirs = Vec::new();
        if let Some(path) = windows_machine_path() {
            dirs.extend(
                split_paths(&path, ';')
                    .into_iter()
                    .map(|entry| expand_live_percent_vars(&entry.to_string_lossy())),
            );
        }
        for name in ["uv", "uvx"] {
            if let Some(exe) = windows_app_path(name) {
                if let Some(parent) = exe.parent() {
                    dirs.push(parent.to_path_buf());
                }
            }
        }
        dirs
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(windows)]
fn expand_live_percent_vars(path: &str) -> PathBuf {
    if !path.contains('%') {
        return PathBuf::from(path);
    }
    PathBuf::from(expand_percent_vars(&LiveHost, path))
}

#[cfg(windows)]
fn windows_machine_path() -> Option<OsString> {
    let hklm = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
    let environment = hklm
        .open_subkey(r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment")
        .ok()?;
    environment
        .get_value::<String, _>("Path")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(OsString::from)
}

#[cfg(windows)]
fn windows_app_path(name: &str) -> Option<PathBuf> {
    let file = format!("{name}.exe");
    let relative = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{file}");
    for hive in [
        winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER),
        winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE),
    ] {
        let Ok(key) = hive.open_subkey(&relative) else {
            continue;
        };
        let Ok(value) = key.get_value::<String, _>("") else {
            continue;
        };
        let path = PathBuf::from(value.trim().trim_matches('"'));
        if is_executable_file(&path) {
            return Some(path);
        }
    }
    None
}

#[cfg(windows)]
fn push_python_script_dirs(roots: &mut Vec<PathBuf>, python_root: &Path) {
    let Ok(entries) = fs::read_dir(python_root) else {
        return;
    };
    for entry in entries.flatten() {
        let scripts = entry.path().join("Scripts");
        if scripts.is_dir() {
            roots.push(scripts);
        }
    }
}

fn live_managed_tools_root() -> Option<PathBuf> {
    crate::install_v1::SystemPaths::from_system()
        .ok()
        .map(|paths| paths.local_data.join("skill-manager").join("tools"))
}

fn live_executable_extensions() -> Vec<String> {
    #[cfg(windows)]
    {
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        pathext
            .split(';')
            .map(|part| part.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|part| !part.is_empty())
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
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

#[cfg(windows)]
fn live_session_path() -> Option<OsString> {
    windows_user_path()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn live_session_path() -> Option<OsString> {
    None
}

#[cfg(windows)]
fn windows_user_path() -> Option<OsString> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let environment = hkcu.open_subkey("Environment").ok()?;
    environment
        .get_value::<String, _>("Path")
        .or_else(|_| environment.get_value::<String, _>("PATH"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(OsString::from)
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

#[cfg(windows)]
fn persist_session_env(key: &str, value: &OsStr) -> Result<(), String> {
    persist_windows_user_env(key, value)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn persist_session_env(_key: &str, _value: &OsStr) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn persist_windows_user_env(key: &str, value: &OsStr) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_EXPAND_SZ};
    use winreg::RegValue;

    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let environment = hkcu
        .open_subkey_with_flags("Environment", KEY_READ | KEY_SET_VALUE)
        .or_else(|_| hkcu.create_subkey("Environment").map(|(key, _)| key))
        .map_err(|error| format!("Could not open the user environment: {error}"))?;
    let name = if key.eq_ignore_ascii_case("PATH") {
        "Path"
    } else {
        key
    };
    if name == "Path" {
        let mut wide: Vec<u16> = value.encode_wide().collect();
        wide.push(0);
        let bytes = wide.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        environment
            .set_raw_value(
                name,
                &RegValue {
                    bytes,
                    vtype: REG_EXPAND_SZ,
                },
            )
            .map_err(|error| format!("Could not write the user Path: {error}"))?;
    } else {
        let text = value.to_string_lossy();
        environment
            .set_value(name, &text.as_ref())
            .map_err(|error| format!("Could not write user environment {name}: {error}"))?;
    }
    broadcast_environment_change();
    Ok(())
}

#[cfg(windows)]
fn broadcast_environment_change() {
    const HWND_BROADCAST: isize = 0xffff;
    const WM_SETTINGCHANGE: u32 = 0x001A;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;
    let mut name: Vec<u16> = "Environment"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            name.as_mut_ptr() as isize,
            SMTO_ABORTIFHUNG,
            2_000,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn SendMessageTimeoutW(
        hwnd: isize,
        msg: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        timeout_ms: u32,
        result: *mut usize,
    ) -> isize;
}

#[cfg(any(test, target_os = "macos"))]
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

#[cfg(any(test, target_os = "macos"))]
fn split_scutil_field(line: &str) -> Option<(&str, &str)> {
    line.split_once(" : ")
        .map(|(key, value)| (key.trim(), value.trim()))
}

#[cfg(any(test, target_os = "macos"))]
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

#[cfg(any(test, target_os = "macos"))]
fn http_proxy_url(host: Option<&String>, port: Option<&String>) -> Option<String> {
    proxy_url("http", host.map(String::as_str), port.map(String::as_str))
}

#[cfg(any(test, target_os = "macos"))]
fn socks_proxy_url(host: Option<&String>, port: Option<&String>) -> Option<String> {
    proxy_url("socks5", host.map(String::as_str), port.map(String::as_str))
}

#[cfg(any(test, target_os = "macos"))]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolArchiveKind {
    Zip,
    TarGz,
}

struct ToolDownload {
    url: String,
    kind: ToolArchiveKind,
}

fn pack_download(pack: ToolPack) -> Result<ToolDownload, String> {
    pack_download_for(pack, std::env::consts::OS, std::env::consts::ARCH)
}

fn pack_download_for(pack: ToolPack, os: &str, arch: &str) -> Result<ToolDownload, String> {
    let url = match (pack, os, arch) {
        (ToolPack::Uv, "windows", "x86_64") => {
            "https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-pc-windows-msvc.zip"
        }
        (ToolPack::Uv, "windows", "aarch64") => {
            "https://github.com/astral-sh/uv/releases/latest/download/uv-aarch64-pc-windows-msvc.zip"
        }
        (ToolPack::Uv, "macos", "x86_64") => {
            "https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-apple-darwin.tar.gz"
        }
        (ToolPack::Uv, "macos", "aarch64") => {
            "https://github.com/astral-sh/uv/releases/latest/download/uv-aarch64-apple-darwin.tar.gz"
        }
        (ToolPack::Uv, "linux", "x86_64") => {
            "https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-unknown-linux-gnu.tar.gz"
        }
        (ToolPack::Uv, "linux", "aarch64") => {
            "https://github.com/astral-sh/uv/releases/latest/download/uv-aarch64-unknown-linux-gnu.tar.gz"
        }
        _ => {
            return Err(format!(
                "No user-level {} build is published for {os}/{arch}.",
                pack.id()
            ));
        }
    };
    let kind = if url.ends_with(".zip") {
        ToolArchiveKind::Zip
    } else {
        ToolArchiveKind::TarGz
    };
    Ok(ToolDownload {
        url: url.to_string(),
        kind,
    })
}

fn install_official_tool_pack(host: &impl Host, pack: ToolPack) -> Result<PathBuf, String> {
    let root = host
        .managed_tools_root()
        .ok_or_else(|| "Could not find a user-writable tools directory.".to_string())?;
    let dest = root.join(pack.id());
    if pack_is_present(host, &dest, pack) {
        return Ok(dest);
    }
    let download = pack_download(pack)?;
    eprintln!(
        "Agent Plugins startup: downloading {} from {}.",
        pack.id(),
        download.url
    );
    let bytes = download_https(&download.url)?;
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create {}: {error}", root.display()))?;
    let staging = crate::sources::temporary_path(&root, pack.id());
    if let Err(error) = extract_tool_archive(&bytes, download.kind, &staging) {
        let _ = crate::fs_retry::remove_dir_all(&staging);
        return Err(error);
    }
    make_extracted_files_executable(&staging);
    let bin_dir = find_pack_bin_dir(host, &staging, pack).ok_or_else(|| {
        let _ = crate::fs_retry::remove_dir_all(&staging);
        format!(
            "The {} archive did not contain {}.",
            pack.id(),
            pack.tools().join(" and ")
        )
    })?;
    if dest.exists() {
        crate::fs_retry::remove_dir_all(&dest)
            .map_err(|error| format!("Could not replace {}: {error}", dest.display()))?;
    }
    let rename_from = if bin_dir == staging {
        staging.clone()
    } else {
        bin_dir
    };
    crate::fs_retry::rename(&rename_from, &dest).map_err(|error| {
        format!(
            "Could not install {} to {}: {error}",
            pack.id(),
            dest.display()
        )
    })?;
    if staging.exists() {
        let _ = crate::fs_retry::remove_dir_all(&staging);
    }
    Ok(dest)
}

fn pack_is_present(host: &impl Host, dir: &Path, pack: ToolPack) -> bool {
    pack.tools()
        .iter()
        .all(|name| find_tool_in_dir(host, dir, name).is_some())
}

fn find_pack_bin_dir(host: &impl Host, root: &Path, pack: ToolPack) -> Option<PathBuf> {
    let mut current = vec![root.to_path_buf()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for dir in current {
            if pack_is_present(host, &dir, pack) {
                return Some(dir);
            }
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                        next.push(entry.path());
                    }
                }
            }
        }
        current = next;
    }
    None
}

fn make_extracted_files_executable(root: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let Ok(metadata) = path.metadata() else {
                    continue;
                };
                let mut permissions = metadata.permissions();
                permissions.set_mode(permissions.mode() | 0o755);
                let _ = fs::set_permissions(&path, permissions);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = root;
    }
}

fn download_https(url: &str) -> Result<Vec<u8>, String> {
    let parsed = url::Url::parse(url).map_err(|error| format!("Invalid download URL: {error}"))?;
    if parsed.scheme() != "https" {
        return Err("Tool downloads must use https://.".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Tool download URLs may not contain credentials.".to_string());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(TOOL_FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= TOOL_FETCH_REDIRECTS {
                return attempt.error("The download redirected too many times.");
            }
            if attempt.url().scheme() != "https" {
                return attempt.error("The download redirected to a non-HTTPS URL.");
            }
            attempt.follow()
        }))
        .build()
        .map_err(|error| format!("Could not create the download client: {error}"))?;
    let response = client
        .get(url)
        .send()
        .map_err(|error| format!("Could not download {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not download {url}: HTTP {}.",
            response.status()
        ));
    }
    if let Some(length) = response.content_length() {
        if length > MAX_TOOL_ARCHIVE_BYTES {
            return Err("The tool archive is larger than the 80 MB download limit.".to_string());
        }
    }
    let mut bytes = Vec::new();
    let mut reader = response;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("Could not download {url}: {error}"))?;
        if read == 0 {
            break;
        }
        let next = bytes.len().checked_add(read).ok_or_else(|| {
            "The tool archive is larger than the 80 MB download limit.".to_string()
        })?;
        if next as u64 > MAX_TOOL_ARCHIVE_BYTES {
            return Err("The tool archive is larger than the 80 MB download limit.".to_string());
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    Ok(bytes)
}

fn extract_tool_archive(
    bytes: &[u8],
    kind: ToolArchiveKind,
    destination: &Path,
) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;
    match kind {
        ToolArchiveKind::Zip => extract_tool_zip(bytes, destination),
        ToolArchiveKind::TarGz => extract_tool_tar(
            flate2::read::GzDecoder::new(Cursor::new(bytes)),
            destination,
        ),
    }
}

fn extract_tool_zip(bytes: &[u8], destination: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("Could not read the tool zip: {error}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Could not read a zip entry: {error}"))?;
        if entry.is_symlink() {
            return Err("Tool archives may not contain symbolic links.".to_string());
        }
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| "Tool archives may not contain unsafe paths.".to_string())?;
        let relative = sanitize_tool_archive_path(&enclosed)?;
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
        let mut file = File::create(&path)
            .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
        io::copy(&mut entry, &mut file)
            .map_err(|error| format!("Could not extract {}: {error}", path.display()))?;
    }
    Ok(())
}

fn extract_tool_tar<R: Read>(reader: R, destination: &Path) -> Result<(), String> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive
        .entries()
        .map_err(|error| format!("Could not read the tool archive: {error}"))?
    {
        let mut entry = entry.map_err(|error| format!("Could not read a tar entry: {error}"))?;
        let header = entry.header();
        if header.entry_type().is_symlink() || header.entry_type().is_hard_link() {
            return Err("Tool archives may not contain symbolic links.".to_string());
        }
        let enclosed = entry
            .path()
            .map_err(|error| format!("Could not read a tar path: {error}"))?;
        let relative = sanitize_tool_archive_path(&enclosed)?;
        let path = destination.join(&relative);
        if header.entry_type().is_dir() {
            fs::create_dir_all(&path)
                .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
            continue;
        }
        if !header.entry_type().is_file() {
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        let mut file = File::create(&path)
            .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
        io::copy(&mut entry, &mut file)
            .map_err(|error| format!("Could not extract {}: {error}", path.display()))?;
    }
    Ok(())
}

fn sanitize_tool_archive_path(path: &Path) -> Result<PathBuf, String> {
    let mut sanitized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => sanitized.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("Tool archives may not contain parent-directory paths.".to_string());
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err("Tool archives may not contain absolute paths.".to_string());
            }
        }
    }
    if sanitized.as_os_str().is_empty() {
        return Err("Tool archives may not contain empty paths.".to_string());
    }
    Ok(sanitized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    struct FakeHost {
        extra_roots: Vec<PathBuf>,
        additional_dirs: Vec<PathBuf>,
        vars: BTreeMap<String, OsString>,
        executables: BTreeSet<PathBuf>,
        executable_extensions: Vec<String>,
        path_separator: char,
        case_insensitive: bool,
        system_proxy: Option<SystemProxy>,
        session_path: Option<OsString>,
        session_path_defaults_to_gui: bool,
        persist_enabled: bool,
        persisted: BTreeMap<String, OsString>,
        managed_root: Option<PathBuf>,
        install_error: Option<String>,
        installed: Vec<ToolPack>,
        https_proxy_at_install: Option<String>,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                extra_roots: Vec::new(),
                additional_dirs: Vec::new(),
                vars: BTreeMap::new(),
                executables: BTreeSet::new(),
                executable_extensions: default_test_extensions(),
                path_separator: ':',
                case_insensitive: false,
                system_proxy: None,
                session_path: None,
                session_path_defaults_to_gui: false,
                persist_enabled: true,
                persisted: BTreeMap::new(),
                managed_root: None,
                install_error: None,
                installed: Vec::new(),
                https_proxy_at_install: None,
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

        fn with_additional_dir(mut self, path: &str) -> Self {
            self.additional_dirs.push(PathBuf::from(path));
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

        fn with_managed_root(mut self, path: &str) -> Self {
            self.managed_root = Some(PathBuf::from(path));
            self
        }

        fn with_extensions(mut self, extensions: &[&str]) -> Self {
            self.executable_extensions = extensions.iter().map(|ext| (*ext).to_string()).collect();
            self
        }

        fn with_install_error(mut self, error: &str) -> Self {
            self.install_error = Some(error.to_string());
            self
        }
    }

    fn default_test_extensions() -> Vec<String> {
        if cfg!(windows) {
            vec!["exe".to_string()]
        } else {
            Vec::new()
        }
    }

    fn tool_file_name(name: &str) -> String {
        format!("{name}{}", std::env::consts::EXE_SUFFIX)
    }

    impl Host for FakeHost {
        fn extra_search_roots(&self) -> Vec<PathBuf> {
            self.extra_roots.clone()
        }

        fn additional_search_dirs(&self) -> Vec<PathBuf> {
            self.additional_dirs.clone()
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

        fn executable_extensions(&self) -> Vec<String> {
            self.executable_extensions.clone()
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

        fn session_path_defaults_to_gui(&self) -> bool {
            self.session_path_defaults_to_gui
        }

        fn persist_enabled(&self) -> bool {
            self.persist_enabled
        }

        fn persist_session(&mut self, key: &str, value: &OsStr) -> Result<(), String> {
            self.persisted.insert(key.to_string(), value.to_os_string());
            Ok(())
        }

        fn managed_tools_root(&self) -> Option<PathBuf> {
            self.managed_root.clone()
        }

        fn install_tool_pack(&mut self, pack: ToolPack) -> Result<PathBuf, String> {
            self.https_proxy_at_install = self.env_str("HTTPS_PROXY");
            if let Some(error) = &self.install_error {
                return Err(error.clone());
            }
            let root = self
                .managed_root
                .clone()
                .ok_or_else(|| "No user-writable tools directory is configured.".to_string())?;
            let dir = root.join(pack.id());
            for name in pack.tools() {
                for file_name in tool_file_names(name, &self.executable_extensions) {
                    self.executables.insert(dir.join(file_name));
                }
            }
            if !self.extra_roots.iter().any(|existing| existing == &dir) {
                self.extra_roots.insert(0, dir.clone());
            }
            self.installed.push(pack);
            Ok(dir)
        }
    }

    fn uv_path() -> PathBuf {
        PathBuf::from("/home/user/.local/bin").join(tool_file_name("uv"))
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

    #[test]
    fn installs_missing_uv_into_the_user_tools_directory() {
        let mut host = FakeHost::new()
            .with_path("/usr/bin")
            .with_managed_root("/home/user/.local/share/skill-manager/tools");
        let report = prepare_with(&mut host);
        assert_eq!(host.installed, [ToolPack::Uv]);
        assert!(report.tools.iter().all(|tool| tool.path.is_some()));
        assert!(host
            .env_str("PATH")
            .is_some_and(|path| path.contains("/home/user/.local/share/skill-manager/tools/uv")));
    }

    #[test]
    fn does_not_install_tools_that_are_already_on_path() {
        let mut host = FakeHost::new()
            .with_path("/usr/bin")
            .with_root("/home/user/.local/bin")
            .with_executable(uv_path().to_str().expect("utf8"))
            .with_executable(
                PathBuf::from("/home/user/.local/bin")
                    .join(tool_file_name("uvx"))
                    .to_str()
                    .expect("utf8"),
            )
            .with_managed_root("/tmp/tools");
        prepare_with(&mut host);
        assert!(host.installed.is_empty());
    }

    #[test]
    fn failed_user_level_install_is_reported_and_does_not_stop_startup() {
        let mut host = FakeHost::new()
            .with_path("/usr/bin")
            .with_managed_root("/tmp/tools")
            .with_install_error("download blocked");
        let report = prepare_with(&mut host);
        assert!(report.tools.iter().all(|tool| tool.path.is_none()));
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("uv") && note.contains("download blocked")));
        assert_eq!(host.env_str("PATH").as_deref(), Some("/usr/bin"));
    }

    #[test]
    fn applies_proxy_before_installing_tools() {
        let mut host = FakeHost::new()
            .with_path("/usr/bin")
            .with_managed_root("/tmp/tools")
            .with_env("https_proxy", "http://proxy.corp:8080");
        prepare_with(&mut host);
        assert_eq!(
            host.https_proxy_at_install.as_deref(),
            Some("http://proxy.corp:8080")
        );
    }

    #[test]
    fn windows_session_path_persist_keeps_the_user_path_only() {
        let uv = PathBuf::from(r"C:\Users\me\.local\bin").join(tool_file_name("uv"));
        let mut host = FakeHost::new()
            .with_extensions(&["exe"])
            .with_path(r"C:\Windows\system32;C:\Windows;C:\Users\me\.local\bin")
            .with_root(r"C:\Users\me\.local\bin")
            .with_executable(uv.to_str().expect("utf8"));
        host.path_separator = ';';
        host.case_insensitive = true;
        host.session_path = Some(OsString::from(r"%USERPROFILE%\bin"));
        prepare_with(&mut host);
        assert_eq!(
            host.persisted
                .get("PATH")
                .map(|value| value.to_string_lossy().into_owned())
                .as_deref(),
            Some(r"C:\Users\me\.local\bin;%USERPROFILE%\bin")
        );
    }

    #[test]
    fn empty_windows_user_path_persists_only_tool_directories() {
        let uv = PathBuf::from(r"C:\Users\me\.local\bin").join(tool_file_name("uv"));
        let mut host = FakeHost::new()
            .with_extensions(&["exe"])
            .with_path(r"C:\Windows\system32;C:\Users\me\.local\bin")
            .with_root(r"C:\Users\me\.local\bin")
            .with_executable(uv.to_str().expect("utf8"));
        host.path_separator = ';';
        host.session_path_defaults_to_gui = false;
        prepare_with(&mut host);
        assert_eq!(
            host.persisted
                .get("PATH")
                .map(|value| value.to_string_lossy().into_owned())
                .as_deref(),
            Some(r"C:\Users\me\.local\bin")
        );
    }

    #[test]
    fn windows_tool_download_urls_are_user_level_zips() {
        let uv = pack_download_for(ToolPack::Uv, "windows", "x86_64").expect("uv");
        assert!(uv.url.contains("uv-x86_64-pc-windows-msvc.zip"));
        assert_eq!(uv.kind, ToolArchiveKind::Zip);
    }

    #[test]
    fn finds_uv_on_the_login_path_without_installing() {
        let uv = PathBuf::from("/users/me/.local/bin").join(tool_file_name("uv"));
        let mut host = FakeHost::new()
            .with_path("/windows/system32")
            .with_executable(uv.to_str().expect("utf8"));
        host.session_path = Some(OsString::from("/users/me/.local/bin"));
        let report = prepare_with(&mut host);
        assert_eq!(
            report
                .tools
                .iter()
                .find(|tool| tool.name == "uv")
                .and_then(|tool| tool.path.as_ref()),
            Some(&uv)
        );
        assert!(host.installed.is_empty());
    }

    #[test]
    fn expands_percent_variables_in_the_login_path() {
        let uv = PathBuf::from("/users/me/bin").join(tool_file_name("uv"));
        let mut host = FakeHost::new()
            .with_path("/windows/system32")
            .with_env("USERPROFILE", "/users/me")
            .with_executable(uv.to_str().expect("utf8"));
        host.session_path = Some(OsString::from("%USERPROFILE%/bin"));
        let report = prepare_with(&mut host);
        assert_eq!(
            report
                .tools
                .iter()
                .find(|tool| tool.name == "uv")
                .and_then(|tool| tool.path.as_ref()),
            Some(&uv)
        );
    }

    #[test]
    fn finds_uv_from_an_additional_search_dir() {
        let uv_dir = PathBuf::from("/users/me/.local/bin");
        let uv = uv_dir.join(tool_file_name("uv"));
        let mut host = FakeHost::new()
            .with_path("/windows/system32")
            .with_additional_dir(uv_dir.to_str().expect("utf8"))
            .with_executable(uv.to_str().expect("utf8"));
        let report = prepare_with(&mut host);
        assert_eq!(
            report
                .tools
                .iter()
                .find(|tool| tool.name == "uv")
                .and_then(|tool| tool.path.as_ref()),
            Some(&uv)
        );
        assert!(host.installed.is_empty());
    }

    #[test]
    fn does_not_download_uv_when_only_uvx_is_missing() {
        let mut host = FakeHost::new()
            .with_path("/usr/bin")
            .with_root("/home/user/.local/bin")
            .with_executable(uv_path().to_str().expect("utf8"))
            .with_managed_root("/tmp/tools");
        prepare_with(&mut host);
        assert!(!host.installed.contains(&ToolPack::Uv));
    }

    #[test]
    fn expand_percent_vars_replaces_known_names() {
        let host = FakeHost::new().with_env("LOCALAPPDATA", r"C:\Users\me\AppData\Local");
        assert_eq!(
            expand_percent_vars(&host, r"%LOCALAPPDATA%\Microsoft\WinGet\Links"),
            r"C:\Users\me\AppData\Local\Microsoft\WinGet\Links"
        );
        assert_eq!(
            expand_percent_vars(&host, r"%MISSING%\bin"),
            r"%MISSING%\bin"
        );
    }

    #[test]
    fn finds_uv_binaries_in_the_official_windows_zip_layout() {
        let root = tempfile::tempdir().expect("temp");
        let nested = root.path().join("uv-x86_64-pc-windows-msvc");
        fs::create_dir_all(&nested).expect("uv dir");
        for name in ["uv.exe", "uvx.exe"] {
            fs::write(nested.join(name), []).expect("tool file");
        }
        let mut host = FakeHost::new().with_extensions(&["exe"]);
        for name in ["uv.exe", "uvx.exe"] {
            host.executables.insert(nested.join(name));
        }
        assert_eq!(
            find_pack_bin_dir(&host, root.path(), ToolPack::Uv).as_deref(),
            Some(nested.as_path())
        );
    }
}
