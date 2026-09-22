mod io;
use std::collections::HashMap;

use carbide_uuid::machine::MachineId;
use carbide_uuid::nvlink::NvLinkDomainId;
use carbide_uuid::rack::RackId;
pub use io::NicoClient;
use rpc::forge::{Machine, Rack};

use crate::error::RvsError;

/// NVLink fields extracted from gRPC Machine.
#[derive(Debug)]
pub struct TrayNvlData {
    /// NVL domain this tray belongs to.
    pub domain_uuid: Option<NvLinkDomainId>,
    /// GPU count reported via NVLink info.
    pub gpu_count: u32,
}

/// InfiniBand fields extracted from gRPC Machine.
#[derive(Debug)]
pub struct TrayIbData {
    /// Fabric IDs observed across IB interfaces (used as partition keys).
    pub fabric_ids: Vec<String>,
    /// Total IB interfaces on this machine.
    pub port_count: u32,
    /// IB interfaces with active LID.
    pub active_port_count: u32,
}

/// Intermediate representation of a gRPC Machine.
#[derive(Debug)]
pub struct TrayData {
    /// Machine ID.
    pub id: MachineId,
    /// Rack-validation labels (`rv.*`) from machine metadata.
    pub rv_labels: HashMap<String, String>,
    /// NVLink data, if machine has NVLink info.
    pub nvl: Option<TrayNvlData>,
    /// InfiniBand data, if machine has IB status.
    pub ib: Option<TrayIbData>,
}

/// Extract TrayData from gRPC Machine.
impl TryFrom<Machine> for TrayData {
    type Error = RvsError;

    fn try_from(value: Machine) -> Result<Self, Self::Error> {
        let id = value.id.ok_or(RvsError::MissingField("Machine.id"))?;
        let status = value.status.unwrap_or_default();

        let nvl = status.nvlink_info.map(|info| TrayNvlData {
            domain_uuid: info.domain_uuid,
            gpu_count: info.gpus.len() as u32,
        });

        let ib = status.infiniband.map(|status| {
            let port_count = status.ib_interfaces.len() as u32;
            let active_port_count = status
                .ib_interfaces
                .iter()
                .filter(|iface| matches!(iface.lid, Some(lid) if lid != 0 && lid != 0xffff))
                .count() as u32;
            let fabric_ids = status
                .ib_interfaces
                .iter()
                .filter_map(|iface| iface.fabric_id.clone())
                .collect();
            TrayIbData {
                fabric_ids,
                port_count,
                active_port_count,
            }
        });

        let rv_labels = value
            .metadata
            .map(|m| {
                m.labels
                    .into_iter()
                    .filter(|l| l.key.starts_with("rv."))
                    .filter_map(|l| l.value.map(|v| (l.key, v)))
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            id,
            rv_labels,
            nvl,
            ib,
        })
    }
}

/// Intermediate representation of a gRPC Rack.
#[derive(Debug)]
pub struct RackData {
    /// Rack ID.
    pub id: RackId,
    /// Rack lifecycle state.
    pub state: String,
}

/// Extract RackData from gRPC Rack.
impl TryFrom<Rack> for RackData {
    type Error = RvsError;

    fn try_from(value: Rack) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id.ok_or(RvsError::MissingField("Rack.id"))?,
            state: value.rack_state,
        })
    }
}

/// Parsed SOT JSON document used for JSONPath artifact resolution.
///
/// Produced from a local file today (`cfg.sot_path`); see that field's TODO
/// for why the SOT has no API source post-#1861.
#[derive(Debug)]
pub struct RackFirmwareData {
    /// Identifier for this SOT record (e.g. "override").
    pub id: String,
    /// Parsed SOT JSON -- used for JSONPath artifact resolution.
    pub config: serde_json::Value,
}
