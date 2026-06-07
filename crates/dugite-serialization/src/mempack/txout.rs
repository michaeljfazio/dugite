//! MemPack TxOut decoder.
//!
//! The first byte of a MemPack TxOut blob selects the Haskell constructor variant,
//! as defined in `Cardano.Ledger.Alonzo.TxOut` (eras/alonzo/impl/src/Cardano/Ledger/Alonzo/TxOut.hs):
//!
//! | Tag | Variant                                | Fields                                        |
//! |-----|----------------------------------------|-----------------------------------------------|
//! |  0  | `TxOutCompact'`                        | CompactAddr + CompactValue                    |
//! |  1  | `TxOutCompactDH'`                      | CompactAddr + CompactValue + DataHash         |
//! |  2  | `TxOut_AddrHash28_AdaOnly`             | Credential Staking + Addr28Extra + Coin       |
//! |  3  | `TxOut_AddrHash28_AdaOnly_DataHash32`  | Credential Staking + Addr28Extra + Coin + DH  |
//! |  4  | `TxOutCompactDatum` (Babbage+)         | CompactAddr + CompactValue + Datum            |
//! |  5  | `TxOutCompactRefScript` (Babbage+)     | CompactAddr + CompactValue + Datum + Script   |
//!
//! ## Tags 2 and 3 — `Addr28Extra` packed form
//!
//! When a TxOut is an ADA-only output at a base address whose payment and stake
//! credentials are both 28-byte hashes, cardano-ledger uses a compact encoding:
//!
//! ```text
//! tag(1)
//!   Credential Staking           (1-byte tag + 28-byte hash = 29 bytes)
//!   Addr28Extra                  (32 bytes = 4 × Word64 native-endian)
//!   CompactForm Coin             (1 inner tag + VarLen Word64)
//!   [DataHash32 — tag 3 only]    (32 bytes = 4 × Word64 native-endian)
//! ```
//!
//! The `Addr28Extra` holds the payment hash28 plus a 4-bit metadata nibble
//! (network + payment-credential type). Port of the Haskell layout:
//!
//! * `Credential Staking` tag: `0` = `ScriptHashObj`, `1` = `KeyHashObj`
//!   (see `Cardano.Ledger.Credential`). **Note**: this tag convention is the
//!   opposite of the payment-cred bit inside `Addr28Extra`.
//!
//! * `Addr28Extra` = four `Word64` values `(w0, w1, w2, w3)` serialized via
//!   MemPack's native `packM @Word64`, which writes each word as a **native
//!   endian** 8-byte chunk. On all Cardano build targets (x86_64, aarch64) that
//!   is little-endian. The 28-byte payment hash is reconstructed from
//!   `PackedBytes28 w0 w1 w2 (w3 >> 32 :: Word32)` where each slot is written
//!   as **big-endian** bytes (see `Cardano.Crypto.PackedBytes.Internal`):
//!
//!   ```text
//!   payment_hash28 = be_u64(w0) ‖ be_u64(w1) ‖ be_u64(w2) ‖ be_u32(w3 >> 32)
//!   ```
//!
//!   The low 32 bits of `w3` carry the metadata:
//!   - bit 0 (`d.testBit 0`): `1` = `KeyHashObj`, `0` = `ScriptHashObj`
//!   - bit 1 (`d.testBit 1`): `1` = `Mainnet`,    `0` = `Testnet`
//!
//!   See `encodeAddress28` / `decodeAddress28` in `Cardano.Ledger.Alonzo.TxOut`.
//!
//! * `CompactForm Coin` is serialized as `packTagM 0 >> packM (VarLen c)` — an
//!   inner 1-byte tag (`0x00`) followed by a MemPack VarLen Word64. See the
//!   `MemPack (CompactForm Coin)` instance in `Cardano.Ledger.Coin`.
//!
//! * `DataHash32` has the same layout as `Addr28Extra` — 4×Word64 LE — but all
//!   four words form the full 32-byte datum hash (no metadata bits).
//!
//! References:
//! - `IntersectMBO/cardano-ledger/eras/alonzo/impl/src/Cardano/Ledger/Alonzo/TxOut.hs`
//!   (lines ~99-198: `Addr28Extra`, `DataHash32`, `AlonzoTxOut`, `decodeAddress28`,
//!   `MemPack AlonzoTxOut`)
//! - `IntersectMBO/cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Coin.hs`
//!   (lines ~154-164: `instance MemPack (CompactForm Coin)`)
//! - `IntersectMBO/cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Credential.hs`
//!   (lines ~99-112: `instance MemPack (Credential kr)`)
//! - `IntersectMBO/cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Address.hs`
//!   (lines ~266-304: Shelley address header bit layout and `putAddr`)
//! - `IntersectMBO/cardano-base/cardano-crypto-class/src/Cardano/Crypto/PackedBytes/Internal.hs`
//!   (lines ~113-134: `MemPack (PackedBytes n)` — hash slots use big-endian
//!   `writeWord64BE`/`writeWord32BE`)

