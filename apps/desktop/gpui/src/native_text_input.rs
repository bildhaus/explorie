use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    Render, Rgba, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window,
    div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

gpui::actions!(
    native_text_input,
    [
        Backspace,
        DeleteToStart,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
    ]
);

/// Text field bindings. `paste_through_menu` leaves Cmd+V to the Edit
/// menu's Paste, which routes it back to the focused field (on macOS only a
/// paste through the menu may read text another app copied without asking).
pub fn key_bindings(paste_through_menu: bool) -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, Some("NativeTextInput")),
        // macOS text fields delete to the start of the line on Cmd+Backspace.
        KeyBinding::new("cmd-backspace", DeleteToStart, Some("NativeTextInput")),
        KeyBinding::new("delete", Delete, Some("NativeTextInput")),
        KeyBinding::new("left", Left, Some("NativeTextInput")),
        KeyBinding::new("right", Right, Some("NativeTextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("NativeTextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("NativeTextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("NativeTextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("NativeTextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("NativeTextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("NativeTextInput")),
        KeyBinding::new("cmd-c", Copy, Some("NativeTextInput")),
        KeyBinding::new("ctrl-x", Cut, Some("NativeTextInput")),
        KeyBinding::new("cmd-x", Cut, Some("NativeTextInput")),
        KeyBinding::new("home", Home, Some("NativeTextInput")),
        KeyBinding::new("end", End, Some("NativeTextInput")),
    ];
    if !paste_through_menu {
        bindings.push(KeyBinding::new("cmd-v", Paste, Some("NativeTextInput")));
    }
    bindings
}

#[derive(Clone, Debug)]
pub enum NativeTextInputEvent {
    Changed(String),
}

#[derive(Clone, Copy)]
pub struct NativeTextInputAppearance {
    pub colors: [Rgba; 5],
    pub scale: f32,
}

pub struct NativeTextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    label: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    last_display_to_content: Option<Vec<usize>>,
    scroll_x: Pixels,
    is_selecting: bool,
    background: Rgba,
    border: Rgba,
    text: Rgba,
    muted: Rgba,
    accent: Rgba,
    masked: bool,
    scale: f32,
    undo_stack: Vec<TextSnapshot>,
    redo_stack: Vec<TextSnapshot>,
    last_edit: Option<EditKind>,
}

/// The field's text and selection before an edit, for Undo and Redo.
#[derive(Clone)]
struct TextSnapshot {
    content: SharedString,
    selected_range: Range<usize>,
}

/// Consecutive edits of one kind undo as one step, as in macOS text fields:
/// a typed word, or a run of deletions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Typing,
    Deleting,
    Other,
}

const MAX_UNDO_STEPS: usize = 100;

impl EventEmitter<NativeTextInputEvent> for NativeTextInput {}

