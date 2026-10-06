//! Rendering for structured preview blocks, including Markdown's headings,
//! lists, quotes, code, tables and inline styles.

use std::ops::Range;

use explorie_native_services::{RichBlock, RichSpan};
use gpui::{FontStyle, InteractiveText, StrikethroughStyle, UnderlineStyle};

use crate::*;

/// A block's inline text laid out for [`StyledText`]: highlight ranges for
/// the styled spans, monospace ranges for code and the link targets.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct InlineText {
    pub(crate) text: String,
    pub(crate) highlights: Vec<(Range<usize>, HighlightStyle)>,
    pub(crate) code: Vec<Range<usize>>,
    pub(crate) links: Vec<(Range<usize>, String)>,
}

pub(crate) fn inline_text(spans: &[RichSpan], palette: UiPalette) -> InlineText {
    let mut inline = InlineText::default();
    for span in spans {
        let start = inline.text.len();
        inline.text.push_str(&span.text);
        let range = start..inline.text.len();
        let style = &span.style;
        let mut highlight = HighlightStyle::default();
        if style.strong {
            highlight.font_weight = Some(FontWeight::BOLD);
        }
        if style.emphasis || style.image {
            highlight.font_style = Some(FontStyle::Italic);
        }
        if style.image {
            highlight.color = Some(palette.muted.into());
        }
        if style.strikethrough {
            highlight.strikethrough = Some(StrikethroughStyle {
                thickness: px(1.0),
                color: None,
            });
        }
        if style.code {
            highlight.background_color = Some(palette.control.into());
            inline.code.push(range.clone());
        }
        if let Some(link) = &style.link {
            highlight.color = Some(palette.accent.into());
            highlight.underline = Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(palette.accent.into()),
                wavy: false,
            });
            inline.links.push((range.clone(), link.clone()));
        }
        if highlight != HighlightStyle::default() {
            inline.highlights.push((range, highlight));
        }
    }
    inline
}

/// Links open in the system browser on an explicit click, and only for web
/// and mail destinations; the preview itself never fetches anything.
pub(crate) fn opens_externally(link: &str) -> bool {
    let link = link.trim().to_ascii_lowercase();
    ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| link.starts_with(scheme))
}

fn inline_element(id: ElementId, spans: &[RichSpan], palette: UiPalette) -> AnyElement {
    let InlineText {
        text,
        highlights,
        code,
        links,
    } = inline_text(spans, palette);
    let monospace: SharedString = monospace_font_family().into();
    let styled = StyledText::new(text)
        .with_highlights(highlights)
        .with_font_family_overrides(code.into_iter().map(|range| (range, monospace.clone())));
    if links.is_empty() {
        return styled.into_any_element();
    }
    let (ranges, targets): (Vec<_>, Vec<_>) = links.into_iter().unzip();
    let tooltip_ranges = ranges.clone();
    let tooltip_targets = targets.clone();
    InteractiveText::new(id, styled)
        .on_click(ranges, move |index, _, cx| {
            if let Some(link) = targets.get(index).filter(|link| opens_externally(link)) {
                cx.open_url(link);
            }
        })
        .tooltip(move |index, _, cx| {
            let link = tooltip_ranges
                .iter()
                .position(|range| range.contains(&index))
                .and_then(|position| tooltip_targets.get(position))?;
            Some(app_tooltip(link.clone(), palette, cx))
        })
        .into_any_element()
}

