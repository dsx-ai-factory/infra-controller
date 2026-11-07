use carbide_uuid::rack::RackId;

use crate::client::TrayData;

/// Bundled result of fetching one rack's data from NICo.
#[derive(Debug)]
pub struct Rack {
    /// Rack ID this data belongs to.
    pub rack_id: RackId,
    /// Raw rack lifecycle state string as returned by NICo.
    pub rack_state: String,
    /// Resolved tray data for this rack's compute trays.
    pub trays: Vec<TrayData>,
}

impl Rack {
    /// Construct from rack ID, rack state, and its fetched tray data.
    pub fn new(rack_id: RackId, rack_state: String, trays: Vec<TrayData>) -> Self {
        Self {
            rack_id,
            rack_state,
            trays,
        }
    }
}

pub struct Racks {
    pub inner: Vec<Rack>,
}
