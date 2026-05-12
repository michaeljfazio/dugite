//! Criterion benchmarks for the network subsystem.
//!
//! Covers:
//!   - ChainSync `MsgRollForward` / `MsgRollBackward` encode + decode
//!   - BlockFetch `MsgBlock` encode + decode (small + large)
//!   - LocalStateQuery HFC success / tag24 wrapping (PParams- and GovState-sized payloads)
//!
//! Handshake encode/decode lives behind private helpers (`encode_propose_versions_*`)
//! so we exercise it via a representative manual CBOR roundtrip of `N2NVersionData`
//! and `N2CVersionData` shapes.
//!
//! Run: `cargo bench -p dugite-network --bench network_bench`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use minicbor::Encoder;

use dugite_network::codec::Point;
use dugite_network::handshake::{N2CVersionData, N2NVersionData};
use dugite_network::protocol::blockfetch::{
    decode_message as bf_decode, encode_message as bf_encode, BlockFetchMessage,
};
use dugite_network::protocol::chainsync::{
    decode_message as cs_decode, encode_message as cs_encode, ChainSyncMessage,
};
use dugite_network::protocol::local_state_query::encoding::{
    encode_cbor_tag24, encode_hfc_era_mismatch, wrap_hfc_success,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const TIP_HASH: [u8; 32] = [0xAB; 32];

/// Build a representative HFC-wrapped header blob `[era_id, #6.24(bstr(inner))]`.
fn make_hfc_header(inner_size: usize) -> Vec<u8> {
    let inner = vec![0x77u8; inner_size];
    let mut buf = Vec::new();
    {
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(6).unwrap(); // Conway era id
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        enc.bytes(&inner).unwrap();
    }
    buf
}

fn roll_forward(header_size: usize) -> ChainSyncMessage {
    ChainSyncMessage::MsgRollForward {
        header: make_hfc_header(header_size),
        tip_slot: 111_661_041,
        tip_hash: TIP_HASH,
        tip_block_number: 4_265_661,
    }
}

fn roll_backward() -> ChainSyncMessage {
    ChainSyncMessage::MsgRollBackward {
        point: Point::Specific(111_660_000, TIP_HASH),
        tip_slot: 111_661_041,
        tip_hash: TIP_HASH,
        tip_block_number: 4_265_661,
    }
}

/// Representative PParams-shaped payload: a 31-entry integer-keyed map of small ints.
fn make_pparams_payload() -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(31).unwrap();
    for i in 0..31u64 {
        enc.u64(i.wrapping_mul(101)).unwrap();
    }
    buf
}

/// Representative GovState-sized payload (Conway gov-state CBOR is typically a
/// few hundred bytes for an idle ledger). 800 bytes is a realistic median.
fn make_govstate_payload() -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(7).unwrap();
    for _ in 0..6 {
        enc.bytes(&[0xCDu8; 128]).unwrap();
    }
    enc.array(0).unwrap();
    buf
}

// ---------------------------------------------------------------------------
// Handshake encode/decode (shape roundtrip — public encode_propose helpers
// are crate-private, so we benchmark the equivalent CBOR-shape work)
// ---------------------------------------------------------------------------

fn encode_n2n_version_data(d: &N2NVersionData) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(4).unwrap();
    enc.u64(d.network_magic).unwrap();
    enc.bool(d.initiator_only).unwrap();
    enc.bool(d.peer_sharing).unwrap();
    enc.bool(d.query).unwrap();
    buf
}

fn encode_n2c_version_data(d: &N2CVersionData) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).unwrap();
    enc.u64(d.network_magic).unwrap();
    enc.bool(d.query).unwrap();
    buf
}

