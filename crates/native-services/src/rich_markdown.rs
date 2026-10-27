//! Markdown converted to rich preview blocks: headings, paragraphs, list
//! items, quotes, code, tables and rules, with inline emphasis, code and
//! links kept as styled spans. Nothing is fetched: links stay text with a
//! destination and images are shown as their alt text.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::preview::highlight_code;
use crate::rich_preview::{
    MAX_BLOCK_TEXT, MAX_BLOCKS, RichBlock, RichBlockKind, RichSpan, RichSpanStyle, text_blocks,
};

/// The blocks a Markdown document renders as, at most [`MAX_BLOCKS`].
pub(crate) fn markdown_blocks(source: &str) -> Vec<RichBlock> {
    // Math is left out so prices such as "$5 and $10" stay prose.
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
        | Options::ENABLE_GFM
        | Options::ENABLE_DEFINITION_LIST;
    let mut builder = Builder::default();
    for event in Parser::new_ext(source, options) {
        if builder.blocks.len() >= MAX_BLOCKS {
            break;
        }
        builder.event(event);
    }
    builder.flush();
    builder.blocks.truncate(MAX_BLOCKS);
    builder.blocks
}

/// A block whose inline content is still arriving.
struct Pending {
    kind: RichBlockKind,
    level: u8,
    marker: Option<String>,
    quote_depth: u8,
    spans: Vec<RichSpan>,
}

#[derive(Default)]
struct Table {
    row: Vec<Vec<RichSpan>>,
    cell: Option<Vec<RichSpan>>,
}

#[derive(Default)]
struct Builder {
    blocks: Vec<RichBlock>,
    pending: Option<Pending>,
    strong: u16,
    emphasis: u16,
    strikethrough: u16,
    links: Vec<String>,
    image: u16,
    image_has_alt: bool,
    quote_depth: u8,
    /// Open lists, innermost last, with the next number of ordered ones.
    lists: Vec<Option<u64>>,
    /// The marker for the first block of the list item just opened.
    item_marker: Option<String>,
    definition: bool,
    footnote: Option<String>,
    code: Option<(String, String)>,
    html: Option<String>,
    table: Option<Table>,
    metadata: u16,
}

