use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::{
    BlockingTask, ErrorCode, ServiceContext, ServiceError, ServiceEvent, ServiceEvents,
    ServiceResult,
};

const MAX_CONTENT_BYTES: u64 = 5 * 1024 * 1024;
/// Soft cap: a root with more entries (or Spotlight hits) ends the search
/// with partial, `truncated` results instead of failing it.
const MAX_INDEXED_ENTRIES_PER_ROOT: usize = 500_000;
const MAX_CACHED_INDEX_ENTRIES: usize = 1_000_000;
const MAX_SEARCH_RESULTS: usize = 50_000;
const SEARCH_PROGRESS_INTERVAL: usize = 512;
const SEARCH_RESULT_BATCH_SIZE: usize = 128;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchType {
    #[default]
    All,
    Files,
    Folders,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CombineMode {
    #[default]
    And,
    Or,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchCriteria {
    pub name_pattern: Option<String>,
    pub name_regex: bool,
    pub extensions: Vec<String>,
    pub type_filter: SearchType,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    pub modified_after: Option<u64>,
    pub modified_before: Option<u64>,
    pub content_search: Option<String>,
    pub search_paths: Vec<PathBuf>,
    pub recursive: bool,
    pub combine_mode: CombineMode,
    pub exclude_pattern: Option<String>,
    /// A Finder tag name items must carry (case-insensitive). Absent from
    /// criteria saved before tags were searchable, and omitted when unset so
    /// older versions can still read saved smart folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

impl Default for SearchCriteria {
    fn default() -> Self {
        Self {
            name_pattern: None,
            name_regex: false,
            extensions: Vec::new(),
            type_filter: SearchType::All,
            size_min: None,
            size_max: None,
            modified_after: None,
            modified_before: None,
            content_search: None,
            search_paths: Vec::new(),
            recursive: true,
            combine_mode: CombineMode::And,
            exclude_pattern: None,
            tag: None,
        }
    }
}

/// Which engine answered a search.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchSource {
    /// The crawler and its cached index answered every root.
    #[default]
    Crawler,
    /// Spotlight answered every root.
    Spotlight,
    /// Spotlight answered some roots and the crawler the others.
    Mixed,
}

#[derive(Clone, Debug)]
pub struct SearchResult {
    pub entries: Vec<explorie_core::FileEntry>,
    pub indexed_entries: usize,
    pub content_reads: usize,
    pub reused_index: bool,
    pub truncated: bool,
    pub source: SearchSource,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SearchIndexHealth {
    pub roots: usize,
    pub indexed_entries: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchProgressEvent {
    pub request_id: String,
    pub phase: String,
    pub indexed_entries: usize,
    pub matched_entries: usize,
    pub current_path: PathBuf,
    #[serde(default)]
    pub entries: Vec<explorie_core::FileEntry>,
}

struct SearchProgress<'a> {
    events: &'a ServiceEvents,
    request_id: &'a str,
}

impl SearchProgress<'_> {
    fn publish(
        &self,
        phase: &str,
        indexed_entries: usize,
        matched_entries: usize,
        current_path: &Path,
        entries: Vec<explorie_core::FileEntry>,
    ) {
        self.events
            .publish(ServiceEvent::SearchProgress(SearchProgressEvent {
                request_id: self.request_id.to_string(),
                phase: phase.to_string(),
                indexed_entries,
                matched_entries,
                current_path: current_path.to_path_buf(),
                entries,
            }));
    }
}

/// Caps that end a search early with partial results instead of failing it.
#[derive(Clone, Copy, Debug)]
struct SearchLimits {
    /// Paths one root may contribute: crawled entries or Spotlight hits.
    entries_per_root: usize,
    /// Matches returned across all roots.
    results: usize,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            entries_per_root: MAX_INDEXED_ENTRIES_PER_ROOT,
            results: MAX_SEARCH_RESULTS,
        }
    }
}

/// How searches run: their caps and the system index to ask first.
#[derive(Clone, Default)]
struct SearchEngine {
    limits: SearchLimits,
    spotlight: Option<Arc<dyn SpotlightBackend>>,
}

impl SearchEngine {
    fn system() -> Self {
        #[cfg(target_os = "macos")]
        let spotlight: Option<Arc<dyn SpotlightBackend>> =
            Some(Arc::new(spotlight::Mdfind::default()));
        #[cfg(not(target_os = "macos"))]
        let spotlight = None;
        Self {
            limits: SearchLimits::default(),
            spotlight,
        }
    }
}

/// A system search index (Spotlight on macOS) that answers name and content
/// queries without crawling or reading files.
trait SpotlightBackend: Send + Sync {
    /// Whether the index covers the items below `scope`. Unindexed volumes,
    /// folders excluded from Spotlight and hidden folders report false.
    fn covers(&self, scope: &Path) -> bool;

    /// Stream the paths below `scope` (spelled with `scope` as given) that
    /// match the Spotlight `query` to `visit` until it breaks or the query
    /// finishes. `cancelled` is polled while waiting for results. An error
    /// means the query could not run to completion.
    fn query(
        &self,
        scope: &Path,
        query: &str,
        cancelled: &dyn Fn() -> bool,
        visit: &mut dyn FnMut(PathBuf) -> ControlFlow<()>,
    ) -> io::Result<()>;
}

#[derive(Clone)]
pub struct SearchService {
    context: ServiceContext,
    index: Arc<Mutex<SearchIndex>>,
    generation: Arc<AtomicU64>,
    engine: SearchEngine,
}

impl SearchService {
    pub(crate) fn new(context: ServiceContext) -> Self {
        Self {
            context,
            index: Arc::new(Mutex::new(SearchIndex::default())),
            generation: Arc::new(AtomicU64::new(0)),
            engine: SearchEngine::system(),
        }
    }

    pub fn fork_cancellation_scope(&self) -> Self {
        Self {
            context: self.context.clone(),
            index: Arc::clone(&self.index),
            generation: Arc::new(AtomicU64::new(0)),
            engine: self.engine.clone(),
        }
    }

    pub fn search(&self, criteria: SearchCriteria) -> BlockingTask<SearchResult> {
        let index = Arc::clone(&self.index);
        let generation = Arc::clone(&self.generation);
        let engine = self.engine.clone();
        let ticket = generation.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        self.context
            .spawn_blocking(move || search(&index, criteria, &generation, ticket, None, &engine))
    }

