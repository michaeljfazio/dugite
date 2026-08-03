//! The node's N2C `LocalStateQuery` reply encoder, compiled in directly.
//!
//! `encoding.rs` refers to its sibling as `crate::node::n2c_query::types`, so
//! this module path is reproduced verbatim — the same `#[path]` technique
//! `dugite-node`'s own lib.rs uses to expose these two files without dragging
//! in its 11.8k-line `node/mod.rs`.
//!
//! Compiled in rather than reached through the `dugite-node` crate because
//! depending on that crate pulls in `mithril-client`, whose native deps (blst,
//! aws-lc-sys) and `inventory`/`typetag` static initializers do not survive
//! sancov instrumentation — `ld: initializer pointer has no target`. Adding the
//! dependency broke the build of every target in this workspace, not just the
//! new ones.
//!
//! These two files are self-contained: `types.rs` has no `crate::` references
//! and `encoding.rs` refers only to `types`.

#[path = "../../../../crates/dugite-node/src/node/n2c_query/types.rs"]
pub mod types;

#[path = "../../../../crates/dugite-node/src/node/n2c_query/encoding.rs"]
pub mod encoding;
