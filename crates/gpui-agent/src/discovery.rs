//! Find a running AgentHost by bundle id or executable name.
//!
//! Each host writes one JSON record when it starts listening and removes it
//! on shutdown. The CLI matches `--connect <id>` against those records, then
//! remembers the choice, which a later command reuses only when it asks for it
//! with `--connect last`. A command with neither `--addr` nor `--connect` goes
//! to the default address: the saved choice is per user, not per terminal, so
//! reusing it silently would let one agent drive the app another agent picked.

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::protocol::HelloInfo;
use crate::server::default_addr;

/// Host-side override. When set, `--connect` matches this string even if the
/// process has no bundle id.
pub const CONNECT_ID_ENV: &str = "GPUI_AGENT_CONNECT_ID";
/// Directory that holds `hosts/` and `session.json`. Tests and launchers set
/// this so records stay out of the real per-user directory.
pub const RUNTIME_DIR_ENV: &str = "GPUI_AGENT_RUNTIME_DIR";
/// `--connect last`: the app the last `--connect <id>` picked.
pub const LAST_SESSION: &str = "last";

const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn getuid() -> u32;
    fn geteuid() -> u32;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostRecord {
    pub addr: String,
    pub pid: u32,
    pub app: String,
    pub executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub addr: String,
    pub id: String,
}

/// A saved address and credentials for a named CLI session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamedSession {
    pub name: String,
    pub addr: String,
    pub token: Option<String>,
    pub allow_remote: bool,
    pub app: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HostIdentity {
    pub executable: String,
    pub bundle_id: Option<String>,
    pub connect_id: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolveError {
    #[error("no AgentHost is running for '{id}'")]
    Missing { id: String },
    #[error("more than one AgentHost matches '{id}': {detail}")]
    Ambiguous { id: String, detail: String },
    #[error("invalid host address '{addr}'")]
    BadAddr { addr: String },
    #[error("no earlier --connect to reuse; pass --connect <bundle id or executable>")]
    NoSession,
    #[error(
        "no session named \"{name}\"; create it with: gpui-agent --session <name> attach <target>"
    )]
    NoNamedSession { name: String },
    #[error("invalid session name \"{name}\"; use [a-z0-9][a-z0-9_-]{{0,63}}")]
    BadSessionName { name: String },
    #[error("could not use runtime directory: {detail}")]
    RuntimeDirectory { detail: String },
    #[error("could not read session \"{name}\": {detail}")]
    NamedSessionRead { name: String, detail: String },
}

/// Where a command goes, and which app that is when discovery chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTarget {
    pub addr: SocketAddr,
    /// The app's `hello().app` from its record; `None` for `--addr` and the default address.
    pub app: Option<String>,
    /// What the command asked for: `--connect <id>`, `--connect last`, or nothing.
    pub connect: Option<String>,
}

impl ClientTarget {
    /// One line naming the app a command is about to act on, for stderr.
    pub fn describe(&self) -> String {
        match (&self.app, &self.connect) {
            (Some(app), Some(id)) => format!("acting on {app} at {} (--connect {id})", self.addr),
            (Some(app), None) => format!("acting on {app} at {}", self.addr),
            (None, _) => format!("acting on {}", self.addr),
        }
    }
}

/// Per-user directory for discovery records.
pub fn runtime_root() -> PathBuf {
    if let Some(over) = std::env::var_os(RUNTIME_DIR_ENV) {
        if !over.is_empty() {
            return PathBuf::from(over);
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("gpui-agent");
        }
    }
    let mut base = std::env::temp_dir();
    base.push(format!("gpui-agent-{}", current_uid()));
    base
}

fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getuid takes no arguments, cannot fail and touches no
        // memory of ours.
        unsafe { getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no
    // memory of ours.
    unsafe { geteuid() }
}

