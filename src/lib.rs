//! Universal SKVOZ byte streams, sans-I/O admission and optional NATS runtimes.
//!
//! Stream/Manager hosts own transport and the clock. Optional NATS runtimes
//! provide routing, bounded transport driving and session lifecycle. This crate
//! owns stream state, byte credit and admission. Frames are an experimental API,
//! not a stable wire format. Polling data does not acknowledge consumption.

#[cfg(feature = "nats")]
#[path = "nats_node.rs"]
pub mod nats;
mod peer_credit;
#[cfg(feature = "nats")]
pub mod runtime;
mod stream;
mod stream_manager;
#[cfg(feature = "nats")]
mod tls_config;
pub mod wire;

pub use skvoz_core_contracts::*;
pub use stream::Stream;
pub use stream_manager::*;
