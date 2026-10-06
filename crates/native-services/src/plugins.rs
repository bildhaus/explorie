//! Opt-in, trusted executable integrations. Capabilities are disclosures, not a sandbox.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, mpsc};
use std::time::Duration;

use explorie_plugin_protocol::{
    ActionEffect, ActionRequest, CatalogEntry, Contribution, Inspection, Manifest,
    PROTOCOL_VERSION, Preferences, read_frame, write_frame,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{BlockingTask, ErrorCode, ServiceContext, ServiceError, ServiceEvent, ServiceResult};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_PACKAGE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;

pub fn platform_target() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else {
        "unsupported"
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum PluginSource {
    Official,
    Bundled,
    Development,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginStatus {
    pub manifest: Manifest,
    pub source: PluginSource,
    pub installed: bool,
    pub installing: bool,
    pub enabled: bool,
    pub configuration: Value,
    pub error: Option<String>,
    pub update_available: bool,
    pub dependency_status: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginResult {
    pub id: String,
    pub contribution: Option<Contribution>,
    pub error: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Installed {
    manifest: Manifest,
    directory: PathBuf,
    source: PluginSource,
    #[serde(default)]
    preferences: Preferences,
    #[serde(default)]
    package_sha256: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
struct Registry {
    #[serde(default)]
    installed: BTreeMap<String, Installed>,
}

#[derive(Default)]
struct Runtime {
    process: Option<PluginProcess>,
    error: Option<String>,
}

/// Shared by every window through `PluginService::clone`.
pub struct PluginManager {
    root: PathBuf,
    catalog: RwLock<Vec<CatalogEntry>>,
    registry: Mutex<Registry>,
    registry_error: Option<String>,
    runtimes: Mutex<HashMap<String, Arc<Mutex<Runtime>>>>,
    mutations: Mutex<()>,
    installing: Mutex<HashSet<String>>,
    bundled_catalog: Mutex<HashSet<String>>,
    // Persisted bundled packages must be checked against this application's catalog each launch.
    verified_bundled: Mutex<HashSet<String>>,
    shutting_down: AtomicBool,
}

#[derive(Clone)]
pub struct PluginService {
    context: ServiceContext,
    manager: Arc<PluginManager>,
}

struct InstallationActivity {
    service: PluginService,
    id: String,
}

impl Drop for InstallationActivity {
    fn drop(&mut self) {
        lock(&self.service.manager.installing).remove(&self.id);
        self.service.changed();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn failure(message: impl Into<String>) -> ServiceError {
    ServiceError::new(ErrorCode::InvalidInput, message).retryable(true)
}

impl PluginService {
    pub fn new(context: ServiceContext, catalog: Vec<CatalogEntry>) -> Self {
        let root = context.resources().config_dir.join("plugins");
        let (registry, registry_error) = match read_registry(&root) {
            Ok(registry) => (registry, None),
            Err(error) => (Registry::default(), Some(error.message)),
        };
        Self {
            context,
            manager: Arc::new(PluginManager {
                root,
                catalog: RwLock::new(catalog),
                registry: Mutex::new(registry),
                registry_error,
                runtimes: Mutex::new(HashMap::new()),
                mutations: Mutex::new(()),
                installing: Mutex::new(HashSet::new()),
                bundled_catalog: Mutex::new(HashSet::new()),
                verified_bundled: Mutex::new(HashSet::new()),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    /// Host setup only. No disk, process, or network work takes this lock.
    pub fn set_catalog(&self, catalog: Vec<CatalogEntry>) {
        *self
            .manager
            .catalog
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = catalog;
    }

    pub fn list(&self) -> BlockingTask<Vec<PluginStatus>> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            let catalog = service.catalog();
            let installed = lock(&service.manager.registry).installed.clone();
            let installing = lock(&service.manager.installing).clone();
            let bundled_catalog = lock(&service.manager.bundled_catalog).clone();
            let mut statuses = BTreeMap::new();
            for entry in catalog
                .iter()
                .filter(|entry| entry.target == platform_target())
            {
                statuses.insert(
                    entry.manifest.id.clone(),
                    PluginStatus {
                        manifest: entry.manifest.clone(),
                        source: if bundled_catalog.contains(&entry.manifest.id) {
                            PluginSource::Bundled
                        } else {
                            PluginSource::Official
                        },
                        installed: false,
                        installing: installing.contains(&entry.manifest.id),
                        enabled: false,
                        configuration: Value::Null,
                        error: service.manager.registry_error.clone().or_else(|| {
                            bundled_catalog.contains(&entry.manifest.id).then(|| {
                                "Bundled integration is unavailable; repair or reinstall Explorie".into()
                            })
                        }),
                        update_available: false,
                        dependency_status: super::plugin_detection::detect_dependency(
                            &entry.manifest.id,
                        ),
                    },
                );
            }
            for (id, record) in installed {
                let runtime = service.runtime(&id);
                let runtime = lock(&runtime);
                let bundled = record.source == PluginSource::Bundled
                    || (record.source != PluginSource::Development && bundled_catalog.contains(&id));
                statuses.insert(
                    id.clone(),
                    PluginStatus {
                        manifest: record.manifest.clone(),
                        source: if bundled { PluginSource::Bundled } else { record.source },
                        installed: true,
                        installing: installing.contains(&id),
                        enabled: record.preferences.enabled,
                        configuration: record.preferences.configuration,
                        error: if bundled
                            && !lock(&service.manager.verified_bundled).contains(&id)
                        {
                            Some(
                                "Bundled integration is unavailable; repair or reinstall Explorie"
                                    .into(),
                            )
                        } else {
                            runtime.error.clone()
                        },
                        update_available: !bundled && record.source == PluginSource::Official
                            && catalog.iter().any(|entry| {
                                entry.target == platform_target()
                                    && entry.manifest.id == id
                                    && entry.manifest.version != record.manifest.version
                            }),
                        dependency_status: super::plugin_detection::detect_dependency(&id),
                    },
                );
            }
            Ok(statuses.into_values().collect())
        })
    }

    pub fn inspect(&self, inspection: Inspection) -> BlockingTask<Vec<PluginResult>> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            let records: Vec<_> = lock(&service.manager.registry)
                .installed
                .values()
                .filter(|record| record.preferences.enabled)
                .cloned()
                .collect();
            // Each plugin owns its deadline. A failed plugin cannot prevent the others inspecting.
            let tasks: Vec<_> = records
                .into_iter()
                .map(|record| {
                    let service = service.clone();
                    let inspection = inspection.clone();
                    std::thread::spawn(move || {
                        let id = record.manifest.id.clone();
                        match service.inspect_one(&id, inspection) {
                            Ok(contribution) => PluginResult {
                                id,
                                contribution: Some(contribution),
                                error: None,
                            },
                            Err(error) => PluginResult {
                                id,
                                contribution: None,
                                error: Some(error.message),
                            },
                        }
                    })
                })
                .collect();
            tasks
                .into_iter()
                .map(|task| {
                    task.join()
                        .map_err(|_| failure("Plugin inspection worker stopped"))
                })
                .collect()
        })
    }

    pub fn invoke(&self, id: String, request: ActionRequest) -> BlockingTask<ActionEffect> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            let value = service.call(
                &id,
                "invoke",
                serde_json::to_value(request).map_err(|_| failure("Invalid action"))?,
            )?;
            serde_json::from_value(value).map_err(|_| failure("Plugin returned an invalid action"))
        })
    }

    /// Enables fresh installations; updates preserve existing enablement and configuration.
    pub fn install(&self, id: String) -> BlockingTask<()> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            service.require_registry()?;
            if lock(&service.manager.bundled_catalog).contains(&id)
                || lock(&service.manager.registry).installed.get(&id)
                    .is_some_and(|record| record.source == PluginSource::Bundled)
            {
                return Err(failure("Bundled integrations cannot be downloaded separately; repair or reinstall Explorie"));
            }
            let entry = service
                .catalog()
                .into_iter()
                .find(|entry| entry.manifest.id == id && entry.target == platform_target())
                .ok_or_else(|| failure("No official package is available for this platform"))?;
            entry.manifest.validate().map_err(failure)?;
            if !entry.asset_url.starts_with("https://")
                || entry.sha256.len() != 64
                || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(failure(
                    "This build has no valid published package for the integration",
                ));
            }
            if !lock(&service.manager.installing).insert(id.clone()) {
                return Err(failure("Integration installation is already in progress"));
            }
            let _activity = InstallationActivity {
                service: service.clone(),
                id: id.clone(),
            };
            service.changed();
            let original_directory = lock(&service.manager.registry)
                .installed
                .get(&id)
                .map(|record| record.directory.clone());
            let response = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(60))
                .build()
                .get(&entry.asset_url)
                .call()
                .map_err(|_| failure("Could not download the integration package"))?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(MAX_PACKAGE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| failure("Integration download was interrupted"))?;
            let _mutation = lock(&service.manager.mutations);
            let current_directory = lock(&service.manager.registry)
                .installed
                .get(&id)
                .map(|record| record.directory.clone());
            if current_directory != original_directory {
                return Err(failure(
                    "Integration changed while downloading; retry installation",
                ));
            }
            service.install_bytes(&entry, &bytes)
        })
    }

    pub fn set_enabled(&self, id: String, enabled: bool) -> BlockingTask<()> {
        self.change_record(id, move |record| {
            record.preferences.enabled = enabled;
        })
    }

    /// Registers the packages shipped with this app, without launching or downloading them.
    /// Hashes come from the host's embedded catalog, never from the package directory.
    pub fn load_bundled(&self, directory: PathBuf) -> BlockingTask<()> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            let _mutation = lock(&service.manager.mutations);
            let entries: Vec<_> = service
                .catalog()
                .into_iter()
                .filter(|entry| entry.target == platform_target())
                .collect();
            let bundled_catalog: HashSet<_> = entries
                .iter()
                .map(|entry| entry.manifest.id.clone())
                .collect();
            *lock(&service.manager.bundled_catalog) = bundled_catalog.clone();
            service.require_registry()?;
            lock(&service.manager.verified_bundled).clear();
            let bundled_ids: Vec<_> = lock(&service.manager.registry)
                .installed
                .iter()
                .filter(|(id, record)| {
                    record.source == PluginSource::Bundled
                        || (record.source != PluginSource::Development
                            && bundled_catalog.contains(*id))
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in bundled_ids {
                service.stop(&id);
            }
            let mut first_error = None;
            for entry in entries {
                let id = entry.manifest.id.clone();
                if lock(&service.manager.registry)
                    .installed
                    .get(&id)
                    .is_some_and(|record| record.source == PluginSource::Development)
                {
                    continue;
                }
                let result = (|| {
                    let filename = entry.asset_url.rsplit('/').next().unwrap_or_default();
                    if !is_root_filename(Path::new(filename)) {
                        return Err(failure("Invalid bundled integration package filename"));
                    }
                    let bytes = read_bounded(&directory.join(filename), MAX_PACKAGE_BYTES)?;
                    service.install_package(&entry, &bytes, PluginSource::Bundled)?;
                    lock(&service.manager.verified_bundled).insert(id.clone());
                    Ok(())
                })();
                if let Err(error) = result {
                    first_error.get_or_insert(error);
                }
            }
            service.changed();
            first_error.map_or(Ok(()), Err)
        })
    }

    pub fn configure(&self, id: String, configuration: Value) -> BlockingTask<()> {
        self.change_record(id, move |record| {
            record.preferences.configuration = configuration;
        })
    }

    fn change_record(
        &self,
        id: String,
        change: impl FnOnce(&mut Installed) + Send + 'static,
    ) -> BlockingTask<()> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            let _mutation = lock(&service.manager.mutations);
            service.require_registry()?;
            let mut records = lock(&service.manager.registry).installed.clone();
            let record = records
                .get_mut(&id)
                .ok_or_else(|| failure("Integration is not installed"))?;
            change(record);
            commit_plugin_change(&service, records, &id)?;
            service.changed();
            Ok(())
        })
    }

    pub fn uninstall(&self, id: String) -> BlockingTask<()> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            let _mutation = lock(&service.manager.mutations);
            service.require_registry()?;
            let mut records = lock(&service.manager.registry).installed.clone();
            let removed = records
                .remove(&id)
                .ok_or_else(|| failure("Integration is not installed"))?;
            if removed.source == PluginSource::Bundled
                || (removed.source != PluginSource::Development
                    && lock(&service.manager.bundled_catalog).contains(&id))
            {
                return Err(failure(
                    "Bundled integrations cannot be uninstalled; disable the integration instead",
                ));
            }
            commit_plugin_change(&service, records, &id)?;
            if removed.source == PluginSource::Official {
                remove_owned_directory(&service.manager.root, &removed.directory)?;
            }
            service.changed();
            Ok(())
        })
    }

    pub fn load_development(&self, directory: PathBuf) -> BlockingTask<()> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            let directory = directory.canonicalize()?;
            let manifest = read_manifest(&directory)?;
            validate_executable(&directory, &manifest)?;
            let _mutation = lock(&service.manager.mutations);
            let id = manifest.id.clone();
            if lock(&service.manager.registry).installed.contains_key(&id) {
                return Err(failure("An integration with this identity is already loaded; use a distinct development integration identity"));
            }
            lock(&service.manager.registry).installed.insert(id, Installed {
                manifest, directory, source: PluginSource::Development,
                preferences: Preferences::default(),
                package_sha256: None,
            });
            service.changed();
            Ok(())
        })
    }

    pub fn retry(&self, id: String) -> BlockingTask<()> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            service.stop(&id);
            service.changed();
            Ok(())
        })
    }

    pub fn open_url(&self, url: String) -> BlockingTask<()> {
        self.context.spawn_blocking(move || {
            if !["https://", "http://", "obsidian://"]
                .iter()
                .any(|scheme| url.starts_with(scheme))
                || url.chars().any(char::is_control)
            {
                return Err(failure("Integration requested an unsupported link"));
            }
            open::that(url).map_err(|_| {
                failure("Could not open the link. Check that its application is installed")
            })
        })
    }

    pub fn shutdown(&self) -> BlockingTask<()> {
        let service = self.clone();
        self.manager.shutting_down.store(true, Ordering::Release);
        self.context.spawn_blocking(move || {
            let runtimes: Vec<_> = lock(&service.manager.runtimes).values().cloned().collect();
            for runtime in runtimes {
                *lock(&runtime) = Runtime::default();
            }
            Ok(())
        })
    }

    fn catalog(&self) -> Vec<CatalogEntry> {
        self.manager
            .catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn runtime(&self, id: &str) -> Arc<Mutex<Runtime>> {
        Arc::clone(
            lock(&self.manager.runtimes)
                .entry(id.to_owned())
                .or_default(),
        )
    }

    fn stop(&self, id: &str) {
        *lock(&self.runtime(id)) = Runtime::default();
    }

    fn changed(&self) {
        self.context
            .events()
            .publish(ServiceEvent::PluginRegistryChanged);
    }

    fn require_registry(&self) -> ServiceResult<()> {
        if let Some(error) = &self.manager.registry_error {
            return Err(failure(error.clone()));
        }
        Ok(())
    }

    fn commit_records(&self, records: BTreeMap<String, Installed>) -> ServiceResult<()> {
        let persisted = Registry {
            installed: records
                .iter()
                .filter(|(_, record)| record.source != PluginSource::Development)
                .map(|(id, record)| (id.clone(), record.clone()))
                .collect(),
        };
        fs::create_dir_all(&self.manager.root)?;
        let bytes = serde_json::to_vec_pretty(&persisted)
            .map_err(|_| failure("Could not save integrations"))?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(failure("Integration configuration is too large"));
        }
        atomic_write(&self.manager.root.join("registry.json"), &bytes)?;
        lock(&self.manager.registry).installed = records;
        Ok(())
    }

    fn install_bytes(&self, entry: &CatalogEntry, bytes: &[u8]) -> ServiceResult<()> {
        self.install_package(entry, bytes, PluginSource::Official)
    }

    fn install_package(
        &self,
        entry: &CatalogEntry,
        bytes: &[u8],
        source: PluginSource,
    ) -> ServiceResult<()> {
        self.require_registry()?;
        entry.manifest.validate().map_err(failure)?;
        if entry.target != platform_target() {
            return Err(failure("Package platform is incompatible"));
        }
        if bytes.len() as u64 > MAX_PACKAGE_BYTES
            || !format!("{:x}", Sha256::digest(bytes)).eq_ignore_ascii_case(&entry.sha256)
        {
            return Err(failure("Integration package failed its integrity check"));
        }
        let old = lock(&self.manager.registry)
            .installed
            .get(&entry.manifest.id)
            .cloned();
        if old
            .as_ref()
            .is_some_and(|record| record.source == PluginSource::Development)
        {
            return Err(failure(
                "Uninstall the development integration before installing the official package",
            ));
        }
        if source == PluginSource::Bundled
            && let Some(record) = &old
            && record.source == PluginSource::Bundled
            && record.manifest == entry.manifest
            && record
                .package_sha256
                .as_deref()
                .is_some_and(|hash| hash.eq_ignore_ascii_case(&entry.sha256))
            && cached_package_matches(&self.manager.root, record, bytes).unwrap_or(false)
        {
            return Ok(());
        }
        fs::create_dir_all(&self.manager.root)?;
        let name = PathBuf::from(format!("{}-{}", entry.manifest.id, uuid::Uuid::new_v4()));
        let directory = self.manager.root.join(&name);
        fs::create_dir(&directory)?;
        let result = (|| {
            extract_package(bytes, &directory)?;
            let manifest = read_manifest(&directory)?;
            if manifest != entry.manifest {
                return Err(failure(
                    "Package manifest does not match the official catalog",
                ));
            }
            validate_executable(&directory, &manifest)?;
            let mut records = lock(&self.manager.registry).installed.clone();
            let preferences =
                old.as_ref()
                    .map(|old| old.preferences.clone())
                    .unwrap_or(Preferences {
                        enabled: source == PluginSource::Official,
                        ..Preferences::default()
                    });
            records.insert(
                manifest.id.clone(),
                Installed {
                    manifest,
                    directory: name.clone(),
                    source,
                    preferences,
                    package_sha256: Some(entry.sha256.clone()),
                },
            );
            commit_plugin_change(self, records, &entry.manifest.id)?;
            if let Some(old) = old {
                let _ = remove_owned_directory(&self.manager.root, &old.directory);
            }
            self.changed();
            Ok(())
        })();
        if result.is_err() {
            let _ = remove_owned_directory(&self.manager.root, &name);
        }
        result
    }

    fn inspect_one(&self, id: &str, inspection: Inspection) -> ServiceResult<Contribution> {
        let slot = self.runtime(id);
        let mut runtime = lock(&slot);
        if !lock(&self.manager.registry)
            .installed
            .get(id)
            .is_some_and(|record| record.preferences.enabled)
        {
            return Err(failure("Integration is disabled or uninstalled"));
        }
        // Deliver every generation to the provider so subsequent notifications target current
        // windows. Providers cache expensive repository/folder data independently of context.
        let value = self.call_locked(
            id,
            "inspect",
            serde_json::to_value(&inspection).map_err(|_| failure("Invalid inspection"))?,
            &mut runtime,
        )?;
        let contribution: Contribution = serde_json::from_value(value)
            .map_err(|_| failure("Plugin returned invalid folder information"))?;
        if contribution.path != inspection.path
            || contribution.generation != inspection.generation
            || contribution.context_id != inspection.context_id
        {
            return Err(failure("Plugin returned obsolete folder information"));
        }
        Ok(contribution)
    }

    fn call(&self, id: &str, method: &str, params: Value) -> ServiceResult<Value> {
        let slot = self.runtime(id);
        let mut runtime = lock(&slot);
        self.call_locked(id, method, params, &mut runtime)
    }

    fn call_locked(
        &self,
        id: &str,
        method: &str,
        params: Value,
        runtime: &mut Runtime,
    ) -> ServiceResult<Value> {
        if self.manager.shutting_down.load(Ordering::Acquire) {
            return Err(failure("Integrations are shutting down"));
        }
        // Read enablement after taking the process lock: queued work cannot revive a disabled plugin.
        let record = lock(&self.manager.registry)
            .installed
            .get(id)
            .cloned()
            .filter(|record| record.preferences.enabled)
            .ok_or_else(|| failure("Integration is disabled or uninstalled"))?;
        if (record.source == PluginSource::Bundled
            || (record.source != PluginSource::Development
                && lock(&self.manager.bundled_catalog).contains(id)))
            && !lock(&self.manager.verified_bundled).contains(id)
        {
            return Err(failure(
                "Bundled integration is unavailable; repair or reinstall Explorie",
            ));
        }
        if let Some(error) = &runtime.error {
            return Err(failure(error.clone()));
        }
        let result = (|| {
            if runtime.process.is_none() {
                let directory = if record.source != PluginSource::Development {
                    self.manager.root.join(&record.directory)
                } else {
                    record.directory.clone()
                };
                let executable = validate_executable(&directory, &record.manifest)?;
                let mut process = PluginProcess::start(
                    Command::new(executable),
                    id.to_owned(),
                    self.context.clone(),
                )?;
                let manifest: Manifest = serde_json::from_value(process.request(
                    "initialize",
                    json!({"protocolVersion":PROTOCOL_VERSION}),
                    REQUEST_TIMEOUT,
                )?)
                .map_err(|_| failure("Integration initialization returned an invalid manifest"))?;
                if manifest != record.manifest {
                    return Err(failure(
                        "Integration executable does not match its installed manifest",
                    ));
                }
                process.request(
                    "configure",
                    record.preferences.configuration,
                    REQUEST_TIMEOUT,
                )?;
                runtime.process = Some(process);
            }
            runtime
                .process
                .as_mut()
                .expect("process initialized")
                .request(method, params, REQUEST_TIMEOUT)
        })();
        if let Err(error) = &result {
            runtime.process = None;
            runtime.error = Some(error.message.clone());
        }
        result
    }
}

