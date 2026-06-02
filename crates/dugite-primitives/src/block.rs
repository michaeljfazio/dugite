use crate::era::Era;
use crate::hash::{BlockHeaderHash, Hash32};
use crate::time::{BlockNo, SlotNo};
use crate::transaction::Transaction;
use serde::{Deserialize, Serialize};

/// A point on the chain (for chain-sync protocol)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Point {
    /// The genesis / origin point
    Origin,
    /// A specific block identified by slot and hash
    Specific(SlotNo, BlockHeaderHash),
}

impl Point {
    pub fn slot(&self) -> Option<SlotNo> {
        match self {
            Point::Origin => None,
            Point::Specific(slot, _) => Some(*slot),
        }
    }

    pub fn hash(&self) -> Option<&BlockHeaderHash> {
        match self {
            Point::Origin => None,
            Point::Specific(_, hash) => Some(hash),
        }
    }
}

impl std::fmt::Display for Point {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Point::Origin => write!(f, "origin"),
            Point::Specific(slot, hash) => write!(f, "{}@{}", slot, hash),
        }
    }
}

impl PartialOrd for Point {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Point {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Point::Origin, Point::Origin) => std::cmp::Ordering::Equal,
            (Point::Origin, _) => std::cmp::Ordering::Less,
            (_, Point::Origin) => std::cmp::Ordering::Greater,
            (Point::Specific(s1, _), Point::Specific(s2, _)) => s1.cmp(s2),
        }
    }
}

/// Block header (Shelley+ era)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockHeader {
    pub header_hash: BlockHeaderHash,
    pub prev_hash: BlockHeaderHash,
    pub issuer_vkey: Vec<u8>,
    pub vrf_vkey: Vec<u8>,
    pub vrf_result: VrfOutput,
    pub block_number: BlockNo,
    pub slot: SlotNo,
    pub epoch_nonce: Hash32,
    pub body_size: u64,
    pub body_hash: Hash32,
    pub operational_cert: OperationalCert,
    pub protocol_version: ProtocolVersion,
    /// KES signature over the header body (448 bytes for Sum6Kes)
    #[serde(default)]
    pub kes_signature: Vec<u8>,
    /// Pre-computed nonce VRF contribution (eta) for the nonce state machine.
    ///
    /// This is the era-specific, single-step-hashed nonce value fed into
    /// `evolving_nonce = blake2b_256(evolving_nonce || nonce_vrf_output)`:
    ///
    /// - Shelley / Allegra / Mary / Alonzo (TPraos, proto < 7):
    ///   `nonce_vrf_output = blake2b_256(nonce_vrf_cert.output)`
    ///   Uses the *nonce* VRF certificate (separate from the leader certificate),
    ///   hashed once without prefix.  This matches Haskell's `vrfNonceValue`
    ///   in the TPraos era where `hashRaw id (certifiedOutput vrf)`.
    ///
    /// - Babbage / Conway (Praos, proto >= 7):
    ///   `nonce_vrf_output = blake2b_256("N" || vrf_result.output)`
    ///   The single `vrf_result` field replaces both nonce_vrf and leader_vrf.
    ///   The nonce contribution is derived with the "N" tag.  Matches the legacy decoder's
    ///   `HeaderBody::nonce_vrf_output()` and Haskell's `vrfNonceValue` in Praos.
    ///
    /// Empty for Byron blocks (OBFT — no VRF).
    #[serde(default)]
    pub nonce_vrf_output: Vec<u8>,
    /// TPraos nonce VRF proof (80 bytes for Shelley–Alonzo, empty for Praos/Byron).
    ///
    /// In TPraos (proto < 7), the header contains separate leader_vrf and nonce_vrf
    /// certificates. This field preserves the nonce VRF proof so consensus can
    /// cryptographically verify it. For Praos (proto >= 7) there is only one VRF
    /// certificate, so this field is empty.
    #[serde(default)]
    pub nonce_vrf_proof: Vec<u8>,

    /// Dijkstra-era header field: previous epoch nonce (`prevNonce`).
    ///
    /// Added in protocol version 12+ (Dijkstra era) for cross-epoch nonce
    /// chaining adjustments via `prevNonceBlockHeaderL` (see
    /// `Cardano.Ledger.Dijkstra.Era.DijkstraEraBlockHeader`). The field is
    /// obtained from the consensus header, not the ledger block body.
    ///
    /// Wire encoding: the Dijkstra `header_body` may extend the Conway
    /// `array(10)` with an 11th element — a 32-byte bytes value for the
    /// previous epoch nonce, or `null` / absent if not applicable.
    ///
    /// `None` for all pre-Dijkstra eras (Byron through Conway).
    #[serde(default)]
    pub prev_nonce: Option<Hash32>,

    /// Raw CBOR bytes of the header body (the first element of the wire
    /// `[header_body, kes_signature]` array), captured verbatim during decode.
    ///
    /// This is the EXACT message the KES signature signs: Haskell verifies KES
    /// over `serialize'(pvMajor, body)`, and for a canonically-encoded mainnet
    /// block the on-wire header-body bytes ARE that serialization (a relay
    /// cannot alter them without breaking the header hash). Using the wire bytes
    /// avoids a byte-exact re-encoder for every era — critical because the
    /// TPraos (Shelley–Alonzo) body is a flat `array(15)` with two VRF certs and
    /// an inlined opcert/protver, whereas the Praos (Babbage+) body is an
    /// `array(10)` with one VRF cert and a nested opcert.
    ///
    /// `None` for Byron (no Praos KES) and for headers dugite forges itself
    /// (the forge path encodes + signs its own body); in those cases
    /// `verify_kes_signature` falls back to `encode_block_header_body`.
    #[serde(default)]
    pub raw_header_body: Option<Vec<u8>>,
}

