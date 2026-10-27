use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{EntryFilter, SortDirection, SortKey, ViewMode};

const WORKSPACE_VERSION: u32 = 2;
const WORKSPACE_FILE: &str = "workspaces-v1.json";
const MAX_WORKSPACES: usize = 1_000;
const MAX_TABS: usize = 100;
static ID_COUNTER: AtomicU64 = AtomicU64::new(1);
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceTab {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceWindowState {
    pub(crate) width: Option<f32>,
    pub(crate) height: Option<f32>,
    pub(crate) x: Option<f32>,
    pub(crate) y: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Workspace {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) tabs: Vec<WorkspaceTab>,
    pub(crate) active_tab_id: String,
    pub(crate) view_mode: ViewMode,
    pub(crate) sort_key: SortKey,
    pub(crate) sort_direction: SortDirection,
    pub(crate) show_hidden: bool,
    pub(crate) filter_mode: EntryFilter,
    pub(crate) show_preview_panel: bool,
    pub(crate) grid_min_width: u16,
    #[serde(default)]
    pub(crate) window: WorkspaceWindowState,
    #[serde(default = "default_sidebar_width")]
    pub(crate) sidebar_width: f32,
    #[serde(default = "default_preview_panel_width")]
    pub(crate) preview_panel_width: f32,
    #[serde(default)]
    pub(crate) sidebar_collapsed: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) tabs: Vec<WorkspaceTab>,
    pub(crate) active_tab_id: String,
    pub(crate) view_mode: ViewMode,
    pub(crate) sort_key: SortKey,
    pub(crate) sort_direction: SortDirection,
    pub(crate) show_hidden: bool,
    pub(crate) filter_mode: EntryFilter,
    pub(crate) show_preview_panel: bool,
    pub(crate) grid_min_width: u16,
    pub(crate) window: WorkspaceWindowState,
    pub(crate) sidebar_width: f32,
    pub(crate) preview_panel_width: f32,
    pub(crate) sidebar_collapsed: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceState {
    version: u32,
    workspaces: Vec<Workspace>,
    last_workspace_id: Option<String>,
}

impl WorkspaceState {
    pub(crate) fn empty() -> Self {
        Self {
            version: WORKSPACE_VERSION,
            workspaces: Vec::new(),
            last_workspace_id: None,
        }
    }

    pub(crate) fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }

    pub(crate) fn last_workspace_id(&self) -> Option<&str> {
        self.last_workspace_id.as_deref()
    }

    pub(crate) fn get(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }

    pub(crate) fn save_current(
        &mut self,
        name: &str,
        snapshot: WorkspaceSnapshot,
    ) -> Result<String, String> {
        let name = valid_name(name)?;
        if snapshot.tabs.is_empty() {
            return Err("cannot save a workspace without tabs".to_string());
        }
        let now = unix_ms();
        let id = next_id(now);
        let workspace = Workspace {
            id: id.clone(),
            name,
            created_at: now,
            updated_at: now,
            tabs: snapshot.tabs,
            active_tab_id: snapshot.active_tab_id,
            view_mode: snapshot.view_mode,
            sort_key: snapshot.sort_key,
            sort_direction: snapshot.sort_direction,
            show_hidden: snapshot.show_hidden,
            filter_mode: snapshot.filter_mode,
            show_preview_panel: snapshot.show_preview_panel,
            grid_min_width: snapshot.grid_min_width.clamp(120, 260),
            window: sanitize_window(snapshot.window),
            sidebar_width: snapshot.sidebar_width.clamp(140.0, 480.0),
            preview_panel_width: snapshot.preview_panel_width.clamp(280.0, 640.0),
            sidebar_collapsed: snapshot.sidebar_collapsed,
        };
        self.workspaces.push(workspace);
        self.workspaces
            .sort_by_key(|workspace| std::cmp::Reverse(workspace.updated_at));
        self.workspaces.truncate(MAX_WORKSPACES);
        self.last_workspace_id = Some(id.clone());
        Ok(id)
    }

    pub(crate) fn rename(&mut self, id: &str, name: &str) -> Result<(), String> {
        let name = valid_name(name)?;
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == id)
            .ok_or_else(|| "workspace no longer exists".to_string())?;
        workspace.name = name;
        workspace.updated_at = unix_ms();
        self.workspaces
            .sort_by_key(|workspace| std::cmp::Reverse(workspace.updated_at));
        Ok(())
    }

    pub(crate) fn delete(&mut self, id: &str) -> bool {
        let before = self.workspaces.len();
        self.workspaces.retain(|workspace| workspace.id != id);
        if self.last_workspace_id.as_deref() == Some(id) {
            self.last_workspace_id = None;
        }
        self.workspaces.len() != before
    }

    pub(crate) fn mark_loaded(&mut self, id: &str) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        self.last_workspace_id = Some(id.to_string());
        true
    }

    pub(crate) fn export_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.workspaces)
            .map(|json| format!("{json}\n"))
            .map_err(|error| error.to_string())
    }

    pub(crate) fn import_json(&mut self, json: &str) -> Result<usize, String> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|error| error.to_string())?;
        let values = match value {
            serde_json::Value::Array(values) => values,
            value @ serde_json::Value::Object(_) => vec![value],
            _ => return Err("workspace import must be an object or array".to_string()),
        };
        let now = unix_ms();
        let mut imported = 0;
        for value in values {
            if self.workspaces.len() == MAX_WORKSPACES {
                break;
            }
            let generated_id = next_id(now + imported as u64);
            let native = serde_json::from_value::<Workspace>(value.clone())
                .ok()
                .and_then(|mut workspace| {
                    validate_workspace(&mut workspace).ok()?;
                    Some(workspace)
                });
            if let Some(mut workspace) =
                native.or_else(|| legacy_workspace(&value, &generated_id, now))
            {
                workspace.id = generated_id;
                workspace.created_at = now;
                workspace.updated_at = now;
                self.workspaces.push(workspace);
                imported += 1;
            }
        }
        if imported == 0 {
            return Err("no valid workspaces found".to_string());
        }
        self.workspaces
            .sort_by_key(|workspace| std::cmp::Reverse(workspace.updated_at));
        Ok(imported)
    }

    fn validate(mut self) -> Result<Self, String> {
        if self.version != WORKSPACE_VERSION {
            return Err(format!(
                "unsupported workspace version {}; expected {WORKSPACE_VERSION}",
                self.version
            ));
        }
        if self.workspaces.len() > MAX_WORKSPACES {
            return Err(format!("workspace count exceeds {MAX_WORKSPACES}"));
        }
        let mut ids = HashSet::with_capacity(self.workspaces.len());
        for workspace in &mut self.workspaces {
            validate_workspace(workspace)?;
            if !ids.insert(workspace.id.clone()) {
                return Err("workspace identifiers are not unique".to_string());
            }
        }
        self.workspaces
            .sort_by_key(|workspace| std::cmp::Reverse(workspace.updated_at));
        if self
            .last_workspace_id
            .as_deref()
            .is_some_and(|id| !ids.contains(id))
        {
            self.last_workspace_id = None;
        }
        Ok(self)
    }
}

