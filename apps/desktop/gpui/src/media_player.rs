//! Audio, video and 3D-model playback for the previewed item, as its own
//! entity.
//!
//! Playback refreshes often: video polls its decoder every 66 ms and audio
//! its position every 250 ms. Keeping that state in a separate view means a
//! tick notifies only this entity, so GPUI re-renders the player and its
//! ancestors but can reuse any cached sibling (such as the file listing)
//! instead of rebuilding the whole window.
//!
//! [`DirectoryWindow`] owns the player. It loads the previewed item as
//! before and hands the loaded audio, video or model to the player, stops it
//! whenever the preview changes or closes, forwards media keyboard shortcuts
//! and window-wide pointer moves and releases (for slider and orbit drags),
//! and embeds the player where the preview body goes. The player reports
//! what the window has to act on through [`MediaEvent`]s.

use crate::image_memory::RetiredImages;
use crate::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MediaKind {
    Audio,
    Video,
    Model,
}

/// The item the player is playing and the preview it belongs to.
#[derive(Clone, Debug)]
pub(crate) struct MediaItem {
    pub(crate) path: PathBuf,
    pub(crate) kind: MediaKind,
    /// The window's preview generation when the item was loaded.
    pub(crate) generation: u64,
}

/// Something the window has to act on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MediaEvent {
    /// Show a message in the window's status line.
    Status(String),
    /// Show a warning toast.
    Warning(String),
    /// Finished audio was asked to play again: reload it and play it.
    RestartAudio,
    /// Open the file in its default application.
    Open(PathBuf),
}

pub(crate) struct MediaPlayer {
    services: NativeServices,
    /// Frames replaced or dropped by the player. The window releases them
    /// at the start of each of its renders, once no presented frame can
    /// reference them.
    retired: Rc<RefCell<RetiredImages>>,
    palette: UiPalette,
    /// Whether the player is shown in Quick Look, which allows larger media.
    quick_look: bool,
    pub(crate) item: Option<MediaItem>,
    pub(crate) audio_status: Option<AudioStatus>,
    audio_tick_task: Option<Task<()>>,
    pub(crate) video_status: Option<VideoStatus>,
    pub(crate) video_frame: Option<Arc<RenderImage>>,
    pub(crate) video_frame_position_ms: Option<u64>,
    video_frame_task: Option<Task<()>>,
    video_tick_task: Option<Task<()>>,
    video_control_task: Option<Task<()>>,
    slider_drag: Option<MediaSliderDrag>,
    /// The model on screen, with its latest rendered frame.
    pub(crate) model: Option<ModelPreview>,
    pub(crate) model_camera: ModelCamera,
    pub(crate) model_rendered_camera: ModelCamera,
    pub(crate) model_frame: Option<Arc<RenderImage>>,
    model_render_generation: u64,
    pub(crate) model_task: Option<Task<()>>,
    model_drag: Option<ModelDrag>,
    #[cfg(test)]
    pub(crate) renders: usize,
}

impl EventEmitter<MediaEvent> for MediaPlayer {}

impl MediaPlayer {
    pub(crate) fn new(
        services: NativeServices,
        retired: Rc<RefCell<RetiredImages>>,
        palette: UiPalette,
    ) -> Self {
        Self {
            services,
            retired,
            palette,
            quick_look: false,
            item: None,
            audio_status: None,
            audio_tick_task: None,
            video_status: None,
            video_frame: None,
            video_frame_position_ms: None,
            video_frame_task: None,
            video_tick_task: None,
            video_control_task: None,
            slider_drag: None,
            model: None,
            model_camera: ModelCamera::default(),
            model_rendered_camera: ModelCamera::default(),
            model_frame: None,
            model_render_generation: 0,
            model_task: None,
            model_drag: None,
            #[cfg(test)]
            renders: 0,
        }
    }

    /// Match the window's palette and placement before the player renders.
    pub(crate) fn set_presentation(&mut self, palette: UiPalette, quick_look: bool) {
        self.palette = palette;
        self.quick_look = quick_look;
    }

    fn is_current(&self, generation: u64, kind: MediaKind) -> bool {
        self.item
            .as_ref()
            .is_some_and(|item| item.generation == generation && item.kind == kind)
    }

    /// Play loaded audio, polling its position while it stays current.
    pub(crate) fn start_audio(
        &mut self,
        path: PathBuf,
        generation: u64,
        status: AudioStatus,
        autoplay: bool,
        cx: &mut Context<Self>,
    ) {
        self.item = Some(MediaItem {
            path,
            kind: MediaKind::Audio,
            generation,
        });
        self.audio_status = Some(if autoplay {
            self.services.audio.play().unwrap_or(status)
        } else {
            status
        });
        self.start_audio_status_poll(generation, cx);
        cx.notify();
    }

    /// Show a loaded video's first frame and poll the decoder while it stays
    /// current.
    pub(crate) fn start_video(
        &mut self,
        path: PathBuf,
        generation: u64,
        status: VideoStatus,
        cx: &mut Context<Self>,
    ) {
        self.item = Some(MediaItem {
            path,
            kind: MediaKind::Video,
            generation,
        });
        self.video_status = Some(status);
        self.refresh_video_frame(cx);
        self.start_video_status_poll(generation, cx);
        cx.notify();
    }

