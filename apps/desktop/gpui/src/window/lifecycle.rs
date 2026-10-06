//! `DirectoryWindow` behavior for lifecycle.

use crate::*;

impl DirectoryWindow {
    pub fn install_shortcut_bindings(&self, cx: &mut Context<Self>) {
        cx.clear_key_bindings();
        let mut bindings = application_key_bindings(&self.settings.shortcut_bindings);
        bindings.extend(native_text_input::key_bindings(
            shortcut::text_paste_belongs_to_menu(&self.settings.shortcut_bindings),
        ));
        cx.bind_keys(bindings);
    }

    pub fn new(path: PathBuf, services: NativeServices, cx: &mut Context<Self>) -> Self {
        Self::from_session(
            SessionState::new(path),
            services,
            PersistenceStores {
                session: None,
                settings: None,
                workspaces: None,
                operation_recovery: None,
                interrupted_operations: Vec::new(),
                window_lifetime: None,
            },
            AppSettings::default(),
            WorkspaceState::empty(),
            None,
            cx,
        )
    }

    pub fn restore(
        path: PathBuf,
        startup_path_is_explicit: bool,
        services: NativeServices,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::restore_window_session(
            path,
            startup_path_is_explicit,
            services,
            None,
            "primary".to_string(),
            cx,
        )
    }

    pub fn restore_window_session(
        path: PathBuf,
        startup_path_is_explicit: bool,
        services: NativeServices,
        runtime: Option<WindowRuntime>,
        session_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let session_path = runtime.as_ref().map_or_else(
            || services.resources().config_dir.join("session-v1.json"),
            |runtime| {
                runtime
                    .registry
                    .session_path(&services.resources().config_dir, &session_id)
            },
        );
        let (operation_recovery, interrupted_operations, operation_recovery_warning) =
            match runtime.as_ref() {
                Some(runtime) => runtime.operation_recovery(),
                None => OperationRecoveryStore::open(&services.resources().config_dir),
            };
        let window_lifetime = runtime.map(|runtime| runtime.lifetime(session_id));
        let (store, mut session, warning) = SessionStore::open(session_path, path.clone());
        let (settings_store, settings, settings_warning) =
            SettingsStore::open(&services.resources().config_dir);
        let (workspace_store, workspaces, workspace_warning) =
            WorkspaceStore::open(&services.resources().config_dir, &settings.legacy_values);
        if session.go_to_folder_recents().is_empty()
            && let Some(raw) = settings.legacy_values.get("explorie:goToFolderRecent")
            && let Ok(paths) = serde_json::from_str::<Vec<String>>(raw)
        {
            session.seed_go_to_folder_recents(paths.into_iter().map(PathBuf::from));
            let _ = store.save(&session);
        }
        session.apply_browser_preferences(
            settings.view.show_hidden,
            settings.view.show_system_files,
            settings.view.filter_mode,
            settings.view.sort_key.clone(),
            settings.view.sort_direction,
            settings.view.view_mode,
        );
        if let Some(criteria) = session
            .active_smart_folder()
            .map(|folder| folder.criteria().clone())
        {
            apply_smart_folder_browser_state(&mut session, &criteria);
        }
        if startup_path_is_explicit {
            let path_changed = session.path() != path && session.navigate(path);
            let smart_folder_cleared = session.active_smart_folder().is_some();
            if smart_folder_cleared {
                session.clear_active_smart_folder();
            }
            session.apply_browser_preferences(
                settings.view.show_hidden,
                settings.view.show_system_files,
                settings.view.filter_mode,
                settings.view.sort_key.clone(),
                settings.view.sort_direction,
                settings.view.view_mode,
            );
            session.clear_search();
            if path_changed || smart_folder_cleared {
                session.record_current_path();
                let _ = store.save(&session);
            }
        }
        Self::from_session(
            session,
            services,
            PersistenceStores {
                session: Some(store),
                settings: Some(settings_store),
                workspaces: Some(workspace_store),
                operation_recovery,
                interrupted_operations,
                window_lifetime,
            },
            settings,
            workspaces,
            join_warnings(
                join_warnings(join_warnings(warning, settings_warning), workspace_warning),
                operation_recovery_warning,
            ),
            cx,
        )
    }