    pub fn search_with_progress(
        &self,
        criteria: SearchCriteria,
        request_id: String,
    ) -> BlockingTask<SearchResult> {
        let index = Arc::clone(&self.index);
        let generation = Arc::clone(&self.generation);
        let engine = self.engine.clone();
        let events = self.context.events();
        let ticket = generation.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        self.context.spawn_blocking(move || {
            let progress = SearchProgress {
                events: &events,
                request_id: &request_id,
            };
            search(
                &index,
                criteria,
                &generation,
                ticket,
                Some(&progress),
                &engine,
            )
        })
    }

    pub fn search_blocking(&self, criteria: SearchCriteria) -> ServiceResult<SearchResult> {
        let ticket = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        search(
            &self.index,
            criteria,
            &self.generation,
            ticket,
            None,
            &self.engine,
        )
    }

    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn invalidate(&self, paths: &[PathBuf]) {
        self.cancel();
        let roots = self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .roots
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for cached in roots {
            let mut root = cached
                .index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if paths.iter().any(|path| {
                comparable(path).starts_with(&root.comparable_root)
                    || root.comparable_root.starts_with(&comparable(path))
            }) {
                root.invalidated = true;
            }
        }
    }

    pub fn clear(&self) {
        self.cancel();
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        index.roots.clear();
        index.cached_entries = 0;
    }

    pub fn index_health(&self) -> SearchIndexHealth {
        let index = self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        SearchIndexHealth {
            roots: index.roots.len(),
            indexed_entries: index.cached_entries,
        }
    }
}

#[derive(Default)]
struct SearchIndex {
    roots: HashMap<String, CachedRootIndex>,
    cached_entries: usize,
}

#[derive(Clone)]
struct CachedRootIndex {
    index: Arc<Mutex<RootIndex>>,
    entry_count: usize,
}

struct RootIndex {
    comparable_root: String,
    entries: Vec<IndexedEntry>,
    content_query: Option<String>,
    invalidated: bool,
    /// The crawl stopped at the per-root cap, so the index is partial.
    truncated: bool,
}

struct IndexedEntry {
    entry: explorie_core::FileEntry,
    content_matches: Option<bool>,
}

enum NameMatcher {
    Contains(String),
    Regex(regex::Regex),
}

impl NameMatcher {
    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Contains(needle) => name.to_lowercase().contains(needle),
            Self::Regex(regex) => regex.is_match(name),
        }
    }
}

/// The criteria a candidate entry is checked against.
#[derive(Clone, Copy)]
struct Filters<'a> {
    criteria: &'a SearchCriteria,
    name: Option<&'a NameMatcher>,
    exclude: Option<&'a NameMatcher>,
    extensions: &'a HashSet<String>,
    /// The lowercased Finder tag name to require.
    tag: Option<&'a str>,
}

/// Matches collected across roots, de-duplicated by path and capped.
struct Matches {
    seen: HashSet<String>,
    entries: Vec<explorie_core::FileEntry>,
    batch: Vec<explorie_core::FileEntry>,
    limit: usize,
    truncated: bool,
}

impl Matches {
    fn new(limit: usize) -> Self {
        Self {
            seen: HashSet::new(),
            entries: Vec::new(),
            batch: Vec::with_capacity(SEARCH_RESULT_BATCH_SIZE),
            limit,
            truncated: false,
        }
    }

    /// Record a matching entry; false once the result cap has been reached.
    fn add(
        &mut self,
        entry: &explorie_core::FileEntry,
        indexed_entries: usize,
        progress: Option<&SearchProgress<'_>>,
    ) -> bool {
        if !self.seen.insert(comparable(&entry.path)) {
            return true;
        }
        if self.entries.len() >= self.limit {
            self.truncated = true;
            return false;
        }
        self.entries.push(entry.clone());
        self.batch.push(entry.clone());
        if self.batch.len() == SEARCH_RESULT_BATCH_SIZE {
            self.flush(indexed_entries, &entry.path, progress);
        }
        true
    }

    fn flush(
        &mut self,
        indexed_entries: usize,
        current_path: &Path,
        progress: Option<&SearchProgress<'_>>,
    ) {
        if self.batch.is_empty() {
            return;
        }
        if let Some(progress) = progress {
            progress.publish(
                "results",
                indexed_entries,
                self.entries.len(),
                current_path,
                std::mem::take(&mut self.batch),
            );
        } else {
            self.batch.clear();
        }
    }
}

