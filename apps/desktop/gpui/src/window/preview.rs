//! `DirectoryWindow` behavior for preview.

use crate::*;

/// The Quick Look overlay: whether it is open, which files it pages through,
/// its info and index sheets, and the Space-key state that opens and closes it.
#[derive(Default)]
pub(crate) struct QuickLookUi {
    pub(crate) open: bool,
    pub(crate) info_open: bool,
    pub(crate) index_open: bool,
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) tracks_selection: bool,
    pub(crate) space_down: bool,
    pub(crate) space_closes_on_release: bool,
}

/// The preview panel and inspector: what is previewed and how far loading got,
/// the per-kind viewer state (text, PDF, photo metadata, Finder tags, archive
/// contents, custom fields), and the jobs that load them.
pub(crate) struct PreviewUi {
    pub(crate) state: PreviewState,
    pub(crate) pending_refresh: Option<PathBuf>,
    pub(crate) tab: PreviewTab,
    pub(crate) generation: u64,
    pub(crate) task: Option<Task<()>>,
    pub(crate) debounce: PreviewDebounce,
    pub(crate) cache_task: Option<Task<()>>,
    pub(crate) prefetch_tasks: Vec<Task<()>>,
    pub(crate) detection: Option<PreviewDetection>,
    pub(crate) text: TextPreviewUiState,
    pub(crate) pdf_zoom_percent: u16,
    pub(crate) pdf_fit: bool,
    pub(crate) photo_metadata: PhotoMetadataState,
    pub(crate) photo_metadata_task: Option<Task<()>>,
    pub(crate) photo_gps_revealed: bool,
    pub(crate) finder_tags: FinderTagsState,
    pub(crate) finder_tags_task: Option<Task<()>>,
    pub(crate) finder_tags_generation: u64,
    pub(crate) helpers: Vec<HelperStatus>,
    pub(crate) helpers_loading: bool,
    pub(crate) helpers_task: Option<Task<()>>,
    pub(crate) archive_inspection: Option<(PathBuf, ArchiveInfo)>,
    pub(crate) archive_inspection_loading: bool,
    pub(crate) custom_fields_editor: Option<CustomFieldsEditor>,
    /// Play the next audio preview as soon as it loads (finished audio was
    /// asked to play again).
    pub(crate) audio_autoplay_after_load: bool,
}

impl Default for PreviewUi {
    fn default() -> Self {
        Self {
            state: PreviewState::Closed,
            pending_refresh: None,
            tab: PreviewTab::Preview,
            generation: 0,
            task: None,
            debounce: PreviewDebounce::default(),
            cache_task: None,
            prefetch_tasks: Vec::new(),
            detection: None,
            text: TextPreviewUiState::default(),
            pdf_zoom_percent: 100,
            pdf_fit: true,
            photo_metadata: PhotoMetadataState::Unavailable,
            photo_metadata_task: None,
            photo_gps_revealed: false,
            finder_tags: FinderTagsState::default(),
            finder_tags_task: None,
            finder_tags_generation: 0,
            helpers: Vec::new(),
            helpers_loading: false,
            helpers_task: None,
            archive_inspection: None,
            archive_inspection_loading: false,
            custom_fields_editor: None,
            audio_autoplay_after_load: false,
        }
    }
}

/// What a debounced preview does once the selection settles.
#[derive(Clone, Copy)]
enum DeferredPreview {
    /// Preview a specific file (Quick Look navigation).
    Start,
    /// Re-read the selection, like any other selection change.
    SyncSelection,
}

impl DirectoryWindow {
    #[cfg(test)]
    pub(crate) fn preview_selected(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected_preview_path(cx) else {
            return;
        };
        self.start_preview(path, cx);
    }

    pub(crate) fn toggle_quick_look_selected(&mut self, cx: &mut Context<Self>) {
        if self.quick_look.open {
            self.close_quick_look(cx);
            return;
        }
        let selected = self
            .effective_selected_entries()
            .into_iter()
            .filter(|entry| !is_directory_entry(entry))
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        let Some(path) = selected.first().cloned() else {
            self.status_message = Some("Select one or more files to preview".to_string());
            cx.notify();
            return;
        };
        let paths = if selected.len() > 1 {
            selected
        } else {
            self.preview_files_for(&path)
        };
        self.open_quick_look(path, paths, cx);
    }

    pub(crate) fn open_quick_look(
        &mut self,
        path: PathBuf,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let selected_paths = self.effective_selected_paths();
        self.quick_look.tracks_selection = selected_paths.len() == 1 && selected_paths[0] == path;
        self.quick_look.open = true;
        self.quick_look.info_open = false;
        self.quick_look.index_open = false;
        self.quick_look.paths = paths;
        if !self.quick_look.paths.contains(&path) {
            self.quick_look.paths.insert(0, path.clone());
        }
        if self.preview.state.path() == Some(path.as_path()) {
            cx.notify();
        } else {
            self.start_preview(path, cx);
        }
    }