impl Builder {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text, self.style()),
            Event::Code(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                let style = RichSpanStyle {
                    code: true,
                    ..self.style()
                };
                self.text(&text, style);
            }
            Event::Html(html) => {
                if let Some(block) = &mut self.html {
                    block.push_str(&html);
                }
            }
            // Inline tags have no safe rendering; their text still arrives
            // as ordinary events.
            Event::InlineHtml(_) => {}
            Event::FootnoteReference(label) => self.text(&format!("[{label}]"), self.style()),
            Event::SoftBreak => self.text(" ", self.style()),
            Event::HardBreak => self.text("\n", self.style()),
            Event::Rule => {
                self.flush();
                self.blocks.push(RichBlock {
                    kind: RichBlockKind::Rule,
                    quote_depth: self.quote_depth,
                    ..RichBlock::default()
                });
            }
            Event::TaskListMarker(checked) => {
                let marker = Some(if checked { "[x]" } else { "[ ]" }.to_string());
                match &mut self.pending {
                    Some(pending) => pending.marker = marker,
                    None => self.item_marker = marker,
                }
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.begin(RichBlockKind::Paragraph, 0),
            Tag::Heading { level, .. } => self.begin(RichBlockKind::Heading, heading_level(level)),
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_add(1);
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split(|character: char| character.is_whitespace() || character == ',')
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::HtmlBlock => {
                self.flush();
                self.html = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len().saturating_sub(1);
                self.item_marker = Some(match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}.");
                        *number += 1;
                        marker
                    }
                    _ => ["•", "◦", "▪"][depth.min(2)].to_string(),
                });
            }
            Tag::FootnoteDefinition(label) => {
                self.flush();
                self.footnote = Some(label.to_string());
            }
            Tag::DefinitionList => self.flush(),
            Tag::DefinitionListTitle => {
                self.begin(RichBlockKind::Paragraph, 0);
                self.strong += 1;
            }
            Tag::DefinitionListDefinition => {
                self.flush();
                self.definition = true;
            }
            Tag::Table(_) => {
                self.flush();
                self.table = Some(Table::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(table) = &mut self.table {
                    table.cell = Some(Vec::new());
                }
            }
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strikethrough += 1,
            Tag::Superscript | Tag::Subscript => {}
            Tag::Link { dest_url, .. } => self.links.push(dest_url.to_string()),
            Tag::Image { .. } => {
                self.image += 1;
                self.image_has_alt = false;
            }
            Tag::MetadataBlock(_) => self.metadata += 1,
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.flush(),
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some((language, mut text)) = self.code.take() {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    truncate_text(&mut text);
                    let highlights = highlight_code(&language, &text)
                        .into_iter()
                        .filter(|highlight| highlight.end <= text.len())
                        .collect();
                    self.blocks.push(RichBlock {
                        kind: RichBlockKind::Code,
                        text,
                        level: self.list_depth(),
                        quote_depth: self.quote_depth,
                        highlights,
                        ..RichBlock::default()
                    });
                }
            }
            TagEnd::HtmlBlock => {
                if let Some(html) = self.html.take()
                    && let Ok(text) = html2text::from_read(html.as_bytes(), 100)
                {
                    let quote_depth = self.quote_depth;
                    self.blocks
                        .extend(text_blocks(&text).into_iter().map(|block| RichBlock {
                            quote_depth,
                            ..block
                        }));
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.flush();
                self.item_marker = None;
            }
            TagEnd::FootnoteDefinition => {
                self.flush();
                self.footnote = None;
            }
            TagEnd::DefinitionList => self.flush(),
            TagEnd::DefinitionListTitle => {
                self.flush();
                self.strong = self.strong.saturating_sub(1);
            }
            TagEnd::DefinitionListDefinition => {
                self.flush();
                self.definition = false;
            }
            TagEnd::Table => self.table = None,
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let cells = std::mem::take(&mut table.row);
                    let text = cells
                        .iter()
                        .map(|cell| spans_text(cell))
                        .collect::<Vec<_>>()
                        .join("  ·  ");
                    self.blocks.push(RichBlock {
                        kind: if tag == TagEnd::TableHead {
                            RichBlockKind::TableHeader
                        } else {
                            RichBlockKind::TableRow
                        },
                        text,
                        quote_depth: self.quote_depth,
                        cells,
                        ..RichBlock::default()
                    });
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    let mut cell = table.cell.take().unwrap_or_default();
                    trim_spans(&mut cell);
                    table.row.push(cell);
                }
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strikethrough = self.strikethrough.saturating_sub(1),
            TagEnd::Superscript | TagEnd::Subscript => {}
            TagEnd::Link => {
                self.links.pop();
            }
            TagEnd::Image => {
                if !self.image_has_alt {
                    self.text("image", self.style());
                }
                self.image = self.image.saturating_sub(1);
            }
            TagEnd::MetadataBlock(_) => self.metadata = self.metadata.saturating_sub(1),
        }
    }

    fn style(&self) -> RichSpanStyle {
        RichSpanStyle {
            strong: self.strong > 0,
            emphasis: self.emphasis > 0,
            strikethrough: self.strikethrough > 0,
            code: false,
            link: self.links.last().cloned(),
            image: self.image > 0,
        }
    }

    fn list_depth(&self) -> u8 {
        u8::try_from(self.lists.len()).unwrap_or(u8::MAX)
    }

    /// Starts a block, or a list item's block when inside one.
    fn begin(&mut self, kind: RichBlockKind, level: u8) {
        self.flush();
        let in_item = !self.lists.is_empty() || self.definition;
        let (kind, level, marker) = if kind == RichBlockKind::Paragraph && in_item {
            (
                RichBlockKind::ListItem,
                self.list_depth().saturating_sub(1),
                self.item_marker.take(),
            )
        } else {
            (kind, level, None)
        };
        let mut spans = Vec::new();
        if let Some(label) = self.footnote.take() {
            spans.push(RichSpan {
                text: format!("[{label}] "),
                style: RichSpanStyle::default(),
            });
        }
        self.pending = Some(Pending {
            kind,
            level,
            marker,
            quote_depth: self.quote_depth,
            spans,
        });
    }

    fn text(&mut self, text: &str, style: RichSpanStyle) {
        if self.metadata > 0 {
            return;
        }
        if let Some((_, code)) = &mut self.code {
            code.push_str(text);
            return;
        }
        if let Some(html) = &mut self.html {
            html.push_str(text);
            return;
        }
        if self.image > 0 && !text.trim().is_empty() {
            self.image_has_alt = true;
        }
        if let Some(cell) = self.table.as_mut().and_then(|table| table.cell.as_mut()) {
            push_span(cell, text, style);
            return;
        }
        if self.pending.is_none() {
            // Tight list items carry their text without a paragraph.
            self.begin(RichBlockKind::Paragraph, 0);
        }
        if let Some(pending) = &mut self.pending {
            push_span(&mut pending.spans, text, style);
        }
    }

    fn flush(&mut self) {
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        trim_spans(&mut pending.spans);
        if pending.spans.is_empty() && pending.marker.is_none() {
            return;
        }
        truncate_spans(&mut pending.spans);
        self.blocks.push(RichBlock {
            kind: pending.kind,
            text: spans_text(&pending.spans),
            spans: pending.spans,
            level: pending.level,
            marker: pending.marker,
            quote_depth: pending.quote_depth,
            ..RichBlock::default()
        });
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn push_span(spans: &mut Vec<RichSpan>, text: &str, style: RichSpanStyle) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => spans.push(RichSpan {
            text: text.to_string(),
            style,
        }),
    }
}

