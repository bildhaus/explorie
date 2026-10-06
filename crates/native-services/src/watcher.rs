use crate::{
    ErrorCode, ServiceContext, ServiceError, ServiceEvent, ServiceResult, WatcherEvent,
    WatcherState,
};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::task::{Context as TaskContext, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

pub const COALESCE_DELAY: Duration = Duration::from_millis(200);
/// Past this many changed paths in one batch, the batch reports the watched
/// roots instead. Consumers treat a changed root as "re-list everything",
/// which is also how backend overflow and rescan notices are delivered.
const MAX_CHANGED_PATHS: usize = 4_096;

#[derive(Clone)]
pub struct WatcherService {
    context: ServiceContext,
    inner: Arc<WatcherInner>,
}

struct WatcherInner {
    next_id: AtomicU64,
    registrations: Mutex<HashMap<String, Registration>>,
    subscriptions: Mutex<HashMap<u64, WatchSubscription>>,
}

struct Registration {
    _watcher: notify::RecommendedWatcher,
    subscribers: HashMap<u64, mpsc::Sender<Result<Event, String>>>,
}

pub struct WatchSubscription {
    inner: Weak<WatcherInner>,
    keys: Vec<String>,
    paths: Vec<PathBuf>,
    cancelled: Arc<AtomicBool>,
    events: Arc<WatchEventQueue>,
    id: u64,
}

#[derive(Default)]
struct WatchEventQueue {
    /// The subscription's watched paths, reported when merged events overflow.
    roots: Vec<PathBuf>,
    events: Mutex<VecDeque<WatcherEvent>>,
    waker: Mutex<Option<Waker>>,
    closed: AtomicBool,
}

impl WatchEventQueue {
    fn publish(&self, event: WatcherEvent) {
        let mut queue = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let merged = if event.state == WatcherState::Changed {
            queue.back_mut().is_some_and(|pending| {
                if pending.state != WatcherState::Changed
                    || pending.registration_id != event.registration_id
                {
                    return false;
                }
                pending.paths.extend(event.paths.iter().cloned());
                pending.paths.sort();
                pending.paths.dedup();
                if pending.paths.len() > MAX_CHANGED_PATHS {
                    // Dropping paths would hide changes; report the roots.
                    pending.paths.clone_from(&self.roots);
                }
                true
            })
        } else {
            false
        };
        if !merged {
            queue.push_back(event);
        }
        drop(queue);
        if let Some(waker) = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            waker.wake();
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(waker) = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            waker.wake();
        }
    }
}

pub struct WatchNext<'a> {
    subscription: &'a WatchSubscription,
}

