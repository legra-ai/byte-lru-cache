# byte-lru-cache

[![Crates.io][crates-badge]][crates-url]
[![Documentation][docs-badge]][docs-url]
[![CI][ci-badge]][ci-url]
[![License][license-badge]][license-url]
[![Downloads][downloads-badge]][downloads-url]

Thread-safe LRU caching governed by an explicit byte budget.

`byte-lru-cache` stores typed values behind `Arc` and evicts the
least-recently-used entries when the sum of caller-supplied weights exceeds
the configured ceiling. It performs no I/O, starts no tasks, and has no
runtime dependency.

## Why a byte budget?

An entry-count limit is a poor memory bound when values have different sizes.
This crate makes the accounting decision explicit at insertion time:

```rust
use std::num::NonZeroU64;
use std::sync::Arc;

use byte_lru_cache::ByteLruCache;

let cache = ByteLruCache::new(1024, "parsed-values");
cache.insert(
    "answer",
    Arc::new(42_u32),
    NonZeroU64::new(4).expect("weight is positive"),
);

assert_eq!(cache.get(&"answer").as_deref(), Some(&42));
assert_eq!(cache.current_bytes(), 4);
```

Weights must be positive. This prevents zero-weight entries from bypassing
the memory bound. A value whose weight is larger than the current ceiling is
inserted and then immediately evicted.

The cache tracks the supplied weights, not the allocator's complete heap
footprint. Callers should provide conservative weights for their values.

## Runtime resizing

The ceiling can be lowered or raised while the cache is in use. Lowering it
evicts entries synchronously before the method returns:

```rust
use std::sync::Arc;

use byte_lru_cache::ByteLruCache;

let cache = ByteLruCache::new(1024, "parsed-values");
cache.insert(
    "answer",
    Arc::new(42_u32),
    std::num::NonZeroU64::new(4).expect("weight is positive"),
);

cache.set_max_bytes(512);
assert!(cache.current_bytes() <= 512);
```

The cache is independent of any process-wide memory governor. A governor can
sample `current_bytes()` and call `set_max_bytes()` without adding a runtime,
I/O, or policy dependency to this crate.

## Concurrency

`ByteLruCache<K, V>` is safe to share through `Arc` when `K` and `V` are
`Send + Sync`. Cache operations serialize only the short LRU bookkeeping
critical section. Returned values remain independently owned through their
`Arc` handles.

## License

Licensed under either of:

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE));
- MIT License ([`LICENSE-MIT`](LICENSE-MIT)).

## Links

[crates-badge]: https://img.shields.io/crates/v/byte-lru-cache.svg
[crates-url]: https://crates.io/crates/byte-lru-cache
[docs-badge]: https://docs.rs/byte-lru-cache/badge.svg
[docs-url]: https://docs.rs/byte-lru-cache
[ci-badge]: https://github.com/legra-ai/byte-lru-cache/actions/workflows/ci.yml/badge.svg
[ci-url]: https://github.com/legra-ai/byte-lru-cache/actions/workflows/ci.yml
[license-badge]: https://img.shields.io/crates/l/byte-lru-cache.svg
[license-url]: https://github.com/legra-ai/byte-lru-cache/blob/main/LICENSE-APACHE
[downloads-badge]: https://img.shields.io/crates/d/byte-lru-cache.svg
[downloads-url]: https://crates.io/crates/byte-lru-cache
