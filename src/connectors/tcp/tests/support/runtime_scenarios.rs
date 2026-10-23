//! Dynamic universal Core qualification helpers; credentials are runner-provisioned.
use super::BenchError;
use skvoz_core::{
    ManagerConfig, PeerId,
    runtime::{Authentication, Membership, NatsRuntime, RuntimeConfig, Trust},
};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
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
pub async fn node(id: u64, case: &str) -> Result<NatsRuntime, BenchError> {
    Ok(NatsRuntime::connect(config(id, case)?, ManagerConfig::default()).await?)
}
pub async fn joined(
    client: &mut NatsRuntime,
    server: &mut NatsRuntime,
    id: u64,
) -> Result<(), BenchError> {
    let start = Instant::now();
    while !client.peer_ready(PeerId(0)) || !server.peer_ready(PeerId(id)) {
        if start.elapsed() > Duration::from_secs(8) {
            return Err(std::io::Error::other(format!(
                "join deadline client={:?} server={:?}",
                client.status(),
                server.status()
            ))
            .into());
        }
        client.turn(Duration::ZERO).await?;
        server.turn(Duration::from_millis(1)).await?;
    }
    Ok(())
}
