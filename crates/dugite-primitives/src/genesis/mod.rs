//! Genesis-file types.
//!
//! Today this module hosts only the Dijkstra (post-Conway HFC) genesis type;
//! the older Byron/Shelley/Alonzo/Conway genesis structs still live in
//! `dugite-node::genesis` for historical reasons and will be migrated here in
//! a follow-up consolidation pass. New genesis types should land here.

pub mod dijkstra;

pub use dijkstra::{DijkstraGenesis, PositiveInterval};
