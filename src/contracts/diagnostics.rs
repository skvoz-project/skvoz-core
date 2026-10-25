//! Opt-in owner-local timings; these are elapsed durations, not CPU samples.

#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(feature = "codec", derive(serde::Serialize, serde::Deserialize))]
pub struct TurnDiagnostics {
    pub enabled: bool,
    pub turns: u64,
    pub turn_us: u64,
    pub progress: u64,
    pub idle_count: u64,
    pub idle_us: u64,
    pub output_us: u64,
}
impl TurnDiagnostics {
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            *self = Self {
                enabled,
                ..Self::default()
            };
        }
    }
}
#[cfg(test)]
#[path = "tests/unit/diagnostics.rs"]
mod tests;
