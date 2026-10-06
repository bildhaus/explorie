//! `DirectoryWindow` behavior for drag drop.

use std::cell::RefCell;

use crate::*;

impl DirectoryWindow {
    pub(crate) fn file_drag_for_entry(&self, entry: &FileEntry) -> FileDrag {
        let entries = if self.is_effectively_selected(&entry.path) {
            let selected = self.effective_selected_entries();
            if selected.is_empty() {
                vec![entry.clone()]
            } else {
                selected
            }
        } else {
            vec![entry.clone()]
        };
        FileDrag::from_entries(entries)
    }

    /// Fill a deferred Column view drag when it starts rather than building
    /// the selected rows' payload on every frame. Like the list and grid,
    /// it uses the selection the mouse-down saw.
    pub(crate) fn initialize_column_file_drag(&self, drag: &FileDrag, source: &FileEntry) {
        if drag.items.get().is_some() {
            return;
        }
        let built = match self.pointer.file_drag_selection.as_deref() {
            Some(selection) if selection.contains(&source.path) => {
                let mut entries = Vec::new();
                for column in self.column_view.columns.columns() {
                    entries.extend(
                        column
                            .visible_entries(&self.browser)
                            .iter()
                            .filter(|entry| selection.contains(&entry.path))
                            .map(|entry| entry.as_ref().clone()),
                    );
                }
                if entries.is_empty() {
                    entries.push(source.clone());
                }
                FileDrag::from_entries(entries)
            }
            Some(_) => FileDrag::from_entries(vec![source.clone()]),
            None => self.file_drag_for_entry(source),
        };
        let _ = drag.items.set(built.items().to_vec());
    }

