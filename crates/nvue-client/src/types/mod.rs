pub mod bgp;
pub mod revision;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MacTableEntry {
    pub mac: String,
    pub interface: String,
    /// NVUE omits `entry-type` for locally learned (dynamic) MACs, e.g. host-side
    /// MACs learned on a VLAN of `br_default` in L2 segment mode.
    #[serde(default)]
    pub entry_type: Option<String>,
    pub vlan: Option<u16>,
}