use crate::error::SerializationError;
use crate::mempack::compact::{decode_compact_addr, decode_compact_value_exact, decode_varlen};

/// A decoded MemPack TxOut.
///
/// Fields are populated according to the tag variant. For every tag, `address`
/// holds a fully-formed Shelley/Byron address byte sequence that can be fed
/// directly into `dugite_primitives::address::Address::from_bytes`, and `coin`
/// holds the lovelace amount (possibly `0` for a multi-asset-only output).
#[derive(Debug, Clone)]
pub struct MemPackTxOut {
    /// MemPack constructor tag (0–5).
    pub tag: u8,
    /// Fully decoded Shelley (or Byron) address bytes, ready for
    /// `Address::from_bytes`.
    pub address: Vec<u8>,
    /// Lovelace amount.
    pub coin: u64,
    /// Raw multi-asset bytes (when CompactValue tag = 1).
    pub multi_asset: Option<Vec<u8>>,
    /// Number of distinct `(policy, asset)` pairs in `multi_asset` (the
    /// `CompactValue` `numMA` header). `0` when ADA-only or when the multi-asset
    /// blob came from the opaque (tags 0–3) path. Needed to split the `rep`
    /// ShortByteString into `(PolicyID, AssetName, Quantity)` triples via
    /// [`crate::mempack::compact::parse_multi_asset_rep`].
    pub num_assets: u64,
    /// 32-byte datum hash (tags 1, 3).
    pub datum_hash: Option<[u8; 32]>,
    /// Inline datum bytes (tags 4, 5).
    pub datum: Option<Vec<u8>>,
    /// Reference script as a MemPack-encoded `AlonzoScript` blob (tag 5).
    ///
    /// The bytes are the raw MemPack serialization of the Haskell `Script era`:
    /// a 1-byte `AlonzoScript` tag (`0` = native/timelock, `1` = Plutus) followed
    /// by the script body. See [`decode_mempack_script`] for the exact layout and
    /// [`ScriptRefKind`] for a fully-classified decode.
    pub script_ref: Option<Vec<u8>>,
    /// Opaque remaining bytes for variants we cannot fully split yet (tag 5
    /// multi-asset payloads, etc.). For tags 0–3 this is always `None`.
    pub opaque_tail: Option<Vec<u8>>,
}

/// Decode a MemPack TxOut from raw bytes.
///
/// The input is the value payload of a `tvar` map entry (already unwrapped from
/// its CBOR bytestring envelope).
///
/// Returns `(txout, bytes_consumed)`.  For a well-formed entry `bytes_consumed`
/// equals `data.len()`.
pub fn decode_mempack_txout(data: &[u8]) -> Result<(MemPackTxOut, usize), SerializationError> {
    if data.is_empty() {
        return Err(SerializationError::CborDecode(
            "mempack_txout: empty input".into(),
        ));
    }

    let tag = data[0];
    match tag {
        0 => decode_tag0(data),
        1 => decode_tag1(data),
        2 => decode_tag2(data),
        3 => decode_tag3(data),
        4 => decode_tag4(data),
        5 => decode_tag5(data),
        _ => Err(SerializationError::CborDecode(format!(
            "mempack_txout: unknown tag {tag}"
        ))),
    }
}

