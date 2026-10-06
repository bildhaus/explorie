//! `DirectoryWindow` behavior for remote drives.

use crate::*;

/// Remote drive state for one window: what the helper reports, the manager's
/// editors and confirmations, and the polling, retry and auto-connect work.
#[derive(Default)]
pub(crate) struct RemoteDrivesUi {
    pub(crate) environment: Option<RemoteDriveEnvironment>,
    pub(crate) available: Vec<String>,
    pub(crate) statuses: BTreeMap<String, RemoteDriveStatus>,
    pub(crate) setup_error: Option<String>,
    pub(crate) editor: Option<RemoteProfileEditor>,
    pub(crate) delete_confirmation: Option<String>,
    pub(crate) blocked_disconnect: Option<BlockedRemoteDisconnect>,
    pub(crate) exit_blocker: Option<RemoteDriveExitBlocker>,
    pub(crate) tasks: Vec<Task<()>>,
    pub(crate) poll_task: Option<Task<()>>,
    pub(crate) close_task: Option<Task<()>>,
    pub(crate) close_pending: bool,
    pub(crate) disable_pending: bool,
    pub(crate) environment_generation: u64,
    pub(crate) poll_generation: u64,
    pub(crate) auto_connect_started: bool,
    pub(crate) auto_connect_active: Option<String>,
    pub(crate) auto_connect_queue: VecDeque<RemoteDriveProfile>,
    pub(crate) connect_generations: BTreeMap<String, u64>,
    pub(crate) retries: BTreeMap<String, RemoteRetryState>,
}

impl DirectoryWindow {
    pub(crate) fn open_remote_drive_manager(&mut self, cx: &mut Context<Self>) {
        self.remote.editor = None;
        self.remote.delete_confirmation = None;
        self.remote.blocked_disconnect = None;
        self.open_control_surface(ControlSurface::RemoteDrives, cx);
        self.refresh_remote_environment(cx);
    }

    pub fn start_remote_drives(&mut self, cx: &mut Context<Self>) {
        if self.settings.behavior.remote_drives_enabled {
            self.refresh_remote_environment(cx);
            self.start_remote_status_polling(cx);
        }
    }