impl BlockHeader {
    /// Whether this header uses the **TPraos** consensus protocol (Shelley
    /// through Alonzo) as opposed to **Praos** (Babbage onward).
    ///
    /// The consensus protocol is a function of the block's ERA / header
    /// STRUCTURE, **not** its `protocol_version`. A TPraos header carries two
    /// VRF certificates (a leader VRF and a separate nonce VRF); a Praos header
    /// carries a single VRF certificate and derives the nonce by hashing its
    /// output, leaving `nonce_vrf_proof` empty. So the presence of a separate
    /// nonce-VRF proof is the authoritative discriminator.
    ///
    /// This matters at the Vasil hard-fork transition: on a from-genesis sync
    /// the `protocol_version` bumps to 7 *mid-epoch* while blocks are still
    /// structurally TPraos/Alonzo (15-field header, separate nonce VRF). Gating
    /// VRF-seed construction / leader checks on `protocol_version >= 7` would
    /// mis-verify those transition blocks (a Praos seed applied to a TPraos
    /// header) and wedge the node. Gate on this instead.
    ///
    /// A header is TPraos iff its protocol version is pre-Babbage (`< 7`) OR it
    /// carries a separate nonce-VRF certificate. The second clause is what
    /// catches the Vasil transition block (PV7 but still a TPraos structure);
    /// the first keeps the classification correct for any pre-Babbage block
    /// regardless of how the nonce field is populated.
    pub fn is_tpraos(&self) -> bool {
        self.protocol_version.major < 7 || !self.nonce_vrf_proof.is_empty()
    }
}

/// VRF output
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VrfOutput {
    pub output: Vec<u8>,
    pub proof: Vec<u8>,
}

/// Operational certificate for block production
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationalCert {
    pub hot_vkey: Vec<u8>,
    pub sequence_number: u64,
    pub kes_period: u64,
    pub sigma: Vec<u8>,
}

/// Protocol version (major.minor)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u64,
    pub minor: u64,
}

impl ProtocolVersion {
    pub fn era(&self) -> Era {
        match self.major {
            0 | 1 => Era::Byron,
            2 => Era::Shelley,
            3 => Era::Allegra,
            4 => Era::Mary,
            5 | 6 => Era::Alonzo,
            7 | 8 => Era::Babbage,
            9..=11 => Era::Conway,
            _ => Era::Dijkstra, // 12+ = Dijkstra (preview testnet activated 2026-05-07)
        }
    }
}

/// A complete block with header and body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub era: Era,
    pub raw_cbor: Option<Vec<u8>>,
}

impl Block {
    pub fn hash(&self) -> &BlockHeaderHash {
        &self.header.header_hash
    }

    pub fn slot(&self) -> SlotNo {
        self.header.slot
    }

    pub fn block_number(&self) -> BlockNo {
        self.header.block_number
    }

