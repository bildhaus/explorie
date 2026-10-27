//! `DirectoryWindow` behavior for entry icons.

use std::hash::Hash;

use crate::*;

/// Native icons and grid thumbnails for listing entries: the LRU caches, the
/// load queues and how many loads are running, and the render frame used to
/// drop queued loads nothing asks for any more.
pub(crate) struct EntryVisuals {
    pub(crate) icons: EntryVisualCache<EntryIconKey, EntryIconState>,
    pub(crate) icon_queue: VecDeque<EntryIconRequest>,
    pub(crate) icon_active: usize,
    #[cfg(test)]
    pub(crate) icon_loading_enabled: bool,
    pub(crate) thumbnails: EntryVisualCache<EntryThumbnailKey, EntryThumbnailState>,
    pub(crate) thumbnail_queue: VecDeque<EntryThumbnailRequest>,
    pub(crate) thumbnail_active: usize,
    pub(crate) frame: u64,
    #[cfg(test)]
    pub(crate) thumbnail_loading_enabled: bool,
}

impl Default for EntryVisuals {
    fn default() -> Self {
        Self {
            icons: EntryVisualCache::new(ENTRY_ICON_CACHE_LIMIT),
            icon_queue: VecDeque::new(),
            icon_active: 0,
            #[cfg(test)]
            icon_loading_enabled: false,
            thumbnails: EntryVisualCache::new(ENTRY_THUMBNAIL_CACHE_LIMIT),
            thumbnail_queue: VecDeque::new(),
            thumbnail_active: 0,
            frame: 0,
            #[cfg(test)]
            thumbnail_loading_enabled: false,
        }
    }
}

/// Native icon extraction is serialized by the preview service, so a couple of
/// requests in flight only hide the round trip between one icon and the next.
const ENTRY_ICON_MAX_CONCURRENT: usize = 3;

impl DirectoryWindow {
    /// Start a new frame for icon and thumbnail requests. Queued loads that
    /// the next frame does not request again are dropped before they start.
    pub(crate) fn begin_entry_visual_frame(&mut self) {
        self.visuals.frame = self.visuals.frame.wrapping_add(1);
    }

    /// Keep at least twice the rows or cells on screen cached, so scrolling a
    /// full viewport never evicts what is about to be drawn again.
    pub(crate) fn reserve_entry_visuals(&mut self, visible: usize) {
        let wanted = visible.saturating_mul(2);
        self.visuals.icons.ensure_capacity(wanted);
        self.visuals.thumbnails.ensure_capacity(wanted);
    }

    pub(crate) fn clear_entry_visuals(&mut self) {
        self.visuals.icons.clear();
        self.visuals.icon_queue.clear();
        self.visuals.thumbnails.clear();
        self.visuals.thumbnail_queue.clear();
    }

    pub(crate) fn cached_entry_icon(
        &mut self,
        entry: &FileEntry,
        cx: &mut Context<Self>,
    ) -> Option<PathBuf> {
        #[cfg(test)]
        if !self.visuals.icon_loading_enabled {
            return None;
        }
        let key = EntryIconKey::for_entry(entry);
        let frame = self.visuals.frame;
        if let Some(state) = self.visuals.icons.request(&key, frame) {
            // Paths were validated once, when their load finished.
            return match state {
                EntryIconState::Ready(path) => path.clone(),
                EntryIconState::Loading => None,
            };
        }
        self.visuals
            .icons
            .insert(key.clone(), EntryIconState::Loading, frame);
        self.visuals.icon_queue.push_back(EntryIconRequest {
            key,
            path: entry.path.clone(),
        });
        prune_entry_visual_queue(
            &mut self.visuals.icon_queue,
            &mut self.visuals.icons,
            frame,
            |request| &request.key,
            |state| matches!(state, EntryIconState::Loading),
        );
        self.start_entry_icon_tasks(cx);
        None
    }

    pub(crate) fn start_entry_icon_tasks(&mut self, cx: &mut Context<Self>) {
        while self.visuals.icon_active < ENTRY_ICON_MAX_CONCURRENT {
            let Some(request) = next_entry_visual_request(
                &mut self.visuals.icon_queue,
                &mut self.visuals.icons,
                self.visuals.frame,
                |request| &request.key,
                |state| matches!(state, EntryIconState::Loading),
            ) else {
                break;
            };
            self.visuals.icon_active += 1;
            let task = self.services.previews.file_icon(request.path);
            let icon = cx.background_spawn(async move {
                task.await.ok().flatten().filter(|path| path.is_file())
            });
            let key = request.key;
            cx.spawn(async move |this, cx| {
                let icon = icon.await;
                let _ = this.update(cx, |view, cx| {
                    view.visuals.icon_active = view.visuals.icon_active.saturating_sub(1);
                    if matches!(view.visuals.icons.get(&key), Some(EntryIconState::Loading)) {
                        view.visuals.icons.set(&key, EntryIconState::Ready(icon));
                        cx.notify();
                    }
                    view.start_entry_icon_tasks(cx);
                });
            })
            .detach();
        }
    }

