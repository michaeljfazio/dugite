//! FieldMask application stubs — issue #672 M1.A.4.
//!
//! Most utxorpc requests carry a `google.protobuf.FieldMask` selecting
//! which response fields the server should populate. Honouring it
//! correctly requires per-top-level-message logic (mapping mask paths
//! → struct field clearing), which lives in M1.B / M2 alongside the
//! mapping modules that produce each response type.
//!
//! This module provides:
//!
//! * [`FieldMaskPath`] — a thin newtype that splits a single
//!   dot-separated mask path into segments. Parsing happens up front so
//!   the per-field hot loop in mapping code is just slice comparisons.
//! * [`apply`] — the entry point. M1.A returns the response unchanged
//!   when the mask is empty / missing; specific overloads land per
//!   response type as the corresponding service implementation lands.
//!
//! The reason we hand-roll mask logic rather than reach for
//! `prost-reflect` is that prost-reflect adds ~700 KB of code-size and a
//! complete descriptor-walker we don't need — masks in utxorpc are
//! shallow and well-known per response shape.

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

/// Apply a FieldMask (sequence of paths) to a response.
///
/// In M1.A this is a no-op stub — the response is returned unchanged
/// regardless of the mask. M1.B and later milestones add per-response
/// `apply_impl::<T>` specialisations that traverse the proto message
/// and clear fields not covered by any path. Until those land, clients
/// receive every field whether or not they asked for it — strictly more
/// data than requested, never less, so no client correctness issue.
pub fn apply<T>(mask_paths: &[String], response: T) -> T {
    let _ = mask_paths;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn apply_is_identity_in_m1a() {
        let r = apply(&[], "hello".to_string());
        assert_eq!(r, "hello");
        let r2 = apply(&["foo".to_string()], 42);
        assert_eq!(r2, 42);
    }
}