/// Tag 0: `TxOutCompact` — CompactAddr + CompactValue.
fn decode_tag0(data: &[u8]) -> Result<(MemPackTxOut, usize), SerializationError> {
    let mut off = 1; // skip tag byte

    // CompactAddr: VarLen(len) + raw_addr_bytes.
    let (address, addr_consumed) = decode_compact_addr(&data[off..])?;
    off += addr_consumed;

    // CompactValue: tag(0/1) + VarLen(coin) [+ VarLen(numMA) + rep].
    //
    // Parse it EXACTLY (not the opaque "everything to the end" path): the
    // multi-asset `rep` ShortByteString carries its own `VarLen` length, and the
    // `numMA` header is required to split it into (PolicyID, AssetName, Quantity)
    // triples. The opaque path left `num_assets = 0`, so the node-side fold
    // (`parse_multi_asset_rep(rep, 0)`) returned an empty asset map and silently
    // dropped every native token on import — the #10 MultiAssetNotConserved /
    // input_side:0 bug. Verified empirically: 898,515 tag-0 multi-asset preprod
    // UTxOs were all decoded with num_assets=0 before this change.
    let val = decode_compact_value_exact(&data[off..])?;
    off += val.consumed;

    Ok((
        MemPackTxOut {
            tag: 0,
            address,
            coin: val.coin,
            multi_asset: val.multi_asset_raw,
            num_assets: val.num_assets,
            datum_hash: None,
            datum: None,
            script_ref: None,
            opaque_tail: None,
        },
        off,
    ))
}

/// Tag 1: `TxOutCompactDH` — CompactAddr + CompactValue + DataHash(32 bytes).
///
/// The datum hash is the last 32 bytes of the blob.
fn decode_tag1(data: &[u8]) -> Result<(MemPackTxOut, usize), SerializationError> {
    if data.len() < 34 {
        // tag(1) + at minimum some addr + value + 32-byte hash
        return Err(SerializationError::CborDecode(
            "mempack_txout tag 1: too short".into(),
        ));
    }

    let mut off = 1;

    let (address, addr_consumed) = decode_compact_addr(&data[off..])?;
    off += addr_consumed;

    // CompactValue, parsed EXACTLY so we land precisely on the 32-byte DataHash
    // that follows (`TxOutCompactDH' cAddr cValue dataHash`). The previous decoder
    // located the hash as "the last 32 bytes" and treated everything in between as
    // an OPAQUE multi-asset blob with num_assets=0 — so the node-side fold dropped
    // all native tokens (the #10 MultiAssetNotConserved bug; 71,940 tag-1
    // multi-asset preprod UTxOs were affected). The exact parse recovers the
    // numMA header and rep length so the triples can be reconstructed, and the
    // DataHash offset is now derived from the value extent rather than assumed.
    let val = decode_compact_value_exact(&data[off..])?;
    off += val.consumed;

    let datum_end = off.checked_add(32).ok_or_else(|| {
        SerializationError::CborDecode("mempack_txout tag 1: datum hash offset overflow".into())
    })?;
    if datum_end > data.len() {
        return Err(SerializationError::CborDecode(format!(
            "mempack_txout tag 1: need {datum_end} bytes for value + datum hash, have {}",
            data.len()
        )));
    }
    let mut datum_hash_bytes = [0u8; 32];
    datum_hash_bytes.copy_from_slice(&data[off..datum_end]);
    off = datum_end;

    Ok((
        MemPackTxOut {
            tag: 1,
            address,
            coin: val.coin,
            multi_asset: val.multi_asset_raw,
            num_assets: val.num_assets,
            datum_hash: Some(datum_hash_bytes),
            datum: None,
            script_ref: None,
            opaque_tail: None,
        },
        off,
    ))
}

/// Intermediate result of decoding an `Addr28Extra + CompactCoin` payload.
struct Addr28Decoded {
    /// Fully assembled 57-byte Shelley base address.
    address: Vec<u8>,
    /// Lovelace amount from the `CompactForm Coin` VarLen.
    coin: u64,
    /// Total bytes consumed (29 cred + 32 addr28extra + `CompactCoin` length).
    consumed: usize,
}