    pub(crate) fn track_file_drag(&mut self, drag: &FileDrag, cx: &mut Context<Self>) {
        let sources: BTreeSet<_> = drag.items().iter().map(|item| item.path.clone()).collect();
        if self.pointer.file_drag_sources != sources {
            self.pointer.file_drag_sources = sources;
            cx.notify();
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    pub(crate) fn maybe_start_external_file_drag(
        &mut self,
        drag: &FileDrag,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = window.viewport_size();
        let x = f32::from(position.x);
        let y = f32::from(position.y);
        let width = f32::from(viewport.width);
        let height = f32::from(viewport.height);
        if x > 1.0 && y > 1.0 && x < width - 1.0 && y < height - 1.0 {
            return;
        }
        if drag
            .native_export_attempted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let paths = drag
            .items()
            .iter()
            .map(|item| item.path.clone())
            .collect::<Vec<_>>();
        let preview = drag::Image::Raw(
            include_bytes!(concat!(env!("OUT_DIR"), "/titlebar-icon.png")).to_vec(),
        );
        if let Err(error) = drag::start_drag(
            window,
            drag::DragItem::Files(paths),
            preview,
            |_, _| {},
            drag::Options::default(),
        ) {
            drag.native_export_attempted.store(false, Ordering::Release);
            self.show_toast(
                format!("Unable to start system drag: {error}"),
                ToastKind::Warning,
                cx,
            );
        } else {
            self.pointer.file_drag_hover_target = None;
            self.pointer.file_drag_hover_task = None;
            self.pointer.file_drag_sources.clear();
            cx.notify();
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub(crate) fn maybe_start_external_file_drag(
        &mut self,
        _drag: &FileDrag,
        _position: gpui::Point<gpui::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    pub(crate) fn start_file_drag_hover(
        &mut self,
        target: FileDragHoverTarget,
        cx: &mut Context<Self>,
    ) {
        if self.pointer.file_drag_hover_target.as_ref() == Some(&target) {
            return;
        }
        self.pointer.file_drag_hover_task = None;
        self.pointer.file_drag_hover_target = Some(target.clone());
        let executor = cx.background_executor().clone();
        self.pointer.file_drag_hover_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(700)).await;
            let _ = this.update(cx, |view, cx| {
                if view.pointer.file_drag_hover_target.as_ref() != Some(&target) {
                    return;
                }
                view.pointer.file_drag_hover_task = None;
                match target {
                    FileDragHoverTarget::Folder(path) => view.navigate_to(path, cx),
                    FileDragHoverTarget::Tab(id) => view.activate_tab(id, cx),
                }
            });
        }));
    }

    pub(crate) fn hover_file_drag_folder(
        &mut self,
        drag: &FileDrag,
        target: PathBuf,
        target_is_link: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.track_file_drag(drag, cx);
        if target_is_link || !valid_file_drop_target(&target, drag, file_drag_operation(window)) {
            self.cancel_file_drag_hover(&FileDragHoverTarget::Folder(target));
            return;
        }
        self.start_file_drag_hover(FileDragHoverTarget::Folder(target), cx);
    }

    pub(crate) fn hover_file_drag_tab(
        &mut self,
        drag: &FileDrag,
        id: TabId,
        cx: &mut Context<Self>,
    ) {
        self.track_file_drag(drag, cx);
        self.start_file_drag_hover(FileDragHoverTarget::Tab(id), cx);
    }

    pub(crate) fn cancel_file_drag_hover(&mut self, target: &FileDragHoverTarget) {
        if self.pointer.file_drag_hover_target.as_ref() == Some(target) {
            self.pointer.file_drag_hover_target = None;
            self.pointer.file_drag_hover_task = None;
        }
    }

    pub(crate) fn finish_file_drag(&mut self, cx: &mut Context<Self>) {
        self.pointer.file_drag_selection = None;
        let changed = !self.pointer.file_drag_sources.is_empty()
            || self.pointer.file_drag_hover_target.is_some()
            || self.pointer.file_drag_hover_task.is_some();
        self.pointer.file_drag_sources.clear();
        self.pointer.file_drag_hover_target = None;
        self.pointer.file_drag_hover_task = None;
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn drop_files_to(
        &mut self,
        drag: &FileDrag,
        target: PathBuf,
        target_is_link: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let kind = file_drag_operation(window);
        let target = operation_destination(&target);
        if target_is_link || !valid_file_drop_target(&target, drag, kind) {
            let target_key = comparable_drop_path(&target);
            if drag.items().iter().any(|item| {
                let source = comparable_drop_path(&item.path);
                item.is_dir
                    && (target_key == source || target_key.starts_with(&format!("{source}/")))
            }) {
                self.show_toast(
                    "A folder cannot be dropped into itself",
                    ToastKind::Warning,
                    cx,
                );
            } else if target_is_link {
                self.show_toast(
                    "Dropping into links and junctions is disabled",
                    ToastKind::Warning,
                    cx,
                );
            }
            self.finish_file_drag(cx);
            return;
        }

        let target_key = comparable_drop_path(&target);
        let sources = drag
            .items()
            .iter()
            .filter(|item| {
                kind != FileOperationKind::Move
                    || item
                        .path
                        .parent()
                        .is_none_or(|parent| comparable_drop_path(parent) != target_key)
            })
            .map(|item| item.path.clone())
            .collect::<Vec<_>>();
        if !sources.is_empty() {
            self.start_file_operation(
                FileOperationRequest {
                    kind,
                    sources,
                    destination: Some(target),
                    conflict_policy: self.operation_ui.conflict_policy,
                },
                cx,
            );
        }
        self.finish_file_drag(cx);
    }

    /// Copy dropped external paths into `target`. Checking which paths exist
    /// and which are folders touches the filesystem (possibly a slow or
    /// offline volume), so it runs off the UI thread.
    pub(crate) fn drop_external_paths_to(
        &mut self,
        paths: &ExternalPaths,
        target: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let dropped = paths.paths().to_vec();
        let target = operation_destination(&target);
        let destination = target.clone();
        let sources = cx.background_spawn(async move { external_drop_sources(dropped, &target) });
        cx.spawn(async move |this, cx| {
            let sources = sources.await;
            let _ = this.update(cx, |view, cx| {
                if sources.is_empty() {
                    view.show_toast(
                        "Nothing can be copied to this folder",
                        ToastKind::Warning,
                        cx,
                    );
                    return;
                }
                view.start_file_operation(
                    FileOperationRequest {
                        kind: FileOperationKind::Copy,
                        sources,
                        destination: Some(destination),
                        conflict_policy: view.operation_ui.conflict_policy,
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    pub(crate) fn drop_files_to_favorites(&mut self, drag: &FileDrag, cx: &mut Context<Self>) {
        let paths = drag
            .items()
            .iter()
            .filter(|item| item.is_dir)
            .map(|item| item.path.clone())
            .collect::<Vec<_>>();
        let added = self.mutate_shared_session(|session| {
            paths
                .into_iter()
                .filter(|path| session.add_favorite(path.clone()))
                .count()
        });
        if added > 0 {
            self.persist_session();
            self.show_toast(
                format!(
                    "Added {added} folder{} to Favorites",
                    if added == 1 { "" } else { "s" }
                ),
                ToastKind::Success,
                cx,
            );
        } else {
            self.show_toast(
                "Only new folders can be added to Favorites",
                ToastKind::Warning,
                cx,
            );
        }
        self.finish_file_drag(cx);
    }

    pub(crate) fn drop_external_paths_to_favorites(
        &mut self,
        paths: &ExternalPaths,
        cx: &mut Context<Self>,
    ) {
        let dropped = paths.paths().to_vec();
        let folders = cx.background_spawn(async move {
            dropped
                .into_iter()
                .filter(|path| path.is_dir())
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let folders = folders.await;
            let _ = this.update(cx, |view, cx| view.add_dropped_favorites(folders, cx));
        })
        .detach();
    }

    fn add_dropped_favorites(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let added = self.mutate_shared_session(|session| {
            paths
                .into_iter()
                .filter(|path| session.add_favorite(path.clone()))
                .count()
        });
        if added > 0 {
            self.persist_session();
            self.show_toast(
                format!(
                    "Added {added} folder{} to Favorites",
                    if added == 1 { "" } else { "s" }
                ),
                ToastKind::Success,
                cx,
            );
        } else {
            self.show_toast(
                "Only new folders can be added to Favorites",
                ToastKind::Warning,
                cx,
            );
        }
    }

    pub(crate) fn reorder_favorite_to(
        &mut self,
        path: &Path,
        target_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = path.to_path_buf();
        if self.mutate_shared_session(|session| session.reorder_favorite(&path, target_index)) {
            self.persist_session();
            if let Some(handle) = self.favorite_focus_handles.get(target_index) {
                window.focus(handle, cx);
            }
            self.status_message = Some("Favorite order updated".to_string());
            cx.notify();
        }
    }

    pub(crate) fn move_favorite_at(
        &mut self,
        index: usize,
        offset: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.browser.favorites().len();
        if count == 0 {
            return;
        }
        let target = index.saturating_add_signed(offset).min(count - 1);
        if target != index
            && let Some(path) = self
                .browser
                .favorites()
                .get(index)
                .map(|favorite| favorite.path().to_path_buf())
        {
            self.reorder_favorite_to(&path, target, window, cx);
        }
    }

    pub(crate) fn auto_scroll_file_drag_handle(
        handle: &ScrollHandle,
        event: &gpui::DragMoveEvent<FileDrag>,
    ) {
        let pointer_y = f32::from(event.event.position.y) - f32::from(event.bounds.origin.y);
        let delta = file_drag_scroll_delta(pointer_y, f32::from(event.bounds.size.height));
        if delta == 0.0 {
            return;
        }
        let offset = handle.offset();
        let max_offset = f32::from(handle.max_offset().y);
        let next_y = (f32::from(offset.y) + delta).clamp(-max_offset, 0.0);
        if next_y != f32::from(offset.y) {
            handle.set_offset(gpui::point(offset.x, px(next_y)));
        }
    }

    pub(crate) fn auto_scroll_primary_file_drag(&self, event: &gpui::DragMoveEvent<FileDrag>) {
        let handle = self.listing.scroll_handle.0.borrow().base_handle.clone();
        Self::auto_scroll_file_drag_handle(&handle, event);
    }

    pub(crate) fn auto_scroll_column_file_drag(
        &self,
        column_index: usize,
        event: &gpui::DragMoveEvent<FileDrag>,
    ) {
        if let Some(handle) = self.column_view.scroll_handles.get(column_index) {
            let handle = handle.0.borrow().base_handle.clone();
            Self::auto_scroll_file_drag_handle(&handle, event);
        }
    }
}

/// The dropped external paths that can be copied into `target`: they still
/// exist, are not already in it, and are not `target` or one of its
/// ancestors.
fn external_drop_sources(paths: Vec<PathBuf>, target: &Path) -> Vec<PathBuf> {
    let target_key = comparable_drop_path(target);
    paths
        .into_iter()
        .filter(|path| path.exists())
        .filter(|path| {
            path.parent()
                .is_none_or(|parent| comparable_drop_path(parent) != target_key)
        })
        .filter(|path| {
            let source = comparable_drop_path(path);
            !path.is_dir()
                || (target_key != source && !target_key.starts_with(&format!("{source}/")))
        })
        .collect()
}

/// Whether an external drag carries a folder. Hover styling asks on every
/// frame while a drag is over a target, so the answer is computed once per
/// distinct set of dragged paths.
pub(crate) fn external_paths_include_directory(paths: &ExternalPaths) -> bool {
    thread_local! {
        static LAST_DRAG: RefCell<Option<(Vec<PathBuf>, bool)>> = const { RefCell::new(None) };
    }
    LAST_DRAG.with(|last| {
        let mut last = last.borrow_mut();
        if let Some((dragged, includes_directory)) = last.as_ref()
            && dragged.as_slice() == paths.paths()
        {
            return *includes_directory;
        }
        let includes_directory = paths.paths().iter().any(|path| path.is_dir());
        *last = Some((paths.paths().to_vec(), includes_directory));
        includes_directory
    })
}

/// The directory a copy or move into `path` should target. File operations
/// refuse link destinations (they never follow links), so a link to a folder,
/// such as a link row or a folder browsed through its link, is resolved to
/// the folder it points at; the operation then validates that real folder
/// like any other destination.
pub(crate) fn operation_destination(path: &Path) -> PathBuf {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::canonicalize(path)
            .ok()
            .filter(|target| target.is_dir())
            .unwrap_or_else(|| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_drops_skip_missing_same_folder_and_self_containing_paths() {
        use std::fs;

        let root = std::env::temp_dir().join(format!("explorie-drop-{}", uuid::Uuid::new_v4()));
        let target = root.join("target");
        let outside = root.join("outside");
        fs::create_dir_all(target.join("inner")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("file.txt"), "file").unwrap();
        fs::write(target.join("already.txt"), "here").unwrap();

        let sources = external_drop_sources(
            vec![
                outside.join("file.txt"),
                outside.clone(),
                root.join("missing.txt"),
                target.join("already.txt"),
                target.join("inner"),
                target.clone(),
                root.clone(),
            ],
            &target,
        );
        assert_eq!(sources, vec![outside.join("file.txt"), outside.clone()]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_drag_folder_checks_run_once_per_drag() {
        use std::fs;

        let root = std::env::temp_dir().join(format!("explorie-hover-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("folder")).unwrap();
        fs::write(root.join("file.txt"), "file").unwrap();
        let files = ExternalPaths(vec![root.join("file.txt")].into());
        let folder = ExternalPaths(vec![root.join("file.txt"), root.join("folder")].into());
        assert!(!external_paths_include_directory(&files));
        assert!(external_paths_include_directory(&folder));
        // Later hover frames of the same drag reuse the answer without
        // touching the filesystem again.
        fs::remove_dir(root.join("folder")).unwrap();
        assert!(external_paths_include_directory(&folder));
        assert!(!external_paths_include_directory(&files));
        fs::remove_dir_all(root).unwrap();
    }
}
