use crate::{BlockingTask, ServiceContext, ServiceError, ServiceResult, SharedState};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ListRequest {
    pub path: PathBuf,
    pub calc_dir_size: bool,
}

/// A directory listing plus the non-fatal problems met while producing it.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ListResponse {
    pub entries: Vec<explorie_core::FileEntry>,
    /// Problems that did not stop the listing: an unreadable or malformed
    /// `.explorie.json`, items whose details could not be read, or folder
    /// sizes that only cover the readable part of a folder.
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl ListResponse {
    /// All warnings as one status line (joined like the UI's other warnings),
    /// or `None` when the listing was clean.
    pub fn warning(&self) -> Option<String> {
        (!self.warnings.is_empty()).then(|| self.warnings.join(" • "))
    }
}

impl From<explorie_core::DirListing> for ListResponse {
    fn from(listing: explorie_core::DirListing) -> Self {
        Self {
            entries: listing.entries,
            warnings: listing.warnings,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemLocations {
    pub desktop: Option<String>,
    pub documents: Option<String>,
    pub downloads: Option<String>,
    pub music: Option<String>,
    pub pictures: Option<String>,
    pub videos: Option<String>,
    pub home: Option<String>,
    pub drives: Vec<String>,
    /// The same volumes as `drives`, with the name the OS shows for each.
    #[serde(default)]
    pub volumes: Vec<VolumeLocation>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VolumeLocation {
    pub path: String,
    /// The volume's display name, such as "Macintosh HD"; empty when unknown.
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct DiskInfo {
    pub mount_point: String,
    pub total_space: u64,
    pub available_space: u64,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DirInfo {
    pub count: u64,
    pub size: u64,
}

#[derive(Clone)]
pub struct ListingService {
    context: ServiceContext,
    shared: Arc<SharedState>,
}

impl ListingService {
    pub(crate) fn new(context: ServiceContext, shared: Arc<SharedState>) -> Self {
        Self { context, shared }
    }

    pub fn list(&self, request: ListRequest) -> BlockingTask<Vec<explorie_core::FileEntry>> {
        self.context.spawn_blocking(move || {
            explorie_core::list_dir_with_sizes(&request.path, request.calc_dir_size)
                .map_err(ServiceError::from)
        })
    }

    pub fn list_blocking(
        &self,
        request: &ListRequest,
    ) -> ServiceResult<Vec<explorie_core::FileEntry>> {
        explorie_core::list_dir_with_sizes(&request.path, request.calc_dir_size)
            .map_err(ServiceError::from)
    }

    /// List a directory and keep the warnings that [`Self::list`] drops.
    pub fn list_with_warnings(&self, request: ListRequest) -> BlockingTask<ListResponse> {
        self.context
            .spawn_blocking(move || list_with_warnings(&request))
    }

    pub fn list_with_warnings_blocking(
        &self,
        request: &ListRequest,
    ) -> ServiceResult<ListResponse> {
        list_with_warnings(request)
    }

    pub fn system_locations(&self) -> BlockingTask<SystemLocations> {
        let shared = Arc::clone(&self.shared);
        self.context
            .spawn_blocking(move || system_locations(&shared))
    }

    pub fn system_locations_blocking(&self) -> ServiceResult<SystemLocations> {
        system_locations(&self.shared)
    }

    pub fn disk_info(&self, path: PathBuf) -> BlockingTask<DiskInfo> {
        self.context
            .spawn_blocking(move || disk_info(&path).map_err(ServiceError::from))
    }

    pub fn disk_info_blocking(&self, path: &Path) -> ServiceResult<DiskInfo> {
        disk_info(path).map_err(ServiceError::from)
    }

    pub fn folder_size(&self, path: PathBuf) -> BlockingTask<u64> {
        self.context
            .spawn_blocking(move || explorie_core::dir_size(&path).map_err(ServiceError::from))
    }

    pub fn folder_size_blocking(&self, path: &Path) -> ServiceResult<u64> {
        explorie_core::dir_size(path).map_err(ServiceError::from)
    }

    pub fn dir_info(&self, path: PathBuf) -> BlockingTask<DirInfo> {
        self.context.spawn_blocking(move || {
            explorie_core::dir_info(&path)
                .map(|(count, size)| DirInfo { count, size })
                .map_err(ServiceError::from)
        })
    }

    pub fn dir_info_blocking(&self, path: &Path) -> ServiceResult<DirInfo> {
        explorie_core::dir_info(path)
            .map(|(count, size)| DirInfo { count, size })
            .map_err(ServiceError::from)
    }

    pub fn launch_path(&self, args: Vec<std::ffi::OsString>) -> BlockingTask<Option<PathBuf>> {
        self.context
            .spawn_blocking(move || Ok(launch_directory_from_args(args)))
    }

    pub fn launch_path_blocking(&self, args: Vec<std::ffi::OsString>) -> Option<PathBuf> {
        launch_directory_from_args(args)
    }
}

fn list_with_warnings(request: &ListRequest) -> ServiceResult<ListResponse> {
    explorie_core::list_dir_with_warnings(&request.path, request.calc_dir_size)
        .map(ListResponse::from)
        .map_err(ServiceError::from)
}

fn system_locations(shared: &SharedState) -> ServiceResult<SystemLocations> {
    let desktop = dirs::desktop_dir().map(|path| path_string(&path));
    let documents = dirs::document_dir().map(|path| path_string(&path));
    let downloads = dirs::download_dir().map(|path| path_string(&path));
    let music = dirs::audio_dir().map(|path| path_string(&path));
    let pictures = dirs::picture_dir().map(|path| path_string(&path));
    let videos = dirs::video_dir().map(|path| path_string(&path));
    let home = dirs::home_dir().map(|path| path_string(&path));

    let disks = sysinfo::Disks::new_with_refreshed_list();
    let volumes: Vec<VolumeLocation> = disks
        .iter()
        .filter(|disk| {
            let path = disk.mount_point();
            !is_remote_root(shared, path) && !is_hidden_system_volume(path)
        })
        .map(|disk| VolumeLocation {
            path: path_string(disk.mount_point()),
            name: disk.name().to_string_lossy().into_owned(),
        })
        .collect();
    let drives = volumes.iter().map(|volume| volume.path.clone()).collect();

    Ok(SystemLocations {
        desktop,
        documents,
        downloads,
        music,
        pictures,
        videos,
        home,
        drives,
        volumes,
    })
}

/// macOS reports its sealed system volume's writable half and other system
/// volumes under /System/Volumes (the Data volume even counts as browsable),
/// but Finder never shows them; "/" already stands for the startup disk.
fn is_hidden_system_volume(path: &Path) -> bool {
    cfg!(target_os = "macos") && path.starts_with("/System/Volumes")
}

fn disk_info(path: &Path) -> std::io::Result<DiskInfo> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut best_match = None;
    let mut best_match_len = 0;
    for disk in disks.iter() {
        let mount = disk.mount_point();
        if path.starts_with(mount) {
            let length = mount.to_string_lossy().len();
            if length > best_match_len {
                best_match = Some(disk);
                best_match_len = length;
            }
        }
    }

    best_match
        .map(|disk| DiskInfo {
            mount_point: path_string(disk.mount_point()),
            total_space: disk.total_space(),
            available_space: disk.available_space(),
            name: disk.name().to_string_lossy().into_owned(),
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not find disk for the given path",
            )
        })
}

fn is_remote_root(shared: &SharedState, path: &Path) -> bool {
    let value = normalize_path(path);
    shared
        .remote_roots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&value)
}

pub(crate) fn normalize_path(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.trim_end_matches('/').to_ascii_lowercase()
    } else {
        value.trim_end_matches('/').to_string()
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn launch_directory_from_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<PathBuf> {
    args.into_iter()
        .skip(1)
        .map(PathBuf::from)
        .find(|path| path.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeServices, ResourcePaths};
    use std::fs;

    #[test]
    fn locations_name_volumes_and_hide_macos_system_volumes() {
        let root = tempfile::tempdir().unwrap();
        let services = NativeServices::new(ResourcePaths::test(root.path()));
        let locations = services.listing.system_locations_blocking().unwrap();
        let paths: Vec<&String> = locations.volumes.iter().map(|v| &v.path).collect();
        assert_eq!(locations.drives.iter().collect::<Vec<_>>(), paths);
        assert!(
            locations
                .volumes
                .iter()
                .all(|volume| !is_hidden_system_volume(Path::new(&volume.path))),
            "{:?}",
            locations.volumes
        );
        #[cfg(target_os = "macos")]
        {
            assert!(is_hidden_system_volume(Path::new("/System/Volumes/Data")));
            assert!(!is_hidden_system_volume(Path::new("/Volumes/Backup")));
            let startup = locations
                .volumes
                .iter()
                .find(|volume| volume.path == "/")
                .expect("the startup disk is listed");
            assert!(!startup.name.is_empty());
        }
    }

    #[test]
    fn listing_with_warnings_surfaces_malformed_metadata_without_failing() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("notes.txt"), b"notes").unwrap();
        fs::write(root.path().join(".explorie.json"), b"{ not json").unwrap();
        let services = NativeServices::new(ResourcePaths::test(root.path()));
        let request = ListRequest {
            path: root.path().to_path_buf(),
            calc_dir_size: false,
        };

        let response = services
            .listing
            .list_with_warnings(request.clone())
            .wait()
            .unwrap();
        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.warnings.len(), 1);
        assert!(response.warning().unwrap().contains(".explorie.json"));

        let entries = services.listing.list(request.clone()).wait().unwrap();
        assert_eq!(entries.len(), 1);

        fs::remove_file(root.path().join(".explorie.json")).unwrap();
        let clean = services
            .listing
            .list_with_warnings_blocking(&request)
            .unwrap();
        assert!(clean.warning().is_none());
    }

    #[test]
    fn joined_warning_uses_the_ui_separator() {
        let response = ListResponse {
            entries: Vec::new(),
            warnings: vec!["first".to_string(), "second".to_string()],
        };
        assert_eq!(response.warning().as_deref(), Some("first • second"));
    }
}
