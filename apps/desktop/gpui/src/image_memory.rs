//! Keeps decoded images and their GPU textures bounded.
//!
//! GPUI never evicts on its own: `img(path)` without an image cache parks
//! every decoded image in the app-wide asset cache for the life of the
//! process, and every `RenderImage` that is painted stays in the window's
//! sprite atlas until someone calls `drop_image`. This module provides
//!
//! * [`BoundedImageCache`], an [`ImageCache`] that evicts least-recently-used
//!   images past an item and byte budget and releases their textures, and
//!   [`with_image_cache`], which scopes it over a subtree (including virtual
//!   lists, whose rows are laid out during prepaint);
//! * [`RetiredImages`], which releases replaced video and 3D-model frames
//!   once no presented frame can reference them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, AnyImageCache, App, AppContext, Asset, AssetLogger, Bounds, Context, ElementId,
    Entity, GlobalElementId, ImageAssetLoader, ImageCache, ImageCacheError, InspectorElementId,
    IntoElement, LayoutId, Pixels, RenderImage, Resource, WeakEntity, Window, hash,
};

/// Decoded icons and grid thumbnails kept for the file listing. Thumbnails are
/// at most 512px, so a full budget stays well under the byte bound; the item
/// bound leaves room for a few screens of scroll-back.
pub(crate) const LISTING_IMAGE_CACHE_ITEMS: usize = 384;
pub(crate) const LISTING_IMAGE_CACHE_BYTES: usize = 96 * 1024 * 1024;
/// Decoded preview images (photos, PDF pages, generated artifacts). Enough to
/// flip back and forth between neighbours without re-decoding.
pub(crate) const PREVIEW_IMAGE_CACHE_ITEMS: usize = 12;
pub(crate) const PREVIEW_IMAGE_CACHE_BYTES: usize = 160 * 1024 * 1024;

fn render_image_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .map(|frame| image.as_bytes(frame).map_or(0, <[u8]>::len))
        .sum()
}

enum CachedImage {
    Loading,
    Loaded(Result<Arc<RenderImage>, ImageCacheError>),
}

struct CacheEntry {
    image: CachedImage,
    bytes: usize,
    last_used: u64,
    last_frame: u64,
    generation: u64,
}

/// An LRU image cache bounded by item count and decoded bytes.
///
/// Images used in the current or previous frame are never evicted, so a
/// viewport that shows more images than the item budget grows the cache
/// temporarily instead of thrashing, and no texture referenced by the
/// presented frame is released.
pub(crate) struct BoundedImageCache {
    this: WeakEntity<Self>,
    max_items: usize,
    max_bytes: usize,
    entries: HashMap<u64, CacheEntry>,
    bytes: usize,
    clock: u64,
    frame: u64,
    next_generation: u64,
    #[cfg(test)]
    evicted: usize,
}