fn commit_plugin_change(
    service: &PluginService,
    records: BTreeMap<String, Installed>,
    id: &str,
) -> ServiceResult<()> {
    let slot = service.runtime(id);
    let mut runtime = lock(&slot);
    service.commit_records(records)?;
    *runtime = Runtime::default();
    Ok(())
}

fn read_registry(root: &Path) -> ServiceResult<Registry> {
    let path = root.join("registry.json");
    if !path.exists() {
        return Ok(Registry::default());
    }
    let bytes = read_bounded(&path, MAX_REGISTRY_BYTES)?;
    let registry: Registry = serde_json::from_slice(&bytes).map_err(|_| failure("Integration registry is unreadable; restore registry.json before changing installations"))?;
    for (id, record) in &registry.installed {
        record.manifest.validate().map_err(failure)?;
        if id != &record.manifest.id
            || record.source == PluginSource::Development
            || !is_root_filename(&record.directory)
        {
            return Err(failure(
                "Integration registry contains an invalid installation",
            ));
        }
    }
    Ok(registry)
}

fn read_bounded(path: &Path, limit: u64) -> ServiceResult<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(failure("Integration file exceeds its size limit"));
    }
    Ok(bytes)
}

fn read_manifest(directory: &Path) -> ServiceResult<Manifest> {
    let manifest: Manifest =
        serde_json::from_slice(&read_bounded(&directory.join("plugin.json"), 64 * 1024)?)
            .map_err(|_| failure("Invalid integration manifest"))?;
    manifest.validate().map_err(failure)?;
    Ok(manifest)
}

