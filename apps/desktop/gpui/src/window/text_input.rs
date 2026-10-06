//! `DirectoryWindow` behavior for text input.

use crate::*;

/// The native text field shared by the window's editors: the input entity,
/// which editor it currently edits, and whether it should take focus on the
/// next render.
#[derive(Default)]
pub(crate) struct NativeTextInputState {
    pub(crate) entity: Option<Entity<NativeTextInput>>,
    pub(crate) target: Option<TextInputTarget>,
    pub(crate) focus_pending: bool,
    /// The current input's focus handle.
    pub(crate) focus: Option<FocusHandle>,
    /// The focus handle of a removed input: if it still held the keyboard
    /// focus, the next render hands the focus back to the file list instead
    /// of leaving it nowhere.
    pub(crate) removed_focus: Option<FocusHandle>,
}

impl DirectoryWindow {
    pub(crate) fn activate_native_text_input(
        &mut self,
        target: TextInputTarget,
        content: impl Into<String>,
        placeholder: &'static str,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        let colors = [
            self.palette.surface,
            self.palette.border,
            self.palette.text,
            self.palette.tertiary,
            self.palette.accent,
        ];
        let scale = self.settings.appearance.ui_scale;
        let appearance = NativeTextInputAppearance { colors, scale };
        let input = cx.new(|cx| NativeTextInput::new(content, placeholder, label, appearance, cx));
        cx.subscribe(&input, |this, _, event, cx| match event {
            NativeTextInputEvent::Changed(value) => {
                this.apply_native_text_input_change(value.clone(), cx)
            }
        })
        .detach();
        self.text_input.focus = Some(input.focus_handle(cx));
        self.text_input.entity = Some(input);
        self.text_input.target = Some(target);
        self.text_input.focus_pending = true;
        cx.notify();
    }

    pub(crate) fn deactivate_native_text_input(&mut self) {
        if self.text_input.entity.take().is_some() {
            self.text_input.removed_focus = self.text_input.focus.take();
        }
        self.text_input.target = None;
        self.text_input.focus_pending = false;
    }

    pub(crate) fn sync_native_text_input(
        &mut self,
        value: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.text_input.entity.as_ref() else {
            return;
        };
        let value = value.into();
        if input.read(cx).content() != value {
            input.update(cx, |input, cx| input.set_content(value, cx));
        }
    }

    pub(crate) fn apply_native_text_input_change(&mut self, value: String, cx: &mut Context<Self>) {
        match self.text_input.target {
            Some(TextInputTarget::PluginSetting) => {
                if let Some((_, _, input)) = self.plugin_ui.text_editor.as_mut() {
                    *input = value;
                }
            }
            Some(TextInputTarget::Search) => {
                self.browser.set_search_query(value);
                self.search.active = true;
                self.search_query_did_change(cx);
            }
            Some(TextInputTarget::ControlQuery) => match self.overlay.surface {
                ControlSurface::SmartFolders => self.update_smart_folder_query(value, cx),
                ControlSurface::BatchRename => {
                    self.overlay.query.clone_from(&value);
                    if let Some(editor) = self.batch_rename.as_mut() {
                        editor.set_active_value(value);
                    }
                }
                _ => {
                    self.overlay.query = value;
                    self.overlay.selected = 0;
                }
            },
            Some(TextInputTarget::GoToFolder) => {
                if let Some(dialog) = self.navigation_ui.go_to_folder.as_mut() {
                    dialog.input = value;
                    dialog.replace_on_type = false;
                    dialog.error = None;
                }
                self.schedule_go_to_folder_suggestions(cx);
            }
            Some(TextInputTarget::Breadcrumb) => {
                if let Some(editor) = self.navigation_ui.breadcrumb_editor.as_mut() {
                    editor.input = value;
                    editor.replace_on_type = false;
                }
            }
            Some(TextInputTarget::MutationPrompt) => {
                if let Some(prompt) = self.mutation.prompt.as_mut() {
                    prompt.input = value;
                    prompt.replace_on_type = false;
                    prompt.error = None;
                }
            }
            Some(TextInputTarget::AppearanceValue) => {
                if let Some(editor) = self.settings_ui.appearance_value_editor.as_mut() {
                    editor.input = value;
                    editor.error = None;
                }
            }
            Some(TextInputTarget::NamedTheme) => {
                if let Some(editor) = self.settings_ui.named_theme_editor.as_mut() {
                    editor.input = value;
                    editor.error = None;
                }
            }
            Some(TextInputTarget::PreviewFind) => {
                self.preview.text.find = value;
                self.preview.text.find_match = 0;
                self.scroll_to_current_text_preview_match();
            }
            Some(TextInputTarget::CustomField) => {
                if let Some(editor) = self.preview.custom_fields_editor.as_mut() {
                    editor.set_active_value(value);
                }
            }
            Some(TextInputTarget::FinderTag) => {
                if let Some(editor) = self.preview.finder_tags.editor.as_mut() {
                    editor.input = value.chars().take(80).collect();
                    editor.error = None;
                }
            }
            None => return,
        }
        cx.notify();
    }

    pub(crate) fn native_text_input_element(&self, target: TextInputTarget) -> Option<AnyElement> {
        (self.text_input.target == Some(target))
            .then(|| self.text_input.entity.clone())
            .flatten()
            .map(IntoElement::into_any_element)
    }

    pub(crate) fn ensure_native_text_input(&mut self, cx: &mut Context<Self>) {
        let control_input_needed = match self.overlay.surface {
            ControlSurface::CommandPalette
            | ControlSurface::Shortcuts
            | ControlSurface::Workspaces => true,
            ControlSurface::SmartFolders => self.smart_folder_editor.is_some(),
            ControlSurface::RemoteDrives => self.remote.editor.is_some(),
            ControlSurface::BatchRename => self.batch_rename.as_ref().is_some_and(|editor| {
                !matches!(editor.mode, BatchRenameMode::Case | BatchRenameMode::Number)
            }),
            ControlSurface::Closed | ControlSurface::Diagnostics => false,
        };
        // Like Finder, renaming a file starts with its name selected but not
        // its extension; a folder's whole name is selected.
        let mut initial_selection = None;
        let desired = if let Some(editor) = self.settings_ui.appearance_value_editor.as_ref() {
            Some((
                TextInputTarget::AppearanceValue,
                editor.input.clone(),
                editor.hint(),
                editor.title(),
                false,
                false,
            ))
        } else if let Some(editor) = self.settings_ui.named_theme_editor.as_ref() {
            Some((
                TextInputTarget::NamedTheme,
                editor.input.clone(),
                "Theme name",
                "Theme name",
                false,
                false,
            ))
        } else if let Some(dialog) = self.navigation_ui.go_to_folder.as_ref() {
            Some((
                TextInputTarget::GoToFolder,
                dialog.input.clone(),
                "Enter folder path…",
                "Folder path",
                false,
                dialog.replace_on_type,
            ))
        } else if let Some(editor) = self.navigation_ui.breadcrumb_editor.as_ref() {
            Some((
                TextInputTarget::Breadcrumb,
                editor.input.clone(),
                "Enter folder path…",
                "Folder path",
                false,
                editor.replace_on_type,
            ))
        } else if let Some(prompt) = self
            .mutation
            .prompt
            .as_ref()
            .filter(|prompt| !matches!(prompt.kind, MutationPromptKind::Trash { .. }))
        {
            let masked = matches!(
                prompt.kind,
                MutationPromptKind::ArchivePassword { .. }
                    | MutationPromptKind::ExtractPassword { .. }
            );
            if let MutationPromptKind::Rename { source } = &prompt.kind
                && prompt.replace_on_type
                && !self.renames_whole_name(source)
            {
                initial_selection = Some(0..name_stem_len(&prompt.input));
            }
            Some((
                TextInputTarget::MutationPrompt,
                prompt.input.clone(),
                if masked { "No password" } else { "Type here" },
                prompt.title(),
                masked,
                prompt.replace_on_type,
            ))
        } else if let Some(value) = self
            .preview
            .custom_fields_editor
            .as_ref()
            .and_then(|editor| {
                editor
                    .draft
                    .as_ref()
                    .map(|_| editor.active_value().to_string())
            })
        {
            Some((
                TextInputTarget::CustomField,
                value,
                "Type a value…",
                "Custom field value",
                false,
                false,
            ))
        } else if let Some(editor) = self.preview.finder_tags.editor.as_ref() {
            Some((
                TextInputTarget::FinderTag,
                editor.input.clone(),
                "Tag name…",
                "Finder tag name",
                false,
                false,
            ))
        } else if self.search.active {
            let (placeholder, label) = match self.search.scope {
                SearchScope::ThisFolder => ("Search current folder…", "Search current folder"),
                SearchScope::Subfolders => (
                    "Search folder and subfolders…",
                    "Search current folder and subfolders",
                ),
            };
            Some((
                TextInputTarget::Search,
                self.browser.search_query().to_string(),
                placeholder,
                label,
                false,
                false,
            ))
        } else if control_input_needed {
            let platform = self
                .remote
                .environment
                .as_ref()
                .map(|environment| environment.platform.as_str());
            let (placeholder, label) = match (self.overlay.surface, self.remote.editor.as_ref()) {
                (ControlSurface::RemoteDrives, Some(editor)) => (
                    editor.field.placeholder(platform),
                    editor.field.label(platform),
                ),
                _ => ("Type to filter…", "Filter or edit value"),
            };
            Some((
                TextInputTarget::ControlQuery,
                self.overlay.query.clone(),
                placeholder,
                label,
                false,
                false,
            ))
        } else {
            None
        };

        if let Some((target, content, placeholder, label, masked, replace_on_type)) = desired {
            let target_changed = self.text_input.target != Some(target);
            if target_changed {
                self.activate_native_text_input(target, content.clone(), placeholder, label, cx);
            }
            if let Some(input) = self.text_input.entity.as_ref() {
                let colors = [
                    self.palette.surface,
                    self.palette.border,
                    self.palette.text,
                    self.palette.tertiary,
                    self.palette.accent,
                ];
                let scale = self.settings.appearance.ui_scale;
                input.update(cx, |input, cx| {
                    input.reconfigure(
                        content,
                        placeholder,
                        label,
                        masked,
                        NativeTextInputAppearance { colors, scale },
                        cx,
                    );
                    if target_changed && let Some(range) = initial_selection {
                        input.select_content_range(range, cx);
                    } else if target_changed && replace_on_type {
                        input.select_all_content(cx);
                    }
                });
            }
        } else if self.text_input.target != Some(TextInputTarget::PreviewFind) {
            self.deactivate_native_text_input();
        }
    }
}

impl DirectoryWindow {
    /// Whether renaming `source` starts with its whole name selected: folders
    /// (not packages, whose extension Finder leaves out too).
    fn renames_whole_name(&self, source: &Path) -> bool {
        self.browser
            .visible_entries()
            .iter()
            .find(|entry| entry.path == source)
            .map_or_else(
                || source.is_dir(),
                |entry| is_directory_entry(entry) && !entry.is_package,
            )
    }
}

/// The length of `name` without its extension: up to the last dot, unless
/// the only dot starts the name (".profile") or ends it.
fn name_stem_len(name: &str) -> usize {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => dot,
        _ => name.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::name_stem_len;

    #[test]
    fn rename_selection_leaves_out_only_the_last_extension() {
        assert_eq!(name_stem_len("report.pdf"), 6);
        assert_eq!(name_stem_len("archive.tar.gz"), 11);
        assert_eq!(name_stem_len(".profile"), 8);
        assert_eq!(name_stem_len("README"), 6);
        assert_eq!(name_stem_len("trailing."), 9);
        assert_eq!(name_stem_len("café.txt"), "café".len());
    }
}
