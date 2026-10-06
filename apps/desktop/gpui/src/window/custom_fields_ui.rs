//! `DirectoryWindow` behavior for custom fields ui.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn begin_add_custom_field(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.preview.custom_fields_editor
            && !editor.pending
        {
            editor.begin_add();
            cx.notify();
        }
    }

    pub(crate) fn begin_edit_custom_field(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.preview.custom_fields_editor
            && !editor.pending
            && editor.begin_edit(name)
        {
            cx.notify();
        }
    }

    pub(crate) fn cancel_custom_field_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.preview.custom_fields_editor {
            editor.cancel();
            cx.notify();
        }
    }

    pub(crate) fn toggle_custom_field_input(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.preview.custom_fields_editor {
            editor.toggle_input();
            let value = editor.active_value().to_string();
            self.sync_native_text_input(value, cx);
            cx.notify();
        }
    }

    pub(crate) fn select_custom_field_input(
        &mut self,
        input: CustomFieldInput,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &mut self.preview.custom_fields_editor {
            editor.set_active_input(input);
            let value = editor.active_value().to_string();
            self.sync_native_text_input(value, cx);
            self.text_input.focus_pending = true;
            cx.notify();
        }
    }

    pub(crate) fn save_custom_field(&mut self, cx: &mut Context<Self>) {
        let fields = match self
            .preview
            .custom_fields_editor
            .as_ref()
            .map(CustomFieldsEditor::proposed_fields)
        {
            Some(Ok(fields)) => fields,
            Some(Err(error)) => {
                self.show_toast(error, ToastKind::Warning, cx);
                return;
            }
            None => return,
        };
        self.persist_custom_fields(fields, "Custom fields saved".to_string(), cx);
    }

    pub(crate) fn remove_custom_field(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(editor) = &self.preview.custom_fields_editor else {
            return;
        };
        if editor.pending || !editor.fields.contains_key(name) {
            return;
        }
        self.persist_custom_fields(
            editor.fields_without(name),
            format!("Removed custom field {name}"),
            cx,
        );
    }

    pub(crate) fn request_remove_custom_field(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(editor) = &self.preview.custom_fields_editor else {
            return;
        };
        if editor.pending || !editor.fields.contains_key(name) {
            return;
        }
        self.request_settings_confirmation(
            SettingsConfirmation::RemoveCustomField(name.to_string()),
            cx,
        );
    }

    pub(crate) fn persist_custom_fields(
        &mut self,
        fields: HashMap<String, serde_json::Value>,
        success_message: String,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &mut self.preview.custom_fields_editor else {
            return;
        };
        if editor.pending {
            self.show_toast(
                "Wait for the current metadata save to finish",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        let path = editor.path.clone();
        let Some(parent) = path.parent().map(Path::to_path_buf) else {
            self.show_toast(
                "Cannot save metadata for a filesystem root",
                ToastKind::Warning,
                cx,
            );
            return;
        };
        let Some(file_name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            return;
        };
        editor.pending = true;
        let task = self
            .services
            .metadata
            .update_fields(parent, file_name, fields.clone());
        self.push_mutation_task(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                let matches = view
                    .preview
                    .custom_fields_editor
                    .as_ref()
                    .is_some_and(|editor| editor.path == path);
                if matches {
                    match result {
                        Ok(()) => {
                            if let Some(editor) = &mut view.preview.custom_fields_editor {
                                editor.apply_saved(fields);
                            }
                            view.show_toast(success_message, ToastKind::Success, cx);
                            view.refresh(cx);
                        }
                        Err(error) => {
                            if let Some(editor) = &mut view.preview.custom_fields_editor {
                                editor.pending = false;
                            }
                            view.record_error("Custom fields save failed", error.to_string());
                            view.show_toast(
                                format!("Unable to save custom fields: {error}"),
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

    pub(crate) fn handle_custom_field_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.cancel_custom_field_edit(cx),
            "tab" => self.toggle_custom_field_input(cx),
            "enter" => self.save_custom_field(cx),
            "backspace" => {
                if let Some(editor) = &mut self.preview.custom_fields_editor {
                    let mut value = editor.active_value().to_string();
                    value.pop();
                    editor.set_active_value(value);
                    cx.notify();
                }
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref()
                    && !text.chars().any(char::is_control)
                    && let Some(editor) = &mut self.preview.custom_fields_editor
                {
                    let mut value = editor.active_value().to_string();
                    value.push_str(text);
                    editor.set_active_value(value);
                    cx.notify();
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn render_custom_fields(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.preview.custom_fields_editor.clone() else {
            return div()
                .p_4()
                .text_sm()
                .text_color(self.palette.muted)
                .child("Custom fields are unavailable for this item")
                .into_any_element();
        };
        let rows: Vec<AnyElement> = editor
            .fields
            .iter()
            .map(|(name, value)| {
                let edit_name = name.clone();
                let remove_name = name.clone();
                div()
                    .id(ElementId::Name(format!("custom-field-row-{name}").into()))
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .border_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.panel)
                    .child(
                        div()
                            .w(px(120.0))
                            .min_w(px(120.0))
                            .text_sm()
                            .text_color(self.palette.text)
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(self.palette.muted)
                            .child(display_value(value)),
                    )
                    .child(
                        toolbar_button(
                            ElementId::Name(format!("edit-custom-field-{name}").into()),
                            "Edit",
                            self.palette.control,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.begin_edit_custom_field(&edit_name, cx)
                        })),
                    )
                    .child(
                        toolbar_button(
                            ElementId::Name(format!("remove-custom-field-{name}").into()),
                            "Remove",
                            self.palette.control,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.request_remove_custom_field(&remove_name, cx)
                        })),
                    )
                    .into_any_element()
            })
            .collect();
        let has_draft = editor.draft.is_some();
        let draft = editor.draft.clone().map(|draft| {
            let name_active = draft.active == CustomFieldInput::Name;
            let value_active = draft.active == CustomFieldInput::Value;
            let suggestions: Vec<AnyElement> = FIELD_SUGGESTIONS
                .iter()
                .filter(|suggestion| {
                    draft.name.is_empty()
                        || suggestion
                            .to_ascii_lowercase()
                            .contains(&draft.name.to_ascii_lowercase())
                })
                .map(|suggestion| {
                    let suggestion = (*suggestion).to_string();
                    let label = suggestion.clone();
                    toolbar_button(
                        ElementId::Name(format!("custom-field-suggestion-{suggestion}").into()),
                        &label,
                        self.palette.control,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(editor) = &mut this.preview.custom_fields_editor
                            && let Some(draft) = &mut editor.draft
                        {
                            draft.name = suggestion.clone();
                            draft.active = CustomFieldInput::Value;
                            cx.notify();
                        }
                    }))
                    .into_any_element()
                })
                .collect();
            div()
                .id("custom-field-draft")
                .flex()
                .flex_col()
                .gap_2()
                .pt_3()
                .border_t_1()
                .border_color(self.palette.border)
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .id("custom-field-name-input")
                                .flex_1()
                                .text_sm()
                                .cursor_text()
                                .when(!name_active, |input| {
                                    input
                                        .role(Role::Button)
                                        .aria_label("Edit custom field name")
                                        .focusable()
                                        .tab_stop(true)
                                        .h(px(34.0))
                                        .px_2()
                                        .border_1()
                                        .border_color(self.palette.border)
                                        .bg(self.palette.window)
                                        .focus(|input| input.border_color(self.palette.accent))
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.select_custom_field_input(CustomFieldInput::Name, cx)
                                }))
                                .child(if name_active {
                                    self.native_text_input_element(TextInputTarget::CustomField)
                                        .unwrap_or_else(|| {
                                            div().child(draft.name.clone()).into_any_element()
                                        })
                                } else {
                                    div()
                                        .flex()
                                        .h_full()
                                        .items_center()
                                        .child(if draft.name.is_empty() {
                                            "Field name…".to_string()
                                        } else {
                                            draft.name.clone()
                                        })
                                        .into_any_element()
                                }),
                        )
                        .child(
                            div()
                                .id("custom-field-value-input")
                                .flex_1()
                                .text_sm()
                                .cursor_text()
                                .when(!value_active, |input| {
                                    input
                                        .role(Role::Button)
                                        .aria_label("Edit custom field value")
                                        .focusable()
                                        .tab_stop(true)
                                        .h(px(34.0))
                                        .px_2()
                                        .border_1()
                                        .border_color(self.palette.border)
                                        .bg(self.palette.window)
                                        .focus(|input| input.border_color(self.palette.accent))
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.select_custom_field_input(CustomFieldInput::Value, cx)
                                }))
                                .child(if value_active {
                                    self.native_text_input_element(TextInputTarget::CustomField)
                                        .unwrap_or_else(|| {
                                            div().child(draft.value.clone()).into_any_element()
                                        })
                                } else {
                                    div()
                                        .flex()
                                        .h_full()
                                        .items_center()
                                        .child(if draft.value.is_empty() {
                                            "Value…".to_string()
                                        } else {
                                            draft.value.clone()
                                        })
                                        .into_any_element()
                                }),
                        ),
                )
                .when(name_active && !suggestions.is_empty(), |form| {
                    form.child(div().flex().flex_wrap().gap_1().children(suggestions))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .whitespace_nowrap()
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child("Tab switches fields • Enter saves • Esc cancels"),
                        )
                        .child(
                            toolbar_button_enabled(
                                "cancel-custom-field",
                                "Cancel",
                                self.palette.control,
                                !editor.pending,
                            )
                            .when(!editor.pending, |button| {
                                button.on_click(
                                    cx.listener(|this, _, _, cx| this.cancel_custom_field_edit(cx)),
                                )
                            }),
                        )
                        .child(
                            toolbar_button_enabled(
                                "save-custom-field",
                                if editor.pending { "Saving…" } else { "Save" },
                                if editor.pending {
                                    self.palette.disabled
                                } else {
                                    self.palette.accent
                                },
                                !editor.pending,
                            )
                            .when(!editor.pending, |button| {
                                button.on_click(
                                    cx.listener(|this, _, _, cx| this.save_custom_field(cx)),
                                )
                            }),
                        ),
                )
                .into_any_element()
        });
        div()
            .id("custom-fields-editor")
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(380.0))
            .overflow_y_scroll()
            .p_3()
            .border_t_1()
            .border_color(self.palette.border)
            .bg(self.palette.window)
            .child(
                div()
                    .flex()
                    .items_center()
                    .pb_2()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .child(div().flex_1().text_sm().child("Custom Fields"))
                    .when(!has_draft, |header| {
                        header.child(
                            toolbar_button("add-custom-field", "Add field", self.palette.control)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.begin_add_custom_field(cx)),
                                ),
                        )
                    }),
            )
            .when(rows.is_empty(), |panel| {
                panel.child(
                    div()
                        .py_3()
                        .text_sm()
                        .text_color(self.palette.muted)
                        .child("No custom fields yet"),
                )
            })
            .children(rows)
            .when_some(draft, |panel, draft| panel.child(draft))
            .into_any_element()
    }
}
