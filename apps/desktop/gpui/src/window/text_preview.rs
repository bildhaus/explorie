//! `DirectoryWindow` behavior for text preview.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn text_preview_match_count(&self) -> usize {
        let PreviewState::Ready {
            content: PreviewContent::Text(preview),
            ..
        } = &self.preview.state
        else {
            return 0;
        };
        self.preview.text.find_matches(preview).len()
    }

    pub(crate) fn open_text_preview_find(&mut self, cx: &mut Context<Self>) {
        self.activate_native_text_input(
            TextInputTarget::PreviewFind,
            self.preview.text.find.clone(),
            "Find in preview…",
            "Find in text preview",
            cx,
        );
    }

    pub(crate) fn advance_text_preview_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.text_preview_match_count();
        if count == 0 {
            self.preview.text.find_match = 0;
        } else {
            self.preview.text.find_match =
                (self.preview.text.find_match as isize + delta).rem_euclid(count as isize) as usize;
        }
        self.scroll_to_current_text_preview_match();
        cx.notify();
    }

    pub(crate) fn scroll_to_current_text_preview_match(&self) {
        let PreviewState::Ready {
            content: PreviewContent::Text(preview),
            ..
        } = &self.preview.state
        else {
            return;
        };
        let matches = self.preview.text.find_matches(preview);
        let Some(found) = matches.get(self.preview.text.find_match) else {
            return;
        };
        let line = preview.line_for_offset(found.start);
        let wrapped = self.preview.text.wrap_override.unwrap_or(preview.wrapped);
        if wrapped {
            self.preview.text.wrapped_list.scroll_to_reveal_item(line);
        } else {
            self.preview
                .text
                .unwrapped_scroll
                .scroll_to_item(line, ScrollStrategy::Center);
        }
    }

    pub(crate) fn copy_text_preview(&mut self, cx: &mut Context<Self>) {
        if let PreviewState::Ready {
            content: PreviewContent::Text(preview),
            ..
        } = &self.preview.state
        {
            cx.write_to_clipboard(ClipboardItem::new_string(preview.text.to_string()));
            self.show_toast("Preview text copied", ToastKind::Success, cx);
        }
    }

    pub(crate) fn toggle_text_preview_wrap(
        &mut self,
        default_wrapped: bool,
        cx: &mut Context<Self>,
    ) {
        let wrapped = !self.preview.text.wrap_override.unwrap_or(default_wrapped);
        self.preview.text.wrap_override = Some(wrapped);
        self.preview.text.wrapped_list.remeasure();
        self.scroll_to_current_text_preview_match();
        cx.notify();
    }
}