    pub(crate) fn cached_entry_thumbnail(
        &mut self,
        entry: &FileEntry,
        max_size: u32,
        cx: &mut Context<Self>,
    ) -> Option<EntryThumbnailState> {
        #[cfg(test)]
        if !self.visuals.thumbnail_loading_enabled {
            return None;
        }
        if !entry_supports_grid_thumbnail(entry) {
            return None;
        }

        let key = EntryThumbnailKey::for_entry(entry, max_size);
        let frame = self.visuals.frame;
        if let Some(state) = self.visuals.thumbnails.request(&key, frame) {
            return Some(state.clone());
        }
        self.visuals
            .thumbnails
            .insert(key.clone(), EntryThumbnailState::Loading, frame);
        self.visuals
            .thumbnail_queue
            .push_back(EntryThumbnailRequest {
                key,
                path: entry.path.clone(),
                max_size,
            });
        prune_entry_visual_queue(
            &mut self.visuals.thumbnail_queue,
            &mut self.visuals.thumbnails,
            frame,
            |request| &request.key,
            |state| matches!(state, EntryThumbnailState::Loading),
        );
        self.start_entry_thumbnail_tasks(cx);
        Some(EntryThumbnailState::Loading)
    }

    pub(crate) fn start_entry_thumbnail_tasks(&mut self, cx: &mut Context<Self>) {
        while self.visuals.thumbnail_active < ENTRY_THUMBNAIL_MAX_CONCURRENT {
            let Some(request) = next_entry_visual_request(
                &mut self.visuals.thumbnail_queue,
                &mut self.visuals.thumbnails,
                self.visuals.frame,
                |request| &request.key,
                |state| matches!(state, EntryThumbnailState::Loading),
            ) else {
                break;
            };
            self.visuals.thumbnail_active += 1;
            let task = self
                .services
                .previews
                .thumbnail(request.path, request.max_size);
            let thumbnail = cx.background_spawn(async move {
                task.await.ok().flatten().filter(|path| path.is_file())
            });
            let key = request.key;
            cx.spawn(async move |this, cx| {
                let thumbnail = thumbnail.await;
                let _ = this.update(cx, |view, cx| {
                    view.visuals.thumbnail_active = view.visuals.thumbnail_active.saturating_sub(1);
                    if matches!(
                        view.visuals.thumbnails.get(&key),
                        Some(EntryThumbnailState::Loading)
                    ) {
                        let state = thumbnail
                            .map(EntryThumbnailState::Ready)
                            .unwrap_or(EntryThumbnailState::Failed);
                        view.visuals.thumbnails.set(&key, state);
                        cx.notify();
                    }
                    view.start_entry_thumbnail_tasks(cx);
                });
            })
            .detach();
        }
    }
}

/// Drop queued requests the latest frame did not repeat, with their
/// placeholders, once rapid scrolling has queued far more than is visible.
fn prune_entry_visual_queue<R, K: Clone + Eq + Hash, V>(
    queue: &mut VecDeque<R>,
    cache: &mut EntryVisualCache<K, V>,
    frame: u64,
    key: impl Fn(&R) -> &K,
    loading: impl Fn(&V) -> bool,
) {
    if queue.len() <= cache.capacity().saturating_mul(2) {
        return;
    }
    queue.retain(|request| {
        let key = key(request);
        match cache.pending_frame(key) {
            Some(requested) if requested == frame => true,
            Some(_) => {
                if cache.get(key).is_some_and(&loading) {
                    cache.remove(key);
                }
                false
            }
            None => false,
        }
    });
}

