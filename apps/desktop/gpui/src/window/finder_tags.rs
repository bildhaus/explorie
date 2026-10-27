//! `DirectoryWindow` behavior for finder tags.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn start_finder_tags(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.preview.finder_tags_generation = self.preview.finder_tags_generation.wrapping_add(1);
        let generation = self.preview.finder_tags_generation;
        if !self.services.integration.finder_tags_supported() {
            self.preview.finder_tags.unavailable();
            self.preview.finder_tags_task = None;
            return;
        }
        self.preview.finder_tags.start_loading(path.clone());
        let task = self
            .services
            .integration
            .finder_tags(path.clone())
            .cancel_on_drop();
        self.preview.finder_tags_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if view.preview.finder_tags_generation != generation
                    || view.preview.finder_tags.path.as_deref() != Some(path.as_path())
                {
                    return;
                }
                match result {
                    Ok(tags) => view.preview.finder_tags.set_tags(path, tags),
                    Err(error) => {
                        view.record_error("Finder tags load failed", error.to_string());
                        view.preview.finder_tags.loading = false;
                        view.preview.finder_tags.error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn retry_finder_tags(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.finder_tags.path.clone() else {
            return;
        };
        self.start_finder_tags(path, cx);
        cx.notify();
    }

    pub(crate) fn begin_add_finder_tag(&mut self, cx: &mut Context<Self>) {
        self.search.active = false;
        self.preview.finder_tags.begin_add();
        cx.notify();
    }

    pub(crate) fn cancel_add_finder_tag(&mut self, cx: &mut Context<Self>) {
        self.preview.finder_tags.cancel_add();
        cx.notify();
    }

    pub(crate) fn cycle_finder_tag_color(&mut self, offset: i8, cx: &mut Context<Self>) {
        self.preview.finder_tags.cycle_color(offset);
        cx.notify();
    }

    pub(crate) fn submit_finder_tag(&mut self, cx: &mut Context<Self>) {
        if self.preview.finder_tags.saving {
            return;
        }
        let tags = match self.preview.finder_tags.candidate_tags() {
            Ok(tags) => tags,
            Err(error) => {
                if let Some(editor) = self.preview.finder_tags.editor.as_mut() {
                    editor.error = Some(error);
                }
                cx.notify();
                return;
            }
        };
        self.persist_finder_tags(tags, true, cx);
    }

    pub(crate) fn remove_finder_tag(&mut self, raw: &str, cx: &mut Context<Self>) {
        if self.preview.finder_tags.saving
            || !self
                .preview
                .finder_tags
                .tags
                .iter()
                .any(|tag| tag.raw == raw)
        {
            return;
        }
        let tags = self.preview.finder_tags.tags_without(raw);
        self.persist_finder_tags(tags, false, cx);
    }

    pub(crate) fn persist_finder_tags(
        &mut self,
        tags: Vec<String>,
        from_editor: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.preview.finder_tags.path.clone() else {
            return;
        };
        self.preview.finder_tags_generation = self.preview.finder_tags_generation.wrapping_add(1);
        let generation = self.preview.finder_tags_generation;
        self.preview.finder_tags.saving = true;
        self.preview.finder_tags.error = None;
        if let Some(editor) = self.preview.finder_tags.editor.as_mut() {
            editor.error = None;
        }
        let task = self
            .services
            .integration
            .set_finder_tags(path.clone(), tags.clone());
        self.preview.finder_tags_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if view.preview.finder_tags_generation != generation
                    || view.preview.finder_tags.path.as_deref() != Some(path.as_path())
                {
                    return;
                }
                view.preview.finder_tags.saving = false;
                match result {
                    Ok(()) => {
                        view.show_saved_finder_tags(&path, &tags);
                        view.preview.finder_tags.set_tags(path, tags);
                        view.status_message = Some("Finder tags updated".to_string());
                    }
                    Err(error) => {
                        let message = error.to_string();
                        view.record_error("Finder tags save failed", &message);
                        view.preview.finder_tags.error = Some(message.clone());
                        if from_editor
                            && let Some(editor) = view.preview.finder_tags.editor.as_mut()
                        {
                            editor.error = Some(message);
                        }
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Repaint the row of an item whose tags were just saved, without
    /// waiting for the watcher to report the attribute change.
    pub(crate) fn show_saved_finder_tags(&mut self, path: &Path, tags: &[String]) {
        let Some(listed) = self.listed_entry(path) else {
            return;
        };
        let mut entry = listed.as_ref().clone();
        entry.tags = tags
            .iter()
            .map(|raw| explorie_core::FinderTag::from_raw(raw))
            .filter(|tag| !tag.name.is_empty())
            .collect();
        entry.has_xattrs |= !entry.tags.is_empty();
        let change = vec![(path.to_path_buf(), Some(entry))];
        if let Some(parent) = path.parent() {
            self.column_view
                .columns
                .apply_changes(parent, change.clone());
        }
        if path.parent() == Some(self.browser.path()) {
            self.browser.apply_entry_changes(change);
        }
    }

    pub(crate) fn render_finder_tags(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        if !self.services.integration.finder_tags_supported()
            || self.preview.finder_tags.path.as_deref() != Some(path)
        {
            return div().into_any_element();
        }
        let state = self.preview.finder_tags.clone();
        let tags: Vec<AnyElement> = state
            .tags
            .iter()
            .enumerate()
            .map(|(index, tag)| {
                let color = tag.color();
                let tag_color = rgb(color.rgb);
                let foreground = if tag.color_index == 0 {
                    self.palette.text
                } else {
                    contrasting_text(tag_color)
                };
                let raw = tag.raw.clone();
                let remove_label = format!("Remove Finder tag {}", tag.name);
                div()
                    .id(ElementId::Name(format!("finder-tag-{index}").into()))
                    .debug_selector(move || format!("finder-tag-{index}"))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(if tag.color_index == 0 {
                        self.palette.border
                    } else {
                        tag_color
                    })
                    .bg(if tag.color_index == 0 {
                        self.palette.control
                    } else {
                        tag_color
                    })
                    .text_xs()
                    .text_color(foreground)
                    .when(tag.color_index > 0, |chip| {
                        chip.child(div().w(px(7.0)).h(px(7.0)).rounded_md().bg(foreground))
                    })
                    .child(div().max_w(px(120.0)).truncate().child(tag.name.clone()))
                    .when(!state.saving, |chip| {
                        chip.child(
                            div()
                                .id(ElementId::Name(format!("remove-finder-tag-{index}").into()))
                                .debug_selector(move || format!("remove-finder-tag-{index}"))
                                .role(Role::Button)
                                .aria_label(remove_label)
                                .focusable()
                                .tab_stop(true)
                                .ml_1()
                                .px_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(with_alpha(foreground, 0.0))
                                .focus(move |button| {
                                    button
                                        .border_color(foreground)
                                        .bg(with_alpha(foreground, 0.16))
                                })
                                .hover(move |button| button.bg(with_alpha(foreground, 0.12)))
                                .cursor_pointer()
                                .text_color(if tag.color_index == 0 {
                                    self.palette.muted
                                } else {
                                    foreground
                                })
                                .child(toolbar_icon(
                                    "close",
                                    self.palette.icon_size.clamp(10.0, 14.0),
                                    if tag.color_index == 0 {
                                        self.palette.muted
                                    } else {
                                        foreground
                                    },
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_finder_tag(&raw, cx)
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let editor = state.editor.clone();
        let error = editor
            .as_ref()
            .and_then(|editor| editor.error.clone())
            .or(state.error.clone());

        div()
            .id("finder-tags")
            .debug_selector(|| "finder-tags".to_string())
            .flex()
            .flex_col()
            .gap_2()
            .mt_2()
            .pt_3()
            .border_t_1()
            .border_color(self.palette.border)
            .child(
                div()
                    .text_sm()
                    .text_color(self.palette.text)
                    .child("Finder tags"),
            )
            .when(state.loading, |section| {
                section.child(
                    div()
                        .text_sm()
                        .text_color(self.palette.muted)
                        .child("Loading tags…"),
                )
            })
            .when(!state.loading, |section| {
                section.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .when(tags.is_empty(), |list| {
                            list.child(
                                div()
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child("No tags"),
                            )
                        })
                        .children(tags)
                        .when(editor.is_none() && !state.saving, |list| {
                            list.child(
                                toolbar_button("add-finder-tag", "+ Add tag", self.palette.control)
                                    .debug_selector(|| "add-finder-tag".to_string())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.begin_add_finder_tag(cx)),
                                    ),
                            )
                        }),
                )
            })
            .when_some(editor, |section, editor| {
                let color = finder_tag_color(editor.color_index);
                section.child(
                    div()
                        .id("finder-tag-editor")
                        .debug_selector(|| "finder-tag-editor".to_string())
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("finder-tag-name-input")
                                .debug_selector(|| "finder-tag-name-input".to_string())
                                .min_w(px(120.0))
                                .max_w(px(200.0))
                                .flex_1()
                                .child(
                                    self.native_text_input_element(TextInputTarget::FinderTag)
                                        .unwrap_or_else(|| {
                                            div()
                                                .h(px(34.0))
                                                .px_2()
                                                .flex()
                                                .items_center()
                                                .border_1()
                                                .border_color(self.palette.accent)
                                                .bg(self.palette.window)
                                                .text_sm()
                                                .child(if editor.input.is_empty() {
                                                    "Tag name…".to_string()
                                                } else {
                                                    editor.input
                                                })
                                                .into_any_element()
                                        }),
                                ),
                        )
                        .child(
                            toolbar_button(
                                "finder-tag-color",
                                &format!("Color: {}", color.name),
                                rgb(color.rgb),
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.cycle_finder_tag_color(1, cx)),
                            ),
                        )
                        .child(
                            toolbar_button("confirm-finder-tag", "Add", self.palette.accent)
                                .on_click(cx.listener(|this, _, _, cx| this.submit_finder_tag(cx))),
                        )
                        .child(
                            toolbar_button("cancel-finder-tag", "Cancel", self.palette.control)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.cancel_add_finder_tag(cx)),
                                ),
                        ),
                )
            })
            .when(state.saving, |section| {
                section.child(
                    div()
                        .text_xs()
                        .text_color(self.palette.muted)
                        .child("Saving tags…"),
                )
            })
            .when_some(error, |section, error| {
                section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(rgb(0xffb86c))
                        .child(div().flex_1().child(error))
                        .when(state.tags.is_empty() && state.editor.is_none(), |row| {
                            row.child(
                                toolbar_button("retry-finder-tags", "Retry", self.palette.control)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.retry_finder_tags(cx)),
                                    ),
                            )
                        }),
                )
            })
            .into_any_element()
    }
}
