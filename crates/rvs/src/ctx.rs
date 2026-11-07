use crate::client::NicoClient;
use crate::config::Config;
use crate::scenario::Scenario;

/// Top-level RVS runtime context -- passed to all major routines.
///
/// Bundles the NICo client, loaded scenarios, and service config so individual
/// routines don't need to accept each piece separately.
pub struct RvsCtx {
    pub nico: NicoClient,
    pub scenarios: Vec<Scenario>,
    pub cfg: Config,
}
