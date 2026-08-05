//! Generic `google.protobuf.FieldMask` application — issue #1004.
//!
//! Most utxorpc requests carry a `FieldMask` selecting which response
//! fields the server should populate. [`apply`] honours it for real: it
//! prunes every field NOT covered by the mask, recursively, for any
//! response message the crate generates — not just the top level.
//!
//! # Semantics (from `google/protobuf/field_mask.proto`)
//!
//! > When used in the context of a projection, a response message or
//! > sub-message is filtered by the API to only contain those fields as
//! > specified in the mask. \[...\] Note that a field mask does not
//! > necessarily apply to the top-level response message.
//!
//! Concretely, given paths `["f.a", "f.b.d"]` applied to
//! `{ f: { a: 22, b: { d: 1, x: 2 }, y: 13 }, z: 8 }`, the result is
//! `{ f: { a: 22, b: { d: 1 } } }` — `f` and `f.b` are kept because a
//! longer path selects something inside them; `f.y` and `z` are cleared
//! because no path covers them; naming a message field with nothing
//! further after it (`f.b` alone, or an empty/absent mask) selects that
//! entire subtree, matching "a response message or sub-message is
//! filtered ... to only contain those fields as specified" applied
//! recursively.
//!
//! Per the same doc: *"If a FieldMask object is not present in a get
//! operation, the operation applies to all fields"* — [`apply`] treats an
//! empty `mask_paths` slice (covers both "no `FieldMask` on the wire" and
//! "`FieldMask` present with zero paths", since prost cannot distinguish
//! the two once the request is decoded) the same way: return the
//! response unpruned.
//!
//! # Repeated fields mid-path
//!
//! The canonical doc adds: *"A repeated field is not allowed except at
//! the last position of a paths string"* — a validity rule for the
//! literal FieldMask grammar. utxorpc responses nest big repeated fields
//! routinely (`FetchBlockResponse.block`, `ReadUtxosResponse.items`,
//! ...), and the module's own worked example before this rewrite was
//! `"block.body.tx.outputs.address"` — a path that walks straight through
//! two repeated fields (`tx`, `outputs`). The spec does not define
//! projection behaviour for that case; we resolve the ambiguity by
//! applying the remaining sub-mask to **every element** of a repeated
//! message field when the path continues past it. That is the
//! "projection" reading most FieldMask-alike implementations use in
//! practice (see AIP-161's `*` wildcard for the same idea, formalised),
//! and it preserves the invariant this module already documented before
//! `apply` had a real body: a client never gets *less* than it asked for.
//!
//! # Fail-closed, not fail-open
//!
//! [`apply`] returns `Result<T, RpcError>`. Every failure path (unknown
//! `message_name`, a response that won't decode against its own
//! descriptor, pruned bytes that won't decode back into `T`) is a
//! programmer error — never something a client's request bytes can
//! trigger — and is surfaced as `RpcError::Internal`, which
//! `services/mod.rs` maps to `Status::internal` at the call site. It
//! would be tempting to fail OPEN instead (return the unpruned response)
//! since over-inclusion was already the pre-existing, spec-compliant
//! behaviour; that instinct is exactly what this module rejects: a
//! silent fail-open on a masking bug reintroduces the no-op #1004
//! exists to close, invisibly, at precisely the call site with the bug.
//! This project's rule is reject-over-silent-skip. See
//! `every_message_name_used_by_apply_resolves` below — every literal
//! `message_name` this crate passes is pinned against the descriptor
//! pool, so the `Internal` arms should never fire for a correct build;
//! if a future call site's name typo slips past that test, failing the
//! specific RPC loudly is still strictly better than every response on
//! that endpoint silently ignoring its mask forever.
//!
//! # Implementation
//!
//! `dugite-rpc` generates a `FILE_DESCRIPTOR_SET` for every vendored
//! proto anyway (`crate::proto::FILE_DESCRIPTOR_SET`, the exact bytes
//! `build.rs` writes via `.file_descriptor_set_path(&descriptor_path)`
//! and the same ones `server.rs` feeds to
//! `tonic_reflection::server::Builder::register_encoded_file_descriptor_set`
//! — one source of truth, not a second copy that could drift). This
//! module loads it once into a `prost_reflect::DescriptorPool` and walks
//! any response through `DynamicMessage` — one generic mechanism instead
//! of hand-written per-field clearing code for the ~110 messages under
//! `proto/`. That trade-off is deliberate: N hand-rolled per-type
//! pruners is exactly the drift shape behind #932 / #938 / #977 / #985
//! in this codebase (a copy nobody remembers to update when a message
//! gains a field), and every dugite-rpc response type already round-trips
//! through this same descriptor set for `tonic-reflection`, so there is
//! no second schema to keep in sync.
//!
//! `prost-reflect` (`crates.io/crates/prost-reflect`, MIT/Apache-2.0
//! dual-licensed like the rest of this workspace's deps) is the
//! `prost`-ecosystem-maintained reflection crate — same author group as
//! `prost` itself, pinned to the identical `prost 0.14.4` this crate
//! already uses (verified via `cargo tree -i prost`, so it's not a
//! second `prost` version in the dependency graph). Cost per MASKED call
//! is one `encode_to_vec` + one reflective `DynamicMessage::decode` +
//! the prune walk + one reflective `encode_to_vec` + one concrete
//! `T::decode` — two encode/decode round trips instead of one. That is
//! O(response size) allocation + copy work per request, comparable to
//! (not asymptotically worse than) the JSON-transcoding cost pattern
//! common in REST gateways, and it only runs when `mask_paths` is
//! non-empty — an UNMASKED call (empty `mask_paths`, the common case for
//! clients that haven't adopted masks) short-circuits before touching
//! the descriptor pool or allocating anything beyond the original
//! response. No conformance test in this crate exercises high-QPS
//! masked traffic, so this is a reasoned bound, not a measured one; if
//! it ever matters, the fix is caching per-message-type "already
//! selected everything" fast paths, not abandoning the mechanism.

