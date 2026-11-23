//! Bounded second-chance caches for one KS database context. Invalidating a
//! context still clears its entries; capacity eviction only removes cold items.
use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
};

pub(super) struct ClockCache<K, V> {
    items: HashMap<K, (V, usize, bool)>,
    clock: VecDeque<K>,
    bytes: usize,
}
impl<K, V> Default for ClockCache<K, V> {
    fn default() -> Self {
        Self {
            items: HashMap::new(),
            clock: VecDeque::new(),
            bytes: 0,
        }
    }
}
impl<K: Copy + Eq + Hash, V> ClockCache<K, V> {
    pub(super) fn get(&mut self, key: &K) -> Option<&V> {
        self.get_mut(key).map(|value| &*value)
    }
    pub(super) fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let (value, _, referenced) = self.items.get_mut(key)?;
        *referenced = true;
        Some(value)
    }
    pub(super) fn contains_key(&self, key: &K) -> bool {
        self.items.contains_key(key)
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = &K> {
        self.items.keys()
    }
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
    pub(super) fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        self.items.retain(|key, (value, bytes, _)| {
            if keep(key, value) {
                true
            } else {
                self.bytes -= *bytes;
                false
            }
        });
        self.clock.retain(|key| self.items.contains_key(key));
    }
    pub(super) fn insert(&mut self, key: K, value: V, bytes: usize, budget: usize, count: usize) {
        if bytes > budget || count == 0 || self.items.contains_key(&key) {
            return;
        }
        while self.bytes + bytes > budget || self.items.len() >= count {
            let oldest = self.clock.pop_front().unwrap();
            let (_, _, referenced) = self.items.get_mut(&oldest).unwrap();
            if *referenced {
                *referenced = false;
                self.clock.push_back(oldest);
            } else {
                self.bytes -= self.items.remove(&oldest).unwrap().1;
            }
        }
        self.bytes += bytes;
        self.clock.push_back(key);
        self.items.insert(key, (value, bytes, true));
    }
}
