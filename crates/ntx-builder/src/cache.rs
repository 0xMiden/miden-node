use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;
use tokio::sync::Mutex;

/// A newtype wrapper around an LRU cache. Which ensures that the cache lock is not held across
/// await points.
#[derive(Clone)]
pub struct Cache<K, V>(Arc<Mutex<LruCache<K, V>>>);

impl<K, V> Cache<K, V>
where
    K: Hash + Eq,
    V: Clone,
{
    /// Creates a new cache with the given capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self(Arc::new(Mutex::new(LruCache::new(capacity))))
    }

    /// Retrieves a value from the cache.
    pub async fn get(&self, key: &K) -> Option<V> {
        let mut cache_guard = self.0.lock().await;
        cache_guard.get(key).cloned()
    }

    /// Puts a value into the cache.
    pub async fn put(&mut self, key: K, value: V) {
        let mut cache_guard = self.0.lock().await;
        cache_guard.put(key, value);
    }
}
