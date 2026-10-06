//! `DirectoryWindow` behavior for mutation prompts.

use super::archives::existing_folder_message;
use crate::*;

/// Renames, new items, deletes and other mutations started from this window:
/// the prompt collecting input, the jobs running, and whether the app waits for
/// them before quitting.
#[derive(Default)]
pub(crate) struct MutationUi {
    pub(crate) prompt: Option<MutationPrompt>,
    pub(crate) tasks: Vec<Task<()>>,
    pub(crate) in_progress: bool,
    pub(crate) exit_waiting: bool,
}

impl DirectoryWindow {
    pub(crate) fn prompt_new_folder(&mut self, cx: &mut Context<Self>) {
        self.search.active = false;
        self.mutation.prompt = Some(MutationPrompt::new(
            MutationPromptKind::NewFolder,
            String::new(),
        ));
        cx.notify();
    }

    pub(crate) fn prompt_new_note(&mut self, cx: &mut Context<Self>) {
        self.search.active = false;
        self.mutation.prompt = Some(MutationPrompt::new(
            MutationPromptKind::NewNote,
            String::new(),
        ));
        cx.notify();
    }

    pub(crate) fn prompt_new_website_link(&mut self, cx: &mut Context<Self>) {
        self.search.active = false;
        self.mutation.prompt = Some(MutationPrompt::new(
            MutationPromptKind::NewWebsiteLinkName,
            String::new(),
        ));
        cx.notify();
    }

    /// Return confirms an open prompt: it applies a file prompt, and skips
    /// the item in a conflict prompt (as its key hint says). In the list,
    /// Return is Rename on macOS and Open on Windows; neither may act on the
    /// selection behind a prompt. Returns whether a prompt took the key.
    pub(crate) fn confirm_open_prompt(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.operation_ui.conflict_prompts.is_empty() {
            self.resolve_file_conflict(FileConflictChoice::Skip, cx);
            true
        } else if self.mutation.prompt.is_some() {
            self.submit_mutation_prompt(cx);
            true
        } else {
            false
        }
    }

    pub(crate) fn prompt_rename_selected(&mut self, cx: &mut Context<Self>) {
        let paths = self.effective_selected_paths();
        if paths.len() > 1 {
            self.open_batch_rename(cx);
            return;
        }
        if paths.is_empty() {
            self.status_message = Some("Select one or more items to rename".to_string());
            cx.notify();
            return;
        }
        let source = paths.into_iter().next().expect("one selected path");
        self.prompt_rename_path(source, cx);
    }

    pub(crate) fn prompt_rename_path(&mut self, source: PathBuf, cx: &mut Context<Self>) {
        let input = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.search.active = false;
        self.mutation.prompt = Some(MutationPrompt::new(
            MutationPromptKind::Rename { source },
            input,
        ));
        cx.notify();
    }

