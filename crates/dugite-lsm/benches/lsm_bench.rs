//! Criterion benchmarks for the LSM-tree (UTxO on-disk storage).
//!
//! Workload shape mirrors the Cardano UTxO set:
//!   - Key = 36-byte TxIn (32-byte tx hash + 4-byte index)
//!   - Value = ~96 bytes (representative TxOut: addr+coin+datum stub)
//!
//! Covered:
//!   - 10k insert batch (random keys)
//!   - Point lookup throughput
//!   - Range / prefix scan throughput
//!   - `apply_batch` (per-block UTxO diff)
//!   - Snapshot save + load
//!
//! Run: `cargo bench -p dugite-lsm --bench lsm_bench`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use dugite_lsm::{Key, LsmConfig, LsmTree, Value};

const KEY_LEN: usize = 36; // 32-byte tx hash + 4-byte output index
const VAL_LEN: usize = 96; // typical TxOut payload
const POPULATE: usize = 10_000;
const BATCH: usize = 10_000;
const LOOKUPS: usize = 1_000;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lcg_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

fn make_key(seed: u64) -> Key {
    let mut bytes = [0u8; KEY_LEN];
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for chunk in bytes.chunks_mut(8) {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let n = chunk.len();
        chunk.copy_from_slice(&s.to_le_bytes()[..n]);
    }
    Key::from(bytes.as_slice())
}

/// Sequential key — used to ensure ordered-iter coverage and reproducible
/// prefix scans.
fn make_seq_key(idx: u64) -> Key {
    let mut bytes = [0u8; KEY_LEN];
    bytes[0..8].copy_from_slice(&idx.to_be_bytes());
    Key::from(bytes.as_slice())
}

fn make_value(seed: u64) -> Value {
    let mut bytes = vec![0u8; VAL_LEN];
    bytes[0..8].copy_from_slice(&seed.to_le_bytes());
    Value::from(bytes)
}

fn bench_config() -> LsmConfig {
    // Smaller memtable than production so that flushes happen during the bench
    // and we exercise the SSTable path.
    LsmConfig {
        memtable_size: 4 * 1024 * 1024,
        block_cache_size: 16 * 1024 * 1024,
        bloom_filter_bits_per_key: 10,
        ..LsmConfig::default()
    }
}

fn open_populated(path: &std::path::Path, count: usize, seq: bool) -> LsmTree {
    let mut tree = LsmTree::open(path, bench_config()).unwrap();
    tree.set_wal_enabled(false); // bulk-load semantics for setup
    for i in 0..count {
        let k = if seq {
            make_seq_key(i as u64)
        } else {
            make_key(i as u64)
        };
        tree.insert(&k, &make_value(i as u64)).unwrap();
    }
    tree.flush().unwrap();
    tree.set_wal_enabled(true);
    tree
}

// ---------------------------------------------------------------------------
// 1. Insert — 10k random-key batch
// ---------------------------------------------------------------------------