fn cached_package_matches(root: &Path, record: &Installed, bytes: &[u8]) -> ServiceResult<bool> {
    let root = root.canonicalize()?;
    let directory = root.join(&record.directory);
    if fs::symlink_metadata(&directory)?.file_type().is_symlink()
        || directory.canonicalize()?.parent() != Some(root.as_path())
        || read_manifest(&directory)? != record.manifest
    {
        return Ok(false);
    }
    let executable = validate_executable(&directory, &record.manifest)?;
    let cached = read_bounded(&executable, MAX_PACKAGE_BYTES)?;
    let mut package = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| failure("Invalid integration archive"))?;
    let mut expected = Vec::new();
    package
        .by_name(&record.manifest.executables[platform_target()])
        .map_err(|_| failure("Integration archive executable is missing"))?
        .take(MAX_PACKAGE_BYTES + 1)
        .read_to_end(&mut expected)?;
    Ok(cached == expected)
}

fn validate_executable(directory: &Path, manifest: &Manifest) -> ServiceResult<PathBuf> {
    manifest.validate().map_err(failure)?;
    let name = manifest
        .executables
        .get(platform_target())
        .ok_or_else(|| failure("Integration does not support this platform"))?;
    let root = directory.canonicalize()?;
    let path = root.join(name).canonicalize()?;
    if path.parent() != Some(root.as_path()) || !path.is_file() {
        return Err(failure("Integration executable is outside its package"));
    }
    Ok(path)
}