fn default_sidebar_width() -> f32 {
    190.0
}

fn default_preview_panel_width() -> f32 {
    360.0
}

fn next_id(now: u64) -> String {
    format!(
        "workspace-{now}-{}",
        ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn valid_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.contains('\0') {
        return Err("workspace name cannot be empty".to_string());
    }
    if name.chars().count() > 120 {
        return Err("workspace name is longer than 120 characters".to_string());
    }
    Ok(name.to_string())
}

fn validate_workspace(workspace: &mut Workspace) -> Result<(), String> {
    if workspace.id.is_empty() || workspace.id.contains('\0') {
        return Err("workspace identifier is invalid".to_string());
    }
    workspace.name = valid_name(&workspace.name)?;
    if workspace.tabs.is_empty() || workspace.tabs.len() > MAX_TABS {
        return Err(format!("workspace must contain 1 to {MAX_TABS} tabs"));
    }
    let mut tab_ids = HashSet::with_capacity(workspace.tabs.len());
    for tab in &workspace.tabs {
        if tab.id.is_empty()
            || tab.id.contains('\0')
            || tab.path.as_os_str().is_empty()
            || tab.path.to_string_lossy().contains('\0')
            || !tab_ids.insert(tab.id.clone())
        {
            return Err("workspace contains an invalid tab".to_string());
        }
    }
    if !tab_ids.contains(&workspace.active_tab_id) {
        workspace.active_tab_id = workspace.tabs[0].id.clone();
    }
    workspace.grid_min_width = workspace.grid_min_width.clamp(120, 260);
    workspace.window = sanitize_window(workspace.window);
    workspace.sidebar_width = workspace.sidebar_width.clamp(140.0, 480.0);
    workspace.preview_panel_width = workspace.preview_panel_width.clamp(280.0, 640.0);
    Ok(())
}

fn sanitize_window(mut window: WorkspaceWindowState) -> WorkspaceWindowState {
    window.width = window
        .width
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(800.0, 10_000.0));
    window.height = window
        .height
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(600.0, 10_000.0));
    window.x = window.x.filter(|value| value.is_finite());
    window.y = window.y.filter(|value| value.is_finite());
    window
}