    /// Show a loaded model rendered from `camera`.
    pub(crate) fn start_model(
        &mut self,
        path: PathBuf,
        generation: u64,
        model: ModelPreview,
        frame: Option<Arc<RenderImage>>,
        camera: ModelCamera,
        cx: &mut Context<Self>,
    ) {
        self.item = Some(MediaItem {
            path,
            kind: MediaKind::Model,
            generation,
        });
        self.set_model_frame(frame);
        self.model_rendered_camera = camera;
        self.model = Some(model);
        cx.notify();
    }

    pub(crate) fn stop_audio(&mut self) {
        self.services.audio.stop();
        self.audio_status = None;
        self.audio_tick_task = None;
    }

    /// Stop audio and video playback. Called whenever the preview changes or
    /// closes; the model view is reset separately by [`Self::reset_model`].
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        self.stop_audio();
        self.services.video.stop();
        self.video_status = None;
        self.set_video_frame(None);
        self.video_frame_position_ms = None;
        self.video_frame_task = None;
        self.video_tick_task = None;
        self.video_control_task = None;
        self.item = None;
        cx.notify();
    }

    /// Forget the model view before a new preview starts.
    pub(crate) fn reset_model(&mut self) {
        self.model_camera = ModelCamera::default();
        self.model_rendered_camera = self.model_camera;
        self.set_model_frame(None);
        self.model_drag = None;
        self.model_render_generation = self.model_render_generation.wrapping_add(1);
        self.model_task = None;
        self.model = None;
    }

    /// Drop a model redraw in flight.
    pub(crate) fn cancel_model_render(&mut self) {
        self.model_task = None;
    }

    /// Pause playback while the preview is hidden behind another tab.
    pub(crate) fn pause_for_hidden_preview(&mut self, cx: &mut Context<Self>) {
        let _ = self.services.audio.pause();
        self.refresh_audio_status();
        self.pause_video(cx);
    }

    /// Replace the displayed video frame. The previous frame's texture is
    /// released from the sprite atlas once no presented frame can use it.
    pub(crate) fn set_video_frame(&mut self, frame: Option<Arc<RenderImage>>) {
        let previous = std::mem::replace(&mut self.video_frame, frame);
        self.retired.borrow_mut().retire(previous);
    }

    /// Replace the displayed 3D-model frame, retiring the previous one.
    pub(crate) fn set_model_frame(&mut self, frame: Option<Arc<RenderImage>>) {
        let previous = std::mem::replace(&mut self.model_frame, frame);
        self.retired.borrow_mut().retire(previous);
    }

    pub(crate) fn request_model_render(&mut self, cx: &mut Context<Self>) {
        if self.model_task.is_some() || self.model_camera == self.model_rendered_camera {
            return;
        }
        let Some(item) = self
            .item
            .clone()
            .filter(|item| item.kind == MediaKind::Model)
        else {
            return;
        };
        let generation = item.generation;
        self.model_render_generation = self.model_render_generation.wrapping_add(1);
        let model_generation = self.model_render_generation;
        let camera = self.model_camera;
        let task = self
            .services
            .previews
            .model(item.path, camera, 960, 640)
            .cancel_on_drop();
        self.model_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let rendered = match &result {
                Ok(preview) => {
                    let frame = preview.frame.clone();
                    cx.background_spawn(async move { render_model_frame(&frame) })
                        .await
                }
                Err(_) => None,
            };
            let _ = this.update(cx, |player, cx| {
                if !player.is_current(generation, MediaKind::Model)
                    || player.model_render_generation != model_generation
                {
                    return;
                }
                player.model_task = None;
                match result {
                    Ok(preview) => {
                        player.set_model_frame(rendered);
                        player.model_rendered_camera = camera;
                        player.model = Some(preview);
                    }
                    Err(error) => cx.emit(MediaEvent::Warning(format!(
                        "Unable to redraw the model: {error}"
                    ))),
                }
                let needs_follow_up = player.model_camera != player.model_rendered_camera;
                if needs_follow_up {
                    player.request_model_render(cx);
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn adjust_model_camera(
        &mut self,
        yaw: f32,
        pitch: f32,
        zoom_factor: f32,
        pan_x: f32,
        pan_y: f32,
        cx: &mut Context<Self>,
    ) {
        self.model_camera.yaw += yaw;
        self.model_camera.pitch = (self.model_camera.pitch + pitch).clamp(-1.45, 1.45);
        self.model_camera.zoom = (self.model_camera.zoom * zoom_factor).clamp(0.25, 4.0);
        self.model_camera.pan_x = (self.model_camera.pan_x + pan_x).clamp(-1.5, 1.5);
        self.model_camera.pan_y = (self.model_camera.pan_y + pan_y).clamp(-1.5, 1.5);
        self.request_model_render(cx);
        cx.notify();
    }

    pub(crate) fn reset_model_camera(&mut self, cx: &mut Context<Self>) {
        self.model_camera = ModelCamera::default();
        self.request_model_render(cx);
    }

    pub(crate) fn start_model_drag(&mut self, event: &gpui::MouseDownEvent, pan: bool) {
        self.model_drag = Some(ModelDrag {
            start_x: f32::from(event.position.x),
            start_y: f32::from(event.position.y),
            camera: self.model_camera,
            pan,
        });
    }

    pub(crate) fn update_model_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.model_drag else {
            return;
        };
        let expected_button = if drag.pan {
            MouseButton::Right
        } else {
            MouseButton::Left
        };
        if event.pressed_button != Some(expected_button) {
            self.model_drag = None;
            return;
        }
        let delta_x = f32::from(event.position.x) - drag.start_x;
        let delta_y = f32::from(event.position.y) - drag.start_y;
        self.model_camera = drag.camera;
        if drag.pan {
            self.model_camera.pan_x = (drag.camera.pan_x + delta_x * 0.0025).clamp(-1.5, 1.5);
            self.model_camera.pan_y = (drag.camera.pan_y - delta_y * 0.0025).clamp(-1.5, 1.5);
        } else {
            self.model_camera.yaw = drag.camera.yaw + delta_x * 0.01;
            self.model_camera.pitch = (drag.camera.pitch + delta_y * 0.01).clamp(-1.45, 1.45);
        }
        self.request_model_render(cx);
        cx.notify();
    }

    pub(crate) fn finish_model_drag(&mut self) {
        self.model_drag = None;
    }

    pub(crate) fn refresh_audio_status(&mut self) {
        if let Ok(status) = self.services.audio.status() {
            self.audio_status = Some(status);
        }
    }

    fn start_audio_status_poll(&mut self, generation: u64, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        self.audio_tick_task = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(250)).await;
                let keep_polling = this
                    .update(cx, |player, cx| {
                        if !player.is_current(generation, MediaKind::Audio) {
                            return false;
                        }
                        player.refresh_audio_status();
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        }));
    }

    pub(crate) fn toggle_audio_playback(&mut self, cx: &mut Context<Self>) {
        if self
            .audio_status
            .as_ref()
            .is_some_and(|status| status.finished)
        {
            cx.emit(MediaEvent::RestartAudio);
            return;
        }
        let result = if self
            .audio_status
            .as_ref()
            .is_some_and(|status| status.playing)
        {
            self.services.audio.pause()
        } else {
            self.services.audio.play()
        };
        match result {
            Ok(status) => self.audio_status = Some(status),
            Err(error) => cx.emit(MediaEvent::Status(format!(
                "Unable to control audio: {error}"
            ))),
        }
        cx.notify();
    }

    pub(crate) fn seek_audio_fraction(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let Some(duration_ms) = self
            .audio_status
            .as_ref()
            .and_then(|status| status.duration_ms)
        else {
            return;
        };
        match self
            .services
            .audio
            .seek((duration_ms as f64 * f64::from(fraction.clamp(0.0, 1.0))) as u64)
        {
            Ok(status) => self.audio_status = Some(status),
            Err(error) => cx.emit(MediaEvent::Status(format!("Unable to seek audio: {error}"))),
        }
        cx.notify();
    }

    fn handle_audio_seek_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "left" | "down" => self.skip_audio(-5_000, cx),
            "right" | "up" => self.skip_audio(5_000, cx),
            "home" => self.seek_audio_fraction(0.0, cx),
            "end" => self.seek_audio_fraction(1.0, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn skip_audio(&mut self, delta_ms: i64, cx: &mut Context<Self>) {
        let Some(status) = &self.audio_status else {
            return;
        };
        let target = status.position_ms.saturating_add_signed(delta_ms);
        match self.services.audio.seek(target) {
            Ok(status) => self.audio_status = Some(status),
            Err(error) => cx.emit(MediaEvent::Status(format!("Unable to seek audio: {error}"))),
        }
        cx.notify();
    }

    pub(crate) fn adjust_audio_volume(&mut self, delta: f32, cx: &mut Context<Self>) {
        let Some(status) = &self.audio_status else {
            return;
        };
        match self.services.audio.set_volume(status.volume + delta) {
            Ok(status) => self.audio_status = Some(status),
            Err(error) => cx.emit(MediaEvent::Status(format!("Unable to set volume: {error}"))),
        }
        cx.notify();
    }

    pub(crate) fn set_audio_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        match self.services.audio.set_volume(volume.clamp(0.0, 1.0)) {
            Ok(status) => self.audio_status = Some(status),
            Err(error) => cx.emit(MediaEvent::Status(format!("Unable to set volume: {error}"))),
        }
        cx.notify();
    }

    fn handle_audio_volume_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "left" | "down" => self.adjust_audio_volume(-0.1, cx),
            "right" | "up" => self.adjust_audio_volume(0.1, cx),
            "home" => self.set_audio_volume(0.0, cx),
            "end" => self.set_audio_volume(1.0, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn toggle_audio_mute(&mut self, cx: &mut Context<Self>) {
        let Some(status) = &self.audio_status else {
            return;
        };
        let volume = if status.volume <= 0.01 { 0.8 } else { 0.0 };
        match self.services.audio.set_volume(volume) {
            Ok(status) => self.audio_status = Some(status),
            Err(error) => cx.emit(MediaEvent::Status(format!("Unable to set volume: {error}"))),
        }
        cx.notify();
    }

    /// Refresh the playback status and, when the decoder has a new frame,
    /// convert it off the UI thread. At most one conversion is in flight; a
    /// newer frame is picked up by the next refresh.
    pub(crate) fn refresh_video_frame(&mut self, cx: &mut Context<Self>) {
        if let Ok(status) = self.services.video.status() {
            self.video_status = Some(status);
        }
        if self.video_frame_task.is_some() {
            return;
        }
        let Ok(Some(frame)) = self.services.video.take_frame() else {
            return;
        };
        if self.video_frame_position_ms == Some(frame.position_ms) {
            return;
        }
        let generation = self.item.as_ref().map(|item| item.generation);
        let position_ms = frame.position_ms;
        let conversion = cx.background_spawn(async move { render_video_frame(&frame) });
        self.video_frame_task = Some(cx.spawn(async move |this, cx| {
            let rendered = conversion.await;
            let _ = this.update(cx, |player, cx| {
                player.video_frame_task = None;
                if player.item.as_ref().map(|item| item.generation) != generation
                    || player.video_status.is_none()
                {
                    return;
                }
                if let Some(rendered) = rendered {
                    player.video_frame_position_ms = Some(position_ms);
                    player.set_video_frame(Some(rendered));
                    cx.notify();
                }
            });
        }));
    }

    fn start_video_status_poll(&mut self, generation: u64, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        self.video_tick_task = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(66)).await;
                let keep_polling = this
                    .update(cx, |player, cx| {
                        if !player.is_current(generation, MediaKind::Video) {
                            return false;
                        }
                        let previous_position = player.video_frame_position_ms;
                        let previous_playing = player
                            .video_status
                            .as_ref()
                            .is_some_and(|status| status.playing);
                        let converting = player.video_frame_task.is_some();
                        player.refresh_video_frame(cx);
                        let playing = player
                            .video_status
                            .as_ref()
                            .is_some_and(|status| status.playing);
                        // During steady playback a tick that starts converting
                        // a new frame leaves the redraw to the conversion,
                        // which shows the frame and the new position together.
                        let frame_coming = !converting && player.video_frame_task.is_some();
                        if previous_position != player.video_frame_position_ms
                            || previous_playing != playing
                            || (playing && !frame_coming)
                        {
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        }));
    }

    fn run_video_control(
        &mut self,
        task: BlockingTask<VideoStatus>,
        action: &'static str,
        cx: &mut Context<Self>,
    ) {
        let generation = self.item.as_ref().map(|item| item.generation);
        self.video_control_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |player, cx| {
                if !generation
                    .is_some_and(|generation| player.is_current(generation, MediaKind::Video))
                {
                    return;
                }
                match result {
                    Ok(status) => {
                        player.video_status = Some(status);
                        player.refresh_video_frame(cx);
                    }
                    Err(error) => {
                        cx.emit(MediaEvent::Status(format!(
                            "Unable to {action} video: {error}"
                        )));
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn toggle_video_playback(&mut self, cx: &mut Context<Self>) {
        let task = if self
            .video_status
            .as_ref()
            .is_some_and(|status| status.playing)
        {
            self.services.video.pause()
        } else {
            self.services.video.play()
        };
        self.run_video_control(task, "control", cx);
    }

    fn pause_video(&mut self, cx: &mut Context<Self>) {
        if self.video_status.is_some() {
            let task = self.services.video.pause();
            self.run_video_control(task, "pause", cx);
        }
    }

    pub(crate) fn seek_video_fraction(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let Some(duration_ms) = self
            .video_status
            .as_ref()
            .and_then(|status| status.duration_ms)
        else {
            return;
        };
        let target = (duration_ms as f64 * f64::from(fraction.clamp(0.0, 1.0))) as u64;
        self.run_video_control(self.services.video.seek(target), "seek", cx);
    }

    fn handle_video_seek_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "left" | "down" => self.skip_video(-5_000, cx),
            "right" | "up" => self.skip_video(5_000, cx),
            "home" => self.seek_video_fraction(0.0, cx),
            "end" => self.seek_video_fraction(1.0, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn skip_video(&mut self, delta_ms: i64, cx: &mut Context<Self>) {
        let Some(status) = &self.video_status else {
            return;
        };
        let target = status.position_ms.saturating_add_signed(delta_ms);
        self.run_video_control(self.services.video.seek(target), "seek", cx);
    }

    pub(crate) fn adjust_video_volume(&mut self, delta: f32, cx: &mut Context<Self>) {
        let Some(status) = &self.video_status else {
            return;
        };
        match self.services.video.set_volume(status.volume + delta) {
            Ok(status) => self.video_status = Some(status),
            Err(error) => cx.emit(MediaEvent::Status(format!("Unable to set volume: {error}"))),
        }
        cx.notify();
    }

    pub(crate) fn set_video_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        match self.services.video.set_volume(volume.clamp(0.0, 1.0)) {
            Ok(status) => self.video_status = Some(status),
            Err(error) => cx.emit(MediaEvent::Status(format!("Unable to set volume: {error}"))),
        }
        cx.notify();
    }

    fn handle_video_volume_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "left" | "down" => self.adjust_video_volume(-0.1, cx),
            "right" | "up" => self.adjust_video_volume(0.1, cx),
            "home" => self.set_video_volume(0.0, cx),
            "end" => self.set_video_volume(1.0, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn toggle_video_mute(&mut self, cx: &mut Context<Self>) {
        let Some(status) = &self.video_status else {
            return;
        };
        let volume = if status.volume <= 0.01 { 0.8 } else { 0.0 };
        match self.services.video.set_volume(volume) {
            Ok(status) => self.video_status = Some(status),
            Err(error) => cx.emit(MediaEvent::Status(format!("Unable to set volume: {error}"))),
        }
        cx.notify();
    }

    /// Quick Look's playback shortcuts: J/L seek, K plays or pauses, M
    /// mutes, Home/End jump to either end.
    pub(crate) fn handle_shortcut(&mut self, key: &str, is_held: bool, cx: &mut Context<Self>) {
        let Some(kind) = self.item.as_ref().map(|item| item.kind) else {
            return;
        };
        match (kind, key) {
            (MediaKind::Audio, "k") if !is_held => self.toggle_audio_playback(cx),
            (MediaKind::Audio, "j") => self.skip_audio(-10_000, cx),
            (MediaKind::Audio, "l") => self.skip_audio(10_000, cx),
            (MediaKind::Audio, "m") if !is_held => self.toggle_audio_mute(cx),
            (MediaKind::Audio, "home") => self.seek_audio_fraction(0.0, cx),
            (MediaKind::Audio, "end") => self.seek_audio_fraction(1.0, cx),
            (MediaKind::Video, "k") if !is_held => self.toggle_video_playback(cx),
            (MediaKind::Video, "j") => self.skip_video(-10_000, cx),
            (MediaKind::Video, "l") => self.skip_video(10_000, cx),
            (MediaKind::Video, "m") if !is_held => self.toggle_video_mute(cx),
            (MediaKind::Video, "home") => self.seek_video_fraction(0.0, cx),
            (MediaKind::Video, "end") => self.seek_video_fraction(1.0, cx),
            _ => {}
        }
    }

    fn update_media_slider_value(
        &mut self,
        kind: MediaSliderKind,
        bounds: Bounds<Pixels>,
        position_x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let fraction = slider_fraction(bounds, position_x);
        match kind {
            MediaSliderKind::AudioSeek => self.seek_audio_fraction(fraction, cx),
            MediaSliderKind::AudioVolume => self.set_audio_volume(fraction, cx),
            MediaSliderKind::VideoSeek => self.seek_video_fraction(fraction, cx),
            MediaSliderKind::VideoVolume => self.set_video_volume(fraction, cx),
        }
    }

    fn start_media_slider_drag(
        &mut self,
        kind: MediaSliderKind,
        bounds: Bounds<Pixels>,
        position_x: Pixels,
        cx: &mut Context<Self>,
    ) {
        self.slider_drag = Some(MediaSliderDrag { kind, bounds });
        self.update_media_slider_value(kind, bounds, position_x, cx);
    }

    /// Follow a pointer move anywhere in the window with the slider or
    /// orbit drag in progress, if any.
    pub(crate) fn update_pointer_drags(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if let Some(drag) = self.slider_drag {
            self.update_media_slider_value(drag.kind, drag.bounds, event.position.x, cx);
        }
        self.update_model_drag(event, cx);
    }

    /// End the slider and orbit drags when the left button is released
    /// anywhere.
    pub(crate) fn finish_pointer_drags(&mut self) {
        self.slider_drag = None;
        self.model_drag = None;
    }
}

impl Render for MediaPlayer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        let Some(item) = self.item.clone() else {
            return div().into_any_element();
        };
        match item.kind {
            MediaKind::Audio => self.render_audio(&item.path, cx),
            MediaKind::Video => self.render_video(&item.path, cx),
            MediaKind::Model => self.render_model(cx),
        }
    }
}

impl MediaPlayer {
    fn render_audio(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let status = self.audio_status.clone();
        let playing = status.as_ref().is_some_and(|status| status.playing);
        let position_ms = status.as_ref().map_or(0, |status| status.position_ms);
        let duration_ms = status.as_ref().and_then(|status| status.duration_ms);
        let progress = duration_ms
            .filter(|duration| *duration > 0)
            .map_or(0.0, |duration| position_ms as f32 / duration as f32)
            .clamp(0.0, 1.0);
        let volume = status.as_ref().map_or(0.8, |status| status.volume);
        let file_label = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let audio_seek_bounds = Rc::new(Cell::new(None));
        let store_audio_seek_bounds = audio_seek_bounds.clone();
        let click_audio_seek_bounds = audio_seek_bounds.clone();
        let drag_audio_seek_bounds = audio_seek_bounds.clone();
        let audio_volume_bounds = Rc::new(Cell::new(None));
        let store_audio_volume_bounds = audio_volume_bounds.clone();
        let click_audio_volume_bounds = audio_volume_bounds.clone();
        let drag_audio_volume_bounds = audio_volume_bounds.clone();
        let audio_seek_increment = cx.entity().downgrade();
        let audio_seek_decrement = audio_seek_increment.clone();
        let audio_volume_increment = audio_seek_increment.clone();
        let audio_volume_decrement = audio_seek_increment.clone();
        div()
            .id("audio-preview")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .min_h(px(220.0))
            .px_4()
            .py_5()
            .child(
                div()
                    .text_size(px(34.0))
                    .text_color(self.palette.muted)
                    .child("♫"),
            )
            .child(
                div()
                    .id("audio-controls")
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .max_w(px(400.0 * self.palette.scale))
                    .px_2()
                    .py_3()
                    .child(
                        div()
                            .on_children_prepainted(move |bounds, _, _| {
                                store_audio_seek_bounds.set(bounds.first().copied());
                            })
                            .id("audio-seek")
                            .debug_selector(|| "audio-seek".to_string())
                            .role(Role::Slider)
                            .aria_label("Audio position")
                            .aria_numeric_value(position_ms as f64)
                            .aria_min_numeric_value(0.0)
                            .aria_max_numeric_value(duration_ms.unwrap_or(0) as f64)
                            .focusable()
                            .tab_stop(true)
                            .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                                audio_seek_increment
                                    .update(cx, |this, cx| this.skip_audio(5_000, cx))
                                    .ok();
                            })
                            .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                                audio_seek_decrement
                                    .update(cx, |this, cx| this.skip_audio(-5_000, cx))
                                    .ok();
                            })
                            .on_key_down(cx.listener(|this, event, _, cx| {
                                this.handle_audio_seek_key(event, cx)
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                    if let Some(bounds) = drag_audio_seek_bounds.get() {
                                        this.start_media_slider_drag(
                                            MediaSliderKind::AudioSeek,
                                            bounds,
                                            event.position.x,
                                            cx,
                                        );
                                    }
                                }),
                            )
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                if let Some(bounds) = click_audio_seek_bounds.get() {
                                    this.seek_audio_fraction(
                                        slider_fraction(bounds, event.position().x),
                                        cx,
                                    );
                                }
                            }))
                            .flex()
                            .items_center()
                            .h(px(32.0 * self.palette.scale))
                            .px_1()
                            .rounded_full()
                            .border_1()
                            .border_color(with_alpha(self.palette.border, 0.0))
                            .focus_visible(|slider| slider.border_color(self.palette.accent))
                            .cursor_pointer()
                            .child(media_slider_track(progress, self.palette)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format_audio_time(position_ms))
                            .child(
                                duration_ms.map_or_else(|| "--:--".to_string(), format_audio_time),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(
                                toolbar_button("audio-back-ten", "−10s", self.palette.control)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.skip_audio(-10_000, cx)),
                                    ),
                            )
                            .child(
                                toolbar_button(
                                    "audio-play-pause",
                                    if playing { "Pause" } else { "Play" },
                                    self.palette.accent,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.toggle_audio_playback(cx)),
                                ),
                            )
                            .child(
                                toolbar_button("audio-forward-ten", "+10s", self.palette.control)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.skip_audio(10_000, cx)),
                                    ),
                            )
                            .child(
                                toolbar_button(
                                    "audio-mute",
                                    if volume <= 0.01 { "Unmute" } else { "Mute" },
                                    self.palette.control,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_audio_mute(cx))),
                            )
                            .child(
                                div()
                                    .on_children_prepainted(move |bounds, _, _| {
                                        store_audio_volume_bounds.set(bounds.first().copied());
                                    })
                                    .id("audio-volume")
                                    .role(Role::Slider)
                                    .aria_label("Audio volume")
                                    .aria_numeric_value(f64::from(volume * 100.0))
                                    .aria_min_numeric_value(0.0)
                                    .aria_max_numeric_value(100.0)
                                    .focusable()
                                    .tab_stop(true)
                                    .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                                        audio_volume_increment
                                            .update(cx, |this, cx| {
                                                this.adjust_audio_volume(0.1, cx)
                                            })
                                            .ok();
                                    })
                                    .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                                        audio_volume_decrement
                                            .update(cx, |this, cx| {
                                                this.adjust_audio_volume(-0.1, cx)
                                            })
                                            .ok();
                                    })
                                    .on_key_down(cx.listener(|this, event, _, cx| {
                                        this.handle_audio_volume_key(event, cx)
                                    }))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |this, event: &gpui::MouseDownEvent, _, cx| {
                                                if let Some(bounds) = drag_audio_volume_bounds.get()
                                                {
                                                    this.start_media_slider_drag(
                                                        MediaSliderKind::AudioVolume,
                                                        bounds,
                                                        event.position.x,
                                                        cx,
                                                    );
                                                }
                                            },
                                        ),
                                    )
                                    .on_click(cx.listener(
                                        move |this, event: &gpui::ClickEvent, _, cx| {
                                            if let Some(bounds) = click_audio_volume_bounds.get() {
                                                this.set_audio_volume(
                                                    slider_fraction(bounds, event.position().x),
                                                    cx,
                                                );
                                            }
                                        },
                                    ))
                                    .flex()
                                    .items_center()
                                    .w(px(120.0 * self.palette.scale))
                                    .h(px(32.0 * self.palette.scale))
                                    .px_2()
                                    .border_1()
                                    .border_color(self.palette.border)
                                    .rounded_full()
                                    .focus_visible(|slider| {
                                        slider.border_color(self.palette.accent)
                                    })
                                    .cursor_pointer()
                                    .child(media_slider_track(volume, self.palette)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child(format!("{}%", (volume * 100.0).round())),
                            ),
                    )
                    .when(self.quick_look, |controls| {
                        controls.child(media_shortcut_hint(self.palette))
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child(file_label),
            )
            .into_any_element()
    }

    fn render_video(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let quick_look = self.quick_look;
        let status = self.video_status.clone();
        let playing = status.as_ref().is_some_and(|status| status.playing);
        let finished = status.as_ref().is_some_and(|status| status.finished);
        let position_ms = status.as_ref().map_or(0, |status| status.position_ms);
        let duration_ms = status.as_ref().and_then(|status| status.duration_ms);
        let progress = duration_ms
            .filter(|duration| *duration > 0)
            .map_or(0.0, |duration| position_ms as f32 / duration as f32)
            .clamp(0.0, 1.0);
        let volume = status.as_ref().map_or(0.8, |status| status.volume);
        let has_audio = status.as_ref().is_some_and(|status| status.has_audio);
        let frame = self.video_frame.clone();
        let file_label = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let open_path = path.to_path_buf();
        let video_seek_bounds = Rc::new(Cell::new(None));
        let store_video_seek_bounds = video_seek_bounds.clone();
        let click_video_seek_bounds = video_seek_bounds.clone();
        let drag_video_seek_bounds = video_seek_bounds.clone();
        let video_volume_bounds = Rc::new(Cell::new(None));
        let store_video_volume_bounds = video_volume_bounds.clone();
        let click_video_volume_bounds = video_volume_bounds.clone();
        let drag_video_volume_bounds = video_volume_bounds.clone();
        let video_seek_increment = cx.entity().downgrade();
        let video_seek_decrement = video_seek_increment.clone();
        let video_volume_increment = video_seek_increment.clone();
        let video_volume_decrement = video_seek_increment.clone();
        div()
            .id("video-preview")
            .flex()
            .flex_col()
            .w_full()
            .min_h(px(300.0))
            .bg(self.palette.window)
            .child(
                div()
                    .id("video-viewport")
                    .flex()
                    .flex_1()
                    .min_h(px(220.0))
                    .max_h(px(if quick_look { 760.0 } else { 520.0 }))
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .bg(rgb(0x000000))
                    .when_some(frame, |viewport, frame| {
                        viewport.child(img(frame).max_w_full().max_h(px(if quick_look {
                            740.0
                        } else {
                            500.0
                        })))
                    })
                    .when(self.video_frame.is_none(), |viewport| {
                        viewport.child(
                            div()
                                .text_sm()
                                .text_color(rgb(0x9ca3af))
                                .child("Decoding video frame…"),
                        )
                    }),
            )
            .child(
                div()
                    .id("video-controls")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_t_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.surface)
                    .child(
                        div()
                            .on_children_prepainted(move |bounds, _, _| {
                                store_video_seek_bounds.set(bounds.first().copied());
                            })
                            .id("video-seek")
                            .role(Role::Slider)
                            .aria_label("Video position")
                            .aria_numeric_value(position_ms as f64)
                            .aria_min_numeric_value(0.0)
                            .aria_max_numeric_value(duration_ms.unwrap_or(0) as f64)
                            .focusable()
                            .tab_stop(true)
                            .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                                video_seek_increment
                                    .update(cx, |this, cx| this.skip_video(5_000, cx))
                                    .ok();
                            })
                            .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                                video_seek_decrement
                                    .update(cx, |this, cx| this.skip_video(-5_000, cx))
                                    .ok();
                            })
                            .on_key_down(cx.listener(|this, event, _, cx| {
                                this.handle_video_seek_key(event, cx)
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                    if let Some(bounds) = drag_video_seek_bounds.get() {
                                        this.start_media_slider_drag(
                                            MediaSliderKind::VideoSeek,
                                            bounds,
                                            event.position.x,
                                            cx,
                                        );
                                    }
                                }),
                            )
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                if let Some(bounds) = click_video_seek_bounds.get() {
                                    this.seek_video_fraction(
                                        slider_fraction(bounds, event.position().x),
                                        cx,
                                    );
                                }
                            }))
                            .flex()
                            .items_center()
                            .h(px(32.0 * self.palette.scale))
                            .px_1()
                            .rounded_full()
                            .border_1()
                            .border_color(with_alpha(self.palette.border, 0.0))
                            .focus_visible(|slider| slider.border_color(self.palette.accent))
                            .cursor_pointer()
                            .child(media_slider_track(progress, self.palette)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format_audio_time(position_ms))
                            .child(
                                duration_ms.map_or_else(|| "--:--".to_string(), format_audio_time),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(
                                toolbar_button("video-back-ten", "−10s", self.palette.control)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.skip_video(-10_000, cx)),
                                    ),
                            )
                            .child(
                                toolbar_button(
                                    "video-play-pause",
                                    if playing {
                                        "Pause"
                                    } else if finished {
                                        "Replay"
                                    } else {
                                        "Play"
                                    },
                                    self.palette.accent,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.toggle_video_playback(cx)),
                                ),
                            )
                            .child(
                                toolbar_button("video-forward-ten", "+10s", self.palette.control)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.skip_video(10_000, cx)),
                                    ),
                            )
                            .child(
                                toolbar_button(
                                    "video-mute",
                                    if volume <= 0.01 { "Unmute" } else { "Mute" },
                                    self.palette.control,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_video_mute(cx))),
                            )
                            .when(has_audio, |controls| {
                                controls.child(
                                    div()
                                        .on_children_prepainted(move |bounds, _, _| {
                                            store_video_volume_bounds.set(bounds.first().copied());
                                        })
                                        .id("video-volume")
                                        .role(Role::Slider)
                                        .aria_label("Video volume")
                                        .aria_numeric_value(f64::from(volume * 100.0))
                                        .aria_min_numeric_value(0.0)
                                        .aria_max_numeric_value(100.0)
                                        .focusable()
                                        .tab_stop(true)
                                        .on_a11y_action(
                                            AccessibleAction::Increment,
                                            move |_, _, cx| {
                                                video_volume_increment
                                                    .update(cx, |this, cx| {
                                                        this.adjust_video_volume(0.1, cx)
                                                    })
                                                    .ok();
                                            },
                                        )
                                        .on_a11y_action(
                                            AccessibleAction::Decrement,
                                            move |_, _, cx| {
                                                video_volume_decrement
                                                    .update(cx, |this, cx| {
                                                        this.adjust_video_volume(-0.1, cx)
                                                    })
                                                    .ok();
                                            },
                                        )
                                        .on_key_down(cx.listener(|this, event, _, cx| {
                                            this.handle_video_volume_key(event, cx)
                                        }))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                move |this, event: &gpui::MouseDownEvent, _, cx| {
                                                    if let Some(bounds) =
                                                        drag_video_volume_bounds.get()
                                                    {
                                                        this.start_media_slider_drag(
                                                            MediaSliderKind::VideoVolume,
                                                            bounds,
                                                            event.position.x,
                                                            cx,
                                                        );
                                                    }
                                                },
                                            ),
                                        )
                                        .on_click(cx.listener(
                                            move |this, event: &gpui::ClickEvent, _, cx| {
                                                if let Some(bounds) =
                                                    click_video_volume_bounds.get()
                                                {
                                                    this.set_video_volume(
                                                        slider_fraction(bounds, event.position().x),
                                                        cx,
                                                    );
                                                }
                                            },
                                        ))
                                        .flex()
                                        .items_center()
                                        .w(px(120.0 * self.palette.scale))
                                        .h(px(32.0 * self.palette.scale))
                                        .px_2()
                                        .border_1()
                                        .border_color(self.palette.border)
                                        .rounded_full()
                                        .focus_visible(|slider| {
                                            slider.border_color(self.palette.accent)
                                        })
                                        .cursor_pointer()
                                        .child(media_slider_track(volume, self.palette)),
                                )
                            })
                            .child(div().text_xs().text_color(self.palette.muted).child(
                                if has_audio {
                                    format!("{}%", (volume * 100.0).round())
                                } else {
                                    "No audio".to_string()
                                },
                            ))
                            .child(
                                toolbar_button("video-open", "Open", self.palette.control)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(MediaEvent::Open(open_path.clone()))
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_center()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(file_label),
                    )
                    .when(self.quick_look, |controls| {
                        controls.child(media_shortcut_hint(self.palette))
                    }),
            )
            .into_any_element()
    }

    fn render_model(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(model) = self.model.clone() else {
            return div().into_any_element();
        };
        let quick_look = self.quick_look;
        let frame = self.model_frame.clone();
        let details = format!(
            "{} · {} mesh{} · {} vertices · {} triangles{}",
            model.format,
            model.mesh_count,
            if model.mesh_count == 1 { "" } else { "es" },
            model.vertex_count,
            model.triangle_count,
            if model.sampled {
                " · sampled for display"
            } else {
                ""
            }
        );
        div()
            .id("model-preview")
            .flex()
            .flex_col()
            .w_full()
            .min_h(px(300.0))
            .bg(self.palette.window)
            .child(
                div()
                    .id("model-viewport")
                    .flex()
                    .flex_1()
                    .min_h(px(if quick_look { 480.0 } else { 280.0 }))
                    .max_h(px(if quick_look { 760.0 } else { 520.0 }))
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .bg(rgb(0x101317))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event, _, cx| {
                            this.start_model_drag(event, false);
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event, _, cx| {
                            this.start_model_drag(event, true);
                            cx.stop_propagation();
                        }),
                    )
                    .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                        let delta = f32::from(event.delta.pixel_delta(px(16.0)).y);
                        let zoom = (-delta * 0.0025).exp();
                        this.adjust_model_camera(0.0, 0.0, zoom, 0.0, 0.0, cx);
                        cx.stop_propagation();
                    }))
                    .when_some(frame, |viewport, frame| {
                        viewport.child(img(frame).max_w_full().max_h(px(if quick_look {
                            740.0
                        } else {
                            500.0
                        })))
                    })
                    .when(self.model_frame.is_none(), |viewport| {
                        viewport.child(
                            div()
                                .text_sm()
                                .text_color(self.palette.muted)
                                .child("Rendering model…"),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.surface)
                    .child(
                        toolbar_button("model-left", "↶", self.palette.control).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.adjust_model_camera(-0.2, 0.0, 1.0, 0.0, 0.0, cx)
                            }),
                        ),
                    )
                    .child(
                        toolbar_button("model-right", "↷", self.palette.control).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.adjust_model_camera(0.2, 0.0, 1.0, 0.0, 0.0, cx)
                            }),
                        ),
                    )
                    .child(
                        toolbar_button("model-zoom-out", "−", self.palette.control).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.adjust_model_camera(0.0, 0.0, 0.8, 0.0, 0.0, cx)
                            }),
                        ),
                    )
                    .child(
                        toolbar_button("model-reset", "Reset", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.reset_model_camera(cx))),
                    )
                    .child(
                        toolbar_button("model-zoom-in", "+", self.palette.control).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.adjust_model_camera(0.0, 0.0, 1.25, 0.0, 0.0, cx)
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .text_center()
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child(details),
            )
            .child(
                div()
                    .px_3()
                    .pb_3()
                    .text_center()
                    .text_xs()
                    .text_color(self.palette.tertiary)
                    .child("Drag to orbit · right-drag to pan · scroll to zoom"),
            )
            .into_any_element()
    }
}

/// The playback shortcuts, shown only in Quick Look: in the inspector those
/// letters select files by name instead.
fn media_shortcut_hint(palette: UiPalette) -> impl IntoElement {
    div()
        .debug_selector(|| "media-shortcut-hint".to_string())
        .text_center()
        .text_xs()
        .text_color(palette.tertiary)
        .child("J/L seek  •  K play/pause  •  M mute")
}