fn is_root_filename(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().count() == 1
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && !path.to_string_lossy().contains(['/', '\\', ':'])
}

fn remove_owned_directory(root: &Path, name: &Path) -> ServiceResult<()> {
    if !is_root_filename(name) {
        return Err(failure("Invalid owned integration directory"));
    }
    let root = root.canonicalize()?;
    let directory = root.join(name);
    if !directory.exists() {
        return Ok(());
    }
    if fs::symlink_metadata(&directory)?.file_type().is_symlink()
        || directory.canonicalize()?.parent() != Some(root.as_path())
    {
        return Err(failure(
            "Refusing to remove an integration outside its owned directory",
        ));
    }
    fs::remove_dir_all(directory)?;
    Ok(())
}

fn extract_package(bytes: &[u8], directory: &Path) -> ServiceResult<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| failure("Invalid integration archive"))?;
    if archive.len() > 128 {
        return Err(failure("Integration package contains too many files"));
    }
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|_| failure("Invalid integration archive entry"))?;
        let name = file.name().to_owned();
        if name.contains(['\\', ':'])
            || file.enclosed_name().is_none()
            || Path::new(&name)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(failure(
                "Integration archive contains an unsafe path or link",
            ));
        }
        total = total
            .checked_add(file.size())
            .ok_or_else(|| failure("Integration package is too large"))?;
        if total > MAX_PACKAGE_BYTES {
            return Err(failure("Integration package is too large"));
        }
        let target = directory.join(&name);
        if file.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        let expected = file.size();
        let copied = std::io::copy(&mut (&mut file).take(expected + 1), &mut output)?;
        if copied != expected {
            return Err(failure(
                "Integration archive entry is truncated or oversized",
            ));
        }
        output.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &target,
                fs::Permissions::from_mode(
                    if file.unix_mode().is_some_and(|mode| mode & 0o111 != 0) {
                        0o755
                    } else {
                        0o644
                    },
                ),
            )?;
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> ServiceResult<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            };
            let from: Vec<_> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            // Both pointers are NUL-terminated and valid for this call.
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        #[cfg(not(windows))]
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

struct PluginProcess {
    child: Child,
    input: mpsc::SyncSender<Value>,
    output: mpsc::Receiver<ServiceResult<Value>>,
    active: Arc<AtomicBool>,
    notification_gate: Arc<Mutex<()>>,
    next_id: u64,
    #[cfg(windows)]
    job: ProcessJob,
}