    pub(crate) fn submit_mutation_prompt(&mut self, cx: &mut Context<Self>) {
        if self.undo_ledger.is_processing()
            || self.mutation.in_progress
            || self.operations.active_count() > 0
        {
            self.status_message =
                Some("Wait for the current filesystem change to finish".to_string());
            cx.notify();
            return;
        }
        let Some(mut prompt) = self.mutation.prompt.take() else {
            return;
        };
        prompt.error = None;
        if let MutationPromptKind::ArchiveName {
            sources,
            format,
            compression_level,
        } = &prompt.kind
        {
            let name = prompt.input.trim();
            if !valid_leaf_name(name) {
                let message = "Use a valid single archive name".to_string();
                self.status_message = Some(message.clone());
                prompt.error = Some(message);
                self.mutation.prompt = Some(prompt);
                cx.notify();
                return;
            }
            let output_path = self
                .browser
                .path()
                .join(format!("{name}{}", archive_extension(*format)));
            self.mutation.prompt = Some(MutationPrompt::new(
                MutationPromptKind::ArchivePassword {
                    sources: sources.clone(),
                    output_path,
                    format: *format,
                    compression_level: *compression_level,
                },
                String::new(),
            ));
            self.status_message = Some("Optionally enter an archive password".to_string());
            cx.notify();
            return;
        }
        if let MutationPromptKind::ExtractDirectory { archive_path } = &prompt.kind {
            let name = prompt.input.trim();
            if !valid_leaf_name(name) {
                let message = "Use a valid single extraction-folder name".to_string();
                self.status_message = Some(message.clone());
                prompt.error = Some(message);
                self.mutation.prompt = Some(prompt);
                cx.notify();
                return;
            }
            // Extraction always creates a new folder; the core refuses to
            // merge into (and overwrite files in) an existing one.
            let output_dir = self.browser.path().join(name);
            if std::fs::symlink_metadata(&output_dir).is_ok() {
                let message = existing_folder_message(name);
                self.status_message = Some(message.clone());
                prompt.error = Some(message);
                self.mutation.prompt = Some(prompt);
                cx.notify();
                return;
            }
            self.mutation.prompt = Some(MutationPrompt::new(
                MutationPromptKind::ExtractPassword {
                    archive_path: archive_path.clone(),
                    output_dir,
                    allow_extended_limits: false,
                },
                String::new(),
            ));
            self.status_message =
                Some("Enter a password only if the archive requires one".to_string());
            cx.notify();
            return;
        }
        if let MutationPromptKind::ArchivePassword {
            sources,
            output_path,
            format,
            compression_level,
        } = &prompt.kind
        {
            if matches!(format, ArchiveFormat::Tar | ArchiveFormat::TarGz)
                && !prompt.input.is_empty()
            {
                let message =
                    "TAR and TAR.GZ do not support passwords; leave this field empty".to_string();
                self.status_message = Some(message.clone());
                prompt.error = Some(message);
                self.mutation.prompt = Some(prompt);
                cx.notify();
                return;
            }
            let operation_id = format!(
                "gpui-archive-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let request = CompressRequest {
                paths: sources.clone(),
                output_path: output_path.clone(),
                format: *format,
                compression_level: *compression_level,
                password: (!prompt.input.is_empty()).then(|| prompt.input.clone()),
                operation_id: operation_id.clone(),
            };
            let runtime = self.window_lifetime.as_ref().map(|lifetime| {
                lifetime.runtime.track_background_operation(
                    lifetime.id.clone(),
                    operation_id.clone(),
                    "Create archive",
                    sources.len(),
                    Some(output_path.clone()),
                );
                lifetime.runtime.clone()
            });
            let task = self.services.archives.compress(request);
            let retry_prompt = prompt.clone();
            self.mutation.in_progress = true;
            self.operation_ui.archive_progress = None;
            self.operation_ui.archive_creation_id = Some(operation_id.clone());
            self.status_message = Some("Creating archive…".to_string());
            self.push_mutation_task(cx.spawn(async move |this, cx| {
                let result = task.await;
                if let Some(runtime) = runtime.as_ref() {
                    let (status, error) = match &result {
                        Ok(_) => (OperationStatus::Completed, None),
                        Err(error) => (OperationStatus::Failed, Some(error.to_string())),
                    };
                    runtime.finish_background_operation(&operation_id, status, error);
                }
                let _ = this.update(cx, |view, cx| {
                    view.mutation.in_progress = false;
                    view.operation_ui.archive_progress = None;
                    view.operation_ui.archive_creation_id = None;
                    match result {
                        Ok(result) => {
                            view.refresh(cx);
                            view.status_message = Some(format!(
                                "Created {} ({})",
                                result.output_path.display(),
                                format_size(result.total_bytes)
                            ));
                        }
                        Err(error) => {
                            view.record_error("Archive creation failed", error.to_string());
                            let message = format!("Archive creation failed: {error}");
                            view.status_message = Some(message.clone());
                            let mut retry_prompt = retry_prompt;
                            retry_prompt.error = Some(message);
                            view.mutation.prompt = Some(retry_prompt);
                        }
                    }
                    cx.notify();
                });
            }));
            cx.notify();
            return;
        }
        if let MutationPromptKind::ExtractPassword {
            archive_path,
            output_dir,
            allow_extended_limits,
        } = &prompt.kind
        {
            let operation_id = uuid::Uuid::new_v4().to_string();
            let runtime = self.window_lifetime.as_ref().map(|lifetime| {
                lifetime.runtime.track_background_operation(
                    lifetime.id.clone(),
                    operation_id.clone(),
                    "Extract archive",
                    1,
                    Some(output_dir.clone()),
                );
                lifetime.runtime.clone()
            });
            let task = self.services.archives.extract(ExtractRequest {
                archive_path: archive_path.clone(),
                output_dir: output_dir.clone(),
                password: (!prompt.input.is_empty()).then(|| prompt.input.clone()),
                allow_extended_limits: *allow_extended_limits,
                operation_id: operation_id.clone(),
            });
            let retry_prompt = prompt.clone();
            self.mutation.in_progress = true;
            self.operation_ui.archive_progress = None;
            self.operation_ui.archive_extraction_id = Some(operation_id.clone());
            self.status_message = Some("Extracting archive…".to_string());
            self.push_mutation_task(cx.spawn(async move |this, cx| {
                let result = task.await;
                if let Some(runtime) = runtime.as_ref() {
                    let (status, error) = match &result {
                        Ok(_) => (OperationStatus::Completed, None),
                        Err(error) if error.code == ErrorCode::Cancelled => {
                            (OperationStatus::Cancelled, None)
                        }
                        Err(error) => (OperationStatus::Failed, Some(error.to_string())),
                    };
                    runtime.finish_background_operation(&operation_id, status, error);
                }
                let _ = this.update(cx, |view, cx| {
                    view.mutation.in_progress = false;
                    view.operation_ui.archive_progress = None;
                    view.operation_ui.archive_extraction_id = None;
                    match result {
                        Ok(result) => {
                            view.refresh(cx);
                            view.status_message = Some(format!(
                                "Extracted {} to {}",
                                format_size(result.total_bytes),
                                result.output_dir.display()
                            ));
                        }
                        Err(error) => {
                            if error.code == ErrorCode::Cancelled {
                                view.status_message =
                                    Some("Archive extraction cancelled".to_string());
                                cx.notify();
                                return;
                            }
                            if error.code == ErrorCode::Conflict
                                && let MutationPromptKind::ExtractPassword {
                                    archive_path,
                                    output_dir,
                                    ..
                                } = &retry_prompt.kind
                                && std::fs::symlink_metadata(output_dir).is_ok()
                            {
                                // The folder appeared after its name was
                                // chosen: ask for another name.
                                let name = output_dir
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                let message = existing_folder_message(&name);
                                let mut prompt = MutationPrompt::new(
                                    MutationPromptKind::ExtractDirectory {
                                        archive_path: archive_path.clone(),
                                    },
                                    name,
                                );
                                prompt.error = Some(message.clone());
                                view.status_message = Some(message);
                                view.mutation.prompt = Some(prompt);
                                cx.notify();
                                return;
                            }
                            view.record_error("Archive extraction failed", error.to_string());
                            let message = if error.retryable {
                                format!(
                                    "{error}. Press Enter again to approve extended extraction limits"
                                )
                            } else {
                                format!("Archive extraction failed: {error}")
                            };
                            view.status_message = Some(message.clone());
                            let mut retry_prompt = retry_prompt;
                            if error.retryable
                                && let MutationPromptKind::ExtractPassword {
                                    allow_extended_limits,
                                    ..
                                } = &mut retry_prompt.kind
                            {
                                *allow_extended_limits = true;
                            }
                            retry_prompt.error = Some(message);
                            view.mutation.prompt = Some(retry_prompt);
                        }
                    }
                    cx.notify();
                });
            }));
            cx.notify();
            return;
        }
        if let MutationPromptKind::PermanentDelete { items } = &prompt.kind {
            if prompt.input != "DELETE" {
                let message = "Type DELETE exactly to confirm permanent deletion".to_string();
                self.status_message = Some(message.clone());
                prompt.error = Some(message);
                self.mutation.prompt = Some(prompt);
                cx.notify();
                return;
            }
            let count = items.len();
            let items = items.clone();
            let services = self.services.clone();
            self.mutation.in_progress = true;
            self.status_message = Some(format!("Permanently deleting {count} item(s)…"));
            self.push_mutation_task(cx.spawn(async move |this, cx| {
                let result = permanently_delete_items(services, items).await;
                let _ = this.update(cx, |view, cx| {
                    view.mutation.in_progress = false;
                    view.refresh(cx);
                    view.status_message = Some(match result {
                        Ok(result) => match result.failure {
                            None => format!(
                                "Permanently deleted {} item(s) • This cannot be undone",
                                result.deleted_items
                            ),
                            Some(failure) => format!(
                                "Permanent deletion stopped after {} item(s); {} failed: {}",
                                result.deleted_items,
                                failure.path.display(),
                                failure.error
                            ),
                        },
                        Err(error) => format!("Permanent deletion failed: {error}"),
                    });
                    cx.notify();
                });
            }));
            cx.notify();
            return;
        }
        if let MutationPromptKind::Trash { paths } = prompt.kind {
            self.start_trash_operations(paths, cx);
            return;
        }
        let name = prompt.input.trim().to_string();
        if name.is_empty() {
            let message = "A name is required".to_string();
            self.status_message = Some(message.clone());
            prompt.error = Some(message);
            self.mutation.prompt = Some(prompt);
            cx.notify();
            return;
        }
        if let MutationPromptKind::Rename { source } = &prompt.kind
            && source.file_name() == Some(std::ffi::OsStr::new(&name))
        {
            // Submitting the unchanged name is not a rename.
            self.deactivate_native_text_input();
            cx.notify();
            return;
        }
        if matches!(prompt.kind, MutationPromptKind::NewWebsiteLinkName) {
            self.mutation.prompt = Some(MutationPrompt::new(
                MutationPromptKind::NewWebsiteLinkUrl { name },
                "https://".to_string(),
            ));
            self.status_message = Some("Enter the website URL".to_string());
            cx.notify();
            return;
        }
        let task = match &prompt.kind {
            MutationPromptKind::NewFolder => self
                .services
                .mutations
                .create_folder(self.browser.path().to_path_buf(), name),
            MutationPromptKind::NewNote => self
                .services
                .mutations
                .create_note(self.browser.path().to_path_buf(), name),
            MutationPromptKind::NewWebsiteLinkUrl { name: link_name } => self
                .services
                .mutations
                .create_website_link(self.browser.path().to_path_buf(), link_name.clone(), name),
            MutationPromptKind::NewWebsiteLinkName => unreachable!("handled above"),
            MutationPromptKind::Rename { source } => {
                self.services.mutations.rename_path(source.clone(), name)
            }
            MutationPromptKind::PermanentDelete { .. } => {
                unreachable!("handled before safe mutation dispatch")
            }
            MutationPromptKind::Trash { .. } => {
                unreachable!("handled before safe mutation dispatch")
            }
            MutationPromptKind::ArchiveName { .. }
            | MutationPromptKind::ArchivePassword { .. }
            | MutationPromptKind::ExtractDirectory { .. }
            | MutationPromptKind::ExtractPassword { .. } => {
                unreachable!("handled before safe mutation dispatch")
            }
        };
        self.mutation.in_progress = true;
        self.status_message = Some("Applying native filesystem change…".to_string());
        self.push_mutation_task(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.mutation.in_progress = false;
                match result {
                    Ok(path) => {
                        let path = PathBuf::from(path);
                        let record = match &prompt.kind {
                            MutationPromptKind::NewFolder => {
                                UndoRecord::created(CreatedKind::Folder, path.clone())
                            }
                            MutationPromptKind::NewNote => {
                                UndoRecord::created(CreatedKind::Note, path.clone())
                            }
                            MutationPromptKind::NewWebsiteLinkUrl { .. } => {
                                let url = prompt.input.trim().to_string();
                                UndoRecord::created(CreatedKind::WebsiteLink { url }, path.clone())
                            }
                            MutationPromptKind::NewWebsiteLinkName => {
                                unreachable!("handled before dispatch")
                            }
                            MutationPromptKind::Rename { source } => {
                                UndoRecord::renamed(source.clone(), path.clone())
                            }
                            MutationPromptKind::PermanentDelete { .. } => {
                                unreachable!("handled before undo recording")
                            }
                            MutationPromptKind::Trash { .. } => {
                                unreachable!("handled before undo recording")
                            }
                            MutationPromptKind::ArchiveName { .. }
                            | MutationPromptKind::ArchivePassword { .. }
                            | MutationPromptKind::ExtractDirectory { .. }
                            | MutationPromptKind::ExtractPassword { .. } => {
                                unreachable!("handled before undo recording")
                            }
                        };
                        view.undo_ledger.push(record);
                        view.refresh(cx);
                        view.status_message = Some(format!(
                            "Created or renamed {} • Undo available",
                            path.display()
                        ));
                    }
                    Err(error) => {
                        view.record_error("Filesystem change failed", error.to_string());
                        let message = format!("Filesystem change failed: {error}");
                        view.status_message = Some(message.clone());
                        let mut retry_prompt = prompt.clone();
                        retry_prompt.error = Some(message);
                        view.mutation.prompt = Some(retry_prompt);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn cancel_mutation_prompt(&mut self, cx: &mut Context<Self>) {
        if self.mutation.prompt.take().is_some() {
            self.deactivate_native_text_input();
            cx.notify();
        }
    }

    pub(crate) fn handle_mutation_prompt_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "backspace" => {
                if let Some(prompt) = &mut self.mutation.prompt {
                    prompt.error = None;
                    if prompt.replace_on_type {
                        prompt.input.clear();
                        prompt.replace_on_type = false;
                    } else {
                        prompt.input.pop();
                    }
                }
                cx.stop_propagation();
                cx.notify();
            }
            "escape" => {
                self.cancel_mutation_prompt(cx);
                cx.stop_propagation();
            }
            "enter" => {
                self.submit_mutation_prompt(cx);
                cx.stop_propagation();
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref()
                    && !text.chars().any(char::is_control)
                    && let Some(prompt) = &mut self.mutation.prompt
                {
                    prompt.error = None;
                    if prompt.replace_on_type {
                        prompt.input.clear();
                        prompt.replace_on_type = false;
                    }
                    prompt.input.push_str(text);
                    cx.stop_propagation();
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn render_mutation_exit_notice(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.mutation.exit_waiting {
            return div().into_any_element();
        }
        let active = self.services.mutations.active_count();
        div()
            .id("mutation-exit-backdrop")
            .debug_selector(|| "mutation-exit-backdrop".to_string())
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_center()
            .bg(with_alpha(rgb(0x000000), 0.58))
            .child(
                div()
                    .id("mutation-exit-notice")
                    .debug_selector(|| "mutation-exit-notice".to_string())
                    .role(Role::AlertDialog)
                    .aria_label("Finishing filesystem work before closing")
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .max_w(px(520.0 * self.palette.scale))
                    .p_5()
                    .border_1()
                    .border_color(rgb(0xffb86c))
                    .bg(self.palette.panel)
                    .child(div().text_lg().child("Finishing filesystem work before closing"))
                    .child(
                        div()
                            .text_sm()
                            .child(format!(
                                "{active} native mutation(s) are still active. Explorie will close automatically when they settle."
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(
                                "Cancel stops cancellable copy/move jobs; atomic renames, metadata, archives, and helper work are allowed to reach a safe boundary.",
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                toolbar_button(
                                    "keep-app-open",
                                    "Keep app open",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.keep_app_open(cx))),
                            )
                            .child(
                                toolbar_button(
                                    "cancel-operations-close",
                                    "Cancel transfers and close",
                                    rgb(0x6b482b),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_operations_and_close(cx)
                                })),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_mutation_prompt(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(prompt) = self.mutation.prompt.clone() else {
            return div().into_any_element();
        };
        let is_destructive = matches!(
            &prompt.kind,
            MutationPromptKind::PermanentDelete { .. } | MutationPromptKind::Trash { .. }
        );
        let is_archive = matches!(
            &prompt.kind,
            MutationPromptKind::ArchiveName { .. }
                | MutationPromptKind::ArchivePassword { .. }
                | MutationPromptKind::ExtractDirectory { .. }
                | MutationPromptKind::ExtractPassword { .. }
        );
        let is_basic = !is_destructive && !is_archive;
        let title = prompt.title();
        let (subtitle, submit_label) = match &prompt.kind {
            MutationPromptKind::NewFolder | MutationPromptKind::NewNote => (None, "Create"),
            MutationPromptKind::NewWebsiteLinkName => (None, "Next"),
            MutationPromptKind::NewWebsiteLinkUrl { .. } => {
                (Some("Enter an HTTP or HTTPS URL"), "Create")
            }
            MutationPromptKind::Rename { .. } => (None, "Rename"),
            MutationPromptKind::PermanentDelete { .. } => (
                Some("This action bypasses Trash and cannot be undone."),
                "Delete Permanently",
            ),
            MutationPromptKind::Trash { .. } => (
                Some("Items moved to the operating system Trash can be restored later."),
                "Move to Trash",
            ),
            MutationPromptKind::ArchiveName { sources, .. }
            | MutationPromptKind::ArchivePassword { sources, .. } => (
                Some(if sources.len() == 1 {
                    "1 item selected"
                } else {
                    "Multiple items selected"
                }),
                if matches!(&prompt.kind, MutationPromptKind::ArchiveName { .. }) {
                    "Next"
                } else {
                    "Create Archive"
                },
            ),
            MutationPromptKind::ExtractDirectory { .. } => {
                (Some("Choose an extraction folder"), "Next")
            }
            MutationPromptKind::ExtractPassword { .. } => (
                Some("Enter a password only when the archive requires one"),
                "Extract Archive",
            ),
        };
        let input_label = match &prompt.kind {
            MutationPromptKind::NewFolder => "Folder name",
            MutationPromptKind::NewNote => "Note name",
            MutationPromptKind::NewWebsiteLinkName => "Link name",
            MutationPromptKind::NewWebsiteLinkUrl { .. } => "Website URL",
            MutationPromptKind::Rename { .. } => "New name",
            MutationPromptKind::PermanentDelete { .. } => "Type DELETE to confirm",
            MutationPromptKind::Trash { .. } => "",
            MutationPromptKind::ArchiveName { .. } => "Archive name",
            MutationPromptKind::ArchivePassword { .. }
            | MutationPromptKind::ExtractPassword { .. } => "Password (optional)",
            MutationPromptKind::ExtractDirectory { .. } => "Extraction folder name",
        };
        let placeholder = match &prompt.kind {
            MutationPromptKind::PermanentDelete { .. } => "Type DELETE",
            MutationPromptKind::ArchivePassword { .. }
            | MutationPromptKind::ExtractPassword { .. } => "No password",
            _ => "Type here",
        };
        let destructive_items: Vec<(PathBuf, bool)> = match &prompt.kind {
            MutationPromptKind::PermanentDelete { items } => items.clone(),
            MutationPromptKind::Trash { paths } => {
                paths.iter().cloned().map(|path| (path, false)).collect()
            }
            _ => Vec::new(),
        };
        let destructive_count = destructive_items.len();
        let destructive_rows: Vec<AnyElement> = destructive_items
            .iter()
            .take(10)
            .enumerate()
            .map(|(index, (path, is_dir))| {
                div()
                    .id(("mutation-prompt-item", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .child(toolbar_icon(
                        if *is_dir { "folder" } else { "file" },
                        14.0,
                        self.palette.muted,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(path_label(path)),
                    )
                    .into_any_element()
            })
            .collect();
        let archive_options = if let MutationPromptKind::ArchiveName {
            format,
            compression_level,
            ..
        } = &prompt.kind
        {
            Some((
                format!("Format: {}", archive_format_label(*format)),
                format!("Compression: {}", compression_label(*compression_level)),
            ))
        } else {
            None
        };

        let content = div()
            .id("mutation-prompt-content")
            .debug_selector(|| "mutation-prompt-content".to_string())
            .flex()
            .flex_col()
            .gap_3()
            .when(is_destructive, |content| {
                content
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .p_3()
                            .border_1()
                            .border_color(self.palette.border)
                            .bg(self.palette.surface)
                            .text_sm()
                            .child("Items selected")
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(destructive_count.to_string()),
                            ),
                    )
                    .child(
                        div()
                            .id("mutation-prompt-items")
                            .debug_selector(|| "mutation-prompt-items".to_string())
                            .flex()
                            .flex_col()
                            .max_h(px(200.0))
                            .overflow_y_scroll()
                            .border_1()
                            .border_color(self.palette.border)
                            .children(destructive_rows)
                            .when(destructive_count > 10, |list| {
                                list.child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .text_center()
                                        .text_xs()
                                        .text_color(self.palette.muted)
                                        .child(format!(
                                            "…and {} more items",
                                            destructive_count - 10
                                        )),
                                )
                            }),
                    )
            })
            .when(!input_label.is_empty(), |content| {
                content
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(input_label),
                    )
                    .child(
                        div()
                            .id("mutation-prompt-input")
                            .debug_selector(|| "mutation-prompt-input".to_string())
                            .child(
                                self.native_text_input_element(TextInputTarget::MutationPrompt)
                                    .unwrap_or_else(|| div().child(placeholder).into_any_element()),
                            ),
                    )
            })
            .when_some(
                archive_options,
                |content, (format_label, compression_label)| {
                    content.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                toolbar_button(
                                    "archive-format",
                                    &format_label,
                                    self.palette.control,
                                )
                                .debug_selector(|| "archive-format".to_string())
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.cycle_archive_format(cx)),
                                ),
                            )
                            .child(
                                toolbar_button(
                                    "archive-compression",
                                    &compression_label,
                                    self.palette.control,
                                )
                                .debug_selector(|| "archive-compression".to_string())
                                .on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.cycle_archive_compression(cx)
                                    }),
                                ),
                            ),
                    )
                },
            )
            .when_some(prompt.error.clone(), |content, error| {
                content.child(
                    div()
                        .id("mutation-prompt-error")
                        .debug_selector(|| "mutation-prompt-error".to_string())
                        .px_3()
                        .py_2()
                        .border_1()
                        .border_color(rgb(0x8b3340))
                        .bg(with_alpha(rgb(0x8b3340), 0.2))
                        .text_xs()
                        .text_color(rgb(0xff8a80))
                        .child(error),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child(prompt.submit_hint()),
            );
        let cancel = toolbar_button("mutation-prompt-cancel", "Cancel", self.palette.control)
            .debug_selector(|| "mutation-prompt-cancel".to_string())
            .on_click(cx.listener(|this, _, _, cx| this.cancel_mutation_prompt(cx)));
        let can_submit = match &prompt.kind {
            MutationPromptKind::PermanentDelete { .. } => prompt.input == "DELETE",
            MutationPromptKind::ArchivePassword { .. }
            | MutationPromptKind::ExtractPassword { .. }
            | MutationPromptKind::Trash { .. } => true,
            MutationPromptKind::NewFolder
            | MutationPromptKind::NewNote
            | MutationPromptKind::NewWebsiteLinkName
            | MutationPromptKind::NewWebsiteLinkUrl { .. }
            | MutationPromptKind::Rename { .. }
            | MutationPromptKind::ArchiveName { .. }
            | MutationPromptKind::ExtractDirectory { .. } => !prompt.input.trim().is_empty(),
        };
        let submit_color = if can_submit {
            if matches!(&prompt.kind, MutationPromptKind::PermanentDelete { .. }) {
                rgb(0x8b3340)
            } else {
                self.palette.accent
            }
        } else {
            self.palette.disabled
        };
        let submit = toolbar_button_enabled(
            "mutation-prompt-submit",
            submit_label,
            submit_color,
            can_submit,
        )
        .debug_selector(|| "mutation-prompt-submit".to_string())
        .when(can_submit, |button| {
            button.on_click(cx.listener(|this, _, _, cx| this.submit_mutation_prompt(cx)))
        });

        let dialog = if is_basic {
            div()
                .id("mutation-prompt-dialog")
                .debug_selector(|| "mutation-prompt-dialog".to_string())
                .role(Role::Dialog)
                .aria_label(title)
                .flex()
                .flex_col()
                .gap_4()
                .w_full()
                .max_w(px(360.0 * self.palette.scale))
                .p_5()
                .border_1()
                .border_color(self.palette.border)
                .bg(self.palette.panel)
                .shadow_lg()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(content)
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .items_center()
                        .gap_2()
                        .child(cancel)
                        .child(submit),
                )
                .into_any_element()
        } else {
            div()
                .id("mutation-prompt-dialog")
                .debug_selector(|| "mutation-prompt-dialog".to_string())
                .role(if is_destructive {
                    Role::AlertDialog
                } else {
                    Role::Dialog
                })
                .aria_label(title)
                .flex()
                .flex_col()
                .w_full()
                .max_w(px(
                    if is_destructive { 540.0 } else { 500.0 } * self.palette.scale
                ))
                .max_h(px((self
                    .layout
                    .last_window_bounds
                    .height
                    .unwrap_or(DEFAULT_WINDOW_HEIGHT)
                    - 24.0 * self.palette.scale)
                    .max(320.0 * self.palette.scale)))
                .border_1()
                .border_color(self.palette.border)
                .bg(self.palette.panel)
                .shadow_lg()
                .child(
                    div()
                        .id("mutation-prompt-header")
                        .debug_selector(|| "mutation-prompt-header".to_string())
                        .flex()
                        .items_center()
                        .gap_3()
                        .min_h(px(54.0))
                        .flex_none()
                        .px_4()
                        .border_b_1()
                        .border_color(self.palette.border)
                        .bg(self.palette.topbar)
                        .child(
                            div()
                                .text_lg()
                                .text_color(if is_destructive {
                                    rgb(0xff8a80)
                                } else {
                                    self.palette.accent
                                })
                                .child(if is_destructive { "⚠" } else { "▣" }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                                .when_some(subtitle, |header, subtitle| {
                                    header.child(
                                        div()
                                            .text_xs()
                                            .text_color(self.palette.muted)
                                            .child(subtitle),
                                    )
                                }),
                        ),
                )
                .child(
                    div()
                        .id("mutation-prompt-scroll")
                        .flex()
                        .flex_col()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_4()
                        .child(content),
                )
                .child(
                    div()
                        .id("mutation-prompt-footer")
                        .debug_selector(|| "mutation-prompt-footer".to_string())
                        .flex()
                        .justify_end()
                        .items_center()
                        .gap_2()
                        .min_h(px(50.0))
                        .flex_none()
                        .px_4()
                        .border_t_1()
                        .border_color(self.palette.border)
                        .bg(self.palette.topbar)
                        .child(cancel)
                        .child(submit),
                )
                .into_any_element()
        };

        div()
            .id("mutation-prompt-backdrop")
            .debug_selector(|| "mutation-prompt-backdrop".to_string())
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_center()
            .p_3()
            .bg(with_alpha(rgb(0x000000), 0.58))
            .child(dialog)
            .into_any_element()
    }
}
