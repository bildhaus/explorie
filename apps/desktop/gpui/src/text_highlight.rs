//! Syntax and find highlighting for text previews.

use crate::*;

/// A text preview prepared for rendering: the text is shared rather than
/// copied, and line ranges and per-line syntax spans are computed once when
/// the preview loads, so a frame only touches the lines it shows.
pub(crate) struct TextPreviewDocument {
    id: u64,
    pub(crate) text: Arc<str>,
    pub(crate) truncated: bool,
    pub(crate) language: Option<String>,
    pub(crate) encoding: String,
    pub(crate) wrapped: bool,
    /// Byte range of every line, without its `\n` or trailing `\r`.
    line_ranges: Vec<std::ops::Range<usize>>,
    /// `line_spans[line_span_offsets[i]..line_span_offsets[i + 1]]` are the
    /// syntax spans of line `i`, relative to the line start and in the order
    /// the per-line highlighter expects.
    line_span_offsets: Vec<usize>,
    line_spans: Vec<(std::ops::Range<usize>, TextHighlightKind)>,
    valid_highlights: usize,
}

impl TextPreviewDocument {
    pub(crate) fn new(preview: TextPreview) -> Self {
        static NEXT_DOCUMENT_ID: AtomicU64 = AtomicU64::new(1);
        let TextPreview {
            text,
            truncated,
            language,
            encoding,
            wrapped,
            highlights,
        } = preview;
        #[cfg(test)]
        record_text_preview_scan(text.len());
        let text: Arc<str> = Arc::from(text);

        let mut next_line_start = 0;
        let line_ranges = text
            .split('\n')
            .map(|raw_line| {
                let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
                let range = next_line_start..next_line_start + line.len();
                next_line_start += raw_line.len() + 1;
                range
            })
            .collect::<Vec<_>>();

        let mut valid = highlights
            .iter()
            .filter(|highlight| {
                highlight.start < highlight.end
                    && highlight.end <= text.len()
                    && text.is_char_boundary(highlight.start)
                    && text.is_char_boundary(highlight.end)
            })
            .collect::<Vec<_>>();
        valid.sort_by_key(|highlight| (highlight.start, highlight.end));
        let mut spans = Vec::with_capacity(valid.len());
        for highlight in &valid {
            let first_line = line_ranges
                .partition_point(|line| line.start <= highlight.start)
                .saturating_sub(1);
            for (line_index, line) in line_ranges.iter().enumerate().skip(first_line) {
                if line.start >= highlight.end {
                    break;
                }
                let start = highlight.start.max(line.start);
                let end = highlight.end.min(line.end);
                if start < end {
                    spans.push((
                        line_index,
                        start - line.start..end - line.start,
                        highlight.kind,
                    ));
                }
            }
        }
        // Stable, so spans keep the start order within each line.
        spans.sort_by_key(|(line_index, _, _)| *line_index);
        let mut line_span_offsets = Vec::with_capacity(line_ranges.len() + 1);
        let mut next_span = 0;
        for line_index in 0..line_ranges.len() {
            line_span_offsets.push(next_span);
            while spans
                .get(next_span)
                .is_some_and(|(span_line, _, _)| *span_line == line_index)
            {
                next_span += 1;
            }
        }
        line_span_offsets.push(next_span);
        let valid_highlights = valid.len();

        Self {
            id: NEXT_DOCUMENT_ID.fetch_add(1, Ordering::Relaxed),
            text,
            truncated,
            language,
            encoding,
            wrapped,
            line_ranges,
            line_span_offsets,
            line_spans: spans
                .into_iter()
                .map(|(_, range, kind)| (range, kind))
                .collect(),
            valid_highlights,
        }
    }

    /// A single unhighlighted line shown in place of an empty file.
    pub(crate) fn placeholder(text: &str) -> Self {
        Self::new(TextPreview {
            text: text.to_string(),
            truncated: false,
            language: None,
            encoding: String::new(),
            wrapped: true,
            highlights: Vec::new(),
        })
    }

    pub(crate) fn line_count(&self) -> usize {
        self.line_ranges.len()
    }

    pub(crate) fn line_range(&self, line_index: usize) -> Option<std::ops::Range<usize>> {
        self.line_ranges.get(line_index).cloned()
    }

    /// Index of the line containing byte `offset`.
    pub(crate) fn line_for_offset(&self, offset: usize) -> usize {
        self.line_ranges
            .partition_point(|line| line.start <= offset)
            .saturating_sub(1)
    }

    /// Syntax spans of one line, relative to the line start.
    pub(crate) fn line_syntax_spans(
        &self,
        line_index: usize,
    ) -> &[(std::ops::Range<usize>, TextHighlightKind)] {
        match (
            self.line_span_offsets.get(line_index),
            self.line_span_offsets.get(line_index + 1),
        ) {
            (Some(start), Some(end)) => &self.line_spans[*start..*end],
            _ => &[],
        }
    }