impl PluginProcess {
    fn start(mut command: Command, id: String, context: ServiceContext) -> ServiceResult<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|_| failure("Could not start the integration executable"))?;
        #[cfg(windows)]
        let job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let mut stdin = child.stdin.take().expect("piped input");
        let stdout = child.stdout.take().expect("piped output");
        let mut stderr = child.stderr.take().expect("piped diagnostics");
        let (input, requests) = mpsc::sync_channel(1);
        let (responses, output) = mpsc::sync_channel(8);
        let active = Arc::new(AtomicBool::new(true));
        let reading = Arc::clone(&active);
        let notification_gate = Arc::new(Mutex::new(()));
        let publishing_gate = Arc::clone(&notification_gate);
        let pending_notification = Arc::new(Mutex::new(BTreeMap::<u64, Contribution>::new()));
        let pending_reader = Arc::clone(&pending_notification);
        std::thread::spawn(move || {
            while reading.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(200));
                let _publishing = lock(&publishing_gate);
                let pending = std::mem::take(&mut *lock(&pending_notification));
                if reading.load(Ordering::Acquire) {
                    for contribution in pending.into_values() {
                        context.events().publish(ServiceEvent::PluginStatusChanged {
                            id: id.clone(),
                            contribution,
                        });
                    }
                }
            }
        });
        std::thread::spawn(move || {
            while let Ok(request) = requests.recv() {
                if write_frame(&mut stdin, &request).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_frame(&mut reader) {
                    Ok(Some(frame)) => {
                        if frame.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                            let _ = responses
                                .try_send(Err(failure("Integration emitted invalid JSON-RPC")));
                            break;
                        }
                        if frame.get("method").and_then(Value::as_str) == Some("statusChanged")
                            && frame.get("id").is_none()
                        {
                            if let Some(params) = frame.get("params")
                                && let Ok(contribution) =
                                    serde_json::from_value::<Contribution>(params.clone())
                            {
                                let mut pending = lock(&pending_reader);
                                if pending.len() >= 32
                                    && !pending.contains_key(&contribution.context_id)
                                {
                                    pending.pop_first();
                                }
                                pending.insert(contribution.context_id, contribution);
                            }
                            continue;
                        }
                        if responses.try_send(Ok(frame)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = responses
                            .try_send(Err(failure("Integration process exited; select Retry")));
                        break;
                    }
                    Err(_) => {
                        let _ = responses.try_send(Err(failure(
                            "Integration emitted malformed or oversized output",
                        )));
                        break;
                    }
                }
            }
        });
        // Drain diagnostics with fixed memory; never copy credential-bearing child stderr into logs.
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            while let Ok(count) = stderr.read(&mut buffer) {
                if count == 0 {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input,
            output,
            active,
            notification_gate,
            next_id: 0,
            #[cfg(windows)]
            job,
        })
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> ServiceResult<Value> {
        self.next_id += 1;
        self.input
            .try_send(json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params}))
            .map_err(|_| failure("Integration is not accepting requests"))?;
        let response = self.output.recv_timeout(timeout).map_err(|_| {
            failure("Integration did not respond before its deadline; select Retry")
        })??;
        if response.get("id").and_then(Value::as_u64) != Some(self.next_id) {
            return Err(failure("Integration returned an unexpected response"));
        }
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Integration request failed");
            return Err(failure(message.chars().take(512).collect::<String>()));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| failure("Integration response has no result"))
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        let _publishing = lock(&self.notification_gate);
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
struct ProcessJob(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
unsafe impl Send for ProcessJob {}
#[cfg(windows)]
impl ProcessJob {
    fn attach(child: &Child) -> ServiceResult<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        // The job owns descendants and is closed once, after the child has been terminated.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(failure("Could not create an integration process job"));
        }
        let job = Self(handle);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
            || unsafe { AssignProcessToJobObject(handle, child.as_raw_handle().cast()) } == 0
        {
            return Err(failure("Could not contain the integration process tree"));
        }
        Ok(job)
    }

    fn terminate(&self) {
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use std::time::Instant;

    fn manifest() -> Manifest {
        Manifest {
            id: "fixture".into(),
            name: "Fixture integration".into(),
            version: "1.0.0".into(),
            protocol_version: PROTOCOL_VERSION,
            description: "Test integration".into(),
            executables: BTreeMap::from([(
                platform_target().into(),
                format!("fixture{}", std::env::consts::EXE_SUFFIX),
            )]),
            capabilities: vec!["Read test folders".into()],
            dependencies: vec![],
            settings: vec![],
        }
    }

    fn service(root: &Path) -> PluginService {
        PluginService::new(
            ServiceContext::new(crate::ResourcePaths::test(root)),
            vec![],
        )
    }

    fn archive(manifest: &Manifest) -> Vec<u8> {
        archive_with_executable(manifest, b"placeholder executable")
    }

    fn archive_with_executable(manifest: &Manifest, executable: &[u8]) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("plugin.json", options).unwrap();
        archive
            .write_all(&serde_json::to_vec(manifest).unwrap())
            .unwrap();
        archive
            .start_file(
                &manifest.executables[platform_target()],
                options.unix_permissions(0o755),
            )
            .unwrap();
        archive.write_all(executable).unwrap();
        archive.finish().unwrap().into_inner()
    }

    fn catalog(manifest: Manifest, bytes: &[u8]) -> CatalogEntry {
        CatalogEntry {
            manifest,
            target: platform_target().into(),
            asset_url: "https://example.invalid/package.zip".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
        }
    }

    /// Compile a real dependency-free protocol peer once; it runs as the installed executable.
    fn fixture_directory() -> &'static Path {
        static FIXTURE: OnceLock<tempfile::TempDir> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let directory = tempfile::tempdir().unwrap();
            let manifest = manifest();
            fs::write(directory.path().join("plugin.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
            let manifest_literal = format!("{:?}", serde_json::to_string(&manifest).unwrap());
            let source = r#"
use std::io::{self, BufRead, Write};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|v| v == "crash") { std::process::exit(3); }
    if args.get(1).is_some_and(|v| v == "hang") { std::thread::sleep(std::time::Duration::from_secs(120)); return; }
    if args.get(1).is_some_and(|v| v == "malformed") { println!("broken JSON"); return; }
    if args.get(1).is_some_and(|v| v == "oversized") { println!("{}", "x".repeat(16*1024*1024+1)); return; }
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let index = line.find("\"id\":").unwrap()+5;
        let id: String = line[index..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let params = &line[line.find("\"params\":").unwrap()+9..line.len()-1];
        let result = if line.contains("\"method\":\"initialize\"") { MANIFEST.to_string() }
        else if line.contains("\"method\":\"inspect\"") {
            format!("{},\"badge\":\"Fixture\"}}", &params[..params.len()-1])
        } else { "null".into() };
        if line.contains("\"method\":\"inspect\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"method\":\"statusChanged\",\"params\":{}}}", result);
        }
        println!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}", id, result);
        io::stdout().flush().unwrap();
    }
}
"#.replace("MANIFEST", &manifest_literal);
            let source_path = directory.path().join("fixture.rs");
            fs::write(&source_path, source).unwrap();
            let output = Command::new("rustc").arg("--edition=2024").arg(&source_path).arg("-o")
                .arg(directory.path().join(&manifest.executables[platform_target()])).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            directory
        }).path()
    }

    #[test]
    fn disabled_catalog_never_starts_or_installs_anything() {
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let bytes = archive(&manifest());
        service.set_catalog(vec![catalog(manifest(), &bytes)]);
        assert!(!service.list().wait().unwrap()[0].enabled);
        assert!(
            service
                .inspect(Inspection::default())
                .wait()
                .unwrap()
                .is_empty()
        );
        assert!(lock(&service.manager.runtimes).is_empty());
        assert!(!service.manager.root.exists());
    }

    fn bundled_fixture(
        service: &PluginService,
        directory: &Path,
        manifest: Manifest,
        bytes: &[u8],
    ) {
        fs::write(directory.join("package.zip"), bytes).unwrap();
        service.set_catalog(vec![catalog(manifest, bytes)]);
    }

    #[test]
    fn bundled_package_is_offline_opt_in_and_executes_after_enablement() {
        let root = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let executable =
            fs::read(fixture_directory().join(&manifest().executables[platform_target()])).unwrap();
        let bytes = archive_with_executable(&manifest(), &executable);
        bundled_fixture(&service, packages.path(), manifest(), &bytes);
        service
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        let status = service.list().wait().unwrap().remove(0);
        assert!(status.installed);
        assert_eq!(status.source, PluginSource::Bundled);
        assert!(!status.enabled);
        assert!(!status.update_available);
        assert!(
            service
                .inspect(Inspection::default())
                .wait()
                .unwrap()
                .is_empty()
        );
        assert!(
            lock(&service.manager.runtimes)
                .values()
                .all(|runtime| lock(runtime).process.is_none())
        );
        service.set_enabled("fixture".into(), true).wait().unwrap();
        let result = service
            .inspect(Inspection::default())
            .wait()
            .unwrap()
            .remove(0);
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(
            result.contribution.unwrap().badge.as_deref(),
            Some("Fixture")
        );
        assert!(service.uninstall("fixture".into()).wait().is_err());
        service.shutdown().wait().unwrap();
    }

    #[test]
    fn missing_or_corrupt_bundles_never_offer_a_download_on_a_fresh_profile() {
        for corrupt in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let packages = tempfile::tempdir().unwrap();
            let service = service(root.path());
            let bytes = archive(&manifest());
            service.set_catalog(vec![catalog(manifest(), &bytes)]);
            if corrupt {
                fs::write(packages.path().join("package.zip"), b"corrupt").unwrap();
            }
            assert!(
                service
                    .load_bundled(packages.path().to_owned())
                    .wait()
                    .is_err()
            );
            let status = service.list().wait().unwrap().remove(0);
            assert_eq!(status.source, PluginSource::Bundled);
            assert!(!status.installed);
            assert!(!status.enabled);
            assert!(!status.update_available);
            assert!(status.error.unwrap().contains("repair or reinstall"));
            let error = service.install("fixture".into()).wait().unwrap_err();
            assert!(error.message.contains("cannot be downloaded separately"));
            assert!(!service.manager.root.exists());
            assert!(lock(&service.manager.runtimes).is_empty());
        }
    }

    #[test]
    fn bundled_updates_preserve_preferences_and_skip_only_matching_valid_packages() {
        let root = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let bytes = archive(&manifest());
        bundled_fixture(&service, packages.path(), manifest(), &bytes);
        service
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        service.set_enabled("fixture".into(), true).wait().unwrap();
        service
            .configure("fixture".into(), json!({"folder":"chosen"}))
            .wait()
            .unwrap();
        let first_directory = lock(&service.manager.registry).installed["fixture"]
            .directory
            .clone();
        let restored = PluginService::new(service.context.clone(), service.catalog());
        restored
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        assert_eq!(
            lock(&restored.manager.registry).installed["fixture"].directory,
            first_directory
        );
        let cached_executable = restored
            .manager
            .root
            .join(&first_directory)
            .join(&manifest().executables[platform_target()]);
        let mut corrupted = fs::read(&cached_executable).unwrap();
        corrupted[0] ^= 1; // Same filename and length must not conceal corruption.
        fs::write(cached_executable, corrupted).unwrap();
        restored
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        let repaired_directory = lock(&restored.manager.registry).installed["fixture"]
            .directory
            .clone();
        assert_ne!(first_directory, repaired_directory);
        assert_eq!(
            fs::read(
                restored
                    .manager
                    .root
                    .join(&repaired_directory)
                    .join(&manifest().executables[platform_target()])
            )
            .unwrap(),
            b"placeholder executable"
        );
        let changed_bytes =
            archive_with_executable(&manifest(), b"updated executable bytes, same version");
        bundled_fixture(&restored, packages.path(), manifest(), &changed_bytes);
        restored
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        let replaced_directory = lock(&restored.manager.registry).installed["fixture"]
            .directory
            .clone();
        assert_ne!(repaired_directory, replaced_directory);
        assert!(!restored.manager.root.join(first_directory).exists());
        fs::remove_file(
            restored
                .manager
                .root
                .join(&replaced_directory)
                .join(&manifest().executables[platform_target()]),
        )
        .unwrap();
        restored
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        assert_ne!(
            lock(&restored.manager.registry).installed["fixture"].directory,
            replaced_directory
        );
        let mut updated = manifest();
        updated.version = "2.0.0".into();
        let updated_bytes = archive(&updated);
        bundled_fixture(&restored, packages.path(), updated, &updated_bytes);
        restored
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        let status = restored.list().wait().unwrap().remove(0);
        assert_eq!(status.manifest.version, "2.0.0");
        assert!(status.enabled);
        assert_eq!(status.configuration["folder"], "chosen");
        assert!(!status.update_available);
        restored
            .set_enabled("fixture".into(), false)
            .wait()
            .unwrap();
        let restarted = PluginService::new(restored.context.clone(), restored.catalog());
        restarted
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        assert!(!restarted.list().wait().unwrap()[0].enabled);
    }

    #[test]
    fn bundled_migration_preserves_official_preferences_and_development_provenance() {
        let root = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let bytes = archive(&manifest());
        service
            .install_bytes(&catalog(manifest(), &bytes), &bytes)
            .unwrap();
        service
            .configure("fixture".into(), json!({"configured":true}))
            .wait()
            .unwrap();
        bundled_fixture(&service, packages.path(), manifest(), &bytes);
        service
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        let status = service.list().wait().unwrap().remove(0);
        assert_eq!(status.source, PluginSource::Bundled);
        assert!(status.enabled);
        assert_eq!(status.configuration["configured"], true);

        let other_root = tempfile::tempdir().unwrap();
        let development = self::service(other_root.path());
        development
            .load_development(fixture_directory().to_owned())
            .wait()
            .unwrap();
        bundled_fixture(&development, packages.path(), manifest(), &bytes);
        development
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        assert_eq!(
            development.list().wait().unwrap()[0].source,
            PluginSource::Development
        );
        assert!(!development.manager.root.exists());
    }

    #[test]
    fn missing_or_invalid_bundle_cannot_run_a_persisted_package_or_replace_it() {
        let root = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let executable =
            fs::read(fixture_directory().join(&manifest().executables[platform_target()])).unwrap();
        let bytes = archive_with_executable(&manifest(), &executable);
        bundled_fixture(&service, packages.path(), manifest(), &bytes);
        service
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        service.set_enabled("fixture".into(), true).wait().unwrap();
        assert!(
            service.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_none()
        );
        let registry_before = fs::read(service.manager.root.join("registry.json")).unwrap();
        let directory = lock(&service.manager.registry).installed["fixture"]
            .directory
            .clone();

        let restored = PluginService::new(service.context.clone(), service.catalog());
        assert!(
            restored.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_some()
        );
        fs::write(packages.path().join("package.zip"), b"invalid archive").unwrap();
        assert!(
            service
                .load_bundled(packages.path().to_owned())
                .wait()
                .is_err()
        );
        assert!(
            service.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_some()
        );
        assert!(lock(&service.runtime("fixture")).process.is_none());
        let unavailable = service.list().wait().unwrap().remove(0);
        assert!(unavailable.error.is_some());
        assert!(
            unavailable.enabled,
            "Unavailability must preserve the user's opt-in preference"
        );
        service.retry("fixture".into()).wait().unwrap();
        assert!(
            service.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_some()
        );
        assert_eq!(
            fs::read(service.manager.root.join("registry.json")).unwrap(),
            registry_before
        );
        assert!(
            service
                .manager
                .root
                .join(directory)
                .join("plugin.json")
                .is_file()
        );
        assert!(
            restored
                .load_bundled(packages.path().join("missing"))
                .wait()
                .is_err()
        );
        assert!(
            restored.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_some()
        );

        fs::write(packages.path().join("package.zip"), bytes).unwrap();
        service
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        assert!(
            service.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_none()
        );
        service.shutdown().wait().unwrap();
    }

    #[test]
    fn failed_bundled_migration_blocks_the_previous_downloaded_executable() {
        let root = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let executable =
            fs::read(fixture_directory().join(&manifest().executables[platform_target()])).unwrap();
        let bytes = archive_with_executable(&manifest(), &executable);
        service
            .install_bytes(&catalog(manifest(), &bytes), &bytes)
            .unwrap();
        assert!(
            service.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_none()
        );
        let registry_before = fs::read(service.manager.root.join("registry.json")).unwrap();
        bundled_fixture(&service, packages.path(), manifest(), &bytes);
        fs::write(packages.path().join("package.zip"), b"corrupt").unwrap();
        assert!(
            service
                .load_bundled(packages.path().to_owned())
                .wait()
                .is_err()
        );
        assert!(lock(&service.runtime("fixture")).process.is_none());
        assert!(
            service.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_some()
        );
        let status = service.list().wait().unwrap().remove(0);
        assert_eq!(status.source, PluginSource::Bundled);
        assert!(status.enabled);
        assert!(status.error.is_some());
        assert!(!status.update_available);
        assert!(service.install("fixture".into()).wait().is_err());
        assert!(service.uninstall("fixture".into()).wait().is_err());
        assert_eq!(
            fs::read(service.manager.root.join("registry.json")).unwrap(),
            registry_before
        );
        fs::write(packages.path().join("package.zip"), bytes).unwrap();
        service
            .load_bundled(packages.path().to_owned())
            .wait()
            .unwrap();
        assert!(
            service.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_none()
        );
        service.shutdown().wait().unwrap();
    }

    #[test]
    fn install_configure_update_and_uninstall_are_persistent() {
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let bytes = archive(&manifest());
        service
            .install_bytes(&catalog(manifest(), &bytes), &bytes)
            .unwrap();
        service
            .configure("fixture".into(), json!({"path":"private/config.xml"}))
            .wait()
            .unwrap();
        service.set_enabled("fixture".into(), false).wait().unwrap();
        let restored = PluginService::new(service.context.clone(), vec![]);
        let status = restored.list().wait().unwrap().remove(0);
        assert!(!status.enabled);
        assert_eq!(status.configuration["path"], "private/config.xml");
        let mut updated = manifest();
        updated.version = "2.0.0".into();
        let updated_bytes = archive(&updated);
        service
            .install_bytes(&catalog(updated, &updated_bytes), &updated_bytes)
            .unwrap();
        let status = service.list().wait().unwrap().remove(0);
        assert_eq!(status.manifest.version, "2.0.0");
        assert_eq!(status.configuration["path"], "private/config.xml");
        assert!(
            !status.enabled,
            "Updating must preserve an explicit disable"
        );
        service.uninstall("fixture".into()).wait().unwrap();
        assert!(service.list().wait().unwrap().is_empty());
        assert_eq!(fs::read_dir(&service.manager.root).unwrap().count(), 1); // registry only
    }

    #[test]
    fn installation_progress_is_shared_and_cleared_after_failure() {
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let bytes = archive(&manifest());
        service.set_catalog(vec![catalog(manifest(), &bytes)]);
        let other_window = service.clone();
        lock(&service.manager.installing).insert("fixture".into());
        let activity = InstallationActivity {
            service: service.clone(),
            id: "fixture".into(),
        };
        assert!(other_window.list().wait().unwrap()[0].installing);
        drop(activity);
        let status = other_window.list().wait().unwrap().remove(0);
        assert!(!status.installing);
        assert!(!status.installed);
        assert!(!status.enabled);
    }

    #[test]
    fn failed_updates_preserve_registry_and_working_package() {
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let bytes = archive(&manifest());
        let entry = catalog(manifest(), &bytes);
        service.install_bytes(&entry, &bytes).unwrap();
        let before = fs::read(service.manager.root.join("registry.json")).unwrap();
        let directory = lock(&service.manager.registry).installed["fixture"]
            .directory
            .clone();
        assert!(
            service
                .install_bytes(&entry, &bytes[..bytes.len() / 2])
                .is_err()
        );
        let mut mismatch = entry.clone();
        mismatch.manifest.version = "2.0.0".into();
        assert!(service.install_bytes(&mismatch, &bytes).is_err());
        let mut incompatible = entry.clone();
        incompatible.manifest.protocol_version += 1;
        assert!(service.install_bytes(&incompatible, &bytes).is_err());
        assert_eq!(
            before,
            fs::read(service.manager.root.join("registry.json")).unwrap()
        );
        assert!(
            service
                .manager
                .root
                .join(directory)
                .join("plugin.json")
                .is_file()
        );
        assert_eq!(fs::read_dir(&service.manager.root).unwrap().count(), 2);
    }

    #[test]
    fn package_extraction_rejects_unsafe_paths_and_links() {
        for name in [
            "../escape",
            "/absolute",
            "C:/escape",
            "nested/../../escape",
            "nested\\escape",
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(b"bad").unwrap();
            let bytes = archive.finish().unwrap().into_inner();
            assert!(
                extract_package(&bytes, root.path()).is_err(),
                "accepted {name}"
            );
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        }
        let root = tempfile::tempdir().unwrap();
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        archive
            .add_symlink(
                "link",
                "../outside",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        assert!(extract_package(&archive.finish().unwrap().into_inner(), root.path()).is_err());
    }

    #[test]
    fn corrupt_registry_is_preserved_instead_of_silently_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("config/plugins");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("registry.json"), b"broken").unwrap();
        let service = service(root.path());
        assert!(service.set_enabled("fixture".into(), true).wait().is_err());
        assert!(service.load_bundled(root.path().to_owned()).wait().is_err());
        assert_eq!(
            fs::read(directory.join("registry.json")).unwrap(),
            b"broken"
        );
    }

    #[test]
    fn development_peer_is_explicit_shared_lazy_and_never_persisted_as_official() {
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let other_window = service.clone();
        service
            .load_development(fixture_directory().to_owned())
            .wait()
            .unwrap();
        assert!(
            service
                .inspect(Inspection::default())
                .wait()
                .unwrap()
                .is_empty()
        );
        service.set_enabled("fixture".into(), true).wait().unwrap();
        assert_eq!(
            other_window.list().wait().unwrap()[0].source,
            PluginSource::Development
        );
        assert!(lock(&service.runtime("fixture")).process.is_none());
        let context = Inspection {
            path: root.path().to_owned(),
            generation: 7,
            context_id: 42,
            ..Inspection::default()
        };
        let first = service.inspect(context.clone()).wait().unwrap().remove(0);
        assert!(first.error.is_none(), "{:?}", first.error);
        assert_eq!(
            first.contribution.unwrap().badge.as_deref(),
            Some("Fixture")
        );
        let pid = lock(&service.runtime("fixture"))
            .process
            .as_ref()
            .unwrap()
            .child
            .id();
        let request_count = lock(&service.runtime("fixture"))
            .process
            .as_ref()
            .unwrap()
            .next_id;
        let next = other_window
            .inspect(Inspection {
                generation: 8,
                ..context
            })
            .wait()
            .unwrap()
            .remove(0);
        assert_eq!(next.contribution.unwrap().generation, 8);
        assert_eq!(
            lock(&service.runtime("fixture"))
                .process
                .as_ref()
                .unwrap()
                .next_id,
            request_count + 1,
            "Every generation must reach the provider even for the same path"
        );
        assert_eq!(
            lock(&service.runtime("fixture"))
                .process
                .as_ref()
                .unwrap()
                .child
                .id(),
            pid
        );
        other_window
            .set_enabled("fixture".into(), false)
            .wait()
            .unwrap();
        assert!(lock(&service.runtime("fixture")).process.is_none());
        assert!(
            service
                .inspect(Inspection::default())
                .wait()
                .unwrap()
                .is_empty()
        );
        assert!(
            PluginService::new(service.context.clone(), vec![])
                .list()
                .wait()
                .unwrap()
                .is_empty()
        );
        service.uninstall("fixture".into()).wait().unwrap();
        assert!(fixture_directory().join("plugin.json").exists());
    }

    #[test]
    fn real_process_failures_are_bounded_and_children_are_reaped() {
        for mode in ["crash", "hang", "malformed", "oversized"] {
            let root = tempfile::tempdir().unwrap();
            let service = service(root.path());
            let executable = fixture_directory().join(&manifest().executables[platform_target()]);
            let mut command = Command::new(executable);
            command.arg(mode);
            let mut process =
                PluginProcess::start(command, "fixture".into(), service.context.clone()).unwrap();
            let started = Instant::now();
            let result = process.request("initialize", Value::Null, Duration::from_millis(500));
            assert!(result.is_err(), "{mode} unexpectedly succeeded");
            drop(process);
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{mode} exceeded bound"
            );
        }
    }

    #[test]
    fn retry_clears_failure_and_shutdown_stops_future_starts() {
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        service
            .load_development(fixture_directory().to_owned())
            .wait()
            .unwrap();
        assert!(
            service
                .inspect(Inspection::default())
                .wait()
                .unwrap()
                .is_empty()
        );
        service.set_enabled("fixture".into(), true).wait().unwrap();
        lock(&service.runtime("fixture")).error = Some("failed".into());
        assert!(
            service.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_some()
        );
        service.retry("fixture".into()).wait().unwrap();
        assert!(
            service.inspect(Inspection::default()).wait().unwrap()[0]
                .error
                .is_none()
        );
        service.shutdown().wait().unwrap();
        assert!(lock(&service.runtime("fixture")).process.is_none());
        assert!(
            service
                .inspect(Inspection {
                    force: true,
                    ..Inspection::default()
                })
                .wait()
                .unwrap()[0]
                .error
                .is_some()
        );
    }

    #[test]
    fn configuration_commit_waits_until_the_active_process_is_stopped() {
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        service
            .load_development(fixture_directory().to_owned())
            .wait()
            .unwrap();
        assert!(
            service
                .inspect(Inspection::default())
                .wait()
                .unwrap()
                .is_empty()
        );
        service.set_enabled("fixture".into(), true).wait().unwrap();
        service.inspect(Inspection::default()).wait().unwrap();
        let slot = service.runtime("fixture");
        let runtime = lock(&slot);
        let change = service.configure("fixture".into(), json!({"configured":true}));
        let started = Instant::now();
        loop {
            if service.manager.mutations.try_lock().is_err() {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::yield_now();
        }
        assert_eq!(
            lock(&service.manager.registry).installed["fixture"]
                .preferences
                .configuration,
            Value::Null
        );
        drop(runtime);
        change.wait().unwrap();
        assert_eq!(
            lock(&service.manager.registry).installed["fixture"]
                .preferences
                .configuration["configured"],
            true
        );
        assert!(lock(&slot).process.is_none());
    }

    #[test]
    fn notifications_preserve_each_window_context() {
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let events = service.context.subscribe();
        let executable = fixture_directory().join(&manifest().executables[platform_target()]);
        let mut process = PluginProcess::start(
            Command::new(executable),
            "fixture".into(),
            service.context.clone(),
        )
        .unwrap();
        for context_id in [101, 202] {
            let context = Inspection {
                context_id,
                generation: 3,
                path: root.path().to_owned(),
                ..Inspection::default()
            };
            process
                .request(
                    "inspect",
                    serde_json::to_value(context).unwrap(),
                    Duration::from_secs(10),
                )
                .unwrap();
        }
        let mut seen = Vec::new();
        for _ in 0..2 {
            let ServiceEvent::PluginStatusChanged { contribution, .. } =
                events.recv_timeout(Duration::from_secs(10)).unwrap()
            else {
                panic!("expected plugin update")
            };
            seen.push(contribution.context_id);
            assert_eq!(contribution.generation, 3);
        }
        seen.sort();
        assert_eq!(seen, [101, 202]);
        drop(process);
        assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    }

    #[test]
    #[ignore = "Set EXPLORIE_PLUGIN_SMOKE_CATALOG to a locally built official catalog"]
    fn official_packages_install_and_execute_through_native_manager() {
        real_packages_execute(false);
    }

    #[test]
    #[ignore = "Set EXPLORIE_PLUGIN_SMOKE_CATALOG and optionally EXPLORIE_PLUGIN_SMOKE_DIRECTORY to bundled packages"]
    fn official_bundled_packages_install_and_execute_through_native_manager() {
        real_packages_execute(true);
    }

    fn real_packages_execute(bundled: bool) {
        let catalog_path = PathBuf::from(
            std::env::var_os("EXPLORIE_PLUGIN_SMOKE_CATALOG")
                .expect("Set EXPLORIE_PLUGIN_SMOKE_CATALOG"),
        );
        let entries: Vec<CatalogEntry> =
            serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
        assert_eq!(entries.len(), 3);
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        service.set_catalog(entries.clone());
        if bundled {
            let package_directory = std::env::var_os("EXPLORIE_PLUGIN_SMOKE_DIRECTORY")
                .map(PathBuf::from)
                .unwrap_or_else(|| catalog_path.parent().unwrap().to_owned());
            service.load_bundled(package_directory).wait().unwrap();
            assert!(
                service
                    .list()
                    .wait()
                    .unwrap()
                    .iter()
                    .all(|status| status.installed && !status.enabled)
            );
            assert!(
                service
                    .inspect(Inspection::default())
                    .wait()
                    .unwrap()
                    .is_empty()
            );
            assert!(
                lock(&service.manager.runtimes)
                    .values()
                    .all(|runtime| lock(runtime).process.is_none())
            );
            for entry in &entries {
                service
                    .set_enabled(entry.manifest.id.clone(), true)
                    .wait()
                    .unwrap();
            }
        } else {
            for entry in &entries {
                let filename = entry.asset_url.rsplit('/').next().unwrap();
                assert!(is_root_filename(Path::new(filename)));
                let bytes = fs::read(catalog_path.parent().unwrap().join(filename)).unwrap();
                service.install_bytes(entry, &bytes).unwrap();
            }
        }
        let folder = root.path().join("fixture-folder");
        fs::create_dir_all(folder.join(".stfolder")).unwrap();
        fs::create_dir_all(folder.join(".obsidian")).unwrap();
        let output = Command::new("git")
            .arg("init")
            .arg(&folder)
            .output()
            .unwrap();
        assert!(output.status.success(), "Git fixture init failed");
        fs::write(folder.join("note.md"), "# Integration smoke").unwrap();
        let results = service
            .inspect(Inspection {
                path: folder,
                generation: 17,
                force: true,
                ..Inspection::default()
            })
            .wait()
            .unwrap();
        assert_eq!(results.len(), 3);
        for result in results {
            assert!(result.error.is_none(), "{}: {:?}", result.id, result.error);
            assert!(
                result.contribution.unwrap().badge.is_some(),
                "{} did not detect the fixture",
                result.id
            );
            assert_eq!(
                service
                    .list()
                    .wait()
                    .unwrap()
                    .iter()
                    .find(|status| status.manifest.id == result.id)
                    .unwrap()
                    .source,
                if bundled {
                    PluginSource::Bundled
                } else {
                    PluginSource::Official
                }
            );
        }
        service.shutdown().wait().unwrap();
        assert!(
            lock(&service.manager.runtimes)
                .values()
                .all(|runtime| lock(runtime).process.is_none())
        );
    }
}