fn search(
    index: &Mutex<SearchIndex>,
    criteria: SearchCriteria,
    generation: &AtomicU64,
    ticket: u64,
    progress: Option<&SearchProgress<'_>>,
    engine: &SearchEngine,
) -> ServiceResult<SearchResult> {
    let paths: Vec<_> = criteria
        .search_paths
        .iter()
        .filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .collect();
    if paths.is_empty() {
        return Ok(SearchResult {
            entries: Vec::new(),
            indexed_entries: 0,
            content_reads: 0,
            reused_index: true,
            truncated: false,
            source: SearchSource::Crawler,
        });
    }

    let name_matcher = build_matcher(criteria.name_pattern.as_deref(), criteria.name_regex)?;
    let exclude_matcher = build_matcher(criteria.exclude_pattern.as_deref(), criteria.name_regex)?;
    let extensions = normalized_extensions(&criteria.extensions);
    let content_query = criteria
        .content_search
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_lowercase);
    let tag = normalized_tag(criteria.tag.as_deref());
    let filters = Filters {
        criteria: &criteria,
        name: name_matcher.as_ref(),
        exclude: exclude_matcher.as_ref(),
        extensions: &extensions,
        tag: tag.as_deref(),
    };
    let spotlight = engine.spotlight.as_deref().and_then(|backend| {
        spotlight_query(&criteria, content_query.as_deref()).map(|query| (backend, query))
    });
    let mut matches = Matches::new(engine.limits.results);
    let mut indexed_entries = 0;
    let mut content_reads = 0;
    let mut reused_index = true;
    let mut index_truncated = false;
    let mut spotlight_roots = 0;
    let mut crawled_roots = 0;

    for root in paths {
        ensure_search_current(generation, ticket)?;
        if let Some((backend, query)) = &spotlight
            && backend.covers(&root)
            && let Some(hits) = search_root_with_spotlight(
                *backend,
                &root,
                query,
                &filters,
                &mut matches,
                engine.limits.entries_per_root,
                generation,
                ticket,
                progress,
            )?
        {
            indexed_entries += hits.examined;
            index_truncated |= hits.capped;
            spotlight_roots += 1;
            if matches.truncated {
                break;
            }
            continue;
        }

        crawled_roots += 1;
        let key = format!("{}|{}", comparable(&root), criteria.recursive);
        let cached = index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .roots
            .get(&key)
            .cloned();
        let rebuild = cached.as_ref().is_none_or(|cached| {
            cached
                .index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .invalidated
        });
        let cached = if rebuild {
            let root_index = build_index(
                &root,
                criteria.recursive,
                engine.limits.entries_per_root,
                generation,
                ticket,
                progress,
            )?;
            let entry_count = root_index.entries.len();
            let cached = CachedRootIndex {
                index: Arc::new(Mutex::new(root_index)),
                entry_count,
            };
            let mut index = index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if index.cached_entries.saturating_add(entry_count) > MAX_CACHED_INDEX_ENTRIES {
                index.roots.clear();
                index.cached_entries = 0;
            }
            if let Some(replaced) = index.roots.insert(key.clone(), cached.clone()) {
                index.cached_entries = index.cached_entries.saturating_sub(replaced.entry_count);
            }
            index.cached_entries = index.cached_entries.saturating_add(entry_count);
            reused_index = false;
            cached
        } else {
            cached.expect("a reusable search index must exist")
        };
        let mut root_index = cached
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        index_truncated |= root_index.truncated;
        if content_query.is_some()
            && root_index.content_query.as_deref() != content_query.as_deref()
        {
            for (index, indexed) in root_index.entries.iter_mut().enumerate() {
                ensure_search_current(generation, ticket)?;
                indexed.content_matches = None;
                // Reading a cloud placeholder would download it first.
                if indexed.entry.is_dir
                    || indexed.entry.size > MAX_CONTENT_BYTES
                    || indexed.entry.is_cloud_placeholder
                {
                    continue;
                }
                indexed.content_matches = content_file_matches(
                    &indexed.entry.path,
                    content_query.as_deref().unwrap_or_default(),
                );
                content_reads += 1;
                if index % SEARCH_PROGRESS_INTERVAL == 0
                    && let Some(progress) = progress
                {
                    progress.publish(
                        "content",
                        indexed_entries + index,
                        matches.entries.len(),
                        &indexed.entry.path,
                        Vec::new(),
                    );
                }
            }
            root_index.content_query = content_query.clone();
            reused_index = false;
        }
        indexed_entries += root_index.entries.len();

        for (index, indexed) in root_index.entries.iter().enumerate() {
            ensure_search_current(generation, ticket)?;
            if matches_criteria(indexed, &filters, content_query.as_deref())
                && !matches.add(&indexed.entry, indexed_entries, progress)
            {
                break;
            }
            if index % SEARCH_PROGRESS_INTERVAL == 0
                && let Some(progress) = progress
            {
                progress.publish(
                    "matching",
                    indexed_entries,
                    matches.entries.len(),
                    &indexed.entry.path,
                    Vec::new(),
                );
            }
        }
        matches.flush(indexed_entries, &root, progress);
        if matches.truncated {
            break;
        }
    }

    Ok(SearchResult {
        truncated: matches.truncated || index_truncated,
        entries: matches.entries,
        indexed_entries,
        content_reads,
        reused_index,
        source: match (spotlight_roots, crawled_roots) {
            (0, _) => SearchSource::Crawler,
            (_, 0) => SearchSource::Spotlight,
            _ => SearchSource::Mixed,
        },
    })
}

/// The trimmed, lowercased tag criterion, if any.
fn normalized_tag(tag: Option<&str>) -> Option<String> {
    tag.map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_lowercase)
}

/// Escape a value for a double-quoted Spotlight query string, where `*` and
/// `?` are wildcards.
fn escape_spotlight_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '"' | '*' | '?') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// The Spotlight query for criteria that Spotlight can answer: recursive
/// plain-text name and/or content searches whose other criteria can be
/// checked on Spotlight's hits. Everything else uses the crawler.
fn spotlight_query(criteria: &SearchCriteria, content_query: Option<&str>) -> Option<String> {
    let name = criteria
        .name_pattern
        .as_deref()
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty());
    let tag = criteria
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty());
    if !criteria.recursive || (name.is_some() && criteria.name_regex) {
        return None;
    }
    let checks = [
        name.is_some(),
        tag.is_some(),
        criteria.type_filter != SearchType::All,
        !criteria.extensions.is_empty(),
        criteria.size_min.is_some() || criteria.size_max.is_some(),
        criteria.modified_after.is_some() || criteria.modified_before.is_some(),
        content_query.is_some(),
    ];
    // Spotlight narrows the candidates, so an OR over other criteria could
    // miss items that match only those.
    if criteria.combine_mode == CombineMode::Or && checks.iter().filter(|check| **check).count() > 1
    {
        return None;
    }
    let mut clauses = Vec::new();
    if let Some(name) = name {
        clauses.push(format!(
            "kMDItemFSName == \"*{}*\"cd",
            escape_spotlight_value(name)
        ));
    }
    if let Some(content) = content_query {
        clauses.push(format!(
            "kMDItemTextContent == \"*{}*\"cd",
            escape_spotlight_value(content)
        ));
    }
    if let Some(tag) = tag {
        clauses.push(format!(
            "kMDItemUserTags == \"{}\"cd",
            escape_spotlight_value(tag)
        ));
    }
    (!clauses.is_empty()).then(|| clauses.join(" && "))
}

struct SpotlightHits {
    examined: usize,
    capped: bool,
}

/// Answer one root from Spotlight. Returns `None` when Spotlight failed
/// before producing anything, so the crawler should run instead.
#[allow(clippy::too_many_arguments)]
fn search_root_with_spotlight(
    backend: &dyn SpotlightBackend,
    root: &Path,
    query: &str,
    filters: &Filters<'_>,
    matches: &mut Matches,
    limit: usize,
    generation: &AtomicU64,
    ticket: u64,
    progress: Option<&SearchProgress<'_>>,
) -> ServiceResult<Option<SpotlightHits>> {
    let superseded = || generation.load(Ordering::Acquire) != ticket;
    let mut examined = 0;
    let mut capped = false;
    let outcome = backend.query(root, query, &superseded, &mut |path| {
        if superseded() {
            return ControlFlow::Break(());
        }
        if path == root {
            return ControlFlow::Continue(());
        }
        if examined >= limit {
            capped = true;
            return ControlFlow::Break(());
        }
        examined += 1;
        // Items can vanish after Spotlight indexed them; skip those.
        if let Ok(entry) = explorie_core::entry_for_path(&path) {
            let indexed = IndexedEntry {
                entry,
                content_matches: None,
            };
            // Spotlight already matched any content query, so only the
            // remaining criteria are checked here, without reading files.
            if matches_criteria(&indexed, filters, None)
                && !matches.add(&indexed.entry, examined, progress)
            {
                return ControlFlow::Break(());
            }
        }
        if examined % SEARCH_PROGRESS_INTERVAL == 0
            && let Some(progress) = progress
        {
            progress.publish(
                "spotlight",
                examined,
                matches.entries.len(),
                &path,
                Vec::new(),
            );
        }
        ControlFlow::Continue(())
    });
    ensure_search_current(generation, ticket)?;
    if outcome.is_err() && examined == 0 {
        return Ok(None);
    }
    matches.flush(examined, root, progress);
    Ok(Some(SpotlightHits { examined, capped }))
}

