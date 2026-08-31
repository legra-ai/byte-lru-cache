#![doc = include_str!("../README.md")]

//! A thread-safe LRU cache whose admission is governed by a byte budget.

use std::hash::Hash;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Monotonic counters collected by a [`ByteLruCache`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    /// Successful lookups.
    pub hits: u64,
    /// Lookups that did not find an entry.
    pub misses: u64,
    /// Entries removed by LRU eviction or cap reduction.
    pub evictions: u64,
}

/// A thread-safe LRU cache governed by a caller-supplied byte budget.
///
/// Values are stored behind [`Arc`] so callers can hold a value without
/// copying it. Each entry carries a strictly positive weight. The cache
/// evicts least-recently-used entries until the sum of weights is at most
/// `max_bytes`.
///
/// The weight is an accounting value supplied by the caller. It should be a
/// conservative estimate of the memory retained by the value; the cache
/// cannot measure an arbitrary `V`'s heap allocation itself.
pub struct ByteLruCache<K, V> {
    inner: Mutex<Inner<K, V>>,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    label: String,
}

struct Inner<K, V> {
    entries: lru::LruCache<K, Weighted<V>>,
    current_bytes: u64,
    max_bytes: u64,
}

struct Weighted<V> {
    value: Arc<V>,
    weight_bytes: NonZeroU64,
}

