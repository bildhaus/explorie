use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use explorie_plugin_protocol::{
    ActionEffect, ActionRequest, Contribution, Detail, EntryDecoration, Inspection, Manifest,
    Plugin, PluginAction, unix_time,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use serde_json::Value;

const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

#[derive(Debug)]
struct LocalCertificate(CertificateDer<'static>);
impl ServerCertVerifier for LocalCertificate {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // The selected installation certificate is the trust anchor, including its exact identity.
        if end.as_ref() != self.0.as_ref() {
            return Err(rustls::Error::General(
                "Local Syncthing certificate does not match".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Clone)]
struct Folder {
    id: String,
    path: PathBuf,
}
struct Connection {
    agent: ureq::Agent,
    url: url::Url,
    api_key: String,
    folders: Vec<Folder>,
}

fn discover_config() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(path).join("Syncthing/config.xml"));
    }
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        candidates.push(PathBuf::from(path).join("syncthing/config.xml"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Library/Application Support/Syncthing/config.xml"));
        candidates.push(home.join(".local/state/syncthing/config.xml"));
        candidates.push(home.join(".config/syncthing/config.xml"));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn loopback_url(address: &str, tls: bool) -> Result<url::Url, String> {
    let scheme = if tls { "https" } else { "http" };
    let mut url = url::Url::parse(&format!("{scheme}://{address}"))
        .map_err(|_| "Unsupported Syncthing GUI address")?;
    // Replace localhost with a literal loopback address so DNS cannot move API keys off-device.
    if url.host_str() == Some("localhost") {
        url.set_host(Some("127.0.0.1"))
            .map_err(|_| "Invalid local GUI address")?;
    }
    let local = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if !local
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Only local loopback Syncthing GUI addresses are supported".into());
    }
    Ok(url)
}

fn load_connection(path: &Path) -> Result<Connection, String> {
    let file = std::fs::File::open(path).map_err(|_| "Could not read Syncthing config.xml")?;
    let mut bytes = Vec::new();
    file.take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read Syncthing configuration")?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err("Syncthing configuration exceeds the supported size".into());
    }
    let xml =
        std::str::from_utf8(&bytes).map_err(|_| "Invalid Syncthing configuration encoding")?;
    let document =
        roxmltree::Document::parse(xml).map_err(|_| "Invalid Syncthing configuration XML")?;
    let gui = document
        .descendants()
        .find(|node| node.has_tag_name("gui"))
        .ok_or("Syncthing GUI configuration is missing")?;
    if gui.attribute("enabled") == Some("false") {
        return Err("The Syncthing GUI/API is disabled".into());
    }
    let field = |name| {
        gui.children()
            .find(|node| node.has_tag_name(name))
            .and_then(|node| node.text())
            .unwrap_or("")
    };
    let url = loopback_url(field("address"), gui.attribute("tls") == Some("true"))?;
    let api_key = field("apikey").to_owned();
    if api_key.is_empty() || api_key.contains(['\r', '\n']) {
        return Err("Syncthing API key is missing or invalid".into());
    }
    let mut builder = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(Duration::from_secs(5))
        .timeout_connect(Duration::from_secs(2));
    if url.scheme() == "https" {
        let certificate_path = path
            .parent()
            .ok_or("Configuration directory is missing")?
            .join("https-cert.pem");
        let certificate_file = std::fs::File::open(certificate_path)
            .map_err(|_| "HTTPS requires the installation's local https-cert.pem")?;
        let mut bytes = Vec::new();
        certificate_file
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read the local HTTPS certificate")?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err("Local HTTPS certificate exceeds the supported size".into());
        }
        let certificate = CertificateDer::pem_slice_iter(&bytes)
            .next()
            .ok_or("Local HTTPS certificate is missing")?
            .map_err(|_| "Local HTTPS certificate is invalid")?;
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| "TLS configuration is unsupported")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(LocalCertificate(certificate)))
        .with_no_client_auth();
        builder = builder.tls_config(Arc::new(config));
    }
    let mut folders = Vec::new();
    for node in document
        .root_element()
        .children()
        .filter(|node| node.has_tag_name("folder"))
    {
        let Some(id) = node.attribute("id") else {
            continue;
        };
        let Some(value) = node.attribute("path") else {
            continue;
        };
        let folder_path = if let Some(rest) = value
            .strip_prefix("~/")
            .or_else(|| value.strip_prefix("~\\"))
        {
            dirs::home_dir()
                .map(|home| home.join(rest))
                .unwrap_or_else(|| PathBuf::from(value))
        } else {
            PathBuf::from(value)
        };
        if !folder_path.is_absolute() {
            continue;
        }
        folders.push(Folder {
            id: id.into(),
            path: folder_path,
        });
    }
    Ok(Connection {
        agent: builder.build(),
        url,
        api_key,
        folders,
    })
}