/// Decode `Credential Staking + Addr28Extra + CompactForm Coin`, returning the
/// reconstructed Shelley base address, coin value, and total bytes consumed.
///
/// `data` must start with the `Credential Staking` tag byte (i.e. `&blob[1..]`
/// for a tag-2/tag-3 TxOut blob). This is shared by both tag-2 and tag-3
/// decoders because the prefix is identical.
fn decode_addr28_payload(data: &[u8]) -> Result<Addr28Decoded, SerializationError> {
    // Credential Staking: 1-byte tag (0 = ScriptHash, 1 = KeyHash) + 28-byte hash.
    if data.len() < 29 {
        return Err(SerializationError::CborDecode(
            "mempack_txout tag 2/3: truncated Credential Staking".into(),
        ));
    }
    let stake_cred_tag = data[0];
    let stake_hash: &[u8; 28] = data[1..29]
        .try_into()
        .expect("slice of length 28 fits [u8; 28]");

    // Addr28Extra: 4 × Word64 native-endian (= little-endian on x86_64/aarch64).
    if data.len() < 29 + 32 {
        return Err(SerializationError::CborDecode(
            "mempack_txout tag 2/3: truncated Addr28Extra".into(),
        ));
    }
    let ae = &data[29..29 + 32];
    let w0 = u64::from_le_bytes(ae[0..8].try_into().unwrap());
    let w1 = u64::from_le_bytes(ae[8..16].try_into().unwrap());
    let w2 = u64::from_le_bytes(ae[16..24].try_into().unwrap());
    let w3 = u64::from_le_bytes(ae[24..32].try_into().unwrap());

    // Payment hash28 = BE(w0) ‖ BE(w1) ‖ BE(w2) ‖ BE(w3 >> 32 as u32).
    let mut payment_hash = [0u8; 28];
    payment_hash[0..8].copy_from_slice(&w0.to_be_bytes());
    payment_hash[8..16].copy_from_slice(&w1.to_be_bytes());
    payment_hash[16..24].copy_from_slice(&w2.to_be_bytes());
    let w3_top: u32 = (w3 >> 32) as u32;
    payment_hash[24..28].copy_from_slice(&w3_top.to_be_bytes());

    // Metadata bits live in the low 32 bits of w3.
    let meta = w3 as u32;
    let payment_is_key = (meta & 0b01) != 0; // bit 0
    let is_mainnet = (meta & 0b10) != 0; // bit 1

    // Reconstruct the 57-byte Shelley base address: header(1) + pay28 + stake28.
    //
    // Shelley base-address header (see Cardano.Ledger.Address):
    //   bit 0: mainnet (1) vs testnet (0)
    //   bit 4: payCredIsScript
    //   bit 5: stakeCredIsScript
    //   bits 6-7: 0b00 (base address)
    //
    // The staking credential tag convention here is 0 = ScriptHashObj,
    // 1 = KeyHashObj (Credential MemPack instance).
    let stake_is_script = match stake_cred_tag {
        0 => true,
        1 => false,
        other => {
            return Err(SerializationError::CborDecode(format!(
                "mempack_txout tag 2/3: invalid Credential Staking tag {other} (expected 0 or 1)"
            )));
        }
    };

    let mut header: u8 = 0;
    if is_mainnet {
        header |= 0b0000_0001;
    }
    if !payment_is_key {
        header |= 0b0001_0000;
    }
    if stake_is_script {
        header |= 0b0010_0000;
    }

    let mut address = Vec::with_capacity(57);
    address.push(header);
    address.extend_from_slice(&payment_hash);
    address.extend_from_slice(stake_hash);
    debug_assert_eq!(address.len(), 57);

    // CompactForm Coin: inner tag byte (must be 0) + VarLen Word64.
    let coin_start = 29 + 32;
    if coin_start >= data.len() {
        return Err(SerializationError::CborDecode(
            "mempack_txout tag 2/3: truncated before CompactCoin".into(),
        ));
    }
    let inner_tag = data[coin_start];
    if inner_tag != 0 {
        return Err(SerializationError::CborDecode(format!(
            "mempack_txout tag 2/3: unexpected CompactCoin inner tag {inner_tag}"
        )));
    }
    let (coin, coin_varlen_bytes) = decode_varlen(&data[coin_start + 1..])?;
    let consumed = coin_start + 1 + coin_varlen_bytes;

    Ok(Addr28Decoded {
        address,
        coin,
        consumed,
    })
}

