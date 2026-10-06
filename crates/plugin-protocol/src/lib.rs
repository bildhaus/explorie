//! Explorie's public integration contract. Plugins are trusted executables, not a sandbox.
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol_version: u32,
    pub description: String,
    pub executables: BTreeMap<String, String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub settings: Vec<SettingDescriptor>,
}

impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err("This plugin requires a different Explorie protocol version".into());
        }
        if self.id.is_empty()
            || self.id.len() > 80
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("Invalid plugin identity".into());
        }
        if self.name.is_empty() || self.version.is_empty() {
            return Err("Plugin name and version are required".into());
        }
        for executable in self.executables.values() {
            if executable.is_empty()
                || executable.contains(['/', '\\', ':'])
                || executable == "."
                || executable == ".."
            {
                return Err("Plugin executables must be package-root filenames".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingDescriptor {
    pub key: String,
    pub label: String,
    pub kind: SettingKind,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SettingKind {
    Text,
    File,
    Directory,
    Boolean,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub manifest: Manifest,
    pub target: String,
    pub asset_url: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub configuration: Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    #[serde(default)]
    pub context_id: u64,
    pub path: PathBuf,
    #[serde(default)]
    pub entries: Vec<EntryContext>,
    #[serde(default)]
    pub selected: Vec<PathBuf>,
    pub generation: u64,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryContext {
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contribution {
    #[serde(default)]
    pub context_id: u64,
    pub path: PathBuf,
    pub generation: u64,
    pub root: Option<PathBuf>,
    pub badge: Option<String>,
    #[serde(default)]
    pub details: Vec<Detail>,
    #[serde(default)]
    pub decorations: Vec<EntryDecoration>,
    #[serde(default)]
    pub actions: Vec<PluginAction>,
    #[serde(default)]
    pub observed_at: u64,
}

impl Contribution {
    pub fn empty(context: &Inspection) -> Self {
        Self {
            context_id: context.context_id,
            path: context.path.clone(),
            generation: context.generation,
            observed_at: unix_time(),
            ..Self::default()
        }
    }
}

pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Detail {
    pub label: String,
    pub value: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EntryDecoration {
    pub path: PathBuf,
    pub label: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginAction {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionRequest {
    pub action_id: String,
    pub context: Inspection,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum ActionEffect {
    #[default]
    None,
    OpenUrl(String),
    CopyText(String),
}

/// stdout is exclusively JSON-RPC. Never put credentials in errors or diagnostics.
pub trait Plugin {
    fn manifest(&self) -> Manifest;
    fn configure(&mut self, configuration: Value) -> Result<(), String>;
    fn inspect(&mut self, context: Inspection) -> Result<Contribution, String>;
    fn invoke(&mut self, request: ActionRequest) -> Result<ActionEffect, String>;
    /// Optional coalesced background updates, retaining the original navigation generation.
    fn poll(&mut self) -> Option<Contribution> {
        None
    }
}

pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unterminated plugin frame",
                ))
            };
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        if bytes.len() + count > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Plugin frame exceeds limit",
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if bytes.last() == Some(&b'\n') {
            return serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Malformed plugin frame"));
        }
    }
}

pub fn write_frame(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() + 1 > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Plugin frame exceeds limit",
        ));
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Single-threaded plugin state with a bounded input channel and periodic notifications.
pub fn run_stdio(mut plugin: impl Plugin) -> io::Result<()> {
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        while let Ok(Some(frame)) = read_frame(&mut reader) {
            if tx.send(frame).is_err() {
                break;
            }
        }
    });
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(request) => {
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                let params = request.get("params").cloned().unwrap_or(Value::Null);
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                let result: Result<Value, String> =
                    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                        Err("Expected JSON-RPC 2.0".into())
                    } else {
                        match method {
                            "initialize" => {
                                if params.get("protocolVersion").and_then(Value::as_u64)
                                    != Some(u64::from(PROTOCOL_VERSION))
                                {
                                    Err("Incompatible protocol version".into())
                                } else {
                                    serde_json::to_value(plugin.manifest())
                                        .map_err(|e| e.to_string())
                                }
                            }
                            "configure" => plugin.configure(params).map(|()| Value::Null),
                            "inspect" => serde_json::from_value(params)
                                .map_err(|e| e.to_string())
                                .and_then(|context| plugin.inspect(context))
                                .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
                            "invoke" => serde_json::from_value(params)
                                .map_err(|e| e.to_string())
                                .and_then(|context| plugin.invoke(context))
                                .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
                            "shutdown" => break,
                            _ => Err("Unknown plugin method".into()),
                        }
                    };
                let response = match result {
                    Ok(result) => json!({"jsonrpc":"2.0", "id":id, "result":result}),
                    Err(message) => {
                        json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32000,"message":message}})
                    }
                };
                write_frame(&mut writer, &response)?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(contribution) = plugin.poll() {
            write_frame(
                &mut writer,
                &json!({"jsonrpc":"2.0", "method":"statusChanged", "params":contribution}),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_rejects_truncated_and_malformed_messages() {
        assert!(read_frame(&mut io::Cursor::new(b"{}")).is_err());
        assert!(read_frame(&mut io::Cursor::new(b"bad\n")).is_err());
        assert!(read_frame(&mut io::Cursor::new(b"{}\n")).unwrap().is_some());
    }
}
