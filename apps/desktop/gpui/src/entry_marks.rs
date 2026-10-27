//! Status marks shown after an entry's name in list and grid rows.

use crate::*;

/// A small cloud for items whose contents live in the cloud (evicted
/// iCloud Drive files, OneDrive online-only files): opening or previewing
/// them downloads them first.
pub(crate) fn entry_cloud_badge(
    entry: &FileEntry,
    selector: String,
    size: f32,
    palette: UiPalette,
) -> Option<AnyElement> {
    entry.is_cloud_placeholder.then(|| {
        div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector.clone())
            .role(Role::Image)
            .aria_label("Stored in the cloud")
            .flex()
            .flex_none()
            .items_center()
            .child(toolbar_icon("cloud", size, palette.muted))
            .into_any_element()
    })
}

/// Rows show at most this many tag dots, like Finder.
const MAX_TAG_DOTS: usize = 3;

/// Finder-style dots for an entry's tags: filled with the tag colour, or a
/// hollow ring for tags without one. Coloured tags come first; the label
/// names every tag.
pub(crate) fn entry_tag_dots(
    entry: &FileEntry,
    selector: String,
    size: f32,
    palette: UiPalette,
) -> Option<AnyElement> {
    if entry.tags.is_empty() {
        return None;
    }
    let mut tags: Vec<&explorie_core::FinderTag> = entry.tags.iter().collect();
    tags.sort_by_key(|tag| tag.color == 0);
    let label = format!(
        "Tags: {}",
        entry
            .tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let dots = tags.into_iter().take(MAX_TAG_DOTS).map(|tag| {
        let dot = div().flex_none().w(px(size)).h(px(size)).rounded_full();
        if tag.color == 0 {
            dot.border_1().border_color(palette.muted)
        } else {
            dot.bg(rgb(finder_tag_color(tag.color).rgb))
        }
    });
    Some(
        div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector.clone())
            .role(Role::Image)
            .aria_label(label)
            .flex()
            .flex_none()
            .items_center()
            .gap(px((size * 0.3).max(2.0)))
            .children(dots)
            .into_any_element(),
    )
}