/// Tag 2: `TxOut_AddrHash28_AdaOnly` — Credential Staking + Addr28Extra + Coin.
///
/// Yields a fully-decoded 57-byte Shelley base address and the exact lovelace
/// amount (byte-for-byte compatible with what a tag-0 decode would produce for
/// the same logical UTxO).
fn decode_tag2(data: &[u8]) -> Result<(MemPackTxOut, usize), SerializationError> {
    // Skip outer tag byte.
    let decoded = decode_addr28_payload(&data[1..])?;
    let consumed = 1 + decoded.consumed;
    Ok((
        MemPackTxOut {
            tag: 2,
            address: decoded.address,
            coin: decoded.coin,
            multi_asset: None,
            num_assets: 0,
            datum_hash: None,
            datum: None,
            script_ref: None,
            opaque_tail: None,
        },
        consumed,
    ))
}

/// Tag 3: `TxOut_AddrHash28_AdaOnly_DataHash32` — tag 2 plus a trailing
/// `DataHash32` (32 raw bytes interpreted as 4 × Word64 little-endian, then
/// re-serialized big-endian to recover the original datum hash).
fn decode_tag3(data: &[u8]) -> Result<(MemPackTxOut, usize), SerializationError> {
    let decoded = decode_addr28_payload(&data[1..])?;
    let after_coin = 1 + decoded.consumed;

    if data.len() < after_coin + 32 {
        return Err(SerializationError::CborDecode(
            "mempack_txout tag 3: truncated DataHash32".into(),
        ));
    }
    let dh_slice = &data[after_coin..after_coin + 32];
    let dw0 = u64::from_le_bytes(dh_slice[0..8].try_into().unwrap());
    let dw1 = u64::from_le_bytes(dh_slice[8..16].try_into().unwrap());
    let dw2 = u64::from_le_bytes(dh_slice[16..24].try_into().unwrap());
    let dw3 = u64::from_le_bytes(dh_slice[24..32].try_into().unwrap());
    let mut datum_hash = [0u8; 32];
    datum_hash[0..8].copy_from_slice(&dw0.to_be_bytes());
    datum_hash[8..16].copy_from_slice(&dw1.to_be_bytes());
    datum_hash[16..24].copy_from_slice(&dw2.to_be_bytes());
    datum_hash[24..32].copy_from_slice(&dw3.to_be_bytes());

    Ok((
        MemPackTxOut {
            tag: 3,
            address: decoded.address,
            coin: decoded.coin,
            multi_asset: None,
            num_assets: 0,
            datum_hash: Some(datum_hash),
            datum: None,
            script_ref: None,
            opaque_tail: None,
        },
        after_coin + 32,
    ))
}

/// Tag 4: `TxOutCompactDatum` — CompactAddr + CompactValue + inline datum.
///
/// Matches the byte-exact MemPack layout from cardano-ledger
/// (`eras/babbage/impl/src/Cardano/Ledger/Babbage/TxOut.hs`):
///
/// ```haskell
/// TxOutCompactDatum cAddr cValue datum ->
///   packTagM 4 >> packM cAddr >> packM cValue >> packM datum
/// ```
///
/// Unlike tag 5's `Datum era` *option*, the tag-4 `datum` field is a
/// `BinaryData era` **directly** (an inline datum is always present), i.e. a
/// newtype over `ShortByteString` whose MemPack form is
/// `VarLen(len) ‖ raw_cbor_datum`. The `VarLen` length prefix MUST be stripped
/// so `datum` carries the bare on-chain Plutus `Data` CBOR — the same bytes a
/// tag-24 inline datum yields during normal block decode. (The previous decoder
/// stored the length-prefixed bytes for ADA-only and dropped the datum entirely
/// for multi-asset; both produced a wrong inline datum on import — gap A of #10.)
///
/// Both the ADA-only and multi-asset CompactValue cases are parsed exactly (via
/// [`decode_compact_value_exact`]) so the `BinaryData` datum tail is recovered
/// from the correct offset.
fn decode_tag4(data: &[u8]) -> Result<(MemPackTxOut, usize), SerializationError> {
    let mut off = 1;

    let (address, addr_consumed) = decode_compact_addr(&data[off..])?;
    off += addr_consumed;

    if off >= data.len() {
        return Err(SerializationError::CborDecode(
            "mempack_txout tag 4: no value data".into(),
        ));
    }

    // CompactValue (ADA-only or multi-asset), parsed exactly so we land on the
    // BinaryData length prefix.
    let val = decode_compact_value_exact(&data[off..])?;
    off += val.consumed;

    // BinaryData : ShortByteString = VarLen(len) ‖ raw_cbor.
    let (datum, datum_consumed) = decode_binary_data(&data[off..])?;
    off += datum_consumed;

    Ok((
        MemPackTxOut {
            tag: 4,
            address,
            coin: val.coin,
            multi_asset: val.multi_asset_raw,
            num_assets: val.num_assets,
            datum_hash: None,
            datum: Some(datum),
            script_ref: None,
            opaque_tail: None,
        },
        off,
    ))
}