#[cfg(unix)]
fn ensure_private_directory(path: &Path, create: bool) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    if create {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        let created = match builder.create(path) {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error),
        };
        if created {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "unsafe runtime directory '{}': expected a real directory, not a symlink",
                path.display()
            ),
        ));
    }
    if metadata.uid() != effective_uid() {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "unsafe runtime directory '{}': it is not owned by the effective user",
                path.display()
            ),
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "unsafe runtime directory '{}': could not set mode 0700: {error}",
                    path.display()
                ),
            )
        })?;
        let tightened = fs::symlink_metadata(path)?;
        if !tightened.file_type().is_dir() {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "unsafe runtime directory '{}': expected a real directory, not a symlink",
                    path.display()
                ),
            ));
        }
        if tightened.uid() != effective_uid() {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "unsafe runtime directory '{}': it is not owned by the effective user",
                    path.display()
                ),
            ));
        }
        if tightened.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "unsafe runtime directory '{}': group and other access is not allowed; set its mode to 0700",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_directory(path: &Path, create: bool) -> io::Result<()> {
    if create {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Create or validate the private per-user runtime directory.
pub fn ensure_runtime_dir(path: &Path) -> io::Result<()> {
    ensure_private_directory(path, true)
}

fn ensure_runtime_root_for_discovery(root: &Path) -> Result<(), ResolveError> {
    ensure_runtime_dir(root).map_err(|error| ResolveError::RuntimeDirectory {
        detail: error.to_string(),
    })
}

#[cfg(unix)]
fn ensure_private_session_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "unsafe named session file '{}': expected a regular file",
                path.display()
            ),
        ));
    }
    if metadata.uid() != effective_uid() {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "unsafe named session file '{}': it is not owned by the effective user",
                path.display()
            ),
        ));
    }
    if metadata.mode() & 0o044 != 0 {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "unsafe named session file '{}': group and other read access is not allowed",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_session_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn hosts_dir(root: &Path) -> PathBuf {
    root.join("hosts")
}

fn session_path(root: &Path) -> PathBuf {
    root.join("session.json")
}

fn named_sessions_dir(root: &Path) -> PathBuf {
    root.join("sessions")
}

fn named_session_path(root: &Path, name: &str) -> PathBuf {
    named_sessions_dir(root).join(format!("{name}.json"))
}

fn checked_named_sessions_dir(root: &Path, create: bool) -> io::Result<PathBuf> {
    ensure_private_directory(root, create)?;
    let dir = named_sessions_dir(root);
    ensure_private_directory(&dir, create)?;
    Ok(dir)
}

fn valid_session_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (1..=64).contains(&name.len())
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn validate_session_name(name: &str) -> io::Result<()> {
    if valid_session_name(name) {
        Ok(())
    } else {
        Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("invalid session name \"{name}\"; use [a-z0-9][a-z0-9_-]{{0,63}}"),
        ))
    }
}

fn invalid_session_name(name: &str) -> ResolveError {
    ResolveError::BadSessionName {
        name: name.to_string(),
    }
}

fn named_session_read_error(name: &str, err: io::Error) -> ResolveError {
    if err.kind() == ErrorKind::NotFound {
        ResolveError::NoNamedSession {
            name: name.to_string(),
        }
    } else {
        ResolveError::NamedSessionRead {
            name: name.to_string(),
            detail: err.to_string(),
        }
    }
}