fn bench_insert_random(c: &mut Criterion) {
    let mut group = c.benchmark_group("lsm/insert");
    group.sample_size(10);
    group.throughput(Throughput::Elements(POPULATE as u64));

    group.bench_function(BenchmarkId::new("random_keys", POPULATE), |b| {
        b.iter_batched(
            || {
                let dir = tempfile::tempdir().unwrap();
                let mut tree = LsmTree::open(dir.path(), bench_config()).unwrap();
                tree.set_wal_enabled(false);
                (dir, tree)
            },
            |(_dir, mut tree)| {
                for i in 0..POPULATE {
                    tree.insert(
                        black_box(&make_key(i as u64)),
                        black_box(&make_value(i as u64)),
                    )
                    .unwrap();
                }
                black_box(())
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 2. Point lookup (hit-heavy workload)
// ---------------------------------------------------------------------------

fn bench_point_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("lsm/point_lookup");
    group.throughput(Throughput::Elements(LOOKUPS as u64));

    let dir = tempfile::tempdir().unwrap();
    let tree = open_populated(dir.path(), POPULATE, false);

    let mut rng_state: u64 = 0x00C0_FFEE_F00D;
    let lookup_keys: Vec<Key> = (0..LOOKUPS)
        .map(|_| make_key(lcg_next(&mut rng_state) % POPULATE as u64))
        .collect();

    group.bench_function(BenchmarkId::new("hit_random", POPULATE), |b| {
        b.iter(|| {
            for k in &lookup_keys {
                let v = tree.get(black_box(k)).unwrap();
                black_box(v.is_some());
            }
        });
    });

    // miss path — keys outside the inserted range, exercises bloom filter
    let miss_keys: Vec<Key> = (0..LOOKUPS)
        .map(|i| make_key((POPULATE + i + 1_000_000) as u64))
        .collect();
    group.bench_function(BenchmarkId::new("miss_random", POPULATE), |b| {
        b.iter(|| {
            for k in &miss_keys {
                black_box(tree.get(k).unwrap().is_some());
            }
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 3. Prefix / range scan
// ---------------------------------------------------------------------------

fn bench_prefix_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("lsm/range_scan");
    group.sample_size(20);

    let dir = tempfile::tempdir().unwrap();
    let tree = open_populated(dir.path(), POPULATE, true);

    // Scan a 100-key window starting from a fixed offset.
    let lo = make_seq_key(5_000);
    let hi = make_seq_key(5_100);
    group.throughput(Throughput::Elements(100));
    group.bench_function(BenchmarkId::new("window_100_of_10k", POPULATE), |b| {
        b.iter(|| {
            let iter = tree.range(black_box(&lo), black_box(&hi));
            let mut n = 0u64;
            for kv in iter {
                black_box(kv);
                n += 1;
            }
            black_box(n)
        });
    });

    // Full scan.
    let lo_full = make_seq_key(0);
    let hi_full = make_seq_key(POPULATE as u64);
    group.throughput(Throughput::Elements(POPULATE as u64));
    group.bench_function(BenchmarkId::new("full_scan", POPULATE), |b| {
        b.iter(|| {
            let iter = tree.range(black_box(&lo_full), black_box(&hi_full));
            let mut n = 0u64;
            for kv in iter {
                black_box(kv);
                n += 1;
            }
            black_box(n)
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 4. apply_batch — UTxO diff commit (inserts + deletes)
// ---------------------------------------------------------------------------

fn bench_apply_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("lsm/apply_batch");
    group.sample_size(10);
    group.throughput(Throughput::Elements(BATCH as u64));

    // Pre-build the diff outside the timed region.
    let inserts: Vec<(Key, Value)> = (0..BATCH)
        .map(|i| (make_key(i as u64 + 10_000_000), make_value(i as u64)))
        .collect();
    let deletes: Vec<Key> = (0..(BATCH / 4)).map(|i| make_key(i as u64)).collect();

    group.bench_function(BenchmarkId::new("inserts_10k_deletes_2.5k", BATCH), |b| {
        b.iter_batched(
            || {
                let dir = tempfile::tempdir().unwrap();
                let tree = open_populated(dir.path(), POPULATE, false);
                (dir, tree)
            },
            |(_dir, mut tree)| {
                tree.apply_batch(black_box(&inserts), black_box(&deletes))
                    .unwrap();
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 5. Snapshot save + load
// ---------------------------------------------------------------------------

fn bench_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("lsm/snapshot");
    group.sample_size(10);

    group.bench_function("save_10k", |b| {
        b.iter_batched(
            || {
                let dir = tempfile::tempdir().unwrap();
                let tree = open_populated(dir.path(), POPULATE, false);
                (dir, tree)
            },
            |(_dir, mut tree)| {
                tree.save_snapshot(black_box("bench-snap"), black_box("benchmark"))
                    .unwrap();
            },
            BatchSize::PerIteration,
        );
    });

    group.bench_function("load_10k", |b| {
        b.iter_batched(
            || {
                let dir = tempfile::tempdir().unwrap();
                let mut tree = open_populated(dir.path(), POPULATE, false);
                tree.save_snapshot("bench-snap", "benchmark").unwrap();
                drop(tree);
                dir
            },
            |dir| {
                let tree = LsmTree::open_snapshot(dir.path(), "bench-snap").unwrap();
                black_box(tree);
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

criterion_group!(
    lsm_benches,
    bench_insert_random,
    bench_point_lookup,
    bench_prefix_scan,
    bench_apply_batch,
    bench_snapshot,
);
criterion_main!(lsm_benches);