/// Decode a MemPack `BinaryData era` field: a newtype over `ShortByteString`
/// serialized as `VarLen(len) ‖ raw_bytes`. Returns the bare bytes (the original
/// on-chain Plutus `Data` CBOR) and the total bytes consumed (prefix + body).
///
/// `deriving newtype MemPack` on `BinaryData` (see
/// `libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Data.hs`) means it
/// inherits the `MemPack ShortByteString` instance.
fn decode_binary_data(data: &[u8]) -> Result<(Vec<u8>, usize), SerializationError> {
    let (len, len_bytes) = decode_varlen(data)?;
    let len = len as usize;
    let start = len_bytes;
    let end = start.checked_add(len).ok_or_else(|| {
        SerializationError::CborDecode("mempack_txout: BinaryData length overflow".into())
    })?;
    if end > data.len() {
        return Err(SerializationError::CborDecode(format!(
            "mempack_txout: BinaryData needs {end} bytes, have {}",
            data.len()
        )));
    }
    Ok((data[start..end].to_vec(), end))
}

/// Tag 5: `TxOutCompactRefScript` — CompactAddr + CompactValue + Datum + Script.
///
/// Matches the byte-exact MemPack layout from cardano-ledger
/// (`eras/babbage/impl/src/Cardano/Ledger/Babbage/TxOut.hs`):
///
/// ```haskell
/// TxOutCompactRefScript cAddr cValue datum script ->
///   packTagM 5 >> packM cAddr >> packM cValue >> packM datum >> packM script
/// ```
///
/// where:
/// * `datum :: Datum era`  — option, see [`decode_datum_option`]:
///   `packTagM 0` (NoDatum) / `packTagM 1 >> packM dataHash` (DatumHash) /
///   `packTagM 2 >> packM binaryData` (inline Datum).
/// * `script :: Script era` — `AlonzoScript`, see [`decode_mempack_script`].
///
/// Both the ADA-only and multi-asset CompactValue cases are now parsed exactly
/// (via [`decode_compact_value_exact`]) so the Datum + Script tail is recovered
/// rather than dumped into `opaque_tail`.
fn decode_tag5(data: &[u8]) -> Result<(MemPackTxOut, usize), SerializationError> {
    let mut off = 1;

    let (address, addr_consumed) = decode_compact_addr(&data[off..])?;
    off += addr_consumed;

    if off >= data.len() {
        return Err(SerializationError::CborDecode(
            "mempack_txout tag 5: no value data".into(),
        ));
    }

    // CompactValue (ADA-only or multi-asset) — exact length so we land on the
    // Datum option byte.
    let val = decode_compact_value_exact(&data[off..])?;
    off += val.consumed;

    // Datum option.
    let datum_opt = decode_datum_option(&data[off..])?;
    off += datum_opt.consumed;

    // Reference script (raw MemPack AlonzoScript blob). We validate that it
    // decodes to a known shape but store the raw bytes for the caller to
    // reconstruct a typed ScriptRef.
    let (script_blob, script_consumed) = decode_mempack_script(&data[off..])?;
    off += script_consumed;

    Ok((
        MemPackTxOut {
            tag: 5,
            address,
            coin: val.coin,
            multi_asset: val.multi_asset_raw,
            num_assets: val.num_assets,
            datum_hash: datum_opt.datum_hash,
            datum: datum_opt.datum,
            script_ref: Some(script_blob),
            opaque_tail: None,
        },
        off,
    ))
}