impl Connection {
    fn get(&self, endpoint: &str, query: &[(&str, String)]) -> Result<Value, String> {
        let url = self
            .url
            .join(endpoint)
            .map_err(|_| "Invalid local API endpoint")?;
        let mut request = self.agent.get(url.as_str()).set("X-API-Key", &self.api_key);
        for (key, value) in query {
            request = request.query(key, value);
        }
        let response = request.call().map_err(|error| match error {
            ureq::Error::Status(401 | 403, _) => {
                "Syncthing authentication failed; reconnect using its current config.xml".to_owned()
            }
            ureq::Error::Status(_, _) => "Syncthing API returned an error".to_owned(),
            _ => "Syncthing is unavailable or its local TLS certificate could not be verified"
                .to_owned(),
        })?;
        if response.status() != 200 {
            return Err("Unexpected Syncthing API response; redirects are not followed".into());
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read Syncthing response")?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err("Syncthing response exceeds the supported size".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "Invalid Syncthing API response".into())
    }
}

#[derive(Clone, Default)]
struct LiveStatus {
    state: String,
    pending: u64,
    errors: u64,
    observed: u64,
    unavailable: Option<String>,
    refresh: bool,
}
#[derive(Default)]
struct Shared {
    folders: HashMap<String, LiveStatus>,
    revision: u64,
}
struct Worker {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn snapshot(connection: &Connection, id: &str) -> Result<LiveStatus, String> {
    let encoded: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();
    let config = connection.get(
        &format!("rest/config/folders/{}", encoded.replace('+', "%20")),
        &[],
    )?;
    if config.get("paused").and_then(Value::as_bool) == Some(true) {
        return Ok(LiveStatus {
            state: "paused".into(),
            observed: unix_time(),
            ..Default::default()
        });
    }
    let value = connection.get("rest/db/status", &[("folder", id.into())])?;
    Ok(summary(&value))
}
fn summary(value: &Value) -> LiveStatus {
    LiveStatus {
        state: value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .into(),
        pending: value
            .get("needTotalItems")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        errors: value.get("pullErrors").and_then(Value::as_u64).unwrap_or(0),
        observed: unix_time(),
        ..Default::default()
    }
}

fn unavailable(state: &Mutex<Shared>, error: String) {
    if let Ok(mut state) = state.lock() {
        for status in state.folders.values_mut() {
            status.unavailable = Some(error.clone());
        }
        state.revision += 1;
    }
}
fn backoff(cancelled: &AtomicBool) {
    for _ in 0..20 {
        if cancelled.load(Ordering::Acquire) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn start_worker(connection: Connection) -> Worker {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let state = shared.clone();
    let cancelled = stop.clone();
    std::thread::spawn(move || {
        let mut since = 0;
        let mut reconnect = true;
        while !cancelled.load(Ordering::Acquire) {
            if reconnect {
                // Establish a cursor before the snapshot: historical events must not overwrite
                // fresh state, and restart/reset event IDs must not stall the subscription.
                match connection
                    .get(
                        "rest/events",
                        &[
                            ("since", "0".into()),
                            ("limit", "1".into()),
                            ("timeout", "1".into()),
                        ],
                    )
                    .and_then(|value| {
                        value
                            .as_array()
                            .cloned()
                            .ok_or("Invalid Syncthing event response".into())
                    }) {
                    Ok(events) => {
                        since = events
                            .last()
                            .and_then(|event| event.get("id"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    }
                    Err(error) => {
                        unavailable(&state, error);
                        backoff(&cancelled);
                        continue;
                    }
                }
            }

            let ids = match state.lock() {
                Ok(state) => state
                    .folders
                    .iter()
                    .filter(|(_, value)| reconnect || value.refresh || value.observed == 0)
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>(),
                Err(_) => break,
            };
            for id in ids {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                let status = snapshot(&connection, &id).unwrap_or_else(|error| LiveStatus {
                    unavailable: Some(error),
                    observed: unix_time(),
                    ..Default::default()
                });
                if let Ok(mut state) = state.lock() {
                    if let Some(current) = state.folders.get_mut(&id) {
                        *current = status;
                    }
                    state.revision += 1;
                }
            }
            reconnect = false;
            let events = connection
                .get(
                    "rest/events",
                    &[("since", since.to_string()), ("timeout", "1".into())],
                )
                .and_then(|value| {
                    value
                        .as_array()
                        .cloned()
                        .ok_or("Invalid Syncthing event response".into())
                });
            match events {
                Ok(events) => {
                    for event in events {
                        let id = event.get("id").and_then(Value::as_u64).unwrap_or(0);
                        if since > 0 && id != since + 1 {
                            reconnect = true;
                        }
                        since = id;
                        let data = &event["data"];
                        let folder = data.get("folder").and_then(Value::as_str).unwrap_or("");
                        if let Ok(mut state) = state.lock()
                            && let Some(status) = state.folders.get_mut(folder)
                        {
                            match event.get("type").and_then(Value::as_str).unwrap_or("") {
                                "FolderSummary" => {
                                    let prior_state = status.state.clone();
                                    *status = summary(&data["summary"]);
                                    if status.state == "unknown" {
                                        status.state = prior_state;
                                    }
                                }
                                "StateChanged" => {
                                    status.state = data
                                        .get("to")
                                        .and_then(Value::as_str)
                                        .unwrap_or("unknown")
                                        .into();
                                    status.observed = unix_time();
                                    status.unavailable = None;
                                }
                                "FolderErrors" => {
                                    status.errors = data
                                        .get("errors")
                                        .and_then(Value::as_array)
                                        .map_or(0, |errors| errors.len() as u64);
                                    status.observed = unix_time();
                                }
                                "FolderPaused" => {
                                    status.state = "paused".into();
                                    status.observed = unix_time();
                                }
                                "FolderResumed" => status.refresh = true,
                                _ => {}
                            }
                            state.revision += 1;
                        }
                    }
                }
                Err(error) => {
                    unavailable(&state, error);
                    reconnect = true;
                    backoff(&cancelled);
                }
            }
        }
    });
    Worker { shared, stop }
}

#[derive(Default)]
pub struct SyncthingPlugin {
    folders: Vec<Folder>,
    url: Option<String>,
    worker: Option<Worker>,
    configuration_error: Option<String>,
    contexts: Vec<Inspection>,
    pending: Vec<Contribution>,
    revision: u64,
}

fn marker_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.join(".stfolder").exists())
        .map(Path::to_path_buf)
}
fn conflict(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().contains(".sync-conflict-"))
}

impl SyncthingPlugin {
    fn contribution(&self, context: &Inspection) -> Contribution {
        let mut result = Contribution::empty(context);
        let folder = self
            .folders
            .iter()
            .filter(|folder| context.path.starts_with(&folder.path))
            .max_by_key(|folder| folder.path.components().count());
        let root = folder
            .map(|folder| folder.path.clone())
            .or_else(|| marker_root(&context.path));
        let Some(root) = root else { return result };
        result.root = Some(root);
        result.badge = Some("Syncthing".into());
        result.details.push(Detail {
            label: "Detection".into(),
            value: "Syncthing folder detected".into(),
        });
        let conflicts: Vec<_> = context
            .entries
            .iter()
            .filter(|entry| !entry.is_dir && conflict(&entry.path))
            .collect();
        result.details.push(Detail {
            label: "Conflict copies in this listing".into(),
            value: conflicts.len().to_string(),
        });
        result.decorations = conflicts
            .iter()
            .map(|entry| EntryDecoration {
                path: entry.path.clone(),
                label: "Syncthing conflict copy".into(),
            })
            .collect();
        if let Some(error) = &self.configuration_error {
            result.details.push(Detail {
                label: "Connection".into(),
                value: error.clone(),
            });
        } else if let (Some(folder), Some(worker)) = (folder, &self.worker) {
            if let Ok(state) = worker.shared.lock()
                && let Some(status) = state.folders.get(&folder.id)
            {
                let value = if let Some(error) = &status.unavailable {
                    error.clone()
                } else if status.observed == 0 {
                    "Connecting…".into()
                } else if status.errors > 0 {
                    format!("{} sync errors", status.errors)
                } else {
                    match status.state.as_str() {
                        "idle" => "Locally idle (peer status not checked)",
                        "scanning" | "scan-waiting" => "Scanning",
                        "syncing" | "sync-waiting" => "Syncing",
                        "paused" => "Paused",
                        "error" => "Error",
                        other => other,
                    }
                    .to_string()
                };
                result.details.push(Detail {
                    label: "Local state".into(),
                    value,
                });
                result.details.push(Detail {
                    label: "Pending local items".into(),
                    value: status.pending.to_string(),
                });
                if status.observed > 0 {
                    result.observed_at = status.observed;
                    result.details.push(Detail {
                        label: "Last updated".into(),
                        value: format!(
                            "{} seconds ago",
                            unix_time().saturating_sub(status.observed)
                        ),
                    });
                }
            }
        } else {
            result.details.push(Detail {
                label: "Connection".into(),
                value: if self.worker.is_some() {
                    "Folder is not in the selected Syncthing configuration"
                } else {
                    "Not connected; configure the integration for live status"
                }
                .into(),
            });
        }
        result.actions.push(PluginAction {
            id: "refresh".into(),
            label: "Refresh".into(),
        });
        if self.url.is_some() {
            result.actions.push(PluginAction {
                id: "open-ui".into(),
                label: "Open Syncthing UI".into(),
            });
        }
        result
    }
}

impl Plugin for SyncthingPlugin {
    fn manifest(&self) -> Manifest {
        serde_json::from_str(include_str!("../plugin.json")).expect("valid bundled manifest")
    }
    fn configure(&mut self, value: Value) -> Result<(), String> {
        self.worker = None;
        self.folders.clear();
        self.url = None;
        self.configuration_error = None;
        self.contexts.clear();
        self.pending.clear();
        self.revision = 0;
        if value.get("connected").and_then(Value::as_bool) != Some(true) {
            return Ok(());
        }
        let configured = value
            .get("configPath")
            .and_then(Value::as_str)
            .unwrap_or("");
        let path = if configured.is_empty() {
            discover_config()
        } else {
            Some(PathBuf::from(configured))
        };
        let result = path
            .ok_or_else(|| "Syncthing config.xml was not found; select it in Configure".to_string())
            .and_then(|path| load_connection(&path));
        match result {
            Ok(connection) => {
                self.folders = connection.folders.clone();
                self.url = Some(connection.url.to_string());
                self.worker = Some(start_worker(connection));
                Ok(())
            }
            Err(error) => {
                self.configuration_error = Some(error.clone());
                Err(error)
            }
        }
    }
    fn inspect(&mut self, context: Inspection) -> Result<Contribution, String> {
        if let Some(worker) = &self.worker
            && let Some(folder) = self
                .folders
                .iter()
                .filter(|folder| context.path.starts_with(&folder.path))
                .max_by_key(|folder| folder.path.components().count())
            && let Ok(mut state) = worker.shared.lock()
        {
            let status = state.folders.entry(folder.id.clone()).or_default();
            if context.force {
                status.refresh = true;
            }
        }
        self.contexts
            .retain(|prior| prior.context_id != context.context_id);
        if self.contexts.len() >= 32 {
            self.contexts.remove(0);
        }
        self.contexts.push(context.clone());
        self.pending
            .retain(|prior| prior.context_id != context.context_id);
        if let Some(worker) = &self.worker
            && let Ok(mut state) = worker.shared.lock()
        {
            state.folders.retain(|id, _| {
                self.folders.iter().any(|folder| {
                    &folder.id == id
                        && self
                            .contexts
                            .iter()
                            .any(|context| context.path.starts_with(&folder.path))
                })
            });
        }
        Ok(self.contribution(&context))
    }
    fn invoke(&mut self, request: ActionRequest) -> Result<ActionEffect, String> {
        match request.action_id.as_str() {
            "open-ui" => self
                .url
                .clone()
                .map(ActionEffect::OpenUrl)
                .ok_or("Connect to local Syncthing first".into()),
            "refresh" => {
                if let Some(worker) = &self.worker
                    && let Ok(mut state) = worker.shared.lock()
                {
                    for value in state.folders.values_mut() {
                        value.refresh = true;
                    }
                }
                Ok(ActionEffect::None)
            }
            _ => Err("Unknown Syncthing action".into()),
        }
    }
    fn poll(&mut self) -> Option<Contribution> {
        let revision = self
            .worker
            .as_ref()
            .and_then(|worker| worker.shared.lock().ok().map(|state| state.revision))
            .unwrap_or(0);
        if revision != self.revision {
            self.revision = revision;
            for context in &self.contexts {
                let contribution = self.contribution(context);
                if let Some(pending) = self
                    .pending
                    .iter_mut()
                    .find(|prior| prior.context_id == contribution.context_id)
                {
                    *pending = contribution;
                } else {
                    self.pending.insert(0, contribution);
                }
            }
        }
        self.pending.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorie_plugin_protocol::EntryContext;
    use serde_json::json;
    #[test]
    fn localhost_https_authenticates_exact_pin_before_sending_api_key() {
        use std::io::Write;
        use std::time::Instant;
        const CERTIFICATE: &[u8] = include_bytes!("../tests/fixtures/localhost-cert.pem");
        const WRONG_CERTIFICATE: &[u8] = include_bytes!("../tests/fixtures/wrong-cert.pem");
        const PRIVATE_KEY: &[u8] = include_bytes!("../tests/fixtures/localhost-key.pem");
        for (selected_certificate, should_connect) in
            [(CERTIFICATE, true), (WRONG_CERTIFICATE, false)]
        {
            let certificate = CertificateDer::pem_slice_iter(CERTIFICATE)
                .next()
                .unwrap()
                .unwrap();
            let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(PRIVATE_KEY).unwrap();
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let stream = loop {
                    if let Ok((stream, _)) = listener.accept() {
                        break stream;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "HTTPS client must connect to the local listener"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let session = rustls::ServerConnection::new(Arc::new(config)).unwrap();
                let mut stream = rustls::StreamOwned::new(session, stream);
                let mut buffer = [0; 4096];
                match stream.read(&mut buffer) {
                    Ok(size) if size > 0 => {
                        let request = String::from_utf8_lossy(&buffer[..size]);
                        assert!(request.starts_with("GET /rest/db/status "));
                        assert!(
                            request
                                .to_ascii_lowercase()
                                .contains("x-api-key: public-fixture-token")
                        );
                        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\n{\"state\":\"idle\"}").unwrap();
                        stream.flush().unwrap();
                        true
                    }
                    Err(error) if should_connect => {
                        panic!("Expected TLS fixture handshake: {error:?}")
                    }
                    _ => false,
                }
            });
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(
                directory.path().join("https-cert.pem"),
                selected_certificate,
            )
            .unwrap();
            let path = directory.path().join("config.xml");
            std::fs::write(&path,format!("<configuration><gui enabled=\"true\" tls=\"true\"><address>{address}</address><apikey>public-fixture-token</apikey></gui></configuration>")).unwrap();
            let connection = load_connection(&path).unwrap();
            let result = connection.get("rest/db/status", &[]);
            assert_eq!(
                server.join().unwrap(),
                should_connect,
                "API key must never reach a server whose certificate differs from the selected pin"
            );
            if should_connect {
                assert_eq!(result.unwrap()["state"], "idle");
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .contains("certificate could not be verified")
                );
            }
        }
    }
    #[test]
    fn paused_snapshot_uses_live_configuration_without_reading_database_status() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0; 2048];
            let size = stream.read(&mut bytes).unwrap();
            assert!(
                String::from_utf8_lossy(&bytes[..size])
                    .starts_with("GET /rest/config/folders/fixture ")
            );
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"paused\":true}").unwrap();
        });
        let connection = Connection {
            agent: ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .timeout(Duration::from_secs(2))
                .build(),
            url: loopback_url(&address.to_string(), false).unwrap(),
            api_key: "fixture".into(),
            folders: vec![],
        };
        assert_eq!(snapshot(&connection, "fixture").unwrap().state, "paused");
        server.join().unwrap();
    }
    #[test]
    fn notifications_keep_same_folder_windows_and_replace_only_their_own_context() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".stfolder")).unwrap();
        let mut plugin = SyncthingPlugin {
            worker: Some(Worker {
                shared: Arc::new(Mutex::new(Shared {
                    revision: 1,
                    ..Default::default()
                })),
                stop: Arc::new(AtomicBool::new(false)),
            }),
            ..Default::default()
        };
        for (context_id, generation) in [(1, 10), (2, 20), (1, 11)] {
            plugin
                .inspect(Inspection {
                    path: temp.path().into(),
                    context_id,
                    generation,
                    ..Default::default()
                })
                .unwrap();
        }
        assert_eq!(plugin.contexts.len(), 2);
        let mut updates = Vec::new();
        while let Some(value) = plugin.poll() {
            updates.push((value.context_id, value.generation));
        }
        updates.sort_unstable();
        assert_eq!(updates, [(1, 11), (2, 20)]);
    }
    #[test]
    fn tls_pin_accepts_only_the_selected_installation_certificate() {
        let verifier = LocalCertificate(CertificateDer::from(vec![1, 2, 3]));
        let hostname = ServerName::try_from("localhost").unwrap();
        assert!(
            verifier
                .verify_server_cert(
                    &CertificateDer::from(vec![1, 2, 3]),
                    &[],
                    &hostname,
                    &[],
                    UnixTime::now()
                )
                .is_ok()
        );
        assert!(
            verifier
                .verify_server_cert(
                    &CertificateDer::from(vec![1, 2, 4]),
                    &[],
                    &hostname,
                    &[],
                    UnixTime::now()
                )
                .is_err()
        );
        assert!(!verifier.supported_verify_schemes().is_empty());
    }

    #[test]
    fn events_reconcile_gaps_and_restart_without_repeated_status_polling() {
        use std::io::Write;
        use std::sync::atomic::AtomicUsize;
        use std::time::Instant;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stop = stopped.clone();
        let cursors = Arc::new(AtomicUsize::new(0));
        let server_cursors = cursors.clone();
        let snapshots = Arc::new(AtomicUsize::new(0));
        let server_snapshots = snapshots.clone();
        let server = std::thread::spawn(move || {
            let mut event_calls = 0;
            while !server_stop.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut bytes = [0; 4096];
                let size = stream.read(&mut bytes).unwrap_or(0);
                let request = String::from_utf8_lossy(&bytes[..size]);
                let (status, body) = if request.contains("limit=1") {
                    let cursor = server_cursors.fetch_add(1, Ordering::AcqRel);
                    (200, format!("[{{\"id\":{}}}]", [10, 14, 1][cursor.min(2)]))
                } else if request.contains("/rest/config/folders/") {
                    (200, "{\"paused\":false}".into())
                } else if request.contains("/rest/db/status") {
                    server_snapshots.fetch_add(1, Ordering::AcqRel);
                    let count = server_cursors.load(Ordering::Acquire);
                    (
                        200,
                        format!(
                            "{{\"state\":\"idle\",\"needTotalItems\":{}}}",
                            [3, 9, 7][count.saturating_sub(1).min(2)]
                        ),
                    )
                } else {
                    event_calls += 1;
                    match event_calls {
                        1 => (200, "[{\"id\":11,\"type\":\"FolderSummary\",\"data\":{\"folder\":\"fixture\",\"summary\":{\"needTotalItems\":2}}}]".into()),
                        2 => (200, "[{\"id\":14,\"type\":\"StateChanged\",\"data\":{\"folder\":\"fixture\",\"to\":\"scanning\"}}]".into()),
                        3 => (503, "{}".into()),
                        _ => {std::thread::sleep(Duration::from_millis(50));(200, "[]".into())}
                    }
                };
                let response = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        let connection = Connection {
            agent: ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .timeout(Duration::from_secs(2))
                .build(),
            url: loopback_url(&address.to_string(), false).unwrap(),
            api_key: "fixture".into(),
            folders: vec![],
        };
        let worker = start_worker(connection);
        worker
            .shared
            .lock()
            .unwrap()
            .folders
            .insert("fixture".into(), LiveStatus::default());
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut ready = false;
        while Instant::now() < deadline {
            let state = worker.shared.lock().unwrap();
            if state.folders["fixture"].pending == 7
                && state.folders["fixture"].unavailable.is_none()
            {
                ready = true;
                break;
            }
            drop(state);
            std::thread::sleep(Duration::from_millis(10));
        }
        let snapshot_count = snapshots.load(Ordering::Acquire);
        worker.stop.store(true, Ordering::Release);
        stopped.store(true, Ordering::Release);
        server.join().unwrap();
        assert!(
            ready,
            "event cursor gap and server restart must reconcile to the latest snapshot"
        );
        assert_eq!(cursors.load(Ordering::Acquire), 3);
        assert_eq!(snapshot_count, 3);
    }
    #[test]
    fn marker_only_never_connects_and_counts_only_current_listing() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".stfolder")).unwrap();
        let child = temp.path().join("nested");
        std::fs::create_dir(&child).unwrap();
        let mut plugin = SyncthingPlugin::default();
        plugin
            .configure(json!({"connected":false,"configPath":"missing"}))
            .unwrap();
        let result = plugin
            .inspect(Inspection {
                path: child.clone(),
                entries: vec![EntryContext {
                    path: child.join("file.sync-conflict-20260101-1234.md"),
                    is_dir: false,
                }],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.root.as_deref(), Some(temp.path()));
        assert_eq!(result.decorations.len(), 1);
        assert!(plugin.worker.is_none());
    }
    #[test]
    fn remote_wildcard_and_credential_addresses_are_rejected() {
        for address in [
            "0.0.0.0:8384",
            "[::]:8384",
            "192.168.1.2:8384",
            "example.com:8384",
            "user@127.0.0.1:8384",
            "127.0.0.1:8384/path",
        ] {
            assert!(loopback_url(address, false).is_err(), "{address}");
        }
        assert!(loopback_url("[::1]:8384", true).is_ok());
        assert_eq!(
            loopback_url("localhost:8384", false).unwrap().host_str(),
            Some("127.0.0.1")
        );
    }
    #[test]
    fn selected_configuration_recognizes_custom_markers_and_paused_folders() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.xml");
        let root = temp.path().to_string_lossy().replace('&', "&amp;");
        std::fs::write(&config,format!("<configuration><folder id=\"custom\" path=\"{root}\"><markerName>.custom</markerName><paused>true</paused></folder><gui enabled=\"true\" tls=\"false\"><address>127.0.0.1:1</address><apikey>fixture-secret</apikey></gui></configuration>")).unwrap();
        let connection = load_connection(&config).unwrap();
        assert_eq!(connection.folders[0].id, "custom");
        let mut plugin = SyncthingPlugin {
            folders: connection.folders.clone(),
            ..Default::default()
        };
        assert!(
            plugin
                .inspect(Inspection {
                    path: temp.path().into(),
                    ..Default::default()
                })
                .unwrap()
                .badge
                .is_some()
        );
    }
    #[test]
    fn api_failures_are_redacted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            use std::io::Write;
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 2048];
            let _ = stream.read(&mut buffer);
            stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 14\r\nConnection: close\r\n\r\nfixture-secret").unwrap();
        });
        let connection = Connection {
            agent: ureq::AgentBuilder::new().try_proxy_from_env(false).build(),
            url: loopback_url(&address.to_string(), false).unwrap(),
            api_key: "fixture-secret".into(),
            folders: vec![],
        };
        let error = connection.get("rest/db/status", &[]).unwrap_err();
        assert!(error.contains("authentication"));
        assert!(!error.contains("fixture-secret"));
        server.join().unwrap();
    }
    #[test]
    fn unavailable_and_pending_summary_are_honest() {
        let value = summary(&json!({"state":"idle","needTotalItems":4,"pullErrors":2}));
        assert_eq!((value.pending, value.errors), (4, 2));
        let connection = Connection {
            agent: ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .timeout(Duration::from_millis(100))
                .build(),
            url: loopback_url("127.0.0.1:1", false).unwrap(),
            api_key: "secret".into(),
            folders: vec![],
        };
        assert!(
            connection
                .get("rest/events", &[])
                .unwrap_err()
                .contains("unavailable")
        );
    }
}