/// Pop the next request worth starting: its entry is still cached, still
/// loading and not started yet, and was requested in the latest frame.
/// Requests the latest frame did not repeat have scrolled out of view; they
/// are dropped with their placeholder so a later frame can ask again.
fn next_entry_visual_request<R, K: Clone + Eq + Hash, V>(
    queue: &mut VecDeque<R>,
    cache: &mut EntryVisualCache<K, V>,
    frame: u64,
    key: impl Fn(&R) -> &K,
    loading: impl Fn(&V) -> bool,
) -> Option<R> {
    while let Some(request) = queue.pop_front() {
        let key = key(&request);
        match cache.pending_frame(key) {
            Some(requested) if requested == frame && cache.get(key).is_some_and(&loading) => {
                cache.mark_started(key);
                return Some(request);
            }
            Some(_) if cache.get(key).is_some_and(&loading) => cache.remove(key),
            _ => {}
        }
    }
    None
}

/// A least-recently-used map with O(1) lookups, touches and evictions: a
/// slab of slots threaded on a doubly linked recency list. Each slot records
/// the frame that last requested it, and eviction never removes a slot the
/// current frame requested, so a viewport larger than the capacity grows the
/// cache instead of evicting and re-requesting its own rows.
pub(crate) struct EntryVisualCache<K, V> {
    slots: Vec<Option<EntryVisualSlot<K, V>>>,
    index: HashMap<K, usize>,
    free: Vec<usize>,
    /// Most recently used.
    head: Option<usize>,
    /// Least recently used.
    tail: Option<usize>,
    capacity: usize,
}

struct EntryVisualSlot<K, V> {
    key: K,
    value: V,
    frame: u64,
    /// Queued for loading and not started yet.
    pending: bool,
    newer: Option<usize>,
    older: Option<usize>,
}