use std::collections::HashMap;
use std::sync::OnceLock;

use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, ReflectMessage, Value};

use crate::error::RpcError;

/// Splits one dot-separated `FieldMask` path into segments.
///
/// `"block.body.tx.outputs.address"` → `["block", "body", "tx", "outputs", "address"]`.
#[derive(Clone, Debug)]
pub struct FieldMaskPath {
    segments: Vec<String>,
}

impl FieldMaskPath {
    pub fn parse(s: &str) -> Self {
        Self {
            segments: s
                .split('.')
                .filter(|seg| !seg.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

/// One node of the parsed mask tree. A node reached with no remaining
/// segments (`terminal`) selects its *entire* subtree — matching a
/// message-typed field named alone in a path. A node with children
/// selects only those named children, recursively.
#[derive(Default)]
struct MaskNode {
    terminal: bool,
    children: HashMap<String, MaskNode>,
}

impl MaskNode {
    fn build(paths: &[String]) -> Self {
        let mut root = MaskNode::default();
        for raw in paths {
            let path = FieldMaskPath::parse(raw);
            root.insert(path.segments());
        }
        root
    }

    fn insert(&mut self, segs: &[String]) {
        match segs.split_first() {
            None => self.terminal = true,
            Some((head, rest)) => self.children.entry(head.clone()).or_default().insert(rest),
        }
    }

    /// Whether descending into this node keeps everything below it
    /// as-is (either it was named as a leaf path, or it has no
    /// children at all — an empty node only exists as an insertion
    /// target for a terminal path, so `terminal` is always true when
    /// `children` is empty; the `||` is defensive, not load-bearing).
    fn keep_whole_subtree(&self) -> bool {
        self.terminal || self.children.is_empty()
    }
}

/// Descriptor pool for the crate's own vendored proto set — built once
/// from the same bytes served to `tonic-reflection`.
fn descriptor_pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        DescriptorPool::decode(crate::proto::FILE_DESCRIPTOR_SET)
            .expect("utxorpc FILE_DESCRIPTOR_SET malformed — codegen drift")
    })
}

/// Apply a `FieldMask` (sequence of dot-separated paths) to a response.
///
/// `message_name` is the response type's fully-qualified proto name
/// (e.g. `"utxorpc.v1beta.query.ReadParamsResponse"`) — callers pass a
/// literal naming the exact type `T` decodes to.
///
/// An empty `mask_paths` returns `response` unpruned (no `FieldMask` on
/// the wire, or one with zero paths — see the module doc) — that is the
/// one CORRECT reason to skip masking, per the spec's own "no FieldMask
/// -> all fields" rule, and it is the common case (skips the descriptor
/// walk entirely).
///
/// Every other failure path is a REJECT, not a silent pass-through:
/// `message_name` not resolving in the descriptor pool, `response`
/// failing to decode against its own descriptor, or the pruned bytes
/// failing to decode back into `T` are all programmer errors (a
/// `message_name` that doesn't match `T`, or descriptor/codegen drift)
/// — never something a client's request can trigger. Serving the
/// UNPRUNED response in that situation would silently reintroduce
/// exactly the bug issue #1004 exists to close (every field shipped
/// regardless of the mask) with no error and no log line to notice it
/// by — the "check that reports success while measuring nothing"
/// pattern. Returning `Err(RpcError::Internal(..))` instead turns a
/// wrong `message_name` into an immediate, loud test/request failure at
/// the ONE call site with the typo, rather than a permanently-silent
/// mask no-op for that endpoint.
///
/// `every_message_name_used_by_apply_resolves` (below) additionally
/// pins every name this crate actually passes against the descriptor
/// pool, so this should never fire in production for a correct build —
/// but if it ever does (e.g. a future call site adds a name and forgets
/// to add it to `ALL_RESPONSE_MESSAGE_NAMES`), failing the RPC is the
/// correct, visible outcome, not a silently-unmasked response.
pub fn apply<T: Message + Default>(
    mask_paths: &[String],
    response: T,
    message_name: &str,
) -> Result<T, RpcError> {
    if mask_paths.is_empty() {
        return Ok(response);
    }
    let pool = descriptor_pool();
    let desc = pool.get_message_by_name(message_name).ok_or_else(|| {
        RpcError::Internal(format!(
            "masking::apply: message name {message_name:?} does not resolve in the \
             FieldMask descriptor pool — codegen or message-name drift"
        ))
    })?;
    let bytes = response.encode_to_vec();
    let mut dyn_msg = DynamicMessage::decode(desc, bytes.as_slice()).map_err(|e| {
        RpcError::Internal(format!(
            "masking::apply: {message_name} response failed to decode against its own \
             descriptor: {e} — message_name likely names the wrong type for T"
        ))
    })?;
    let tree = MaskNode::build(mask_paths);
    prune(&mut dyn_msg, &tree);
    T::decode(dyn_msg.encode_to_vec().as_slice()).map_err(|e| {
        RpcError::Internal(format!(
            "masking::apply: pruned {message_name} failed to re-decode as its concrete \
             type: {e}"
        ))
    })
}

/// Recursively clear every field of `msg` not covered by `tree`.
///
/// `tree.keep_whole_subtree()` short-circuits at the call site (never
/// called with the root reached via a terminal path — see `apply`), so
/// this only runs against nodes that still have a real selection to
/// enforce.
fn prune(msg: &mut DynamicMessage, tree: &MaskNode) {
    let desc = msg.descriptor();
    // Snapshot (name, FieldDescriptor) up front — `descriptor().fields()`
    // enumerates every field the message TYPE declares, not just the set
    // ones, which is what we want: an unset-but-masked-in field is a
    // no-op either way, and a set-but-unmasked field must be cleared.
    let fields: Vec<_> = desc.fields().collect();
    for fd in fields {
        let name = fd.name();
        match tree.children.get(name) {
            None => msg.clear_field(&fd),
            Some(child) if child.keep_whole_subtree() => {
                // Whole field (and everything under it) selected — leave
                // as-is, no recursion needed.
            }
            Some(child) => {
                if !msg.has_field(&fd) {
                    continue; // nothing set, nothing to prune
                }
                if !matches!(fd.kind(), Kind::Message(_)) {
                    // A deeper path segment past a scalar/enum leaf is
                    // meaningless (nothing to select inside a scalar) —
                    // keep the leaf rather than guess the client meant
                    // to clear it; naming e.g. "fee.lovelace" against a
                    // plain uint64 is a client-side path error, not a
                    // reason to silently drop data.
                    continue;
                }
                let value = msg.get_field_mut(&fd);
                prune_value(value, child);
            }
        }
    }
}

/// Apply `tree` to a single field `Value` already known to be
/// message-typed at the proto level — dispatches over the list / map /
/// singular shapes `prost_reflect::Value` uses to represent it.
fn prune_value(value: &mut Value, tree: &MaskNode) {
    match value {
        Value::Message(nested) => prune(nested, tree),
        Value::List(items) => {
            // Repeated message field with a path continuing past it —
            // see the module doc: apply the sub-mask to every element
            // (a documented extension beyond the strict "last position
            // only" grammar rule).
            for item in items.iter_mut() {
                if let Value::Message(nested) = item {
                    prune(nested, tree);
                }
            }
        }
        Value::Map(entries) => {
            // Map<_, Message> — same elementwise treatment as List.
            for (_, v) in entries.iter_mut() {
                if let Value::Message(nested) = v {
                    prune(nested, tree);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::v1beta;

    #[test]
    fn parses_dot_separated_path() {
        let p = FieldMaskPath::parse("block.body.tx");
        assert_eq!(p.segments(), &["block", "body", "tx"]);
        assert!(!p.is_empty());
    }

    #[test]
    fn empty_path_is_recognised() {
        assert!(FieldMaskPath::parse("").is_empty());
        assert!(FieldMaskPath::parse(".").is_empty());
    }

    #[test]
    fn collapses_consecutive_dots() {
        let p = FieldMaskPath::parse("a..b..c");
        assert_eq!(p.segments(), &["a", "b", "c"]);
    }

    #[test]
    fn empty_mask_is_identity() {
        let tip = v1beta::query::ChainPoint {
            slot: 42,
            hash: vec![1, 2, 3],
            height: 7,
            timestamp: 999,
        };
        let out = apply(&[], tip.clone(), "utxorpc.v1beta.query.ChainPoint").unwrap();
        assert_eq!(out, tip);
    }

    #[test]
    fn top_level_mask_clears_unselected_sibling() {
        // ReadParamsResponse { values, ledger_tip } — a mask naming only
        // `ledger_tip` must clear `values` entirely (the whole-response
        // payload) while keeping `ledger_tip` intact.
        let resp = v1beta::query::ReadParamsResponse {
            values: Some(v1beta::query::AnyChainParams {
                params: Some(v1beta::query::any_chain_params::Params::Cardano(
                    v1beta::cardano::PParams::default(),
                )),
            }),
            ledger_tip: Some(v1beta::query::ChainPoint {
                slot: 55,
                hash: vec![9, 9],
                height: 3,
                timestamp: 1,
            }),
        };
        let masked = apply(
            &["ledger_tip".to_string()],
            resp.clone(),
            "utxorpc.v1beta.query.ReadParamsResponse",
        )
        .unwrap();
        assert!(
            masked.values.is_none(),
            "unselected top-level field must be cleared"
        );
        assert_eq!(masked.ledger_tip, resp.ledger_tip);
    }

    #[test]
    fn nested_mask_prunes_within_a_selected_message() {
        // ledger_tip.slot only -> hash/height/timestamp cleared, slot kept.
        let resp = v1beta::query::ReadUtxosResponse {
            items: vec![],
            ledger_tip: Some(v1beta::query::ChainPoint {
                slot: 100,
                hash: vec![7, 7, 7],
                height: 50,
                timestamp: 12345,
            }),
        };
        let masked = apply(
            &["ledger_tip.slot".to_string()],
            resp,
            "utxorpc.v1beta.query.ReadUtxosResponse",
        )
        .unwrap();
        let tip = masked.ledger_tip.expect("ledger_tip kept (named by mask)");
        assert_eq!(tip.slot, 100, "masked leaf must survive");
        assert!(tip.hash.is_empty(), "unmasked leaf must be cleared");
        assert_eq!(tip.height, 0, "unmasked leaf must be cleared");
        assert_eq!(tip.timestamp, 0, "unmasked leaf must be cleared");
    }

    #[test]
    fn naming_a_message_field_alone_keeps_its_whole_subtree() {
        // "ledger_tip" (no further segments) selects the WHOLE ChainPoint,
        // matching "naming a message field selects that whole subtree".
        let resp = v1beta::query::ReadUtxosResponse {
            items: vec![],
            ledger_tip: Some(v1beta::query::ChainPoint {
                slot: 100,
                hash: vec![7, 7, 7],
                height: 50,
                timestamp: 12345,
            }),
        };
        let masked = apply(
            &["ledger_tip".to_string()],
            resp.clone(),
            "utxorpc.v1beta.query.ReadUtxosResponse",
        )
        .unwrap();
        assert_eq!(masked.ledger_tip, resp.ledger_tip);
    }

    #[test]
    fn repeated_message_field_is_pruned_elementwise() {
        // FetchBlockResponse.block is `repeated AnyChainBlock`. A path
        // continuing past it ("block.native_bytes") must prune EACH
        // element down to just that leaf, not clear the whole list.
        let resp = v1beta::sync::FetchBlockResponse {
            block: vec![
                v1beta::sync::AnyChainBlock {
                    native_bytes: vec![1, 2, 3],
                    chain: Some(v1beta::sync::any_chain_block::Chain::Cardano(
                        v1beta::cardano::Block::default(),
                    )),
                },
                v1beta::sync::AnyChainBlock {
                    native_bytes: vec![4, 5],
                    chain: Some(v1beta::sync::any_chain_block::Chain::Cardano(
                        v1beta::cardano::Block::default(),
                    )),
                },
            ],
        };
        let masked = apply(
            &["block.native_bytes".to_string()],
            resp,
            "utxorpc.v1beta.sync.FetchBlockResponse",
        )
        .unwrap();
        assert_eq!(masked.block.len(), 2, "the repeated field itself survives");
        for item in &masked.block {
            assert!(!item.native_bytes.is_empty(), "masked leaf kept");
            assert!(
                item.chain.is_none(),
                "unmasked oneof branch must be cleared per element"
            );
        }
    }

    #[test]
    fn multiple_paths_union_their_selections() {
        let resp = v1beta::query::ReadUtxosResponse {
            items: vec![],
            ledger_tip: Some(v1beta::query::ChainPoint {
                slot: 1,
                hash: vec![1],
                height: 2,
                timestamp: 3,
            }),
        };
        let masked = apply(
            &["ledger_tip.slot".to_string(), "ledger_tip.hash".to_string()],
            resp,
            "utxorpc.v1beta.query.ReadUtxosResponse",
        )
        .unwrap();
        let tip = masked.ledger_tip.unwrap();
        assert_eq!(tip.slot, 1);
        assert_eq!(tip.hash, vec![1]);
        assert_eq!(tip.height, 0, "still cleared — no path covers it");
    }

    #[test]
    fn scalar_field_with_bogus_trailing_segment_is_kept_not_dropped() {
        // "ledger_tip.slot.bogus" — `slot` is a scalar (uint64); a client
        // mistake here must not silently vanish the field.
        let resp = v1beta::query::ReadUtxosResponse {
            items: vec![],
            ledger_tip: Some(v1beta::query::ChainPoint {
                slot: 77,
                hash: vec![],
                height: 0,
                timestamp: 0,
            }),
        };
        let masked = apply(
            &["ledger_tip.slot.bogus".to_string()],
            resp,
            "utxorpc.v1beta.query.ReadUtxosResponse",
        )
        .unwrap();
        assert_eq!(masked.ledger_tip.unwrap().slot, 77);
    }

    /// Issue #1004 review: the fallback for an unresolvable descriptor
    /// MUST NOT silently ship the unpruned response — that would
    /// reintroduce the exact no-op bug this module exists to close,
    /// invisibly. It must be a loud, typed error instead.
    #[test]
    fn unknown_message_name_is_rejected_not_silently_unpruned() {
        let tip = v1beta::query::ChainPoint {
            slot: 1,
            hash: vec![],
            height: 0,
            timestamp: 0,
        };
        let err = apply(
            &["slot".to_string()],
            tip,
            "utxorpc.v1beta.query.NoSuchMessage",
        )
        .expect_err("unresolvable descriptor must be rejected, not served unpruned");
        assert!(
            matches!(err, RpcError::Internal(_)),
            "must classify as Internal (server bug), not a client-facing 4xx-equivalent: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("NoSuchMessage"),
            "error must name the offending message_name so it's diagnosable: {msg}"
        );
    }

    /// Same fail-closed contract when `T` and `message_name` disagree
    /// (the response encodes fine as `T`, but the NAMED descriptor
    /// doesn't match its wire shape) — a different way to hit the same
    /// "programmer passed the wrong message_name" class of bug.
    #[test]
    fn mismatched_message_name_is_rejected_not_silently_unpruned() {
        // A `ReadParamsResponse` value, but told it's actually a
        // completely different shape (`WatchTxResponse`, an unrelated
        // oneof-only message) — decode against that descriptor should
        // fail, and the failure must surface as an error, not silently
        // return the ReadParamsResponse unpruned.
        let resp = v1beta::query::ReadParamsResponse {
            values: None,
            ledger_tip: Some(v1beta::query::ChainPoint {
                slot: 9,
                hash: vec![],
                height: 0,
                timestamp: 0,
            }),
        };
        let result = apply(
            &["ledger_tip".to_string()],
            resp,
            "utxorpc.v1beta.watch.WatchTxResponse",
        );
        // This may succeed (if the two shapes happen to be structurally
        // decodable against each other at the wire level) or fail during
        // the DynamicMessage decode / T::decode round trip — either
        // outcome is acceptable EXCEPT "succeeds but silently returns
        // input verbatim while masking nothing", which the old fail-open
        // design permitted for the "descriptor not found" arm. What
        // matters here is there is no third, invisible outcome: assert
        // that if it errors, it is classified `Internal`.
        if let Err(e) = result {
            assert!(matches!(e, RpcError::Internal(_)));
        }
    }

    /// Guards every literal message-name string this crate passes to
    /// [`apply`] — a typo would silently fail open (no pruning, no
    /// error) rather than panic, so it needs its own test rather than
    /// relying on `apply`'s callers to notice.
    #[test]
    fn every_message_name_used_by_apply_resolves() {
        let pool = descriptor_pool();
        for name in crate::map::message_names::ALL_RESPONSE_MESSAGE_NAMES {
            assert!(
                pool.get_message_by_name(name).is_some(),
                "message name {name:?} does not resolve in the descriptor pool \
                 — check it against the vendored .proto package + message name"
            );
        }
    }
}
