mod io;

pub use io::{plan, submit_report, validate_partition};

use crate::rack::Tray;

/// A single unit of validation work derived from a partition.
///
/// Stub: will carry partition type (NVL domain / IB fabric), tray IDs, etc.
pub struct ValidationJob {
    pub(crate) trays: Vec<Tray>,
}

pub struct Report {
    trays_cnt: u32,
}

/// Aggregator for per-partition reports within one validation cycle.
///
/// TODO[#416]: to be wired when `plan()` starts returning multiple jobs
/// and the top-level loop needs a single object to hand to
/// `submit_report`. The `_inner` prefix marks the field intentionally
/// unused until then, without resorting to `#[allow(dead_code)]`.
pub struct Reports {
    _inner: Vec<Report>,
}
