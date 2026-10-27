use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};

use crate::browser::{BrowserState, FolderViewState};
use crate::{EntryFilter, SortDirection, SortKey, ViewMode};
use explorie_native_services::SearchCriteria;

const RECENT_LIMIT: usize = 5;
const GO_TO_FOLDER_RECENT_LIMIT: usize = 10;
const SESSION_VERSION: u32 = 2;
const WINDOW_SESSION_REGISTRY_VERSION: u32 = 1;
const MAX_WINDOW_SESSIONS: usize = 32;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct WindowSessionRegistry {
    path: PathBuf,
    ids: Arc<Mutex<Vec<String>>>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowSessionRegistrySnapshot {
    version: u32,
    #[serde(default)]
    ids: Vec<String>,
}

impl WindowSessionRegistry {
    pub fn open(config_dir: &Path) -> (Self, Vec<String>) {
        let path = config_dir.join("window-sessions-v1.json");
        let ids = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<WindowSessionRegistrySnapshot>(&bytes).ok())
            .filter(|snapshot| snapshot.version == WINDOW_SESSION_REGISTRY_VERSION)
            .map(|snapshot| {
                let mut seen = std::collections::HashSet::new();
                snapshot
                    .ids
                    .into_iter()
                    .filter(|id| {
                        valid_window_session_id(id)
                            && seen.insert(id.clone())
                            && config_dir
                                .join(format!("session-window-{id}.json"))
                                .is_file()
                    })
                    .take(MAX_WINDOW_SESSIONS)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        prune_orphaned_window_sessions(config_dir, &ids);
        (
            Self {
                path,
                ids: Arc::new(Mutex::new(ids.clone())),
            },
            ids,
        )
    }

    pub fn add(&self, id: String) -> io::Result<()> {
        if !valid_window_session_id(&id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid window session identifier",
            ));
        }
        let mut ids = self
            .ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !ids.contains(&id) {
            ids.push(id);
            save_window_session_registry(&self.path, &ids)?;
        }
        Ok(())
    }

    pub fn remove(&self, id: &str) -> io::Result<()> {
        let mut ids = self
            .ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = ids.len();
        ids.retain(|candidate| candidate != id);
        if ids.len() != before {
            save_window_session_registry(&self.path, &ids)?;
            if let Some(config_dir) = self.path.parent() {
                match fs::remove_file(self.session_path(config_dir, id)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    pub fn session_path(&self, config_dir: &Path, id: &str) -> PathBuf {
        if id == "primary" {
            config_dir.join("session-v1.json")
        } else {
            config_dir.join(format!("session-window-{id}.json"))
        }
    }
}

fn prune_orphaned_window_sessions(config_dir: &Path, active_ids: &[String]) {
    let Ok(entries) = fs::read_dir(config_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(id) = name
            .strip_prefix("session-window-")
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if valid_window_session_id(id) && !active_ids.iter().any(|active| active == id) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn valid_window_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn save_window_session_registry(path: &Path, ids: &[String]) -> io::Result<()> {
    let snapshot = WindowSessionRegistrySnapshot {
        version: WINDOW_SESSION_REGISTRY_VERSION,
        ids: ids.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&snapshot).map_err(io::Error::other)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path).or_else(|error| {
        if path.exists() {
            fs::remove_file(path)?;
            fs::rename(&temp, path)
        } else {
            Err(error)
        }
    })
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TabId(u64);

impl TabId {
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
pub struct TabState {
    id: TabId,
    browser: BrowserState,
}

impl TabState {
    pub fn id(&self) -> TabId {
        self.id
    }

    pub fn path(&self) -> &Path {
        self.browser.path()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Favorite {
    path: PathBuf,
    name: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartFolder {
    id: u64,
    name: String,
    criteria: SearchCriteria,
}

impl SmartFolder {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn criteria(&self) -> &SearchCriteria {
        &self.criteria
    }
}

impl Favorite {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug)]
pub struct SessionState {
    tabs: Vec<TabState>,
    active: usize,
    next_tab_id: u64,
    favorites: Vec<Favorite>,
    recents: Vec<PathBuf>,
    go_to_folder_recents: Vec<PathBuf>,
    smart_folders: Vec<SmartFolder>,
    active_smart_folder_id: Option<u64>,
    next_smart_folder_id: u64,
    window_placement: Option<SessionWindowPlacement>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SharedSessionState {
    favorites: Vec<Favorite>,
    recents: Vec<PathBuf>,
    go_to_folder_recents: Vec<PathBuf>,
    smart_folders: Vec<SmartFolder>,
    next_smart_folder_id: u64,
}

impl SharedSessionState {
    pub fn record_current_path(&mut self, path: PathBuf) {
        self.recents.retain(|recent| !same_path(recent, &path));
        self.recents.insert(0, path);
        self.recents.truncate(RECENT_LIMIT);
    }

    pub fn record_go_to_folder(&mut self, path: PathBuf) {
        self.go_to_folder_recents
            .retain(|recent| !same_path(recent, &path));
        self.go_to_folder_recents.insert(0, path);
        self.go_to_folder_recents
            .truncate(GO_TO_FOLDER_RECENT_LIMIT);
    }

    pub fn toggle_favorite(&mut self, path: PathBuf) -> bool {
        if self.remove_favorite(&path) {
            return false;
        }
        self.add_favorite(path)
    }

    pub fn add_favorite(&mut self, path: PathBuf) -> bool {
        if self
            .favorites
            .iter()
            .any(|favorite| same_path(&favorite.path, &path))
        {
            return false;
        }
        let name = path
            .file_name()
            .unwrap_or_else(|| path.as_os_str())
            .to_string_lossy()
            .into_owned();
        self.favorites.push(Favorite { path, name });
        true
    }

    pub fn remove_favorite(&mut self, path: &Path) -> bool {
        let before = self.favorites.len();
        self.favorites
            .retain(|favorite| !same_path(&favorite.path, path));
        self.favorites.len() != before
    }

    pub fn reorder_favorite(&mut self, path: &Path, target_index: usize) -> bool {
        let Some(source_index) = self
            .favorites
            .iter()
            .position(|favorite| same_path(&favorite.path, path))
        else {
            return false;
        };
        let target_index = target_index.min(self.favorites.len().saturating_sub(1));
        if source_index == target_index {
            return false;
        }
        let favorite = self.favorites.remove(source_index);
        self.favorites.insert(target_index, favorite);
        true
    }

    pub fn save_smart_folder(&mut self, name: String, criteria: SearchCriteria) -> u64 {
        let id = self.next_smart_folder_id;
        self.next_smart_folder_id = self.next_smart_folder_id.wrapping_add(1).max(1);
        self.smart_folders.push(SmartFolder { id, name, criteria });
        id
    }

    pub fn update_smart_folder(&mut self, id: u64, name: String, criteria: SearchCriteria) -> bool {
        let Some(folder) = self.smart_folders.iter_mut().find(|folder| folder.id == id) else {
            return false;
        };
        folder.name = name;
        folder.criteria = criteria;
        true
    }

    pub fn delete_smart_folder(&mut self, id: u64) -> bool {
        let before = self.smart_folders.len();
        self.smart_folders.retain(|folder| folder.id != id);
        self.smart_folders.len() != before
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionWindowPlacement {
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub x: Option<f32>,
    pub y: Option<f32>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredTab {
    id: u64,
    path: PathBuf,
    #[serde(default)]
    back: Vec<PathBuf>,
    #[serde(default)]
    forward: Vec<PathBuf>,
    #[serde(default)]
    folder_view_states: HashMap<PathBuf, FolderViewState>,
    #[serde(default)]
    column_view_widths: HashMap<PathBuf, u16>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredFavorite {
    path: PathBuf,
    name: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSnapshot {
    version: u32,
    tabs: Vec<StoredTab>,
    active_tab_id: u64,
    favorites: Vec<StoredFavorite>,
    recents: Vec<PathBuf>,
    #[serde(default)]
    go_to_folder_recents: Vec<PathBuf>,
    #[serde(default)]
    smart_folders: Vec<SmartFolder>,
    #[serde(default)]
    active_smart_folder_id: Option<u64>,
    #[serde(default)]
    window_placement: Option<SessionWindowPlacement>,
}

impl SessionState {
    pub fn new(path: PathBuf) -> Self {
        Self {
            tabs: vec![TabState {
                id: TabId(1),
                browser: BrowserState::new(path.clone()),
            }],
            active: 0,
            next_tab_id: 2,
            favorites: Vec::new(),
            recents: vec![path],
            go_to_folder_recents: Vec::new(),
            smart_folders: Vec::new(),
            active_smart_folder_id: None,
            next_smart_folder_id: 1,
            window_placement: None,
        }
    }

    pub fn tabs(&self) -> &[TabState] {
        &self.tabs
    }

    pub fn active_tab_window_session(&self) -> Self {
        Self {
            tabs: vec![TabState {
                id: TabId(1),
                browser: self.tabs[self.active].browser.clone(),
            }],
            active: 0,
            next_tab_id: 2,
            favorites: self.favorites.clone(),
            recents: self.recents.clone(),
            go_to_folder_recents: self.go_to_folder_recents.clone(),
            smart_folders: self.smart_folders.clone(),
            active_smart_folder_id: self.active_smart_folder_id,
            next_smart_folder_id: self.next_smart_folder_id,
            window_placement: None,
        }
    }

    pub fn shared_state(&self) -> SharedSessionState {
        SharedSessionState {
            favorites: self.favorites.clone(),
            recents: self.recents.clone(),
            go_to_folder_recents: self.go_to_folder_recents.clone(),
            smart_folders: self.smart_folders.clone(),
            next_smart_folder_id: self.next_smart_folder_id,
        }
    }

    pub fn apply_shared_state(&mut self, state: SharedSessionState) {
        self.favorites = state.favorites;
        self.recents = state.recents;
        self.go_to_folder_recents = state.go_to_folder_recents;
        self.smart_folders = state.smart_folders;
        self.next_smart_folder_id = state.next_smart_folder_id;
        if self
            .active_smart_folder_id
            .is_some_and(|id| !self.smart_folders.iter().any(|folder| folder.id == id))
        {
            self.active_smart_folder_id = None;
        }
    }

    pub fn workspace_tabs(&self) -> Vec<(String, PathBuf)> {
        self.tabs
            .iter()
            .map(|tab| (tab.id.0.to_string(), tab.path().to_path_buf()))
            .collect()
    }

    pub fn replace_workspace_tabs(
        &mut self,
        tabs: Vec<(String, PathBuf)>,
        active_tab_id: &str,
    ) -> Result<(), String> {
        if tabs.is_empty() {
            return Err("workspace has no available tabs".to_string());
        }
        let active = tabs
            .iter()
            .position(|(id, _)| id == active_tab_id)
            .unwrap_or(0);
        let mut next_id = self.next_tab_id;
        self.tabs = tabs
            .into_iter()
            .map(|(_, path)| {
                let id = TabId(next_id);
                next_id = next_id.wrapping_add(1).max(1);
                TabState {
                    id,
                    browser: BrowserState::new(path),
                }
            })
            .collect();
        self.active = active.min(self.tabs.len() - 1);
        self.next_tab_id = next_id;
        self.active_smart_folder_id = None;
        self.record_current_path();
        Ok(())
    }

    pub fn apply_browser_preferences(
        &mut self,
        show_hidden: bool,
        show_system_files: bool,
        filter: EntryFilter,
        sort_key: SortKey,
        sort_direction: SortDirection,
        view_mode: ViewMode,
    ) {
        for tab in &mut self.tabs {
            if tab.browser.folder_view_states().is_empty() {
                tab.browser.apply_preferences(
                    show_hidden,
                    show_system_files,
                    filter,
                    sort_key.clone(),
                    sort_direction,
                    view_mode,
                );
            } else {
                tab.browser
                    .apply_common_preferences(show_hidden, show_system_files, filter);
            }
        }
    }

    pub fn active_tab_id(&self) -> TabId {
        self.tabs[self.active].id
    }

    pub fn window_placement(&self) -> Option<SessionWindowPlacement> {
        self.window_placement
    }

    pub fn set_window_placement(&mut self, placement: SessionWindowPlacement) {
        self.window_placement = Some(placement);
    }

    pub fn new_tab(&mut self) -> TabId {
        let id = TabId(self.next_tab_id);
        self.next_tab_id = self.next_tab_id.wrapping_add(1).max(1);
        let browser = self.tabs[self.active].browser.fork_for_new_tab();
        self.tabs.push(TabState { id, browser });
        self.active = self.tabs.len() - 1;
        id
    }

    pub fn activate(&mut self, id: TabId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        if index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    pub fn activate_offset(&mut self, offset: isize) -> bool {
        if self.tabs.len() < 2 || offset == 0 {
            return false;
        }
        let count = self.tabs.len() as isize;
        let next = (self.active as isize + offset).rem_euclid(count) as usize;
        self.active = next;
        true
    }

    pub fn close(&mut self, id: TabId) -> bool {
        if self.tabs.len() == 1 {
            return false;
        }
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        let was_active = index == self.active;
        self.tabs.remove(index);
        if was_active {
            self.active = index.saturating_sub(1).min(self.tabs.len() - 1);
        } else if index < self.active {
            self.active -= 1;
        }
        true
    }

    pub fn reorder(&mut self, from: TabId, to: TabId) -> bool {
        if from == to {
            return false;
        }
        let Some(from_index) = self.tabs.iter().position(|tab| tab.id == from) else {
            return false;
        };
        let Some(to_index) = self.tabs.iter().position(|tab| tab.id == to) else {
            return false;
        };
        let active_id = self.active_tab_id();
        let tab = self.tabs.remove(from_index);
        self.tabs.insert(to_index, tab);
        self.active = self
            .tabs
            .iter()
            .position(|tab| tab.id == active_id)
            .expect("active tab remains present after reorder");
        true
    }

    pub fn move_active(&mut self, offset: isize) -> bool {
        let target = self.active.saturating_add_signed(offset);
        if target >= self.tabs.len() || target == self.active {
            return false;
        }
        let from = self.active_tab_id();
        let to = self.tabs[target].id;
        self.reorder(from, to)
    }

    pub fn record_current_path(&mut self) {
        let path = self.path().to_path_buf();
        self.recents.retain(|recent| !same_path(recent, &path));
        self.recents.insert(0, path);
        self.recents.truncate(RECENT_LIMIT);
    }

    pub fn recents(&self) -> &[PathBuf] {
        &self.recents
    }

    pub fn go_to_folder_recents(&self) -> &[PathBuf] {
        &self.go_to_folder_recents
    }

    #[cfg(test)]
    pub fn record_go_to_folder(&mut self, path: PathBuf) {
        self.go_to_folder_recents
            .retain(|recent| !same_path(recent, &path));
        self.go_to_folder_recents.insert(0, path);
        self.go_to_folder_recents
            .truncate(GO_TO_FOLDER_RECENT_LIMIT);
    }

    pub fn seed_go_to_folder_recents(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        if !self.go_to_folder_recents.is_empty() {
            return;
        }
        for path in paths {
            if self
                .go_to_folder_recents
                .iter()
                .any(|recent| same_path(recent, &path))
            {
                continue;
            }
            self.go_to_folder_recents.push(path);
            if self.go_to_folder_recents.len() == GO_TO_FOLDER_RECENT_LIMIT {
                break;
            }
        }
    }

    pub fn favorites(&self) -> &[Favorite] {
        &self.favorites
    }

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.favorites
            .iter()
            .any(|favorite| same_path(&favorite.path, path))
    }

    #[cfg(test)]
    pub fn toggle_favorite(&mut self, path: PathBuf) -> bool {
        if let Some(index) = self
            .favorites
            .iter()
            .position(|favorite| same_path(&favorite.path, &path))
        {
            self.favorites.remove(index);
            return false;
        }
        let name = path
            .file_name()
            .unwrap_or_else(|| path.as_os_str())
            .to_string_lossy()
            .into_owned();
        self.favorites.push(Favorite { path, name });
        true
    }

    #[cfg(test)]
    pub fn reorder_favorite(&mut self, path: &Path, target_index: usize) -> bool {
        let Some(source_index) = self
            .favorites
            .iter()
            .position(|favorite| same_path(&favorite.path, path))
        else {
            return false;
        };
        let target_index = target_index.min(self.favorites.len().saturating_sub(1));
        if source_index == target_index {
            return false;
        }
        let favorite = self.favorites.remove(source_index);
        self.favorites.insert(target_index, favorite);
        true
    }

    pub fn smart_folders(&self) -> &[SmartFolder] {
        &self.smart_folders
    }

    pub fn active_smart_folder(&self) -> Option<&SmartFolder> {
        let id = self.active_smart_folder_id?;
        self.smart_folders.iter().find(|folder| folder.id == id)
    }

    #[cfg(test)]
    pub fn save_smart_folder(&mut self, name: String, criteria: SearchCriteria) -> u64 {
        let id = self.next_smart_folder_id;
        self.next_smart_folder_id = self.next_smart_folder_id.wrapping_add(1).max(1);
        self.smart_folders.push(SmartFolder { id, name, criteria });
        id
    }

    #[cfg(test)]
    pub fn update_smart_folder(&mut self, id: u64, name: String, criteria: SearchCriteria) -> bool {
        let Some(folder) = self.smart_folders.iter_mut().find(|folder| folder.id == id) else {
            return false;
        };
        folder.name = name;
        folder.criteria = criteria;
        true
    }

    pub fn activate_smart_folder(&mut self, id: u64) -> bool {
        if !self.smart_folders.iter().any(|folder| folder.id == id) {
            return false;
        }
        if self.active_smart_folder_id == Some(id) {
            return false;
        }
        self.active_smart_folder_id = Some(id);
        true
    }

    pub fn clear_active_smart_folder(&mut self) {
        self.active_smart_folder_id = None;
    }

    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            version: SESSION_VERSION,
            tabs: self
                .tabs
                .iter()
                .map(|tab| StoredTab {
                    id: tab.id.0,
                    path: tab.browser.path().to_path_buf(),
                    back: tab.browser.back_history().to_vec(),
                    forward: tab.browser.forward_history().to_vec(),
                    folder_view_states: tab.browser.folder_view_states().clone(),
                    column_view_widths: tab.browser.column_view_widths().clone(),
                })
                .collect(),
            active_tab_id: self.active_tab_id().0,
            favorites: self
                .favorites
                .iter()
                .map(|favorite| StoredFavorite {
                    path: favorite.path.clone(),
                    name: favorite.name.clone(),
                })
                .collect(),
            recents: self.recents.clone(),
            go_to_folder_recents: self.go_to_folder_recents.clone(),
            smart_folders: self.smart_folders.clone(),
            active_smart_folder_id: self.active_smart_folder_id,
            window_placement: self.window_placement,
        }
    }

    fn from_snapshot(snapshot: SessionSnapshot) -> Result<Self, String> {
        if snapshot.version != SESSION_VERSION {
            return Err(format!(
                "unsupported session version {}; expected {SESSION_VERSION}",
                snapshot.version
            ));
        }
        if snapshot.tabs.is_empty() {
            return Err("session has no tabs".to_string());
        }
        let mut ids = std::collections::HashSet::with_capacity(snapshot.tabs.len());
        if snapshot
            .tabs
            .iter()
            .any(|tab| tab.id == 0 || !ids.insert(tab.id))
        {
            return Err("session contains invalid or duplicate tab identifiers".to_string());
        }
        let active = snapshot
            .tabs
            .iter()
            .position(|tab| tab.id == snapshot.active_tab_id)
            .unwrap_or(0);
        let next_tab_id = snapshot
            .tabs
            .iter()
            .map(|tab| tab.id)
            .max()
            .unwrap_or(0)
            .wrapping_add(1)
            .max(1);
        let tabs = snapshot
            .tabs
            .into_iter()
            .map(|tab| {
                let mut browser = BrowserState::new(tab.path);
                browser.restore_navigation_history(tab.back, tab.forward);
                browser.restore_folder_view_states(tab.folder_view_states);
                browser.restore_column_view_widths(tab.column_view_widths);
                TabState {
                    id: TabId(tab.id),
                    browser,
                }
            })
            .collect();
        let mut session = Self {
            tabs,
            active,
            next_tab_id,
            favorites: Vec::new(),
            recents: Vec::new(),
            go_to_folder_recents: Vec::new(),
            next_smart_folder_id: snapshot
                .smart_folders
                .iter()
                .map(|folder| folder.id)
                .max()
                .unwrap_or(0)
                .wrapping_add(1)
                .max(1),
            active_smart_folder_id: snapshot
                .active_smart_folder_id
                .filter(|id| snapshot.smart_folders.iter().any(|folder| folder.id == *id)),
            smart_folders: snapshot.smart_folders,
            window_placement: snapshot.window_placement,
        };
        for favorite in snapshot.favorites {
            if session.is_favorite(&favorite.path) {
                continue;
            }
            let name = if favorite.name.trim().is_empty() {
                favorite
                    .path
                    .file_name()
                    .unwrap_or(favorite.path.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            } else {
                favorite.name
            };
            session.favorites.push(Favorite {
                path: favorite.path,
                name,
            });
        }
        for recent in snapshot.recents {
            if session
                .recents
                .iter()
                .any(|existing| same_path(existing, &recent))
            {
                continue;
            }
            session.recents.push(recent);
            if session.recents.len() == RECENT_LIMIT {
                break;
            }
        }
        session.seed_go_to_folder_recents(snapshot.go_to_folder_recents);
        Ok(session)
    }
}

impl Deref for SessionState {
    type Target = BrowserState;

    fn deref(&self) -> &Self::Target {
        &self.tabs[self.active].browser
    }
}

impl DerefMut for SessionState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tabs[self.active].browser
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .replace('/', "\\")
            .eq_ignore_ascii_case(&right.to_string_lossy().replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

enum StoreMessage {
    Save(Vec<u8>),
    Flush(mpsc::SyncSender<()>),
}

pub(crate) struct SessionStore {
    sender: Option<mpsc::Sender<StoreMessage>>,
    worker: Option<JoinHandle<()>>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl SessionStore {
    pub fn open(path: PathBuf, fallback_path: PathBuf) -> (Self, SessionState, Option<String>) {
        let (state, warning, save_initial) = match fs::read(&path) {
            Ok(bytes) => match decode_session(&bytes) {
                Ok((state, false)) => (state, None, false),
                Ok((state, true)) => {
                    let backup = migration_copy_path(&path, 1);
                    match fs::copy(&path, &backup) {
                        Ok(_) => (
                            state,
                            Some(format!(
                                "Session was migrated to schema v{SESSION_VERSION}; the v1 source was preserved at {}",
                                backup.display()
                            )),
                            true,
                        ),
                        Err(error) => (
                            state,
                            Some(format!(
                                "Session is using the v{SESSION_VERSION} schema in memory, but the v1 backup could not be created ({error}); the source file was left unchanged"
                            )),
                            false,
                        ),
                    }
                }
                Err(error) => {
                    let backup = preserved_copy_path(&path);
                    let preservation = fs::copy(&path, &backup);
                    let warning = match &preservation {
                        Ok(_) => format!(
                            "Session recovery used a clean session; the invalid file was preserved at {}: {error}",
                            backup.display()
                        ),
                        Err(copy_error) => format!(
                            "Session recovery used a clean session; preserving the invalid file failed ({copy_error}): {error}"
                        ),
                    };
                    (
                        SessionState::new(fallback_path),
                        Some(warning),
                        preservation.is_ok(),
                    )
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                (SessionState::new(fallback_path), None, true)
            }
            Err(error) => (
                SessionState::new(fallback_path),
                Some(format!("Session recovery unavailable: {error}")),
                false,
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

    pub fn save(&self, state: &SessionState) -> Result<(), String> {
        let bytes =
            serde_json::to_vec_pretty(&state.snapshot()).map_err(|error| error.to_string())?;
        self.sender
            .as_ref()
            .ok_or_else(|| "session writer is unavailable".to_string())?
            .send(StoreMessage::Save(bytes))
            .map_err(|_| "session writer stopped unexpectedly".to_string())
    }

    pub fn last_error(&self) -> Option<String> {
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

fn decode_session(bytes: &[u8]) -> Result<(SessionState, bool), String> {
    let mut value =
        serde_json::from_slice::<serde_json::Value>(bytes).map_err(|error| error.to_string())?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "session version is missing or invalid".to_string())?;
    let migrated = match version {
        1 => {
            value["version"] = serde_json::Value::from(SESSION_VERSION);
            true
        }
        version if version == u64::from(SESSION_VERSION) => false,
        version => {
            return Err(format!(
                "unsupported session version {version}; expected 1 or {SESSION_VERSION}"
            ));
        }
    };
    serde_json::from_value::<SessionSnapshot>(value)
        .map_err(|error| error.to_string())
        .and_then(SessionState::from_snapshot)
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

impl Drop for SessionStore {
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
    let stem = path
        .file_stem()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let mut candidate = parent.join(format!("{stem}.invalid.json"));
    let mut suffix = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{stem}.invalid-{suffix}.json"));
        suffix += 1;
    }
    candidate
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "session path has no parent"))?;
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
        io::Error::new(io::ErrorKind::InvalidInput, "session path has no parent")
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
    use explorie_native_services::SearchType;
    use uuid::Uuid;

    fn fixture_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("explorie-session-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn tabs_preserve_independent_navigation_and_close_to_the_left() {
        let mut session = SessionState::new(PathBuf::from("root"));
        let first = session.active_tab_id();
        assert!(session.navigate(PathBuf::from("root/one")));
        let second = session.new_tab();
        assert_eq!(session.path(), Path::new("root/one"));
        assert!(session.navigate(PathBuf::from("root/two")));

        assert!(session.activate(first));
        assert_eq!(session.path(), Path::new("root/one"));
        assert!(session.activate(second));
        assert_eq!(session.path(), Path::new("root/two"));

        assert!(session.close(session.active_tab_id()));
        assert_eq!(session.active_tab_id(), first);
        assert_eq!(session.path(), Path::new("root/one"));
        assert!(!session.close(session.active_tab_id()));
    }

    #[test]
    fn tab_activation_wraps_and_reorder_preserves_identity() {
        let mut session = SessionState::new(PathBuf::from("root"));
        let first = session.active_tab_id();
        let second = session.new_tab();
        let third = session.new_tab();

        assert!(session.activate_offset(1));
        assert_eq!(session.active_tab_id(), first);
        assert!(session.reorder(first, third));
        assert_eq!(session.active_tab_id(), first);
        assert_eq!(session.tabs()[2].id(), first);
        assert!(session.move_active(-1));
        assert_eq!(session.tabs()[1].id(), first);
        assert!(session.activate(second));
    }

    #[test]
    fn favorites_are_unique_and_recents_are_most_recent_first_with_a_limit() {
        let mut session = SessionState::new(PathBuf::from("root"));
        assert!(session.toggle_favorite(PathBuf::from("root/favorite")));
        assert!(!session.toggle_favorite(PathBuf::from("root/favorite")));
        assert!(session.toggle_favorite(PathBuf::from("root/favorite")));
        assert_eq!(session.favorites()[0].name(), "favorite");
        assert!(session.toggle_favorite(PathBuf::from("root/second")));
        assert!(session.toggle_favorite(PathBuf::from("root/third")));
        assert!(session.reorder_favorite(Path::new("root/favorite"), 2));
        assert_eq!(session.favorites()[0].name(), "second");
        assert_eq!(session.favorites()[1].name(), "third");
        assert_eq!(session.favorites()[2].name(), "favorite");
        assert!(!session.reorder_favorite(Path::new("root/favorite"), 2));
        assert!(!session.reorder_favorite(Path::new("root/missing"), 0));

        for index in 0..7 {
            session.navigate(PathBuf::from(format!("root/{index}")));
            session.record_current_path();
        }
        assert_eq!(session.recents().len(), RECENT_LIMIT);
        assert_eq!(session.recents()[0], PathBuf::from("root/6"));
        assert_eq!(session.recents()[4], PathBuf::from("root/2"));

        session.navigate(PathBuf::from("root/4"));
        session.record_current_path();
        assert_eq!(session.recents()[0], PathBuf::from("root/4"));
        assert_eq!(
            session
                .recents()
                .iter()
                .filter(|path| *path == Path::new("root/4"))
                .count(),
            1
        );
    }

    #[test]
    fn versioned_session_round_trips_tabs_favorites_and_recents() {
        let root = fixture_dir();
        let path = root.join("session-v1.json");
        let (store, mut session, warning) = SessionStore::open(path.clone(), PathBuf::from("root"));
        assert!(warning.is_none());
        session.navigate(PathBuf::from("root/one"));
        session.record_current_path();
        let first = session.active_tab_id();
        let second = session.new_tab();
        session.navigate(PathBuf::from("root/two"));
        session.record_current_path();
        session.record_go_to_folder(PathBuf::from("root/recent-target"));
        session.record_go_to_folder(PathBuf::from("root/other-target"));
        session.record_go_to_folder(PathBuf::from("root/recent-target"));
        session.toggle_favorite(PathBuf::from("root/favorite"));
        session.toggle_favorite(PathBuf::from("root/second"));
        session.toggle_favorite(PathBuf::from("root/third"));
        assert!(session.reorder_favorite(Path::new("root/favorite"), 2));
        let smart = session.save_smart_folder(
            "Text files".into(),
            SearchCriteria {
                name_pattern: Some("text".into()),
                type_filter: SearchType::Files,
                search_paths: vec![PathBuf::from("root")],
                recursive: true,
                ..SearchCriteria::default()
            },
        );
        assert!(session.update_smart_folder(
            smart,
            "Source files".into(),
            SearchCriteria {
                name_pattern: Some("src".into()),
                extensions: vec!["rs".into(), "md".into()],
                type_filter: SearchType::Files,
                search_paths: vec![PathBuf::from("root")],
                recursive: true,
                ..SearchCriteria::default()
            },
        ));
        assert!(!session.update_smart_folder(
            u64::MAX,
            "Missing".into(),
            SearchCriteria::default(),
        ));
        assert!(session.activate_smart_folder(smart));
        store.save(&session).unwrap();
        store.flush();
        drop(store);

        let (store, restored, warning) = SessionStore::open(path, PathBuf::from("fallback"));
        assert!(warning.is_none());
        assert_eq!(restored.tabs().len(), 2);
        assert_eq!(restored.tabs()[0].id(), first);
        assert_eq!(restored.active_tab_id(), second);
        assert_eq!(restored.path(), Path::new("root/two"));
        assert_eq!(restored.back_history(), [PathBuf::from("root/one")]);
        assert!(restored.forward_history().is_empty());
        assert!(restored.is_favorite(Path::new("root/favorite")));
        assert_eq!(restored.favorites()[0].path(), Path::new("root/second"));
        assert_eq!(restored.favorites()[1].path(), Path::new("root/third"));
        assert_eq!(restored.favorites()[2].path(), Path::new("root/favorite"));
        assert_eq!(restored.recents()[0], PathBuf::from("root/two"));
        assert_eq!(
            restored.go_to_folder_recents(),
            [
                PathBuf::from("root/recent-target"),
                PathBuf::from("root/other-target")
            ]
        );
        assert_eq!(
            restored.active_smart_folder().unwrap().name(),
            "Source files"
        );
        assert_eq!(
            restored
                .active_smart_folder()
                .unwrap()
                .criteria()
                .extensions,
            ["rs", "md"]
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_session_is_reported_and_preserved() {
        let root = fixture_dir();
        let path = root.join("session-v1.json");
        fs::write(&path, b"{not-json").unwrap();

        let (store, session, warning) = SessionStore::open(path.clone(), PathBuf::from("fallback"));
        assert_eq!(session.path(), Path::new("fallback"));
        assert!(warning.is_some());
        store.flush();
        drop(store);
        assert_eq!(
            fs::read(root.join("session-v1.invalid.json")).unwrap(),
            b"{not-json"
        );
        assert!(serde_json::from_slice::<SessionSnapshot>(&fs::read(&path).unwrap()).is_ok());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_without_navigation_history_fields_keeps_backward_compatibility() {
        let root = fixture_dir();
        let path = root.join("session-v1.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 1,
                "tabs": [{ "id": 1, "path": "legacy-root" }],
                "activeTabId": 1,
                "favorites": [],
                "recents": [],
                "goToFolderRecents": [],
                "smartFolders": [],
                "activeSmartFolderId": null
            }))
            .unwrap(),
        )
        .unwrap();

        let (store, session, warning) = SessionStore::open(path, PathBuf::from("fallback-root"));
        assert!(
            warning
                .as_deref()
                .is_some_and(|warning| warning.starts_with("Session was migrated to schema v2"))
        );
        assert_eq!(session.path(), Path::new("legacy-root"));
        assert!(session.back_history().is_empty());
        assert!(session.forward_history().is_empty());
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
