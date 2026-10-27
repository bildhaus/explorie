use super::*;
use explorie_native_services::plugins::{PluginResult, PluginSource, PluginStatus};
use explorie_plugin_protocol::{
    ActionEffect, ActionRequest, CatalogEntry, Contribution, EntryContext, Inspection, SettingKind,
};
use serde_json::{Value, json};

static NEXT_INSPECTION: AtomicU64 = AtomicU64::new(1);
static NEXT_PLUGIN_CONTEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
pub(super) struct PluginUiState {
    pub statuses: Vec<PluginStatus>,
    pub results: BTreeMap<String, PluginResult>,
    pub text_editor: Option<(String, String, String)>,
    pub generation: u64,
    pub context_id: u64,
    pub details_open: bool,
    pub selected: BTreeSet<String>,
    pub configuring: Option<String>,
    pub error: Option<String>,
    pub activation_observed: bool,
    decorations: HashMap<PathBuf, String>,
    tasks: Vec<Task<()>>,
    scan: Option<Task<()>>,
    scan_running: bool,
    scan_pending: Option<bool>,
    listing: bool,
    status_refresh_pending: bool,
    busy: BTreeSet<String>,
    completed_changes: BTreeSet<String>,
}

pub fn initialize_plugins(
    services: &NativeServices,
    args: impl IntoIterator<Item = OsString>,
) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    initialize_plugins_at(services, args, Some(bundled_plugin_directory(&executable)))
}

fn bundled_plugin_directory(executable: &Path) -> PathBuf {
    let parent = executable
        .parent()
        .expect("Executable has a parent directory");
    if parent.file_name().is_some_and(|name| name == "MacOS") {
        parent.join("../Resources/plugins")
    } else {
        parent.join("plugins")
    }
}

