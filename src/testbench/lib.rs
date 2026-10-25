//! Standalone demo/load and process workload helpers; no dependency on Core tests.
pub mod mesh;
pub mod monitor;
mod node;
pub mod process_workload;
pub mod runtime_scenarios;
pub mod scenarios;
pub use node::{BenchConfig, BenchError, ConnectionConfig, FailureKind, Node, NodeEvent, Role};
