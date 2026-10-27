//! `DirectoryWindow` behavior for watcher.

use crate::*;

/// The filesystem watcher for the shown folders: its status, the task reading
/// its events, and the refreshes and in-place patches it has requested.
pub(crate) struct WatcherUi {
    pub(crate) status: WatchStatus,
    pub(crate) task: Option<Task<()>>,
    pub(crate) generation: u64,
    pub(crate) refresh_pending: bool,
    pub(crate) patch_task: Option<Task<()>>,
    pub(crate) patch_pending: Vec<PathBuf>,
}

impl Default for WatcherUi {
    fn default() -> Self {
        Self {
            status: WatchStatus::Starting,
            task: None,
            generation: 0,
            refresh_pending: false,
            patch_task: None,
            patch_pending: Vec::new(),
        }
    }
}

impl DirectoryWindow {
    pub(crate) fn finish_watcher_refresh(&mut self, cx: &mut Context<Self>) {
        if self.watcher.refresh_pending && !self.listing_in_flight() {
            self.watcher.refresh_pending = false;
            self.refresh(cx);
        }
    }

    pub fn start_watching(&mut self, cx: &mut Context<Self>) {
        self.watcher.generation = self.watcher.generation.wrapping_add(1);
        let generation = self.watcher.generation;
        let watched_paths = self.watched_paths();
        let watcher = self.services.watcher.clone();
        let recursive = self
            .listing_search_criteria()
            .is_some_and(|criteria| criteria.recursive);
        self.watcher.status = WatchStatus::Starting;

        self.watcher.task = Some(cx.spawn(async move |this, cx| {
            let watch_task = if recursive {
                watcher.watch_recursive_task(watched_paths.clone())
            } else {
                watcher.watch_task(watched_paths.clone())
            };
            let subscription = match watch_task.await {
                Ok(subscription) => subscription,
                Err(error) => {
                    let _ = this.update(cx, |view, cx| {
                        if view.watcher_matches(generation, &watched_paths) {
                            view.record_error("Filesystem watcher failed", error.to_string());
                            view.watcher.status = WatchStatus::Unavailable(error.to_string());
                            cx.notify();
                        }
                    });
                    return;
                }
            };

            let active = this
                .update(cx, |view, cx| {
                    if view.watcher_matches(generation, &watched_paths) {
                        view.watcher.status = WatchStatus::Watching;
                        cx.notify();
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if !active {
                return;
            }

            while let Some(event) = subscription.next().await {
                let keep_watching = this
                    .update(cx, |view, cx| {
                        view.apply_watcher_event(generation, &watched_paths, event, cx)
                    })
                    .unwrap_or(false);
                if !keep_watching {
                    break;
                }
            }
        }));
    }

    pub(crate) fn apply_watcher_event(
        &mut self,
        generation: u64,
        watched_paths: &[PathBuf],
        event: WatcherEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        match watcher_disposition(
            self.watcher.generation,
            &self.watched_paths(),
            generation,
            watched_paths,
            &event,
        ) {
            WatcherDisposition::Refresh => {
                self.watcher.status = WatchStatus::Watching;
                self.invalidate_changed_preview(&event.paths);
                self.services.search.invalidate(&event.paths);
                let smart_folder = self.listing_shows_search_results();
                // Let the current listing finish before refreshing. Restarting it for
                // every filesystem event can starve slow folders indefinitely.
                // Search invalidation cancels the active search, so restart it now.
                if !smart_folder && self.listing_in_flight() {
                    self.watcher.refresh_pending = true;
                } else if smart_folder || !self.queue_watcher_patch(event.paths, cx) {
                    self.refresh(cx);
                }
                cx.notify();
                true
            }
            WatcherDisposition::Stop(error) => {
                self.watcher.status = WatchStatus::Unavailable(error);
                cx.notify();
                false
            }
            WatcherDisposition::Ignore => false,
        }
    }

    /// Re-read just the changed paths off the UI thread and patch the listed
    /// entries, instead of re-listing every folder. Returns false when only a
    /// full refresh is safe; see [`plan_watcher_patch`].
    pub(crate) fn queue_watcher_patch(
        &mut self,
        changed: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.watcher.patch_task.is_some() {
            // Apply patches in order: collect paths until this one lands.
            self.watcher.patch_pending.extend(changed);
            return true;
        }
        // A folder that failed to list has nothing to patch; re-list it.
        if !matches!(self.listing.state, ListingState::Ready)
            || (self.browser.view_mode() == ViewMode::Column
                && self
                    .column_view
                    .columns
                    .columns()
                    .iter()
                    .any(|column| column.error().is_some()))
        {
            return false;
        }
        let listed = self.watched_paths();
        let Some(plan) = plan_watcher_patch(&listed, &changed, self.calculate_folder_sizes) else {
            return false;
        };
        let guard = self.watcher_patch_guard();
        let calc_dir_size = self.calculate_folder_sizes;
        let stat = cx.background_spawn(async move {
            plan.into_iter()
                .map(|(folder, children)| {
                    let children: Vec<_> = children.into_iter().collect();
                    explorie_core::stat_entries(&folder, &children, calc_dir_size)
                        .map(|changes| (folder, changes))
                })
                .collect::<std::io::Result<Vec<_>>>()
        });
        self.watcher.patch_task = Some(cx.spawn(async move |this, cx| {
            let result = stat.await;
            let _ = this.update(cx, |view, cx| view.finish_watcher_patch(guard, result, cx));
        }));
        true
    }

    /// Drop an in-flight patch and queued paths; a new listing covers them.
    pub(crate) fn cancel_watcher_patch(&mut self) {
        self.watcher.refresh_pending = false;
        self.watcher.patch_task = None;
        self.watcher.patch_pending.clear();
    }

    fn watcher_patch_guard(&self) -> WatcherPatchGuard {
        WatcherPatchGuard {
            watcher_generation: self.watcher.generation,
            request_generation: self.listing.generation,
            column_generation: self.column_view.generation,
            path: self.browser.path().to_path_buf(),
            view_mode: self.browser.view_mode(),
        }
    }

    pub(crate) fn finish_watcher_patch(
        &mut self,
        guard: WatcherPatchGuard,
        result: std::io::Result<Vec<FolderChanges>>,
        cx: &mut Context<Self>,
    ) {
        self.watcher.patch_task = None;
        if guard != self.watcher_patch_guard() || self.listing_shows_search_results() {
            // A newer listing, tab, folder or view owns the entries now.
            self.watcher.patch_pending.clear();
            return;
        }
        let Ok(folders) = result else {
            self.watcher.patch_pending.clear();
            self.refresh(cx);
            return;
        };
        let column_view = self.browser.view_mode() == ViewMode::Column;
        let mut active_changed = false;
        for (folder, changes) in folders {
            let active = folder == self.browser.path();
            if column_view {
                if active {
                    active_changed |= self.browser.apply_entry_changes(changes.clone());
                }
                self.column_view.columns.apply_changes(&folder, changes);
            } else if active {
                active_changed |= self.browser.apply_entry_changes(changes);
            }
        }
        if active_changed {
            self.start_plugin_scan(true, cx);
        }
        self.finish_pending_preview_refresh(cx);
        let pending = std::mem::take(&mut self.watcher.patch_pending);
        if !pending.is_empty() && !self.queue_watcher_patch(pending, cx) {
            self.refresh(cx);
        }
        cx.notify();
    }

    pub(crate) fn invalidate_changed_preview(&mut self, changed_paths: &[PathBuf]) {
        let affects = |candidate: &Path| {
            changed_paths
                .iter()
                .any(|changed| candidate == changed || candidate.starts_with(changed))
        };
        if let Some(path) = self.preview.state.path().filter(|path| affects(path)) {
            self.preview.pending_refresh = Some(path.to_path_buf());
        }
        self.visuals.thumbnails.retain(|key, _| !affects(&key.path));
        self.visuals
            .thumbnail_queue
            .retain(|request| !affects(&request.path));
        self.visuals
            .icons
            .retain(|key, _| !matches!(key, EntryIconKey::Source { path, .. } if affects(path)));
        self.visuals
            .icon_queue
            .retain(|request| !affects(&request.path));
    }

    pub(crate) fn finish_pending_preview_refresh(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.pending_refresh.take() else {
            if self.settings.view.show_preview_panel
                && matches!(self.preview.state, PreviewState::Closed)
            {
                self.sync_pinned_preview(cx);
            }
            return;
        };
        if self.preview.state.path() != Some(path.as_path()) {
            return;
        }
        let remains_visible = self
            .browser
            .visible_entries()
            .iter()
            .any(|entry| entry.path == path && !is_directory_entry(entry));
        if remains_visible {
            self.start_preview(path, cx);
        } else {
            self.close_preview(cx);
        }
    }

    pub(crate) fn watched_paths(&self) -> Vec<PathBuf> {
        if let Some(criteria) = self.listing_search_criteria() {
            criteria.search_paths
        } else if self.browser.view_mode() == ViewMode::Column {
            self.column_view.columns.paths()
        } else {
            vec![self.browser.path().to_path_buf()]
        }
    }

    pub(crate) fn watcher_matches(&self, generation: u64, watched_paths: &[PathBuf]) -> bool {
        self.watcher.generation == generation && self.watched_paths() == watched_paths
    }

    pub(crate) fn path_did_change(&mut self, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        self.close_preview(cx);
        self.navigation_ui.breadcrumb_editor = None;
        // Subfolder results belong to the folder (and tab) being left.
        self.end_subfolder_search(false, cx);
        self.browser.show_search_results(false);
        let leaving_smart_folder = self.browser.active_smart_folder().is_some();
        self.browser.clear_active_smart_folder();
        if leaving_smart_folder {
            self.apply_global_browser_preferences();
            self.browser.clear_search();
            self.search.active = false;
        }
        self.apply_active_tab_view_state();
        self.services.search.cancel();
        self.search.generation = self.search.generation.wrapping_add(1);
        self.search.task = None;
        self.search.request_id = None;
        self.search.progress = None;
        let path = self.browser.path().to_path_buf();
        self.mutate_shared_session(|session| session.record_current_path(path));
        self.save_session_snapshot();
        self.start_disk_info(cx);
        self.clear_plugin_context(cx);
        if self.browser.view_mode() == ViewMode::Column {
            self.listing.state = ListingState::Loading;
            self.start_column_listings(true, cx);
        } else {
            self.start_listing_with_scroll_reset(false, cx);
        }
        self.start_watching(cx);
    }
}

/// A listed folder and its re-read children (`None` for paths now gone).
type FolderChanges = (PathBuf, Vec<(PathBuf, Option<FileEntry>)>);

/// What a watcher patch was planned against; results for anything else are
/// stale and dropped.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WatcherPatchGuard {
    watcher_generation: u64,
    request_generation: u64,
    column_generation: u64,
    path: PathBuf,
    view_mode: ViewMode,
}

/// Group changed paths by the listed folder whose rows they change, as the
/// children to re-read in each folder. Returns `None` when only a full
/// refresh is safe: a listed folder or one of its ancestors changed (which is
/// also how the watcher reports overflow and rescans), a path is outside
/// every listed folder, or a folder's `.explorie.json` changed.
///
/// In Column view a listed folder is also a row in its parent's column, so
/// the parent re-reads it too (its modified time moves as children change);
/// with folder sizes on, every listed ancestor is re-measured.
pub(crate) fn plan_watcher_patch(
    listed: &[PathBuf],
    changed: &[PathBuf],
    folder_sizes: bool,
) -> Option<BTreeMap<PathBuf, BTreeSet<PathBuf>>> {
    let mut plan: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    for path in changed {
        if listed.iter().any(|folder| folder.starts_with(path)) {
            return None;
        }
        let folder = listed
            .iter()
            .filter(|folder| path.starts_with(folder))
            .max_by_key(|folder| folder.components().count())?;
        let child = folder.join(path.strip_prefix(folder).ok()?.components().next()?);
        if child.file_name() == Some(std::ffi::OsStr::new(".explorie.json")) {
            return None;
        }
        plan.entry(folder.clone()).or_default().insert(child);
        let mut row = folder.as_path();
        while let Some(parent) = row.parent()
            && listed.iter().any(|listed| listed == parent)
        {
            plan.entry(parent.to_path_buf())
                .or_default()
                .insert(row.to_path_buf());
            if !folder_sizes {
                break;
            }
            row = parent;
        }
    }
    (!plan.is_empty()).then_some(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Plan = Vec<(PathBuf, Vec<PathBuf>)>;

    fn plan(listed: &[&str], changed: &[&str], folder_sizes: bool) -> Option<Plan> {
        let listed: Vec<_> = listed.iter().map(PathBuf::from).collect();
        let changed: Vec<_> = changed.iter().map(PathBuf::from).collect();
        plan_watcher_patch(&listed, &changed, folder_sizes).map(|plan| {
            plan.into_iter()
                .map(|(folder, children)| (folder, children.into_iter().collect()))
                .collect()
        })
    }

    /// Paths compare by component, so these also match Windows separators.
    fn expected(groups: &[(&str, &[&str])]) -> Option<Plan> {
        Some(
            groups
                .iter()
                .map(|(folder, children)| {
                    (
                        PathBuf::from(folder),
                        children.iter().map(PathBuf::from).collect(),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn direct_children_are_grouped_by_their_listed_folder() {
        assert_eq!(
            plan(
                &["/root"],
                &["/root/b.txt", "/root/a.txt", "/root/a.txt"],
                false
            ),
            expected(&[("/root", &["/root/a.txt", "/root/b.txt"])])
        );
        // A nested report re-reads the child folder that contains it.
        assert_eq!(
            plan(&["/root"], &["/root/folder/deep/file"], true),
            expected(&[("/root", &["/root/folder"])])
        );
    }

    #[test]
    fn root_ancestor_foreign_and_metadata_changes_need_a_full_refresh() {
        // The watcher reports overflow and rescans as the watched roots.
        assert_eq!(plan(&["/root"], &["/root/a.txt", "/root"], false), None);
        assert_eq!(plan(&["/root/child"], &["/root"], false), None);
        assert_eq!(plan(&["/root"], &["/elsewhere/a.txt"], false), None);
        assert_eq!(plan(&["/root"], &["/root/.explorie.json"], false), None);
        assert_eq!(plan(&["/root"], &[], false), None);
    }

    #[test]
    fn column_parents_re_read_the_listed_folder_rows_above_a_change() {
        let listed = ["/a", "/a/b", "/a/b/c"];
        assert_eq!(
            plan(&listed, &["/a/b/c/new.txt"], false),
            expected(&[("/a/b", &["/a/b/c"]), ("/a/b/c", &["/a/b/c/new.txt"])])
        );
        assert_eq!(
            plan(&listed, &["/a/b/c/new.txt"], true),
            expected(&[
                ("/a", &["/a/b"]),
                ("/a/b", &["/a/b/c"]),
                ("/a/b/c", &["/a/b/c/new.txt"]),
            ])
        );
        // A sibling of the open branch only patches the columns that list it
        // or its folder.
        assert_eq!(
            plan(&listed, &["/a/b/sibling"], false),
            expected(&[("/a", &["/a/b"]), ("/a/b", &["/a/b/sibling"])])
        );
        // The open folder itself changing (renamed, deleted) re-lists.
        assert_eq!(plan(&listed, &["/a/b/c"], false), None);
    }
}