fn import_legacy(values: &BTreeMap<String, String>) -> Result<Option<WorkspaceState>, String> {
    let Some(raw) = values.get("explorie:workspaces") else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "legacy workspaces value is not an object".to_string())?;
    let now = unix_ms();
    let mut workspaces = Vec::new();
    for (id, value) in object {
        if workspaces.len() == MAX_WORKSPACES {
            break;
        }
        if let Some(workspace) = legacy_workspace(value, id, now) {
            workspaces.push(workspace);
        }
    }
    workspaces.sort_by_key(|workspace| std::cmp::Reverse(workspace.updated_at));
    let last_workspace_id = values
        .get("explorie:lastWorkspaceId")
        .filter(|id| workspaces.iter().any(|workspace| &workspace.id == *id))
        .cloned();
    Ok(Some(WorkspaceState {
        version: WORKSPACE_VERSION,
        workspaces,
        last_workspace_id,
    }))
}

fn legacy_workspace(value: &serde_json::Value, fallback_id: &str, now: u64) -> Option<Workspace> {
    let object = value.as_object()?;
    let name = valid_name(object.get("name")?.as_str()?).ok()?;
    let id = object
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty() && !id.contains('\0'))
        .unwrap_or(fallback_id)
        .to_string();
    let tabs: Vec<_> = object
        .get("tabs")?
        .as_array()?
        .iter()
        .take(MAX_TABS)
        .enumerate()
        .filter_map(|(index, tab)| {
            let tab = tab.as_object()?;
            let path = tab.get("path")?.as_str()?;
            if path.is_empty() || path.contains('\0') {
                return None;
            }
            let id = tab
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty() && !id.contains('\0'))
                .map_or_else(|| format!("tab-{index}"), ToString::to_string);
            Some(WorkspaceTab {
                id,
                path: PathBuf::from(path),
            })
        })
        .collect();
    if tabs.is_empty() {
        return None;
    }
    let active_tab_id = object
        .get("activeTabId")
        .and_then(serde_json::Value::as_str)
        .filter(|active| tabs.iter().any(|tab| tab.id == *active))
        .map_or_else(|| tabs[0].id.clone(), ToString::to_string);
    let view_mode = match object.get("viewMode").and_then(serde_json::Value::as_str) {
        Some("grid") => ViewMode::Grid,
        Some("column") => ViewMode::Column,
        _ => ViewMode::List,
    };
    let sort_key = match object.get("sortKey").and_then(serde_json::Value::as_str) {
        Some("size") => SortKey::Size,
        Some("modified") => SortKey::Modified,
        Some(custom) => SortKey::custom(custom.to_string()).unwrap_or(SortKey::Name),
        None => SortKey::Name,
    };
    let sort_direction = match object.get("sortDir").and_then(serde_json::Value::as_str) {
        Some("desc") | Some("descending") => SortDirection::Descending,
        _ => SortDirection::Ascending,
    };
    let filter_mode = match object.get("filterMode").and_then(serde_json::Value::as_str) {
        Some("folders") => EntryFilter::Folders,
        Some("files") => EntryFilter::Files,
        _ => EntryFilter::All,
    };
    let number = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .map(|value| value as f32)
    };
    let workspace = Workspace {
        id,
        name,
        created_at: object
            .get("createdAt")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(now),
        updated_at: object
            .get("updatedAt")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(now),
        tabs,
        active_tab_id,
        view_mode,
        sort_key,
        sort_direction,
        show_hidden: object
            .get("showHidden")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        filter_mode,
        show_preview_panel: object
            .get("showPreviewPanel")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        grid_min_width: object
            .get("gridMinWidth")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(140),
        window: WorkspaceWindowState {
            width: number("windowWidth"),
            height: number("windowHeight"),
            x: number("windowX"),
            y: number("windowY"),
        },
        sidebar_width: number("sidebarWidth").unwrap_or_else(default_sidebar_width),
        preview_panel_width: number("previewPanelWidth")
            .unwrap_or_else(default_preview_panel_width),
        sidebar_collapsed: object
            .get("sidebarCollapsed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    };
    let mut workspace = workspace;
    validate_workspace(&mut workspace).ok()?;
    Some(workspace)
}

enum StoreMessage {
    Save(Vec<u8>),
    Flush(mpsc::SyncSender<()>),
}

pub(crate) struct WorkspaceStore {
    sender: Option<mpsc::Sender<StoreMessage>>,
    worker: Option<JoinHandle<()>>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl WorkspaceStore {
    pub(crate) fn open(
        config_dir: &Path,
        legacy_values: &BTreeMap<String, String>,
    ) -> (Self, WorkspaceState, Option<String>) {
        let path = config_dir.join(WORKSPACE_FILE);
        let mut save_initial = false;
        let (state, warning) = match fs::read(&path) {
            Ok(bytes) => match decode_workspaces(&bytes) {
                Ok((state, false)) => (state, None),
                Ok((state, true)) => {
                    let backup = migration_copy_path(&path, 1);
                    match fs::copy(&path, &backup) {
                        Ok(_) => {
                            save_initial = true;
                            (
                                state,
                                Some(format!(
                                    "Workspaces were migrated to schema v{WORKSPACE_VERSION}; the v1 source was preserved at {}",
                                    backup.display()
                                )),
                            )
                        }
                        Err(error) => (
                            state,
                            Some(format!(
                                "Workspaces are using the v{WORKSPACE_VERSION} schema in memory, but the v1 backup could not be created ({error}); the source file was left unchanged"
                            )),
                        ),
                    }
                }
                Err(error) => {
                    let backup = preserved_copy_path(&path);
                    let preservation = fs::copy(&path, &backup);
                    let warning = match &preservation {
                        Ok(_) => format!(
                            "Workspace recovery used an empty collection; the invalid file was preserved at {}: {error}",
                            backup.display()
                        ),
                        Err(copy_error) => format!(
                            "Workspace recovery used an empty collection; preserving the invalid file failed ({copy_error}): {error}"
                        ),
                    };
                    save_initial = preservation.is_ok();
                    (WorkspaceState::empty(), Some(warning))
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match import_legacy(legacy_values) {
                    Ok(Some(state)) => {
                        let count = state.workspaces.len();
                        save_initial = true;
                        (
                            state,
                            Some(format!(
                                "Imported {count} legacy workspace(s) into {}",
                                path.display()
                            )),
                        )
                    }
                    Ok(None) => {
                        save_initial = true;
                        (WorkspaceState::empty(), None)
                    }
                    Err(error) => {
                        save_initial = true;
                        (
                            WorkspaceState::empty(),
                            Some(format!(
                                "Legacy workspace import was unavailable; the raw value remains preserved in settings-v1.json: {error}"
                            )),
                        )
                    }
                }
            }
            Err(error) => (
                WorkspaceState::empty(),
                Some(format!("Workspace recovery unavailable: {error}")),
            ),
        };

        let (sender, receiver) = mpsc::channel();
        let last_error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&last_error);
        let worker = thread::spawn(move || {
            while let Ok(message) = receiver.recv() {
                match message {
                    StoreMessage::Save(bytes) => {
                        let result = atomic_write(&path, &bytes).map_err(|error| error.to_string());
                        *worker_error
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = result.err();
                    }
                    StoreMessage::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        let store = Self {
            sender: Some(sender),
            worker: Some(worker),
            last_error,
        };
        if save_initial {
            let _ = store.save(&state);
        }
        (store, state, warning)
    }

    pub(crate) fn save(&self, state: &WorkspaceState) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
        self.sender
            .as_ref()
            .ok_or_else(|| "workspace writer is unavailable".to_string())?
            .send(StoreMessage::Save(bytes))
            .map_err(|_| "workspace writer stopped unexpectedly".to_string())
    }

    pub(crate) fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn flush(&self) {
        let (sender, receiver) = mpsc::sync_channel(0);
        if self
            .sender
            .as_ref()
            .is_some_and(|queue| queue.send(StoreMessage::Flush(sender)).is_ok())
        {
            let _ = receiver.recv();
        }
    }
}

fn decode_workspaces(bytes: &[u8]) -> Result<(WorkspaceState, bool), String> {
    let mut value =
        serde_json::from_slice::<serde_json::Value>(bytes).map_err(|error| error.to_string())?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "workspace version is missing or invalid".to_string())?;
    let migrated = match version {
        1 => {
            value["version"] = serde_json::Value::from(WORKSPACE_VERSION);
            true
        }
        version if version == u64::from(WORKSPACE_VERSION) => false,
        version => {
            return Err(format!(
                "unsupported workspace version {version}; expected 1 or {WORKSPACE_VERSION}"
            ));
        }
    };
    serde_json::from_value::<WorkspaceState>(value)
        .map_err(|error| error.to_string())
        .and_then(WorkspaceState::validate)
        .map(|state| (state, migrated))
}

fn migration_copy_path(path: &Path, version: u32) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    parent.join(format!("{stem}.pre-migration-v{version}.json"))
}

impl Drop for WorkspaceStore {
    fn drop(&mut self) {
        self.flush();
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn preserved_copy_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut candidate = parent.join("workspaces-v1.invalid.json");
    let mut suffix = 2;
    while candidate.exists() {
        candidate = parent.join(format!("workspaces-v1.invalid-{suffix}.json"));
        suffix += 1;
    }
    candidate
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "workspace path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy(),
        std::process::id(),
        counter
    ));
    let result = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temp, destination)?;
    File::open(destination.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "workspace path has no parent")
    })?)?
    .sync_all()
}