    pub(crate) fn from_session(
        mut browser: SessionState,
        mut services: NativeServices,
        mut stores: PersistenceStores,
        mut settings: AppSettings,
        mut workspaces: WorkspaceState,
        warning: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        #[cfg(test)]
        cx.background_executor().allow_parking();

        services = services.fork_window_scope();

        let mut shared_state_revision = 0;
        if let Some(runtime) = stores
            .window_lifetime
            .as_ref()
            .map(|lifetime| &lifetime.runtime)
        {
            let local_window_placement = settings.window_placement;
            let (shared, revision) = runtime.adopt_or_snapshot(
                settings.clone(),
                workspaces.clone(),
                browser.shared_state(),
            );
            settings = shared.settings;
            settings.window_placement = local_window_placement;
            workspaces = shared.workspaces;
            browser.apply_shared_state(shared.session);
            shared_state_revision = revision;
        }
        let runtime = stores
            .window_lifetime
            .as_ref()
            .map(|lifetime| lifetime.runtime.clone());
        let settings_store = stores.settings.take().map(|store| match runtime.as_ref() {
            Some(runtime) => runtime.share_settings_store(store),
            None => Arc::new(store),
        });
        let workspace_store = stores
            .workspaces
            .take()
            .map(|store| match runtime.as_ref() {
                Some(runtime) => runtime.share_workspace_store(store),
                None => Arc::new(store),
            });

        let pending_scroll_restore = if browser.has_current_folder_view_state() {
            let (scroll_index, grid_min_width, show_preview_panel) = browser.folder_ui_state();
            settings.view.view_mode = browser.view_mode();
            settings.view.sort_key = browser.sort_key();
            settings.view.sort_direction = browser.sort_direction();
            settings.appearance.grid_min_width = grid_min_width.clamp(120, 260);
            settings.view.show_preview_panel = show_preview_panel;
            Some(scroll_index)
        } else {
            browser.sync_folder_ui_state(
                0,
                settings.appearance.grid_min_width,
                settings.view.show_preview_panel,
            );
            Some(0)
        };
        let columns = ColumnState::new(browser.path());
        let undo_timeout =
            Duration::from_secs(u64::from(settings.behavior.undo_timeout_minutes) * 60);
        let palette = UiPalette::for_settings(&settings, WindowAppearance::Dark);
        let sidebar_width = settings.view.sidebar_width;
        let preview_panel_width = settings.view.preview_panel_width;
        let remembered_window = browser.window_placement().map_or(
            WorkspaceWindowState {
                width: settings.window_placement.width,
                height: settings.window_placement.height,
                x: settings.window_placement.x,
                y: settings.window_placement.y,
            },
            |placement| WorkspaceWindowState {
                width: placement.width,
                height: placement.height,
                x: placement.x,
                y: placement.y,
            },
        );
        let has_remembered_window = remembered_window.width.is_some()
            || remembered_window.height.is_some()
            || remembered_window.x.is_some()
            || remembered_window.y.is_some();
        let image_memory = ImageMemory::default();
        let media = Self::new_media_player(&services, &image_memory, palette, cx);
        let panes = WindowPanes::new(&cx.entity(), cx);
        Self {
            plugin_ui: plugins_ui::PluginUiState::default(),
            browser,
            services,
            window_lifetime: stores.window_lifetime,
            listing: ListingUi {
                state: ListingState::Loading,
                task: None,
                generation: 0,
                reset_scroll_on_result: false,
                pending_scroll_restore,
                scroll_handle: UniformListScrollHandle::new(),
                warning: None,
                announced_warnings: VecDeque::new(),
                type_select_value: String::new(),
                type_select_at: None,
            },
            search: SearchUi::default(),
            system: SystemUi::default(),
            disk: DiskInfoState::default(),
            watcher: WatcherUi::default(),
            column_view: ColumnViewUi {
                columns,
                tasks: Vec::new(),
                generation: 0,
                scroll_handles: Vec::new(),
                strip_scroll: ScrollHandle::new(),
                scroll_to_leaf_attempts: 0,
                preview_fill: 0.0,
                selection: BTreeSet::new(),
                pending_selection: None,
            },
            service_event_task: None,
            shared_state_task: None,
            shared_state_revision,
            operations_revision: 0,
            single_instance_tasks: Vec::new(),
            focus_handle: cx.focus_handle(),
            status_message: warning,
            calculate_folder_sizes: settings.view.show_folder_sizes,
            session_store: stores.session,
            settings,
            settings_store,
            workspaces,
            workspace_store,
            workspace_ui: WorkspaceUi::default(),
            smart_folder_editor: None,
            layout: WindowLayout {
                sidebar_width,
                sidebar_collapsed: false,
                sidebar_resize_focus: cx.focus_handle(),
                preview_panel_width,
                preview_panel_resize_focus: cx.focus_handle(),
                listing_viewport_width: DEFAULT_WINDOW_WIDTH - sidebar_width,
                grid_columns: 1,
                last_window_bounds: remembered_window,
                pending_workspace_bounds: has_remembered_window.then_some(remembered_window),
            },
            pointer: PointerInteractions::default(),
            favorite_focus_handles: Vec::new(),
            remote: RemoteDrivesUi::default(),
            batch_rename: None,
            mutation: MutationUi::default(),
            settings_ui: SettingsUi::new(cx),
            overlay: OverlayUi::default(),
            #[cfg(target_os = "macos")]
            title_bar_drag_pending: false,
            text_input: NativeTextInputState::default(),
            recovery: RecoveryUi {
                previous_session_unclean: false,
                notice: !stores.interrupted_operations.is_empty(),
                store: stores.operation_recovery,
                interrupted: stores.interrupted_operations,
                jobs: HashMap::new(),
            },
            toasts: ToastQueue::default(),
            error_reports: ErrorReportLog::default(),
            context_menu: ContextMenuUi::default(),
            palette,
            operations: OperationQueue::default(),
            operation_ui: OperationUi::default(),
            clipboard: ClipboardUi {
                state: None,
                files: file_clipboard::default_file_clipboard(),
                system: SystemClipboard::Unknown,
                written: None,
            },
            navigation_ui: NavigationUi::default(),
            preview: PreviewUi::default(),
            quick_look: QuickLookUi::default(),
            image_memory,
            media,
            visuals: EntryVisuals::default(),
            panes,
            undo_ledger: UndoLedger::with_timeout(undo_timeout),
            #[cfg(test)]
            last_rendered_items: 0,
            #[cfg(test)]
            render_stats: RenderStats::default(),
        }
    }

