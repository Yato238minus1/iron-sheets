//! A tiny size-bounded LRU map.

use std::collections::BTreeMap;
use std::hash::Hash;

use rustc_hash::FxHashMap;

pub struct Lru<K, V> {
    map: FxHashMap<K, Entry<V>>,
    order: BTreeMap<u64, K>,
    tick: u64,
    bytes: usize,
    budget: usize,
}

struct Entry<V> {
    value: V,
    tick: u64,
    size: usize,
}

impl<K: Hash + Eq + Copy, V> Lru<K, V> {
    pub fn new(budget: usize) -> Self {
        Lru { map: FxHashMap::default(), order: BTreeMap::new(), tick: 0, bytes: 0, budget }
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let entry = self.map.get_mut(key)?;
        self.order.remove(&entry.tick);
        self.tick += 1;
        entry.tick = self.tick;
        self.order.insert(self.tick, *key);
        Some(&entry.value)
    }

    pub fn insert(&mut self, key: K, value: V, size: usize) {
        self.remove(&key);
        self.tick += 1;
        self.order.insert(self.tick, key);
        self.bytes += size;
        self.map.insert(key, Entry { value, tick: self.tick, size });
        while self.bytes > self.budget && self.map.len() > 1 {
            let Some((&tick, &oldest)) = self.order.iter().next() else { break };
            self.order.remove(&tick);
            if let Some(entry) = self.map.remove(&oldest) {
                self.bytes -= entry.size;
            }
        }
    }

    pub fn remove(&mut self, key: &K) {
        if let Some(entry) = self.map.remove(key) {
            self.order.remove(&entry.tick);
            self.bytes -= entry.size;
        }
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        let dead: Vec<K> = self.map.keys().filter(|k| !keep(k)).copied().collect();
        for key in dead {
            self.remove(&key);
        }
    }
}
