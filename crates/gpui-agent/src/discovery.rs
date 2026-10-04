//! Find a running AgentHost by bundle id or executable name.
//!
//! Each host writes one JSON record when it starts listening and removes it
//! on shutdown. The CLI matches `--connect <id>` against those records, then
//! remembers the choice so the next command in the same session can omit
//! both `--connect` and `--addr`.

use std::fs;
use std::io::{self, ErrorKind};
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

const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn getuid() -> u32;
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
        unsafe { getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn hosts_dir(root: &Path) -> PathBuf {
    root.join("hosts")
}

fn session_path(root: &Path) -> PathBuf {
    root.join("session.json")
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

/// `--addr` wins. Otherwise `--connect`, then the saved session, then the
/// loopback default.
pub fn resolve_client_addr(
    root: &Path,
    explicit: Option<SocketAddr>,
    connect: Option<&str>,
) -> Result<SocketAddr, ResolveError> {
    if let Some(addr) = explicit {
        return Ok(addr);
    }
    if let Some(id) = connect.map(str::trim).filter(|s| !s.is_empty()) {
        let record = resolve_id(root, id)?;
        save_session(root, &record.addr, id).map_err(|_| ResolveError::BadAddr {
            addr: record.addr.clone(),
        })?;
        return parse_addr(&record.addr);
    }
    if let Some(session) = load_session(root) {
        if tcp_open(&session.addr) {
            return parse_addr(&session.addr);
        }
        if let Ok(record) = resolve_id(root, &session.id) {
            let _ = save_session(root, &record.addr, &session.id);
            return parse_addr(&record.addr);
        }
        let _ = fs::remove_file(session_path(root));
        return Err(ResolveError::Missing { id: session.id });
    }
    Ok(default_addr())
}

pub fn resolve_id(root: &Path, id: &str) -> Result<HostRecord, ResolveError> {
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
    match open.len() {
        0 => Err(ResolveError::Missing { id: id.to_string() }),
        1 => Ok(open.pop().unwrap()),
        _ => Err(ResolveError::Ambiguous {
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
    exec == want || exec == id
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

pub fn save_session(root: &Path, addr: &str, id: &str) -> io::Result<()> {
    fs::create_dir_all(root)?;
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
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    #[cfg(unix)]
    {
        let rc = unsafe { kill(pid as i32, 0) };
        if rc == 0 {
            return true;
        }
        // EPERM: the process exists but we cannot signal it.
        std::io::Error::last_os_error().raw_os_error() == Some(1)
    }
    #[cfg(not(unix))]
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
        let again = resolve_client_addr(&root, None, None).unwrap();
        assert_eq!(again, addr_b);

        let explicit = "127.0.0.1:9".parse().unwrap();
        assert_eq!(
            resolve_client_addr(&root, Some(explicit), Some("dev.example.nativechat")).unwrap(),
            explicit
        );
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