    #[cfg(test)]
    pub(crate) fn selected_preview_path(&mut self, cx: &mut Context<Self>) -> Option<PathBuf> {
        let entries = self.effective_selected_entries();
        if entries.len() != 1 || is_directory_entry(&entries[0]) {
            self.status_message = Some("Select exactly one file to preview".to_string());
            cx.notify();
            return None;
        }
        Some(entries[0].path.clone())
    }

    /// Where `current` sits among the files the preview steps through: its
    /// 1-based position, the number of files, and the files before and
    /// after it. The preview panel asks on every render, so this walks the
    /// files without copying their paths.
    pub(crate) fn preview_navigation(
        &self,
        current: &Path,
    ) -> (usize, usize, Option<PathBuf>, Option<PathBuf>) {
        if self.quick_look.open && !self.quick_look.paths.is_empty() {
            return preview_neighbors(self.quick_look.paths.iter().map(PathBuf::as_path), current);
        }
        let column_entries = self.preview_column_entries(current);
        let entries = column_entries
            .as_deref()
            .unwrap_or_else(|| self.browser.visible_entries());
        preview_neighbors(
            entries
                .iter()
                .filter(|entry| !is_directory_entry(entry))
                .map(|entry| entry.path.as_path()),
            current,
        )
    }

    pub(crate) fn preview_files_for(&self, current: &Path) -> Vec<PathBuf> {
        let column_entries = self.preview_column_entries(current);
        column_entries
            .as_deref()
            .unwrap_or_else(|| self.browser.visible_entries())
            .iter()
            .filter(|entry| !is_directory_entry(entry))
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// In Column view, the entries of the column that lists `current`.
    fn preview_column_entries(&self, current: &Path) -> Option<Arc<[Arc<FileEntry>]>> {
        if self.browser.view_mode() != ViewMode::Column {
            return None;
        }
        self.column_view
            .columns
            .columns()
            .iter()
            .map(|column| column.visible_entries(&self.browser))
            .find(|entries| entries.iter().any(|entry| entry.path == current))
    }

    /// Move the preview to the previous or next file in response to a key
    /// press. Key repeats are debounced; see [`Self::sync_pinned_preview_after_keyboard`].
    pub(crate) fn navigate_preview(&mut self, offset: isize, cx: &mut Context<Self>) {
        self.navigate_preview_with(offset, true, cx);
    }

    /// Move the preview to the previous or next file from a pointer click,
    /// loading the new preview immediately.
    pub(crate) fn navigate_preview_by_click(&mut self, offset: isize, cx: &mut Context<Self>) {
        self.navigate_preview_with(offset, false, cx);
    }

    fn navigate_preview_with(&mut self, offset: isize, keyboard: bool, cx: &mut Context<Self>) {
        let Some(current) = self.preview.state.path().map(Path::to_path_buf) else {
            return;
        };
        let (_, _, previous, next) = self.preview_navigation(&current);
        let target = if offset < 0 { previous } else { next };
        let Some(target) = target else {
            return;
        };
        if !self.quick_look.open || self.quick_look.tracks_selection {
            self.browser.select(target.clone());
            if let Some(index) = self
                .browser
                .visible_entries()
                .iter()
                .position(|entry| entry.path == target)
            {
                self.reveal_selected(index);
            }
            self.set_column_selection(target.clone());
        }
        self.quick_look.index_open = false;
        if keyboard && self.keyboard_selection_is_repeating(cx) {
            self.defer_preview(target, DeferredPreview::Start, cx);
        } else {
            self.start_preview(target, cx);
        }
    }

    /// Keyboard-driven variant of [`Self::sync_pinned_preview`].
    ///
    /// A single key press previews immediately. While selection changes
    /// arrive faster than [`KEYBOARD_PREVIEW_DEBOUNCE`] (a held arrow key),
    /// the superseded preview's jobs are dropped at once and the next preview
    /// only starts after the selection has settled for that long, so files
    /// passed over never start detection, metadata or Finder-tag jobs.
    pub(crate) fn sync_pinned_preview_after_keyboard(&mut self, cx: &mut Context<Self>) {
        let repeating = self.keyboard_selection_is_repeating(cx);
        let preview_visible = self.settings.view.show_preview_panel
            || !matches!(self.preview.state, PreviewState::Closed);
        let entries = self.effective_selected_entries();
        let target = (entries.len() == 1 && !is_directory_entry(&entries[0]))
            .then(|| entries[0].path.clone());
        match target {
            Some(path) if repeating && preview_visible => {
                self.defer_preview(path, DeferredPreview::SyncSelection, cx)
            }
            _ => self.sync_pinned_preview(cx),
        }
    }

    /// Record a keyboard-driven selection change and report whether it
    /// continues a burst of changes that should be debounced.
    fn keyboard_selection_is_repeating(&mut self, cx: &mut Context<Self>) -> bool {
        let now = cx.background_executor().now();
        let repeating = self.preview.debounce.task.is_some()
            || self
                .preview
                .debounce
                .last_keyboard_change
                .is_some_and(|last| {
                    now.saturating_duration_since(last) < KEYBOARD_PREVIEW_DEBOUNCE
                });
        self.preview.debounce.last_keyboard_change = Some(now);
        repeating
    }

    /// Show `path` as loading, drop every job of the superseded preview, and
    /// start the real preview once no further keyboard change arrives within
    /// [`KEYBOARD_PREVIEW_DEBOUNCE`]. Each call restarts the wait.
    fn defer_preview(&mut self, path: PathBuf, then: DeferredPreview, cx: &mut Context<Self>) {
        self.services.previews.cancel_artifact();
        if self.text_input.target == Some(TextInputTarget::PreviewFind) {
            self.deactivate_native_text_input();
        }
        self.media.update(cx, |media, cx| {
            media.stop(cx);
            media.cancel_model_render();
        });
        self.preview.generation = self.preview.generation.wrapping_add(1);
        self.preview.task = None;
        self.preview.prefetch_tasks.clear();
        self.preview.photo_metadata_task = None;
        self.preview.finder_tags_task = None;
        self.preview.state = PreviewState::Loading { path: path.clone() };
        let executor = cx.background_executor().clone();
        self.preview.debounce.task = Some(cx.spawn(async move |this, cx| {
            executor.timer(KEYBOARD_PREVIEW_DEBOUNCE).await;
            let _ = this.update(cx, |view, cx| {
                view.preview.debounce.task = None;
                match then {
                    DeferredPreview::Start => view.start_preview(path, cx),
                    DeferredPreview::SyncSelection => view.sync_pinned_preview(cx),
                }
            });
        }));
        cx.notify();
    }

    pub(crate) fn start_preview(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.preview.debounce.task = None;
        self.services.previews.cancel_artifact();
        if self.text_input.target == Some(TextInputTarget::PreviewFind) {
            self.deactivate_native_text_input();
        }
        self.preview.text.reset();
        let hinted_route =
            preview_panel::route(&path, self.settings.behavior.preview_executable_scripts);
        let autoplay_audio = self.preview.audio_autoplay_after_load;
        self.preview.audio_autoplay_after_load = false;
        self.preview.pdf_zoom_percent = 100;
        self.preview.pdf_fit = true;
        self.media.update(cx, |media, cx| {
            media.stop(cx);
            media.reset_model();
        });
        self.preview.detection = None;
        self.preview.prefetch_tasks.clear();
        let editor_matches = self
            .preview
            .custom_fields_editor
            .as_ref()
            .is_some_and(|editor| editor.path == path);
        if !editor_matches {
            self.preview.custom_fields_editor = self
                .browser
                .visible_entries()
                .iter()
                .find(|entry| entry.path == path)
                .map(|entry| CustomFieldsEditor::new(entry));
            if self.preview.custom_fields_editor.is_none()
                && self.preview.tab == PreviewTab::CustomFields
            {
                self.preview.tab = PreviewTab::Preview;
            }
        }
        self.preview.generation = self.preview.generation.wrapping_add(1);
        let generation = self.preview.generation;
        if self.is_cloud_placeholder(&path) {
            // Detection, metadata and content previews all read the file,
            // which would download it. Finder tags live in an xattr and are
            // safe to read.
            self.preview.photo_metadata = PhotoMetadataState::Unavailable;
            self.preview.photo_metadata_task = None;
            self.start_finder_tags(path.clone(), cx);
            self.preview.task = None;
            self.preview.state = PreviewState::Ready {
                path,
                content: PreviewContent::CloudPlaceholder,
            };
            cx.notify();
            return;
        }
        self.start_photo_metadata(path.clone(), generation, cx);
        self.start_finder_tags(path.clone(), cx);
        if hinted_route == PreviewRoute::BlockedScript {
            self.load_preview_route(path, hinted_route, None, false, generation, cx);
            return;
        }
        self.preview.state = PreviewState::Loading { path: path.clone() };
        let task = self.services.previews.detect(path.clone()).cancel_on_drop();
        self.preview.task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if view.preview.generation != generation {
                    return;
                }
                let detection = match result {
                    Ok(detection) => detection,
                    Err(error) => {
                        view.preview.state = PreviewState::Failed { path, error };
                        cx.notify();
                        return;
                    }
                };
                let route = preview_panel::route_with_detection(
                    &path,
                    view.settings.behavior.preview_executable_scripts,
                    &detection,
                );
                view.preview.detection = Some(detection.clone());
                view.load_preview_route(
                    path.clone(),
                    route,
                    Some(detection),
                    autoplay_audio,
                    generation,
                    cx,
                );
                view.prefetch_adjacent_previews(&path, cx);
            });
        }));
        cx.notify();
    }

    pub(crate) fn load_preview_route(
        &mut self,
        path: PathBuf,
        route: PreviewRoute,
        detection: Option<PreviewDetection>,
        autoplay_audio: bool,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        match route {
            PreviewRoute::Audio => {
                self.preview.state = PreviewState::Loading { path: path.clone() };
                let task = self.services.audio.load(path.clone());
                let fallback = detection.clone();
                self.preview.task = Some(cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |view, cx| {
                        if view.preview.generation != generation {
                            return;
                        }
                        match result {
                            Ok(status) => {
                                let media_path = path.clone();
                                view.media.update(cx, |media, cx| {
                                    media.start_audio(
                                        media_path,
                                        generation,
                                        status,
                                        autoplay_audio,
                                        cx,
                                    )
                                });
                                view.preview.state = PreviewState::Ready {
                                    path,
                                    content: PreviewContent::Audio,
                                };
                            }
                            Err(error) => {
                                view.preview.state = PreviewState::Ready {
                                    path,
                                    content: PreviewContent::Fallback {
                                        detection: fallback
                                            .unwrap_or_else(unknown_preview_detection),
                                        error: Some(error),
                                    },
                                };
                            }
                        }
                        cx.notify();
                    });
                }));
            }
            PreviewRoute::Video => {
                self.preview.state = PreviewState::Loading { path: path.clone() };
                let task = self.services.video.load(path.clone());
                let fallback = detection.clone();
                self.preview.task = Some(cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |view, cx| {
                        if view.preview.generation != generation {
                            return;
                        }
                        match result {
                            Ok(status) => {
                                let media_path = path.clone();
                                view.media.update(cx, |media, cx| {
                                    media.start_video(media_path, generation, status, cx)
                                });
                                view.preview.state = PreviewState::Ready {
                                    path,
                                    content: PreviewContent::Video,
                                };
                            }
                            Err(error) => {
                                view.preview.state = PreviewState::Ready {
                                    path,
                                    content: PreviewContent::Fallback {
                                        detection: fallback
                                            .unwrap_or_else(unknown_preview_detection),
                                        error: Some(error),
                                    },
                                };
                            }
                        }
                        cx.notify();
                    });
                }));
            }
            PreviewRoute::Pdf => {
                self.start_pdf_page(path.clone(), path, 0, None, generation, cx);
            }
            PreviewRoute::BlockedScript => {
                self.preview.state = PreviewState::Ready {
                    path,
                    content: PreviewContent::BlockedScript,
                };
                self.preview.task = None;
            }
            PreviewRoute::DirectImage => {
                // Small images go to the GPU decoder as they are; large ones
                // are replaced by a downscaled copy from the preview service.
                self.preview.state = PreviewState::Loading { path: path.clone() };
                let task = self.services.previews.display_image(path.clone());
                let fallback = detection.clone();
                self.preview.task = Some(cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |view, cx| {
                        if view.preview.generation != generation {
                            return;
                        }
                        view.preview.state = match result {
                            Ok(image_path) => PreviewState::Ready {
                                path,
                                content: PreviewContent::Image(image_path),
                            },
                            Err(error) => PreviewState::Ready {
                                path,
                                content: PreviewContent::Fallback {
                                    detection: fallback.unwrap_or_else(unknown_preview_detection),
                                    error: Some(error),
                                },
                            },
                        };
                        cx.notify();
                    });
                }));
            }
            PreviewRoute::Model => {
                self.preview.state = PreviewState::Loading { path: path.clone() };
                let camera = self.media.read(cx).model_camera;
                let task = self
                    .services
                    .previews
                    .model(path.clone(), camera, 960, 640)
                    .cancel_on_drop();
                let fallback = detection.clone();
                self.preview.task = Some(cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let rendered = match &result {
                        Ok(preview) => {
                            let frame = preview.frame.clone();
                            cx.background_spawn(async move { render_model_frame(&frame) })
                                .await
                        }
                        Err(_) => None,
                    };
                    let _ = this.update(cx, |view, cx| {
                        if view.preview.generation != generation {
                            return;
                        }
                        view.preview.state = match result {
                            Ok(preview) => {
                                let media_path = path.clone();
                                view.media.update(cx, |media, cx| {
                                    media.start_model(
                                        media_path, generation, preview, rendered, camera, cx,
                                    )
                                });
                                PreviewState::Ready {
                                    path,
                                    content: PreviewContent::Model,
                                }
                            }
                            Err(error) => PreviewState::Ready {
                                path,
                                content: PreviewContent::Fallback {
                                    detection: fallback.unwrap_or_else(unknown_preview_detection),
                                    error: Some(error),
                                },
                            },
                        };
                        cx.notify();
                    });
                }));
            }
            PreviewRoute::Rich => {
                self.preview.state = PreviewState::Loading { path: path.clone() };
                let task = self.services.previews.rich(path.clone()).cancel_on_drop();
                let fallback = detection.clone();
                self.preview.task = Some(cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |view, cx| {
                        if view.preview.generation != generation {
                            return;
                        }
                        view.preview.state = match result {
                            Ok(preview) => PreviewState::Ready {
                                path,
                                content: PreviewContent::Rich(preview),
                            },
                            Err(error) => PreviewState::Ready {
                                path,
                                content: PreviewContent::Fallback {
                                    detection: fallback.unwrap_or_else(unknown_preview_detection),
                                    error: Some(error),
                                },
                            },
                        };
                        cx.notify();
                    });
                }));
            }
            PreviewRoute::Archive => {
                self.preview.state = PreviewState::Ready {
                    path,
                    content: PreviewContent::Archive,
                };
                self.preview.task = None;
            }
            PreviewRoute::External => {
                self.preview.state = PreviewState::Ready {
                    path,
                    content: PreviewContent::Fallback {
                        detection: detection.unwrap_or_else(unknown_preview_detection),
                        error: None,
                    },
                };
                self.preview.task = None;
            }
            PreviewRoute::Text => {
                self.preview.state = PreviewState::Loading { path: path.clone() };
                let task = self
                    .services
                    .previews
                    .read_text(path.clone(), preview_panel::text_preview_bytes())
                    .cancel_on_drop();
                let fallback = detection.clone();
                self.preview.task = Some(cx.spawn(async move |this, cx| {
                    // Index lines and highlights once, off the UI thread.
                    let result = match task.await {
                        Ok(preview) => Ok(Arc::new(
                            cx.background_spawn(async move { TextPreviewDocument::new(preview) })
                                .await,
                        )),
                        Err(error) => Err(error),
                    };
                    let _ = this.update(cx, |view, cx| {
                        if view.preview.generation != generation {
                            return;
                        }
                        view.preview.state = match result {
                            Ok(document) => PreviewState::Ready {
                                path,
                                content: PreviewContent::Text(document),
                            },
                            Err(error) => PreviewState::Ready {
                                path,
                                content: PreviewContent::Fallback {
                                    detection: fallback.unwrap_or_else(unknown_preview_detection),
                                    error: Some(error),
                                },
                            },
                        };
                        cx.notify();
                    });
                }));
            }
            PreviewRoute::GeneratedArtifact => {
                self.preview.state = PreviewState::Loading { path: path.clone() };
                let task = self
                    .services
                    .previews
                    .artifact(path.clone())
                    .cancel_on_drop();
                let previews = self.services.previews.clone();
                let fallback = detection.clone();
                self.preview.task = Some(cx.spawn(async move |this, cx| {
                    let result = match task.await {
                        Ok(artifact) if artifact.kind == "image" => {
                            Ok(PreviewContent::Image(artifact.path))
                        }
                        Ok(artifact) if artifact.kind == "pdf" => previews
                            .pdf_page(artifact.path, 0)
                            .cancel_on_drop()
                            .await
                            .map(|page| PreviewContent::Pdf {
                                page,
                                tool: Some(artifact.tool),
                            }),
                        Ok(artifact) => Ok(PreviewContent::Artifact(artifact)),
                        Err(error) => Err(error),
                    };
                    let _ = this.update(cx, |view, cx| {
                        if view.preview.generation != generation {
                            return;
                        }
                        view.preview.state = match result {
                            Ok(content) => PreviewState::Ready { path, content },
                            Err(error) => PreviewState::Ready {
                                path,
                                content: PreviewContent::Fallback {
                                    detection: fallback.unwrap_or_else(unknown_preview_detection),
                                    error: Some(error),
                                },
                            },
                        };
                        cx.notify();
                    });
                }));
            }
        }
        cx.notify();
    }

    pub(crate) fn prefetch_adjacent_previews(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.preview.prefetch_tasks.clear();
        let (_, _, previous, next) = self.preview_navigation(path);
        for adjacent in [previous, next].into_iter().flatten() {
            if self.is_cloud_placeholder(&adjacent) {
                continue;
            }
            let extension = adjacent
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if extension == "pdf" {
                let task = self
                    .services
                    .previews
                    .pdf_page(adjacent, 0)
                    .cancel_on_drop();
                self.preview
                    .prefetch_tasks
                    .push(cx.spawn(async move |_, _| {
                        let _ = task.await;
                    }));
            }
        }
    }

    /// The listed entry for `path`, from the current listing or a column.
    pub(crate) fn listed_entry(&self, path: &Path) -> Option<Arc<FileEntry>> {
        let find =
            |entries: &[Arc<FileEntry>]| entries.iter().find(|entry| entry.path == path).cloned();
        find(self.browser.entries()).or_else(|| {
            self.column_view
                .columns
                .columns()
                .iter()
                .find_map(|column| find(column.entries()))
        })
    }

    /// Whether `path` is listed as a cloud placeholder, whose contents must
    /// not be read for a preview.
    pub(crate) fn is_cloud_placeholder(&self, path: &Path) -> bool {
        self.listed_entry(path)
            .is_some_and(|entry| entry.is_cloud_placeholder)
    }

    pub(crate) fn retry_preview(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.state.path().map(Path::to_path_buf) else {
            self.status_message = Some("No preview to retry".to_string());
            cx.notify();
            return;
        };
        self.start_preview(path, cx);
    }

    pub(crate) fn start_pdf_page(
        &mut self,
        original_path: PathBuf,
        source_path: PathBuf,
        page_index: usize,
        tool: Option<String>,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        self.preview.state = PreviewState::Loading {
            path: original_path.clone(),
        };
        let task = self
            .services
            .previews
            .pdf_page(source_path, page_index)
            .cancel_on_drop();
        self.preview.task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if view.preview.generation != generation {
                    return;
                }
                view.preview.state = match result {
                    Ok(page) => PreviewState::Ready {
                        path: original_path,
                        content: PreviewContent::Pdf { page, tool },
                    },
                    Err(error) => PreviewState::Ready {
                        path: original_path,
                        content: PreviewContent::Fallback {
                            detection: view
                                .preview
                                .detection
                                .clone()
                                .unwrap_or_else(unknown_preview_detection),
                            error: Some(error),
                        },
                    },
                };
                cx.notify();
            });
        }));
    }

    pub(crate) fn move_pdf_page(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some((original_path, source_path, current, count, tool)) = (match &self.preview.state {
            PreviewState::Ready {
                path,
                content: PreviewContent::Pdf { page, tool },
            } => Some((
                path.clone(),
                page.source_path.clone(),
                page.page_index,
                page.page_count,
                tool.clone(),
            )),
            _ => None,
        }) else {
            return;
        };
        let target = current
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        if target == current {
            return;
        }
        self.preview.generation = self.preview.generation.wrapping_add(1);
        let generation = self.preview.generation;
        self.start_pdf_page(original_path, source_path, target, tool, generation, cx);
        cx.notify();
    }

    pub(crate) fn adjust_pdf_zoom(&mut self, delta: i16, cx: &mut Context<Self>) {
        let current = if self.preview.pdf_fit {
            100_i16
        } else {
            self.preview.pdf_zoom_percent as i16
        };
        self.preview.pdf_fit = false;
        self.preview.pdf_zoom_percent = current.saturating_add(delta).clamp(50, 150) as u16;
        cx.notify();
    }

    pub(crate) fn fit_pdf_page(&mut self, cx: &mut Context<Self>) {
        self.preview.pdf_fit = true;
        cx.notify();
    }

    pub(crate) fn start_photo_metadata(
        &mut self,
        path: PathBuf,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if !is_image_metadata_path(&path) {
            self.preview.photo_metadata = PhotoMetadataState::Unavailable;
            self.preview.photo_metadata_task = None;
            self.preview.photo_gps_revealed = false;
            return;
        }
        if self.preview.photo_metadata.path() != Some(path.as_path()) {
            self.preview.photo_gps_revealed = false;
        }
        self.preview.photo_metadata = PhotoMetadataState::Loading { path: path.clone() };
        let task = self
            .services
            .previews
            .image_metadata(path.clone())
            .cancel_on_drop();
        self.preview.photo_metadata_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if view.preview.generation != generation
                    || view.preview.state.path() != Some(path.as_path())
                {
                    return;
                }
                view.preview.photo_metadata = match result {
                    Ok(metadata) => PhotoMetadataState::Ready { path, metadata },
                    Err(error) => {
                        view.record_error("Image metadata failed", error.to_string());
                        PhotoMetadataState::Failed { path, error }
                    }
                };
                cx.notify();
            });
        }));
    }

    pub(crate) fn retry_photo_metadata(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.photo_metadata.path().map(Path::to_path_buf) else {
            return;
        };
        self.start_photo_metadata(path, self.preview.generation, cx);
        cx.notify();
    }

    pub(crate) fn toggle_photo_gps(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.preview.photo_metadata,
            PhotoMetadataState::Ready {
                metadata: ImageMetadata { gps: Some(_), .. },
                ..
            }
        ) {
            self.preview.photo_gps_revealed = !self.preview.photo_gps_revealed;
            cx.notify();
        }
    }

    pub(crate) fn close_preview(&mut self, cx: &mut Context<Self>) {
        self.preview.debounce.task = None;
        self.services.previews.cancel_artifact();
        if self.text_input.target == Some(TextInputTarget::PreviewFind) {
            self.deactivate_native_text_input();
        }
        self.preview.text.reset();
        self.quick_look.open = false;
        self.quick_look.info_open = false;
        self.quick_look.index_open = false;
        self.quick_look.paths.clear();
        self.quick_look.tracks_selection = false;
        self.preview.audio_autoplay_after_load = false;
        self.stop_media_preview(cx);
        self.preview.generation = self.preview.generation.wrapping_add(1);
        self.preview.task = None;
        self.preview.prefetch_tasks.clear();
        self.preview.detection = None;
        self.preview.state = PreviewState::Closed;
        self.preview.tab = PreviewTab::Preview;
        self.preview.custom_fields_editor = None;
        self.preview.photo_metadata = PhotoMetadataState::Unavailable;
        self.preview.photo_metadata_task = None;
        self.preview.photo_gps_revealed = false;
        self.preview.finder_tags.unavailable();
        self.preview.finder_tags_task = None;
        self.preview.finder_tags_generation = self.preview.finder_tags_generation.wrapping_add(1);
        cx.notify();
    }

    pub(crate) fn close_quick_look(&mut self, cx: &mut Context<Self>) {
        if !self.quick_look.open {
            return;
        }
        self.quick_look.open = false;
        self.quick_look.info_open = false;
        self.quick_look.index_open = false;
        self.quick_look.paths.clear();
        self.quick_look.tracks_selection = false;
        if self.settings.view.show_preview_panel {
            let selected_path = self
                .effective_selected_entries()
                .into_iter()
                .filter(|entry| !is_directory_entry(entry))
                .map(|entry| entry.path)
                .next();
            if selected_path.as_deref() == self.preview.state.path() {
                cx.notify();
            } else {
                self.sync_pinned_preview(cx);
            }
        } else {
            self.close_preview(cx);
        }
    }

    pub(crate) fn toggle_quick_look_info(&mut self, cx: &mut Context<Self>) {
        if self.quick_look.open {
            self.quick_look.info_open = !self.quick_look.info_open;
            cx.notify();
        }
    }

    pub(crate) fn toggle_quick_look_index(&mut self, cx: &mut Context<Self>) {
        if self.quick_look.open && self.quick_look.paths.len() > 1 {
            self.quick_look.index_open = !self.quick_look.index_open;
            self.quick_look.info_open = false;
            cx.notify();
        }
    }

    pub(crate) fn sync_pinned_preview(&mut self, cx: &mut Context<Self>) {
        self.preview.debounce.task = None;
        self.start_plugin_scan(false, cx);
        if !self.settings.view.show_preview_panel
            && matches!(self.preview.state, PreviewState::Closed)
        {
            return;
        }
        let entries = self.effective_selected_entries();
        if entries.len() == 1 && !is_directory_entry(&entries[0]) {
            if self.browser.view_mode() == ViewMode::Column {
                self.column_view.scroll_to_leaf_attempts = 3;
            }
            self.start_preview(entries[0].path.clone(), cx);
        } else {
            self.services.previews.cancel_artifact();
            self.stop_media_preview(cx);
            self.preview.generation = self.preview.generation.wrapping_add(1);
            self.preview.task = None;
            self.preview.prefetch_tasks.clear();
            self.preview.detection = None;
            self.preview.state = PreviewState::Closed;
            self.preview.tab = PreviewTab::Preview;
            self.preview.custom_fields_editor = None;
            self.preview.photo_metadata = PhotoMetadataState::Unavailable;
            self.preview.photo_metadata_task = None;
            self.preview.photo_gps_revealed = false;
            self.preview.finder_tags.unavailable();
            self.preview.finder_tags_task = None;
            self.preview.finder_tags_generation =
                self.preview.finder_tags_generation.wrapping_add(1);
            cx.notify();
        }
    }

    pub(crate) fn clear_preview_cache(&mut self, cx: &mut Context<Self>) {
        if self.preview.cache_task.is_some() {
            self.status_message = Some("Preview cache is already being cleared".to_string());
            cx.notify();
            return;
        }
        let clear_generation = self.preview.generation;
        let cleared_path = preview_panel::cache_backed_preview_path(
            &self.preview.state,
            self.settings.behavior.preview_executable_scripts,
        )
        .map(Path::to_path_buf);
        let task = self.services.previews.clear_cache();
        self.status_message = Some("Clearing preview cache…".to_string());
        self.preview.cache_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.preview.cache_task = None;
                match result {
                    Ok(()) => {
                        view.clear_entry_visuals();
                        let reload_path = preview_panel::cache_reload_path(
                            &view.preview.state,
                            view.preview.generation,
                            clear_generation,
                            cleared_path.as_deref(),
                            view.settings.behavior.preview_executable_scripts,
                        );
                        if let Some(path) = reload_path {
                            view.status_message =
                                Some("Preview cache cleared; refreshing preview…".to_string());
                            view.start_preview(path, cx);
                        } else {
                            view.status_message = Some("Preview cache cleared".to_string());
                        }
                    }
                    Err(error) => {
                        view.status_message =
                            Some(format!("Unable to clear preview cache: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn set_preview_tab(&mut self, tab: PreviewTab, cx: &mut Context<Self>) {
        if tab == PreviewTab::CustomFields && self.preview.custom_fields_editor.is_none() {
            self.show_toast(
                "Custom fields are unavailable for this preview artifact",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        if self.preview.tab == PreviewTab::Preview && tab != PreviewTab::Preview {
            self.media
                .update(cx, |media, cx| media.pause_for_hidden_preview(cx));
        }
        self.preview.tab = tab;
        cx.notify();
    }

    pub(crate) fn reveal_previewed_item(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.state.path().map(Path::to_path_buf) else {
            return;
        };
        self.reveal_item(path, cx);
    }

    pub(crate) fn reveal_item(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let task = self.services.integration.reveal(path.clone());
        let generation = self.begin_integration_action(format!("Revealing {}…", path.display()));
        let task = cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                let status = Some(match result {
                    Ok(()) => format!("Revealed {}", path.display()),
                    Err(error) => {
                        view.record_error("Reveal failed", error.to_string());
                        format!("Unable to reveal {}: {error}", path.display())
                    }
                });
                if view.system.integration_generation == generation {
                    view.status_message = status;
                }
                cx.notify();
            });
        });
        self.push_integration_task(task);
        cx.notify();
    }

    pub(crate) fn open_previewed_with(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.state.path().map(Path::to_path_buf) else {
            return;
        };
        self.open_item_with(path, String::new(), cx);
    }

    pub(crate) fn open_item_with(
        &mut self,
        path: PathBuf,
        app_name: String,
        cx: &mut Context<Self>,
    ) {
        let task = self.services.integration.open_with(path.clone(), app_name);
        let generation =
            self.begin_integration_action(format!("Opening {} with another app…", path.display()));
        let task = cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                let status = match result {
                    Ok(()) => None,
                    Err(error) => {
                        view.record_error("Open With failed", error.to_string());
                        Some(format!(
                            "Unable to open {} with another app: {error}",
                            path.display()
                        ))
                    }
                };
                if view.system.integration_generation == generation {
                    view.status_message = status;
                }
                cx.notify();
            });
        });
        self.push_integration_task(task);
        cx.notify();
    }

    /// Open With ▸ Other…: let the user pick an application, then open
    /// `path` with it.
    pub(crate) fn open_item_with_chosen_app(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let task = self.services.integration.choose_application();
        let task = cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(Some(app)) => view.open_item_with(path, app.to_string_lossy().into_owned(), cx),
                Ok(None) => {}
                Err(error) => {
                    view.record_error("Open With failed", error.to_string());
                    view.status_message = Some(format!("Unable to choose an application: {error}"));
                    cx.notify();
                }
            });
        });
        self.push_integration_task(task);
    }

    pub(crate) fn quick_look_previewed_item(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.state.path().map(Path::to_path_buf) else {
            return;
        };
        let paths = self.preview_files_for(&path);
        self.open_quick_look(path, paths, cx);
    }

    pub(crate) fn is_plain_space(keystroke: &gpui::Keystroke) -> bool {
        keystroke.key == "space"
            && !keystroke.modifiers.control
            && !keystroke.modifiers.alt
            && !keystroke.modifiers.platform
            && !keystroke.modifiers.shift
    }

    pub(crate) fn handle_quick_look_space_down(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        if !self.quick_look.space_down {
            self.quick_look.space_down = true;
            self.quick_look.space_closes_on_release = self.quick_look.open;
            if !event.is_held && !self.quick_look.space_closes_on_release {
                self.toggle_quick_look_selected(cx);
            }
        }
        cx.stop_propagation();
    }

    pub(crate) fn handle_key_up(
        &mut self,
        event: &KeyUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if Self::is_plain_space(&event.keystroke) && self.quick_look.space_down {
            self.quick_look.space_down = false;
            let close_quick_look = std::mem::take(&mut self.quick_look.space_closes_on_release);
            if close_quick_look {
                self.close_quick_look(cx);
            }
            cx.stop_propagation();
        }
    }
}

