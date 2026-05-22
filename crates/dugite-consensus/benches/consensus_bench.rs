//! Criterion benchmarks for the consensus subsystem.
//!
//! Covers:
//!   - VRF leader-value check throughput (single + batch)
//!   - Praos `validate_header` in Light mode (structural / range checks)
//!   - Chain selection comparing the incumbent against a 100-block fork
//!
//! Run: `cargo bench -p dugite-consensus --bench consensus_bench`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use dugite_consensus::{
    chain_selection::ChainSelection, praos::OuroborosPraos, slot_leader::is_slot_leader,
};
use dugite_primitives::block::{
    BlockHeader, OperationalCert, Point, ProtocolVersion, Tip, VrfOutput,
};
use dugite_primitives::hash::{BlockHeaderHash, Hash32};
use dugite_primitives::time::{BlockNo, EpochLength, SlotNo};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn make_vrf_output(seed: u64) -> [u8; 64] {
    let mut out = [0u8; 64];
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let v = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(i as u64);
        chunk.copy_from_slice(&v.to_le_bytes());
    }
    out
}

fn make_hash(seed: u64) -> Hash32 {
    let mut bytes = [0u8; 32];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let v = seed
            .wrapping_mul(0x517C_C1B7_2722_0A95)
            .wrapping_add(i as u64);
        chunk.copy_from_slice(&v.to_le_bytes());
    }
    Hash32::from_bytes(bytes)
}

fn make_header(block_no: u64, slot: u64, hash_seed: u64) -> BlockHeader {
    BlockHeader {
        header_hash: BlockHeaderHash::from_bytes(*make_hash(hash_seed).as_bytes()),
        prev_hash: BlockHeaderHash::from_bytes(*make_hash(hash_seed.wrapping_sub(1)).as_bytes()),
        issuer_vkey: vec![0x11u8; 32],
        vrf_vkey: vec![0x22u8; 32],
        vrf_result: VrfOutput {
            output: make_vrf_output(hash_seed).to_vec(),
            proof: vec![0u8; 80],
        },
        block_number: BlockNo(block_no),
        slot: SlotNo(slot),
        epoch_nonce: make_hash(0xE7),
        body_size: 20_480,
        body_hash: make_hash(hash_seed ^ 0xBEEF),
        operational_cert: OperationalCert {
            hot_vkey: vec![0x33u8; 32],
            sequence_number: 1,
            kes_period: slot / 129_600,
            sigma: vec![0u8; 64],
        },
        protocol_version: ProtocolVersion {
            major: 10,
            minor: 0,
        },
        kes_signature: vec![0u8; 448],
        nonce_vrf_output: vec![0u8; 32],
        nonce_vrf_proof: vec![],
        prev_nonce: None,
    }
}

fn make_tip(block_no: u64, slot: u64, hash_seed: u64) -> Tip {
    Tip {
        point: Point::Specific(
            SlotNo(slot),
            BlockHeaderHash::from_bytes(*make_hash(hash_seed).as_bytes()),
        ),
        block_number: BlockNo(block_no),
    }
}

// ---------------------------------------------------------------------------
// 1. VRF leader check throughput
// ---------------------------------------------------------------------------

fn bench_vrf_leader_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus/vrf_leader_check");

    let vrf = make_vrf_output(0xDEAD_BEEF);
    // Representative SPO σ values: tiny pool, median preview pool, large pool.
    let stakes = [0.0000247_f64, 0.001_f64, 0.01_f64];
    let f = 0.05_f64;

    for sigma in stakes {
        group.bench_with_input(
            BenchmarkId::new("single", format!("sigma={sigma}")),
            &sigma,
            |b, &sigma| {
                b.iter(|| black_box(is_slot_leader(black_box(&vrf), sigma, f)));
            },
        );
    }

    // Batch — 21_600 checks = 1 hour of preview slots; matches the work a node
    // does each hour while idle. Measure per-iteration throughput in items/s.
    let batch = 21_600u64;
    let outputs: Vec<[u8; 64]> = (0..batch).map(make_vrf_output).collect();
    group.throughput(Throughput::Elements(batch));
    group.sample_size(20);
    group.bench_function(BenchmarkId::new("batch", batch), |b| {
        b.iter(|| {
            let mut hits = 0u64;
            for o in &outputs {
                if is_slot_leader(o, 0.0000247, f) {
                    hits += 1;
                }
            }
            black_box(hits)
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 2. Praos validate_header (Light mode)
// ---------------------------------------------------------------------------

fn bench_validate_header_light(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus/validate_header");

    let praos =
        OuroborosPraos::with_genesis_params(0.05, 2160, EpochLength(86_400), 129_600, 62, 10);
    let header = make_header(4_265_661, 111_661_041, 0xC0FFEE);
    let current_slot = SlotNo(111_662_000);

    group.bench_function("replay_mode", |b| {
        b.iter(|| {
            let r = praos.validate_header(
                black_box(&header),
                black_box(current_slot),
                dugite_consensus::praos::ValidationMode::Replay,
                Some(10),
            );
            black_box(r.is_ok())
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 3. Chain selection — incumbent vs 100-block fork
// ---------------------------------------------------------------------------

fn bench_chain_selection_fork(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus/chain_selection");

    // Build incumbent tip at block 1_000_000, fork tip 100 blocks ahead.
    let mut sel = ChainSelection::new();
    let incumbent_tip = make_tip(1_000_000, 50_000_000, 1);
    let incumbent_header = make_header(1_000_000, 50_000_000, 1);
    sel.set_tip(incumbent_tip);

    let fork_tip = make_tip(1_000_100, 50_000_100, 2);
    let fork_header = make_header(1_000_100, 50_000_100, 2);

    // 100-block fork: caller-level decision is just length-compare in Praos,
    // but the deterministic-tiebreak path is exercised on equal-length forks.
    group.bench_function("longer_fork_100", |b| {
        b.iter(|| {
            black_box(sel.prefer_chain_with_headers(
                black_box(&fork_tip),
                black_box(&incumbent_header),
                black_box(&fork_header),
                dugite_primitives::era::Era::Conway,
                u64::MAX,
            ))
        });
    });

    // Equal-length fork — exercises Praos VRF tiebreaker.
    let equal_tip = make_tip(1_000_000, 50_000_001, 3);
    let equal_header = make_header(1_000_000, 50_000_001, 3);
    group.bench_function("equal_length_tiebreak", |b| {
        b.iter(|| {
            black_box(sel.prefer_chain_with_headers(
                black_box(&equal_tip),
                black_box(&incumbent_header),
                black_box(&equal_header),
                dugite_primitives::era::Era::Conway,
                u64::MAX,
            ))
        });
    });

    // Simple prefer() (no header context, no tiebreak).
    group.bench_function("prefer_simple", |b| {
        b.iter(|| black_box(sel.prefer(black_box(&fork_tip))));
    });

    group.finish();
}

criterion_group!(
    consensus_benches,
    bench_vrf_leader_check,
    bench_validate_header_light,
    bench_chain_selection_fork,
);
criterion_main!(consensus_benches);