#[cfg(windows)]
fn replace_file(temp: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let temp: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            temp.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn fixture_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("explorie-workspaces-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        path
    }

    fn snapshot(path: &Path) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            tabs: vec![WorkspaceTab {
                id: "tab-1".to_string(),
                path: path.to_path_buf(),
            }],
            active_tab_id: "tab-1".to_string(),
            view_mode: ViewMode::Grid,
            sort_key: SortKey::custom("status").unwrap(),
            sort_direction: SortDirection::Descending,
            show_hidden: true,
            filter_mode: EntryFilter::Files,
            show_preview_panel: true,
            grid_min_width: 180,
            window: WorkspaceWindowState {
                width: Some(1200.0),
                height: Some(800.0),
                x: Some(20.0),
                y: Some(30.0),
            },
            sidebar_width: 240.0,
            preview_panel_width: 420.0,
            sidebar_collapsed: false,
        }
    }

    #[test]
    fn native_workspace_round_trip_rename_delete_and_export_import() {
        let root = fixture_dir();
        let (store, mut state, warning) = WorkspaceStore::open(&root, &BTreeMap::new());
        assert!(warning.is_none());
        let id = state.save_current("Editing", snapshot(&root)).unwrap();
        state.rename(&id, "Editing renamed").unwrap();
        store.save(&state).unwrap();
        store.flush();
        drop(store);

        let (store, mut restored, warning) = WorkspaceStore::open(&root, &BTreeMap::new());
        assert!(warning.is_none());
        assert_eq!(restored.get(&id).unwrap().name, "Editing renamed");
        assert_eq!(restored.get(&id).unwrap().window.width, Some(1200.0));
        assert_eq!(restored.get(&id).unwrap().preview_panel_width, 420.0);
        assert_eq!(
            restored.get(&id).unwrap().sort_key,
            SortKey::Custom("status".to_string())
        );
        let exported = restored.export_json().unwrap();
        assert_eq!(restored.import_json(&exported).unwrap(), 1);
        assert_eq!(restored.workspaces().len(), 2);
        assert!(restored.delete(&id));
        store.save(&restored).unwrap();
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_map_imports_valid_workspaces_and_keeps_raw_authority_external() {
        let root = fixture_dir();
        let legacy = serde_json::json!({
            "legacy-one": {
                "id": "legacy-one",
                "name": "Legacy",
                "createdAt": 10,
                "updatedAt": 20,
                "tabs": [{"id": "old-tab", "path": root}],
                "activeTabId": "old-tab",
                "viewMode": "column",
                "sortKey": "modified",
                "sortDir": "desc",
                "showHidden": true,
                "filterMode": "folders",
                "showPreviewPanel": true,
                "gridMinWidth": 200,
                "windowWidth": 1300,
                "windowHeight": 900,
                "sidebarWidth": 260
            },
            "invalid": {"name": "No tabs", "tabs": []}
        });
        let values = BTreeMap::from([
            ("explorie:workspaces".to_string(), legacy.to_string()),
            (
                "explorie:lastWorkspaceId".to_string(),
                "legacy-one".to_string(),
            ),
        ]);
        let (store, state, warning) = WorkspaceStore::open(&root, &values);
        assert!(
            warning
                .as_deref()
                .is_some_and(|warning| warning.contains("Imported 1"))
        );
        assert_eq!(state.workspaces().len(), 1);
        let workspace = state.get("legacy-one").unwrap();
        assert_eq!(workspace.preview_panel_width, 360.0);
        assert_eq!(workspace.view_mode, ViewMode::Column);
        assert_eq!(workspace.sort_key, SortKey::Modified);
        assert_eq!(workspace.window.width, Some(1300.0));
        assert_eq!(state.last_workspace_id(), Some("legacy-one"));
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_native_workspace_file_is_preserved_before_recovery() {
        let root = fixture_dir();
        fs::write(root.join(WORKSPACE_FILE), b"{not-json").unwrap();
        let (store, state, warning) = WorkspaceStore::open(&root, &BTreeMap::new());
        assert!(state.workspaces().is_empty());
        assert!(
            warning
                .as_deref()
                .is_some_and(|warning| warning.contains("preserved"))
        );
        assert_eq!(
            fs::read(root.join("workspaces-v1.invalid.json")).unwrap(),
            b"{not-json"
        );
        store.flush();
        drop(store);
        assert!(
            serde_json::from_slice::<WorkspaceState>(&fs::read(root.join(WORKSPACE_FILE)).unwrap())
                .is_ok()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
