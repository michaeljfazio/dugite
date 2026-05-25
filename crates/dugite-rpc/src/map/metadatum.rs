//! Auxiliary metadata → utxorpc `AuxData.metadata` / `Metadatum`.
//!
//! Cardano transaction metadata is a `Map<u64, Metadatum>` where each
//! `Metadatum` is one of: int, bytes, text, array, map.

use crate::proto::v1beta::cardano as pb;
use dugite_primitives::transaction::{AuxiliaryData, TransactionMetadatum as Metadatum};

/// Map a `Vec<(label, Metadatum)>` (the order-preserving form used by
/// AuxiliaryData) into the proto's `repeated Metadata`.
pub fn aux_data_to_proto(aux: &AuxiliaryData) -> pb::AuxData {
    pb::AuxData {
        metadata: aux
            .metadata
            .iter()
            .map(|(label, datum)| pb::Metadata {
                label: *label,
                value: Some(metadatum_to_proto(datum)),
            })
            .collect(),
        scripts: aux
            .native_scripts
            .iter()
            .map(|ns| crate::map::script::native_script_to_proto(ns))
            .map(|ns| pb::Script {
                script: Some(pb::script::Script::Native(ns)),
            })
            .chain(aux.plutus_v1_scripts.iter().map(|b| pb::Script {
                script: Some(pb::script::Script::PlutusV1(b.clone())),
            }))
            .chain(aux.plutus_v2_scripts.iter().map(|b| pb::Script {
                script: Some(pb::script::Script::PlutusV2(b.clone())),
            }))
            .chain(aux.plutus_v3_scripts.iter().map(|b| pb::Script {
                script: Some(pb::script::Script::PlutusV3(b.clone())),
            }))
            .collect(),
    }
}

pub fn metadatum_to_proto(m: &Metadatum) -> pb::Metadatum {
    use pb::metadatum::Metadatum as Inner;
    let inner = match m {
        // dugite stores Int as i128 (CBOR allows >i64); proto Int is
        // i64. Saturate at extremes — on-chain metadata almost never
        // overflows i64 and clients can verify via native_bytes.
        Metadatum::Int(v) => {
            let clamped = if *v > i64::MAX as i128 {
                i64::MAX
            } else if *v < i64::MIN as i128 {
                i64::MIN
            } else {
                *v as i64
            };
            Inner::Int(clamped)
        }
        Metadatum::Bytes(b) => Inner::Bytes(b.clone()),
        Metadatum::Text(s) => Inner::Text(s.clone()),
        Metadatum::List(items) => Inner::Array(pb::MetadatumArray {
            items: items.iter().map(metadatum_to_proto).collect(),
        }),
        Metadatum::Map(pairs) => Inner::Map(pb::MetadatumMap {
            pairs: pairs
                .iter()
                .map(|(k, v)| pb::MetadatumPair {
                    key: Some(metadatum_to_proto(k)),
                    value: Some(metadatum_to_proto(v)),
                })
                .collect(),
        }),
    };
    pb::Metadatum {
        metadatum: Some(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_metadatum_round_trip() {
        let m = Metadatum::Int(-42);
        let pb_m = metadatum_to_proto(&m);
        match pb_m.metadatum.unwrap() {
            pb::metadatum::Metadatum::Int(v) => assert_eq!(v, -42),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn text_metadatum_round_trip() {
        let m = Metadatum::Text("hello".into());
        let pb_m = metadatum_to_proto(&m);
        match pb_m.metadatum.unwrap() {
            pb::metadatum::Metadatum::Text(s) => assert_eq!(s, "hello"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nested_list_metadatum() {
        let m = Metadatum::List(vec![Metadatum::Int(1), Metadatum::Int(2)]);
        let pb_m = metadatum_to_proto(&m);
        match pb_m.metadatum.unwrap() {
            pb::metadatum::Metadatum::Array(a) => assert_eq!(a.items.len(), 2),
            other => panic!("{other:?}"),
        }
    }
}