/// A decoded MemPack `Datum era` option.
struct DatumOption {
    /// 32-byte datum hash (the `DatumHash` branch).
    datum_hash: Option<[u8; 32]>,
    /// Inline datum CBOR bytes (the `Datum`/`BinaryData` branch).
    datum: Option<Vec<u8>>,
    /// Bytes consumed by the option.
    consumed: usize,
}

/// Decode a MemPack `Datum era` option.
///
/// Matches `instance Era era => MemPack (Datum era)` from cardano-ledger
/// (`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Data.hs`):
///
/// ```haskell
/// packM = \case
///   NoDatum            -> packTagM 0
///   DatumHash dataHash -> packTagM 1 >> packM dataHash
///   Datum binaryData   -> packTagM 2 >> packM binaryData
/// ```
///
/// * `DataHash` is `Hash Blake2b_256 ... = PackedBytes32`, packed via
///   `writeWord64BE` (`Cardano.Crypto.PackedBytes.Internal`). That byteswaps to
///   produce **big-endian on-disk bytes directly**, so the 32 stored bytes ARE
///   the datum hash, contiguous and untransformed. (This differs from the
///   `DataHash32`/`Addr28Extra` ledger types used by tags 2/3, which pack four
///   plain native-endian `Word64`s and therefore require the LE→BE slot
///   recovery in [`decode_tag3`]/[`decode_addr28_payload`].)
/// * `BinaryData` is a newtype over `ShortByteString`, so it is serialized as
///   `VarLen(len) ‖ raw_cbor_datum_bytes`.
fn decode_datum_option(data: &[u8]) -> Result<DatumOption, SerializationError> {
    if data.is_empty() {
        return Err(SerializationError::CborDecode(
            "mempack_txout tag 5: truncated Datum option".into(),
        ));
    }
    match data[0] {
        0 => Ok(DatumOption {
            datum_hash: None,
            datum: None,
            consumed: 1,
        }),
        1 => {
            if data.len() < 1 + 32 {
                return Err(SerializationError::CborDecode(
                    "mempack_txout tag 5: truncated DatumHash".into(),
                ));
            }
            let mut dh = [0u8; 32];
            dh.copy_from_slice(&data[1..1 + 32]);
            Ok(DatumOption {
                datum_hash: Some(dh),
                datum: None,
                consumed: 1 + 32,
            })
        }
        2 => {
            // BinaryData : ShortByteString = VarLen(len) ‖ bytes.
            let (len, len_bytes) = decode_varlen(&data[1..])?;
            let len = len as usize;
            let start = 1 + len_bytes;
            let end = start.checked_add(len).ok_or_else(|| {
                SerializationError::CborDecode("mempack_txout tag 5: datum length overflow".into())
            })?;
            if end > data.len() {
                return Err(SerializationError::CborDecode(
                    "mempack_txout tag 5: truncated inline Datum".into(),
                ));
            }
            Ok(DatumOption {
                datum_hash: None,
                datum: Some(data[start..end].to_vec()),
                consumed: end,
            })
        }
        other => Err(SerializationError::CborDecode(format!(
            "mempack_txout tag 5: unknown Datum option tag {other}"
        ))),
    }
}

