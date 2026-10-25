//! Explicit environment configuration for the opt-in standalone workload CLI.
use super::BenchError;
use skvoz_core::{
    PeerId,
    runtime::{Authentication, Membership, RuntimeConfig, Trust},
};
use std::collections::BTreeSet;
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
    Ok(crate::nats_fixture::runtime_scenario_profile(config))
}
