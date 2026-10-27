//! `DirectoryWindow` behavior for batch rename ui.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn open_batch_rename(&mut self, cx: &mut Context<Self>) {
        let entries = self.effective_selected_entries();
        self.open_batch_rename_entries(entries, cx);
    }

    pub(crate) fn open_batch_rename_entries(
        &mut self,
        entries: Vec<FileEntry>,
        cx: &mut Context<Self>,
    ) {
        if entries.len() < 2 {
            self.show_toast(
                "Select at least two items for batch rename",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        if self.mutation.in_progress || self.operations.active_count() > 0 {
            self.show_toast(
                "Wait for the current filesystem operation to finish",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        self.open_control_surface(ControlSurface::BatchRename, cx);
        if self.overlay.surface != ControlSurface::BatchRename {
            return;
        }
        self.batch_rename = Some(BatchRenameEditor::new(&entries));
    }

    pub(crate) fn sync_batch_rename_query(&mut self) {
        if let Some(editor) = &mut self.batch_rename {
            editor.set_active_value(self.overlay.query.clone());
        }
    }

    pub(crate) fn set_batch_rename_mode(&mut self, mode: BatchRenameMode, cx: &mut Context<Self>) {
        self.sync_batch_rename_query();
        if let Some(editor) = &mut self.batch_rename {
            editor.set_mode(mode);
            self.overlay.query = editor.active_value().to_string();
        }
        self.sync_native_text_input(self.overlay.query.clone(), cx);
        cx.notify();
    }

    pub(crate) fn toggle_batch_rename_field(&mut self, cx: &mut Context<Self>) {
        self.sync_batch_rename_query();
        if let Some(editor) = &mut self.batch_rename
            && !matches!(editor.mode, BatchRenameMode::Case | BatchRenameMode::Number)
        {
            editor.toggle_field();
            self.overlay.query = editor.active_value().to_string();
        }
        self.sync_native_text_input(self.overlay.query.clone(), cx);
        cx.notify();
    }

    pub(crate) fn cycle_batch_rename_position(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.batch_rename {
            editor.position = editor.position.next();
            cx.notify();
        }
    }

    pub(crate) fn cycle_batch_rename_case(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.batch_rename {
            editor.case_mode = editor.case_mode.next();
            cx.notify();
        }
    }

    pub(crate) fn cycle_batch_rename_case_target(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.batch_rename {
            editor.case_target = editor.case_target.next();
            cx.notify();
        }
    }

    pub(crate) fn cycle_batch_rename_date_source(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.batch_rename {
            editor.date_source = editor.date_source.next();
            cx.notify();
        }
    }

    pub(crate) fn adjust_batch_rename_number(
        &mut self,
        start_delta: i32,
        digit_delta: i8,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &mut self.batch_rename {
            editor.number_start = editor
                .number_start
                .saturating_add_signed(start_delta)
                .max(1);
            editor.number_digits = editor
                .number_digits
                .saturating_add_signed(digit_delta)
                .clamp(1, 9);
            cx.notify();
        }
    }

    pub(crate) fn apply_batch_rename(&mut self, cx: &mut Context<Self>) {
        if self.mutation.in_progress || self.operations.active_count() > 0 {
            self.show_toast(
                "Wait for the current filesystem operation to finish",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        self.sync_batch_rename_query();
        let Some(editor) = self.batch_rename.clone() else {
            return;
        };
        let request = match editor.request() {
            Ok(request) => request,
            Err(error) => {
                self.show_toast(error, ToastKind::Warning, cx);
                return;
            }
        };
        let count = request.len();
        let task = self.services.mutations.batch_rename(request);
        self.mutation.in_progress = true;
        self.status_message = Some(format!("Renaming {count} items…"));
        self.push_mutation_task(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.mutation.in_progress = false;
                match result {
                    Ok(result) => {
                        let pairs = result
                            .renamed
                            .into_iter()
                            .map(|pair| (pair.before, pair.after))
                            .collect();
                        view.undo_ledger.push(UndoRecord::batch_renamed(pairs));
                        view.batch_rename = None;
                        view.overlay.surface = ControlSurface::Closed;
                        view.overlay.query.clear();
                        view.browser.clear_selection();
                        view.refresh(cx);
                        view.status_message =
                            Some(format!("Renamed {count} items atomically • Undo available"));
                    }
                    Err(error) => {
                        view.record_error("Batch rename failed", error.to_string());
                        view.status_message = Some(format!("Batch rename failed: {error}"));
                        view.show_toast(error.to_string(), ToastKind::Warning, cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn render_batch_rename(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.sync_batch_rename_query();
        let Some(editor) = self.batch_rename.clone() else {
            return div().into_any_element();
        };
        let preview = editor.preview();
        let request_error = editor.request().err();
        let changed = preview.iter().filter(|item| item.changed).count();
        let preview_count = preview.len().min(100);
        let rows: Vec<AnyElement> = preview
            .into_iter()
            .take(preview_count)
            .enumerate()
            .map(|(index, item)| {
                let invalid = item.conflict || item.invalid_reason.is_some();
                let detail = item.invalid_reason.unwrap_or_else(|| {
                    if item.conflict {
                        "duplicate target".to_string()
                    } else if item.changed {
                        "will rename".to_string()
                    } else {
                        "unchanged".to_string()
                    }
                });
                div()
                    .id(("batch-rename-preview", index))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.surface)
                    .child(
                        div()
                            .w(px(210.0))
                            .truncate()
                            .text_sm()
                            .child(item.original_name),
                    )
                    .child(div().text_color(self.palette.muted).child("→"))
                    .child(div().flex_1().truncate().text_sm().child(item.new_name))
                    .child(
                        div()
                            .w(px(150.0))
                            .truncate()
                            .text_xs()
                            .text_color(if invalid {
                                rgb(0xff8a80)
                            } else {
                                self.palette.muted
                            })
                            .child(detail),
                    )
                    .into_any_element()
            })
            .collect();

        let modes: Vec<AnyElement> = BatchRenameMode::ALL
            .into_iter()
            .enumerate()
            .map(|(index, mode)| {
                toolbar_button(
                    ("batch-rename-mode", index),
                    mode.label(),
                    if editor.mode == mode {
                        self.palette.accent
                    } else {
                        self.palette.control
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_batch_rename_mode(mode, cx)))
                .into_any_element()
            })
            .collect();

        let mut options = div().flex().flex_wrap().items_center().gap_2();
        match editor.mode {
            BatchRenameMode::Replace => {
                let label = if editor.replace_all {
                    "Replace: all matches"
                } else {
                    "Replace: first match"
                };
                options = options.child(
                    toolbar_button("batch-replace-all", label, self.palette.control).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(editor) = &mut this.batch_rename {
                                editor.replace_all = !editor.replace_all;
                                cx.notify();
                            }
                        }),
                    ),
                );
            }
            BatchRenameMode::Regex => {
                let label = if editor.regex_case_insensitive {
                    "Regex: ignore case"
                } else {
                    "Regex: match case"
                };
                options = options
                    .child(
                        toolbar_button("batch-regex-case", label, self.palette.control).on_click(
                            cx.listener(|this, _, _, cx| {
                                if let Some(editor) = &mut this.batch_rename {
                                    editor.regex_case_insensitive = !editor.regex_case_insensitive;
                                    cx.notify();
                                }
                            }),
                        ),
                    )
                    .child(
                        toolbar_button(
                            "batch-regex-all",
                            if editor.replace_all {
                                "Regex: all matches"
                            } else {
                                "Regex: first match"
                            },
                            self.palette.control,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(editor) = &mut this.batch_rename {
                                editor.replace_all = !editor.replace_all;
                                cx.notify();
                            }
                        })),
                    )
                    .child(
                        toolbar_button(
                            "batch-regex-multiline",
                            if editor.regex_multiline {
                                "Regex: multiline"
                            } else {
                                "Regex: single line"
                            },
                            self.palette.control,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(editor) = &mut this.batch_rename {
                                editor.regex_multiline = !editor.regex_multiline;
                                cx.notify();
                            }
                        })),
                    );
            }
            BatchRenameMode::Number => {
                options = options
                    .child(
                        toolbar_button("batch-number-start-down", "Start −", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.adjust_batch_rename_number(-1, 0, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .child(format!("Start: {}", editor.number_start)),
                    )
                    .child(
                        toolbar_button("batch-number-start-up", "Start +", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.adjust_batch_rename_number(1, 0, cx)
                            })),
                    )
                    .child(
                        toolbar_button(
                            "batch-number-digits-down",
                            "Digits −",
                            self.palette.control,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| {
                                this.adjust_batch_rename_number(0, -1, cx)
                            }),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .child(format!("Digits: {}", editor.number_digits)),
                    )
                    .child(
                        toolbar_button("batch-number-digits-up", "Digits +", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.adjust_batch_rename_number(0, 1, cx)
                            })),
                    )
                    .child(
                        toolbar_button(
                            "batch-number-position",
                            &format!("Position: {}", editor.position.label()),
                            self.palette.control,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.cycle_batch_rename_position(cx)),
                        ),
                    );
            }
            BatchRenameMode::Case => {
                options = options
                    .child(
                        toolbar_button(
                            "batch-case-mode",
                            &format!("Case: {}", editor.case_mode.label()),
                            self.palette.control,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.cycle_batch_rename_case(cx))),
                    )
                    .child(
                        toolbar_button(
                            "batch-case-target",
                            &format!("Apply to: {}", editor.case_target.label()),
                            self.palette.control,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.cycle_batch_rename_case_target(cx)),
                        ),
                    );
            }
            BatchRenameMode::PrefixSuffix => {}
            BatchRenameMode::DateTime => {
                options = options
                    .child(
                        toolbar_button(
                            "batch-date-position",
                            &format!("Position: {}", editor.position.label()),
                            self.palette.control,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.cycle_batch_rename_position(cx)),
                        ),
                    )
                    .child(
                        toolbar_button(
                            "batch-date-source",
                            &format!("Source: {}", editor.date_source.label()),
                            self.palette.control,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.cycle_batch_rename_date_source(cx)),
                        ),
                    );
            }
        }

        let supports_second_field = matches!(
            editor.mode,
            BatchRenameMode::Replace
                | BatchRenameMode::Regex
                | BatchRenameMode::PrefixSuffix
                | BatchRenameMode::DateTime
        );
        let control_input = self.native_text_input_element(TextInputTarget::ControlQuery);
        div()
            .id("batch-rename-manager")
            .debug_selector(|| "batch-rename-manager".to_string())
            .role(Role::Dialog)
            .aria_label("Batch rename")
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(600.0 * self.palette.scale))
            .max_h(px(590.0))
            .border_1()
            .border_color(self.palette.border)
            .bg(self.palette.panel)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .child(div().text_lg().child("Batch rename"))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format!(
                                "{} selected • {changed} changing",
                                editor.sources.len()
                            )),
                    )
                    .child(
                        toolbar_button("close-batch-rename", "Close", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.close_control_surface(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .children(modes),
            )
            .when(editor.mode != BatchRenameMode::Case, |panel| {
                panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .pb_3()
                        .child(div().w(px(130.0)).text_xs().child(editor.field_label()))
                        .child(div().flex_1().children(control_input))
                        .when(supports_second_field, |row| {
                            row.child(
                                toolbar_button(
                                    "batch-rename-next-field",
                                    "Switch field",
                                    self.palette.control,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.toggle_batch_rename_field(cx)
                                    }),
                                ),
                            )
                        }),
                )
            })
            .child(div().px_4().pb_3().child(options))
            .child(
                div()
                    .id("batch-rename-preview-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .when(editor.sources.len() > preview_count, |panel| {
                panel.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_xs()
                        .text_color(self.palette.muted)
                        .child(format!(
                            "Showing the first {preview_count} of {} items",
                            editor.sources.len()
                        )),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(self.palette.border)
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(if request_error.is_some() {
                                rgb(0xff8a80)
                            } else {
                                self.palette.muted
                            })
                            .child(request_error.unwrap_or_else(|| {
                                "Names are validated before one atomic native transaction"
                                    .to_string()
                            })),
                    )
                    .child(
                        toolbar_button(
                            "apply-batch-rename",
                            &format!("Rename {changed} items"),
                            if changed >= 1 {
                                self.palette.accent
                            } else {
                                self.palette.disabled
                            },
                        )
                        .when(changed >= 1, |button| {
                            button
                                .on_click(cx.listener(|this, _, _, cx| this.apply_batch_rename(cx)))
                        }),
                    ),
            )
            .into_any_element()
    }
}