/// Decode a MemPack `Script era` (= `AlonzoScript`) blob, returning the raw
/// blob bytes (tag + body) and the number of bytes consumed.
///
/// Matches `instance ... => MemPack (AlonzoScript era)` from cardano-ledger
/// (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Scripts.hs`):
///
/// ```haskell
/// packM = \case
///   NativeScript script -> packTagM 0 >> packM script
///   PlutusScript script -> packTagM 1 >> packM script
/// ```
///
/// * NativeScript (`Timelock`) is a `MemoBytes`, packed via `packMemoBytesM` as
///   its memoized CBOR `ShortByteString` = `VarLen(len) ‖ raw_cbor`.
/// * PlutusScript is era-relative (`PlutusScript era`): a further `packTagM`
///   selects the language (Babbage: 0=V1, 1=V2; Conway: 0=V1, 1=V2, 2=V3) and
///   the body is the `Plutus l` newtype over `ShortByteString` =
///   `VarLen(len) ‖ flat_program`.
///
/// We do not interpret the language tag here (it is era-relative); we only need
/// the total byte length to delimit the blob. The full blob (outer tag onward)
/// is returned so the caller can reconstruct a typed `ScriptRef`.
fn decode_mempack_script(data: &[u8]) -> Result<(Vec<u8>, usize), SerializationError> {
    if data.is_empty() {
        return Err(SerializationError::CborDecode(
            "mempack_txout tag 5: truncated Script".into(),
        ));
    }
    let outer_tag = data[0];
    let body_off = match outer_tag {
        0 => 1, // NativeScript: body is the MemoBytes ShortByteString.
        1 => {
            // PlutusScript: one era-relative language tag byte, then ShortByteString.
            if data.len() < 2 {
                return Err(SerializationError::CborDecode(
                    "mempack_txout tag 5: truncated PlutusScript language tag".into(),
                ));
            }
            2
        }
        other => {
            return Err(SerializationError::CborDecode(format!(
                "mempack_txout tag 5: unknown AlonzoScript tag {other}"
            )));
        }
    };

    // ShortByteString body: VarLen(len) ‖ bytes.
    let (len, len_bytes) = decode_varlen(&data[body_off..])?;
    let len = len as usize;
    let body_start = body_off + len_bytes;
    let total = body_start.checked_add(len).ok_or_else(|| {
        SerializationError::CborDecode("mempack_txout tag 5: script length overflow".into())
    })?;
    if total > data.len() {
        return Err(SerializationError::CborDecode(format!(
            "mempack_txout tag 5: script needs {total} bytes, have {}",
            data.len()
        )));
    }
    Ok((data[..total].to_vec(), total))
}

/// Classified view of a MemPack `AlonzoScript` reference-script blob.
///
/// Produced by [`parse_script_ref_kind`]. The language tag carried by Plutus
/// scripts is **era-relative** (Babbage: 0=V1, 1=V2; Conway: 0=V1, 1=V2, 2=V3),
/// so the caller must map it to a global script language using the snapshot era.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptRefKind {
    /// Native (timelock) script — body is the raw CBOR of the native script.
    Native(Vec<u8>),
    /// Plutus script — `(era_relative_language_tag, flat_program_bytes)`.
    Plutus { lang_tag: u8, body: Vec<u8> },
}

/// Classify a raw MemPack `AlonzoScript` blob (as stored in
/// [`MemPackTxOut::script_ref`]) into a [`ScriptRefKind`].
///
/// See [`decode_mempack_script`] for the byte layout this re-parses.
pub fn parse_script_ref_kind(blob: &[u8]) -> Result<ScriptRefKind, SerializationError> {
    if blob.is_empty() {
        return Err(SerializationError::CborDecode(
            "script_ref blob: empty".into(),
        ));
    }
    match blob[0] {
        0 => {
            // Native: VarLen(len) ‖ cbor.
            let (len, len_bytes) = decode_varlen(&blob[1..])?;
            let start = 1 + len_bytes;
            let end = start + len as usize;
            if end > blob.len() {
                return Err(SerializationError::CborDecode(
                    "script_ref blob: truncated native body".into(),
                ));
            }
            Ok(ScriptRefKind::Native(blob[start..end].to_vec()))
        }
        1 => {
            if blob.len() < 2 {
                return Err(SerializationError::CborDecode(
                    "script_ref blob: truncated plutus language tag".into(),
                ));
            }
            let lang_tag = blob[1];
            let (len, len_bytes) = decode_varlen(&blob[2..])?;
            let start = 2 + len_bytes;
            let end = start + len as usize;
            if end > blob.len() {
                return Err(SerializationError::CborDecode(
                    "script_ref blob: truncated plutus body".into(),
                ));
            }
            Ok(ScriptRefKind::Plutus {
                lang_tag,
                body: blob[start..end].to_vec(),
            })
        }
        other => Err(SerializationError::CborDecode(format!(
            "script_ref blob: unknown AlonzoScript tag {other}"
        ))),
    }
}
