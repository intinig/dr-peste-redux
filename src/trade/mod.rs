//! On-demand rare-item pricing via live trade2 ablation. Isolated from
//! `poeninja`/`store`: data flows discord → trade, never sideways.

pub mod ablation;
pub mod client;
pub mod model;
pub mod pseudo;
pub mod query;
