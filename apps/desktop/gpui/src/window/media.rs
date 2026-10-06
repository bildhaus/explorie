//! `DirectoryWindow` side of media playback: creating the window's
//! [`MediaPlayer`], stopping it when the preview changes, and acting on what
//! it reports.

use crate::*;

impl DirectoryWindow {
    /// Create the media player for a window with `services` (already forked
    /// for the window) and route its events to the window.
    pub(crate) fn new_media_player(
        services: &NativeServices,
        image_memory: &ImageMemory,
        palette: UiPalette,
        cx: &mut Context<Self>,
    ) -> Entity<MediaPlayer> {
        let media =
            cx.new(|_| MediaPlayer::new(services.clone(), image_memory.retired.clone(), palette));
        cx.subscribe(&media, |this, _, event, cx| {
            this.handle_media_event(event, cx)
        })
        .detach();
        media
    }

    /// Stop audio and video playback of the previewed item.
    pub(crate) fn stop_media_preview(&mut self, cx: &mut Context<Self>) {
        self.media.update(cx, |media, cx| media.stop(cx));
    }

    /// Release the textures of frames retired before the previous render.
    /// Must run at the start of every render of this window.
    pub(crate) fn release_retired_frames(&mut self, window: &mut Window) {
        self.image_memory.retired.borrow_mut().release(window);
    }

    fn handle_media_event(&mut self, event: &MediaEvent, cx: &mut Context<Self>) {
        match event {
            MediaEvent::Status(message) => {
                self.status_message = Some(message.clone());
                cx.notify();
            }
            MediaEvent::Warning(message) => {
                self.show_toast(message.clone(), ToastKind::Warning, cx);
                cx.notify();
            }
            MediaEvent::RestartAudio => {
                if let Some(path) = self.preview.state.path().map(Path::to_path_buf) {
                    self.preview.audio_autoplay_after_load = true;
                    self.start_preview(path, cx);
                }
            }
            MediaEvent::Open(path) => self.open_entry(path.clone(), false, cx),
        }
    }
}
