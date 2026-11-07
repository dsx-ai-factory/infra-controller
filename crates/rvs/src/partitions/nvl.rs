/// NVLink view of a tray.
#[derive(Debug)]
pub struct NvlNode {
    /// GPUs visible to this tray via NVLink
    gpu_count: u32,
}

impl NvlNode {
    /// Construct from GPU count.
    pub fn new(gpu_count: u32) -> Self {
        let node = Self { gpu_count };
        tracing::trace!(gpu_count = node.gpu_count, "NvlNode constructed");
        node
    }
}