impl NativeTextInput {
    pub fn new(
        content: impl Into<String>,
        placeholder: impl Into<SharedString>,
        label: impl Into<SharedString>,
        appearance: NativeTextInputAppearance,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = single_line(content.into());
        let cursor = content.len();
        let colors = appearance.colors;
        Self {
            focus_handle: cx.focus_handle(),
            content: content.into(),
            placeholder: placeholder.into(),
            label: label.into(),
            selected_range: cursor..cursor,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            last_display_to_content: None,
            scroll_x: px(0.0),
            is_selecting: false,
            background: colors[0],
            border: colors[1],
            text: colors[2],
            muted: colors[3],
            accent: colors[4],
            masked: false,
            scale: appearance.scale,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn set_content(&mut self, content: impl Into<String>, cx: &mut Context<Self>) {
        let content = single_line(content.into());
        let cursor = content.len();
        self.content = content.into();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_x = px(0.0);
        self.clear_history();
        cx.notify();
    }

    pub fn reconfigure(
        &mut self,
        content: impl Into<String>,
        placeholder: impl Into<SharedString>,
        label: impl Into<SharedString>,
        masked: bool,
        appearance: NativeTextInputAppearance,
        cx: &mut Context<Self>,
    ) {
        let content = single_line(content.into());
        let placeholder = placeholder.into();
        let label = label.into();
        let colors = appearance.colors;
        let content_changed = self.content.as_ref() != content;
        let changed = content_changed
            || self.placeholder != placeholder
            || self.label != label
            || self.masked != masked
            || self.background != colors[0]
            || self.border != colors[1]
            || self.text != colors[2]
            || self.muted != colors[3]
            || self.accent != colors[4]
            || self.scale != appearance.scale;
        if !changed {
            return;
        }
        if content_changed {
            let cursor = content.len();
            self.content = content.into();
            self.selected_range = cursor..cursor;
            self.selection_reversed = false;
            self.marked_range = None;
            self.scroll_x = px(0.0);
            self.clear_history();
        }
        self.placeholder = placeholder;
        self.label = label;
        self.masked = masked;
        self.background = colors[0];
        self.border = colors[1];
        self.text = colors[2];
        self.muted = colors[3];
        self.accent = colors[4];
        self.scale = appearance.scale;
        cx.notify();
    }

    pub fn select_all_content(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    /// Select part of the content (byte offsets, snapped to characters), as
    /// a rename field selects a file's name without its extension.
    pub fn select_content_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let snap = |offset: usize| {
            let mut offset = offset.min(self.content.len());
            while !self.content.is_char_boundary(offset) {
                offset -= 1;
            }
            offset
        };
        self.selected_range = snap(range.start)..snap(range.end.max(range.start));
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn emit_changed(&self, cx: &mut Context<Self>) {
        cx.emit(NativeTextInputEvent::Changed(self.content.to_string()));
    }

    fn snapshot(&self) -> TextSnapshot {
        TextSnapshot {
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
        }
    }

    fn clear_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = None;
    }

    /// Remember the text before an edit, unless it continues the previous one.
    fn record_edit(&mut self, kind: EditKind) {
        if kind == EditKind::Other || self.last_edit != Some(kind) {
            if self.undo_stack.len() == MAX_UNDO_STEPS {
                self.undo_stack.remove(0);
            }
            self.undo_stack.push(self.snapshot());
        }
        self.redo_stack.clear();
        self.last_edit = Some(kind);
    }

    fn restore(&mut self, snapshot: TextSnapshot, cx: &mut Context<Self>) {
        self.content = snapshot.content;
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_edit = None;
        self.emit_changed(cx);
        cx.notify();
    }

    // Undo and Redo stay inside a focused field, as in macOS: they never
    // reach the window, where they would undo the last file operation.
    fn undo(&mut self, _: &crate::Undo, window: &mut Window, cx: &mut Context<Self>) {
        match self.undo_stack.pop() {
            Some(snapshot) => {
                self.redo_stack.push(self.snapshot());
                self.restore(snapshot, cx);
            }
            None => window.play_system_bell(),
        }
    }

    fn redo(&mut self, _: &crate::Redo, window: &mut Window, cx: &mut Context<Self>) {
        match self.redo_stack.pop() {
            Some(snapshot) => {
                self.undo_stack.push(self.snapshot());
                self.restore(snapshot, cx);
            }
            None => window.play_system_bell(),
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.last_edit = None;
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.last_edit = None;
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let previous = self.previous_boundary(self.cursor_offset());
            if previous == self.cursor_offset() {
                window.play_system_bell();
                return;
            }
            self.select_to(previous, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_start(&mut self, _: &DeleteToStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            if self.cursor_offset() == 0 {
                window.play_system_bell();
                return;
            }
            self.select_to(0, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if next == self.cursor_offset() {
                window.play_system_bell();
                return;
            }
            self.select_to(next, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace(['\r', '\n'], " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.masked {
            window.play_system_bell();
            return;
        }
        self.copy(&Copy, window, cx);
        if !self.selected_range.is_empty() {
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.last_edit = None;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        let display_index = line.closest_index_for_x(position.x - bounds.left() + self.scroll_x);
        self.display_offset_to_content(display_index)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for character in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf8 += character.len_utf8();
            utf16 += character.len_utf16();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..offset].encode_utf16().count()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn display_offset_to_content(&self, offset: usize) -> usize {
        self.last_display_to_content.as_ref().map_or(offset, |map| {
            map.get(offset)
                .copied()
                .unwrap_or_else(|| *map.last().unwrap_or(&self.content.len()))
        })
    }

    fn content_offset_to_display(&self, offset: usize) -> usize {
        self.last_display_to_content.as_ref().map_or(offset, |map| {
            map.partition_point(|content_offset| *content_offset < offset)
                .min(map.len().saturating_sub(1))
        })
    }
}

fn utf16_offset_in(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for character in text.chars() {
        if utf16 >= offset {
            break;
        }
        utf8 += character.len_utf8();
        utf16 += character.len_utf16();
    }
    utf8
}

impl EntityInputHandler for NativeTextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_text = single_line(new_text.to_string());
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        if range.is_empty() && new_text.is_empty() {
            return;
        }
        // Typed characters (including typing over a selection, and committing
        // composed IME text) coalesce into one step, as do runs of deletions.
        // Pasted or otherwise inserted text is a step of its own.
        let kind = if new_text.is_empty() {
            EditKind::Deleting
        } else if self.marked_range.is_some() || new_text.chars().count() == 1 {
            EditKind::Typing
        } else {
            EditKind::Other
        };
        self.record_edit(kind);
        self.content = format!(
            "{}{}{}",
            &self.content[..range.start],
            new_text,
            &self.content[range.end..]
        )
        .into();
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.emit_changed(cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_text = single_line(new_text.to_string());
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.record_edit(EditKind::Typing);
        self.content = format!(
            "{}{}{}",
            &self.content[..range.start],
            new_text,
            &self.content[range.end..]
        )
        .into();
        self.marked_range =
            (!new_text.is_empty()).then_some(range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|selection| {
                utf16_offset_in(&new_text, selection.start)
                    ..utf16_offset_in(&new_text, selection.end)
            })
            .map(|selection| range.start + selection.start..range.start + selection.end)
            .unwrap_or_else(|| {
                let cursor = range.start + new_text.len();
                cursor..cursor
            });
        self.emit_changed(cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(self.content_offset_to_display(range.start))
                    - self.scroll_x,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(self.content_offset_to_display(range.end))
                    - self.scroll_x,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let index = line.index_for_x(point.x - bounds.left() + self.scroll_x)?;
        Some(self.offset_to_utf16(self.display_offset_to_content(index)))
    }
}

fn single_line(value: String) -> String {
    value.replace(['\r', '\n'], " ")
}

struct NativeTextElement {
    input: Entity<NativeTextInput>,
}

struct PrepaintState {
    line: ShapedLine,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    display_to_content: Option<Vec<usize>>,
    scroll_x: Pixels,
}

impl IntoElement for NativeTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for NativeTextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let display_to_content = if !content.is_empty() && input.masked {
            let mut offsets = content
                .grapheme_indices(true)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            offsets.push(content.len());
            Some(offsets)
        } else {
            None
        };
        let display_text = if content.is_empty() {
            input.placeholder.clone()
        } else if let Some(offsets) = display_to_content.as_ref() {
            "*".repeat(offsets.len().saturating_sub(1)).into()
        } else {
            content.clone()
        };
        let color = if content.is_empty() {
            input.muted
        } else {
            input.text
        };
        let style = window.text_style();
        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: color.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let display_offset = |offset: usize| {
            display_to_content.as_ref().map_or(offset, |map| {
                map.partition_point(|content_offset| *content_offset < offset)
                    .min(map.len().saturating_sub(1))
            })
        };
        let runs = if let Some(marked) = input.marked_range.as_ref() {
            let marked = display_offset(marked.start)..display_offset(marked.end);
            [
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len().saturating_sub(marked.end),
                    ..run.clone()
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let display_len = display_text.len();
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);
        let cursor_x = line.x_for_index(display_offset(input.cursor_offset()));
        let content_width = line.x_for_index(display_len);
        let viewport_width = bounds.size.width.max(px(1.0));
        let mut scroll_x = input
            .scroll_x
            .min((content_width - viewport_width).max(px(0.0)));
        let visible_cursor_x = cursor_x - scroll_x;
        if visible_cursor_x > viewport_width - px(2.0) {
            scroll_x = (cursor_x - viewport_width + px(2.0)).max(px(0.0));
        } else if visible_cursor_x < px(0.0) {
            scroll_x = cursor_x.max(px(0.0));
        }
        let selection = (!selected_range.is_empty()).then(|| {
            fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + line.x_for_index(display_offset(selected_range.start))
                            - scroll_x,
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(display_offset(selected_range.end))
                            - scroll_x,
                        bounds.bottom(),
                    ),
                ),
                gpui::rgba(0x4d7cc74a),
            )
        });
        let cursor = selected_range.is_empty().then(|| {
            fill(
                Bounds::new(
                    point(bounds.left() + cursor_x - scroll_x, bounds.top()),
                    size(px(1.0), bounds.size.height),
                ),
                input.accent,
            )
        });
        PrepaintState {
            line,
            cursor,
            selection,
            display_to_content,
            scroll_x,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        prepaint
            .line
            .paint(
                point(bounds.origin.x - prepaint.scroll_x, bounds.origin.y),
                window.line_height(),
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            )
            .expect("text input line should paint");
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, _| {
            input.last_layout = Some(prepaint.line.clone());
            input.last_bounds = Some(bounds);
            input.last_display_to_content = prepaint.display_to_content.clone();
            input.scroll_x = prepaint.scroll_x;
        });
    }
}

impl Render for NativeTextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = self.accent;
        div()
            .id("native-text-input")
            .role(gpui::Role::TextInput)
            .aria_label(self.label.clone())
            .key_context("NativeTextInput")
            .track_focus(&self.focus_handle)
            .tab_stop(true)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .flex()
            .items_center()
            .w_full()
            .h(px(34.0 * self.scale))
            .px_2()
            .border_1()
            .border_color(self.border)
            .focus(move |input| input.border_color(accent))
            .bg(self.background)
            .text_sm()
            .text_color(self.text)
            .overflow_hidden()
            .child(NativeTextElement { input: cx.entity() })
    }
}

impl Focusable for NativeTextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
