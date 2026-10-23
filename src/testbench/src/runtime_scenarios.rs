//! Explicit environment configuration for the opt-in standalone workload CLI.
use super::BenchError;
use skvoz_core::{
    PeerId,
    runtime::{Authentication, Membership, RuntimeConfig, Trust},
};
use std::{collections::BTreeSet, time::Duration};
pub fn config(id: u64, case: &str) -> Result<RuntimeConfig, BenchError> {
    Ok(RuntimeConfig {
        url: std::env::var("SKVOZ_NATS_URL")?,
        tls_server_name: None,
        trust: Trust::ManagedCa(std::env::var("SKVOZ_NATS_CA")?.into()),
        authentication: Authentication {
            username: format!("p{id}"),
            password: std::env::var(format!("SKVOZ_NATS_P{id}_PASSWORD"))?,
        },
        namespace: format!(
            "skvoz.runtime.{}.{case}",
            std::env::var("SKVOZ_NATS_RUN_TOKEN")?
        ),
        id: PeerId(id),
        membership: if id == 0 {
            Membership::BrokerAuthorized
        } else {
            Membership::Allowlist(BTreeSet::from([PeerId(0)]))
        },
        initiate: if id == 0 { vec![] } else { vec![PeerId(0)] },
        shards: 8,
        subscription_capacity: 128,
        join_capacity: 128,
        client_capacity: 16,
        max_incoming_per_turn: 64,
        max_outgoing_per_turn: 32,
        io_timeout: Duration::from_millis(700),
        heartbeat_interval: Duration::from_millis(100),
        peer_timeout: Duration::from_millis(800),
        join_timeout: Duration::from_secs(5),
        retry_initial: Duration::from_millis(100),
        retry_max: Duration::from_secs(1),
        max_retries: 8,
        terminal_drain_timeout: Duration::from_secs(5),
    })
}