impl<K, V> ByteLruCache<K, V>
where
    K: Eq + Hash + Send + Sync,
    V: Send + Sync,
{
    /// Creates an empty cache with the supplied byte ceiling and label.
    ///
    /// A zero ceiling is valid and causes every inserted entry to be
    /// immediately evicted. This is useful when a caller disables a cache
    /// without changing its construction path.
    #[must_use]
    pub fn new(max_bytes: u64, label: impl Into<String>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: lru::LruCache::unbounded(),
                current_bytes: 0,
                max_bytes,
            }),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            label: label.into(),
        }
    }

    /// Returns the caller-supplied label for this cache.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Looks up a key and promotes a hit to the most-recently-used position.
    ///
    /// # Panics
    ///
    /// Panics if another thread poisoned the cache lock.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<Arc<V>> {
        let mut guard = self.inner.lock().expect("cache lock poisoned");
        if let Some(entry) = guard.entries.get(key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(Arc::clone(&entry.value))
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Inserts or replaces an entry and evicts least-recently-used entries
    /// until the byte ceiling is satisfied.
    ///
    /// The `weight_bytes` argument is non-zero by construction, so the
    /// cache's entry count cannot grow without consuming the configured
    /// byte budget.
    ///
    /// # Panics
    ///
    /// Panics if another thread poisoned the cache lock.
    pub fn insert(&self, key: K, value: Arc<V>, weight_bytes: NonZeroU64) {
        let mut guard = self.inner.lock().expect("cache lock poisoned");
        if let Some(previous) = guard.entries.peek(&key) {
            guard.current_bytes = guard
                .current_bytes
                .saturating_sub(previous.weight_bytes.get());
        }
        guard.current_bytes = guard.current_bytes.saturating_add(weight_bytes.get());
        guard.entries.put(
            key,
            Weighted {
                value,
                weight_bytes,
            },
        );
        self.evict_until_under_cap(&mut guard);
    }

    /// Removes a key if it is present.
    ///
    /// # Panics
    ///
    /// Panics if another thread poisoned the cache lock.
    pub fn evict(&self, key: &K) {
        let mut guard = self.inner.lock().expect("cache lock poisoned");
        if let Some(entry) = guard.entries.pop(key) {
            guard.current_bytes = guard.current_bytes.saturating_sub(entry.weight_bytes.get());
        }
    }

    /// Returns the sum of the weights currently retained by the cache.
    ///
    /// # Panics
    ///
    /// Panics if another thread poisoned the cache lock.
    #[must_use]
    pub fn current_bytes(&self) -> u64 {
        self.inner
            .lock()
            .expect("cache lock poisoned")
            .current_bytes
    }

    /// Returns the configured byte ceiling.
    ///
    /// # Panics
    ///
    /// Panics if another thread poisoned the cache lock.
    #[must_use]
    pub fn max_bytes(&self) -> u64 {
        self.inner.lock().expect("cache lock poisoned").max_bytes
    }

    /// Changes the byte ceiling and immediately evicts entries if necessary.
    ///
    /// # Panics
    ///
    /// Panics if another thread poisoned the cache lock.
    pub fn set_max_bytes(&self, max_bytes: u64) {
        let mut guard = self.inner.lock().expect("cache lock poisoned");
        guard.max_bytes = max_bytes;
        self.evict_until_under_cap(&mut guard);
    }

    /// Returns a point-in-time copy of the cache counters.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
        }
    }

    /// Returns the number of retained entries.
    ///
    /// # Panics
    ///
    /// Panics if another thread poisoned the cache lock.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("cache lock poisoned")
            .entries
            .len()
    }

    /// Returns whether the cache has no retained entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn evict_until_under_cap(&self, guard: &mut Inner<K, V>) {
        while guard.current_bytes > guard.max_bytes {
            let Some((_, entry)) = guard.entries.pop_lru() else {
                break;
            };
            guard.current_bytes = guard.current_bytes.saturating_sub(entry.weight_bytes.get());
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::Arc;

    use super::{ByteLruCache, CacheStats};

    fn weight(bytes: u64) -> NonZeroU64 {
        NonZeroU64::new(bytes).expect("test weights are positive")
    }

    #[test]
    fn stores_and_retrieves_generic_values() {
        let cache = ByteLruCache::new(64, "values");
        cache.insert("answer", Arc::new(42_u32), weight(4));

        assert_eq!(cache.get(&"answer").as_deref(), Some(&42));
        assert_eq!(cache.current_bytes(), 4);
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn misses_are_counted() {
        let cache: ByteLruCache<u32, String> = ByteLruCache::new(64, "values");

        assert!(cache.get(&7).is_none());
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn least_recently_used_entry_is_evicted() {
        let cache = ByteLruCache::new(20, "values");
        cache.insert(1, Arc::new("one"), weight(8));
        cache.insert(2, Arc::new("two"), weight(8));
        let _ = cache.get(&1);
        cache.insert(3, Arc::new("three"), weight(8));

        assert!(cache.get(&1).is_some());
        assert!(cache.get(&2).is_none());
        assert!(cache.get(&3).is_some());
        assert!(cache.current_bytes() <= 20);
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn replacement_updates_weight_without_double_counting() {
        let cache = ByteLruCache::new(64, "values");
        cache.insert(1, Arc::new(1_u32), weight(10));
        cache.insert(1, Arc::new(2_u32), weight(30));

        assert_eq!(cache.current_bytes(), 30);
        assert_eq!(cache.get(&1).as_deref(), Some(&2));
    }

    #[test]
    fn reducing_the_ceiling_evicts_immediately() {
        let cache = ByteLruCache::new(1_000, "values");
        for key in 0..10_u32 {
            cache.insert(key, Arc::new(key), weight(100));
        }

        cache.set_max_bytes(250);

        assert!(cache.current_bytes() <= 250);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn zero_ceiling_retains_nothing() {
        let cache = ByteLruCache::new(0, "disabled");
        cache.insert(1_u32, Arc::new(1_u32), weight(1));

        assert!(cache.is_empty());
        assert_eq!(cache.current_bytes(), 0);
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn labels_and_empty_stats_are_stable() {
        let cache: ByteLruCache<u32, u32> = ByteLruCache::new(64, "test-cache");

        assert_eq!(cache.label(), "test-cache");
        assert!(cache.is_empty());
        assert_eq!(cache.stats(), CacheStats::default());
    }
}

#[cfg(feature = "memory-budget")]
impl<K, V> memory_budget::Resizable for ByteLruCache<K, V>
where
    K: Eq + std::hash::Hash + Send + Sync,
    V: Send + Sync,
{
    fn name(&self) -> &str {
        self.label()
    }

    fn current_bytes(&self) -> u64 {
        self.current_bytes()
    }

    fn max_bytes(&self) -> u64 {
        self.max_bytes()
    }

    fn set_max_bytes(&self, new: u64) {
        self.set_max_bytes(new);
    }

    fn stats(&self) -> memory_budget::ResizableStats {
        let stats = self.stats();
        memory_budget::ResizableStats {
            hits: stats.hits,
            misses: stats.misses,
            evictions: stats.evictions,
        }
    }
}

#[cfg(all(test, feature = "memory-budget"))]
mod resizable_tests {
    use std::num::NonZeroU64;
    use std::sync::Arc;

    use memory_budget::Resizable;

    use super::ByteLruCache;

    #[test]
    fn the_cache_is_governable_through_the_resizable_seam() {
        let cache = ByteLruCache::new(1024, "governed");
        cache.insert(
            "answer",
            Arc::new(42_u32),
            NonZeroU64::new(64).expect("positive weight"),
        );
        let governed: &dyn Resizable = &cache;
        assert_eq!(governed.name(), "governed");
        assert_eq!(governed.current_bytes(), 64);
        assert_eq!(governed.max_bytes(), 1024);
        governed.set_max_bytes(32);
        assert_eq!(governed.current_bytes(), 0, "lowering the cap evicts");
        let _ = cache.get(&"answer");
        assert_eq!(governed.stats().misses, 1);
        assert_eq!(governed.stats().evictions, 1);
    }
}