fn content_file_matches(path: &Path, query: &str) -> Option<bool> {
    let mut bytes = Vec::with_capacity(128 * 1024);
    fs::File::open(path)
        .ok()?
        .take(MAX_CONTENT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CONTENT_BYTES {
        return None;
    }
    String::from_utf8(bytes)
        .ok()
        .map(|content| content.to_lowercase().contains(query))
}

fn build_index(
    root: &Path,
    recursive: bool,
    limit: usize,
    generation: &AtomicU64,
    ticket: u64,
    progress: Option<&SearchProgress<'_>>,
) -> ServiceResult<RootIndex> {
    let comparable_root = comparable(root);
    let mut pending = VecDeque::from([root.to_path_buf()]);
    let mut visited = HashSet::new();
    let mut entries = Vec::new();
    let mut truncated = false;

    'crawl: while let Some(path) = pending.pop_front() {
        ensure_search_current(generation, ticket)?;
        if !visited.insert(comparable(&path)) {
            continue;
        }
        let Ok(listed) = explorie_core::list_dir_with_sizes(&path, false) else {
            continue;
        };
        for entry in listed {
            ensure_search_current(generation, ticket)?;
            if entries.len() >= limit {
                truncated = true;
                break 'crawl;
            }
            let entry_path = entry.path.clone();
            if recursive && entry.is_dir && !entry.is_symlink && !entry.is_junction {
                pending.push_back(entry_path.clone());
            }
            entries.push(IndexedEntry {
                entry,
                content_matches: None,
            });
            if entries.len() % SEARCH_PROGRESS_INTERVAL == 0
                && let Some(progress) = progress
            {
                progress.publish("indexing", entries.len(), 0, &entry_path, Vec::new());
            }
        }
    }

    Ok(RootIndex {
        comparable_root,
        entries,
        content_query: None,
        invalidated: false,
        truncated,
    })
}

fn ensure_search_current(generation: &AtomicU64, ticket: u64) -> ServiceResult<()> {
    if generation.load(Ordering::Acquire) == ticket {
        Ok(())
    } else {
        Err(ServiceError::new(
            ErrorCode::Cancelled,
            "Search superseded by a newer request",
        ))
    }
}

fn build_matcher(pattern: Option<&str>, regex: bool) -> ServiceResult<Option<NameMatcher>> {
    let Some(pattern) = pattern.map(str::trim).filter(|pattern| !pattern.is_empty()) else {
        return Ok(None);
    };
    if regex {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map(NameMatcher::Regex)
            .map(Some)
            .map_err(|error| {
                ServiceError::new(
                    ErrorCode::InvalidInput,
                    format!("Invalid search expression: {error}"),
                )
            })
    } else {
        Ok(Some(NameMatcher::Contains(pattern.to_lowercase())))
    }
}

fn normalized_extensions(extensions: &[String]) -> HashSet<String> {
    extensions
        .iter()
        .map(|extension| extension.trim().to_lowercase())
        .map(|extension| extension.trim_start_matches('.').to_string())
        .filter(|extension| !extension.is_empty())
        .collect()
}

fn matches_criteria(
    indexed: &IndexedEntry,
    filters: &Filters<'_>,
    content_query: Option<&str>,
) -> bool {
    let Filters {
        criteria,
        name: name_matcher,
        exclude: exclude_matcher,
        extensions,
        tag,
    } = *filters;
    let entry = &indexed.entry;
    let name = entry
        .path
        .file_name()
        .unwrap_or(entry.path.as_os_str())
        .to_string_lossy();
    if exclude_matcher.is_some_and(|matcher| matcher.matches(&name)) {
        return false;
    }
    let mut checks = Vec::with_capacity(7);
    if let Some(matcher) = name_matcher {
        checks.push(matcher.matches(&name));
    }
    if let Some(tag) = tag {
        // Tags come from the xattr, read with the listing (or for each
        // Spotlight hit), so checking them never opens the file.
        checks.push(
            entry
                .tags
                .iter()
                .any(|candidate| candidate.name.to_lowercase() == tag),
        );
    }
    if criteria.type_filter != SearchType::All {
        checks.push(match criteria.type_filter {
            SearchType::All => true,
            SearchType::Files => !entry.is_dir,
            SearchType::Folders => entry.is_dir,
        });
    }
    if !extensions.is_empty() {
        checks.push(
            !entry.is_dir
                && entry
                    .path
                    .extension()
                    .map(|extension| extension.to_string_lossy().to_lowercase())
                    .is_some_and(|extension| extensions.contains(&extension)),
        );
    }
    if criteria.size_min.is_some() || criteria.size_max.is_some() {
        checks.push(
            !entry.is_dir
                && criteria
                    .size_min
                    .is_none_or(|minimum| entry.size >= minimum)
                && criteria
                    .size_max
                    .is_none_or(|maximum| entry.size <= maximum),
        );
    }
    if criteria.modified_after.is_some() || criteria.modified_before.is_some() {
        let modified = millis(entry.modified);
        checks.push(
            criteria
                .modified_after
                .is_none_or(|after| modified >= after)
                && criteria
                    .modified_before
                    .is_none_or(|before| modified <= before),
        );
    }
    if content_query.is_some() {
        checks.push(
            !entry.is_dir
                && entry.size <= MAX_CONTENT_BYTES
                && indexed.content_matches == Some(true),
        );
    }
    if checks.is_empty() {
        return true;
    }
    match criteria.combine_mode {
        CombineMode::And => checks.into_iter().all(|matches| matches),
        CombineMode::Or => checks.into_iter().any(|matches| matches),
    }
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn comparable(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        path.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path
    }
}

