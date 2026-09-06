//! Public-API integration test: weighted insertion, lookup, and budget
//! shrinking, from the shipped crate.

use std::num::NonZeroU64;
use std::sync::Arc;

use byte_lru_cache::ByteLruCache;

fn weight(bytes: u64) -> NonZeroU64 {
    NonZeroU64::new(bytes).expect("positive weight")
}

#[test]
fn inserted_values_are_found_and_weighed() {
    let cache = ByteLruCache::new(1024, "public-api");
    cache.insert("answer", Arc::new(42_u32), weight(4));
    assert_eq!(cache.get(&"answer").as_deref(), Some(&42));
    assert_eq!(cache.get(&"missing"), None);
    assert_eq!(cache.current_bytes(), 4);
    assert_eq!(cache.len(), 1);
}

#[test]
fn shrinking_the_budget_evicts_until_it_fits() {
    let cache = ByteLruCache::new(1024, "public-api");
    for key in 0_u32..8 {
        cache.insert(key, Arc::new(key), weight(100));
    }
    assert_eq!(cache.current_bytes(), 800);
    cache.set_max_bytes(250);
    assert!(cache.current_bytes() <= 250, "{}", cache.current_bytes());
    assert!(cache.len() <= 2);
}
