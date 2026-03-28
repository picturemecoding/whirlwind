# Performance Specification

**Project**: whirlwind
**Language**: Rust (edition 2024)
**Last updated**: 2026-03-28
**Status**: Pre-implementation — no substantive code exists yet

---

## Current State

This project is in its initial bootstrapping state. The entire source base is:

```
src/main.rs  —  fn main() { println!("Hello, world!"); }
```

There are no library modules, no dependencies declared in `Cargo.toml`, no data stores, no network
layer, no concurrency primitives, and no business logic. As a result, no performance
characteristics, bottlenecks, benchmarks, caching strategies, or scaling patterns can be documented
from the codebase at this time.

All sections below describe the current gaps and what must be established as the project grows.

---

## Performance Characteristics

### Known Bottlenecks

None identified — no meaningful code paths exist.

### Hot Paths

None identified.

### Baseline Benchmarks

None established. No benchmarking infrastructure exists.

---

## Caching Strategy

No caching layer of any kind is present. No in-process cache (e.g., `moka`, `lru`), no external
cache (e.g., Redis, Memcached), and no HTTP-level cache headers are configured.

**Gap**: When data retrieval or computation is introduced, a caching strategy must be defined here,
covering: what is cached, cache key design, TTL policy, invalidation triggers, and whether caching
is in-process or out-of-process.

---

## Database and Storage

No database, file storage, or persistence layer exists. No query patterns, connection pooling
configuration, or ORM/query-builder dependencies are present.

**Gap**: When storage is introduced, document here:
- The storage engine and client crate (e.g., `sqlx`, `diesel`, `surrealdb`)
- Connection pool configuration (`max_connections`, `min_connections`, `connect_timeout`, `idle_timeout`)
- Query patterns known to be expensive or that require indexing
- Migration tooling

---

## Concurrency and Async Model

No async runtime (e.g., `tokio`, `async-std`) or threading model is present. The binary runs a
single synchronous `main` function.

**Gap**: When an async runtime is introduced, document here:
- Runtime flavor and configuration (e.g., `tokio::main` with `worker_threads`)
- Thread pool sizing strategy
- Where blocking work is offloaded (e.g., `tokio::task::spawn_blocking`)
- Shared state synchronization primitives in use (`Arc<Mutex<T>>`, `RwLock`, channels, atomics)
- Back-pressure mechanisms for inbound request queues

---

## Batching, Pagination, and Lazy Loading

No batching, pagination, or lazy loading patterns exist.

**Gap**: When collections or bulk data operations are introduced, document here:
- Default page sizes and maximum page size limits
- Cursor-based vs. offset-based pagination approach
- Batch size limits for write operations
- Whether N+1 query prevention is enforced

---

## Memory Management

Rust's ownership model provides deterministic memory management with no garbage collector.
No allocator customization, arena allocation, or object pooling is in use.

**Gap**: If performance profiling reveals allocation pressure in hot paths, document allocator
choices (e.g., `jemalloc` via `tikv-jemallocator`, `mimalloc`) and any arena or bump-allocator
usage here.

---

## Benchmarking Infrastructure

No benchmarking tooling is present. Rust's built-in `#[bench]` (nightly-only) and third-party
crates such as `criterion` or `divan` are not configured.

**Gap**: When performance-critical code is introduced, establish a benchmark suite and document:
- The benchmarking crate in use (recommended: `criterion` or `divan` for stable Rust)
- How to run benchmarks (`cargo bench`)
- Baseline numbers for critical operations
- CI policy on benchmark regressions

---

## Profiling Approach

No profiling workflow is established.

**Gap**: Document the agreed profiling toolchain when adopted. Common choices for Rust on macOS and Linux:
- `cargo flamegraph` (wraps `perf` or `dtrace`) for CPU flame graphs
- `heaptrack` or `memory-profiler` for allocation profiling
- `tokio-console` for async task profiling when `tokio` is in use
- `cargo-criterion` for micro-benchmark history

---

## Scaling Considerations

No deployment topology exists, so no horizontal or vertical scaling strategy can be documented.

**Gap**: When the deployment model is defined, document:
- Whether the service is stateless (horizontal scaling trivial) or stateful
- Expected request volume and latency SLOs
- Resource limits (CPU, memory) per instance
- Auto-scaling triggers if running on a managed platform

---

## Summary of Gaps

| Area | Status |
|---|---|
| Caching | Not present |
| Database / connection pooling | Not present |
| Async runtime | Not present |
| Benchmarking suite | Not present |
| Profiling workflow | Not established |
| Pagination / batching | Not present |
| Scaling topology | Not defined |
| Performance SLOs | Not defined |

This document should be updated as each area is introduced. The first meaningful update should
accompany the first dependency addition or the first I/O-bound code path.