/// Bundle id (macOS `.app` Info.plist) and executable basename for this process.
pub fn detect_identity() -> HostIdentity {
    let exe = std::env::current_exe().unwrap_or_default();
    let executable = exe
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string();
    let connect_id = std::env::var(CONNECT_ID_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let bundle_id = bundle_id_from_exe(&exe);
    HostIdentity {
        executable,
        bundle_id,
        connect_id,
    }
}

/// Fill empty `hello.bundle_id` / `hello.executable` from this process.
pub fn fill_hello_identity(hello: &mut HelloInfo) {
    let ident = detect_identity();
    if hello.executable.as_deref().unwrap_or("").is_empty() {
        hello.executable = Some(ident.executable);
    }
    if hello.bundle_id.as_deref().unwrap_or("").is_empty() {
        hello.bundle_id = ident.bundle_id.or(ident.connect_id);
    }
}

fn bundle_id_from_exe(exe: &Path) -> Option<String> {
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..8 {
        if dir.extension().and_then(|e| e.to_str()) == Some("app") {
            return read_bundle_id(&dir.join("Contents").join("Info.plist"));
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

fn read_bundle_id(plist: &Path) -> Option<String> {
    let text = fs::read_to_string(plist).ok()?;
    let key = "<key>CFBundleIdentifier</key>";
    let rest = text.get(text.find(key)? + key.len()..)?;
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    let id = rest[start..end].trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Write the record for this process and return a guard that deletes it.
pub fn publish_current(addr: SocketAddr, app: &str) -> io::Result<PublishedHost> {
    let ident = detect_identity();
    let record = HostRecord {
        addr: addr.to_string(),
        pid: std::process::id(),
        app: app.to_string(),
        executable: ident.executable,
        bundle_id: ident.bundle_id,
        connect_id: ident.connect_id,
    };
    publish_record(&runtime_root(), &record)
}

pub fn publish_record(root: &Path, record: &HostRecord) -> io::Result<PublishedHost> {
    ensure_private_directory(root, true)?;
    let dir = hosts_dir(root);
    fs::create_dir_all(&dir)?;
    let port = record
        .addr
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .unwrap_or(0);
    let path = dir.join(format!("{}-{port}.json", record.pid));
    let body = serde_json::to_vec_pretty(record)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
    let tmp = dir.join(format!(".{}-{port}.json.tmp", record.pid));
    fs::write(&tmp, &body)?;
    fs::rename(&tmp, &path)?;
    Ok(PublishedHost { path })
}

pub struct PublishedHost {
    path: PathBuf,
}

impl Drop for PublishedHost {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl PublishedHost {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `--addr` wins. Otherwise `--connect <id>` (saved for `--connect last`), then
/// `--connect last`, then the loopback default. A bare command never reuses the
/// saved choice (#48).
pub fn resolve_client_addr(
    root: &Path,
    explicit: Option<SocketAddr>,
    connect: Option<&str>,
) -> Result<SocketAddr, ResolveError> {
    resolve_client_target(root, explicit, connect).map(|target| target.addr)
}

/// `resolve_client_addr`, also naming the app discovery chose so the CLI can say
/// which app it is about to drive.
pub fn resolve_client_target(
    root: &Path,
    explicit: Option<SocketAddr>,
    connect: Option<&str>,
) -> Result<ClientTarget, ResolveError> {
    if let Some(addr) = explicit {
        return Ok(ClientTarget {
            addr,
            app: None,
            connect: None,
        });
    }
    let Some(id) = connect.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(ClientTarget {
            addr: default_addr(),
            app: None,
            connect: None,
        });
    };
    if id == LAST_SESSION {
        ensure_runtime_root_for_discovery(root)?;
        let session = load_session(root).ok_or(ResolveError::NoSession)?;
        let found = resolve_id(root, &session.id).ok();
        let addr = match &found {
            Some(record) => {
                let _ = save_session(root, &record.addr, &session.id);
                record.addr.clone()
            }
            None if tcp_open(&session.addr) => session.addr.clone(),
            None => {
                let _ = fs::remove_file(session_path(root));
                return Err(ResolveError::Missing { id: session.id });
            }
        };
        return Ok(ClientTarget {
            addr: parse_addr(&addr)?,
            app: Some(found.map_or(session.id, |record| record.app)),
            connect: Some(LAST_SESSION.to_string()),
        });
    }
    let record = resolve_id(root, id)?;
    save_session(root, &record.addr, id).map_err(|_| ResolveError::BadAddr {
        addr: record.addr.clone(),
    })?;
    Ok(ClientTarget {
        addr: parse_addr(&record.addr)?,
        app: Some(record.app),
        connect: Some(id.to_string()),
    })
}

pub fn resolve_id(root: &Path, id: &str) -> Result<HostRecord, ResolveError> {
    ensure_runtime_root_for_discovery(root)?;
    let live = load_live(root);
    let exact: Vec<_> = live
        .iter()
        .filter(|(_, rec)| exact_match(rec, id))
        .cloned()
        .collect();
    let candidates = if exact.is_empty() {
        live.into_iter()
            .filter(|(_, rec)| basename_match(rec, id))
            .collect()
    } else {
        exact
    };
    let mut open = Vec::new();
    for (path, rec) in candidates {
        if tcp_open(&rec.addr) {
            open.push(rec);
        } else {
            let _ = fs::remove_file(path);
        }
    }
    match <[_; 1]>::try_from(open) {
        Ok([one]) => Ok(one),
        Err(open) if open.is_empty() => Err(ResolveError::Missing { id: id.to_string() }),
        Err(open) => Err(ResolveError::Ambiguous {
            id: id.to_string(),
            detail: open
                .iter()
                .map(|rec| format!("{} ({})", rec.app, rec.addr))
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

fn exact_match(rec: &HostRecord, id: &str) -> bool {
    rec.bundle_id.as_deref() == Some(id)
        || rec.connect_id.as_deref() == Some(id)
        || rec.executable == id
        || rec.app == id
}

fn basename_match(rec: &HostRecord, id: &str) -> bool {
    let want = Path::new(id)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(id);
    let exec = Path::new(&rec.executable)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(rec.executable.as_str());
    #[cfg(windows)]
    {
        let normalize = |name: &str| {
            let name = name.to_ascii_lowercase();
            name.strip_suffix(".exe").unwrap_or(&name).to_owned()
        };
        normalize(exec) == normalize(want)
    }
    #[cfg(not(windows))]
    {
        exec == want || exec == id
    }
}

fn load_live(root: &Path) -> Vec<(PathBuf, HostRecord)> {
    let dir = hosts_dir(root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(body) = fs::read(&path) else {
            continue;
        };
        let Ok(rec) = serde_json::from_slice::<HostRecord>(&body) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        if !pid_alive(rec.pid) {
            let _ = fs::remove_file(&path);
            continue;
        }
        out.push((path, rec));
    }
    out
}

/// Every host that published itself under `root` and is still running.
///
/// Returns an error when the runtime directory is unusable rather than an empty list.
pub fn live_hosts(root: &Path) -> Result<Vec<HostRecord>, ResolveError> {
    ensure_runtime_root_for_discovery(root)?;
    let mut hosts: Vec<_> = load_live(root).into_iter().map(|(_, rec)| rec).collect();
    hosts.sort_by(|a, b| a.addr.cmp(&b.addr));
    Ok(hosts)
}

pub fn save_session(root: &Path, addr: &str, id: &str) -> io::Result<()> {
    ensure_private_directory(root, true)?;
    let rec = SessionRecord {
        addr: addr.to_string(),
        id: id.to_string(),
    };
    let body = serde_json::to_vec_pretty(&rec)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
    let path = session_path(root);
    let tmp = root.join(".session.json.tmp");
    fs::write(&tmp, body)?;
    fs::rename(tmp, path)
}

pub fn load_session(root: &Path) -> Option<SessionRecord> {
    let body = fs::read(session_path(root)).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Atomically save a named session under `root/sessions`.
pub fn save_named_session(root: &Path, session: &NamedSession) -> io::Result<()> {
    validate_session_name(&session.name)?;
    let dir = checked_named_sessions_dir(root, true)?;

    let body = serde_json::to_vec_pretty(session)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = dir.join(format!(
        ".{}.{}.{}.tmp",
        session.name,
        std::process::id(),
        nonce
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let result = (|| {
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, named_session_path(root, &session.name))
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

/// Load a named session, rejecting invalid names and missing records.
pub fn load_named_session(root: &Path, name: &str) -> Result<NamedSession, ResolveError> {
    if !valid_session_name(name) {
        return Err(invalid_session_name(name));
    }
    let dir = checked_named_sessions_dir(root, false)
        .map_err(|err| named_session_read_error(name, err))?;
    let path = dir.join(format!("{name}.json"));
    ensure_private_session_file(&path).map_err(|err| named_session_read_error(name, err))?;
    let body = fs::read(&path).map_err(|err| named_session_read_error(name, err))?;
    let session: NamedSession =
        serde_json::from_slice(&body).map_err(|err| ResolveError::NamedSessionRead {
            name: name.to_string(),
            detail: err.to_string(),
        })?;
    if session.name != name {
        return Err(ResolveError::NamedSessionRead {
            name: name.to_string(),
            detail: "stored session name does not match its file name".into(),
        });
    }
    Ok(session)
}

/// Remove a named session, returning whether a record existed.
pub fn remove_named_session(root: &Path, name: &str) -> io::Result<bool> {
    validate_session_name(name)?;
    let dir = match checked_named_sessions_dir(root, false) {
        Ok(dir) => dir,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    match fs::remove_file(dir.join(format!("{name}.json"))) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// List valid saved sessions, ordered by name.
///
/// Returns an error when session storage is unreadable or unsafe.
pub fn list_named_sessions(root: &Path) -> Result<Vec<NamedSession>, ResolveError> {
    let dir = match checked_named_sessions_dir(root, false) {
        Ok(dir) => dir,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(ResolveError::RuntimeDirectory {
                detail: err.to_string(),
            });
        }
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(ResolveError::RuntimeDirectory {
                detail: err.to_string(),
            });
        }
    };
    let entries =
        entries
            .collect::<io::Result<Vec<_>>>()
            .map_err(|err| ResolveError::RuntimeDirectory {
                detail: err.to_string(),
            })?;
    let mut sessions = Vec::new();
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(name) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        if !valid_session_name(&name) {
            continue;
        }
        match ensure_private_session_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(ResolveError::NamedSessionRead {
                    name,
                    detail: err.to_string(),
                });
            }
        }
        let body = match fs::read(&path) {
            Ok(body) => body,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(ResolveError::NamedSessionRead {
                    name,
                    detail: err.to_string(),
                });
            }
        };
        let Ok(session) = serde_json::from_slice::<NamedSession>(&body) else {
            continue;
        };
        if session.name == name {
            sessions.push(session);
        }
    }
    sessions.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(sessions)
}

fn parse_addr(addr: &str) -> Result<SocketAddr, ResolveError> {
    addr.parse().map_err(|_| ResolveError::BadAddr {
        addr: addr.to_string(),
    })
}

pub fn tcp_open(addr: &str) -> bool {
    let Ok(addr) = addr.parse::<SocketAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok()
}

pub fn pid_alive(pid: u32) -> bool {
    // `kill(-1, 0)` targets every process. Refuse pids that do not fit in a
    // positive pid_t.
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // A pid past i32 would wrap to a negative pid, which signals a
        // whole process group.
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 only checks that `pid` exists and may be
        // signalled; nothing is delivered and no memory is shared.
        let rc = unsafe { kill(pid, 0) };
        if rc == 0 {
            return true;
        }
        // EPERM: the process exists but we cannot signal it.
        std::io::Error::last_os_error().raw_os_error() == Some(1)
    }
    // Windows: a process we can open for query and whose exit code is still
    // STILL_ACTIVE. Without this every record was pruned as dead on Windows.
    #[cfg(windows)]
    {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
            fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
        }
        // SAFETY: a query-only process handle, closed before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code) != 0;
            CloseHandle(handle);
            ok && code == STILL_ACTIVE
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn temp_root(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "gpui-agent-discovery-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    fn listen() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").unwrap()
    }

    fn record(addr: SocketAddr, app: &str, exe: &str, bundle: Option<&str>) -> HostRecord {
        HostRecord {
            addr: addr.to_string(),
            pid: std::process::id(),
            app: app.to_string(),
            executable: exe.to_string(),
            bundle_id: bundle.map(str::to_string),
            connect_id: None,
        }
    }

    /// Every platform, Windows included, sees this process as alive and a
    /// finished child as gone. Windows once answered `false` for everything,
    /// so `load_live` pruned every record there.
    #[test]
    fn pid_alive_sees_this_process_and_not_a_finished_child() {
        assert!(pid_alive(std::process::id()));
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit"])
                .spawn()
                .unwrap()
        } else {
            std::process::Command::new("true").spawn().unwrap()
        };
        let pid = child.id();
        child.wait().unwrap();
        assert!(!pid_alive(pid));
        assert!(!pid_alive(0));
    }

    #[test]
    fn missing_app_names_the_id() {
        let root = temp_root("missing");
        let err = resolve_id(&root, "dev.goldcoders.missing").unwrap_err();
        assert_eq!(
            err,
            ResolveError::Missing {
                id: "dev.goldcoders.missing".into()
            }
        );
        assert!(err.to_string().contains("dev.goldcoders.missing"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn two_hosts_do_not_collide_and_session_sticks() {
        let root = temp_root("two");
        let a = listen();
        let b = listen();
        let addr_a = a.local_addr().unwrap();
        let addr_b = b.local_addr().unwrap();
        let _keep_a = publish_record(
            &root,
            &record(
                addr_a,
                "NativeChat",
                "nativechat",
                Some("dev.example.nativechat"),
            ),
        )
        .unwrap();
        let _keep_b = publish_record(
            &root,
            &record(addr_b, "eBIRForms", "eBIRForms", Some("dev.example.ebir")),
        )
        .unwrap();

        let got_a = resolve_id(&root, "dev.example.nativechat").unwrap();
        let got_b = resolve_id(&root, "eBIRForms").unwrap();
        assert_eq!(got_a.addr, addr_a.to_string());
        assert_eq!(got_b.addr, addr_b.to_string());
        assert_ne!(got_a.addr, got_b.addr);

        let by_exe = resolve_id(&root, "nativechat").unwrap();
        assert_eq!(by_exe.addr, addr_a.to_string());

        let resolved = resolve_client_addr(&root, None, Some("dev.example.ebir")).unwrap();
        assert_eq!(resolved, addr_b);
        let again = resolve_client_addr(&root, None, Some(LAST_SESSION)).unwrap();
        assert_eq!(again, addr_b);

        let explicit = "127.0.0.1:9".parse().unwrap();
        assert_eq!(
            resolve_client_addr(&root, Some(explicit), Some("dev.example.nativechat")).unwrap(),
            explicit
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(windows)]
    fn windows_executable_matching_preserves_logical_id_priority() {
        let root = temp_root("windows-exe");
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let rec = record(addr, "Demo", r"C:\Program Files\Demo\DEMO.EXE", None);
        let _keep = publish_record(&root, &rec).unwrap();
        for id in ["demo", "demo.exe", "DEMO.EXE", r"C:\other\Demo.exe"] {
            assert_eq!(resolve_id(&root, id).unwrap().addr, addr.to_string());
        }
        assert!(!basename_match(&rec, "other.exe"));
        assert!(!basename_match(&rec, ""));
        assert!(!exact_match(&rec, "demo"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn basename_match_is_second_pass() {
        let root = temp_root("base");
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let _keep = publish_record(
            &root,
            &record(
                addr,
                "Demo",
                "/Applications/Demo.app/Contents/MacOS/demo",
                None,
            ),
        )
        .unwrap();
        let got = resolve_id(&root, "demo").unwrap();
        assert_eq!(got.addr, addr.to_string());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dead_pid_and_closed_port_are_dropped() {
        let root = temp_root("stale");
        let dead = HostRecord {
            addr: "127.0.0.1:9".into(),
            pid: i32::MAX as u32,
            app: "gone".into(),
            executable: "gone".into(),
            bundle_id: Some("dev.example.gone".into()),
            connect_id: None,
        };
        let published = publish_record(&root, &dead).unwrap();
        let path = published.path().to_path_buf();
        std::mem::forget(published);
        assert!(path.exists());
        let err = resolve_id(&root, "dev.example.gone").unwrap_err();
        assert!(matches!(err, ResolveError::Missing { .. }));
        assert!(!path.exists());

        let closed = record(
            "127.0.0.1:1".parse().unwrap(),
            "closed",
            "closed",
            Some("dev.example.closed"),
        );
        let published = publish_record(&root, &closed).unwrap();
        let path = published.path().to_path_buf();
        std::mem::forget(published);
        let err = resolve_id(&root, "dev.example.closed").unwrap_err();
        assert!(err.to_string().contains("dev.example.closed"));
        assert!(!path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ambiguous_match_names_both() {
        let root = temp_root("ambig");
        let a = listen();
        let b = listen();
        let _keep_a = publish_record(
            &root,
            &record(
                a.local_addr().unwrap(),
                "Same",
                "one",
                Some("dev.example.same"),
            ),
        )
        .unwrap();
        let _keep_b = publish_record(
            &root,
            &record(
                b.local_addr().unwrap(),
                "Same",
                "two",
                Some("dev.example.same"),
            ),
        )
        .unwrap();
        let err = resolve_id(&root, "dev.example.same").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("dev.example.same"), "{text}");
        assert!(text.contains("more than one"), "{text}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plist_bundle_id_roundtrip() {
        let root = temp_root("plist");
        let plist = root.join("Demo.app").join("Contents").join("Info.plist");
        fs::create_dir_all(plist.parent().unwrap()).unwrap();
        fs::write(
            &plist,
            r#"<?xml version="1.0"?>
            <plist><dict>
            <key>CFBundleIdentifier</key>
            <string>dev.example.demo</string>
            </dict></plist>"#,
        )
        .unwrap();
        let exe = root
            .join("Demo.app")
            .join("Contents")
            .join("MacOS")
            .join("demo");
        assert_eq!(
            bundle_id_from_exe(&exe).as_deref(),
            Some("dev.example.demo")
        );
        let _ = fs::remove_dir_all(root);
    }

    /// One agent's `--connect` is not another agent's: a later command with neither `--addr` nor
    /// `--connect` goes to the default address, not to the app someone else picked last (#48).
    /// Only `--connect last` reuses the saved choice, and the target says which app it is.
    #[test]
    fn a_bare_command_does_not_reuse_another_agents_connect() {
        let root = temp_root("bare");
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let _keep = publish_record(
            &root,
            &record(addr, "eBIRForms", "eBIRForms", Some("dev.example.ebir")),
        )
        .unwrap();

        let picked = resolve_client_target(&root, None, Some("dev.example.ebir")).unwrap();
        assert_eq!(picked.addr, addr);
        assert_eq!(picked.app.as_deref(), Some("eBIRForms"));

        let bare = resolve_client_target(&root, None, None).unwrap();
        assert_eq!(
            bare.addr,
            default_addr(),
            "a bare command must not drive eBIRForms"
        );
        assert_eq!(bare.app, None);

        let last = resolve_client_target(&root, None, Some("last")).unwrap();
        assert_eq!(last.addr, addr);
        assert_eq!(last.app.as_deref(), Some("eBIRForms"));
        assert!(
            last.describe().contains("eBIRForms") && last.describe().contains(&addr.to_string()),
            "{}",
            last.describe()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn last_with_nothing_saved_says_so() {
        let root = temp_root("nolast");
        assert!(matches!(
            resolve_client_target(&root, None, Some(LAST_SESSION)),
            Err(ResolveError::NoSession)
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn named_sessions_roundtrip_list_and_remove() {
        let root = temp_root("named");
        let session = NamedSession {
            name: "work_agent".into(),
            addr: "127.0.0.1:17421".into(),
            token: Some("not-printed".into()),
            allow_remote: true,
            app: Some("NativeChat".into()),
        };
        save_named_session(&root, &session).unwrap();
        assert_eq!(load_named_session(&root, "work_agent").unwrap(), session);

        let another = NamedSession {
            name: "second".into(),
            addr: "127.0.0.1:17422".into(),
            token: None,
            allow_remote: false,
            app: None,
        };
        save_named_session(&root, &another).unwrap();
        assert_eq!(
            list_named_sessions(&root).unwrap(),
            vec![another.clone(), session.clone()]
        );

        assert!(remove_named_session(&root, "work_agent").unwrap());
        assert!(!remove_named_session(&root, "work_agent").unwrap());
        assert_eq!(list_named_sessions(&root).unwrap(), vec![another]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn named_session_names_are_validated() {
        let root = temp_root("bad-name");
        for name in ["../x", "a/b", "", "Work"] {
            let session = NamedSession {
                name: name.to_string(),
                addr: "127.0.0.1:17421".into(),
                token: None,
                allow_remote: false,
                app: None,
            };
            assert_eq!(
                save_named_session(&root, &session).unwrap_err().kind(),
                ErrorKind::InvalidInput
            );
            assert!(matches!(
                load_named_session(&root, name),
                Err(ResolveError::BadSessionName { .. })
            ));
            assert_eq!(
                remove_named_session(&root, name).unwrap_err().kind(),
                ErrorKind::InvalidInput
            );
        }

        let too_long = "x".repeat(65);
        let session = NamedSession {
            name: too_long.clone(),
            addr: "127.0.0.1:17421".into(),
            token: None,
            allow_remote: false,
            app: None,
        };
        assert_eq!(
            save_named_session(&root, &session).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert!(matches!(
            load_named_session(&root, &too_long),
            Err(ResolveError::BadSessionName { .. })
        ));
        assert!(
            load_named_session(&root, "missing")
                .unwrap_err()
                .to_string()
                .contains(
                    "no session named \"missing\"; create it with: gpui-agent --session <name> attach <target>"
                )
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn named_session_storage_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("permissions");
        save_named_session(
            &root,
            &NamedSession {
                name: "private".into(),
                addr: "127.0.0.1:17421".into(),
                token: Some("secret".into()),
                allow_remote: false,
                app: None,
            },
        )
        .unwrap();
        let dir = named_sessions_dir(&root);
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(named_session_path(&root, "private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn named_session_storage_tightens_owned_root_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("insecure-root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            load_named_session(&root, "missing"),
            Err(ResolveError::NoNamedSession { .. })
        ));
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        save_named_session(
            &root,
            &NamedSession {
                name: "private".into(),
                addr: "127.0.0.1:17421".into(),
                token: None,
                allow_remote: false,
                app: None,
            },
        )
        .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_resolution_tightens_owned_runtime_roots() {
        use std::os::unix::fs::PermissionsExt;

        let client_root = temp_root("resolve-client-root");
        fs::set_permissions(&client_root, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve_client_target(&client_root, None, Some(LAST_SESSION)).unwrap_err(),
            ResolveError::NoSession
        );
        assert_eq!(
            fs::metadata(&client_root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _ = fs::remove_dir_all(client_root);

        let id_root = temp_root("resolve-id-root");
        fs::set_permissions(&id_root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            resolve_id(&id_root, "missing"),
            Err(ResolveError::Missing { .. })
        ));
        assert_eq!(
            fs::metadata(&id_root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _ = fs::remove_dir_all(id_root);
    }

    #[cfg(unix)]
    #[test]
    fn named_session_storage_rejects_symlinked_root() {
        use std::os::unix::fs::symlink;

        let target = temp_root("symlink-target");
        let root = temp_root("symlink-root");
        fs::remove_dir_all(&root).unwrap();
        symlink(&target, &root).unwrap();
        let error = save_named_session(
            &root,
            &NamedSession {
                name: "private".into(),
                addr: "127.0.0.1:17421".into(),
                token: None,
                allow_remote: false,
                app: None,
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("real directory"), "{error}");
        let _ = fs::remove_file(root);
        let _ = fs::remove_dir_all(target);
    }

    #[cfg(unix)]
    #[test]
    fn named_session_storage_rejects_group_readable_files() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("readable-file");
        save_named_session(
            &root,
            &NamedSession {
                name: "private".into(),
                addr: "127.0.0.1:17421".into(),
                token: Some("secret".into()),
                allow_remote: false,
                app: None,
            },
        )
        .unwrap();
        let path = named_session_path(&root, "private");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let error = load_named_session(&root, "private").unwrap_err();
        assert!(
            error.to_string().contains("group and other read access"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_addr_beats_connect_without_a_host() {
        let root = temp_root("explicit");
        let addr = "127.0.0.1:9".parse().unwrap();
        assert_eq!(
            resolve_client_addr(&root, Some(addr), Some("missing")).unwrap(),
            addr
        );
        assert_eq!(
            resolve_client_addr(&root, None, None).unwrap(),
            default_addr()
        );
        let _ = fs::remove_dir_all(root);
    }
}
