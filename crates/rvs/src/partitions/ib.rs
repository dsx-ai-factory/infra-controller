/// Per-tray InfiniBand view.
#[derive(Debug)]
pub struct IbNode {
    /// Total IB interfaces on this tray.
    port_count: u32,
    /// IB interfaces with active LID (not 0 or 0xffff).
    active_port_count: u32,
}

impl IbNode {
    /// Construct from port counts.
    pub fn new(port_count: u32, active_port_count: u32) -> Self {
        let node = Self {
            port_count,
            active_port_count,
        };
        tracing::trace!(
            port_count = node.port_count,
            active_port_count = node.active_port_count,
            "IbNode constructed"
        );
        node
    }
}