impl BoundedImageCache {
    pub(crate) fn new(max_items: usize, max_bytes: usize, cx: &mut Context<Self>) -> Self {
        cx.on_release(|cache, cx| {
            for entry in std::mem::take(&mut cache.entries).into_values() {
                if let CachedImage::Loaded(Ok(image)) = entry.image {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        Self {
            this: cx.weak_entity(),
            max_items: max_items.max(1),
            max_bytes,
            entries: HashMap::new(),
            bytes: 0,
            clock: 0,
            frame: 0,
            next_generation: 0,
            #[cfg(test)]
            evicted: 0,
        }
    }

    /// Images currently held (loading or decoded).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Decoded bytes currently held.
    #[cfg(test)]
    pub(crate) fn decoded_bytes(&self) -> usize {
        self.bytes
    }

    /// Images evicted since the cache was created.
    #[cfg(test)]
    pub(crate) fn evicted(&self) -> usize {
        self.evicted
    }

    fn begin_frame(&mut self) {
        self.frame += 1;
    }

    fn touch(&mut self, key: u64) -> Option<&mut CacheEntry> {
        self.clock += 1;
        let (clock, frame) = (self.clock, self.frame);
        let entry = self.entries.get_mut(&key)?;
        entry.last_used = clock;
        entry.last_frame = frame;
        Some(entry)
    }

    fn settle(
        &mut self,
        key: u64,
        generation: u64,
        result: Result<Arc<RenderImage>, ImageCacheError>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(entry) = self
            .entries
            .get_mut(&key)
            .filter(|entry| entry.generation == generation)
        else {
            // Evicted while decoding: the image was never painted, so there is
            // no texture to release.
            return;
        };
        if let Ok(image) = &result {
            entry.bytes = render_image_bytes(image);
            self.bytes += entry.bytes;
        }
        entry.image = CachedImage::Loaded(result);
        self.evict(window, cx);
    }

    fn evict(&mut self, window: &mut Window, cx: &mut App) {
        while self.entries.len() > self.max_items || self.bytes > self.max_bytes {
            let frame = self.frame;
            let Some(key) = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.last_frame + 1 < frame)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            let Some(entry) = self.entries.remove(&key) else {
                break;
            };
            self.bytes -= entry.bytes;
            #[cfg(test)]
            {
                self.evicted += 1;
            }
            if let CachedImage::Loaded(Ok(image)) = entry.image {
                cx.drop_image(image, Some(window));
            }
        }
    }
}

impl ImageCache for BoundedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = hash(resource);
        if let Some(entry) = self.touch(key) {
            return match &entry.image {
                CachedImage::Loading => None,
                CachedImage::Loaded(result) => Some(result.clone()),
            };
        }

        self.next_generation += 1;
        let generation = self.next_generation;
        self.clock += 1;
        self.entries.insert(
            key,
            CacheEntry {
                image: CachedImage::Loading,
                bytes: 0,
                last_used: self.clock,
                last_frame: self.frame,
                generation,
            },
        );
        self.evict(window, cx);

        let load = AssetLogger::<ImageAssetLoader>::load(resource.clone(), cx);
        let decode = cx.background_executor().spawn(load);
        let cache = self.this.clone();
        let view = window.current_view();
        window
            .spawn(cx, async move |cx| {
                let result = decode.await;
                let _ = cx.update(|window, cx| {
                    if let Some(cache) = cache.upgrade() {
                        cache.update(cx, |cache, cx| {
                            cache.settle(key, generation, result, window, cx)
                        });
                    }
                });
                cx.on_next_frame(move |_, cx| cx.notify(view));
            })
            .detach();
        None
    }
}

/// Render `child` with `cache` as the image cache for every `img` inside it.
///
/// Unlike `gpui::image_cache`, the scope also covers prepaint, which is when
/// `uniform_list` and `list` lay out their rows, and it adds no layout node of
/// its own, so wrapping an element does not change how it is sized.
pub(crate) fn with_image_cache(
    cache: &Entity<BoundedImageCache>,
    child: impl IntoElement,
) -> ImageCacheScope {
    ImageCacheScope {
        cache: cache.clone(),
        child: Some(child.into_any_element()),
    }
}

pub(crate) struct ImageCacheScope {
    cache: Entity<BoundedImageCache>,
    child: Option<AnyElement>,
}

impl IntoElement for ImageCacheScope {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for ImageCacheScope {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.cache.update(cx, |cache, _| cache.begin_frame());
        let mut child = self
            .child
            .take()
            .expect("request_layout runs once per frame");
        let image_cache = AnyImageCache::from(self.cache.clone());
        let layout_id =
            window.with_image_cache(Some(image_cache), |window| child.request_layout(window, cx));
        (layout_id, child)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        child: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let image_cache = AnyImageCache::from(self.cache.clone());
        window.with_image_cache(Some(image_cache), |window| {
            child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        child: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let image_cache = AnyImageCache::from(self.cache.clone());
        window.with_image_cache(Some(image_cache), |window| child.paint(window, cx));
    }
}

/// Video and model frames that were replaced or closed but may still be
/// referenced by the frame on screen.
///
/// An image retired before render N is still part of the scene presented
/// after render N-1, so it is released at render N+1, one render after it
/// left the scene (the same delay Zed uses for remote video frames).
#[derive(Default)]
pub(crate) struct RetiredImages {
    since_last_render: Vec<Arc<RenderImage>>,
    releasable: Vec<Arc<RenderImage>>,
    #[cfg(test)]
    pub(crate) released: Vec<gpui::ImageId>,
}

impl RetiredImages {
    pub(crate) fn retire(&mut self, image: Option<Arc<RenderImage>>) {
        self.since_last_render.extend(image);
    }

    /// Call at the start of every render of the window that painted the
    /// retired images.
    pub(crate) fn release(&mut self, window: &mut Window) {
        for image in self.releasable.drain(..) {
            #[cfg(test)]
            self.released.push(image.id);
            let _ = window.drop_image(image);
        }
        std::mem::swap(&mut self.releasable, &mut self.since_last_render);
        if !self.releasable.is_empty() {
            // Make sure the next render happens even if nothing else changes.
            window.request_animation_frame();
        }
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.since_last_render.len() + self.releasable.len()
    }
}

/// Per-window image memory state.
#[derive(Default)]
pub(crate) struct ImageMemory {
    /// Shared with the window's media player, which retires the video and
    /// model frames it replaces; the window releases them as it renders.
    pub(crate) retired: Rc<RefCell<RetiredImages>>,
    listing_cache: Option<Entity<BoundedImageCache>>,
    preview_cache: Option<Entity<BoundedImageCache>>,
}

impl ImageMemory {
    pub(crate) fn listing_cache(&mut self, cx: &mut App) -> Entity<BoundedImageCache> {
        self.listing_cache
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    BoundedImageCache::new(LISTING_IMAGE_CACHE_ITEMS, LISTING_IMAGE_CACHE_BYTES, cx)
                })
            })
            .clone()
    }

    pub(crate) fn preview_cache(&mut self, cx: &mut App) -> Entity<BoundedImageCache> {
        self.preview_cache
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    BoundedImageCache::new(PREVIEW_IMAGE_CACHE_ITEMS, PREVIEW_IMAGE_CACHE_BYTES, cx)
                })
            })
            .clone()
    }
}
