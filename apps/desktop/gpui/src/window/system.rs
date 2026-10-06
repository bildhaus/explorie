//! `DirectoryWindow` behavior for system.

use crate::*;

/// What the window knows about the host system: standard locations, shell
/// integration status, install-media cleanup offers and app updates, with the
/// jobs that fetch them.
pub(crate) struct SystemUi {
    pub(crate) locations: Option<SystemLocations>,
    pub(crate) locations_error: Option<String>,
    pub(crate) locations_task: Option<Task<()>>,
    pub(crate) integration_status: Option<SystemIntegrationStatus>,
    pub(crate) integration_error: Option<String>,
    pub(crate) integration_pending: bool,
    pub(crate) integration_task: Option<Task<()>>,
    pub(crate) integration_tasks: Vec<Task<()>>,
    pub(crate) integration_generation: u64,
    pub(crate) install_cleanup_task: Option<Task<()>>,
    pub(crate) install_cleanup_pending: bool,
    pub(crate) update_status: UpdateStatus,
    pub(crate) update_task: Option<Task<()>>,
}

impl Default for SystemUi {
    fn default() -> Self {
        Self {
            locations: None,
            locations_error: None,
            locations_task: None,
            integration_status: None,
            integration_error: None,
            integration_pending: false,
            integration_task: None,
            integration_tasks: Vec::new(),
            integration_generation: 0,
            install_cleanup_task: None,
            install_cleanup_pending: false,
            update_status: UpdateStatus::Idle,
            update_task: None,
        }
    }
}

impl DirectoryWindow {
    pub(crate) fn push_integration_task(&mut self, task: Task<()>) {
        self.system
            .integration_tasks
            .retain(|task| !task.is_ready());
        self.system.integration_tasks.push(task);
    }

    pub(crate) fn begin_integration_action(&mut self, status: String) -> u64 {
        self.system.integration_generation = self.system.integration_generation.wrapping_add(1);
        self.status_message = Some(status);
        self.system.integration_generation
    }