fn spans_text(spans: &[RichSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

/// Drops the whitespace soft breaks leave at either end of a block.
fn trim_spans(spans: &mut Vec<RichSpan>) {
    if let Some(first) = spans.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    if let Some(last) = spans.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
    spans.retain(|span| !span.text.is_empty());
}

fn truncate_text(text: &mut String) {
    if text.len() > MAX_BLOCK_TEXT {
        let mut end = MAX_BLOCK_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
}

fn truncate_spans(spans: &mut Vec<RichSpan>) {
    let mut remaining = MAX_BLOCK_TEXT;
    let mut kept = 0;
    for span in spans.iter_mut() {
        kept += 1;
        if span.text.len() > remaining {
            let mut end = remaining;
            while !span.text.is_char_boundary(end) {
                end -= 1;
            }
            span.text.truncate(end);
            span.text.push('…');
            break;
        }
        remaining -= span.text.len();
    }
    spans.truncate(kept);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> RichSpan {
        RichSpan {
            text: text.to_string(),
            style: RichSpanStyle::default(),
        }
    }

    #[test]
    fn inline_styles_and_links_become_spans_without_raw_markup() {
        let blocks = markdown_blocks(
            "# Explorie test\n\nSome **markdown** with a [link](https://example.com).\n",
        );
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, RichBlockKind::Heading);
        assert_eq!(blocks[0].level, 1);
        assert_eq!(blocks[0].text, "Explorie test");
        let paragraph = &blocks[1];
        assert_eq!(paragraph.kind, RichBlockKind::Paragraph);
        assert_eq!(paragraph.text, "Some markdown with a link.");
        assert_eq!(
            paragraph.spans,
            vec![
                plain("Some "),
                RichSpan {
                    text: "markdown".to_string(),
                    style: RichSpanStyle {
                        strong: true,
                        ..RichSpanStyle::default()
                    },
                },
                plain(" with a "),
                RichSpan {
                    text: "link".to_string(),
                    style: RichSpanStyle {
                        link: Some("https://example.com".to_string()),
                        ..RichSpanStyle::default()
                    },
                },
                plain("."),
            ]
        );
    }

    #[test]
    fn block_structure_is_kept() {
        let source = "\
## Section

*em* and `code` and ~~gone~~

- one
- two
  1. nested
- [x] done

> quoted **text**

```rust
fn main() {}
```

---

| Name | Size |
| --- | --- |
| a.txt | *1 KB* |

![diagram of the flow](https://example.com/flow.png)
";
        let blocks = markdown_blocks(source);
        let kinds = blocks.iter().map(|block| block.kind).collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                RichBlockKind::Heading,
                RichBlockKind::Paragraph,
                RichBlockKind::ListItem,
                RichBlockKind::ListItem,
                RichBlockKind::ListItem,
                RichBlockKind::ListItem,
                RichBlockKind::Paragraph,
                RichBlockKind::Code,
                RichBlockKind::Rule,
                RichBlockKind::TableHeader,
                RichBlockKind::TableRow,
                RichBlockKind::Paragraph,
            ]
        );
        assert_eq!(blocks[0].level, 2);
        let inline = &blocks[1].spans;
        assert!(inline[0].style.emphasis && inline[0].text == "em");
        assert!(
            inline
                .iter()
                .any(|span| span.style.code && span.text == "code")
        );
        assert!(
            inline
                .iter()
                .any(|span| span.style.strikethrough && span.text == "gone")
        );

        let items = &blocks[2..6];
        assert_eq!(items[0].marker.as_deref(), Some("•"));
        assert_eq!(items[0].text, "one");
        assert_eq!(items[2].marker.as_deref(), Some("1."));
        assert_eq!(items[2].level, 1);
        assert_eq!(items[2].text, "nested");
        assert_eq!(items[3].marker.as_deref(), Some("[x]"));
        assert_eq!(items[3].text, "done");

        assert_eq!(blocks[6].quote_depth, 1);
        assert_eq!(blocks[6].text, "quoted text");
        assert_eq!(blocks[7].text, "fn main() {}");
        assert!(
            !blocks[7].highlights.is_empty(),
            "code is syntax highlighted"
        );

        assert_eq!(
            blocks[9].cells,
            vec![vec![plain("Name")], vec![plain("Size")]]
        );
        assert_eq!(blocks[10].cells[0], vec![plain("a.txt")]);
        assert!(blocks[10].cells[1][0].style.emphasis);

        let image = &blocks[11].spans[0];
        assert_eq!(image.text, "diagram of the flow");
        assert!(image.style.image);
        assert!(blocks.iter().all(|block| !block.text.contains("](")));
    }

    #[test]
    fn metadata_and_html_never_render_as_markup() {
        let blocks = markdown_blocks(
            "---\ntitle: Hidden\n---\n\n<div><script>alert(1)</script><p>Inline <b>HTML</b></p></div>\n\nAfter<br>text\n",
        );
        let text = blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("title: Hidden"));
        assert!(!text.contains('<'), "no raw tags in {text:?}");
        assert!(text.contains("Inline"));
        assert!(text.contains("After"));
    }

    #[test]
    fn block_count_and_text_are_bounded() {
        let many = "para\n\n".repeat(MAX_BLOCKS + 50);
        assert_eq!(markdown_blocks(&many).len(), MAX_BLOCKS);
        let long = "é".repeat(MAX_BLOCK_TEXT);
        let blocks = markdown_blocks(&long);
        assert!(blocks[0].text.len() <= MAX_BLOCK_TEXT + '…'.len_utf8());
        assert!(blocks[0].text.ends_with('…'));
    }
}