fn initialize_plugins_at(
    services: &NativeServices,
    args: impl IntoIterator<Item = OsString>,
    bundled_directory: Option<PathBuf>,
) -> Result<(), String> {
    let mut catalog: Vec<CatalogEntry> = serde_json::from_str(include_str!(concat!(
        env!("OUT_DIR"),
        "/plugin-catalog.json"
    )))
    .map_err(|_| "Invalid embedded plugin catalog")?;
    // Debug builds expose the catalog descriptions without offering unverified downloads.
    if catalog.is_empty() {
        for source in [
            include_str!("../../../../plugins/syncthing/plugin.json"),
            include_str!("../../../../plugins/git/plugin.json"),
            include_str!("../../../../plugins/obsidian/plugin.json"),
        ] {
            catalog.push(CatalogEntry {
                manifest: serde_json::from_str(source)
                    .map_err(|_| "Invalid integration manifest")?,
                target: if cfg!(windows) {
                    "x86_64-pc-windows-msvc"
                } else {
                    "aarch64-apple-darwin"
                }
                .into(),
                asset_url: String::new(),
                sha256: String::new(),
            });
        }
    }
    services.plugins.set_catalog(catalog);
    if let Some(directory) = bundled_directory
        && (directory.is_dir() || !cfg!(debug_assertions))
    {
        services
            .plugins
            .load_bundled(directory)
            .wait()
            .map_err(|e| e.to_string())?;
    }
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--load-plugin" {
            let path = args.next().ok_or("--load-plugin requires a directory")?;
            services
                .plugins
                .load_development(PathBuf::from(path))
                .wait()
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

impl DirectoryWindow {
    pub fn announce_plugin_startup_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.plugin_ui.error = Some(error.clone());
        self.status_message = Some(error);
        cx.notify();
    }

    fn plugin_context(&self, force: bool) -> Inspection {
        Inspection {
            context_id: self.plugin_ui.context_id,
            path: self.browser.path().to_path_buf(),
            entries: self
                .browser
                .entries()
                .iter()
                .map(|entry| EntryContext {
                    path: entry.path.clone(),
                    is_dir: entry.is_dir,
                })
                .collect(),
            selected: self.browser.selected_paths(),
            generation: self.plugin_ui.generation,
            force,
        }
    }

    pub(super) fn clear_plugin_context(&mut self, cx: &mut Context<Self>) {
        self.plugin_ui.generation = NEXT_INSPECTION.fetch_add(1, Ordering::Relaxed);
        self.plugin_ui.results.clear();
        self.plugin_ui.decorations.clear();
        self.plugin_ui.details_open = false;
        self.plugin_ui.scan_pending = None;
        cx.notify();
    }

    pub(super) fn start_plugin_status(&mut self, cx: &mut Context<Self>) {
        if self.plugin_ui.listing {
            self.plugin_ui.status_refresh_pending = true;
            return;
        }
        self.plugin_ui.listing = true;
        let task = self.services.plugins.list();
        self.plugin_ui.tasks.retain(|task| !task.is_ready());
        self.plugin_ui.tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.plugin_ui.listing = false;
                if std::mem::take(&mut view.plugin_ui.status_refresh_pending) {
                    view.start_plugin_status(cx);
                    return;
                }
                for id in std::mem::take(&mut view.plugin_ui.completed_changes) {
                    view.plugin_ui.busy.remove(&id);
                }
                match result {
                    Ok(statuses) => {
                        view.plugin_ui.results.retain(|id, _| {
                            statuses.iter().any(|s| &s.manifest.id == id && s.enabled)
                        });
                        view.plugin_ui.statuses = statuses;
                        view.rebuild_plugin_decorations();
                        view.start_plugin_scan(false, cx);
                    }
                    Err(error) => view.plugin_ui.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn start_plugin_scan(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.plugin_ui.statuses.iter().any(|s| s.enabled) {
            return;
        }
        if self.plugin_ui.context_id == 0 {
            self.plugin_ui.context_id = NEXT_PLUGIN_CONTEXT.fetch_add(1, Ordering::Relaxed);
        }
        self.plugin_ui.generation = NEXT_INSPECTION.fetch_add(1, Ordering::Relaxed);
        if self.plugin_ui.scan_running {
            self.plugin_ui.scan_pending =
                Some(force || self.plugin_ui.scan_pending.unwrap_or(false));
            return;
        }
        self.plugin_ui.scan_running = true;
        let service = self.services.plugins.clone();
        let executor = cx.background_executor().clone();
        self.plugin_ui.scan = Some(cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(80)).await;
            let Ok(context) = this.update(cx, |view, _| {
                let force = force || view.plugin_ui.scan_pending.take().unwrap_or(false);
                view.plugin_context(force)
            }) else {
                return;
            };
            let generation = context.generation;
            let path = context.path.clone();
            let result = service.inspect(context).await;
            let _ = this.update(cx, |view, cx| {
                view.plugin_ui.scan_running = false;
                if generation == view.plugin_ui.generation && path == view.browser.path() {
                    match result {
                        Ok(results) => {
                            view.plugin_ui.results = results
                                .into_iter()
                                .filter(|r| {
                                    view.plugin_ui
                                        .statuses
                                        .iter()
                                        .any(|s| s.enabled && s.manifest.id == r.id)
                                })
                                .map(|r| (r.id.clone(), r))
                                .collect()
                        }
                        Err(error) => view.plugin_ui.error = Some(error.to_string()),
                    }
                    view.rebuild_plugin_decorations();
                }
                if let Some(force) = view.plugin_ui.scan_pending.take() {
                    view.start_plugin_scan(force, cx);
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn apply_plugin_contribution(
        &mut self,
        id: String,
        contribution: Contribution,
        cx: &mut Context<Self>,
    ) {
        if contribution.context_id != self.plugin_ui.context_id
            || contribution.generation != self.plugin_ui.generation
            || contribution.path != self.browser.path()
            || !self
                .plugin_ui
                .statuses
                .iter()
                .any(|s| s.manifest.id == id && s.enabled)
        {
            return;
        }
        self.plugin_ui.results.insert(
            id.clone(),
            PluginResult {
                id,
                contribution: Some(contribution),
                error: None,
            },
        );
        self.rebuild_plugin_decorations();
        cx.notify();
    }

    fn run_plugin_change(&mut self, id: String, task: BlockingTask<()>, cx: &mut Context<Self>) {
        self.plugin_ui.busy.insert(id.clone());
        self.plugin_ui.error = None;
        self.plugin_ui.tasks.retain(|task| !task.is_ready());
        self.plugin_ui.tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.plugin_ui.completed_changes.insert(id.clone());
                if let Err(error) = result {
                    view.plugin_ui.error = Some(error.to_string());
                }
                view.start_plugin_status(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn plugin_configuration(&self, id: &str) -> Value {
        self.plugin_ui
            .statuses
            .iter()
            .find(|s| s.manifest.id == id)
            .map(|s| s.configuration.clone())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    }

    fn set_plugin_setting(
        &mut self,
        id: String,
        key: String,
        value: Value,
        cx: &mut Context<Self>,
    ) {
        if self.plugin_ui.busy.contains(&id) {
            return;
        }
        let mut configuration = self.plugin_configuration(&id);
        configuration[key] = value;
        let task = self.services.plugins.configure(id.clone(), configuration);
        self.run_plugin_change(id, task, cx);
    }

    fn pick_plugin_path(
        &mut self,
        id: String,
        key: String,
        directory: bool,
        cx: &mut Context<Self>,
    ) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: false,
            prompt: Some("Choose integration path".into()),
        });
        self.plugin_ui.tasks.push(cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && let Some(path) = paths.first()
            {
                let value = Value::String(path.to_string_lossy().into_owned());
                let _ = this.update(cx, |view, cx| view.set_plugin_setting(id, key, value, cx));
            }
        }));
    }

    fn invoke_plugin(&mut self, id: String, action_id: String, cx: &mut Context<Self>) {
        let task = self.services.plugins.invoke(
            id,
            ActionRequest {
                action_id,
                context: self.plugin_context(false),
            },
        );
        let service = self.services.plugins.clone();
        self.plugin_ui.tasks.push(cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(ActionEffect::OpenUrl(url)) => {
                    service.open_url(url).await.map(|()| ActionEffect::None)
                }
                other => other,
            };
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(ActionEffect::CopyText(text)) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        view.show_toast("Copied integration link", ToastKind::Success, cx);
                    }
                    Ok(_) => view.start_plugin_scan(true, cx),
                    Err(error) => view.plugin_ui.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn plugin_entry_decoration(&self, path: &Path) -> Option<String> {
        self.plugin_ui.decorations.get(path).cloned()
    }

    fn rebuild_plugin_decorations(&mut self) {
        self.plugin_ui.decorations.clear();
        for decoration in self
            .plugin_ui
            .results
            .values()
            .filter_map(|r| r.contribution.as_ref())
            .flat_map(|c| &c.decorations)
        {
            self.plugin_ui
                .decorations
                .entry(decoration.path.clone())
                .and_modify(|label| {
                    label.push_str(" · ");
                    label.push_str(&decoration.label);
                })
                .or_insert_with(|| decoration.label.clone());
        }
    }

    pub(super) fn render_plugin_badges(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut badges = Vec::new();
        for status in &self.plugin_ui.statuses {
            let Some(result) = self.plugin_ui.results.get(&status.manifest.id) else {
                continue;
            };
            let detected = result
                .contribution
                .as_ref()
                .is_some_and(|c| c.badge.is_some());
            if !detected && result.error.is_none() {
                continue;
            }
            let id = status.manifest.id.clone();
            let selector = format!("plugin-badge-{id}");
            let name = status.manifest.name.clone();
            badges.push(
                toolbar_button(
                    ElementId::Name(selector.clone().into()),
                    &name,
                    self.palette.selected,
                )
                .debug_selector(move || selector.clone())
                .h(px(24.0))
                .px_2()
                .text_xs()
                .on_click(cx.listener(|view, _, _, cx| {
                    view.plugin_ui.details_open = !view.plugin_ui.details_open;
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        div()
            .id("plugin-badges")
            .debug_selector(|| "plugin-badges".into())
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .children(badges)
            .into_any_element()
    }

    pub(super) fn render_plugin_details(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.plugin_ui.details_open || self.settings_ui.panel_open {
            return div().into_any_element();
        }
        let mut sections = Vec::new();
        for (id, result) in &self.plugin_ui.results {
            let mut rows = Vec::new();
            if let Some(error) = &result.error {
                rows.push(div().text_sm().child(error.clone()).into_any_element());
            }
            if let Some(contribution) = &result.contribution {
                if contribution.badge.is_none() {
                    continue;
                }
                rows.push(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(contribution.badge.clone().unwrap_or_default())
                        .into_any_element(),
                );
                for detail in &contribution.details {
                    rows.push(
                        div()
                            .text_xs()
                            .child(format!("{}: {}", detail.label, detail.value))
                            .into_any_element(),
                    );
                }
                if contribution.observed_at > 0 {
                    rows.push(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format!(
                                "Updated {}",
                                format_modified(
                                    UNIX_EPOCH + Duration::from_secs(contribution.observed_at)
                                )
                            ))
                            .into_any_element(),
                    );
                }
                for action in &contribution.actions {
                    let plugin = id.clone();
                    let action_id = action.id.clone();
                    rows.push(
                        toolbar_button(
                            ElementId::Name(format!("plugin-action-{id}-{}", action.id).into()),
                            &action.label,
                            self.palette.control,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.invoke_plugin(plugin.clone(), action_id.clone(), cx)
                        }))
                        .into_any_element(),
                    );
                }
            }
            let plugin = id.clone();
            rows.push(
                toolbar_button(
                    ElementId::Name(format!("plugin-retry-{id}").into()),
                    "Refresh / Retry",
                    self.palette.control,
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    let task = view.services.plugins.retry(plugin.clone());
                    view.run_plugin_change(plugin.clone(), task, cx);
                }))
                .into_any_element(),
            );
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .pb_3()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .children(rows)
                    .into_any_element(),
            );
        }
        div()
            .id("plugin-details")
            .debug_selector(|| "plugin-details".into())
            .role(Role::Dialog)
            .aria_label("Folder integrations")
            .absolute()
            .top(px(110.0))
            .right(px(16.0))
            .w(px(340.0))
            .max_h(px(430.0))
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .bg(self.palette.surface)
            .text_color(self.palette.text)
            .border_1()
            .border_color(self.palette.border)
            .rounded_md()
            .shadow_lg()
            .occlude()
            .child(
                toolbar_button("close-plugin-details", "Close", self.palette.control).on_click(
                    cx.listener(|view, _, _, cx| {
                        view.plugin_ui.details_open = false;
                        cx.notify();
                    }),
                ),
            )
            .when_some(self.plugin_ui.error.clone(), |panel, error| {
                panel.child(div().text_sm().child(error))
            })
            .children(sections)
            .into_any_element()
    }

    pub(super) fn render_plugin_invitation(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.settings.integrations_onboarding_complete {
            return div().into_any_element();
        }
        div()
            .id("integrations-invitation")
            .debug_selector(|| "integrations-invitation".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .bg(self.palette.surface)
            .text_xs()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child("Add optional folder integrations"),
            )
            .child(
                toolbar_button(
                    "choose-integrations",
                    "Choose integrations",
                    self.palette.control,
                )
                .on_click(cx.listener(|view, _, _, cx| {
                    view.settings_ui.panel_open = true;
                    view.settings_ui.tab = SettingsTab::Plugins;
                    view.start_plugin_status(cx);
                    cx.notify();
                })),
            )
            .child(
                toolbar_button("skip-integrations", "Skip", self.palette.control).on_click(
                    cx.listener(|view, _, _, cx| {
                        view.settings.integrations_onboarding_complete = true;
                        view.persist_settings();
                        cx.notify();
                    }),
                ),
            )
            .into_any_element()
    }

    pub(super) fn render_plugin_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut cards = Vec::new();
        for status in self.plugin_ui.statuses.clone() {
            let id = status.manifest.id.clone();
            let busy = status.installing || self.plugin_ui.busy.contains(&id);
            let source = match status.source {
                PluginSource::Bundled => "Included with Explorie",
                PluginSource::Official => "Official",
                PluginSource::Development => "Development — local trusted code",
            };
            let mut buttons = Vec::new();
            if !status.enabled && (status.installed || status.source != PluginSource::Bundled) {
                let selected = self.plugin_ui.selected.contains(&id);
                let select_id = id.clone();
                buttons.push(
                    toolbar_button(
                        ElementId::Name(format!("plugin-select-{id}").into()),
                        if selected {
                            "☑ Selected"
                        } else {
                            "☐ Select"
                        },
                        self.palette.control,
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if !view.plugin_ui.selected.remove(&select_id) {
                            view.plugin_ui.selected.insert(select_id.clone());
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                );
            }
            if !status.installed && status.source != PluginSource::Bundled {
                let install_id = id.clone();
                let install_selector = format!("plugin-install-{id}");
                buttons.push(
                    toolbar_button_enabled(
                        ElementId::Name(format!("plugin-install-{id}").into()),
                        "Install integration",
                        self.palette.control,
                        !busy,
                    )
                    .debug_selector(move || install_selector.clone())
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if view.plugin_ui.busy.contains(&install_id) {
                            return;
                        }
                        let task = view.services.plugins.install(install_id.clone());
                        view.run_plugin_change(install_id.clone(), task, cx);
                    }))
                    .into_any_element(),
                );
            } else if status.installed {
                let enable_id = id.clone();
                let enabled = status.enabled;
                let enable_selector = format!("plugin-enable-{id}");
                buttons.push(
                    toolbar_button_enabled(
                        ElementId::Name(format!("plugin-enable-{id}").into()),
                        if enabled {
                            "Disable integration"
                        } else {
                            "Enable integration"
                        },
                        self.palette.control,
                        !busy,
                    )
                    .debug_selector(move || enable_selector.clone())
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if view.plugin_ui.busy.contains(&enable_id) {
                            return;
                        }
                        if enabled {
                            view.plugin_ui.results.remove(&enable_id);
                            if let Some(status) = view
                                .plugin_ui
                                .statuses
                                .iter_mut()
                                .find(|s| s.manifest.id == enable_id)
                            {
                                status.enabled = false;
                            }
                            view.rebuild_plugin_decorations();
                        } else if !view.settings.integrations_onboarding_complete {
                            // Enabling an integration is the choice the
                            // invitation asks for, so it stops asking.
                            view.settings.integrations_onboarding_complete = true;
                            view.persist_settings();
                        }
                        let task = view
                            .services
                            .plugins
                            .set_enabled(enable_id.clone(), !enabled);
                        view.run_plugin_change(enable_id.clone(), task, cx);
                    }))
                    .into_any_element(),
                );
                let configure_id = id.clone();
                buttons.push(
                    toolbar_button(
                        ElementId::Name(format!("plugin-configure-{id}").into()),
                        "Configure",
                        self.palette.control,
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.plugin_ui.configuring = Some(configure_id.clone());
                        cx.notify();
                    }))
                    .into_any_element(),
                );
                if status.source != PluginSource::Bundled {
                    let uninstall_id = id.clone();
                    let uninstall_selector = format!("plugin-uninstall-{id}");
                    buttons.push(
                        toolbar_button_enabled(
                            ElementId::Name(format!("plugin-uninstall-{id}").into()),
                            "Uninstall",
                            self.palette.control,
                            !busy,
                        )
                        .debug_selector(move || uninstall_selector.clone())
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if view.plugin_ui.busy.contains(&uninstall_id) {
                                return;
                            }
                            view.plugin_ui.results.remove(&uninstall_id);
                            if let Some(status) = view
                                .plugin_ui
                                .statuses
                                .iter_mut()
                                .find(|s| s.manifest.id == uninstall_id)
                            {
                                status.enabled = false;
                            }
                            view.rebuild_plugin_decorations();
                            let task = view.services.plugins.uninstall(uninstall_id.clone());
                            view.run_plugin_change(uninstall_id.clone(), task, cx);
                        }))
                        .into_any_element(),
                    );
                }
                if status.update_available {
                    let update_id = id.clone();
                    buttons.push(
                        toolbar_button_enabled(
                            ElementId::Name(format!("plugin-update-{id}").into()),
                            "Update",
                            self.palette.control,
                            !busy,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if view.plugin_ui.busy.contains(&update_id) {
                                return;
                            }
                            let task = view.services.plugins.install(update_id.clone());
                            view.run_plugin_change(update_id.clone(), task, cx);
                        }))
                        .into_any_element(),
                    );
                }
            }
            let mut fields = Vec::new();
            if self.plugin_ui.configuring.as_deref() == Some(&id) && status.installed {
                if status.manifest.settings.is_empty() {
                    fields.push(
                        div()
                            .text_xs()
                            .child("No configuration required.")
                            .into_any_element(),
                    );
                }
                for setting in &status.manifest.settings {
                    let plugin_id = id.clone();
                    let key = setting.key.clone();
                    let value = status
                        .configuration
                        .get(&key)
                        .cloned()
                        .unwrap_or(Value::Null);
                    let field_id = ElementId::Name(format!("plugin-setting-{id}-{key}").into());
                    let button = match setting.kind {
                        SettingKind::Boolean => {
                            let enabled = value.as_bool().unwrap_or(false);
                            let label = if key == "connected" {
                                if enabled {
                                    "Disconnect".into()
                                } else {
                                    "Connect using local config".into()
                                }
                            } else {
                                format!("{}: {}", setting.label, on_off(enabled))
                            };
                            toolbar_button(field_id, &label, self.palette.control)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.set_plugin_setting(
                                        plugin_id.clone(),
                                        key.clone(),
                                        json!(!enabled),
                                        cx,
                                    );
                                }))
                                .into_any_element()
                        }
                        SettingKind::File | SettingKind::Directory => {
                            let directory = setting.kind == SettingKind::Directory;
                            toolbar_button(
                                field_id,
                                &format!("Choose {}", setting.label),
                                self.palette.control,
                            )
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.pick_plugin_path(
                                    plugin_id.clone(),
                                    key.clone(),
                                    directory,
                                    cx,
                                );
                            }))
                            .into_any_element()
                        }
                        SettingKind::Text => toolbar_button(
                            field_id,
                            &format!("Edit {}", setting.label),
                            self.palette.control,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            let value = view
                                .plugin_configuration(&plugin_id)
                                .get(&key)
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            view.plugin_ui.text_editor =
                                Some((plugin_id.clone(), key.clone(), value.clone()));
                            view.activate_native_text_input(
                                TextInputTarget::PluginSetting,
                                value,
                                "Setting value",
                                "Integration setting",
                                cx,
                            );
                        }))
                        .into_any_element(),
                    };
                    let mut row = div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_xs().child(setting.description.clone()))
                        .child(button);
                    if let Some(value) = value.as_str().filter(|s| !s.is_empty()) {
                        let clear_id = id.clone();
                        let clear_key = setting.key.clone();
                        row = row
                            .child(div().text_xs().whitespace_normal().child(value.to_string()))
                            .child(
                                toolbar_button(
                                    ElementId::Name(
                                        format!("plugin-clear-{id}-{}", setting.key).into(),
                                    ),
                                    "Use default",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        view.set_plugin_setting(
                                            clear_id.clone(),
                                            clear_key.clone(),
                                            Value::String(String::new()),
                                            cx,
                                        )
                                    },
                                )),
                            );
                    }
                    fields.push(row.into_any_element());
                }
            }
            let live_status = self.plugin_ui.results.get(&id).and_then(|result| {
                result
                    .error
                    .clone()
                    .or_else(|| result.contribution.as_ref().and_then(|c| c.badge.clone()))
            });
            let connection_status = (id == "syncthing").then(|| {
                if !status.enabled
                    || status
                        .configuration
                        .get("connected")
                        .and_then(Value::as_bool)
                        != Some(true)
                {
                    "Connection: Not connected".to_owned()
                } else {
                    self.plugin_ui
                        .results
                        .get(&id)
                        .and_then(|result| result.contribution.as_ref())
                        .and_then(|contribution| {
                            contribution.details.iter().find(|detail| {
                                detail.label == "Connection" || detail.label == "Local state"
                            })
                        })
                        .map(|detail| format!("{}: {}", detail.label, detail.value))
                        .unwrap_or_else(|| "Connection: Enabled · Awaiting status".into())
                }
            });
            let state = if busy {
                "Working…"
            } else if status.enabled {
                "Enabled"
            } else if status.installed {
                "Disabled"
            } else if status.source == PluginSource::Bundled {
                "Integration unavailable"
            } else {
                "Integration not installed"
            };
            let card_id = id.clone();
            cards.push(
                div()
                    .id(ElementId::Name(format!("plugin-card-{id}").into()))
                    .debug_selector(move || format!("plugin-card-{card_id}"))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .border_1()
                    .border_color(self.palette.border)
                    .rounded_md()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                        "{} · {}",
                        status.manifest.name, status.manifest.version
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format!("{source} · {state}")),
                    )
                    .child(div().text_sm().child(status.manifest.description))
                    .when_some(status.dependency_status, |card, detection| {
                        card.child(
                            div()
                                .id(ElementId::Name(format!("plugin-detection-{id}").into()))
                                .debug_selector(move || format!("plugin-detection-{id}"))
                                .text_xs()
                                .child(detection),
                        )
                    })
                    .children(
                        status
                            .manifest
                            .capabilities
                            .into_iter()
                            .map(|c| div().text_xs().child(c).into_any_element()),
                    )
                    .when(!status.manifest.dependencies.is_empty(), |card| {
                        card.child(div().text_xs().child(format!(
                            "Requires: {}",
                            status.manifest.dependencies.join(", ")
                        )))
                    })
                    .when_some(status.error, |card, error| {
                        card.child(div().text_sm().child(error))
                    })
                    .when_some(live_status, |card, status| {
                        card.child(div().text_xs().child(status))
                    })
                    .when_some(connection_status, |card, status| {
                        card.child(div().text_xs().child(status))
                    })
                    .child(div().flex().flex_wrap().gap_2().children(buttons))
                    .children(fields)
                    .into_any_element(),
            );
        }
        let text_input = self.native_text_input_element(TextInputTarget::PluginSetting);
        div().id("settings-plugins").debug_selector(|| "settings-plugins".into()).flex().flex_col().gap_3().p_3()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Integrations"))
            .child(div().text_sm().child("Git, Obsidian and Syncthing integrations are included. Enable the ones you want to use with your existing apps. They run with your account's access and stay off until enabled."))
            .when_some(self.plugin_ui.error.clone(), |panel, error| panel.child(div().text_sm().child(error)))
            .children(cards)
            .when_some(text_input, |panel, input| panel.child(input).child(toolbar_button("save-plugin-setting", "Save setting", self.palette.control)
                .on_click(cx.listener(|view, _, _, cx| {
                    if let Some((id, key, value)) = view.plugin_ui.text_editor.take() { view.set_plugin_setting(id, key, Value::String(value), cx); }
                    view.deactivate_native_text_input(); cx.notify();
                }))))
            .child(div().flex().flex_wrap().gap_2()
                .child(toolbar_button("enable-selected-plugins", "Enable selected", self.palette.control).on_click(cx.listener(|view, _, _, cx| {
                    for id in std::mem::take(&mut view.plugin_ui.selected) {
                        if !view.plugin_ui.busy.contains(&id) {
                            let task = if view.plugin_ui.statuses.iter().any(|s| s.manifest.id == id && s.installed) {
                                view.services.plugins.set_enabled(id.clone(), true)
                            } else { view.services.plugins.install(id.clone()) };
                            view.run_plugin_change(id, task, cx);
                        }
                    }
                    view.settings.integrations_onboarding_complete = true; view.persist_settings(); cx.notify();
                })))
                .child(toolbar_button("finish-plugin-onboarding", "Done / Skip", self.palette.control).on_click(cx.listener(|view, _, _, cx| {
                    view.settings.integrations_onboarding_complete = true; view.persist_settings(); view.close_settings_panel(cx);
                })))
                .child(toolbar_button("plugin-remote-drives", "Remote Drives", self.palette.control).on_click(cx.listener(|view, _, _, cx| {
                    view.close_settings_panel(cx); view.open_control_surface(ControlSurface::RemoteDrives, cx);
                })))).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorie_native_services::ResourcePaths;
    use gpui::TestAppContext;

    fn test_services() -> (PathBuf, NativeServices) {
        let root =
            std::env::temp_dir().join(format!("explorie-integrations-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let services = NativeServices::new(ResourcePaths::test(&root));
        initialize_plugins_at(&services, [OsString::from("explorie")], None).unwrap();
        (root, services)
    }

    #[gpui::test]
    fn plugin_onboarding_defaults_off_and_settings_are_native(cx: &mut TestAppContext) {
        let (root, services) = test_services();
        let statuses = services.plugins.list().wait().unwrap();
        assert_eq!(statuses.len(), 3);
        assert!(statuses.iter().all(|s| !s.enabled && !s.installed));
        let (view, window) = cx.add_window_view(|_, cx| {
            let mut view = DirectoryWindow::new(root.clone(), services.clone(), cx);
            view.plugin_ui.statuses = statuses;
            view
        });
        window.simulate_resize(gpui::size(px(1000.0), px(720.0)));
        window.run_until_parked();
        assert!(window.debug_bounds("integrations-invitation").is_some());
        view.update(window, |view, cx| {
            assert!(view.plugin_ui.selected.is_empty());
            view.settings_ui.panel_open = true;
            view.settings_ui.tab = SettingsTab::Plugins;
            cx.notify();
        });
        window.run_until_parked();
        for selector in [
            "settings-plugins",
            "plugin-card-syncthing",
            "plugin-card-git",
            "plugin-card-obsidian",
        ] {
            assert!(
                window.debug_bounds(selector).is_some(),
                "Missing native integration control {selector}"
            );
        }
        assert!(
            services
                .plugins
                .list()
                .wait()
                .unwrap()
                .iter()
                .all(|s| !s.enabled && !s.installed)
        );
        view.update(window, |view, cx| {
            view.settings.integrations_onboarding_complete = true;
            view.settings_ui.panel_open = false;
            cx.notify();
        });
        window.run_until_parked();
        assert!(window.debug_bounds("integrations-invitation").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn enabling_one_integration_dismisses_the_invitation(cx: &mut TestAppContext) {
        let (root, services) = test_services();
        let mut statuses = services.plugins.list().wait().unwrap();
        for status in &mut statuses {
            status.source = PluginSource::Bundled;
            status.installed = true;
        }
        let (view, window) = cx.add_window_view(|_, cx| {
            let mut view = DirectoryWindow::new(root.clone(), services.clone(), cx);
            view.plugin_ui.statuses = statuses;
            view
        });
        window.simulate_resize(gpui::size(px(1000.0), px(720.0)));
        window.run_until_parked();
        assert!(window.debug_bounds("integrations-invitation").is_some());
        view.update(window, |view, cx| {
            view.settings_ui.panel_open = true;
            view.settings_ui.tab = SettingsTab::Plugins;
            cx.notify();
        });
        window.run_until_parked();
        let enable = window.debug_bounds("plugin-enable-git").unwrap().center();
        window.simulate_click(enable, gpui::Modifiers::default());
        window.run_until_parked();
        view.update(window, |view, cx| {
            assert!(view.settings.integrations_onboarding_complete);
            view.settings_ui.panel_open = false;
            cx.notify();
        });
        window.run_until_parked();
        assert!(window.debug_bounds("integrations-invitation").is_none());
        view.update(window, |view, _| view.plugin_ui.statuses.clear());
        services.plugins.shutdown().wait().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn bundled_settings_offer_enablement_and_separate_detection(cx: &mut TestAppContext) {
        let (root, services) = test_services();
        let mut statuses = services.plugins.list().wait().unwrap();
        for status in &mut statuses {
            status.source = PluginSource::Bundled;
            status.installed = true;
        }
        let (_, window) = cx.add_window_view(|_, cx| {
            let mut view = DirectoryWindow::new(root.clone(), services, cx);
            view.plugin_ui.statuses = statuses;
            view.settings_ui.panel_open = true;
            view.settings_ui.tab = SettingsTab::Plugins;
            view
        });
        window.simulate_resize(gpui::size(px(1100.0), px(1000.0)));
        window.run_until_parked();
        for selector in [
            "plugin-enable-git",
            "plugin-enable-obsidian",
            "plugin-enable-syncthing",
            "plugin-detection-git",
            "plugin-detection-obsidian",
            "plugin-detection-syncthing",
        ] {
            assert!(
                window.debug_bounds(selector).is_some(),
                "Missing {selector}"
            );
        }
        for selector in [
            "plugin-install-git",
            "plugin-install-obsidian",
            "plugin-install-syncthing",
            "plugin-uninstall-git",
            "plugin-uninstall-obsidian",
            "plugin-uninstall-syncthing",
        ] {
            assert!(
                window.debug_bounds(selector).is_none(),
                "Unexpected {selector}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundled_paths_match_installer_layouts() {
        assert_eq!(
            bundled_plugin_directory(Path::new(
                "/Applications/explorie.app/Contents/MacOS/explorie-gpui"
            )),
            PathBuf::from("/Applications/explorie.app/Contents/MacOS/../Resources/plugins")
        );
        assert_eq!(
            bundled_plugin_directory(Path::new("/apps/explorie/Explorie.exe")),
            PathBuf::from("/apps/explorie/plugins")
        );
    }

    #[gpui::test]
    fn overlapping_plugin_badges_preserve_views_and_ignore_stale_updates(cx: &mut TestAppContext) {
        let (root, services) = test_services();
        let mut statuses = services.plugins.list().wait().unwrap();
        for status in &mut statuses {
            status.installed = true;
            status.enabled = true;
        }
        let (view, window) = cx.add_window_view(|_, cx| {
            let mut view = DirectoryWindow::new(root.clone(), services, cx);
            view.plugin_ui.activation_observed = true;
            view.plugin_ui.context_id = 7;
            view.plugin_ui.generation = 11;
            view.plugin_ui.statuses = statuses;
            view.settings.integrations_onboarding_complete = true;
            view.listing.state = ListingState::Ready;
            view
        });
        view.update(window, |view, cx| {
            for id in ["syncthing", "git", "obsidian"] {
                view.apply_plugin_contribution(
                    id.into(),
                    Contribution {
                        context_id: 7,
                        generation: 11,
                        path: root.clone(),
                        root: Some(root.clone()),
                        badge: Some(id.into()),
                        decorations: vec![explorie_plugin_protocol::EntryDecoration {
                            path: root.join("note.md"),
                            label: id.into(),
                        }],
                        ..Contribution::default()
                    },
                    cx,
                );
            }
            assert!(
                view.plugin_entry_decoration(&root.join("note.md"))
                    .unwrap()
                    .contains("git")
            );
            view.apply_plugin_contribution(
                "git".into(),
                Contribution {
                    context_id: 8,
                    generation: 11,
                    path: root.clone(),
                    badge: Some("wrong window".into()),
                    ..Contribution::default()
                },
                cx,
            );
            view.apply_plugin_contribution(
                "git".into(),
                Contribution {
                    context_id: 7,
                    generation: 10,
                    path: root.clone(),
                    badge: Some("old navigation".into()),
                    ..Contribution::default()
                },
                cx,
            );
            assert_eq!(
                view.plugin_ui.results["git"]
                    .contribution
                    .as_ref()
                    .unwrap()
                    .badge
                    .as_deref(),
                Some("git")
            );
        });
        for width in [1200.0, 800.0] {
            window.simulate_resize(gpui::size(px(width), px(720.0)));
            for mode in [ViewMode::List, ViewMode::Grid, ViewMode::Column] {
                view.update(window, |view, cx| {
                    view.browser.set_view_mode(mode);
                    cx.notify();
                });
                window.run_until_parked();
                let toolbar = window.debug_bounds("browser-toolbar").unwrap();
                for selector in [
                    "plugin-badge-syncthing",
                    "plugin-badge-git",
                    "plugin-badge-obsidian",
                ] {
                    let badge = window.debug_bounds(selector).unwrap();
                    assert!(
                        badge.left() >= toolbar.left() && badge.right() <= toolbar.right(),
                        "{selector} escaped {mode:?} toolbar at {width}"
                    );
                }
                assert!(window.debug_bounds("create-menu-button").is_some());
            }
        }
        let badge = window.debug_bounds("plugin-badge-git").unwrap();
        window.simulate_click(badge.center(), gpui::Modifiers::default());
        window.run_until_parked();
        assert!(window.debug_bounds("plugin-details").is_some());
        view.update(window, |view, cx| {
            view.clear_plugin_context(cx);
            assert!(
                view.plugin_entry_decoration(&root.join("note.md"))
                    .is_none()
            );
        });
        window.run_until_parked();
        assert!(window.debug_bounds("plugin-badge-git").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn developer_argument_does_not_become_the_browsing_directory() {
        let (root, _) = test_services();
        assert!(
            parse_startup_path([
                OsString::from("explorie"),
                OsString::from("--load-plugin"),
                root.clone().into_os_string()
            ])
            .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    #[ignore = "requires packaged integrations via EXPLORIE_PLUGIN_SMOKE_CATALOG and EXPLORIE_PLUGIN_SMOKE_DIRECTORY"]
    fn bundled_integrations_do_not_stall_column_navigation(cx: &mut TestAppContext) {
        let catalog = std::env::var_os("EXPLORIE_PLUGIN_SMOKE_CATALOG")
            .expect("set the packaged integration catalog path");
        let packages = std::env::var_os("EXPLORIE_PLUGIN_SMOKE_DIRECTORY")
            .expect("set the bundled integration package directory");
        let root = std::env::temp_dir().join(format!(
            "explorie-plugin-navigation-{}",
            uuid::Uuid::new_v4()
        ));
        let folder = root.join("vault");
        let nested = folder.join("notes");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(folder.join(".obsidian")).unwrap();
        std::fs::write(nested.join("note.md"), "fixture note").unwrap();
        let mut git = std::process::Command::new("git");
        git.args(["init", "--quiet"]).arg(&folder);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            git.creation_flags(0x08000000);
        }
        assert!(
            git.output()
                .expect("Git is required for this smoke test")
                .status
                .success()
        );
        let services = NativeServices::new(ResourcePaths::test(&root));
        services
            .plugins
            .set_catalog(serde_json::from_slice(&std::fs::read(catalog).unwrap()).unwrap());
        services
            .plugins
            .load_bundled(packages.into())
            .wait()
            .unwrap();
        for id in ["git", "obsidian", "syncthing"] {
            services
                .plugins
                .set_enabled(id.into(), true)
                .wait()
                .unwrap();
        }
        let statuses = services.plugins.list().wait().unwrap();
        assert_eq!(statuses.len(), 3);
        assert!(
            statuses
                .iter()
                .all(|status| status.installed && status.enabled)
        );
        let (view, window) = cx.add_window_view(|_, cx| {
            let mut view = DirectoryWindow::new(folder.clone(), services.clone(), cx);
            view.plugin_ui.statuses = statuses;
            view.browser.set_view_mode(ViewMode::Column);
            view.start_service_events(cx);
            view.start_listing(cx);
            view.start_watching(cx);
            view
        });
        let mut destinations = vec![folder.clone(), nested.clone()];
        if let Some(path) = std::env::var_os("EXPLORIE_PLUGIN_NAVIGATION_PATH") {
            let path = PathBuf::from(path);
            assert!(
                path.is_absolute() && path.is_dir(),
                "navigation path must be an existing absolute directory"
            );
            destinations.extend([path, folder.clone(), nested]);
        }
        for destination in destinations {
            view.update(window, |view, cx| view.navigate_to(destination.clone(), cx));
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                window.run_until_parked();
                window.executor().advance_clock(Duration::from_millis(10));
                let ready = view.update(window, |view, _| {
                    matches!(view.listing.state, ListingState::Ready)
                        && view
                            .column_view
                            .columns
                            .columns()
                            .iter()
                            .all(|column| !column.loading())
                        && !view.plugin_ui.scan_running
                        && view.plugin_ui.results.len() == 3
                });
                if ready || std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            view.update(window, |view, _| {
                assert!(
                    view.column_view.columns.columns().iter().all(|column| !column.loading()),
                    "columns remained loading at {} (column generation {}, plugin generation {}): {:?}",
                    destination.display(),
                    view.column_view.generation,
                    view.plugin_ui.generation,
                    view.column_view.columns
                );
                assert!(matches!(view.listing.state, ListingState::Ready));
                assert_eq!(
                    view.plugin_ui.results.len(),
                    3,
                    "all integrations should finish"
                );
                assert!(!view.plugin_ui.scan_running);
                if destination.starts_with(&folder) {
                    for result in view.plugin_ui.results.values() {
                        assert!(result.error.is_none(), "{} failed: {:?}", result.id, result.error);
                    }
                    let git = view.plugin_ui.results["git"].contribution.as_ref().unwrap();
                    assert_eq!(git.root.as_deref(), Some(folder.as_path()));
                    assert!(git.badge.is_some());
                }
            });
        }
        view.update(window, |view, _| {
            view.plugin_ui.statuses.clear();
            view.plugin_ui.scan = None;
            view.watcher.task = None;
        });
        services.plugins.shutdown().wait().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
