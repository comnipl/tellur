use std::collections::HashMap;
use std::hash::Hash;

/// The value-owning half of a cache.
///
/// Eviction deliberately does not live here. The policy owns residency and
/// recency, while this type only keeps values for the keys that the policy has
/// committed.
#[derive(Debug)]
pub(super) struct ValueStore<K, V> {
    values: HashMap<K, V>,
}

impl<K, V> ValueStore<K, V>
where
    K: Eq + Hash,
{
    pub(super) fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    pub(super) fn contains_key(&self, key: &K) -> bool {
        self.values.contains_key(key)
    }

    /// Maps a borrowed value to an owned lookup result.
    ///
    /// The returned value cannot borrow from the store, so callers are free to
    /// update policy state immediately after this method returns.
    pub(super) fn map<T>(&self, key: &K, map: impl FnOnce(&V) -> T) -> Option<T> {
        self.values.get(key).map(map)
    }

    pub(super) fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.values.insert(key, value)
    }

    pub(super) fn remove(&mut self, key: &K) -> Option<V> {
        self.values.remove(key)
    }

    pub(super) fn clear(&mut self) {
        self.values.clear();
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.values.len()
    }
}

impl<K, V> Default for ValueStore<K, V>
where
    K: Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}