    pub fn prev_hash(&self) -> &BlockHeaderHash {
        &self.header.prev_hash
    }

    pub fn tx_count(&self) -> usize {
        self.transactions.len()
    }

    pub fn point(&self) -> Point {
        Point::Specific(self.header.slot, self.header.header_hash)
    }

    pub fn tip(&self) -> Tip {
        Tip {
            point: self.point(),
            block_number: self.header.block_number,
        }
    }

    /// Per-transaction CBOR-body byte ranges within [`Block::raw_cbor`] —
    /// issue #672 M0.4.
    ///
    /// For each transaction `tx` in [`Block::transactions`], returns the
    /// `Range<usize>` such that `block.raw_cbor[range]` is byte-equal to
    /// `tx.raw_body_cbor`. This is the prerequisite for the forthcoming
    /// UTxO RPC mapper that wants to populate utxorpc `Tx.native_bytes`
    /// without re-encoding.
    ///
    /// # Behaviour
    ///
    /// - Returns `None` if [`Block::raw_cbor`] is `None` or if any
    ///   transaction lacks `raw_body_cbor`.
    /// - Returns `None` if any transaction body cannot be located within
    ///   the block CBOR (which indicates upstream decoder drift — the
    ///   invariant `tx.raw_body_cbor` ⊆ `block.raw_cbor` is expected).
    /// - Uses a left-to-right moving cursor so that two transactions with
    ///   identical body bytes (a body-hash collision — practically
    ///   impossible) would map to distinct ranges.
    ///
    /// # Implementation
    ///
    /// Substring search via [`slice::windows`]. Cardano blocks are bounded
    /// (~72 KB on mainnet) so the O(n·m) cost is acceptable; per-call
    /// runtime stays well under one millisecond. A future decoder-side
    /// capture path (Path B in the M0 design) can replace this if
    /// profiling shows the search is hot for RPC consumers.
    ///
    /// # Note on full-tx ranges
    ///
    /// Cardano blocks store transactions in parallel arrays
    /// (`transaction_bodies`, `transaction_witness_sets`,
    /// `auxiliary_data_set`, `invalid_transactions`) rather than as a
    /// single contiguous per-tx CBOR. Therefore only the BODY range is
    /// recoverable from `block.raw_cbor`. Consumers wanting the wire-
    /// format whole-tx CBOR (`[body, witness_set, valid?, aux?]`) must
    /// re-assemble from `tx.raw_body_cbor` / `tx.raw_witness_cbor` /
    /// `tx.is_valid` / `tx.auxiliary_data` — that re-assembly lives in
    /// `dugite-rpc::map::tx`, not here.
    pub fn tx_byte_ranges(&self) -> Option<Vec<std::ops::Range<usize>>> {
        let block_cbor: &[u8] = self.raw_cbor.as_deref()?;
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::with_capacity(self.transactions.len());
        let mut cursor: usize = 0;

        for tx in &self.transactions {
            let body_cbor: &[u8] = tx.raw_body_cbor.as_deref()?;
            if body_cbor.is_empty() {
                // An empty body is malformed but we report a degenerate
                // zero-length range at the cursor rather than failing the
                // whole call — protects RPC consumers from a single bad tx.
                ranges.push(cursor..cursor);
                continue;
            }
            let haystack = block_cbor.get(cursor..)?;
            let offset = haystack
                .windows(body_cbor.len())
                .position(|w| w == body_cbor)?;
            let start = cursor + offset;
            let end = start + body_cbor.len();
            ranges.push(start..end);
            cursor = end;
        }

        Some(ranges)
    }
}

/// Chain tip information
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tip {
    pub point: Point,
    pub block_number: BlockNo,
}

impl Tip {
    pub fn origin() -> Self {
        Tip {
            point: Point::Origin,
            block_number: BlockNo(0),
        }
    }
}

