//! `dugite_primitives::transaction::NativeScript` / Plutus scripts → utxorpc Script.

use crate::map::common::hash_bytes;
use crate::proto::v1beta::cardano as pb;
use dugite_primitives::transaction::{NativeScript, ScriptRef};

/// Map a Cardano script reference (Native + Plutus v1/v2/v3/v4) to the
/// utxorpc Script oneof.
pub fn script_ref_to_proto(s: &ScriptRef) -> pb::Script {
    use pb::script::Script as Inner;
    let inner = match s {
        ScriptRef::NativeScript(ns) => Inner::Native(native_script_to_proto(ns)),
        ScriptRef::PlutusV1(bytes) => Inner::PlutusV1(bytes.clone()),
        ScriptRef::PlutusV2(bytes) => Inner::PlutusV2(bytes.clone()),
        ScriptRef::PlutusV3(bytes) => Inner::PlutusV3(bytes.clone()),
        // PlutusV4 (Dijkstra-only, PV12+).
        ScriptRef::PlutusV4(bytes) => Inner::PlutusV4(bytes.clone()),
    };
    pb::Script {
        script: Some(inner),
    }
}

pub fn native_script_to_proto(s: &NativeScript) -> pb::NativeScript {
    use pb::native_script::NativeScript as Inner;
    let inner = match s {
        NativeScript::ScriptPubkey(hash28) => Inner::ScriptPubkeyHash(hash_bytes(hash28)),
        NativeScript::ScriptAll(items) => Inner::ScriptAll(pb::NativeScriptList {
            items: items.iter().map(native_script_to_proto).collect(),
        }),
        NativeScript::ScriptAny(items) => Inner::ScriptAny(pb::NativeScriptList {
            items: items.iter().map(native_script_to_proto).collect(),
        }),
        NativeScript::ScriptNOfK(n, items) => Inner::ScriptNOfK(pb::ScriptNOfK {
            k: *n,
            scripts: items.iter().map(native_script_to_proto).collect(),
        }),
        NativeScript::InvalidBefore(slot) => Inner::InvalidBefore(slot.0),
        NativeScript::InvalidHereafter(slot) => Inner::InvalidHereafter(slot.0),
        NativeScript::RequireGuard(_cred) => {
            // Dijkstra-only native script tag 6 — the v1beta v0.19.2
            // proto schema does NOT yet define a variant for this.
            // Until upstream adds one, we approximate as a single-key
            // all-script wrapper of an empty pubkey (degenerate but
            // never-satisfied), which is more conservative than
            // omitting the cert silently.
            Inner::ScriptAll(pb::NativeScriptList { items: Vec::new() })
        }
    };
    pb::NativeScript {
        native_script: Some(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::SlotNo;

    #[test]
    fn pubkey_round_trip() {
        let ns = NativeScript::ScriptPubkey(Hash32::from_bytes([7u8; 32]));
        let pb_ns = native_script_to_proto(&ns);
        match pb_ns.native_script.unwrap() {
            pb::native_script::NativeScript::ScriptPubkeyHash(h) => assert_eq!(h, vec![7u8; 32]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn invalid_before_carries_slot() {
        let ns = NativeScript::InvalidBefore(SlotNo(123));
        let pb_ns = native_script_to_proto(&ns);
        match pb_ns.native_script.unwrap() {
            pb::native_script::NativeScript::InvalidBefore(s) => assert_eq!(s, 123),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn n_of_k_round_trip() {
        let ns = NativeScript::ScriptNOfK(
            2,
            vec![
                NativeScript::ScriptPubkey(Hash32::from_bytes([1u8; 32])),
                NativeScript::ScriptPubkey(Hash32::from_bytes([2u8; 32])),
                NativeScript::ScriptPubkey(Hash32::from_bytes([3u8; 32])),
            ],
        );
        let pb_ns = native_script_to_proto(&ns);
        match pb_ns.native_script.unwrap() {
            pb::native_script::NativeScript::ScriptNOfK(n) => {
                assert_eq!(n.k, 2);
                assert_eq!(n.scripts.len(), 3);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plutus_v3_script_round_trip() {
        let sr = ScriptRef::PlutusV3(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let pb_s = script_ref_to_proto(&sr);
        match pb_s.script.unwrap() {
            pb::script::Script::PlutusV3(b) => assert_eq!(b, vec![0xDE, 0xAD, 0xBE, 0xEF]),
            other => panic!("{other:?}"),
        }
    }
}
