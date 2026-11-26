pub mod bgp;
pub mod revision;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MacTableEntry {
    pub mac: String,
    pub interface: String,
    #[serde(default)]
    pub entry_type: Option<String>,
    pub vlan: Option<u16>,
}