    pub(crate) fn start_remote_status_polling(&mut self, cx: &mut Context<Self>) {
        self.remote.poll_generation = self.remote.poll_generation.wrapping_add(1);
        let generation = self.remote.poll_generation;
        self.remote.poll_task = None;
        if !self.settings.behavior.remote_drives_enabled {
            return;
        }
        let remotes = self.services.remotes.clone();
        let executor = cx.background_executor().clone();
        self.remote.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(REMOTE_STATUS_POLL_INTERVAL).await;
                let current = this
                    .update(cx, |view, _| {
                        view.settings.behavior.remote_drives_enabled
                            && view.remote.poll_generation == generation
                    })
                    .unwrap_or(false);
                if !current {
                    return;
                }
                let statuses = remotes.statuses_task().await;
                let current = this
                    .update(cx, |view, cx| {
                        if !view.settings.behavior.remote_drives_enabled
                            || view.remote.poll_generation != generation
                        {
                            return false;
                        }
                        if let Ok(statuses) = statuses {
                            view.apply_polled_remote_statuses(statuses, cx);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    return;
                }
            }
        }));
    }

    pub(crate) fn stop_remote_status_polling(&mut self) {
        self.remote.poll_generation = self.remote.poll_generation.wrapping_add(1);
        self.remote.poll_task = None;
    }

    pub(crate) fn push_remote_task(&mut self, task: Task<()>) {
        self.remote.tasks.retain(|task| !task.is_ready());
        self.remote.tasks.push(task);
    }

    pub(crate) fn apply_polled_remote_statuses(
        &mut self,
        statuses: Vec<RemoteDriveStatus>,
        cx: &mut Context<Self>,
    ) {
        let mut reconnect = Vec::new();
        for status in statuses {
            let id = status.id.clone();
            let was_connected = self
                .remote
                .statuses
                .get(&id)
                .is_some_and(|previous| previous.state == RemoteDriveState::Connected);
            let retry_active = self.remote.retries.get(&id).is_some_and(|retry| {
                matches!(
                    retry.phase,
                    RemoteRetryPhase::Connecting | RemoteRetryPhase::Waiting
                )
            });
            if retry_active {
                continue;
            }
            let degraded = was_connected
                && status.state == RemoteDriveState::Error
                && status.error.as_ref().is_some_and(|error| error.retryable)
                && !self.remote.retries.contains_key(&id);
            self.remote.statuses.insert(id.clone(), status);
            if degraded
                && let Some(profile) = self
                    .settings
                    .remote_profiles
                    .iter()
                    .find(|profile| profile.id == id)
                    .cloned()
            {
                reconnect.push(profile);
            }
        }
        if !reconnect.is_empty() {
            self.show_toast(
                if reconnect.len() == 1 {
                    "Remote drive disconnected; reconnecting with bounded retries".to_string()
                } else {
                    format!(
                        "{} remote drives disconnected; reconnecting with bounded retries",
                        reconnect.len()
                    )
                },
                ToastKind::Warning,
                cx,
            );
            for profile in reconnect {
                self.start_remote_connect_sequence(profile, false, cx);
            }
        } else {
            cx.notify();
        }
    }

    pub(crate) fn refresh_remote_environment(&mut self, cx: &mut Context<Self>) {
        self.remote.environment_generation = self.remote.environment_generation.wrapping_add(1);
        let generation = self.remote.environment_generation;
        let remotes = self.services.remotes.clone();
        self.remote.setup_error = None;
        self.push_remote_task(cx.spawn(async move |this, cx| {
            let environment = remotes.environment().await;
            let (available, statuses) = match &environment {
                Ok(environment) if environment.rclone_available => {
                    let available = remotes.list_remotes().await;
                    let statuses = remotes.statuses_task().await;
                    (available, statuses)
                }
                _ => (Ok(Vec::new()), remotes.statuses_task().await),
            };
            let _ = this.update(cx, |view, cx| {
                if view.remote.environment_generation != generation {
                    return;
                }
                match environment {
                    Ok(environment) => {
                        view.remote.setup_error = environment
                            .error
                            .as_ref()
                            .map(ToString::to_string)
                            .or_else(|| available.as_ref().err().map(ToString::to_string));
                        view.remote.environment = Some(environment);
                    }
                    Err(error) => {
                        view.record_error("Remote environment failed", error.to_string());
                        view.remote.setup_error = Some(error.to_string());
                    }
                }
                if let Ok(available) = available {
                    view.remote.available = available;
                }
                if let Ok(statuses) = statuses {
                    for status in statuses {
                        view.remote.statuses.insert(status.id.clone(), status);
                    }
                }
                view.maybe_auto_connect_remotes(cx);
                cx.notify();
            });
        }));
    }

    pub(crate) fn maybe_auto_connect_remotes(&mut self, cx: &mut Context<Self>) {
        let Some(environment) = &self.remote.environment else {
            return;
        };
        let ready = environment.rclone_available
            && (environment.platform != "windows" || environment.winfsp_available == Some(true))
            && (environment.platform != "macos"
                || environment.helper_status.as_deref() == Some("enabled"));
        if !self.settings.behavior.remote_drives_enabled
            || !ready
            || self.remote.auto_connect_started
            || self.settings.remote_profiles.is_empty()
        {
            return;
        }
        self.remote.auto_connect_started = true;
        self.remote.auto_connect_queue = self.settings.remote_profiles.clone().into();
        self.start_next_auto_connect(cx);
    }

    pub(crate) fn start_next_auto_connect(&mut self, cx: &mut Context<Self>) {
        if !self.settings.behavior.remote_drives_enabled
            || self.remote.auto_connect_active.is_some()
        {
            return;
        }
        let Some(profile) = self.remote.auto_connect_queue.pop_front() else {
            return;
        };
        self.remote.auto_connect_active = Some(profile.id.clone());
        self.start_remote_connect_sequence(profile, true, cx);
    }

    pub(crate) fn finish_remote_connect_sequence(
        &mut self,
        id: &str,
        auto_connect: bool,
        cx: &mut Context<Self>,
    ) {
        if auto_connect && self.remote.auto_connect_active.as_deref() == Some(id) {
            self.remote.auto_connect_active = None;
            self.start_next_auto_connect(cx);
        }
    }

    pub(crate) fn connect_remote_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(profile) = self
            .settings
            .remote_profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
        else {
            return;
        };
        let auto_connect = self.remote.auto_connect_active.as_deref() == Some(id.as_str());
        self.start_remote_connect_sequence(profile, auto_connect, cx);
    }

    pub(crate) fn next_remote_connect_generation(&mut self, id: &str) -> u64 {
        let generation = self
            .remote
            .connect_generations
            .entry(id.to_string())
            .or_default();
        *generation = generation.wrapping_add(1);
        *generation
    }

    pub(crate) fn remote_connect_is_current(&self, id: &str, generation: u64) -> bool {
        self.remote.connect_generations.get(id).copied() == Some(generation)
    }

    pub(crate) fn cancel_remote_connect_attempt(&mut self, id: &str) {
        self.next_remote_connect_generation(id);
        let was_retrying = self.remote.retries.remove(id).is_some();
        if was_retrying
            || self
                .remote
                .statuses
                .get(id)
                .is_some_and(|status| status.state == RemoteDriveState::Connecting)
        {
            self.remote.statuses.insert(
                id.to_string(),
                remote_status(id, RemoteDriveState::Disconnected, None, None),
            );
        }
        if self.remote.auto_connect_active.as_deref() == Some(id) {
            self.remote.auto_connect_active = None;
            self.remote.auto_connect_queue.clear();
        }
    }

    pub(crate) fn cancel_all_remote_connect_attempts(&mut self) {
        let ids: BTreeSet<String> = self
            .remote
            .retries
            .keys()
            .cloned()
            .chain(
                self.remote
                    .statuses
                    .iter()
                    .filter(|(_, status)| status.state == RemoteDriveState::Connecting)
                    .map(|(id, _)| id.clone()),
            )
            .collect();
        for id in ids {
            self.cancel_remote_connect_attempt(&id);
        }
    }

    pub(crate) fn start_remote_connect_sequence(
        &mut self,
        profile: RemoteDriveProfile,
        auto_connect: bool,
        cx: &mut Context<Self>,
    ) {
        let id = profile.id.clone();
        let generation = self.next_remote_connect_generation(&id);
        self.remote.statuses.insert(
            id.clone(),
            remote_status(&id, RemoteDriveState::Connecting, None, None),
        );
        self.remote
            .retries
            .insert(id.clone(), RemoteRetryState::connecting(1));
        let remotes = self.services.remotes.clone();
        let executor = cx.background_executor().clone();
        self.push_remote_task(cx.spawn(async move |this, cx| {
            for attempt in 1..=REMOTE_CONNECT_MAX_ATTEMPTS {
                match remotes.connect(profile.clone()).await {
                    Ok(status) => {
                        let connected = status.state == RemoteDriveState::Connected;
                        let helper_not_ready = matches!(
                            status.state,
                            RemoteDriveState::HelperMissing | RemoteDriveState::ApprovalRequired
                        );
                        let applied = this
                            .update(cx, |view, cx| {
                                if !view.remote_connect_is_current(&id, generation) {
                                    return false;
                                }
                                if helper_not_ready {
                                    // Connecting may have just installed the
                                    // helper; show its new state.
                                    view.refresh_remote_environment(cx);
                                }
                                view.remote.statuses.insert(id.clone(), status);
                                view.remote.retries.remove(&id);
                                view.finish_remote_connect_sequence(&id, auto_connect, cx);
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !applied && connected {
                            let _ = remotes.disconnect(id.clone(), false).await;
                        }
                        return;
                    }
                    Err(error) => {
                        let should_retry = error.retryable && attempt < REMOTE_CONNECT_MAX_ATTEMPTS;
                        if !should_retry {
                            let _ = this.update(cx, |view, cx| {
                                if !view.remote_connect_is_current(&id, generation) {
                                    return;
                                }
                                view.remote.statuses.insert(
                                    id.clone(),
                                    remote_status(
                                        &id,
                                        RemoteDriveState::Error,
                                        None,
                                        Some(error.clone()),
                                    ),
                                );
                                if error.retryable {
                                    view.remote.retries.insert(
                                        id.clone(),
                                        RemoteRetryState {
                                            attempt,
                                            max_attempts: REMOTE_CONNECT_MAX_ATTEMPTS,
                                            phase: RemoteRetryPhase::Exhausted,
                                            delay: None,
                                        },
                                    );
                                } else {
                                    view.remote.retries.remove(&id);
                                }
                                view.record_error("Remote connection failed", error.to_string());
                                view.finish_remote_connect_sequence(&id, auto_connect, cx);
                                cx.notify();
                            });
                            return;
                        }

                        let delay = remote_retry_delay(attempt);
                        let current = this
                            .update(cx, |view, cx| {
                                if !view.remote_connect_is_current(&id, generation) {
                                    return false;
                                }
                                view.remote.statuses.insert(
                                    id.clone(),
                                    remote_status(
                                        &id,
                                        RemoteDriveState::Error,
                                        None,
                                        Some(error.clone()),
                                    ),
                                );
                                view.remote.retries.insert(
                                    id.clone(),
                                    RemoteRetryState {
                                        attempt,
                                        max_attempts: REMOTE_CONNECT_MAX_ATTEMPTS,
                                        phase: RemoteRetryPhase::Waiting,
                                        delay: Some(delay),
                                    },
                                );
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !current {
                            return;
                        }
                        executor.timer(delay).await;
                        let current = this
                            .update(cx, |view, cx| {
                                if !view.remote_connect_is_current(&id, generation) {
                                    return false;
                                }
                                view.remote.statuses.insert(
                                    id.clone(),
                                    remote_status(&id, RemoteDriveState::Connecting, None, None),
                                );
                                view.remote
                                    .retries
                                    .insert(id.clone(), RemoteRetryState::connecting(attempt + 1));
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !current {
                            return;
                        }
                    }
                }
            }
        }));
        cx.notify();
    }

    pub(crate) fn disconnect_remote_profile(
        &mut self,
        id: String,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        self.cancel_remote_connect_attempt(&id);
        let remotes = self.services.remotes.clone();
        self.remote.blocked_disconnect = None;
        self.push_remote_task(cx.spawn(async move |this, cx| {
            let result = remotes.disconnect(id.clone(), force).await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(result) => {
                        view.remote
                            .statuses
                            .insert(result.status.id.clone(), result.status);
                        if result.blocked {
                            view.remote.blocked_disconnect = Some(BlockedRemoteDisconnect {
                                id,
                                pending_uploads: result.pending_uploads,
                                errored_files: result.errored_files,
                            });
                        }
                    }
                    Err(error) => {
                        view.record_error("Remote disconnect failed", error.to_string());
                        view.remote.statuses.insert(
                            id.clone(),
                            remote_status(&id, RemoteDriveState::Error, None, Some(error)),
                        );
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn request_remote_profile_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.cancel_remote_connect_attempt(&id);
        self.remote.delete_confirmation = Some(id);
        self.remote.blocked_disconnect = None;
        cx.notify();
    }

    pub(crate) fn confirm_remote_profile_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.cancel_remote_connect_attempt(&id);
        let remotes = self.services.remotes.clone();
        self.push_remote_task(cx.spawn(async move |this, cx| {
            let result = remotes.disconnect(id.clone(), false).await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(result) if result.blocked => {
                        view.remote
                            .statuses
                            .insert(result.status.id.clone(), result.status);
                        view.remote.blocked_disconnect = Some(BlockedRemoteDisconnect {
                            id,
                            pending_uploads: result.pending_uploads,
                            errored_files: result.errored_files,
                        });
                        view.show_toast(
                            "Remote profile retained while writes or errors remain",
                            ToastKind::Warning,
                            cx,
                        );
                    }
                    Ok(_) => {
                        view.remote.statuses.remove(&id);
                        view.remote.retries.remove(&id);
                        view.remote.connect_generations.remove(&id);
                        view.settings
                            .remote_profiles
                            .retain(|profile| profile.id != id);
                        view.remote.delete_confirmation = None;
                        view.persist_settings();
                        view.show_toast("Remote profile removed", ToastKind::Success, cx);
                    }
                    Err(error) => {
                        view.record_error("Remote profile removal failed", error.to_string());
                        view.show_toast(error.to_string(), ToastKind::Warning, cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn open_remote_profile_editor(
        &mut self,
        profile: Option<RemoteDriveProfile>,
        cx: &mut Context<Self>,
    ) {
        let draft = profile.unwrap_or_else(|| RemoteDriveProfile {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            remote: self.remote.available.first().cloned().unwrap_or_default(),
            remote_path: String::new(),
            mount_target: self.default_remote_mount_target(),
        });
        self.overlay.query.clone_from(&draft.name);
        self.remote.editor = Some(RemoteProfileEditor {
            draft,
            field: RemoteEditorField::Name,
        });
        self.remote.delete_confirmation = None;
        self.sync_native_text_input(self.overlay.query.clone(), cx);
        cx.notify();
    }

    pub(crate) fn default_remote_mount_target(&self) -> String {
        let platform = self
            .remote
            .environment
            .as_ref()
            .map(|environment| environment.platform.as_str());
        if platform != Some("windows") {
            return String::new();
        }
        let mut occupied: Vec<String> = self
            .settings
            .remote_profiles
            .iter()
            .map(|profile| profile.mount_target.to_ascii_lowercase())
            .collect();
        if let Some(environment) = &self.remote.environment {
            occupied.extend(
                environment
                    .occupied_mount_targets
                    .iter()
                    .map(|target| target.to_ascii_lowercase()),
            );
        }
        (b'D'..=b'Z')
            .map(|letter| format!("{}:", char::from(letter)))
            .find(|target| !occupied.contains(&target.to_ascii_lowercase()))
            .unwrap_or_default()
    }

    pub(crate) fn remote_editor_value(editor: &RemoteProfileEditor) -> &str {
        match editor.field {
            RemoteEditorField::Name => &editor.draft.name,
            RemoteEditorField::Remote => &editor.draft.remote,
            RemoteEditorField::RemotePath => &editor.draft.remote_path,
            RemoteEditorField::MountTarget => &editor.draft.mount_target,
        }
    }

    pub(crate) fn commit_remote_editor_field(&mut self, cx: &mut Context<Self>) {
        let Some(mut editor) = self.remote.editor.take() else {
            return;
        };
        let value = self.overlay.query.trim().to_string();
        match editor.field {
            RemoteEditorField::Name if value.is_empty() || value.chars().count() > 64 => {
                self.remote.editor = Some(editor);
                self.show_toast(
                    "Remote drive name must be 1–64 characters",
                    ToastKind::Warning,
                    cx,
                );
                return;
            }
            RemoteEditorField::Name => {
                editor.draft.name = value;
                if self
                    .remote
                    .environment
                    .as_ref()
                    .is_some_and(|environment| environment.platform == "macos")
                    && editor.draft.mount_target.is_empty()
                {
                    editor.draft.mount_target.clone_from(&editor.draft.name);
                }
            }
            RemoteEditorField::Remote
                if value.is_empty() || !self.remote.available.contains(&value) =>
            {
                self.remote.editor = Some(editor);
                self.show_toast(
                    "Choose one of the configured rclone remotes",
                    ToastKind::Warning,
                    cx,
                );
                return;
            }
            RemoteEditorField::Remote => editor.draft.remote = value,
            RemoteEditorField::RemotePath => editor.draft.remote_path = value,
            RemoteEditorField::MountTarget => editor.draft.mount_target = value,
        }
        if let Some(next) = editor.field.next() {
            editor.field = next;
            self.overlay.query = Self::remote_editor_value(&editor).to_string();
            self.remote.editor = Some(editor);
            self.sync_native_text_input(self.overlay.query.clone(), cx);
            cx.notify();
            return;
        }
        if let Err(error) = validate_remote_drive_profile(&editor.draft) {
            self.remote.editor = Some(editor);
            self.show_toast(error.to_string(), ToastKind::Warning, cx);
            return;
        }
        if self.settings.remote_profiles.iter().any(|profile| {
            profile.id == editor.draft.id
                && self
                    .remote
                    .statuses
                    .get(&profile.id)
                    .is_some_and(|status| status.state == RemoteDriveState::Connected)
        }) {
            self.remote.editor = Some(editor);
            self.show_toast(
                "Disconnect this remote drive before editing its profile",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        if self.settings.remote_profiles.iter().any(|profile| {
            profile.id != editor.draft.id
                && profile
                    .mount_target
                    .eq_ignore_ascii_case(&editor.draft.mount_target)
        }) {
            self.remote.editor = Some(editor);
            self.show_toast(
                "Another profile already uses that mount target",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        let id = editor.draft.id.clone();
        self.settings
            .remote_profiles
            .retain(|profile| profile.id != id);
        self.settings.remote_profiles.push(editor.draft);
        self.settings
            .remote_profiles
            .sort_by(|left, right| left.name.cmp(&right.name));
        self.overlay.query.clear();
        self.persist_settings();
        self.connect_remote_profile(id, cx);
    }

    pub(crate) fn configure_remote_backend(&mut self, cx: &mut Context<Self>) {
        let remotes = self.services.remotes.clone();
        self.push_remote_task(cx.spawn(async move |this, cx| {
            let result = remotes.configure().await;
            let available = if result.is_ok() {
                remotes.list_remotes().await
            } else {
                Ok(Vec::new())
            };
            let _ = this.update(cx, |view, cx| {
                match (result, available) {
                    (Ok(()), Ok(available)) if !available.is_empty() => {
                        view.remote.available = available;
                        view.open_remote_profile_editor(None, cx);
                    }
                    (Ok(()), Ok(_)) => {
                        view.remote.setup_error =
                            Some("rclone finished without creating a remote".to_string());
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        view.record_error("rclone configuration failed", error.to_string());
                        view.remote.setup_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn install_remote_helper(&mut self, cx: &mut Context<Self>) {
        let remotes = self.services.remotes.clone();
        let platform = self
            .remote
            .environment
            .as_ref()
            .map(|environment| environment.platform.clone());
        self.push_remote_task(cx.spawn(async move |this, cx| {
            let result = if platform.as_deref() == Some("windows") {
                remotes.install_winfsp().await.map(|_| None)
            } else {
                remotes.register_helper().await.map(Some)
            };
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(Some(status)) if status == "approval-required" => {
                        // The helper row then shows what to allow.
                        let remotes = view.services.remotes.clone();
                        view.push_remote_task(cx.spawn(async move |_, _| {
                            let _ = remotes.open_helper_settings().await;
                        }));
                        view.refresh_remote_environment(cx);
                    }
                    Ok(_) => view.refresh_remote_environment(cx),
                    Err(error) => {
                        view.record_error("Remote helper setup failed", error.to_string());
                        view.remote.setup_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn force_disconnect_all_remotes(&mut self, cx: &mut Context<Self>) {
        self.cancel_all_remote_connect_attempts();
        let remotes = self.services.remotes.clone();
        self.push_remote_task(cx.spawn(async move |this, cx| {
            let result = remotes.disconnect_all().await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(()) => {
                        view.remote.statuses.clear();
                        view.remote.blocked_disconnect = None;
                        view.remote.exit_blocker = None;
                        view.show_toast(
                            "Remote drives disconnected; cache retained for recovery",
                            ToastKind::Success,
                            cx,
                        );
                    }
                    Err(error) => {
                        view.record_error("Remote shutdown failed", error.to_string());
                        view.show_toast(error.to_string(), ToastKind::Warning, cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn toggle_remote_drives(&mut self, cx: &mut Context<Self>) {
        if self.remote.disable_pending {
            return;
        }
        if !self.settings.behavior.remote_drives_enabled {
            self.settings.behavior.remote_drives_enabled = true;
            self.finish_settings_change("Remote drives: On", cx);
            self.remote.auto_connect_started = false;
            self.start_remote_drives(cx);
            return;
        }

        self.cancel_all_remote_connect_attempts();
        self.stop_remote_status_polling();
        self.remote.disable_pending = true;
        self.status_message = Some("Disconnecting remote drives safely…".to_string());
        let task = self.services.remotes.disconnect_all_if_clean_task();
        self.push_remote_task(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.remote.disable_pending = false;
                match result {
                    Ok(true) => {
                        view.settings.behavior.remote_drives_enabled = false;
                        view.remote.statuses.clear();
                        view.remote.retries.clear();
                        view.finish_settings_change("Remote drives: Off", cx);
                    }
                    Ok(false) => {
                        view.start_remote_status_polling(cx);
                        view.status_message = Some(
                            "Remote drives remain enabled until pending uploads or errors are resolved"
                                .to_string(),
                        );
                        cx.notify();
                    }
                    Err(error) => {
                        view.start_remote_status_polling(cx);
                        view.record_error("Remote disconnect failed", error.to_string());
                        view.status_message = Some(format!(
                            "Remote drives remain enabled: {error}"
                        ));
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }

    pub(crate) fn start_remote_close_check(&mut self, cx: &mut Context<Self>) {
        if self.remote.close_pending {
            return;
        }
        self.remote.close_pending = true;
        self.status_message = Some("Checking remote drives before closing…".to_string());
        let task = self.services.remotes.disconnect_all_if_clean_task();
        self.remote.close_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.remote.close_pending = false;
                view.remote.close_task = None;
                match result {
                    Ok(true) => {
                        view.mutation.exit_waiting = false;
                        cx.quit();
                    }
                    Ok(false) => {
                        view.status_message = Some(
                            "Remote drive activity must be resolved before closing".to_string(),
                        );
                        cx.notify();
                    }
                    Err(error) => {
                        view.record_error("Remote shutdown check failed", error.to_string());
                        view.status_message = Some(format!(
                            "Unable to check remote drives before closing: {error}"
                        ));
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }

    pub(crate) fn render_remote_drive_manager(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let platform = self
            .remote
            .environment
            .as_ref()
            .map(|environment| environment.platform.clone());
        let environment_label = self.remote.environment.as_ref().map_or_else(
            || "Checking native remote-drive environment…".to_string(),
            |environment| {
                format!(
                    "{} • rclone {}{}{}",
                    environment.platform,
                    environment.rclone_version.as_deref().unwrap_or(
                        if environment.rclone_available {
                            "available"
                        } else {
                            "missing"
                        }
                    ),
                    environment
                        .winfsp_available
                        .map_or_else(String::new, |available| {
                            format!(" • WinFsp {}", if available { "ready" } else { "missing" })
                        }),
                    environment
                        .helper_status
                        .as_deref()
                        .map_or_else(String::new, |status| format!(" • helper {status}"))
                )
            },
        );
        let deleting = self.remote.delete_confirmation.clone();
        let profiles = self.settings.remote_profiles.clone();
        let rows: Vec<AnyElement> = profiles
            .into_iter()
            .enumerate()
            .map(|(index, profile)| {
                let status = self
                    .remote
                    .statuses
                    .get(&profile.id)
                    .cloned()
                    .unwrap_or_else(|| {
                        remote_status(&profile.id, RemoteDriveState::Disconnected, None, None)
                    });
                let state = status.state;
                let retry = self.remote.retries.get(&profile.id).cloned();
                let retry_active = retry.as_ref().is_some_and(|retry| {
                    matches!(
                        retry.phase,
                        RemoteRetryPhase::Connecting | RemoteRetryPhase::Waiting
                    )
                });
                let is_deleting = deleting.as_deref() == Some(profile.id.as_str());
                let open_path = status.mount_path.clone();
                let connect_id = profile.id.clone();
                let cancel_connect_id = profile.id.clone();
                let disconnect_id = profile.id.clone();
                let edit_profile = profile.clone();
                let delete_id = profile.id.clone();
                let confirm_id = profile.id.clone();
                let detail = remote_profile_detail(&profile, &status, retry.as_ref());
                let connect_label = if retry
                    .as_ref()
                    .is_some_and(|retry| retry.phase == RemoteRetryPhase::Waiting)
                {
                    "Retry now"
                } else if state == RemoteDriveState::Error {
                    "Retry"
                } else {
                    "Connect"
                };
                div()
                    .id(("remote-profile-row", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.surface)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_sm().child(profile.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if state == RemoteDriveState::Error {
                                        rgb(0xff8f8f)
                                    } else {
                                        self.palette.muted
                                    })
                                    .child(detail),
                            ),
                    )
                    .when(!is_deleting && open_path.is_some(), |row| {
                        let path = open_path.expect("checked above");
                        row.child(
                            toolbar_button(
                                ("open-remote-profile", index),
                                "Open",
                                self.palette.control,
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.navigate_to(path.clone(), cx)
                                }),
                            ),
                        )
                    })
                    .when(
                        !is_deleting && state == RemoteDriveState::Connected,
                        |row| {
                            row.child(
                                toolbar_button(
                                    ("disconnect-remote-profile", index),
                                    "Disconnect",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.disconnect_remote_profile(
                                            disconnect_id.clone(),
                                            false,
                                            cx,
                                        )
                                    },
                                )),
                            )
                        },
                    )
                    .when(
                        !is_deleting
                            && !matches!(
                                state,
                                RemoteDriveState::Connected
                                    | RemoteDriveState::Connecting
                                    | RemoteDriveState::Disconnecting
                            ),
                        |row| {
                            row.child(
                                toolbar_button(
                                    ("connect-remote-profile", index),
                                    connect_label,
                                    self.palette.control,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.connect_remote_profile(connect_id.clone(), cx)
                                    },
                                )),
                            )
                        },
                    )
                    .when(!is_deleting && retry_active, |row| {
                        row.child(
                            toolbar_button(
                                ("cancel-connect-remote-profile", index),
                                "Cancel",
                                self.palette.control,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.cancel_remote_connect_attempt(&cancel_connect_id);
                                    this.show_toast(
                                        "Remote connection retry cancelled",
                                        ToastKind::Success,
                                        cx,
                                    );
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .when(!is_deleting, |row| {
                        row.child(
                            toolbar_button(
                                ("edit-remote-profile", index),
                                "Edit",
                                self.palette.control,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.open_remote_profile_editor(Some(edit_profile.clone()), cx)
                                },
                            )),
                        )
                        .child(
                            toolbar_button(
                                ("delete-remote-profile", index),
                                "Remove",
                                self.palette.control,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.request_remote_profile_delete(delete_id.clone(), cx)
                                },
                            )),
                        )
                    })
                    .when(is_deleting, |row| {
                        row.child(div().text_xs().child("Disconnect and remove profile?"))
                            .child(
                                toolbar_button(
                                    ("confirm-delete-remote-profile", index),
                                    "Remove",
                                    rgb(0x8b3340),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.confirm_remote_profile_delete(confirm_id.clone(), cx)
                                    },
                                )),
                            )
                            .child(
                                toolbar_button(
                                    ("cancel-delete-remote-profile", index),
                                    "Cancel",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.remote.delete_confirmation = None;
                                        cx.notify();
                                    },
                                )),
                            )
                    })
                    .into_any_element()
            })
            .collect();
        let control_input = self.native_text_input_element(TextInputTarget::ControlQuery);
        let editor = self.remote.editor.clone().map(|editor| {
            let label = editor.field.label(platform.as_deref());
            let action = if editor.field.next().is_some() {
                "Next"
            } else {
                "Save & connect"
            };
            div()
                .id("remote-profile-editor")
                .flex()
                .flex_col()
                .gap_2()
                .px_4()
                .py_3()
                .border_b_1()
                .border_color(self.palette.accent)
                .bg(self.palette.selected)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().w(px(140.0)).text_sm().child(label))
                        .child(
                            div()
                                .id("remote-profile-editor-field")
                                .debug_selector(|| "remote-profile-editor-field".to_string())
                                .flex_1()
                                .children(control_input),
                        )
                        .child(
                            toolbar_button(
                                "commit-remote-profile-field",
                                action,
                                self.palette.control,
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.commit_remote_editor_field(cx)),
                            ),
                        )
                        .child(
                            toolbar_button(
                                "cancel-remote-profile-editor",
                                "Cancel",
                                self.palette.control,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.remote.editor = None;
                                this.overlay.query.clear();
                                cx.notify();
                            })),
                        ),
                )
                .when(editor.field == RemoteEditorField::Remote, |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format!(
                                "Configured: {}",
                                if self.remote.available.is_empty() {
                                    "none".to_string()
                                } else {
                                    self.remote.available.join(", ")
                                }
                            )),
                    )
                })
                .into_any_element()
        });
        let blocked = self.remote.blocked_disconnect.clone().map(|blocked| {
            let id = blocked.id.clone();
            div()
                .id("remote-disconnect-blocker")
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(rgb(0xe08a3e))
                .child(div().flex_1().text_sm().child(format!(
                    "{} pending upload(s), {} errored file(s). Force disconnect keeps the VFS cache for recovery.",
                    blocked.pending_uploads, blocked.errored_files
                )))
                .child(
                    toolbar_button("force-disconnect-remote", "Force disconnect", rgb(0x8b5a24))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.disconnect_remote_profile(id.clone(), true, cx)
                        })),
                )
                .child(
                    toolbar_button("cancel-force-disconnect-remote", "Wait", self.palette.control)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.remote.blocked_disconnect = None;
                            cx.notify();
                        })),
                )
                .into_any_element()
        });
        let exit_blocker = self.remote.exit_blocker.as_ref().map(|blocker| {
            div()
                .id("remote-exit-blocker")
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(rgb(0xe08a3e))
                .child(div().flex_1().text_sm().child(format!(
                    "Close paused: {} pending upload(s), {} errored file(s).",
                    blocker.pending_uploads, blocker.errored_files
                )))
                .child(
                    toolbar_button(
                        "force-disconnect-all-remotes",
                        "Force disconnect all",
                        rgb(0x8b5a24),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.force_disconnect_all_remotes(cx))),
                )
                .into_any_element()
        });
        let setup = self.remote.setup_error.clone().map(|error| {
            div()
                .id("remote-setup-error")
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(rgb(0xe08a3e))
                .child(div().flex_1().text_sm().child(error))
                .child(
                    toolbar_button("retry-remote-setup", "Retry", self.palette.control).on_click(
                        cx.listener(|this, _, _, cx| this.refresh_remote_environment(cx)),
                    ),
                )
                .when(
                    self.remote.environment.as_ref().is_some_and(|environment| {
                        environment.platform == "windows"
                            && environment.winfsp_available == Some(false)
                    }),
                    |row| {
                        row.child(
                            toolbar_button(
                                "install-remote-helper",
                                "Install / enable",
                                self.palette.control,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.install_remote_helper(cx))),
                        )
                    },
                )
                .into_any_element()
        });
        // On macOS remote drives mount through a privileged helper the user
        // has to allow; say so whenever it is not ready, not only after an
        // error, and offer the step that gets it ready.
        let helper_setup = self
            .remote
            .environment
            .as_ref()
            .filter(|environment| {
                environment.platform == "macos"
                    && environment.helper_status.as_deref() != Some("enabled")
            })
            .map(|environment| {
                let waiting = environment.helper_status.as_deref() == Some("approval-required");
                let (message, action) = if waiting {
                    (HELPER_APPROVAL_MESSAGE, "Open Login Items")
                } else {
                    (
                        "Remote drives need Explorie's mount helper. Install it, then allow it when macOS asks.",
                        "Install helper",
                    )
                };
                div()
                    .id("remote-helper-setup")
                    .debug_selector(|| "remote-helper-setup".to_string())
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(0xe08a3e))
                    .child(div().flex_1().text_sm().child(message))
                    .child(
                        toolbar_button("install-remote-helper", action, self.palette.control)
                            .debug_selector(|| "install-remote-helper".to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.install_remote_helper(cx))),
                    )
                    .child(
                        toolbar_button("recheck-remote-helper", "Check again", self.palette.control)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.refresh_remote_environment(cx)),
                            ),
                    )
                    .into_any_element()
            });
        div()
            .id("remote-drive-manager")
            .debug_selector(|| "remote-drive-manager".to_string())
            .role(Role::Dialog)
            .aria_label("Remote drives")
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(780.0 * self.palette.scale))
            .max_h(px(580.0))
            .border_1()
            .border_color(self.palette.border)
            .bg(self.palette.panel)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .child(div().text_lg().child("Remote drives"))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(environment_label),
                    )
                    .child(
                        toolbar_button("configure-remotes", "Configure rclone", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.configure_remote_backend(cx))),
                    )
                    .child(
                        toolbar_button("refresh-remotes", "Refresh", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_remote_environment(cx))),
                    )
                    .child(
                        toolbar_button("add-remote-profile", "Add", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.remote.available.is_empty() {
                                    this.show_toast(
                                        "Configure an rclone remote first",
                                        ToastKind::Warning,
                                        cx,
                                    );
                                } else {
                                    this.open_remote_profile_editor(None, cx);
                                }
                            })),
                    )
                    .child(
                        toolbar_button("close-remote-manager", "Close", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.close_control_surface(cx))),
                    ),
            )
            .children(editor)
            .children(exit_blocker)
            .children(blocked)
            .children(setup)
            .children(helper_setup)
            .child(
                div()
                    .id("remote-profile-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(rows.is_empty(), |list| {
                        list.child(
                            div()
                                .px_4()
                                .py_6()
                                .text_color(self.palette.muted)
                                .child("No native remote-drive profiles"),
                        )
                    })
                    .children(rows),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(self.palette.border)
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child("Profiles contain no provider credentials. rclone remains the credential authority."),
            )
            .into_any_element()
    }
}