/// Renders one preview block. `index` keeps element ids and test selectors
/// unique within the preview.
pub(crate) fn rich_block_element(index: usize, block: RichBlock, palette: UiPalette) -> AnyElement {
    let content = |id: &str, spans: &[RichSpan], text: String| -> AnyElement {
        if spans.is_empty() {
            return text.into_any_element();
        }
        let element = inline_element(
            ElementId::Name(format!("{id}-{index}").into()),
            spans,
            palette,
        );
        if spans.iter().any(|span| span.style.link.is_some()) {
            div()
                .debug_selector(move || format!("rich-link-{index}"))
                .child(element)
                .into_any_element()
        } else {
            element
        }
    };
    let row = div().w_full().min_w_0().text_color(palette.text);
    let element = match block.kind {
        RichBlockKind::Heading => {
            let level = block.level;
            row.debug_selector(move || format!("rich-heading-{level}-{index}"))
                .mt_2()
                .font_weight(FontWeight::SEMIBOLD)
                .map(|heading| match level {
                    1 => heading.text_xl(),
                    2 => heading.text_lg(),
                    3 => heading.text_base(),
                    _ => heading.text_sm(),
                })
                .child(content("rich-inline", &block.spans, block.text))
        }
        RichBlockKind::Paragraph => row
            .debug_selector(move || format!("rich-paragraph-{index}"))
            .text_sm()
            .line_height(px(21.0))
            .child(content("rich-inline", &block.spans, block.text)),
        RichBlockKind::ListItem => row
            .debug_selector(move || format!("rich-list-item-{index}"))
            .flex()
            .gap_2()
            .pl(px(f32::from(block.level) * 16.0))
            .text_sm()
            .line_height(px(21.0))
            .child(
                div()
                    .flex_none()
                    .min_w(px(14.0))
                    .text_color(palette.muted)
                    .child(block.marker.unwrap_or_default()),
            )
            .child(div().flex_1().min_w_0().child(content(
                "rich-inline",
                &block.spans,
                block.text,
            ))),
        RichBlockKind::Code => {
            let highlights = block
                .highlights
                .iter()
                .filter(|highlight| {
                    highlight.start < highlight.end
                        && highlight.end <= block.text.len()
                        && block.text.is_char_boundary(highlight.start)
                        && block.text.is_char_boundary(highlight.end)
                })
                .map(|highlight| {
                    (
                        highlight.start..highlight.end,
                        syntax_highlight_style(highlight.kind, palette),
                    )
                })
                .collect::<Vec<_>>();
            row.debug_selector(move || format!("rich-code-{index}"))
                .ml(px(f32::from(block.level) * 16.0))
                .p_2()
                .rounded_md()
                .bg(palette.control)
                .font_family(monospace_font_family())
                .text_xs()
                .child(StyledText::new(block.text).with_highlights(highlights))
        }
        RichBlockKind::Rule => div()
            .debug_selector(move || format!("rich-rule-{index}"))
            .w_full()
            .h(px(1.0))
            .my_1()
            .bg(palette.border),
        RichBlockKind::TableHeader | RichBlockKind::TableRow if !block.cells.is_empty() => {
            let header = block.kind == RichBlockKind::TableHeader;
            row.debug_selector(move || format!("rich-table-row-{index}"))
                .flex()
                .text_xs()
                .when(header, |row| {
                    row.bg(palette.control).font_weight(FontWeight::SEMIBOLD)
                })
                .when(!header, |row| row.border_b_1().border_color(palette.border))
                .children(block.cells.iter().enumerate().map(|(column, cell)| {
                    div().flex_1().min_w_0().px_2().py_1().child(content(
                        &format!("rich-cell-{column}"),
                        cell,
                        String::new(),
                    ))
                }))
        }
        RichBlockKind::TableHeader => row
            .px_2()
            .py_1()
            .bg(palette.control)
            .font_family(monospace_font_family())
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .child(block.text),
        RichBlockKind::TableRow => row
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(palette.border)
            .font_family(monospace_font_family())
            .text_xs()
            .child(block.text),
        RichBlockKind::Metadata => row.text_xs().text_color(palette.muted).child(block.text),
    };
    (0..block.quote_depth).fold(element.into_any_element(), |element, depth| {
        div()
            .when(depth == 0, |quote| {
                quote.debug_selector(move || format!("rich-quote-{index}"))
            })
            .w_full()
            .pl_3()
            .border_l_2()
            .border_color(palette.border)
            .text_color(palette.muted)
            .child(element)
            .into_any_element()
    })
}

#[cfg(test)]
mod tests {
    use explorie_native_services::RichSpanStyle;

    use super::*;

    #[test]
    fn inline_spans_map_to_highlights_code_fonts_and_links() {
        let palette = UiPalette::for_settings(&AppSettings::default(), WindowAppearance::Light);
        let span = |text: &str, style: RichSpanStyle| RichSpan {
            text: text.to_string(),
            style,
        };
        let inline = inline_text(
            &[
                span("Some ", RichSpanStyle::default()),
                span(
                    "bold",
                    RichSpanStyle {
                        strong: true,
                        ..RichSpanStyle::default()
                    },
                ),
                span(
                    " code",
                    RichSpanStyle {
                        code: true,
                        ..RichSpanStyle::default()
                    },
                ),
                span(
                    " link",
                    RichSpanStyle {
                        link: Some("https://example.com".to_string()),
                        ..RichSpanStyle::default()
                    },
                ),
            ],
            palette,
        );
        assert_eq!(inline.text, "Some bold code link");
        assert_eq!(inline.highlights.len(), 3);
        assert_eq!(inline.highlights[0].0, 5..9);
        assert_eq!(inline.highlights[0].1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(inline.code, vec![9..14]);
        assert_eq!(
            inline.links,
            vec![(14..19, "https://example.com".to_string())]
        );
        assert!(inline.highlights[2].1.underline.is_some());
    }

    #[test]
    fn only_web_and_mail_links_open_externally() {
        assert!(opens_externally("https://example.com"));
        assert!(opens_externally("HTTP://example.com"));
        assert!(opens_externally("mailto:ada@example.com"));
        assert!(!opens_externally("file:///etc/passwd"));
        assert!(!opens_externally("javascript:alert(1)"));
        assert!(!opens_externally("../other.md"));
    }
}