impl Future for WatchNext<'_> {
    type Output = Option<WatcherEvent>;

    fn poll(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let queue = &self.subscription.events;
        if let Some(event) = queue
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
        {
            return Poll::Ready(Some(event));
        }
        if queue.closed.load(Ordering::Acquire) {
            return Poll::Ready(None);
        }

        *queue
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(context.waker().clone());

        if let Some(event) = queue
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
        {
            Poll::Ready(Some(event))
        } else if queue.closed.load(Ordering::Acquire) {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

impl WatcherService {
    pub(crate) fn new(context: ServiceContext) -> Self {
        Self {
            context,
            inner: Arc::new(WatcherInner {
                next_id: AtomicU64::new(0),
                registrations: Mutex::new(HashMap::new()),
                subscriptions: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Register one non-recursive native watcher for a normalized path set.
    /// Equal paths share one OS watcher while each subscription gets its own
    /// coalescing worker.
    pub fn watch(&self, paths: Vec<PathBuf>) -> ServiceResult<WatchSubscription> {
        self.watch_with_mode(paths, RecursiveMode::NonRecursive)
    }

    /// Register recursive native watchers for search roots. Recursive and
    /// non-recursive registrations never share an OS watcher accidentally.
    pub fn watch_recursive(&self, paths: Vec<PathBuf>) -> ServiceResult<WatchSubscription> {
        self.watch_with_mode(paths, RecursiveMode::Recursive)
    }

    fn watch_with_mode(
        &self,
        paths: Vec<PathBuf>,
        recursive_mode: RecursiveMode,
    ) -> ServiceResult<WatchSubscription> {
        let paths = normalize_paths(paths);
        if paths.is_empty() {
            return Err(ServiceError::new(
                ErrorCode::InvalidInput,
                "At least one path is required for a filesystem watcher",
            ));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let mode_key = match recursive_mode {
            RecursiveMode::Recursive => "recursive",
            RecursiveMode::NonRecursive => "direct",
        };
        let keys: Vec<String> = paths
            .iter()
            .map(|path| format!("{mode_key}:{}", normalize_path(path)))
            .collect();
        let cancelled = Arc::new(AtomicBool::new(false));
        let event_queue = Arc::new(WatchEventQueue {
            roots: paths.clone(),
            ..WatchEventQueue::default()
        });
        let (sender, receiver) = mpsc::channel();
        let mut registrations = self
            .inner
            .registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (path, key) in paths.iter().zip(&keys) {
            if let Some(registration) = registrations.get_mut(key) {
                registration.subscribers.insert(id, sender.clone());
                continue;
            }

            let callback_inner = Arc::downgrade(&self.inner);
            let callback_key = key.clone();
            let watcher =
                match notify::recommended_watcher(move |event: Result<Event, notify::Error>| {
                    let Some(inner) = callback_inner.upgrade() else {
                        return;
                    };
                    let event = event.map_err(|error| error.to_string());
                    let mut registrations = inner
                        .registrations
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let Some(registration) = registrations.get_mut(&callback_key) else {
                        return;
                    };
                    registration
                        .subscribers
                        .retain(|_, subscriber| subscriber.send(event.clone()).is_ok());
                }) {
                    Ok(watcher) => watcher,
                    Err(error) => {
                        for rollback_key in &keys {
                            if let Some(registration) = registrations.get_mut(rollback_key) {
                                registration.subscribers.remove(&id);
                            }
                        }
                        registrations
                            .retain(|_, registration| !registration.subscribers.is_empty());
                        return Err(watcher_error("create filesystem watcher", error));
                    }
                };
            let mut watcher = watcher;
            if let Err(error) = watcher.watch(path, recursive_mode) {
                for rollback_key in &keys {
                    if let Some(registration) = registrations.get_mut(rollback_key) {
                        registration.subscribers.remove(&id);
                    }
                }
                registrations.retain(|_, registration| !registration.subscribers.is_empty());
                return Err(watcher_error("register filesystem watcher", error));
            }
            let mut subscribers = HashMap::new();
            subscribers.insert(id, sender.clone());
            registrations.insert(
                key.clone(),
                Registration {
                    _watcher: watcher,
                    subscribers,
                },
            );
        }

        let events = self.context.events();
        let worker_paths = paths.clone();
        let worker_path_aliases = canonical_watch_path_aliases(&paths);
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_events = Arc::clone(&event_queue);
        thread::spawn(move || {
            coalescing_worker(
                id,
                worker_paths,
                worker_path_aliases,
                receiver,
                events,
                worker_events,
                worker_cancelled,
            )
        });
        Ok(WatchSubscription {
            inner: Arc::downgrade(&self.inner),
            keys,
            paths,
            cancelled,
            events: event_queue,
            id,
        })
    }

    /// Register a watcher without performing notify setup on the caller's executor.
    pub fn watch_task(&self, paths: Vec<PathBuf>) -> crate::BlockingTask<WatchSubscription> {
        let service = self.clone();
        self.context.spawn_blocking(move || service.watch(paths))
    }

    pub fn watch_recursive_task(
        &self,
        paths: Vec<PathBuf>,
    ) -> crate::BlockingTask<WatchSubscription> {
        let service = self.clone();
        self.context
            .spawn_blocking(move || service.watch_recursive(paths))
    }

    /// Register a watcher owned by this service. This is the command/host
    /// form of [`watch`](Self::watch): the registration remains alive until
    /// [`unwatch`](Self::unwatch) is called.
    pub fn watch_paths(&self, paths: Vec<PathBuf>) -> ServiceResult<u64> {
        let subscription = self.watch(paths)?;
        let id = subscription.id();
        self.inner
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, subscription);
        Ok(id)
    }

    /// Register a host-owned watcher without running notify setup on the
    /// caller's executor.
    pub fn watch_paths_task(&self, paths: Vec<PathBuf>) -> crate::BlockingTask<u64> {
        let service = self.clone();
        self.context
            .spawn_blocking(move || service.watch_paths(paths))
    }

    /// Drop a host-owned watcher and publish its terminal status.
    pub fn unwatch(&self, id: u64) -> bool {
        let subscription = self
            .inner
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        let Some(subscription) = subscription else {
            return false;
        };
        subscription.cancelled.store(true, Ordering::Release);
        self.context
            .events()
            .publish(ServiceEvent::Watcher(WatcherEvent {
                registration_id: id,
                state: WatcherState::Stopped,
                paths: subscription.paths.clone(),
                error: None,
            }));
        drop(subscription);
        true
    }

    pub fn unwatch_task(&self, id: u64) -> crate::BlockingTask<bool> {
        let service = self.clone();
        self.context.spawn_blocking(move || Ok(service.unwatch(id)))
    }

    pub fn registration_count(&self) -> usize {
        self.inner
            .registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl WatchSubscription {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Await the next coalesced event for only this subscription.
    pub fn next(&self) -> WatchNext<'_> {
        WatchNext { subscription: self }
    }
}

impl Drop for WatchSubscription {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.events.close();
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut registrations = inner
            .registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in &self.keys {
            let remove = registrations.get_mut(key).is_some_and(|registration| {
                registration.subscribers.remove(&self.id);
                registration.subscribers.is_empty()
            });
            if remove {
                registrations.remove(key);
            }
        }
    }
}

fn coalescing_worker(
    id: u64,
    watched_paths: Vec<PathBuf>,
    path_aliases: Vec<(PathBuf, PathBuf)>,
    receiver: mpsc::Receiver<Result<Event, String>>,
    events: crate::ServiceEvents,
    subscription_events: Arc<WatchEventQueue>,
    cancelled: Arc<AtomicBool>,
) {
    while let Ok(first) = receiver.recv() {
        if cancelled.load(Ordering::Acquire) {
            return;
        }
        let mut changed = HashSet::new();
        if let Err(error) = collect_event(first, &mut changed, &watched_paths) {
            let event = WatcherEvent {
                registration_id: id,
                state: WatcherState::Failed,
                paths: watched_paths,
                error: Some(error),
            };
            subscription_events.publish(event.clone());
            events.publish(ServiceEvent::Watcher(event));
            return;
        }
        bound_changed_paths(&mut changed, &watched_paths);

        let deadline = Instant::now() + COALESCE_DELAY;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let Ok(next) = receiver.recv_timeout(remaining) else {
                break;
            };
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            if let Err(error) = collect_event(next, &mut changed, &watched_paths) {
                let event = WatcherEvent {
                    registration_id: id,
                    state: WatcherState::Failed,
                    paths: Vec::new(),
                    error: Some(error),
                };
                subscription_events.publish(event.clone());
                events.publish(ServiceEvent::Watcher(event));
                return;
            }
            bound_changed_paths(&mut changed, &watched_paths);
        }

        remap_canonical_backend_paths(&mut changed, &path_aliases);
        if !cancelled.load(Ordering::Acquire) && !changed.is_empty() {
            let mut paths: Vec<_> = changed.into_iter().collect();
            paths.sort();
            let event = WatcherEvent {
                registration_id: id,
                state: WatcherState::Changed,
                paths,
                error: None,
            };
            subscription_events.publish(event.clone());
            events.publish(ServiceEvent::Watcher(event));
        }
    }
    subscription_events.close();
}

fn canonical_watch_path_aliases(watched_paths: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let mut aliases: Vec<_> = watched_paths
        .iter()
        .filter_map(|requested| {
            let canonical = std::fs::canonicalize(requested).ok()?;
            (canonical != *requested).then(|| (canonical, requested.clone()))
        })
        .collect();
    aliases.sort_by_key(|(canonical, _)| std::cmp::Reverse(canonical.components().count()));
    aliases
}

fn remap_canonical_backend_paths(changed: &mut HashSet<PathBuf>, aliases: &[(PathBuf, PathBuf)]) {
    if aliases.is_empty() {
        return;
    }
    for path in std::mem::take(changed) {
        let mapped = aliases.iter().find_map(|(canonical, requested)| {
            path.strip_prefix(canonical)
                .ok()
                .map(|suffix| requested.join(suffix))
        });
        changed.insert(mapped.unwrap_or(path));
    }
}

fn bound_changed_paths(changed: &mut HashSet<PathBuf>, watched_paths: &[PathBuf]) {
    if changed.len() <= MAX_CHANGED_PATHS {
        return;
    }
    changed.clear();
    changed.extend(watched_paths.iter().take(MAX_CHANGED_PATHS).cloned());
}

fn collect_event(
    event: Result<Event, String>,
    changed: &mut HashSet<PathBuf>,
    watched_paths: &[PathBuf],
) -> ServiceResult<()> {
    let event = event.map_err(|error| watcher_message("receive filesystem event", error))?;
    if is_access_only(&event.kind) {
        return Ok(());
    }
    if event.need_rescan() {
        // The backend lost track of individual changes (FSEvents
        // MustScanSubDirs, a Windows buffer overflow): report the roots.
        changed.extend(watched_paths.iter().cloned());
    }
    changed.extend(event.paths);
    Ok(())
}

fn is_access_only(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Access(_))
}

fn normalize_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut keyed = paths
        .into_iter()
        .map(|path| (normalize_path(&path), path))
        .collect::<HashMap<_, _>>();
    let mut values: Vec<_> = keyed.drain().map(|(_, path)| path).collect();
    values.sort_by_key(|path| normalize_path(path));
    values
}

fn normalize_path(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.trim_end_matches('/').to_ascii_lowercase()
    } else {
        value.trim_end_matches('/').to_string()
    }
}

fn watcher_error(operation: &str, error: notify::Error) -> ServiceError {
    watcher_message(operation, error.to_string())
}

fn watcher_message(operation: &str, error: String) -> ServiceError {
    ServiceError::new(ErrorCode::Io, format!("Failed to {operation}: {error}"))
        .retryable(true)
        .operation("watch")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeServices, ResourcePaths, ServiceEvent, WatcherState};
    use std::fs;

    #[test]
    fn watcher_storm_paths_collapse_to_the_watched_roots() {
        let watched = vec![PathBuf::from("root")];
        let mut changed = (0..=MAX_CHANGED_PATHS)
            .map(|index| PathBuf::from(format!("root/item-{index}")))
            .collect::<HashSet<_>>();
        bound_changed_paths(&mut changed, &watched);
        assert_eq!(changed, HashSet::from([PathBuf::from("root")]));
    }

    #[test]
    fn merged_bursts_past_the_bound_report_the_roots_instead_of_dropping_paths() {
        let queue = WatchEventQueue {
            roots: vec![PathBuf::from("root")],
            ..WatchEventQueue::default()
        };
        for batch in 0..2 {
            queue.publish(WatcherEvent {
                registration_id: 7,
                state: WatcherState::Changed,
                paths: (0..MAX_CHANGED_PATHS)
                    .map(|index| PathBuf::from(format!("root/{batch}-{index}")))
                    .collect(),
                error: None,
            });
        }
        let events = queue.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events.front().unwrap().paths, [PathBuf::from("root")]);
    }

    #[test]
    fn rescan_notices_report_the_watched_roots() {
        let watched = vec![PathBuf::from("root")];
        let mut changed = HashSet::new();
        let rescan = Event::new(EventKind::Other)
            .set_flag(notify::event::Flag::Rescan)
            .add_path(PathBuf::from("root/child"));
        collect_event(Ok(rescan), &mut changed, &watched).unwrap();
        assert!(changed.contains(&PathBuf::from("root")));

        let mut changed = HashSet::new();
        let modified = Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path(PathBuf::from("root/child"));
        collect_event(Ok(modified), &mut changed, &watched).unwrap();
        assert_eq!(changed, HashSet::from([PathBuf::from("root/child")]));
    }

    #[test]
    fn canonical_backend_paths_are_remapped_to_the_requested_watch_root() {
        let mut changed = HashSet::from([
            PathBuf::from("/private/var/folders/test/nested/changed.txt"),
            PathBuf::from("/unrelated/file.txt"),
        ]);
        let aliases = vec![(
            PathBuf::from("/private/var/folders/test"),
            PathBuf::from("/var/folders/test"),
        )];

        remap_canonical_backend_paths(&mut changed, &aliases);

        assert!(changed.contains(&PathBuf::from("/var/folders/test/nested/changed.txt")));
        assert!(changed.contains(&PathBuf::from("/unrelated/file.txt")));
        assert!(!changed.contains(&PathBuf::from(
            "/private/var/folders/test/nested/changed.txt"
        )));
    }

    #[test]
    fn slow_consumers_receive_one_merged_change_event() {
        let queue = WatchEventQueue::default();
        for path in ["root/first", "root/second"] {
            queue.publish(WatcherEvent {
                registration_id: 7,
                state: WatcherState::Changed,
                paths: vec![PathBuf::from(path)],
                error: None,
            });
        }
        let events = queue.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.front().unwrap().paths,
            [PathBuf::from("root/first"), PathBuf::from("root/second")]
        );
    }

    #[test]
    fn registrations_are_deduplicated_and_access_events_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));
        let receiver = services.subscribe();
        let first = services
            .watcher
            .watch(vec![temp.path().to_path_buf(), temp.path().to_path_buf()])
            .unwrap();
        let second = services
            .watcher
            .watch(vec![temp.path().to_path_buf()])
            .unwrap();
        assert_ne!(first.id(), second.id());
        assert_eq!(services.watcher.registration_count(), 1);
        drop(second);
        assert_eq!(services.watcher.registration_count(), 1);

        fs::write(temp.path().join("changed.txt"), "changed").unwrap();
        let event = receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("native change event");
        match event {
            ServiceEvent::Watcher(event) => {
                assert_eq!(event.state, WatcherState::Changed);
                assert!(event.paths.iter().any(|path| path.ends_with("changed.txt")));
            }
            other => panic!("unexpected service event: {other:?}"),
        }
        drop(first);
        assert_eq!(services.watcher.registration_count(), 0);
    }

    #[test]
    fn recursive_watch_reports_changes_below_the_search_root() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));
        let receiver = services.subscribe();
        let _subscription = services
            .watcher
            .watch_recursive(vec![temp.path().to_path_buf()])
            .unwrap();

        let changed = nested.join("changed.txt");
        fs::write(&changed, "changed").unwrap();
        let event = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        let ServiceEvent::Watcher(event) = event else {
            panic!("expected watcher event");
        };
        assert_eq!(event.state, WatcherState::Changed);
        assert!(
            event
                .paths
                .iter()
                .any(|path| path == &changed || changed.starts_with(path)),
            "recursive backends may report either the changed file or its changed ancestor: {:?}",
            event.paths
        );
    }

    #[test]
    fn awaitable_subscription_delivers_coalesced_changes() {
        let temp = tempfile::tempdir().unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));
        let subscription = services
            .watcher
            .watch_task(vec![temp.path().to_path_buf()])
            .wait()
            .unwrap();
        let registration_id = subscription.id();
        let (sender, receiver) = mpsc::sync_channel(1);
        let waiter = thread::spawn(move || {
            sender
                .send(pollster::block_on(subscription.next()))
                .unwrap();
        });

        fs::write(temp.path().join("streamed.txt"), "changed").unwrap();
        let event = receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("awaitable native change event")
            .expect("watch subscription remained open");
        assert_eq!(event.registration_id, registration_id);
        assert_eq!(event.state, WatcherState::Changed);
        assert!(
            event
                .paths
                .iter()
                .any(|path| path.ends_with("streamed.txt"))
        );
        waiter.join().unwrap();
        assert_eq!(services.watcher.registration_count(), 0);
    }

    #[test]
    fn access_only_events_do_not_mark_a_path_changed() {
        assert!(is_access_only(&EventKind::Access(
            notify::event::AccessKind::Read
        )));
        assert!(!is_access_only(&EventKind::Modify(
            notify::event::ModifyKind::Any
        )));
    }

    #[test]
    fn host_owned_watchers_can_be_registered_and_stopped() {
        let temp = tempfile::tempdir().unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));
        let receiver = services.subscribe();
        let id = services
            .watcher
            .watch_paths(vec![temp.path().to_path_buf()])
            .unwrap();
        assert_eq!(services.watcher.registration_count(), 1);
        assert!(services.watcher.unwatch(id));
        assert_eq!(services.watcher.registration_count(), 0);
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            ServiceEvent::Watcher(event) => {
                assert_eq!(event.registration_id, id);
                assert_eq!(event.state, WatcherState::Stopped);
            }
            other => panic!("unexpected service event: {other:?}"),
        }
        assert!(!services.watcher.unwatch(id));
    }
}
