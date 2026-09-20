//! A cache bounded by the bytes it holds, evicting least recently used.

use std::collections::HashMap;
use std::hash::Hash;

pub(crate) struct ByteLru<K, V> {
    entries: HashMap<K, Entry<V>>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

struct Entry<V> {
    value: V,
    bytes: usize,
    last_used: u64,
}

impl<K: Hash + Eq + Clone, V> ByteLru<K, V> {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            budget,
            clock: 0,
        }
    }

    /// Look `key` up, marking it as just used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.clock += 1;
        let entry = self.entries.get_mut(key)?;
        entry.last_used = self.clock;
        Some(&entry.value)
    }

    /// Store `value`, accounted as `bytes`, then evict the least recently
    /// used entries until the cache fits its budget again. The entry just
    /// stored always stays, even when it alone exceeds the budget.
    pub fn insert(&mut self, key: K, value: V, bytes: usize) {
        self.clock += 1;
        let entry = Entry {
            value,
            bytes,
            last_used: self.clock,
        };
        if let Some(old) = self.entries.insert(key, entry) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        while self.bytes > self.budget && self.entries.len() > 1 {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            let Some(oldest) = oldest else { break };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes -= evicted.bytes;
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

impl<K, V> std::fmt::Debug for ByteLru<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ByteLru")
            .field("entries", &self.entries.len())
            .field("bytes", &self.bytes)
            .field("budget", &self.budget)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::ByteLru;

    #[test]
    fn evicts_least_recently_used_by_bytes() {
        let mut cache = ByteLru::new(100);
        cache.insert("label", 1, 40);
        cache.insert("score 1", 2, 40);
        // touching the label makes the old score the eviction candidate
        assert_eq!(cache.get(&"label"), Some(&1));
        cache.insert("score 2", 3, 40);
        assert_eq!(cache.get(&"score 1"), None);
        assert_eq!(cache.get(&"label"), Some(&1));
        assert_eq!(cache.get(&"score 2"), Some(&3));
        assert_eq!(cache.bytes, 80);
    }

    #[test]
    fn replacing_a_key_updates_the_byte_count() {
        let mut cache = ByteLru::new(100);
        cache.insert("a", 1, 60);
        cache.insert("a", 2, 10);
        assert_eq!((cache.bytes, cache.len()), (10, 1));
    }

    #[test]
    fn an_oversized_entry_is_kept_alone() {
        let mut cache = ByteLru::new(100);
        cache.insert("small", 1, 10);
        cache.insert("huge", 2, 500);
        assert_eq!(cache.get(&"huge"), Some(&2));
        assert_eq!(cache.len(), 1);
    }
}
