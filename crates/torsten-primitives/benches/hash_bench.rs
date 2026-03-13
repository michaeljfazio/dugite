use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use torsten_primitives::hash::{blake2b_224, blake2b_256};

fn bench_blake2b_256(c: &mut Criterion) {
    let mut group = c.benchmark_group("blake2b_256");

    // Typical Cardano payload sizes
    let sizes: &[(&str, usize)] = &[
        ("32B_txhash", 32),
        ("64B_vkey", 64),
        ("256B_small_tx", 256),
        ("1KB_tx_body", 1024),
        ("4KB_large_tx", 4096),
        ("16KB_block_header", 16384),
    ];

    for (label, size) in sizes {
        let data = vec![0xABu8; *size];
        group.bench_with_input(BenchmarkId::new("hash", label), &data, |b, data| {
            b.iter(|| blake2b_256(black_box(data)))
        });
    }

    group.finish();
}

fn bench_blake2b_224(c: &mut Criterion) {
    let mut group = c.benchmark_group("blake2b_224");

    let sizes: &[(&str, usize)] = &[
        ("32B_vkey_to_keyhash", 32),
        ("64B_script_bytes", 64),
        ("256B_address_payload", 256),
    ];

    for (label, size) in sizes {
        let data = vec![0xCDu8; *size];
        group.bench_with_input(BenchmarkId::new("hash", label), &data, |b, data| {
            b.iter(|| blake2b_224(black_box(data)))
        });
    }

    group.finish();
}

fn bench_blake2b_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("blake2b_batch");

    // Simulate hashing all vkey witnesses in a block (typical: 50-500 witnesses)
    for count in [10, 50, 100, 500] {
        let keys: Vec<Vec<u8>> = (0..count).map(|i| vec![i as u8; 32]).collect();
        group.bench_with_input(
            BenchmarkId::new("224_keyhashes", count),
            &keys,
            |b, keys| {
                b.iter(|| {
                    for key in keys {
                        black_box(blake2b_224(key));
                    }
                })
            },
        );
    }

    // Simulate hashing transaction bodies in a block
    for count in [10, 50, 100] {
        let bodies: Vec<Vec<u8>> = (0..count).map(|i| vec![i as u8; 512]).collect();
        group.bench_with_input(
            BenchmarkId::new("256_txbodies", count),
            &bodies,
            |b, bodies| {
                b.iter(|| {
                    for body in bodies {
                        black_box(blake2b_256(body));
                    }
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_blake2b_256,
    bench_blake2b_224,
    bench_blake2b_batch
);
criterion_main!(benches);
