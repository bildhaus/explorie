//! `DirectoryWindow` behavior for preview view.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn render_photo_metadata(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.preview.photo_metadata.clone();
        if state.path().is_some_and(|candidate| candidate != path) {
            return div().into_any_element();
        }
        let content = match state {
            PhotoMetadataState::Unavailable => return div().into_any_element(),
            PhotoMetadataState::Loading { .. } => div()
                .text_sm()
                .text_color(self.palette.muted)
                .child("Reading photo metadata…")
                .into_any_element(),
            PhotoMetadataState::Failed { error, .. } => div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(0xffb86c))
                        .child(format!("Unable to read photo metadata: {error}")),
                )
                .child(
                    toolbar_button("retry-photo-metadata", "Retry", self.palette.control)
                        .on_click(cx.listener(|this, _, _, cx| this.retry_photo_metadata(cx))),
                )
                .into_any_element(),
            PhotoMetadataState::Ready { metadata, .. } => {
                let rows: Vec<AnyElement> =
                    photo_metadata_rows(&metadata, self.preview.photo_gps_revealed)
                        .into_iter()
                        .map(|(label, value)| metadata_row(label, value, self.palette))
                        .collect();
                let gps_available = metadata.gps.is_some();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(!metadata.has_photo_metadata(), |panel| {
                        panel.child(
                            div()
                                .text_sm()
                                .text_color(self.palette.muted)
                                .child("No camera or IPTC metadata"),
                        )
                    })
                    .children(rows)
                    .when(gps_available, |panel| {
                        panel.child(
                            toolbar_button(
                                "toggle-photo-location",
                                if self.preview.photo_gps_revealed {
                                    "Hide location"
                                } else {
                                    "Show location"
                                },
                                self.palette.control,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_photo_gps(cx))),
                        )
                    })
                    .into_any_element()
            }
        };
        div()
            .id("photo-metadata")
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
                    .child("Photo metadata"),
            )
            .child(content)
            .into_any_element()
    }

    pub(crate) fn render_preview_metadata(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = self
            .browser
            .visible_entries()
            .iter()
            .find(|entry| entry.path == path)
            .cloned();
        let mut rows = vec![metadata_row(
            "Path",
            path.display().to_string(),
            self.palette,
        )];
        if let Some(entry) = entry {
            let name = entry
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let file_type = self.preview.detection.as_ref().map_or_else(
                || {
                    entry
                        .path
                        .extension()
                        .map(|extension| extension.to_string_lossy().to_ascii_uppercase())
                        .filter(|extension| !extension.is_empty())
                        .unwrap_or_else(|| "Unknown".to_string())
                },
                |detection| detection.description.clone(),
            );
            rows.insert(0, metadata_row("Name", name, self.palette));
            rows.push(metadata_row("Size", format_size(entry.size), self.palette));
            rows.push(metadata_row(
                "Modified",
                format_metadata_modified(entry.modified),
                self.palette,
            ));
            rows.push(metadata_row("Type", file_type, self.palette));
            if let Some(mime_type) = self
                .preview
                .detection
                .as_ref()
                .and_then(|detection| detection.mime_type.clone())
            {
                rows.push(metadata_row("Content type", mime_type, self.palette));
            }
            rows.push(metadata_row(
                "Link",
                if entry.is_symlink {
                    "Symbolic link"
                } else if entry.is_junction {
                    "Junction"
                } else {
                    "No"
                },
                self.palette,
            ));
            if entry.is_cloud_placeholder {
                rows.push(
                    div()
                        .id("metadata-cloud-placeholder")
                        .debug_selector(|| "metadata-cloud-placeholder".to_string())
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .w(px(96.0))
                                .min_w(px(96.0))
                                .text_color(self.palette.muted)
                                .child("Storage"),
                        )
                        .child(toolbar_icon("cloud", 14.0, self.palette.muted))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(self.palette.text)
                                .child("In the cloud, not downloaded"),
                        )
                        .into_any_element(),
                );
            }
        }
        let actions = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .pb_2()
            .child(
                toolbar_button("metadata-open", "Open", self.palette.control)
                    .debug_selector(|| "metadata-open".to_string())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(path) = this.preview.state.path().map(Path::to_path_buf) {
                            this.open_entry(path, false, cx);
                        }
                    })),
            )
            .child(
                toolbar_button("metadata-reveal", "Reveal", self.palette.control)
                    .debug_selector(|| "metadata-reveal".to_string())
                    .on_click(cx.listener(|this, _, _, cx| this.reveal_previewed_item(cx))),
            )
            .when(std::env::consts::OS == "windows", |actions| {
                actions.child(
                    toolbar_button("metadata-open-with", "Open with…", self.palette.control)
                        .debug_selector(|| "metadata-open-with".to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.open_previewed_with(cx))),
                )
            })
            .child(
                toolbar_button("metadata-quick-look", "Quick Look", self.palette.control)
                    .debug_selector(|| "metadata-quick-look".to_string())
                    .on_click(cx.listener(|this, _, _, cx| this.quick_look_previewed_item(cx))),
            );
        let finder_tags = self.render_finder_tags(path, cx);
        let photo_metadata = self.render_photo_metadata(path, cx);
        div()
            .id("preview-metadata")
            .debug_selector(|| "preview-metadata".to_string())
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_3()
            .border_t_1()
            .border_color(self.palette.border)
            .bg(self.palette.window)
            .child(actions)
            .children(rows)
            .child(finder_tags)
            .child(photo_metadata)
            .into_any_element()
    }

    pub(crate) fn render_preview_summary_panel(
        &mut self,
        side_panel: bool,
        column_preview: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.effective_selected_entries();
        let (selector, icon, title, summary, guidance) = if selected.is_empty() {
            let stats = self.browser.entry_stats();
            let (folder_count, file_count) = (stats.folders, stats.files);
            (
                "preview-summary-empty",
                "folder",
                path_label(self.browser.path()),
                format!(
                    "{folder_count} folder{} · {file_count} file{}",
                    if folder_count == 1 { "" } else { "s" },
                    if file_count == 1 { "" } else { "s" }
                ),
                "Select an item to preview".to_string(),
            )
        } else if selected.len() == 1 && is_folder_like(&selected[0]) {
            let entry = &selected[0];
            (
                "preview-summary-folder",
                "folder",
                file_name(entry),
                if entry.is_symlink || entry.is_junction {
                    "Link to a folder".to_string()
                } else {
                    "Folder".to_string()
                },
                "Open this folder to browse its contents".to_string(),
            )
        } else if selected.len() == 1 && selected[0].is_package {
            // Packages are shown as the single item Finder presents.
            let entry = &selected[0];
            let application = entry
                .path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"));
            let kind = if application {
                "Application"
            } else {
                "Package"
            };
            (
                "preview-summary-package",
                "file",
                file_name(entry),
                if entry.is_dir && entry.size > 0 {
                    format!("{kind} · {}", format_size(entry.size))
                } else {
                    kind.to_string()
                },
                if application {
                    "Open to launch it, or use Show Package Contents to browse inside".to_string()
                } else {
                    "Open it with its app, or use Show Package Contents to browse inside"
                        .to_string()
                },
            )
        } else if selected.len() == 1 {
            let entry = &selected[0];
            (
                "preview-summary-file",
                "file",
                file_name(entry),
                format_size(entry.size),
                "Preview is not available for this file".to_string(),
            )
        } else {
            let folder_count = selected
                .iter()
                .filter(|entry| is_folder_like(entry))
                .count();
            let file_count = selected.len().saturating_sub(folder_count);
            let total_size = selected
                .iter()
                .filter(|entry| !is_folder_like(entry))
                .map(|entry| entry.size)
                .sum::<u64>();
            let summary = if total_size > 0 {
                format!(
                    "{folder_count} folder{} · {file_count} file{} · {}",
                    if folder_count == 1 { "" } else { "s" },
                    if file_count == 1 { "" } else { "s" },
                    format_size(total_size)
                )
            } else {
                format!(
                    "{folder_count} folder{} · {file_count} file{}",
                    if folder_count == 1 { "" } else { "s" },
                    if file_count == 1 { "" } else { "s" }
                )
            };
            (
                "preview-summary-multiple",
                "file",
                format!("{} items selected", selected.len()),
                summary,
                "Choose one file for a full preview".to_string(),
            )
        };
        let resizer = side_panel.then(|| self.render_preview_panel_resizer(cx));

        div()
            .id(if column_preview {
                "column-preview"
            } else {
                "preview-panel"
            })
            .debug_selector(move || {
                if column_preview {
                    "column-preview".to_string()
                } else {
                    "preview-panel".to_string()
                }
            })
            .role(Role::Region)
            .aria_label("Pinned preview inspector")
            .relative()
            .flex()
            .flex_col()
            .border_color(self.palette.border)
            .bg(self.palette.panel)
            .when(side_panel, |panel| {
                panel
                    .flex_shrink_0()
                    .w(px(self.layout.preview_panel_width * self.palette.scale
                        + if column_preview {
                            self.column_view.preview_fill
                        } else {
                            0.0
                        }))
                    .min_w(px(MIN_PREVIEW_PANEL_WIDTH * self.palette.scale))
                    .h_full()
                    .border_l_1()
            })
            .when(!side_panel, |panel| panel.border_t_1())
            .when_some(resizer, |panel, resizer| panel.child(resizer))
            .child(
                div()
                    .id("preview-summary-header")
                    .debug_selector(|| "preview-summary-header".to_string())
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Inspector"),
                    )
                    .child(
                        compact_toolbar_button(
                            "close-preview",
                            "Hide preview inspector",
                            "close",
                            self.palette,
                            false,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_preview_panel(cx))),
                    ),
            )
            .child(
                div()
                    .id(selector)
                    .debug_selector(move || selector.to_string())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .min_h(px(180.0))
                    .p_4()
                    .text_center()
                    .child(toolbar_icon(icon, 32.0, self.palette.muted))
                    .child(
                        div()
                            .max_w(px(420.0))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(self.palette.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .max_w(px(420.0))
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(summary),
                    )
                    .child(
                        div()
                            .max_w(px(420.0))
                            .text_xs()
                            .text_color(self.palette.tertiary)
                            .child(guidance),
                    ),
            )
            .into_any_element()
    }

    /// Render the preview panel (inspector, column preview or Quick Look).
    /// Images inside it decode through the window's bounded preview cache.
    pub(crate) fn render_preview_panel(
        &mut self,
        side_panel: bool,
        quick_look: bool,
        column_preview: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let image_cache = self.image_memory.preview_cache(cx);
        let panel = self.render_preview_panel_content(side_panel, quick_look, column_preview, cx);
        with_image_cache(&image_cache, panel).into_any_element()
    }

    fn render_preview_panel_content(
        &mut self,
        side_panel: bool,
        quick_look: bool,
        column_preview: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.preview.state.clone();
        let Some(path) = state.path().map(Path::to_path_buf) else {
            return div().into_any_element();
        };
        let preview_body = match state {
            PreviewState::Closed => unreachable!("closed previews have no path"),
            PreviewState::Loading { .. } => div()
                .id("preview-loading")
                .debug_selector(|| "preview-loading".to_string())
                .flex()
                .flex_1()
                .w_full()
                .min_h_0()
                .items_center()
                .justify_center()
                .min_h(px(if quick_look { 320.0 } else { 160.0 }))
                .px_4()
                .py_6()
                .text_sm()
                .text_color(self.palette.muted)
                .child("Preparing preview…")
                .into_any_element(),
            PreviewState::Failed { error, .. } => div()
                .id("preview-failed")
                .debug_selector(|| "preview-failed".to_string())
                .flex()
                .flex_col()
                .flex_1()
                .w_full()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_3()
                .px_4()
                .py_5()
                .text_sm()
                .text_center()
                .text_color(rgb(0xffb86c))
                .child(
                    div()
                        .id("preview-failed-message")
                        .debug_selector(|| "preview-failed-message".to_string())
                        .max_w(px(480.0))
                        .child(error.to_string()),
                )
                .child(
                    toolbar_button("retry-preview", "Retry", rgb(0x493232))
                        .debug_selector(|| "retry-preview".to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.retry_preview(cx))),
                )
                .into_any_element(),
            PreviewState::Ready { content, .. } => match content {
                PreviewContent::Text(preview) => {
                    let wrapped = self.preview.text.wrap_override.unwrap_or(preview.wrapped);
                    let default_wrapped = preview.wrapped;
                    let show_line_numbers = preview.language.is_some() && !wrapped;
                    let language = preview
                        .language
                        .clone()
                        .unwrap_or_else(|| "Plain text".into());
                    // Everything below is O(visible lines): the document shares
                    // its text and caches line ranges and syntax spans, and find
                    // matches are cached until the query or document changes.
                    let placeholder = preview.text.is_empty() && wrapped;
                    let (document, find_matches) = if placeholder {
                        (
                            Arc::new(TextPreviewDocument::placeholder("Empty markdown file")),
                            Arc::<[std::ops::Range<usize>]>::from([]),
                        )
                    } else {
                        let matches = self.preview.text.find_matches(&preview);
                        (Arc::clone(&preview), matches)
                    };
                    let line_count = document.line_count();
                    let find_query = self.preview.text.find.clone();
                    let find_count = find_matches.len();
                    let active_find_match = find_matches.get(self.preview.text.find_match).cloned();
                    let find_open = self.text_input.target == Some(TextInputTarget::PreviewFind);
                    let find_input = self.native_text_input_element(TextInputTarget::PreviewFind);
                    let preview_scale = self.settings.appearance.ui_scale;
                    let gutter_width =
                        (24.0 + line_count.to_string().len() as f32 * 8.0) * preview_scale;
                    let palette = self.palette;
                    let text_body = if wrapped {
                        if self.preview.text.wrapped_list.item_count() != line_count {
                            self.preview.text.wrapped_list.reset(line_count);
                        }
                        let wrapped_lines = list(
                            self.preview.text.wrapped_list.clone(),
                            move |line_index, _, _| {
                                text_preview_line(
                                    &document,
                                    &find_matches,
                                    active_find_match.as_ref(),
                                    line_index,
                                    show_line_numbers,
                                    gutter_width,
                                    true,
                                    palette,
                                )
                                .unwrap_or_else(|| div().into_any_element())
                            },
                        )
                        .flex_1()
                        .w_full()
                        .px_3()
                        .py_3();
                        div()
                            .id("text-preview-wrapped")
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .text_xs()
                            .line_height(px(19.0 * preview_scale))
                            .font_family(monospace_font_family())
                            .text_color(self.palette.text)
                            .child(wrapped_lines)
                            .into_any_element()
                    } else {
                        let virtual_lines = uniform_list(
                            "text-preview-virtual-lines",
                            line_count,
                            cx.processor(move |_, visible: std::ops::Range<usize>, _, _| {
                                visible
                                    .filter_map(|line_index| {
                                        text_preview_line(
                                            &document,
                                            &find_matches,
                                            active_find_match.as_ref(),
                                            line_index,
                                            show_line_numbers,
                                            gutter_width,
                                            false,
                                            palette,
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .flex_1()
                        .w_full()
                        .track_scroll(&self.preview.text.unwrapped_scroll);
                        div()
                            .id("text-preview-code")
                            .debug_selector(|| "text-preview-code".to_string())
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .overflow_x_scroll()
                            .px_3()
                            .py_3()
                            .text_xs()
                            .line_height(px(19.0 * preview_scale))
                            .font_family(monospace_font_family())
                            .text_color(self.palette.text)
                            .child(virtual_lines)
                            .into_any_element()
                    };
                    div()
                        .id("text-preview-surface")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .max_h(px((if quick_look { 820.0 } else { 360.0 }) * preview_scale))
                        .when(quick_look, |surface| surface.h_full())
                        .bg(self.palette.window)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(self.palette.border)
                                .bg(self.palette.surface)
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child(div().flex_1().min_w_0().truncate().child(format!(
                                    "{language}  •  {}  •  {line_count} line{}",
                                    preview.encoding,
                                    if line_count == 1 { "" } else { "s" }
                                )))
                                .child(
                                    toolbar_button(
                                        "text-preview-find",
                                        "Find",
                                        if find_open {
                                            self.palette.selected
                                        } else {
                                            self.palette.control
                                        },
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.open_text_preview_find(cx)
                                        }),
                                    ),
                                )
                                .child(
                                    toolbar_button(
                                        "text-preview-copy",
                                        "Copy",
                                        self.palette.control,
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.copy_text_preview(cx)),
                                    ),
                                )
                                .child(
                                    toolbar_button(
                                        "text-preview-wrap",
                                        if wrapped { "Wrap: on" } else { "Wrap: off" },
                                        if wrapped {
                                            self.palette.selected
                                        } else {
                                            self.palette.control
                                        },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.toggle_text_preview_wrap(default_wrapped, cx)
                                        },
                                    )),
                                ),
                        )
                        .when(find_open, |surface| {
                            surface.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .border_b_1()
                                    .border_color(self.palette.border)
                                    .bg(self.palette.surface)
                                    .child(div().flex_1().children(find_input))
                                    .child(
                                        toolbar_button(
                                            "text-find-previous",
                                            "Previous",
                                            self.palette.control,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.advance_text_preview_match(-1, cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        toolbar_button(
                                            "text-find-next",
                                            "Next",
                                            self.palette.control,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.advance_text_preview_match(1, cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .w(px(72.0))
                                            .text_right()
                                            .text_xs()
                                            .text_color(self.palette.muted)
                                            .child(if find_query.trim().is_empty() {
                                                "0 matches".to_string()
                                            } else if find_count == 0 {
                                                "No matches".to_string()
                                            } else {
                                                format!(
                                                    "{} / {find_count}",
                                                    self.preview.text.find_match + 1
                                                )
                                            }),
                                    ),
                            )
                        })
                        .child(text_body)
                        .when(preview.truncated, |body| {
                            body.child(
                                div()
                                    .id("text-preview-truncated")
                                    .px_3()
                                    .py_2()
                                    .border_t_1()
                                    .border_color(self.palette.border)
                                    .bg(self.palette.surface)
                                    .text_xs()
                                    .text_color(rgb(0xffb86c))
                                    .child("Preview limited to the first 512 KiB"),
                            )
                        })
                        .into_any_element()
                }
                PreviewContent::Audio | PreviewContent::Video | PreviewContent::Model => {
                    // Playback lives in the media player entity, so its
                    // periodic updates re-render the player, not this panel.
                    let palette = self.palette;
                    self.media.update(cx, |media, _| {
                        media.set_presentation(palette, quick_look);
                    });
                    self.media.clone().into_any_element()
                }
                PreviewContent::Pdf { page, tool } => {
                    let can_previous = page.page_index > 0;
                    let can_next = page.page_index + 1 < page.page_count;
                    let zoom_label = if self.preview.pdf_fit {
                        "Fit".to_string()
                    } else {
                        format!("{}%", self.preview.pdf_zoom_percent)
                    };
                    let display_width = page.display_width as f32
                        * f32::from(self.preview.pdf_zoom_percent)
                        / 100.0;
                    let display_height = page.display_height as f32
                        * f32::from(self.preview.pdf_zoom_percent)
                        / 100.0;
                    let open_path = path.clone();
                    let image_path = page.image_path.clone();
                    div()
                        .id("pdf-preview")
                        .flex()
                        .flex_col()
                        .w_full()
                        .bg(self.palette.window)
                        .child(
                            div()
                                .id("pdf-toolbar")
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(self.palette.border)
                                .bg(self.palette.surface)
                                .child(
                                    toolbar_button(
                                        "pdf-previous-page",
                                        "Previous",
                                        if can_previous {
                                            self.palette.control
                                        } else {
                                            self.palette.surface
                                        },
                                    )
                                    .when(
                                        can_previous,
                                        |button| {
                                            button.on_click(cx.listener(|this, _, _, cx| {
                                                this.move_pdf_page(-1, cx)
                                            }))
                                        },
                                    ),
                                )
                                .child(
                                    div()
                                        .id("pdf-page-label")
                                        .text_xs()
                                        .text_color(self.palette.text)
                                        .child(format!(
                                            "Page {} of {}",
                                            page.page_index + 1,
                                            page.page_count
                                        )),
                                )
                                .child(
                                    toolbar_button(
                                        "pdf-next-page",
                                        "Next",
                                        if can_next {
                                            self.palette.control
                                        } else {
                                            self.palette.surface
                                        },
                                    )
                                    .when(can_next, |button| {
                                        button.on_click(
                                            cx.listener(|this, _, _, cx| this.move_pdf_page(1, cx)),
                                        )
                                    }),
                                )
                                .child(
                                    toolbar_button("pdf-zoom-out", "−", self.palette.control)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.adjust_pdf_zoom(-25, cx)
                                        })),
                                )
                                .child(
                                    toolbar_button(
                                        "pdf-fit-page",
                                        &zoom_label,
                                        if self.preview.pdf_fit {
                                            self.palette.accent
                                        } else {
                                            self.palette.control
                                        },
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.fit_pdf_page(cx))),
                                )
                                .child(
                                    toolbar_button("pdf-zoom-in", "+", self.palette.control)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.adjust_pdf_zoom(25, cx)
                                        })),
                                )
                                .child(
                                    toolbar_button(
                                        "pdf-open-external",
                                        "Open",
                                        self.palette.control,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.open_entry(open_path.clone(), false, cx)
                                        },
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .id("pdf-page-viewport")
                                .w_full()
                                .min_h(px(if quick_look || side_panel {
                                    420.0
                                } else {
                                    300.0
                                }))
                                .max_h(px(if quick_look {
                                    760.0
                                } else if side_panel {
                                    620.0
                                } else {
                                    360.0
                                }))
                                .overflow_scroll()
                                .bg(rgb(0x262626))
                                .child(
                                    div()
                                        .flex()
                                        .min_w_full()
                                        .justify_center()
                                        .items_start()
                                        .p_3()
                                        .child(
                                            img(image_path)
                                                .when(self.preview.pdf_fit, |image| {
                                                    image.max_w_full()
                                                })
                                                .when(!self.preview.pdf_fit, |image| {
                                                    image.w(px(display_width)).h(px(display_height))
                                                }),
                                        ),
                                ),
                        )
                        .when_some(tool, |preview, tool| {
                            preview.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .border_t_1()
                                    .border_color(self.palette.border)
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child(format!("Converted locally by {tool}")),
                            )
                        })
                        .into_any_element()
                }
                PreviewContent::Rich(preview) => {
                    let palette = self.palette;
                    let blocks = preview
                        .blocks
                        .into_iter()
                        .enumerate()
                        .map(move |(index, block)| rich_block_element(index, block, palette));
                    div()
                        .id("rich-preview")
                        .debug_selector(|| "rich-preview".to_string())
                        .flex()
                        .flex_col()
                        .w_full()
                        .max_h(px(if quick_look { 820.0 } else { 520.0 }))
                        .bg(self.palette.window)
                        .child(
                            div()
                                .px_4()
                                .pt_4()
                                .debug_selector(|| "rich-preview-title".to_string())
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(self.palette.text)
                                .child(preview.title),
                        )
                        .child(
                            div()
                                .px_4()
                                .pt_1()
                                .pb_3()
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child(preview.subtitle),
                        )
                        .when_some(preview.image_path, |surface, image_path| {
                            surface.child(
                                div()
                                    .flex()
                                    .justify_center()
                                    .max_h(px(if quick_look { 440.0 } else { 260.0 }))
                                    .overflow_hidden()
                                    .px_4()
                                    .pb_3()
                                    .child(img(image_path).max_w_full().max_h(px(if quick_look {
                                        420.0
                                    } else {
                                        240.0
                                    }))),
                            )
                        })
                        .child(
                            div()
                                .id("rich-preview-blocks")
                                .flex()
                                .flex_col()
                                .gap_2()
                                .min_h_0()
                                .overflow_scroll()
                                .px_4()
                                .pb_4()
                                .children(blocks),
                        )
                        .into_any_element()
                }
                PreviewContent::BlockedScript => div()
                    .id("preview-blocked-script")
                    .debug_selector(|| "preview-blocked-script".to_string())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .min_h(px(180.0))
                    .px_4()
                    .bg(self.palette.window)
                    .text_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(self.palette.text)
                            .child("Script previews are disabled for executable files."),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child("Enable this in Settings > General."),
                    )
                    .into_any_element(),
                PreviewContent::Image(image_path) => div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .max_h(px(if quick_look { 820.0 } else { 420.0 }))
                    .overflow_hidden()
                    .p_3()
                    .child(img(image_path).max_w_full().max_h(px(if quick_look {
                        790.0
                    } else {
                        390.0
                    })))
                    .into_any_element(),
                PreviewContent::Artifact(artifact) => {
                    let open_path = artifact.path.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_4()
                        .text_sm()
                        .child(div().flex_1().child(format!(
                            "{} preview generated by {} ({})",
                            artifact.kind, artifact.tool, artifact.mime_type
                        )))
                        .child(
                            toolbar_button("open-preview-artifact", "Open preview", rgb(0x31506b))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_entry(open_path.clone(), false, cx)
                                })),
                        )
                        .into_any_element()
                }
                PreviewContent::Archive => div()
                    .px_3()
                    .py_4()
                    .text_sm()
                    .child("Archive selected • use Inspect archive to browse its entries")
                    .into_any_element(),
                PreviewContent::CloudPlaceholder => {
                    let open_path = path.clone();
                    let size = self
                        .listed_entry(&path)
                        .map(|entry| format_preview_size(entry.size))
                        .unwrap_or_default();
                    div()
                        .id("preview-cloud-placeholder")
                        .debug_selector(|| "preview-cloud-placeholder".to_string())
                        .flex()
                        .flex_col()
                        .flex_1()
                        .w_full()
                        .min_h_0()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .min_h(px(180.0))
                        .px_4()
                        .text_center()
                        .child(toolbar_icon("cloud", 32.0, self.palette.muted))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(self.palette.text)
                                .child(path_label(&path)),
                        )
                        .when(!size.is_empty(), |body| {
                            body.child(div().text_xs().text_color(self.palette.muted).child(size))
                        })
                        .child(
                            div()
                                .max_w(px(360.0))
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child("Stored in the cloud — open or download it to preview"),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    toolbar_button(
                                        "open-cloud-placeholder",
                                        "Open",
                                        self.palette.control,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.open_entry(open_path.clone(), false, cx)
                                        },
                                    )),
                                )
                                .child(
                                    toolbar_button(
                                        "reveal-cloud-placeholder",
                                        "Reveal",
                                        self.palette.control,
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.reveal_previewed_item(cx)
                                        }),
                                    ),
                                ),
                        )
                        .into_any_element()
                }
                PreviewContent::Fallback { detection, error } => {
                    let open_path = path.clone();
                    let mime_type = detection
                        .mime_type
                        .clone()
                        .unwrap_or_else(|| "Unknown content type".to_string());
                    div()
                        .id("preview-fallback")
                        .flex()
                        .flex_col()
                        .gap_3()
                        .max_h(px(if quick_look { 760.0 } else { 420.0 }))
                        .overflow_y_scroll()
                        .px_4()
                        .py_5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(self.palette.text)
                                .child(detection.description),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child(mime_type),
                        )
                        .when_some(error, |fallback, error| {
                            fallback.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0xffb86c))
                                    .child(format!("Inline preview unavailable: {error}")),
                            )
                        })
                        .when_some(detection.byte_sample, |fallback, sample| {
                            fallback.child(
                                div()
                                    .id("preview-byte-sample")
                                    .overflow_x_scroll()
                                    .p_3()
                                    .rounded_md()
                                    .bg(self.palette.surface)
                                    .font_family(monospace_font_family())
                                    .text_xs()
                                    .line_height(px(18.0))
                                    .whitespace_nowrap()
                                    .text_color(self.palette.muted)
                                    .child(sample),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    toolbar_button(
                                        "open-external-preview",
                                        "Open",
                                        self.palette.accent,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.open_entry(open_path.clone(), false, cx)
                                        },
                                    )),
                                )
                                .when(std::env::consts::OS == "windows", |actions| {
                                    actions.child(
                                        toolbar_button(
                                            "open-with-external-preview",
                                            "Open with…",
                                            self.palette.control,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.open_previewed_with(cx)
                                            }),
                                        ),
                                    )
                                })
                                .child(
                                    toolbar_button(
                                        "reveal-external-preview",
                                        "Reveal",
                                        self.palette.control,
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.reveal_previewed_item(cx)
                                        }),
                                    ),
                                ),
                        )
                        .into_any_element()
                }
            },
        };
        let body = if quick_look {
            preview_body
        } else {
            match self.preview.tab {
                PreviewTab::Preview => preview_body,
                PreviewTab::Metadata => self.render_preview_metadata(&path, cx),
                PreviewTab::CustomFields => self.render_custom_fields(cx),
            }
        };
        let (position, total, previous, next) = self.preview_navigation(&path);
        let has_previous = previous.is_some();
        let has_next = next.is_some();
        if quick_look {
            let file_name = path_label(&path);
            let open_path = path.clone();
            let entry = self
                .browser
                .visible_entries()
                .iter()
                .find(|entry| entry.path == path)
                .cloned();
            let file_kind = self.preview.detection.as_ref().map_or_else(
                || {
                    path.extension()
                        .and_then(|extension| extension.to_str())
                        .filter(|extension| !extension.is_empty())
                        .map_or_else(
                            || "File".to_string(),
                            |extension| format!("{} file", extension.to_ascii_uppercase()),
                        )
                },
                |detection| detection.description.clone(),
            );
            let file_size = entry
                .as_ref()
                .map_or_else(|| "—".to_string(), |entry| format_preview_size(entry.size));
            let modified = entry.as_ref().map_or_else(
                || "—".to_string(),
                |entry| format_metadata_modified(entry.modified),
            );
            let info_rows: Vec<AnyElement> = [
                ("Name", file_name.clone()),
                ("Kind", file_kind.clone()),
                ("Size", file_size.clone()),
                ("Modified", modified),
                ("Path", path.display().to_string()),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (label, value))| {
                div()
                    .id(("quick-look-info-row", index))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(180.0))
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.tertiary)
                            .child(label),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(value),
                    )
                    .into_any_element()
            })
            .collect();
            let info_label = if self.quick_look.info_open {
                "Hide file info"
            } else {
                "Show file info"
            };
            let index_items = self
                .quick_look
                .paths
                .clone()
                .into_iter()
                .enumerate()
                .map(|(index, item_path)| {
                    let selected = item_path == path;
                    let label = path_label(&item_path);
                    div()
                        .id(("quick-look-index-item", index))
                        .debug_selector(move || format!("quick-look-index-item-{index}"))
                        .role(Role::Button)
                        .aria_label(format!("Preview {label}"))
                        .aria_selected(selected)
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .w(px(156.0))
                        .h(px(112.0))
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(if selected {
                            self.palette.accent
                        } else {
                            self.palette.border
                        })
                        .bg(if selected {
                            self.palette.selected
                        } else {
                            self.palette.surface
                        })
                        .hover(|item| item.bg(self.palette.hover))
                        .cursor_pointer()
                        .child(toolbar_icon(
                            "file",
                            28.0,
                            if selected {
                                self.palette.accent
                            } else {
                                self.palette.muted
                            },
                        ))
                        .child(
                            div()
                                .w_full()
                                .truncate()
                                .text_center()
                                .text_xs()
                                .child(label),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.quick_look.index_open = false;
                            this.start_preview(item_path.clone(), cx);
                        }))
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            let quick_look_body = if self.quick_look.index_open {
                div()
                    .id("quick-look-index-sheet")
                    .debug_selector(|| "quick-look-index-sheet".to_string())
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .justify_center()
                    .content_start()
                    .gap_3()
                    .p_4()
                    .children(index_items)
                    .into_any_element()
            } else {
                body
            };
            return div()
                .id("quick-look-backdrop")
                .debug_selector(|| "quick-look-backdrop".to_string())
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p_3()
                .bg(with_alpha(rgb(0x000000), 0.78))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.close_quick_look(cx)),
                )
                .child(
                    div()
                        .id("quick-look-modal")
                        .debug_selector(|| "quick-look-modal".to_string())
                        .role(Role::Dialog)
                        .aria_label(format!("Quick Look: {file_name}"))
                        .flex()
                        .flex_col()
                        .w_full()
                        .h_full()
                        .max_w(px(1360.0))
                        .max_h(px(940.0))
                        .min_h_0()
                        .overflow_hidden()
                        .rounded_lg()
                        .border_1()
                        .border_color(self.palette.border)
                        .bg(self.palette.panel)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .id("quick-look-header")
                                .debug_selector(|| "quick-look-header".to_string())
                                .flex()
                                .items_center()
                                .gap_3()
                                .h(px(54.0))
                                .flex_none()
                                .px_3()
                                .border_b_1()
                                .border_color(self.palette.border)
                                .bg(self.palette.topbar)
                                .child(
                                    div()
                                        .id("quick-look-navigation")
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            compact_toolbar_button(
                                                "quick-look-previous",
                                                "Previous file",
                                                "arrow-left",
                                                self.palette,
                                                false,
                                                has_previous,
                                            )
                                            .when(
                                                has_previous,
                                                |button| {
                                                    button.on_click(cx.listener(
                                                        |this, _, _, cx| {
                                                            this.navigate_preview_by_click(-1, cx)
                                                        },
                                                    ))
                                                },
                                            ),
                                        )
                                        .child(
                                            div()
                                                .id("quick-look-file-counter")
                                                .debug_selector(|| {
                                                    "quick-look-file-counter".to_string()
                                                })
                                                .min_w(px(60.0))
                                                .px_2()
                                                .py_1()
                                                .text_center()
                                                .text_xs()
                                                .text_color(self.palette.muted)
                                                .child(format!("{position} / {total}")),
                                        )
                                        .child(
                                            compact_toolbar_button(
                                                "quick-look-next",
                                                "Next file",
                                                "arrow-right",
                                                self.palette,
                                                false,
                                                has_next,
                                            )
                                            .when(
                                                has_next,
                                                |button| {
                                                    button.on_click(cx.listener(
                                                        |this, _, _, cx| {
                                                            this.navigate_preview_by_click(1, cx)
                                                        },
                                                    ))
                                                },
                                            ),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("quick-look-title-block")
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            div()
                                                .id("quick-look-file-name")
                                                .debug_selector(|| {
                                                    "quick-look-file-name".to_string()
                                                })
                                                .w_full()
                                                .truncate()
                                                .text_center()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(file_name),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .gap_2()
                                                .text_xs()
                                                .text_color(self.palette.tertiary)
                                                .child(file_kind)
                                                .child(
                                                    div()
                                                        .text_color(self.palette.border)
                                                        .child("/"),
                                                )
                                                .child(file_size),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("quick-look-actions")
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            compact_toolbar_button(
                                                "quick-look-index",
                                                "Index sheet",
                                                "grid",
                                                self.palette,
                                                self.quick_look.index_open,
                                                total > 1,
                                            )
                                            .when(
                                                total > 1,
                                                |button| {
                                                    button.on_click(cx.listener(
                                                        |this, _, _, cx| {
                                                            this.toggle_quick_look_index(cx)
                                                        },
                                                    ))
                                                },
                                            ),
                                        )
                                        .child(
                                            compact_toolbar_button(
                                                "quick-look-open",
                                                "Open",
                                                "arrow-right",
                                                self.palette,
                                                false,
                                                true,
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.open_entry(open_path.clone(), false, cx);
                                                    this.close_quick_look(cx);
                                                }),
                                            ),
                                        )
                                        .child(
                                            compact_toolbar_button(
                                                "quick-look-info",
                                                info_label,
                                                "info",
                                                self.palette,
                                                self.quick_look.info_open,
                                                true,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.toggle_quick_look_info(cx)
                                                }),
                                            ),
                                        )
                                        .child(
                                            compact_toolbar_button(
                                                "quick-look-close",
                                                "Close Quick Look",
                                                "close",
                                                self.palette,
                                                false,
                                                true,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.close_quick_look(cx)
                                                }),
                                            ),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .id("quick-look-content")
                                .debug_selector(|| "quick-look-content".to_string())
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_h_0()
                                .overflow_scroll()
                                .p_3()
                                .bg(self.palette.window)
                                .child(quick_look_body),
                        )
                        .when(self.quick_look.info_open, |modal| {
                            modal.child(
                                div()
                                    .id("quick-look-info-drawer")
                                    .debug_selector(|| "quick-look-info-drawer".to_string())
                                    .flex()
                                    .flex_wrap()
                                    .gap_3()
                                    .flex_none()
                                    .p_4()
                                    .border_t_1()
                                    .border_color(self.palette.border)
                                    .bg(self.palette.surface)
                                    .children(info_rows),
                            )
                        }),
                )
                .into_any_element();
        }
        let tabs: Vec<AnyElement> = PreviewTab::ALL
            .into_iter()
            .filter(|tab| {
                *tab != PreviewTab::CustomFields || self.preview.custom_fields_editor.is_some()
            })
            .map(|tab| {
                let active = tab == self.preview.tab;
                div()
                    .id(ElementId::Name(
                        format!("preview-tab-{}", tab.label()).into(),
                    ))
                    .role(Role::Tab)
                    .aria_label(tab.label())
                    .aria_selected(active)
                    .focusable()
                    .tab_stop(true)
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(if active {
                        self.palette.accent
                    } else {
                        with_alpha(self.palette.border, 0.0)
                    })
                    .bg(if active {
                        self.palette.selected
                    } else {
                        with_alpha(self.palette.control, 0.0)
                    })
                    .focus_visible(|tab| {
                        tab.bg(self.palette.hover).border_color(self.palette.accent)
                    })
                    .hover(|tab| tab.bg(self.palette.hover))
                    .active(|tab| tab.opacity(0.82))
                    .cursor_pointer()
                    .text_sm()
                    .child(tab.label())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_preview_tab(tab, cx)))
                    .into_any_element()
            })
            .collect();
        let resizer = side_panel.then(|| self.render_preview_panel_resizer(cx));
        div()
            .id(if column_preview {
                "column-preview"
            } else {
                "preview-panel"
            })
            .debug_selector(move || {
                if column_preview {
                    "column-preview".to_string()
                } else {
                    "preview-panel".to_string()
                }
            })
            .role(Role::Region)
            .aria_label("Pinned preview inspector")
            .relative()
            .flex()
            .flex_col()
            .border_color(self.palette.border)
            .bg(self.palette.panel)
            .when(side_panel, |panel| {
                panel
                    .flex_shrink_0()
                    .w(px(self.layout.preview_panel_width * self.palette.scale
                        + if column_preview {
                            self.column_view.preview_fill
                        } else {
                            0.0
                        }))
                    .min_w(px(MIN_PREVIEW_PANEL_WIDTH * self.palette.scale))
                    .h_full()
                    .border_l_1()
            })
            .when(!side_panel, |panel| panel.border_t_1())
            .when_some(resizer, |panel, resizer| panel.child(resizer))
            .child(
                div()
                    .id("preview-navigation")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        compact_toolbar_button(
                            "preview-previous",
                            "Previous file",
                            "arrow-left",
                            self.palette,
                            false,
                            has_previous,
                        )
                        .when(has_previous, |button| {
                            button.on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.navigate_preview_by_click(-1, cx)
                                }),
                            )
                        }),
                    )
                    .child(
                        div()
                            .id("preview-file-counter")
                            .debug_selector(|| "preview-file-counter".to_string())
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format!("{position} / {total}")),
                    )
                    .child(
                        compact_toolbar_button(
                            "preview-next",
                            "Next file",
                            "arrow-right",
                            self.palette,
                            false,
                            has_next,
                        )
                        .when(has_next, |button| {
                            button.on_click(
                                cx.listener(|this, _, _, cx| this.navigate_preview_by_click(1, cx)),
                            )
                        }),
                    )
                    .child(
                        div()
                            .id("preview-file-name")
                            .debug_selector(|| "preview-file-name".to_string())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .tooltip({
                                let label = path.display().to_string();
                                let palette = self.palette;
                                move |_, cx| app_tooltip(label.clone(), palette, cx)
                            })
                            .child(path_label(&path)),
                    )
                    .when(!column_preview, |navigation| {
                        navigation.child(
                            compact_toolbar_button(
                                "close-preview",
                                "Close preview",
                                "close",
                                self.palette,
                                false,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_preview_panel(cx))),
                        )
                    })
                    .border_b_1()
                    .border_color(self.palette.border),
            )
            .child(
                div()
                    .id("preview-tabs")
                    .role(Role::TabList)
                    .aria_label("Preview sections")
                    .flex()
                    .items_end()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.surface)
                    .children(tabs),
            )
            .child(
                div()
                    .id("preview-tab-panel")
                    .role(Role::TabPanel)
                    .aria_label(self.preview.tab.label())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(body),
            )
            .into_any_element()
    }
}