impl<K: Clone + Eq + Hash, V> EntryVisualCache<K, V> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            slots: Vec::new(),
            index: HashMap::new(),
            free: Vec::new(),
            head: None,
            tail: None,
            capacity,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.index.len()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn ensure_capacity(&mut self, capacity: usize) {
        self.capacity = self.capacity.max(capacity);
    }

    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.index.get(key).map(|&slot| &self.slot(slot).value)
    }

    /// Look up `key` for `frame`, marking it most recently used.
    pub(crate) fn request(&mut self, key: &K, frame: u64) -> Option<&V> {
        let slot = *self.index.get(key)?;
        self.slot_mut(slot).frame = frame;
        self.promote(slot);
        Some(&self.slot(slot).value)
    }

    /// Insert a queued placeholder (or replace `key`) as most recently used,
    /// then evict down to capacity.
    pub(crate) fn insert(&mut self, key: K, value: V, frame: u64) {
        self.remove(&key);
        let entry = EntryVisualSlot {
            key: key.clone(),
            value,
            frame,
            pending: true,
            newer: None,
            older: None,
        };
        let slot = match self.free.pop() {
            Some(slot) => {
                self.slots[slot] = Some(entry);
                slot
            }
            None => {
                self.slots.push(Some(entry));
                self.slots.len() - 1
            }
        };
        self.index.insert(key, slot);
        self.link_front(slot);
        self.evict(frame);
    }

    /// Replace the value of a cached key, marking it most recently used.
    pub(crate) fn set(&mut self, key: &K, value: V) {
        if let Some(&slot) = self.index.get(key) {
            let entry = self.slot_mut(slot);
            entry.value = value;
            entry.pending = false;
            self.promote(slot);
        }
    }

    pub(crate) fn remove(&mut self, key: &K) {
        if let Some(slot) = self.index.remove(key) {
            self.unlink(slot);
            self.slots[slot] = None;
            self.free.push(slot);
        }
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        let removed: Vec<K> = self
            .slots
            .iter()
            .flatten()
            .filter(|entry| !keep(&entry.key, &entry.value))
            .map(|entry| entry.key.clone())
            .collect();
        for key in removed {
            self.remove(&key);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.index.clear();
        self.free.clear();
        self.head = None;
        self.tail = None;
    }

    #[cfg(test)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.slots
            .iter()
            .flatten()
            .map(|entry| (&entry.key, &entry.value))
    }

    #[cfg(test)]
    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }

    /// The frame that last requested a queued, not yet started key.
    fn pending_frame(&self, key: &K) -> Option<u64> {
        let entry = self.slot(*self.index.get(key)?);
        entry.pending.then_some(entry.frame)
    }

    fn mark_started(&mut self, key: &K) {
        if let Some(&slot) = self.index.get(key) {
            self.slot_mut(slot).pending = false;
        }
    }

    fn evict(&mut self, frame: u64) {
        while self.len() > self.capacity {
            let Some(oldest) = self.tail else {
                break;
            };
            if self.slot(oldest).frame == frame {
                // Everything left is on screen now.
                break;
            }
            let key = self.slot(oldest).key.clone();
            self.remove(&key);
        }
    }

    fn slot(&self, slot: usize) -> &EntryVisualSlot<K, V> {
        self.slots[slot].as_ref().expect("indexed slot is occupied")
    }

    fn slot_mut(&mut self, slot: usize) -> &mut EntryVisualSlot<K, V> {
        self.slots[slot].as_mut().expect("indexed slot is occupied")
    }

    fn promote(&mut self, slot: usize) {
        if self.head != Some(slot) {
            self.unlink(slot);
            self.link_front(slot);
        }
    }

    fn link_front(&mut self, slot: usize) {
        let head = self.head;
        let entry = self.slot_mut(slot);
        entry.newer = None;
        entry.older = head;
        match head {
            Some(head) => self.slot_mut(head).newer = Some(slot),
            None => self.tail = Some(slot),
        }
        self.head = Some(slot);
    }

    fn unlink(&mut self, slot: usize) {
        let (newer, older) = {
            let entry = self.slot(slot);
            (entry.newer, entry.older)
        };
        match newer {
            Some(newer) => self.slot_mut(newer).older = older,
            None => self.head = older,
        }
        match older {
            Some(older) => self.slot_mut(older).newer = newer,
            None => self.tail = newer,
        }
        let entry = self.slot_mut(slot);
        entry.newer = None;
        entry.older = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(cache: &EntryVisualCache<u32, &'static str>) -> Vec<u32> {
        let mut order = Vec::new();
        let mut slot = cache.head;
        while let Some(current) = slot {
            order.push(cache.slot(current).key);
            slot = cache.slot(current).older;
        }
        order
    }

    #[test]
    fn requests_promote_and_eviction_takes_the_least_recent_key() {
        let mut cache = EntryVisualCache::new(3);
        for key in 1..=3 {
            cache.insert(key, "ready", 1);
        }
        assert_eq!(keys(&cache), [3, 2, 1]);
        assert_eq!(cache.request(&1, 2), Some(&"ready"));
        assert_eq!(keys(&cache), [1, 3, 2]);
        cache.insert(4, "ready", 2);
        assert_eq!(keys(&cache), [4, 1, 3]);
        assert!(cache.get(&2).is_none());
        cache.set(&3, "updated");
        assert_eq!(keys(&cache), [3, 4, 1]);
        cache.remove(&4);
        assert_eq!(keys(&cache), [3, 1]);
        cache.retain(|key, _| *key != 1);
        assert_eq!(keys(&cache), [3]);
        cache.insert(5, "ready", 2);
        cache.insert(6, "ready", 2);
        assert_eq!(keys(&cache), [6, 5, 3]);
        assert_eq!(cache.len(), 3);
        cache.clear();
        assert!(keys(&cache).is_empty() && cache.values().next().is_none());
    }

    #[test]
    fn keys_requested_this_frame_are_never_evicted() {
        let mut cache = EntryVisualCache::new(2);
        cache.insert(1, "old", 1);
        for key in 2..=5 {
            cache.insert(key, "visible", 2);
        }
        // Only the key from an earlier frame could go; the rest are on screen.
        assert_eq!(keys(&cache), [5, 4, 3, 2]);
        cache.ensure_capacity(8);
        assert_eq!(cache.capacity(), 8);
        cache.ensure_capacity(4);
        assert_eq!(cache.capacity(), 8);
    }

    #[test]
    fn queued_requests_start_once_for_the_latest_frame_and_stale_ones_drop() {
        let mut cache = EntryVisualCache::new(16);
        let mut queue = VecDeque::new();
        for key in [1, 2, 3] {
            cache.insert(key, "loading", 1);
            queue.push_back(key);
        }
        cache.insert(4, "ready", 1);
        queue.push_back(4);
        // Frame 2 still shows 1 and 3; 2 scrolled away.
        cache.request(&1, 2);
        cache.request(&3, 2);
        queue.push_back(1);

        let next = |queue: &mut VecDeque<u32>, cache: &mut EntryVisualCache<u32, &str>| {
            next_entry_visual_request(queue, cache, 2, |key| key, |state| *state == "loading")
        };
        assert_eq!(next(&mut queue, &mut cache), Some(1));
        assert_eq!(next(&mut queue, &mut cache), Some(3));
        // 2 was dropped with its placeholder; 4 finished; 1 already started.
        assert_eq!(next(&mut queue, &mut cache), None);
        assert!(cache.get(&2).is_none());
        assert_eq!(cache.get(&4), Some(&"ready"));
        assert_eq!(cache.get(&1), Some(&"loading"));
    }
}
