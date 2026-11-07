use std::collections::HashMap;

use carbide_uuid::rack::RackId;

use crate::partitions::{IbNode, NvlNode};

/// Resolved compute tray with partition-specific views and rack provenance.
#[derive(Debug)]
pub struct Tray {
    /// The rack this tray physically belongs to.
    ///
    /// Carried here so that partition-level operations (e.g. writing rv.*
    /// labels back to NICo) can identify which rack to target without needing
    /// a separate lookup.
    pub rack_id: RackId,
    /// Raw rack lifecycle state string as returned by NICo.
    ///
    /// Intentionally uninterpreted here: processing modules decide what states
    /// mean (e.g. "all racks in partition are Validation(Pending)" -> partition
    /// is ready for validation).
    pub rack_state: String,
    /// Rack-validation labels (`rv.*`) from machine metadata.
    ///
    /// Used by filtering logic (e.g. `exclude_completed`) to determine
    /// per-tray validation progress without additional I/O.
    pub rv_labels: HashMap<String, String>,
    /// NVLink perspective of this tray.
    pub nvl: NvlNode,
    /// InfiniBand perspective of this tray.
    pub ib: IbNode,
}

impl Tray {
    /// Construct from rack provenance, rack state, rv labels, and partition-specific views.
    pub fn new(
        rack_id: RackId,
        rack_state: String,
        rv_labels: HashMap<String, String>,
        nvl: NvlNode,
        ib: IbNode,
    ) -> Self {
        let tray = Self {
            rack_id,
            rack_state,
            rv_labels,
            nvl,
            ib,
        };
        tracing::trace!(rack_id = %tray.rack_id, rack_state = %tray.rack_state, nvlink_node = ?tray.nvl, ib_node = ?tray.ib, "Tray constructed");
        tray
    }
}
