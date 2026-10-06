//! `DirectoryWindow` behavior for smart folders.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn open_smart_folder_editor(&mut self, id: Option<u64>, cx: &mut Context<Self>) {
        let (name, criteria) = if let Some(id) = id {
            let Some(folder) = self
                .browser
                .smart_folders()
                .iter()
                .find(|folder| folder.id() == id)
            else {
                self.show_toast("Smart folder no longer exists", ToastKind::Warning, cx);
                return;
            };
            (folder.name().to_string(), folder.criteria().clone())
        } else {
            let query = self.browser.search_query().trim().to_string();
            let type_filter = match self.browser.filter() {
                EntryFilter::All => SearchType::All,
                EntryFilter::Files => SearchType::Files,
                EntryFilter::Folders => SearchType::Folders,
            };
            let name = if query.is_empty() {
                "New smart folder".to_string()
            } else {
                format!("Search: {query}")
            };
            (
                name,
                SearchCriteria {
                    name_pattern: (!query.is_empty()).then_some(query),
                    type_filter,
                    search_paths: vec![self.browser.path().to_path_buf()],
                    recursive: true,
                    ..SearchCriteria::default()
                },
            )
        };
        let draft = SmartFolderDraft::new(id, name, criteria);
        self.open_control_surface(ControlSurface::SmartFolders, cx);
        if self.overlay.surface != ControlSurface::SmartFolders {
            return;
        }
        self.overlay.query = draft.value(draft.field).to_string();
        self.smart_folder_editor = Some(draft);
        cx.notify();
    }

    pub(crate) fn save_current_search(&mut self, cx: &mut Context<Self>) {
        self.open_smart_folder_editor(None, cx);
    }

    pub(crate) fn select_smart_folder_field(
        &mut self,
        field: SmartFolderField,
        cx: &mut Context<Self>,
    ) {
        let value = {
            let Some(editor) = &mut self.smart_folder_editor else {
                return;
            };
            editor.field = field;
            editor.value(field).to_string()
        };
        self.overlay.query = value;
        self.sync_native_text_input(self.overlay.query.clone(), cx);
        cx.notify();
    }

    pub(crate) fn update_smart_folder_query(&mut self, value: String, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.smart_folder_editor else {
            return;
        };
        editor.set_value(editor.field, value.clone());
        self.overlay.query = value;
        cx.notify();
    }

    pub(crate) fn move_smart_folder_field(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(editor) = &self.smart_folder_editor else {
            return;
        };
        self.select_smart_folder_field(editor.field.offset(delta), cx);
    }

    pub(crate) fn cycle_smart_folder_type(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.smart_folder_editor else {
            return;
        };
        editor.type_filter = match editor.type_filter {
            SearchType::All => SearchType::Files,
            SearchType::Files => SearchType::Folders,
            SearchType::Folders => SearchType::All,
        };
        cx.notify();
    }

    pub(crate) fn commit_smart_folder_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.smart_folder_editor.clone() else {
            return;
        };
        let (name, criteria) = match editor.validate() {
            Ok(validated) => validated,
            Err(error) => {
                self.show_toast(error, ToastKind::Warning, cx);
                return;
            }
        };
        let was_active = editor.id.is_some_and(|id| {
            self.browser
                .active_smart_folder()
                .is_some_and(|folder| folder.id() == id)
        });
        let message = if let Some(id) = editor.id {
            if !self.mutate_shared_session(|session| {
                session.update_smart_folder(id, name.clone(), criteria.clone())
            }) {
                self.show_toast("Smart folder no longer exists", ToastKind::Warning, cx);
                return;
            }
            format!("Smart folder updated: {name}")
        } else {
            self.mutate_shared_session(|session| {
                session.save_smart_folder(name.clone(), criteria.clone())
            });
            format!("Smart folder saved: {name}")
        };
        self.persist_session();
        self.smart_folder_editor = None;
        self.close_control_surface_unchecked(cx);
        self.show_toast(message, ToastKind::Success, cx);
        if was_active {
            self.start_smart_search(criteria, cx);
            self.start_watching(cx);
        }
    }

    pub(crate) fn toggle_smart_folder_regex(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.smart_folder_editor {
            editor.name_regex = !editor.name_regex;
            cx.notify();
        }
    }

    pub(crate) fn toggle_smart_folder_recursive(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.smart_folder_editor {
            editor.recursive = !editor.recursive;
            cx.notify();
        }
    }

    pub(crate) fn toggle_smart_folder_combine_mode(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.smart_folder_editor {
            editor.combine_mode = match editor.combine_mode {
                CombineMode::And => CombineMode::Or,
                CombineMode::Or => CombineMode::And,
            };
            cx.notify();
        }
    }

    pub(crate) fn activate_smart_folder(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.browser.activate_smart_folder(id) {
            return;
        }
        let Some(criteria) = self
            .browser
            .active_smart_folder()
            .map(|folder| folder.criteria().clone())
        else {
            return;
        };
        self.close_preview(cx);
        self.end_subfolder_search(false, cx);
        self.listing.generation = self.listing.generation.wrapping_add(1);
        self.listing.task = None;
        self.column_view.generation = self.column_view.generation.wrapping_add(1);
        self.column_view.tasks.clear();
        apply_smart_folder_browser_state(&mut self.browser, &criteria);
        self.persist_session();
        self.start_smart_search(criteria, cx);
        self.start_watching(cx);
        self.listing
            .scroll_handle
            .scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    }

    pub(crate) fn delete_smart_folder(&mut self, id: u64, cx: &mut Context<Self>) {
        let was_active = self
            .browser
            .active_smart_folder()
            .is_some_and(|folder| folder.id() == id);
        if self.mutate_shared_session(|session| session.delete_smart_folder(id)) {
            self.persist_session();
            if was_active {
                self.close_preview(cx);
                self.apply_global_browser_preferences();
                self.browser.clear_search();
                self.search.active = false;
                self.start_listing(cx);
                self.start_watching(cx);
            }
            cx.notify();
        }
    }

    pub(crate) fn render_smart_folder_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.smart_folder_editor.clone() else {
            return div().into_any_element();
        };
        let active_field = editor.field;
        let palette = self.palette;
        let rows: Vec<AnyElement> = SmartFolderField::ALL
            .into_iter()
            .map(|field| {
                let active = field == active_field;
                let value = editor.value(field);
                let display = if value.is_empty() {
                    "—".to_string()
                } else {
                    value.to_string()
                };
                let edit_label = format!("Edit {}, current value {display}", field.label());
                let input = active
                    .then(|| self.native_text_input_element(TextInputTarget::ControlQuery))
                    .flatten();
                div()
                    .id(field.debug_id())
                    .debug_selector(move || field.debug_id().to_string())
                    .when(!active, |row| {
                        row.role(Role::Button)
                            .aria_label(edit_label)
                            .focusable()
                            .tab_stop(true)
                            .focus(move |row| row.border_color(palette.accent).bg(palette.hover))
                    })
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(if active {
                        self.palette.accent
                    } else {
                        self.palette.border
                    })
                    .bg(if active {
                        self.palette.selected
                    } else {
                        self.palette.surface
                    })
                    .hover(move |row| row.bg(palette.hover))
                    .cursor_pointer()
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(field.label()),
                    )
                    .when(active, |row| row.child(div().children(input)))
                    .when(!active, |row| {
                        row.child(div().truncate().text_sm().child(display))
                    })
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.select_smart_folder_field(field, cx)
                        }),
                    )
                    .into_any_element()
            })
            .collect();
        let split = rows.len().div_ceil(2);
        let mut left = Vec::with_capacity(split);
        let mut right = Vec::with_capacity(rows.len() - split);
        for (index, row) in rows.into_iter().enumerate() {
            if index < split {
                left.push(row);
            } else {
                right.push(row);
            }
        }
        let type_label = match editor.type_filter {
            SearchType::All => "Type: all",
            SearchType::Files => "Type: files",
            SearchType::Folders => "Type: folders",
        };
        let title = if editor.id.is_some() {
            "Edit smart folder"
        } else {
            "Save search as smart folder"
        };
        let save_label = if editor.id.is_some() {
            "Save changes"
        } else {
            "Save"
        };

        div()
            .id("smart-folder-editor")
            .debug_selector(|| "smart-folder-editor".to_string())
            .role(Role::Dialog)
            .aria_label("Smart folder")
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(760.0 * self.palette.scale))
            .max_h(px((520.0 * self.palette.scale).min(
                (self.layout.last_window_bounds.height.unwrap_or(DEFAULT_WINDOW_HEIGHT)
                    - 24.0 * self.palette.scale)
                    .max(240.0 * self.palette.scale),
            )))
            .border_1()
            .border_color(self.palette.border)
            .rounded_lg()
            .bg(self.palette.panel)
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child("Local saved search • all filters are optional except name and root"),
                            ),
                    )
                    .child(
                        toolbar_button("close-smart-folder-editor", "Close", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.close_control_surface(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .flex_wrap()
                    .items_center()
                    .min_h(px(56.0))
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .child(
                        toolbar_button("smart-folder-type", type_label, self.palette.control)
                            .debug_selector(|| "smart-folder-type".to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cycle_smart_folder_type(cx)
                            })),
                    )
                    .child(
                        toolbar_button(
                            "smart-folder-regex",
                            if editor.name_regex {
                                "Pattern: regex"
                            } else {
                                "Pattern: text"
                            },
                            if editor.name_regex {
                                self.palette.selected
                            } else {
                                self.palette.control
                            },
                        )
                        .debug_selector(|| "smart-folder-regex".to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_smart_folder_regex(cx)
                        })),
                    )
                    .child(
                        toolbar_button(
                            "smart-folder-recursive",
                            if editor.recursive {
                                "Scope: recursive"
                            } else {
                                "Scope: this folder"
                            },
                            if editor.recursive {
                                self.palette.selected
                            } else {
                                self.palette.control
                            },
                        )
                        .debug_selector(|| "smart-folder-recursive".to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_smart_folder_recursive(cx)
                        })),
                    )
                    .child(
                        toolbar_button(
                            "smart-folder-combine",
                            match editor.combine_mode {
                                CombineMode::And => "Match: all (AND)",
                                CombineMode::Or => "Match: any (OR)",
                            },
                            self.palette.control,
                        )
                        .debug_selector(|| "smart-folder-combine".to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_smart_folder_combine_mode(cx)
                        })),
                    ),
            )
            .child(
                div()
                    .id("smart-folder-fields")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .p_4()
                    .overflow_y_scroll()
                    .child(div().flex().flex_col().flex_1().min_w_0().gap_2().children(left))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .children(right),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(self.palette.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .whitespace_nowrap()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format!(
                                "{} • Tab moves fields • {} saves",
                                active_field.hint(),
                                display_binding("secondary-enter"),
                            )),
                    )
                    .child(
                        toolbar_button("cancel-smart-folder", "Cancel", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.close_control_surface(cx))),
                    )
                    .child(
                        toolbar_button("save-smart-folder", save_label, self.palette.accent)
                            .debug_selector(|| "save-smart-folder".to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.commit_smart_folder_editor(cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}
