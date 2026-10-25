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
    let mut config = RuntimeConfig::new(
        std::env::var("SKVOZ_NATS_URL")?,
        Trust::ManagedCa(std::env::var("SKVOZ_NATS_CA")?.into()),
        Authentication {
            username: format!("p{id}"),
            password: std::env::var(format!("SKVOZ_NATS_P{id}_PASSWORD"))?,
        },
        format!(
            "skvoz.runtime.{}.{case}",
            std::env::var("SKVOZ_NATS_RUN_TOKEN")?
        ),
        PeerId(id),
        if id == 0 {
            Membership::BrokerAuthorized
        } else {
            Membership::Allowlist(BTreeSet::from([PeerId(0)]))
        },
    );
    config.initiate = if id == 0 { vec![] } else { vec![PeerId(0)] };
    Ok(skvoz_core_test_support::runtime_scenario_profile(config))
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
