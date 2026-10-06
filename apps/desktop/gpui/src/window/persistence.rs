//! `DirectoryWindow` behavior for persistence.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn persist_session(&mut self) {
        self.sync_active_tab_view_state();
        self.save_session_snapshot();
    }

    pub(crate) fn save_session_snapshot(&mut self) {
        let error = self
            .session_store
            .as_ref()
            .and_then(|store| store.save(&self.browser).err());
        if let Some(error) = error {
            self.record_error("Session save failed", &error);
            self.status_message = Some(format!("Unable to save session: {error}"));
        }
    }

    pub(crate) fn mutate_shared_session<R>(
        &mut self,
        mutation: impl FnOnce(&mut SharedSessionState) -> R,
    ) -> R {
        let (result, state) = if let Some(runtime) = self
            .window_lifetime
            .as_ref()
            .map(|lifetime| lifetime.runtime.clone())
        {
            runtime.mutate_session(&mut self.shared_state_revision, mutation)
        } else {
            let mut state = self.browser.shared_state();
            let result = mutation(&mut state);
            (result, state)
        };
        self.browser.apply_shared_state(state);
        result
    }

    pub(crate) fn sync_active_tab_view_state(&mut self) {
        let offset =
            (-f32::from(self.listing.scroll_handle.0.borrow().base_handle.offset().y)).max(0.0);
        let item_height = match self.browser.view_mode() {
            ViewMode::List => f32::from(self.settings.appearance.list_row_height).max(1.0),
            ViewMode::Grid => grid_layout_metrics(
                self.layout.listing_viewport_width,
                self.settings.appearance.grid_min_width,
                self.settings.appearance.density,
                self.settings.appearance.ui_scale,
            )
            .row_height
            .max(1.0),
            ViewMode::Column => 1.0,
        };
        let scroll_index = if self.browser.view_mode() == ViewMode::Column {
            0
        } else {
            (offset / item_height).floor() as usize
        };
        self.browser.sync_folder_ui_state(
            scroll_index,
            self.settings.appearance.grid_min_width,
            self.settings.view.show_preview_panel,
        );
    }

    pub(crate) fn apply_active_tab_view_state(&mut self) {
        let (scroll_index, grid_min_width, show_preview_panel) = self.browser.folder_ui_state();
        self.settings.view.view_mode = self.browser.view_mode();
        self.settings.view.sort_key = self.browser.sort_key();
        self.settings.view.sort_direction = self.browser.sort_direction();
        self.settings.appearance.grid_min_width = grid_min_width.clamp(120, 260);
        self.settings.view.show_preview_panel = show_preview_panel;
        self.listing.pending_scroll_restore = Some(scroll_index);
    }

    pub(crate) fn persist_window_placement(&mut self) {
        self.settings.window_placement.width = self.layout.last_window_bounds.width;
        self.settings.window_placement.height = self.layout.last_window_bounds.height;
        self.settings.window_placement.x = self.layout.last_window_bounds.x;
        self.settings.window_placement.y = self.layout.last_window_bounds.y;
        self.browser.set_window_placement(SessionWindowPlacement {
            width: self.layout.last_window_bounds.width,
            height: self.layout.last_window_bounds.height,
            x: self.layout.last_window_bounds.x,
            y: self.layout.last_window_bounds.y,
        });
        self.persist_settings();
        self.persist_session();
    }
}
