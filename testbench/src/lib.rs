//! Experimental real-NATS test harness, not a production node runtime.

pub mod mesh;
pub mod monitor;
mod node;
pub mod process_workload;
pub mod runtime_scenarios;
pub mod scenarios;

pub use node::{BenchConfig, BenchError, ConnectionConfig, FailureKind, Node, NodeEvent, Role};
