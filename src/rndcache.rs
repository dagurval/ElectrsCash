use crate::metrics::{CounterVec};
use indexmap::IndexMap;
use prometheus::IntGauge;
use rand::prelude::*;
use std::hash::Hash;

pub struct RndCache<K: Eq + Hash, V> {
    map: IndexMap<K, (usize, V)>,
    bytes_capacity: usize,
    bytes_used: usize,
    lookups: CounterVec,
    usage: IntGauge,
}

impl<K: Eq + Hash, V> RndCache<K, V> {
    pub fn new(bytes_capacity: usize, lookups: CounterVec, usage: IntGauge) -> RndCache<K, V> {
        RndCache {
            map: IndexMap::new(),
            bytes_capacity: bytes_capacity,
            bytes_used: 0,
            lookups,
            usage,
        }
    }

    pub fn put(&mut self, k: K, v: V, size: usize) {
        if size > self.bytes_capacity {
            return;
        }

        if !self.fits_in_cache(size) {
            let mut rng = thread_rng();
            loop {
                self.evict_random(&mut rng);
                if self.fits_in_cache(size) {
                    break;
                }
            }
        }
        match self.map.insert(k, (size, v)) {
            Some(v) => {
                // key existed and value was replaced
                let (old_size, _) = v;
                self.bytes_used -= old_size;
            }
            None => {}
        };
        self.bytes_used += size;
        self.usage.set(self.bytes_used as i64)
    }

    pub fn get(&self, k: &K) -> Option<&V> {
        match self.map.get(k) {
            Some(v) => {
                self.lookups.with_label_values(&["hit"]).inc();
                let (_, value) = v;
                Some(value)
            }
            None => {
                self.lookups.with_label_values(&["miss"]).inc();
                None
            }
        }
    }

    pub fn usage(&self) -> usize {
        self.bytes_used
    }

    pub fn capacity(&self) -> usize {
        self.bytes_capacity
    }

    fn fits_in_cache(&self, bytes: usize) -> bool {
        self.bytes_used + bytes <= self.bytes_capacity
    }

    /// Removes a random cache entry
    fn evict_random(&mut self, rng: &mut ThreadRng) {
        let index = rng.gen_range(0, self.map.len());
        let (_, (size, _)) = self.map.swap_remove_index(index).unwrap();
        self.bytes_used -= size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_counter() -> CounterVec {
        CounterVec::new(prometheus::Opts::new("name", "help"), &["type"]).unwrap()
    }

    fn dummy_gauge() -> IntGauge {
        IntGauge::new("usage", "help").unwrap()
    }

    #[test]
    fn test_insert_newitem() {
        let mut cache: RndCache<i32, i32> = RndCache::new(100,
                                                          dummy_counter(),
                                                          dummy_gauge());
        cache.put(10, 10, 10);
        assert_eq!(&10, cache.get(&10).unwrap());
        assert!(!cache.get(&20).is_some());
        cache.put(20, 20, 20);
        assert_eq!(&10, cache.get(&10).unwrap());
        assert_eq!(&20, cache.get(&20).unwrap());

        assert_eq!(30, cache.usage());
    }

    #[test]
    fn test_insert_replace() {
        let mut cache: RndCache<i32, i32> = RndCache::new(100,
                                                          dummy_counter(),
                                                          dummy_gauge());
        cache.put(10, 10, 10);
        assert_eq!(&10, cache.get(&10).unwrap());
        assert_eq!(10, cache.usage());

        cache.put(10, 20, 20);
        assert_eq!(&20, cache.get(&10).unwrap());
        assert_eq!(20, cache.usage());
    }

    #[test]
    fn test_too_big() {
        let capacity = 100;
        let mut cache: RndCache<i32, i32> = RndCache::new(capacity,
                                                          dummy_counter(),
                                                          dummy_gauge());

        cache.put(10, 10, capacity + 1);
        assert!(!cache.get(&10).is_some());

        cache.put(10, 10, capacity);
        assert!(cache.get(&10).is_some());

        cache.put(10, 10, capacity - 1);
        assert!(cache.get(&10).is_some());
    }

    #[test]
    fn test_capacity() {
        let mut cache: RndCache<&str, usize> = RndCache::new(300,
                                                          dummy_counter(),
                                                          dummy_gauge());
        assert_eq!(300, cache.capacity());
        assert_eq!(0, cache.usage());
        cache.put("key1", 10, 100);
        assert_eq!(100, cache.usage());

        // replace cache entry
        cache.put("key1", 10, 150);
        assert_eq!(150, cache.usage());

        // new entry
        cache.put("key2", 10, 60);
        assert_eq!(210, cache.usage());

        // to make space for next entry, both previous entries need
        // to be evicted
        cache.put("key3", 10, 250);
        assert_eq!(250, cache.usage());
    }

    fn count_hits(cache: &RndCache<&str, i32>, keys: Vec<&str>) -> usize {
        let mut hits = 0;
        for k in keys {
            if cache.get(&k).is_some() {
                hits += 1;
            }
        }
        hits
    }

    #[test]
    fn test_evict() {
        let capacity = 300;

        let mut cache: RndCache<&str, i32> = RndCache::new(capacity,
                                                          dummy_counter(),
                                                          dummy_gauge());

        // fill cache
        cache.put("key1", 1, 100);
        cache.put("key2", 2, 100);
        cache.put("key3", 3, 100);
        assert_eq!(cache.capacity(), cache.usage());
        assert_eq!(3, count_hits(&cache, vec!("key1", "key2", "key3")));

        // evict 1
        cache.put("key4", 4, 100);
        assert_eq!(2, count_hits(&cache, vec!("key1", "key2", "key3")));

        // evict all
        cache.put("key5", 5, capacity);
        assert_eq!(0, count_hits(&cache, vec!("key1", "key2", "key3")));
    }
}