    pub fn start_single_instance_requests(
        &mut self,
        requests: Arc<Mutex<mpsc::Receiver<SingleInstanceRequest>>>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let executor = cx.background_executor().clone();
        self.single_instance_tasks
            .push(cx.spawn_in(window, async move |this, cx| {
                loop {
                    executor.timer(Duration::from_millis(50)).await;
                    let mut pending = Vec::new();
                    let mut disconnected = false;
                    loop {
                        match requests
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .try_recv()
                        {
                            Ok(request) => pending.push(request),
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                disconnected = true;
                                break;
                            }
                        }
                    }

                    for request in pending {
                        if cx
                            .update_root(|_, window, app| {
                                let focus = this.update(app, |view, cx| {
                                    if let Some(path) = request.path {
                                        view.navigate_to(path, cx);
                                    }
                                    view.focus_handle.clone()
                                });
                                if let Ok(focus) = focus {
                                    window.activate_window();
                                    window.focus(&focus, app);
                                    app.activate(true);
                                }
                            })
                            .is_err()
                        {
                            return;
                        }
                    }

                    if disconnected {
                        return;
                    }
                }
            }));
    }

    pub(crate) fn open_new_window(&mut self, move_active_tab: bool, cx: &mut Context<Self>) {
        let Some(runtime) = self
            .window_lifetime
            .as_ref()
            .map(|lifetime| lifetime.runtime.clone())
        else {
            self.show_toast(
                "New windows are unavailable in this runtime",
                ToastKind::Warning,
                cx,
            );
            return;
        };
        let session_id = runtime.next_session_id();
        if let Err(error) = runtime.register(&session_id) {
            self.show_toast(
                format!("Unable to register new window: {error}"),
                ToastKind::Warning,
                cx,
            );
            return;
        }
        self.sync_active_tab_view_state();
        self.save_session_snapshot();
        let path = self.browser.path().to_path_buf();
        let session_path = runtime
            .registry
            .session_path(&self.services.resources().config_dir, &session_id);
        let (session_store, _, _) = SessionStore::open(session_path, path.clone());
        if let Err(error) = session_store.save(&self.browser.active_tab_window_session()) {
            let _ = runtime.registry.remove(&session_id);
            self.show_toast(
                format!("Unable to save new window session: {error}"),
                ToastKind::Warning,
                cx,
            );
            return;
        }
        session_store.flush();
        let services = self.services.clone();
        let centered = Bounds::centered(
            None,
            gpui::size(px(DEFAULT_WINDOW_WIDTH), px(DEFAULT_WINDOW_HEIGHT)),
            cx,
        );
        let displays: Vec<_> = cx
            .displays()
            .into_iter()
            .map(|display| display.bounds())
            .collect();
        let fallback_bounds =
            cascaded_window_bounds(self.layout.last_window_bounds, centered, &displays);
        let options = desktop_window_options(fallback_bounds);
        let runtime_for_window = runtime.clone();
        let session_id_for_window = session_id.clone();
        let opened = cx.open_window(options, move |window, cx| {
            window.set_window_title(APP_NAME);
            let view = cx.new(|cx| {
                let mut view = DirectoryWindow::restore_window_session(
                    path,
                    true,
                    services,
                    Some(runtime_for_window),
                    session_id_for_window,
                    cx,
                );
                view.install_shortcut_bindings(cx);
                view.start_listing(cx);
                view.start_watching(cx);
                view.start_system_locations(cx);
                view.start_service_events(cx);
                view.start_preview_helpers(cx);
                view.start_system_integration_status(cx);
                view.start_remote_drives(cx);
                view
            });
            let close_view = view.clone();
            window.on_window_should_close(cx, move |_, cx| {
                close_view.update(cx, |view, cx| view.request_platform_window_close(cx))
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        });
        match opened {
            Ok(_) => {
                if move_active_tab && self.browser.tabs().len() > 1 {
                    self.close_active_tab(cx);
                }
                cx.activate(true);
            }
            Err(error) => {
                let _ = runtime.registry.remove(&session_id);
                self.show_toast(
                    format!("Unable to open new window: {error}"),
                    ToastKind::Warning,
                    cx,
                );
            }
        }
    }

    pub fn start_service_events(&mut self, cx: &mut Context<Self>) {
        self.start_plugin_status(cx);
        self.start_shared_state_sync(cx);
        let subscription = self.services.subscribe_async();
        self.service_event_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let event = subscription.next().await;
                let _ = this.update(cx, |view, cx| {
                    view.apply_service_event(event, cx);
                });
            }
        }));
    }

    pub(crate) fn start_shared_state_sync(&mut self, cx: &mut Context<Self>) {
        if self.shared_state_task.is_some() || self.window_lifetime.is_none() {
            return;
        }
        let executor = cx.background_executor().clone();
        self.shared_state_task = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(100)).await;
                if this
                    .update(cx, |view, cx| {
                        view.pull_shared_state(cx);
                        view.pull_shared_operations(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub(crate) fn pull_shared_state(&mut self, cx: &mut Context<Self>) {
        let Some((shared, revision)) = self
            .window_lifetime
            .as_ref()
            .and_then(|lifetime| lifetime.runtime.snapshot_after(self.shared_state_revision))
        else {
            return;
        };
        self.shared_state_revision = revision;

        // Folder view and window geometry belong to this window.
        let mut settings = shared.settings;
        settings.window_placement = self.settings.window_placement;
        settings.view.view_mode = self.settings.view.view_mode;
        settings.view.sort_key = self.settings.view.sort_key.clone();
        settings.view.sort_direction = self.settings.view.sort_direction;
        settings.view.show_preview_panel = self.settings.view.show_preview_panel;
        settings.appearance.grid_min_width = self.settings.appearance.grid_min_width;
        let settings_changed = settings != self.settings;
        let workspaces_changed = shared.workspaces != self.workspaces;
        let session_changed = shared.session != self.browser.shared_state();
        // Most revisions come from this window's own changes or touch state this
        // window already has; skip the rebuild, session save and redraw.
        if !settings_changed && !workspaces_changed && !session_changed {
            return;
        }

        if settings_changed {
            let previous = std::mem::replace(&mut self.settings, settings);
            let view = &self.settings.view;
            if (view.show_hidden, view.show_system_files, view.filter_mode)
                != (
                    previous.view.show_hidden,
                    previous.view.show_system_files,
                    previous.view.filter_mode,
                )
            {
                self.browser.apply_common_preferences(
                    view.show_hidden,
                    view.show_system_files,
                    view.filter_mode,
                );
            }
            self.calculate_folder_sizes = self.settings.view.show_folder_sizes;
            if self.pointer.sidebar_resize.is_none() {
                self.layout.sidebar_width = self.settings.view.sidebar_width;
            }
            if self.pointer.preview_panel_resize.is_none() {
                self.layout.preview_panel_width = self.settings.view.preview_panel_width;
            }
            self.undo_ledger.set_timeout(Duration::from_secs(
                u64::from(self.settings.behavior.undo_timeout_minutes) * 60,
            ));
            if self.settings.shortcut_bindings != previous.shortcut_bindings {
                self.install_shortcut_bindings(cx);
            }
        }
        if workspaces_changed {
            self.workspaces = shared.workspaces;
        }
        if session_changed {
            self.browser.apply_shared_state(shared.session);
            if let Some(store) = &self.session_store {
                let _ = store.save(&self.browser);
            }
        }
        cx.notify();
    }

    pub(crate) fn pull_shared_operations(&mut self, cx: &mut Context<Self>) {
        if self.sync_interrupted_operations() {
            cx.notify();
        }
        let Some(revision) = self
            .window_lifetime
            .as_ref()
            .map(|lifetime| lifetime.runtime.operations_revision())
        else {
            return;
        };
        if revision == self.operations_revision {
            return;
        }
        self.operations_revision = revision;
        if self
            .foreign_operation_summaries()
            .iter()
            .any(|operation| operation.status == OperationStatus::Running)
        {
            self.operation_ui.panel_hidden = false;
        }
        cx.notify();
    }

    pub fn request_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        self.persist_window_placement();
        let secondary_window = self
            .window_lifetime
            .as_ref()
            .is_some_and(|lifetime| !lifetime.runtime.is_last_window());
        if secondary_window
            && (self.operations.active_count() > 0
                || self.mutation.in_progress
                || self.operation_ui.undo_progress.is_some())
        {
            self.status_message =
                Some("Finish or cancel this window's filesystem work before closing".to_string());
            cx.notify();
            return false;
        }
        if secondary_window {
            return true;
        }
        self.request_app_exit(cx);
        false
    }

    /// Exits the app once active native mutations finish and remote drives
    /// disconnect cleanly; otherwise shows why the app is staying open.
    pub(crate) fn request_app_exit(&mut self, cx: &mut Context<Self>) {
        if self.services.mutations.request_exit() {
            self.begin_overlay_focus();
            self.mutation.exit_waiting = true;
            self.status_message = Some(format!(
                "Waiting for {} native mutation(s) before closing",
                self.services.mutations.active_count()
            ));
            cx.notify();
            return;
        }
        self.start_remote_close_check(cx);
    }

    pub fn request_platform_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.request_window_dismiss(cx)
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.request_window_close(cx)
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn request_window_dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        self.persist_window_placement();
        if self.operations.active_count() > 0
            || self.mutation.in_progress
            || self.operation_ui.undo_progress.is_some()
        {
            self.status_message =
                Some("Finish or cancel this window's filesystem work before closing".to_string());
            cx.notify();
            return false;
        }
        true
    }

    pub(crate) fn keep_app_open(&mut self, cx: &mut Context<Self>) {
        self.mutation.exit_waiting = false;
        self.finish_overlay_focus_if_inactive();
        self.status_message =
            Some("Close request cancelled; filesystem work continues".to_string());
        cx.notify();
    }

    pub(crate) fn cancel_operations_and_close(&mut self, cx: &mut Context<Self>) {
        self.services.mutations.cancel_all();
        self.mutation.exit_waiting = true;
        self.status_message = Some(
            "Cancelling active file transfers; other native mutations will finish safely"
                .to_string(),
        );
        cx.notify();
    }

    pub(crate) fn finish_waiting_exit(&mut self, cx: &mut Context<Self>) {
        if !self.mutation.exit_waiting || self.services.mutations.request_exit() {
            return;
        }
        self.mutation.exit_waiting = false;
        self.finish_overlay_focus_if_inactive();
        self.start_remote_close_check(cx);
    }

    pub(crate) fn apply_service_event(&mut self, event: ServiceEvent, cx: &mut Context<Self>) {
        match event {
            ServiceEvent::FileOperation(event) => self.apply_file_operation_event(event, cx),
            ServiceEvent::ArchiveProgress(progress) => {
                if let Some(lifetime) = self.window_lifetime.as_ref() {
                    lifetime.runtime.apply_archive_progress(&progress);
                    let owns_operation = self.operation_ui.archive_creation_id.as_deref()
                        == Some(progress.operation_id.as_str())
                        || self.operation_ui.archive_extraction_id.as_deref()
                            == Some(progress.operation_id.as_str());
                    if !owns_operation {
                        self.notify_progress(cx);
                        return;
                    }
                }
                self.status_message = Some(if progress.total_bytes > 0 {
                    format!(
                        "Archiving {} • {} / {}",
                        progress.current_path,
                        format_size(progress.processed_bytes),
                        format_size(progress.total_bytes)
                    )
                } else {
                    format!("Archiving {}", progress.current_path)
                });
                self.operation_ui.archive_progress = Some(progress);
                self.notify_progress(cx);
            }
            ServiceEvent::SearchProgress(progress) => self.apply_search_progress(progress, cx),
            ServiceEvent::RemoteDriveStatus(status) => {
                let new_error = status.error.as_ref().filter(|error| {
                    self.remote
                        .statuses
                        .get(&status.id)
                        .and_then(|previous| previous.error.as_ref())
                        .is_none_or(|previous| previous.message != error.message)
                });
                if let Some(error) = new_error {
                    self.record_error("Remote drive failed", error.to_string());
                }
                self.remote.statuses.insert(status.id.clone(), status);
                cx.notify();
            }
            ServiceEvent::RemoteDriveExitBlocked(blocker) => {
                self.status_message = Some(format!(
                    "Remote-drive shutdown blocked • {} pending upload(s) • {} error(s)",
                    blocker.pending_uploads, blocker.errored_files
                ));
                self.remote.exit_blocker = Some(blocker);
                if self.has_control_draft() || self.has_settings_draft() {
                    self.show_toast(
                        "Remote-drive shutdown needs attention; finish the current draft first",
                        ToastKind::Warning,
                        cx,
                    );
                } else {
                    self.open_control_surface(ControlSurface::RemoteDrives, cx);
                }
            }
            ServiceEvent::MutationIdle => self.finish_waiting_exit(cx),
            ServiceEvent::PluginStatusChanged { id, contribution } => {
                self.apply_plugin_contribution(id, contribution, cx);
            }
            ServiceEvent::PluginRegistryChanged => {
                self.clear_plugin_context(cx);
                self.start_plugin_status(cx);
            }
            ServiceEvent::HelperStatus(_) | ServiceEvent::Watcher(_) => {}
        }
    }
}
