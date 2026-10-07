pub mod bgp;
pub mod revision;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MacTableEntry {
    pub mac: String,
    pub interface: String,
    pub entry_type: String,
    pub vlan: Option<u16>,
}