    /// Service highlights that fall on valid character boundaries.
    #[cfg(test)]
    pub(crate) fn highlight_count(&self) -> usize {
        self.valid_highlights
    }
}

impl std::fmt::Debug for TextPreviewDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TextPreviewDocument")
            .field("bytes", &self.text.len())
            .field("lines", &self.line_ranges.len())
            .field("language", &self.language)
            .field("encoding", &self.encoding)
            .field("wrapped", &self.wrapped)
            .field("truncated", &self.truncated)
            .field("highlights", &self.valid_highlights)
            .finish()
    }
}

/// Find matches for one query in one document, cached across frames.
#[derive(Clone, Debug)]
pub(crate) struct TextFindMatches {
    document: u64,
    query: String,
    ranges: Arc<[std::ops::Range<usize>]>,
}

impl TextPreviewUiState {
    /// Byte ranges of every match of the current find query, scanning the
    /// document only when the document or the query changed.
    pub(crate) fn find_matches(
        &self,
        document: &TextPreviewDocument,
    ) -> Arc<[std::ops::Range<usize>]> {
        let mut cached = self.find_matches.borrow_mut();
        if let Some(matches) = cached.as_ref()
            && matches.document == document.id
            && matches.query == self.find
        {
            return Arc::clone(&matches.ranges);
        }
        let ranges: Arc<[std::ops::Range<usize>]> = match text_find_regex(&self.find) {
            Some(regex) => {
                #[cfg(test)]
                record_text_preview_scan(document.text.len());
                regex
                    .find_iter(&document.text)
                    .map(|found| found.start()..found.end())
                    .collect()
            }
            None => Arc::from([]),
        };
        *cached = Some(TextFindMatches {
            document: document.id,
            query: self.find.clone(),
            ranges: Arc::clone(&ranges),
        });
        ranges
    }
}