/// Spotlight through `mdfind`, used when the scope's volume is indexed and
/// the scope itself is visible to Spotlight.
#[cfg(target_os = "macos")]
mod spotlight {
    use super::{SpotlightBackend, escape_spotlight_value};
    use crate::process::run_with_timeout;
    use std::collections::HashMap;
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::io::{self, BufRead, BufReader};
    use std::ops::ControlFlow;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime};

    const HELPER_TIMEOUT: Duration = Duration::from_secs(5);
    const FIRST_RESULT_TIMEOUT: Duration = Duration::from_secs(30);
    const VOLUME_STATUS_TTL: Duration = Duration::from_secs(300);
    const SCOPE_STATUS_TTL: Duration = Duration::from_secs(60);
    /// Items this old have had time to be indexed.
    const PROBE_MIN_AGE: Duration = Duration::from_secs(300);
    const PROBE_CANDIDATES: usize = 3;
    const PROBE_SCAN_LIMIT: usize = 256;

    /// Remembers recent answers so repeated searches skip the helpers.
    #[derive(Default)]
    struct StatusCache(Mutex<HashMap<PathBuf, (bool, Instant)>>);

    impl StatusCache {
        fn get_or_check(&self, path: &Path, ttl: Duration, check: impl FnOnce() -> bool) -> bool {
            let cached = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(path)
                .filter(|(_, checked)| checked.elapsed() < ttl)
                .map(|(status, _)| *status);
            cached.unwrap_or_else(|| {
                let status = check();
                self.0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(path.to_path_buf(), (status, Instant::now()));
                status
            })
        }
    }

    #[derive(Default)]
    pub(super) struct Mdfind {
        volumes: StatusCache,
        scopes: StatusCache,
    }

    impl SpotlightBackend for Mdfind {
        fn covers(&self, scope: &Path) -> bool {
            let Ok(scope) = scope.canonicalize() else {
                return false;
            };
            // Spotlight never indexes hidden folders or their contents.
            let hidden = scope.components().any(|component| {
                matches!(component, Component::Normal(name) if name.as_bytes().starts_with(b"."))
            });
            !hidden
                && mount_point(&scope).is_some_and(|volume| {
                    self.volumes
                        .get_or_check(&volume, VOLUME_STATUS_TTL, || volume_indexed(&volume))
                })
                && self
                    .scopes
                    .get_or_check(&scope, SCOPE_STATUS_TTL, || scope_is_indexed(&scope))
        }

        fn query(
            &self,
            scope: &Path,
            query: &str,
            cancelled: &dyn Fn() -> bool,
            visit: &mut dyn FnMut(PathBuf) -> ControlFlow<()>,
        ) -> io::Result<()> {
            let canonical = scope.canonicalize()?;
            let mut child = Command::new("/usr/bin/mdfind")
                .arg("-0")
                .arg("-onlyin")
                .arg(&canonical)
                .arg(query)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("mdfind has no output pipe"))?;
            let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(1024);
            let reader = thread::spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let mut path = Vec::new();
                    match stdout.read_until(0, &mut path) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if path.last() == Some(&0) {
                                path.pop();
                            }
                            if !path.is_empty() && sender.send(path).is_err() {
                                break;
                            }
                        }
                    }
                }
            });

            let started = Instant::now();
            let mut received = false;
            let mut outcome = Ok(());
            let mut stop = false;
            loop {
                match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(bytes) => {
                        received = true;
                        let path = PathBuf::from(OsString::from_vec(bytes));
                        // Report results under the scope as the caller spelled it.
                        let path = match path.strip_prefix(&canonical) {
                            Ok(relative) if canonical != scope => scope.join(relative),
                            _ => path,
                        };
                        if visit(path).is_break() {
                            stop = true;
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if cancelled() {
                            stop = true;
                            break;
                        }
                        if !received && started.elapsed() > FIRST_RESULT_TIMEOUT {
                            stop = true;
                            outcome = Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "Spotlight did not answer in time",
                            ));
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            if stop {
                let _ = child.kill();
            }
            drop(receiver);
            let status = child.wait()?;
            let _ = reader.join();
            if !stop && !status.success() {
                outcome = Err(io::Error::other(format!("mdfind failed with {status}")));
            }
            outcome
        }
    }

    fn volume_indexed(volume: &Path) -> bool {
        run_with_timeout(
            Command::new("/usr/bin/mdutil").arg("-s").arg(volume),
            HELPER_TIMEOUT,
            64 * 1024,
            0,
        )
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("Indexing enabled")
        })
    }

    fn mount_point(path: &Path) -> Option<PathBuf> {
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: c_path is NUL-terminated and statfs fills the whole struct
        // when it returns 0.
        if unsafe { libc::statfs(c_path.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: statfs succeeded, so the struct is initialized.
        let stats = unsafe { stats.assume_init() };
        // SAFETY: f_mntonname is a NUL-terminated C string inside the struct.
        let mount = unsafe { CStr::from_ptr(stats.f_mntonname.as_ptr()) };
        Some(PathBuf::from(OsStr::from_bytes(mount.to_bytes())))
    }

    /// Folders can be excluded from Spotlight on an indexed volume (privacy
    /// settings, temporary folders, package contents), so ask Spotlight for a
    /// few of the scope's settled children by exact name.
    fn scope_is_indexed(scope: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(scope) else {
            return false;
        };
        let now = SystemTime::now();
        let candidates: Vec<String> = entries
            .flatten()
            .take(PROBE_SCAN_LIMIT)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                if name.starts_with('.') {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                let age = now.duration_since(metadata.modified().ok()?).ok()?;
                (!metadata.file_type().is_symlink() && age >= PROBE_MIN_AGE).then_some(name)
            })
            .take(PROBE_CANDIDATES)
            .collect();
        candidates.iter().any(|name| {
            let expected = scope.join(name);
            run_with_timeout(
                Command::new("/usr/bin/mdfind")
                    .arg("-0")
                    .arg("-onlyin")
                    .arg(scope)
                    .arg(format!(
                        "kMDItemFSName == \"{}\"",
                        escape_spotlight_value(name)
                    )),
                HELPER_TIMEOUT,
                1024 * 1024,
                0,
            )
            .is_ok_and(|output| {
                output
                    .stdout
                    .split(|byte| *byte == 0)
                    .any(|path| Path::new(OsStr::from_bytes(path)) == expected)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourcePaths;
    use uuid::Uuid;

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("explorie-search-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("alpha.txt"), "needle one").unwrap();
        fs::write(root.join("skip.log"), "needle two").unwrap();
        fs::write(root.join("nested").join("beta.txt"), "other").unwrap();
        root
    }

    fn service(root: &Path) -> SearchService {
        SearchService::new(ServiceContext::new(ResourcePaths::test(root)))
    }

    #[test]
    fn recursive_search_combines_name_extension_type_and_exclusion() {
        let root = fixture();
        let result = service(&root)
            .search_blocking(SearchCriteria {
                name_pattern: Some("a".into()),
                extensions: vec![".txt".into()],
                type_filter: SearchType::Files,
                search_paths: vec![root.clone()],
                recursive: true,
                exclude_pattern: Some("beta".into()),
                ..SearchCriteria::default()
            })
            .unwrap();
        assert_eq!(result.entries.len(), 1);
        assert!(result.entries[0].path.ends_with("alpha.txt"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn content_is_indexed_once_and_invalidated_after_a_watcher_change() {
        let root = fixture();
        let service = service(&root);
        let criteria = SearchCriteria {
            content_search: Some("needle".into()),
            search_paths: vec![root.clone()],
            recursive: true,
            ..SearchCriteria::default()
        };
        let first = service.search_blocking(criteria.clone()).unwrap();
        assert_eq!(first.entries.len(), 2);
        assert_eq!(first.content_reads, 3);
        assert!(!first.reused_index);

        let second = service.search_blocking(criteria.clone()).unwrap();
        assert_eq!(second.entries.len(), 2);
        assert_eq!(second.content_reads, 0);
        assert!(second.reused_index);

        let miss = service
            .search_blocking(SearchCriteria {
                content_search: Some("absent".into()),
                search_paths: vec![root.clone()],
                recursive: true,
                ..SearchCriteria::default()
            })
            .unwrap();
        assert!(miss.entries.is_empty());
        assert_eq!(miss.content_reads, 3);
        assert!(!miss.reused_index);

        fs::write(root.join("nested").join("beta.txt"), "needle three").unwrap();
        service.invalidate(&[root.join("nested").join("beta.txt")]);
        let refreshed = service.search_blocking(criteria).unwrap();
        assert_eq!(refreshed.entries.len(), 3);
        assert_eq!(refreshed.content_reads, 3);
        assert!(!refreshed.reused_index);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_regular_expression_is_a_typed_recoverable_error() {
        let root = fixture();
        let error = service(&root)
            .search_blocking(SearchCriteria {
                name_pattern: Some("[".into()),
                name_regex: true,
                search_paths: vec![root.clone()],
                recursive: true,
                ..SearchCriteria::default()
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ten_thousand_entry_index_query_performs_zero_file_reads() {
        let root = PathBuf::from("indexed-root");
        let key = format!("{}|true", comparable(&root));
        let mut entries: Vec<_> = (0..10_000)
            .map(|index| IndexedEntry {
                entry: explorie_core::FileEntry {
                    id: Uuid::new_v4(),
                    path: root.join(format!("file-{index:05}.txt")),
                    size: 16,
                    modified: UNIX_EPOCH,
                    hidden: false,
                    is_dir: false,
                    custom: HashMap::new(),
                    is_symlink: false,
                    is_junction: false,
                    link_target: None,
                    has_xattrs: false,
                    is_package: false,
                    link_target_is_dir: false,
                    is_cloud_placeholder: false,
                    tags: Vec::new(),
                },
                content_matches: Some(index < 3),
            })
            .collect();
        entries.push(IndexedEntry {
            entry: explorie_core::FileEntry {
                id: Uuid::new_v4(),
                path: root.join("oversize.txt"),
                size: MAX_CONTENT_BYTES + 1,
                modified: UNIX_EPOCH,
                hidden: false,
                is_dir: false,
                custom: HashMap::new(),
                is_symlink: false,
                is_junction: false,
                link_target: None,
                has_xattrs: false,
                is_package: false,
                link_target_is_dir: false,
                is_cloud_placeholder: false,
                tags: Vec::new(),
            },
            content_matches: Some(true),
        });
        let index = Mutex::new(SearchIndex {
            roots: HashMap::from([(
                key,
                CachedRootIndex {
                    entry_count: entries.len(),
                    index: Arc::new(Mutex::new(RootIndex {
                        comparable_root: comparable(&root),
                        entries,
                        content_query: Some("needle".to_string()),
                        invalidated: false,
                        truncated: false,
                    })),
                },
            )]),
            cached_entries: 10_001,
        });

        let generation = AtomicU64::new(1);
        let result = search(
            &index,
            SearchCriteria {
                content_search: Some("needle".into()),
                search_paths: vec![root],
                recursive: true,
                ..SearchCriteria::default()
            },
            &generation,
            1,
            None,
            &SearchEngine::default(),
        )
        .unwrap();
        assert_eq!(result.indexed_entries, 10_001);
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.content_reads, 0);
        assert!(result.reused_index);
    }

    fn file_entry(path: PathBuf) -> explorie_core::FileEntry {
        explorie_core::FileEntry {
            id: Uuid::new_v4(),
            path,
            size: 16,
            modified: UNIX_EPOCH,
            hidden: false,
            is_dir: false,
            custom: HashMap::new(),
            is_symlink: false,
            is_junction: false,
            link_target: None,
            has_xattrs: false,
            is_package: false,
            link_target_is_dir: false,
            is_cloud_placeholder: false,
            tags: Vec::new(),
        }
    }

    #[derive(Default)]
    struct FakeSpotlight {
        uncovered: bool,
        fails: bool,
        hits: Vec<PathBuf>,
        queries: Mutex<Vec<String>>,
    }

    impl SpotlightBackend for FakeSpotlight {
        fn covers(&self, _scope: &Path) -> bool {
            !self.uncovered
        }

        fn query(
            &self,
            _scope: &Path,
            query: &str,
            _cancelled: &dyn Fn() -> bool,
            visit: &mut dyn FnMut(PathBuf) -> ControlFlow<()>,
        ) -> io::Result<()> {
            self.queries.lock().unwrap().push(query.to_string());
            if self.fails {
                return Err(io::Error::other("Spotlight is unavailable"));
            }
            for hit in &self.hits {
                if visit(hit.clone()).is_break() {
                    break;
                }
            }
            Ok(())
        }
    }

    fn search_with(
        criteria: SearchCriteria,
        spotlight: Option<Arc<FakeSpotlight>>,
        limits: SearchLimits,
    ) -> SearchResult {
        let engine = SearchEngine {
            limits,
            spotlight: spotlight.map(|backend| backend as Arc<dyn SpotlightBackend>),
        };
        search(
            &Mutex::new(SearchIndex::default()),
            criteria,
            &AtomicU64::new(1),
            1,
            None,
            &engine,
        )
        .unwrap()
    }

    fn names(result: &SearchResult) -> Vec<String> {
        let mut names: Vec<_> = result
            .entries
            .iter()
            .map(|entry| {
                entry
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn spotlight_answers_covered_scopes_and_hits_are_filtered_locally() {
        let root = fixture();
        fs::write(root.join("gamma.txt"), "x").unwrap();
        let spotlight = Arc::new(FakeSpotlight {
            hits: vec![
                root.clone(),
                root.join("alpha.txt"),
                root.join("skip.log"),
                root.join("deleted-since-indexing.txt"),
                root.join("alpha.txt"),
            ],
            ..FakeSpotlight::default()
        });
        let result = search_with(
            SearchCriteria {
                name_pattern: Some(" a ".trim().into()),
                extensions: vec!["txt".into()],
                search_paths: vec![root.clone()],
                ..SearchCriteria::default()
            },
            Some(Arc::clone(&spotlight)),
            SearchLimits::default(),
        );
        // gamma.txt and nested/beta.txt also match, but only the crawler
        // would find them: Spotlight's answer is used as-is.
        assert_eq!(names(&result), ["alpha.txt"]);
        assert_eq!(result.indexed_entries, 4);
        assert_eq!(result.content_reads, 0);
        assert!(result.reused_index);
        assert!(!result.truncated);
        assert_eq!(result.source, SearchSource::Spotlight);
        assert_eq!(
            *spotlight.queries.lock().unwrap(),
            vec![r#"kMDItemFSName == "*a*"cd"#.to_string()]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn spotlight_content_hits_are_trusted_without_reading_files() {
        let root = fixture();
        let spotlight = Arc::new(FakeSpotlight {
            hits: vec![root.join("skip.log")],
            ..FakeSpotlight::default()
        });
        let result = search_with(
            SearchCriteria {
                content_search: Some("Needle".into()),
                search_paths: vec![root.clone()],
                ..SearchCriteria::default()
            },
            Some(Arc::clone(&spotlight)),
            SearchLimits::default(),
        );
        assert_eq!(names(&result), ["skip.log"]);
        assert_eq!(result.content_reads, 0);
        assert_eq!(
            *spotlight.queries.lock().unwrap(),
            vec![r#"kMDItemTextContent == "*needle*"cd"#.to_string()]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn searches_fall_back_to_the_crawler_when_spotlight_cannot_answer() {
        let root = fixture();
        let criteria = SearchCriteria {
            name_pattern: Some("a".into()),
            extensions: vec!["txt".into()],
            search_paths: vec![root.clone()],
            ..SearchCriteria::default()
        };

        let failing = Arc::new(FakeSpotlight {
            fails: true,
            ..FakeSpotlight::default()
        });
        let result = search_with(
            criteria.clone(),
            Some(Arc::clone(&failing)),
            SearchLimits::default(),
        );
        assert_eq!(names(&result), ["alpha.txt", "beta.txt"]);
        assert!(!result.reused_index);
        assert_eq!(result.source, SearchSource::Crawler);
        assert_eq!(failing.queries.lock().unwrap().len(), 1);

        let unindexed = Arc::new(FakeSpotlight {
            uncovered: true,
            ..FakeSpotlight::default()
        });
        let result = search_with(
            criteria,
            Some(Arc::clone(&unindexed)),
            SearchLimits::default(),
        );
        assert_eq!(names(&result), ["alpha.txt", "beta.txt"]);
        assert!(unindexed.queries.lock().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn spotlight_is_only_asked_what_it_can_answer() {
        let base = SearchCriteria {
            name_pattern: Some(r#"q"u*o?te\"#.into()),
            search_paths: vec![PathBuf::from("/scope")],
            ..SearchCriteria::default()
        };
        assert_eq!(
            spotlight_query(&base, Some("body")).as_deref(),
            Some(r#"kMDItemFSName == "*q\"u\*o\?te\\*"cd && kMDItemTextContent == "*body*"cd"#)
        );
        let unanswerable = [
            SearchCriteria {
                name_regex: true,
                ..base.clone()
            },
            SearchCriteria {
                recursive: false,
                ..base.clone()
            },
            SearchCriteria {
                combine_mode: CombineMode::Or,
                extensions: vec!["txt".into()],
                ..base.clone()
            },
            SearchCriteria {
                name_pattern: None,
                extensions: vec!["txt".into()],
                ..base.clone()
            },
        ];
        for criteria in unanswerable {
            assert_eq!(spotlight_query(&criteria, None), None, "{criteria:?}");
        }
        let single_or = SearchCriteria {
            combine_mode: CombineMode::Or,
            ..base
        };
        assert!(spotlight_query(&single_or, None).is_some());
    }

    #[test]
    fn spotlight_answers_tag_searches_by_tag_name() {
        let tagged = SearchCriteria {
            tag: Some(" Work \"Q3\" ".into()),
            search_paths: vec![PathBuf::from("/scope")],
            ..SearchCriteria::default()
        };
        assert_eq!(
            spotlight_query(&tagged, None).as_deref(),
            Some(r#"kMDItemUserTags == "Work \"Q3\""cd"#)
        );
        let named = SearchCriteria {
            name_pattern: Some("report".into()),
            ..tagged.clone()
        };
        assert_eq!(
            spotlight_query(&named, None).as_deref(),
            Some(r#"kMDItemFSName == "*report*"cd && kMDItemUserTags == "Work \"Q3\""cd"#)
        );
        // An OR across a tag and another criterion would miss items that
        // only match the other one.
        let either = SearchCriteria {
            combine_mode: CombineMode::Or,
            ..named
        };
        assert_eq!(spotlight_query(&either, None), None);
    }

    #[test]
    fn tag_criterion_matches_listed_tags_without_reading_files() {
        let root = PathBuf::from("tagged-root");
        let key = format!("{}|true", comparable(&root));
        let tagged = |name: &str, tags: &[(&str, u8)]| {
            let mut entry = file_entry(root.join(name));
            entry.tags = tags
                .iter()
                .map(|(name, color)| explorie_core::FinderTag {
                    name: (*name).to_string(),
                    color: *color,
                })
                .collect();
            IndexedEntry {
                entry,
                content_matches: None,
            }
        };
        let entries = vec![
            tagged("report.txt", &[("Work", 4), ("Urgent", 6)]),
            tagged("notes.txt", &[("Home", 2)]),
            tagged("plain.txt", &[]),
            tagged("workbook.txt", &[("Workshop", 0)]),
        ];
        let index = Mutex::new(SearchIndex {
            roots: HashMap::from([(
                key,
                CachedRootIndex {
                    entry_count: entries.len(),
                    index: Arc::new(Mutex::new(RootIndex {
                        comparable_root: comparable(&root),
                        entries,
                        content_query: None,
                        invalidated: false,
                        truncated: false,
                    })),
                },
            )]),
            cached_entries: 4,
        });
        let run = |criteria: SearchCriteria| {
            search(
                &index,
                criteria,
                &AtomicU64::new(1),
                1,
                None,
                &SearchEngine::default(),
            )
            .unwrap()
        };

        let result = run(SearchCriteria {
            tag: Some("work".into()),
            search_paths: vec![root.clone()],
            ..SearchCriteria::default()
        });
        assert_eq!(names(&result), ["report.txt"]);
        assert_eq!(result.content_reads, 0);
        assert_eq!(result.source, SearchSource::Crawler);

        let either = run(SearchCriteria {
            tag: Some("Home".into()),
            name_pattern: Some("plain".into()),
            combine_mode: CombineMode::Or,
            search_paths: vec![root.clone()],
            ..SearchCriteria::default()
        });
        assert_eq!(names(&either), ["notes.txt", "plain.txt"]);

        let blank = run(SearchCriteria {
            tag: Some("  ".into()),
            search_paths: vec![root],
            ..SearchCriteria::default()
        });
        assert_eq!(blank.entries.len(), 4, "a blank tag is no criterion");
    }

    #[test]
    fn saved_criteria_without_a_tag_still_load_and_omit_it_when_saved() {
        let legacy = serde_json::json!({
            "namePattern": "report",
            "nameRegex": false,
            "extensions": [],
            "typeFilter": "all",
            "sizeMin": null,
            "sizeMax": null,
            "modifiedAfter": null,
            "modifiedBefore": null,
            "contentSearch": null,
            "searchPaths": ["/scope"],
            "recursive": true,
            "combineMode": "AND",
            "excludePattern": null
        });
        let criteria: SearchCriteria = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(criteria.tag, None);
        assert_eq!(serde_json::to_value(&criteria).unwrap(), legacy);

        let tagged = SearchCriteria {
            tag: Some("Work".into()),
            ..criteria
        };
        let saved = serde_json::to_value(&tagged).unwrap();
        assert_eq!(saved["tag"], "Work");
        assert_eq!(
            serde_json::from_value::<SearchCriteria>(saved).unwrap(),
            tagged
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_crawled_tag_search_reads_tags_from_the_xattr() {
        let root = fixture();
        explorie_core::write_finder_tags(
            &root.join("nested").join("beta.txt"),
            &["Review\n4".to_string()],
        )
        .unwrap();
        let result = search_with(
            SearchCriteria {
                tag: Some("review".into()),
                search_paths: vec![root.clone()],
                ..SearchCriteria::default()
            },
            None,
            SearchLimits::default(),
        );
        assert_eq!(names(&result), ["beta.txt"]);
        assert_eq!(result.entries[0].tags[0].color, 4);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_roots_end_the_search_with_partial_results() {
        let root = fixture();
        let crawled = search_with(
            SearchCriteria {
                search_paths: vec![root.clone()],
                ..SearchCriteria::default()
            },
            None,
            SearchLimits {
                entries_per_root: 2,
                results: MAX_SEARCH_RESULTS,
            },
        );
        assert!(crawled.truncated);
        assert_eq!(crawled.indexed_entries, 2);
        assert_eq!(crawled.entries.len(), 2);

        let spotlight = Arc::new(FakeSpotlight {
            hits: vec![
                root.join("alpha.txt"),
                root.join("skip.log"),
                root.join("nested").join("beta.txt"),
            ],
            ..FakeSpotlight::default()
        });
        let answered = search_with(
            SearchCriteria {
                content_search: Some("needle".into()),
                search_paths: vec![root.clone()],
                ..SearchCriteria::default()
            },
            Some(spotlight),
            SearchLimits {
                entries_per_root: 2,
                results: MAX_SEARCH_RESULTS,
            },
        );
        assert!(answered.truncated);
        assert_eq!(names(&answered), ["alpha.txt", "skip.log"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn content_search_never_reads_cloud_placeholders() {
        let root = fixture();
        let key = format!("{}|true", comparable(&root));
        let mut placeholder = file_entry(root.join("skip.log"));
        placeholder.is_cloud_placeholder = true;
        let entries = vec![
            IndexedEntry {
                entry: file_entry(root.join("alpha.txt")),
                content_matches: None,
            },
            IndexedEntry {
                entry: placeholder,
                content_matches: None,
            },
        ];
        let index = Mutex::new(SearchIndex {
            roots: HashMap::from([(
                key,
                CachedRootIndex {
                    entry_count: entries.len(),
                    index: Arc::new(Mutex::new(RootIndex {
                        comparable_root: comparable(&root),
                        entries,
                        content_query: None,
                        invalidated: false,
                        truncated: false,
                    })),
                },
            )]),
            cached_entries: 2,
        });
        let result = search(
            &index,
            SearchCriteria {
                content_search: Some("needle".into()),
                search_paths: vec![root.clone()],
                ..SearchCriteria::default()
            },
            &AtomicU64::new(1),
            1,
            None,
            &SearchEngine::default(),
        )
        .unwrap();
        assert_eq!(names(&result), ["alpha.txt"]);
        assert_eq!(result.content_reads, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_spotlight_finds_system_applications_by_name() {
        let scope = PathBuf::from("/System/Applications");
        let backend = spotlight::Mdfind::default();
        if !backend.covers(&scope) {
            eprintln!("Spotlight does not cover {}; skipping", scope.display());
            return;
        }
        let result = search_with_backend(
            SearchCriteria {
                name_pattern: Some("textedit".into()),
                type_filter: SearchType::Folders,
                search_paths: vec![scope.clone()],
                ..SearchCriteria::default()
            },
            Arc::new(backend),
        );
        assert!(result.reused_index, "Spotlight, not the crawler, answered");
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.path == scope.join("TextEdit.app") && entry.is_package),
            "{:?}",
            result.entries
        );
    }

    #[cfg(target_os = "macos")]
    fn search_with_backend(
        criteria: SearchCriteria,
        backend: Arc<dyn SpotlightBackend>,
    ) -> SearchResult {
        search(
            &Mutex::new(SearchIndex::default()),
            criteria,
            &AtomicU64::new(1),
            1,
            None,
            &SearchEngine {
                limits: SearchLimits::default(),
                spotlight: Some(backend),
            },
        )
        .unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_fresh_temporary_folders_are_not_treated_as_indexed() {
        let root = fixture();
        assert!(!spotlight::Mdfind::default().covers(&root));
        fs::remove_dir_all(root).unwrap();
    }
}