/// The 1-based position of the first of `files` equal to `current`, the
/// number of files, and the files just before and after it; `(1, 1, None,
/// None)` when `current` is not among them.
fn preview_neighbors<'a>(
    files: impl Iterator<Item = &'a Path>,
    current: &Path,
) -> (usize, usize, Option<PathBuf>, Option<PathBuf>) {
    let mut total = 0;
    let mut found = None;
    let mut last = None;
    let mut next = None;
    for path in files {
        match found {
            None if path == current => found = Some((total, last)),
            None => last = Some(path),
            Some(_) if next.is_none() => next = Some(path),
            Some(_) => {}
        }
        total += 1;
    }
    match found {
        Some((index, previous)) => (
            index + 1,
            total,
            previous.map(Path::to_path_buf),
            next.map(Path::to_path_buf),
        ),
        None => (1, 1, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_neighbors_match_the_first_occurrence() {
        let files = ["a", "b", "c", "b", "d"].map(PathBuf::from);
        let neighbors = |current: &str| {
            preview_neighbors(files.iter().map(PathBuf::as_path), Path::new(current))
        };
        assert_eq!(neighbors("a"), (1, 5, None, Some(PathBuf::from("b"))));
        assert_eq!(
            neighbors("b"),
            (2, 5, Some(PathBuf::from("a")), Some(PathBuf::from("c")))
        );
        assert_eq!(neighbors("d"), (5, 5, Some(PathBuf::from("b")), None));
        assert_eq!(neighbors("missing"), (1, 1, None, None));
        assert_eq!(
            preview_neighbors(std::iter::empty(), Path::new("a")),
            (1, 1, None, None)
        );
    }
}