#[cfg(test)]
thread_local! {
    static TEXT_PREVIEW_SCANNED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_text_preview_scan(bytes: usize) {
    TEXT_PREVIEW_SCANNED_BYTES.with(|scanned| scanned.set(scanned.get() + bytes));
}

/// Bytes of preview text scanned in full (document preparation and find
/// scans) on this thread.
#[cfg(test)]
pub(crate) fn text_preview_scanned_bytes() -> usize {
    TEXT_PREVIEW_SCANNED_BYTES.with(std::cell::Cell::get)
}

pub(crate) fn syntax_highlight_style(
    kind: TextHighlightKind,
    palette: UiPalette,
) -> HighlightStyle {
    let (color, font_weight) = match (palette.dark, kind) {
        (_, TextHighlightKind::Comment) => (palette.muted, None),
        (true, TextHighlightKind::String) => (rgb(0xa8cc8c), None),
        (false, TextHighlightKind::String) => (rgb(0x397300), None),
        (true, TextHighlightKind::Number) => (rgb(0xd19a66), None),
        (false, TextHighlightKind::Number) => (rgb(0x986801), None),
        (true, TextHighlightKind::Keyword) => (rgb(0xc792ea), Some(FontWeight::SEMIBOLD)),
        (false, TextHighlightKind::Keyword) => (rgb(0x7c3aed), Some(FontWeight::SEMIBOLD)),
        (true, TextHighlightKind::Function) => (rgb(0x82aaff), None),
        (false, TextHighlightKind::Function) => (rgb(0x005cc5), None),
        (true, TextHighlightKind::Type) => (rgb(0xffcb6b), Some(FontWeight::MEDIUM)),
        (false, TextHighlightKind::Type) => (rgb(0x9a6700), Some(FontWeight::MEDIUM)),
        (_, TextHighlightKind::Variable) => (palette.text, None),
        (true, TextHighlightKind::Constant) => (rgb(0xf78c6c), None),
        (false, TextHighlightKind::Constant) => (rgb(0xb31d28), None),
        (true, TextHighlightKind::Invalid) => (rgb(0xff5370), Some(FontWeight::SEMIBOLD)),
        (false, TextHighlightKind::Invalid) => (rgb(0xb31d28), Some(FontWeight::SEMIBOLD)),
    };
    HighlightStyle {
        color: Some(color.into()),
        font_weight,
        ..Default::default()
    }
}

/// Find highlights for one line, relative to the line start. `matches` must be
/// sorted and non-overlapping, as `Regex::find_iter` yields them.
pub(crate) fn find_highlights_in_line(
    matches: &[std::ops::Range<usize>],
    line: std::ops::Range<usize>,
    active_match: Option<&std::ops::Range<usize>>,
    palette: UiPalette,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let first = matches.partition_point(|found| found.end <= line.start);
    matches[first..]
        .iter()
        .take_while(|found| found.start < line.end)
        .filter_map(|found| {
            let start = found.start.max(line.start);
            let end = found.end.min(line.end);
            (start < end).then_some((start, end, active_match == Some(found)))
        })
        .map(|(start, end, active)| {
            (
                start - line.start..end - line.start,
                HighlightStyle {
                    background_color: Some(
                        with_alpha(palette.accent, if active { 0.78 } else { 0.34 }).into(),
                    ),
                    font_weight: Some(if active {
                        FontWeight::BOLD
                    } else {
                        FontWeight::MEDIUM
                    }),
                    ..Default::default()
                },
            )
        })
        .collect()
}

/// Build the element for one visible preview line.
#[allow(clippy::too_many_arguments)]
pub(crate) fn text_preview_line(
    document: &TextPreviewDocument,
    find_matches: &[std::ops::Range<usize>],
    active_match: Option<&std::ops::Range<usize>>,
    line_index: usize,
    show_line_numbers: bool,
    gutter_width: f32,
    wrapped: bool,
    palette: UiPalette,
) -> Option<AnyElement> {
    let range = document.line_range(line_index)?;
    let syntax = document
        .line_syntax_spans(line_index)
        .iter()
        .map(|(span, kind)| (span.clone(), syntax_highlight_style(*kind, palette)))
        .collect();
    let find = find_highlights_in_line(find_matches, range.clone(), active_match, palette);
    Some(text_preview_line_element(
        line_index,
        &document.text[range],
        merge_text_highlight_layers(syntax, find),
        show_line_numbers,
        gutter_width,
        wrapped,
        palette,
    ))
}

pub(crate) fn merge_text_highlight_layers(
    mut base: Vec<(std::ops::Range<usize>, HighlightStyle)>,
    mut overlay: Vec<(std::ops::Range<usize>, HighlightStyle)>,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    base.retain(|(range, _)| range.start < range.end);
    overlay.retain(|(range, _)| range.start < range.end);
    base.sort_by_key(|(range, _)| (range.start, range.end));
    overlay.sort_by_key(|(range, _)| (range.start, range.end));

    let mut boundaries = Vec::with_capacity((base.len() + overlay.len()) * 2);
    for (range, _) in base.iter().chain(&overlay) {
        boundaries.push(range.start);
        boundaries.push(range.end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut merged = Vec::<(std::ops::Range<usize>, HighlightStyle)>::new();
    let mut base_index = 0;
    let mut overlay_index = 0;
    for boundary in boundaries.windows(2) {
        let start = boundary[0];
        let end = boundary[1];
        while base
            .get(base_index)
            .is_some_and(|(range, _)| range.end <= start)
        {
            base_index += 1;
        }
        while overlay
            .get(overlay_index)
            .is_some_and(|(range, _)| range.end <= start)
        {
            overlay_index += 1;
        }
        let base_style = base.get(base_index).and_then(|(range, style)| {
            (range.start <= start && range.end >= end).then_some(*style)
        });
        let overlay_style = overlay.get(overlay_index).and_then(|(range, style)| {
            (range.start <= start && range.end >= end).then_some(*style)
        });
        let style = match (base_style, overlay_style) {
            (None, None) => continue,
            (Some(style), None) | (None, Some(style)) => style,
            (Some(base), Some(overlay)) => HighlightStyle {
                color: overlay.color.or(base.color),
                font_weight: overlay.font_weight.or(base.font_weight),
                font_style: overlay.font_style.or(base.font_style),
                background_color: overlay.background_color.or(base.background_color),
                underline: overlay.underline.or(base.underline),
                strikethrough: overlay.strikethrough.or(base.strikethrough),
                fade_out: overlay.fade_out.or(base.fade_out),
            },
        };
        if let Some((range, previous)) = merged.last_mut()
            && range.end == start
            && *previous == style
        {
            range.end = end;
        } else {
            merged.push((start..end, style));
        }
    }
    merged
}

pub(crate) fn text_find_regex(query: &str) -> Option<regex::Regex> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .unicode(true)
        .build()
        .ok()
}

pub(crate) fn text_preview_line_element(
    line_index: usize,
    line: &str,
    highlights: Vec<(std::ops::Range<usize>, HighlightStyle)>,
    show_line_numbers: bool,
    gutter_width: f32,
    wrapped: bool,
    palette: UiPalette,
) -> AnyElement {
    let styled_line = StyledText::new(if line.is_empty() {
        "\u{00a0}".to_string()
    } else {
        line.to_string()
    })
    .with_highlights(highlights);
    div()
        .flex()
        .items_start()
        .min_w_full()
        .when(show_line_numbers, |row| {
            row.child(
                div()
                    .flex_none()
                    .w(px(gutter_width))
                    .pr_3()
                    .text_right()
                    .text_color(palette.tertiary)
                    .child((line_index + 1).to_string()),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .when(wrapped, |line| line.whitespace_normal())
                .when(!wrapped, |line| line.whitespace_nowrap())
                .child(styled_line),
        )
        .into_any_element()
}
