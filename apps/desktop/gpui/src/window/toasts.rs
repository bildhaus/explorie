//! `DirectoryWindow` behavior for toasts.

use crate::*;

/// The toast on screen, the ones waiting behind it, and the timer that
/// dismisses it.
#[derive(Default)]
pub(crate) struct ToastQueue {
    pub(crate) current: Option<ToastNotice>,
    pub(crate) queue: VecDeque<ToastNotice>,
    pub(crate) task: Option<Task<()>>,
    pub(crate) generation: u64,
}

impl DirectoryWindow {
    pub(crate) fn show_toast(
        &mut self,
        message: impl Into<String>,
        kind: ToastKind,
        cx: &mut Context<Self>,
    ) {
        let notice = ToastNotice {
            message: message.into(),
            kind,
        };
        if self.toasts.current.as_ref() == Some(&notice) {
            self.schedule_toast_dismiss(kind, cx);
            cx.notify();
            return;
        }
        if notice.kind == ToastKind::Warning
            && self
                .toasts
                .current
                .as_ref()
                .is_some_and(|toast| toast.kind == ToastKind::Success)
        {
            self.toasts.current = Some(notice);
            self.schedule_toast_dismiss(ToastKind::Warning, cx);
            cx.notify();
            return;
        }
        if self.toasts.current.is_some() {
            if !self.toasts.queue.contains(&notice) {
                self.toasts.queue.push_back(notice);
                self.toasts.queue.truncate(4);
            }
        } else {
            self.toasts.current = Some(notice);
            self.schedule_toast_dismiss(kind, cx);
        }
        cx.notify();
    }

    pub(crate) fn schedule_toast_dismiss(&mut self, kind: ToastKind, cx: &mut Context<Self>) {
        self.toasts.generation = self.toasts.generation.wrapping_add(1);
        let generation = self.toasts.generation;
        let duration = match kind {
            ToastKind::Success => Duration::from_secs(4),
            ToastKind::Warning => Duration::from_secs(9),
        };
        let executor = cx.background_executor().clone();
        self.toasts.task = Some(cx.spawn(async move |this, cx| {
            executor.timer(duration).await;
            let _ = this.update(cx, |view, cx| {
                if view.toasts.generation == generation {
                    view.dismiss_toast(cx);
                }
            });
        }));
    }

    pub(crate) fn pause_toast_dismissal(&mut self, paused: bool, cx: &mut Context<Self>) {
        if paused {
            self.toasts.generation = self.toasts.generation.wrapping_add(1);
            self.toasts.task = None;
        } else if let Some(kind) = self.toasts.current.as_ref().map(|toast| toast.kind) {
            self.schedule_toast_dismiss(kind, cx);
        }
    }

    pub(crate) fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        if self.toasts.current.take().is_some() {
            self.toasts.generation = self.toasts.generation.wrapping_add(1);
            self.toasts.task = None;
            self.toasts.current = self.toasts.queue.pop_front();
            if let Some(kind) = self.toasts.current.as_ref().map(|toast| toast.kind) {
                self.schedule_toast_dismiss(kind, cx);
            }
            cx.notify();
        }
    }

    pub(crate) fn render_toast(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(toast) = self.toasts.current.clone() else {
            return div().into_any_element();
        };
        let color = match toast.kind {
            ToastKind::Success => rgb(0x4fa96c),
            ToastKind::Warning => rgb(0xe08a3e),
        };
        div()
            .id("native-toast")
            .debug_selector(|| "native-toast".to_string())
            .role(Role::Alert)
            .aria_label(toast.message.clone())
            .absolute()
            .right(px(16.0 * self.palette.scale))
            .top(px(48.0 * self.palette.scale))
            .flex()
            .items_center()
            .gap_3()
            .min_w(px(280.0 * self.palette.scale))
            .max_w(px(400.0 * self.palette.scale))
            .px_4()
            .py_3()
            .rounded(px(self.palette.radius))
            .border_1()
            .border_color(color)
            .bg(self.palette.panel)
            .shadow_lg()
            .on_hover(cx.listener(|this, hovered, _, cx| this.pause_toast_dismissal(*hovered, cx)))
            .child(div().flex_1().text_sm().child(toast.message))
            .child(
                toolbar_button("dismiss-toast", "Dismiss", self.palette.control)
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_toast(cx))),
            )
            .into_any_element()
    }
}