impl std::fmt::Display for Tip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (block {})", self.point, self.block_number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Hash;
    use crate::time::{BlockNo, SlotNo};

    fn test_block_hash() -> BlockHeaderHash {
        Hash::from_bytes([0xab; 32])
    }

    fn test_block_hash_2() -> BlockHeaderHash {
        Hash::from_bytes([0xcd; 32])
    }

    /// Helper: build a minimal BlockHeader for testing Block accessors.
    fn test_header(slot: u64, block_number: u64) -> BlockHeader {
        BlockHeader {
            header_hash: test_block_hash(),
            prev_hash: test_block_hash_2(),
            issuer_vkey: vec![],
            vrf_vkey: vec![],
            vrf_result: VrfOutput {
                output: vec![],
                proof: vec![],
            },
            block_number: BlockNo(block_number),
            slot: SlotNo(slot),
            epoch_nonce: Hash::from_bytes([0; 32]),
            body_size: 0,
            body_hash: Hash::from_bytes([0; 32]),
            operational_cert: OperationalCert {
                hot_vkey: vec![],
                sequence_number: 0,
                kes_period: 0,
                sigma: vec![],
            },
            protocol_version: ProtocolVersion { major: 9, minor: 0 },
            kes_signature: vec![],
            nonce_vrf_output: vec![],
            nonce_vrf_proof: vec![],
            prev_nonce: None,
            raw_header_body: None,
        }
    }

    fn test_block(slot: u64, block_number: u64, num_txs: usize) -> Block {
        Block {
            header: test_header(slot, block_number),
            transactions: (0..num_txs)
                .map(|_| Transaction::empty_with_hash(Hash::from_bytes([0; 32])))
                .collect(),
            era: Era::Conway,
            raw_cbor: None,
        }
    }

    // ========== Point ==========

    #[test]
    fn test_point_origin_slot_is_none() {
        assert_eq!(Point::Origin.slot(), None);
    }

    #[test]
    fn test_point_origin_hash_is_none() {
        assert_eq!(Point::Origin.hash(), None);
    }

    #[test]
    fn test_point_specific_slot() {
        let p = Point::Specific(SlotNo(42), test_block_hash());
        assert_eq!(p.slot(), Some(SlotNo(42)));
    }

    #[test]
    fn test_point_specific_hash() {
        let h = test_block_hash();
        let p = Point::Specific(SlotNo(0), h);
        assert_eq!(p.hash(), Some(&test_block_hash()));
    }

    #[test]
    fn test_point_display_origin() {
        assert_eq!(Point::Origin.to_string(), "origin");
    }

    #[test]
    fn test_point_display_specific() {
        let p = Point::Specific(SlotNo(100), test_block_hash());
        let s = p.to_string();
        // SlotNo displays as "slot:100"
        assert!(s.starts_with("slot:100@"));
        assert!(s.contains("abababab"));
    }

    #[test]
    fn test_point_ord_origin_less_than_specific() {
        let specific = Point::Specific(SlotNo(0), test_block_hash());
        assert!(Point::Origin < specific);
    }

    #[test]
    fn test_point_ord_origin_equal_origin() {
        assert_eq!(Point::Origin.cmp(&Point::Origin), std::cmp::Ordering::Equal);
    }

    #[test]
    fn test_point_ord_specific_greater_than_origin() {
        let specific = Point::Specific(SlotNo(0), test_block_hash());
        assert!(specific > Point::Origin);
    }

    #[test]
    fn test_point_ord_specific_by_slot() {
        let p1 = Point::Specific(SlotNo(10), test_block_hash());
        let p2 = Point::Specific(SlotNo(20), test_block_hash_2());
        assert!(p1 < p2);
    }

    #[test]
    fn test_point_ord_same_slot_is_equal() {
        // Ord compares by slot only, ignoring hash
        let p1 = Point::Specific(SlotNo(10), test_block_hash());
        let p2 = Point::Specific(SlotNo(10), test_block_hash_2());
        assert_eq!(p1.cmp(&p2), std::cmp::Ordering::Equal);
    }

    #[test]
    fn test_point_serde_roundtrip_origin() {
        let p = Point::Origin;
        let json = serde_json::to_string(&p).unwrap();
        let p2: Point = serde_json::from_str(&json).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn test_point_serde_roundtrip_specific() {
        let p = Point::Specific(SlotNo(999), test_block_hash());
        let json = serde_json::to_string(&p).unwrap();
        let p2: Point = serde_json::from_str(&json).unwrap();
        assert_eq!(p, p2);
    }

    // ========== ProtocolVersion::era() ==========

    #[test]
    fn test_protocol_version_era_byron() {
        assert_eq!(ProtocolVersion { major: 0, minor: 0 }.era(), Era::Byron);
        assert_eq!(ProtocolVersion { major: 1, minor: 0 }.era(), Era::Byron);
    }

    #[test]
    fn test_protocol_version_era_shelley() {
        assert_eq!(ProtocolVersion { major: 2, minor: 0 }.era(), Era::Shelley);
    }

    #[test]
    fn test_protocol_version_era_allegra() {
        assert_eq!(ProtocolVersion { major: 3, minor: 0 }.era(), Era::Allegra);
    }

    #[test]
    fn test_protocol_version_era_mary() {
        assert_eq!(ProtocolVersion { major: 4, minor: 0 }.era(), Era::Mary);
    }

    #[test]
    fn test_protocol_version_era_alonzo() {
        assert_eq!(ProtocolVersion { major: 5, minor: 0 }.era(), Era::Alonzo);
        assert_eq!(ProtocolVersion { major: 6, minor: 0 }.era(), Era::Alonzo);
    }

    #[test]
    fn test_protocol_version_era_babbage() {
        assert_eq!(ProtocolVersion { major: 7, minor: 0 }.era(), Era::Babbage);
        assert_eq!(ProtocolVersion { major: 8, minor: 0 }.era(), Era::Babbage);
    }

    #[test]
    fn test_protocol_version_era_conway() {
        assert_eq!(ProtocolVersion { major: 9, minor: 0 }.era(), Era::Conway);
        assert_eq!(
            ProtocolVersion {
                major: 10,
                minor: 0
            }
            .era(),
            Era::Conway
        );
        assert_eq!(
            ProtocolVersion {
                major: 11,
                minor: 0
            }
            .era(),
            Era::Conway
        );
    }

    #[test]
    fn test_protocol_version_era_dijkstra() {
        assert_eq!(
            ProtocolVersion {
                major: 12,
                minor: 0
            }
            .era(),
            Era::Dijkstra
        );
        assert_eq!(
            ProtocolVersion {
                major: 100,
                minor: 0
            }
            .era(),
            Era::Dijkstra
        );
    }

    // ========== Block accessors ==========

    #[test]
    fn test_block_hash_accessor() {
        let block = test_block(100, 5, 0);
        assert_eq!(block.hash(), &test_block_hash());
    }

    #[test]
    fn test_block_slot() {
        let block = test_block(42, 5, 0);
        assert_eq!(block.slot(), SlotNo(42));
    }

    #[test]
    fn test_block_number() {
        let block = test_block(0, 99, 0);
        assert_eq!(block.block_number(), BlockNo(99));
    }

    #[test]
    fn test_block_prev_hash() {
        let block = test_block(0, 0, 0);
        assert_eq!(block.prev_hash(), &test_block_hash_2());
    }

    #[test]
    fn test_block_tx_count() {
        assert_eq!(test_block(0, 0, 0).tx_count(), 0);
        assert_eq!(test_block(0, 0, 3).tx_count(), 3);
    }

    #[test]
    fn test_block_point() {
        let block = test_block(42, 5, 0);
        assert_eq!(
            block.point(),
            Point::Specific(SlotNo(42), test_block_hash())
        );
    }

    #[test]
    fn test_block_tip() {
        let block = test_block(42, 5, 0);
        let tip = block.tip();
        assert_eq!(tip.point, Point::Specific(SlotNo(42), test_block_hash()));
        assert_eq!(tip.block_number, BlockNo(5));
    }

    // ========== Tip ==========

    #[test]
    fn test_tip_origin() {
        let tip = Tip::origin();
        assert_eq!(tip.point, Point::Origin);
        assert_eq!(tip.block_number, BlockNo(0));
    }

    #[test]
    fn test_tip_display_origin() {
        let tip = Tip::origin();
        assert_eq!(tip.to_string(), "origin (block block:0)");
    }

    #[test]
    fn test_tip_display_specific() {
        let tip = Tip {
            point: Point::Specific(SlotNo(100), test_block_hash()),
            block_number: BlockNo(50),
        };
        let s = tip.to_string();
        assert!(s.starts_with("slot:100@"));
        assert!(s.ends_with("(block block:50)"));
    }

    #[test]
    fn test_tip_serde_roundtrip() {
        let tip = Tip {
            point: Point::Specific(SlotNo(100), test_block_hash()),
            block_number: BlockNo(50),
        };
        let json = serde_json::to_string(&tip).unwrap();
        let tip2: Tip = serde_json::from_str(&json).unwrap();
        assert_eq!(tip, tip2);
    }

    // ========== tx_byte_ranges (#672 M0.4) ==========

    /// Helper: build a Block where `raw_cbor` is a synthetic byte sequence
    /// composed of `leading || body_1 || gap_1 || body_2 || ... || trailing`,
    /// and the constituent transactions have `raw_body_cbor` set to each
    /// `body_i`. Mirrors the post-decode invariant that `tx.raw_body_cbor`
    /// is a verbatim slice of `block.raw_cbor`.
    fn synth_block_with_bodies(bodies: &[Vec<u8>], gap: &[u8]) -> Block {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"\x83\x05"); // arbitrary leading bytes (not parsed)
        for (i, body) in bodies.iter().enumerate() {
            raw.extend_from_slice(body);
            if i + 1 < bodies.len() {
                raw.extend_from_slice(gap);
            }
        }
        raw.extend_from_slice(b"\xFF\xFF"); // arbitrary trailing bytes

        let transactions: Vec<Transaction> = bodies
            .iter()
            .enumerate()
            .map(|(i, body)| {
                let mut tx = Transaction::empty_with_hash(Hash::from_bytes([i as u8; 32]));
                tx.raw_body_cbor = Some(body.clone());
                tx
            })
            .collect();

        Block {
            header: test_header(1, 1),
            transactions,
            era: Era::Conway,
            raw_cbor: Some(raw),
        }
    }

    #[test]
    fn tx_byte_ranges_locates_each_body_in_block_cbor() {
        let bodies = vec![
            vec![0xA1, 0x01, 0x02, 0x03],
            vec![0xB2, 0x04, 0x05],
            vec![0xC3, 0x06, 0x07, 0x08, 0x09],
        ];
        let block = synth_block_with_bodies(&bodies, &[0x00, 0x00]);
        let raw_cbor = block.raw_cbor.as_deref().unwrap();

        let ranges = block.tx_byte_ranges().expect("ranges");

        assert_eq!(ranges.len(), bodies.len());
        for (range, body) in ranges.iter().zip(bodies.iter()) {
            assert_eq!(&raw_cbor[range.clone()], body.as_slice());
        }
    }

    #[test]
    fn tx_byte_ranges_returns_none_when_block_cbor_missing() {
        let mut block = synth_block_with_bodies(&[vec![0x01, 0x02]], &[]);
        block.raw_cbor = None;
        assert!(block.tx_byte_ranges().is_none());
    }

    #[test]
    fn tx_byte_ranges_returns_none_when_any_body_cbor_missing() {
        let mut block = synth_block_with_bodies(&[vec![0x01, 0x02]], &[]);
        block.transactions[0].raw_body_cbor = None;
        assert!(block.tx_byte_ranges().is_none());
    }

    #[test]
    fn tx_byte_ranges_handles_duplicate_bodies_via_moving_cursor() {
        // Two identical bodies — confirms ranges are distinct and ordered.
        let body = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let bodies = vec![body.clone(), body.clone()];
        let block = synth_block_with_bodies(&bodies, &[0xAA]);
        let ranges = block.tx_byte_ranges().expect("ranges");

        assert_eq!(ranges.len(), 2);
        assert!(ranges[0].end <= ranges[1].start, "ranges must be ordered");
        let raw = block.raw_cbor.as_deref().unwrap();
        assert_eq!(&raw[ranges[0].clone()], body.as_slice());
        assert_eq!(&raw[ranges[1].clone()], body.as_slice());
    }

    #[test]
    fn tx_byte_ranges_returns_none_when_body_not_in_block_cbor() {
        let mut block = synth_block_with_bodies(&[vec![0x01, 0x02]], &[]);
        // Corrupt: body cbor doesn't actually appear in the block cbor.
        block.transactions[0].raw_body_cbor = Some(vec![0xFE, 0xED, 0xFA, 0xCE]);
        assert!(block.tx_byte_ranges().is_none());
    }

    #[test]
    fn tx_byte_ranges_empty_block_returns_empty_vec() {
        let block = Block {
            header: test_header(1, 1),
            transactions: vec![],
            era: Era::Conway,
            raw_cbor: Some(vec![0x80]),
        };
        let ranges = block.tx_byte_ranges().expect("ranges");
        assert!(ranges.is_empty());
    }
}