fn bench_handshake_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("network/handshake_encode");

    let n2n = N2NVersionData {
        network_magic: 764_824_073,
        initiator_only: false,
        peer_sharing: true,
        query: false,
    };
    let n2c = N2CVersionData {
        network_magic: 764_824_073,
        query: false,
    };

    group.bench_function("n2n_version_data", |b| {
        b.iter(|| black_box(encode_n2n_version_data(&n2n)));
    });
    group.bench_function("n2c_version_data", |b| {
        b.iter(|| black_box(encode_n2c_version_data(&n2c)));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// ChainSync — RollForward / RollBackward encode + decode
// ---------------------------------------------------------------------------

fn bench_chainsync_roll_forward(c: &mut Criterion) {
    let mut group = c.benchmark_group("network/chainsync/roll_forward");

    for &hdr_size in &[256usize, 1024, 4096] {
        let msg = roll_forward(hdr_size);
        let encoded = cs_encode(&msg);
        group.throughput(Throughput::Bytes(encoded.len() as u64));

        group.bench_with_input(BenchmarkId::new("encode", hdr_size), &msg, |b, msg| {
            b.iter(|| black_box(cs_encode(black_box(msg))));
        });
        group.bench_with_input(
            BenchmarkId::new("decode", hdr_size),
            &encoded,
            |b, encoded| {
                b.iter(|| black_box(cs_decode(black_box(encoded)).unwrap()));
            },
        );
    }

    group.finish();
}

fn bench_chainsync_roll_backward(c: &mut Criterion) {
    let mut group = c.benchmark_group("network/chainsync/roll_backward");
    let msg = roll_backward();
    let encoded = cs_encode(&msg);
    group.throughput(Throughput::Bytes(encoded.len() as u64));

    group.bench_function("encode", |b| {
        b.iter(|| black_box(cs_encode(black_box(&msg))));
    });
    group.bench_function("decode", |b| {
        b.iter(|| black_box(cs_decode(black_box(&encoded)).unwrap()));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// BlockFetch — MsgBlock encode + decode (small + large)
// ---------------------------------------------------------------------------

fn bench_blockfetch_msg_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("network/blockfetch/msg_block");

    // small (~2 KiB header-only test block), median mainnet (~20 KiB),
    // large (~90 KiB — max practical mainnet block).
    for &size in &[2_048usize, 20_480, 90_000] {
        let block = vec![0x42u8; size];
        let msg = BlockFetchMessage::MsgBlock(block);
        let encoded = bf_encode(&msg);
        group.throughput(Throughput::Bytes(encoded.len() as u64));

        group.bench_with_input(BenchmarkId::new("encode", size), &msg, |b, msg| {
            b.iter(|| black_box(bf_encode(black_box(msg))));
        });
        group.bench_with_input(BenchmarkId::new("decode", size), &encoded, |b, encoded| {
            b.iter(|| {
                let decoded = bf_decode(black_box(encoded)).unwrap();
                black_box(decoded);
            });
        });
    }

    group.finish();
}

fn bench_blockfetch_request_range(c: &mut Criterion) {
    let msg = BlockFetchMessage::MsgRequestRange {
        from: Point::Specific(111_000_000, [0x11; 32]),
        to: Point::Specific(111_661_041, TIP_HASH),
    };
    let encoded = bf_encode(&msg);

    let mut group = c.benchmark_group("network/blockfetch/request_range");
    group.bench_function("encode", |b| {
        b.iter(|| black_box(bf_encode(black_box(&msg))));
    });
    group.bench_function("decode", |b| {
        b.iter(|| black_box(bf_decode(black_box(&encoded)).unwrap()));
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// N2C LocalStateQuery — PParams + GovState encode (HFC wrap + tag24)
// ---------------------------------------------------------------------------

fn bench_n2c_query_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("network/n2c_query_encode");

    let pparams = make_pparams_payload();
    let govstate = make_govstate_payload();

    group.throughput(Throughput::Bytes(pparams.len() as u64));
    group.bench_function("pparams/hfc_success", |b| {
        b.iter(|| black_box(wrap_hfc_success(black_box(&pparams))));
    });
    group.bench_function("pparams/tag24", |b| {
        b.iter(|| black_box(encode_cbor_tag24(black_box(&pparams))));
    });

    group.throughput(Throughput::Bytes(govstate.len() as u64));
    group.bench_function("govstate/hfc_success", |b| {
        b.iter(|| black_box(wrap_hfc_success(black_box(&govstate))));
    });
    group.bench_function("govstate/tag24", |b| {
        b.iter(|| black_box(encode_cbor_tag24(black_box(&govstate))));
    });

    group.bench_function("era_mismatch", |b| {
        b.iter(|| black_box(encode_hfc_era_mismatch(black_box(6))));
    });

    group.finish();
}

criterion_group!(
    network_benches,
    bench_handshake_encode,
    bench_chainsync_roll_forward,
    bench_chainsync_roll_backward,
    bench_blockfetch_msg_block,
    bench_blockfetch_request_range,
    bench_n2c_query_encode,
);
criterion_main!(network_benches);