    pub fn start_preview_helpers(&mut self, cx: &mut Context<Self>) {
        if self.preview.helpers_loading {
            return;
        }
        self.preview.helpers_loading = true;
        let task = self.services.previews.helper_status();
        self.preview.helpers_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.preview.helpers_loading = false;
                match result {
                    Ok(statuses) => view.preview.helpers = statuses,
                    Err(error) => {
                        view.status_message =
                            Some(format!("Unable to inspect preview helpers: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn start_system_integration_status(&mut self, cx: &mut Context<Self>) {
        if self.system.integration_pending {
            return;
        }
        self.system.integration_pending = true;
        self.system.integration_error = None;
        let task = self.services.integration.status();
        self.system.integration_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.system.integration_pending = false;
                match result {
                    Ok(status) => view.system.integration_status = Some(status),
                    Err(error) => {
                        view.system.integration_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn start_install_cleanup_offer(&mut self, cx: &mut Context<Self>) {
        if self.system.install_cleanup_pending {
            return;
        }
        self.system.install_cleanup_pending = true;
        let task = self.services.integration.install_cleanup_offer();
        self.system.install_cleanup_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.system.install_cleanup_pending = false;
                view.system.install_cleanup_task = None;
                if let Ok(Some(offer)) = result
                    && !view.overlay_is_active()
                {
                    view.begin_overlay_focus();
                    view.request_settings_confirmation(
                        SettingsConfirmation::CleanupInstallMedia(offer),
                        cx,
                    );
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn start_install_media_cleanup(
        &mut self,
        offer: InstallCleanupOffer,
        cx: &mut Context<Self>,
    ) {
        if self.system.install_cleanup_pending {
            return;
        }
        self.system.install_cleanup_pending = true;
        let task = self.services.integration.cleanup_install_media(offer);
        self.system.install_cleanup_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.system.install_cleanup_pending = false;
                view.system.install_cleanup_task = None;
                match result {
                    Ok(()) => view.show_toast(
                        "Installer ejected and moved to Trash",
                        ToastKind::Success,
                        cx,
                    ),
                    Err(error) => {
                        view.record_error("Installer cleanup failed", error.to_string());
                        view.show_toast(
                            format!("Unable to clean up the installer: {error}"),
                            ToastKind::Warning,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn toggle_system_integration(&mut self, cx: &mut Context<Self>) {
        if self.system.integration_pending {
            return;
        }
        let Some(status) = self.system.integration_status.as_ref() else {
            self.start_system_integration_status(cx);
            return;
        };
        if !status.supported {
            let message = status.unavailable_reason.clone().unwrap_or_else(|| {
                "Automatic folder opening is available on Windows and macOS".to_string()
            });
            self.show_toast(message, ToastKind::Warning, cx);
            return;
        }

        let enabled = !status.enabled;
        self.system.integration_pending = true;
        self.system.integration_error = None;
        let task = self.services.integration.set_status(enabled);
        self.system.integration_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.system.integration_pending = false;
                match result {
                    Ok(status) => {
                        view.system.integration_status = Some(status);
                        view.show_toast(
                            if enabled {
                                "Folders now open in Explorie"
                            } else if cfg!(target_os = "macos") {
                                "Restored the previous macOS folder handler"
                            } else {
                                "Restored the previous Windows folder handler"
                            },
                            ToastKind::Success,
                            cx,
                        );
                    }
                    // macOS refused the change outright: show why, as a
                    // disabled setting rather than an error to retry.
                    Err(error) if error.code == ErrorCode::Unsupported => {
                        view.system.integration_status = Some(SystemIntegrationStatus {
                            supported: false,
                            enabled: false,
                            unavailable_reason: Some(error.message.clone()),
                        });
                        view.show_toast(error.message, ToastKind::Warning, cx);
                    }
                    Err(error) => {
                        view.system.integration_error = Some(error.to_string());
                        view.record_error("System integration failed", error.to_string());
                        view.show_toast(
                            format!("Unable to update folder handling: {error}"),
                            ToastKind::Warning,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn start_update_check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if matches!(
            self.system.update_status,
            UpdateStatus::Checking | UpdateStatus::Downloading(_) | UpdateStatus::Installing
        ) {
            return;
        }
        self.system.update_status = UpdateStatus::Checking;
        let task = self.services.updater.check();
        self.system.update_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.system.update_task = None;
                match result {
                    Ok(Some(update)) => {
                        view.system.update_status = UpdateStatus::Available(update.clone());
                        if !view.overlay_is_active() {
                            view.begin_overlay_focus();
                            view.request_settings_confirmation(
                                SettingsConfirmation::InstallUpdate(update),
                                cx,
                            );
                        } else {
                            view.show_toast(
                                "A new Explorie update is available",
                                ToastKind::Success,
                                cx,
                            );
                        }
                    }
                    Ok(None) => {
                        view.system.update_status = UpdateStatus::UpToDate;
                        if manual {
                            view.show_toast("Explorie is up to date", ToastKind::Success, cx);
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        view.system.update_status = UpdateStatus::Failed(message.clone());
                        view.record_error("Update check failed", &message);
                        if manual {
                            view.show_toast(
                                format!("Unable to check for updates: {message}"),
                                ToastKind::Warning,
                                cx,
                            );
                        }
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn start_update_download(&mut self, update: UpdateInfo, cx: &mut Context<Self>) {
        if matches!(
            self.system.update_status,
            UpdateStatus::Downloading(_) | UpdateStatus::Installing
        ) {
            return;
        }
        self.system.update_status = UpdateStatus::Downloading(update.clone());
        self.status_message = Some(format!("Downloading Explorie {}…", update.version));
        let task = self.services.updater.download(update.clone());
        self.system.update_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.system.update_task = None;
                match result {
                    Ok(downloaded) => view.start_prepared_update(downloaded, cx),
                    Err(error) => {
                        let message = error.to_string();
                        view.system.update_status = UpdateStatus::Failed(message.clone());
                        view.record_error("Update download failed", &message);
                        view.show_toast(
                            format!("Unable to prepare the update: {message}"),
                            ToastKind::Warning,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn start_prepared_update(
        &mut self,
        update: DownloadedUpdate,
        cx: &mut Context<Self>,
    ) {
        if self.services.mutations.active_count() > 0
            || self.operations.active_count() > 0
            || self.mutation.in_progress
            || self.operation_ui.undo_progress.is_some()
        {
            self.system.update_status = UpdateStatus::Ready(update);
            self.show_toast(
                "Update downloaded; finish filesystem operations before installing",
                ToastKind::Warning,
                cx,
            );
            return;
        }

        self.system.update_status = UpdateStatus::Installing;
        self.status_message = Some("Preparing to restart into the Explorie update…".to_string());
        let disconnect = self.services.remotes.disconnect_all_if_clean_task();
        let updater = self.services.updater.clone();
        self.system.update_task = Some(cx.spawn(async move |this, cx| {
            let result = match disconnect.await {
                Ok(true) => updater.launch(update.clone()).await,
                Ok(false) => Err(ServiceError::new(
                    ErrorCode::Busy,
                    "Remote drive activity must finish before installing the update",
                )
                .retryable(true)),
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |view, cx| {
                view.system.update_task = None;
                match result {
                    Ok(()) => {
                        view.persist_window_placement();
                        cx.quit();
                    }
                    Err(error) => {
                        let message = error.to_string();
                        view.system.update_status = UpdateStatus::Ready(update);
                        view.record_error("Update launch failed", &message);
                        view.show_toast(
                            format!("Unable to install the update: {message}"),
                            ToastKind::Warning,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}
