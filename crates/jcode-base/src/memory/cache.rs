use crate::memory_graph::MemoryGraph;
use crate::memory_types::{MemoryEntry, MemoryScope};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// === Graph Cache ===

struct GraphCacheEntry {
    graph: MemoryGraph,
    modified: Option<SystemTime>,
}

struct GraphCache {
    entries: HashMap<PathBuf, GraphCacheEntry>,
}

impl GraphCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

static GRAPH_CACHE: OnceLock<Mutex<GraphCache>> = OnceLock::new();

fn graph_cache() -> &'static Mutex<GraphCache> {
    GRAPH_CACHE.get_or_init(|| Mutex::new(GraphCache::new()))
}

fn graph_mtime(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

fn mtime_signature(path: Option<&PathBuf>) -> u128 {
    path.and_then(graph_mtime)
        .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

pub(super) fn graph_mtime_signature(path: Option<&PathBuf>) -> u128 {
    mtime_signature(path)
}

pub(super) fn cached_graph(path: &PathBuf) -> Option<MemoryGraph> {
    let modified = graph_mtime(path);
    let cache = graph_cache().lock().ok()?;
    let entry = cache.entries.get(path)?;
    if entry.modified == modified {
        Some(entry.graph.clone())
    } else {
        None
    }
}

pub(super) fn cache_graph(path: PathBuf, graph: &MemoryGraph) {
    let modified = graph_mtime(&path);
    if let Ok(mut cache) = graph_cache().lock() {
        cache.entries.insert(
            path,
            GraphCacheEntry {
                graph: graph.clone(),
                modified,
            },
        );
    }
}

// === Retrieval Result Cache ===

const RETRIEVAL_CACHE_TTL: Duration = Duration::from_secs(30);
const RETRIEVAL_CACHE_CAPACITY: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct RetrievalCacheKey {
    mode: &'static str,
    scope: MemoryScope,
    query_hash: u64,
    threshold_bits: u32,
    limit: usize,
    project_mtime: u128,
    global_mtime: u128,
}

impl RetrievalCacheKey {
    pub(super) fn new(
        mode: &'static str,
        scope: MemoryScope,
        query: &str,
        threshold: f32,
        limit: usize,
        project_mtime: u128,
        global_mtime: u128,
    ) -> Self {
        Self {
            mode,
            scope,
            query_hash: stable_hash(query),
            threshold_bits: threshold.to_bits(),
            limit,
            project_mtime,
            global_mtime,
        }
    }
}

#[derive(Debug, Clone)]
struct RetrievalCacheEntry {
    inserted_at: Instant,
    results: Vec<(MemoryEntry, f32)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetrievalCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
    pub stale: u64,
    pub entries: usize,
}

#[derive(Default)]
struct RetrievalCache {
    entries: HashMap<RetrievalCacheKey, RetrievalCacheEntry>,
    order: VecDeque<RetrievalCacheKey>,
    stats: RetrievalCacheStats,
}

static RETRIEVAL_CACHE: OnceLock<Mutex<RetrievalCache>> = OnceLock::new();

fn retrieval_cache() -> &'static Mutex<RetrievalCache> {
    RETRIEVAL_CACHE.get_or_init(|| Mutex::new(RetrievalCache::default()))
}

pub(super) fn cached_retrieval_results(key: &RetrievalCacheKey) -> Option<Vec<(MemoryEntry, f32)>> {
    let mut cache = retrieval_cache().lock().ok()?;
    let Some(entry) = cache.entries.get(key) else {
        cache.stats.misses += 1;
        cache.stats.entries = cache.entries.len();
        return None;
    };
    if entry.inserted_at.elapsed() > RETRIEVAL_CACHE_TTL {
        cache.entries.remove(key);
        cache.order.retain(|item| item != key);
        cache.stats.stale += 1;
        cache.stats.misses += 1;
        cache.stats.entries = cache.entries.len();
        return None;
    }
    let results = entry.results.clone();
    cache.stats.hits += 1;
    Some(results)
}

pub(super) fn cache_retrieval_results(key: RetrievalCacheKey, results: &[(MemoryEntry, f32)]) {
    if results.is_empty() {
        return;
    }
    let Ok(mut cache) = retrieval_cache().lock() else {
        return;
    };

    if !cache.entries.contains_key(&key) {
        cache.order.push_back(key.clone());
    }
    cache.entries.insert(
        key.clone(),
        RetrievalCacheEntry {
            inserted_at: Instant::now(),
            results: results.to_vec(),
        },
    );
    cache.stats.inserts += 1;

    while cache.entries.len() > RETRIEVAL_CACHE_CAPACITY {
        let Some(oldest) = cache.order.pop_front() else {
            break;
        };
        if cache.entries.remove(&oldest).is_some() {
            cache.stats.evictions += 1;
        }
    }
    cache.stats.entries = cache.entries.len();
}

pub fn retrieval_cache_stats() -> RetrievalCacheStats {
    let Ok(cache) = retrieval_cache().lock() else {
        return RetrievalCacheStats::default();
    };
    let mut stats = cache.stats;
    stats.entries = cache.entries.len();
    stats
}

pub(super) fn clear_retrieval_cache() {
    if let Ok(mut cache) = retrieval_cache().lock() {
        *cache = RetrievalCache::default();
    }
}

#[cfg(test)]
pub(super) fn clear_retrieval_cache_for_tests() {
    clear_retrieval_cache();
}

fn stable_hash(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
