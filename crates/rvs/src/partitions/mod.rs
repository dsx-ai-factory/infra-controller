use std::collections::HashMap;

mod ib;
mod nvl;

use carbide_uuid::machine::MachineId;
pub use ib::IbNode;
pub use nvl::NvlNode;

use crate::error::RvsError;
use crate::rack::{Racks, Tray};

/// Cross-rack partition index.
///
/// NVL domains and IB fabrics may span multiple racks, so partitions are
/// indexed globally rather than per-rack. Each tray in `all` carries its
/// `rack_id` for cases where the physical rack matters (e.g. NICo label
/// writes).
#[derive(Debug)]
pub struct Partitions {
    /// NVL domain UUID -> tray IDs in that domain (across all racks).
    pub nvl: HashMap<String, Vec<MachineId>>,
    /// IB fabric ID -> tray IDs on that fabric (across all racks).
    pub ib: HashMap<String, Vec<MachineId>>,
    /// Tray ID -> resolved tray (carries rack_id for provenance).
    pub all: HashMap<MachineId, Tray>,
}

impl Partitions {
    /// Drop trays whose rack is not in a validation-ready state, then prune
    /// any partition that lost a tray.
    fn exclude_non_validation_states(&mut self) {
        self.all.retain(|_, t| is_validation_state(&t.rack_state));
        self.nvl
            .retain(|_, ids| ids.iter().all(|id| self.all.contains_key(id)));
        self.ib
            .retain(|_, ids| ids.iter().all(|id| self.all.contains_key(id)));
    }

    /// Drop partitions where every tray already has `rv.st == "pass"`.
    fn exclude_completed(&mut self) {
        let all = &self.all;
        let all_passed = |ids: &Vec<MachineId>| {
            !ids.is_empty()
                && ids.iter().all(|id| {
                    all.get(id)
                        .and_then(|t| t.rv_labels.get("rv.st"))
                        .map(|st| st == "pass")
                        .unwrap_or(false)
                })
        };
        self.nvl.retain(|_, ids| !all_passed(ids));
        self.ib.retain(|_, ids| !all_passed(ids));
    }

    /// Construct from pre-built partition maps and tray lookup.
    pub fn new(
        nvl: HashMap<String, Vec<MachineId>>,
        ib: HashMap<String, Vec<MachineId>>,
        all: HashMap<MachineId, Tray>,
    ) -> Self {
        Self { nvl, ib, all }
    }
}

/// Build a flat Partitions index from all fetched racks.
///
/// NVL domains and IB fabrics are grouped globally: a domain or fabric that
/// spans multiple racks contributes trays from each rack into the same
/// partition entry.
impl TryFrom<Racks> for Partitions {
    type Error = RvsError;

    fn try_from(racks: Racks) -> Result<Self, Self::Error> {
        let mut nvl: HashMap<String, Vec<MachineId>> = HashMap::new();
        let mut ib: HashMap<String, Vec<MachineId>> = HashMap::new();
        let mut all: HashMap<MachineId, Tray> = HashMap::new();

        for fetched in racks.inner {
            for tray in fetched.trays {
                if let Some(ref nvl_data) = tray.nvl
                    && let Some(domain_uuid) = &nvl_data.domain_uuid
                {
                    nvl.entry(domain_uuid.to_string())
                        .or_default()
                        .push(tray.id);
                }

                if let Some(ref ib_data) = tray.ib {
                    for fabric_id in &ib_data.fabric_ids {
                        ib.entry(fabric_id.clone()).or_default().push(tray.id);
                    }
                }

                let gpu_count = tray.nvl.as_ref().map(|n| n.gpu_count).unwrap_or(0);
                let (ib_port_count, ib_active_port_count) = tray
                    .ib
                    .as_ref()
                    .map(|i| (i.port_count, i.active_port_count))
                    .unwrap_or((0, 0));

                all.insert(
                    tray.id,
                    Tray::new(
                        fetched.rack_id.clone(),
                        fetched.rack_state.clone(),
                        tray.rv_labels,
                        NvlNode::new(gpu_count),
                        IbNode::new(ib_port_count, ib_active_port_count),
                    ),
                );
            }
        }

        let mut p = Partitions::new(nvl, ib, all);
        p.exclude_non_validation_states();
        p.exclude_completed();
        Ok(p)
    }
}

/// Whether a rack lifecycle state string indicates the rack is ready for validation.
fn is_validation_state(state: &str) -> bool {
    matches!(
        state,
        "Validation(Pending)" | "Validation(Partial)" | "Validation(FailedPartial)"
    )
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use carbide_uuid::nvlink::NvLinkDomainId;
    use carbide_uuid::rack::RackId;

    use super::*;
    use crate::client::{TrayData, TrayIbData, TrayNvlData};
    use crate::rack::Racks;
    use crate::rack::racks::Rack;

    const DOMAIN_A: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    const DOMAIN_B: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    const DOMAIN_X: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";

    /// Build a deterministic `MachineId` keyed on `seed` -- used as a stand-in
    /// for real hardware IDs in tests.
    fn mid(seed: u8) -> MachineId {
        MachineId::new(MachineIdSource::Tpm, [seed; 32], MachineType::Host)
    }

    /// Test helper -- build a TrayData with optional NVL/IB fields.
    fn tray(id: MachineId, domain: Option<&str>, gpus: u32, ib_fabrics: &[&str]) -> TrayData {
        let nvl = if domain.is_some() || gpus > 0 {
            Some(TrayNvlData {
                domain_uuid: domain.map(|d| NvLinkDomainId::from_str(d).unwrap()),
                gpu_count: gpus,
            })
        } else {
            None
        };

        let ib = if !ib_fabrics.is_empty() {
            Some(TrayIbData {
                fabric_ids: ib_fabrics.iter().map(|f| f.to_string()).collect(),
                port_count: ib_fabrics.len() as u32,
                active_port_count: ib_fabrics.len() as u32,
            })
        } else {
            None
        };

        TrayData {
            id,
            rv_labels: HashMap::new(),
            nvl,
            ib,
        }
    }

    /// Test helper -- build a FetchedRack with a given state.
    fn rack(id: &str, state: &str, trays: Vec<TrayData>) -> Rack {
        Rack::new(RackId::from(id), state.to_string(), trays)
    }

    #[test]
    fn test_single_rack_partitions() {
        let (m1, m2, m3) = (mid(1), mid(2), mid(3));
        let fetched = vec![rack(
            "rack-1",
            "Validation(Pending)",
            vec![
                tray(m1, Some(DOMAIN_A), 4, &["fabric-1"]),
                tray(m2, Some(DOMAIN_A), 4, &["fabric-1"]),
                tray(m3, Some(DOMAIN_B), 4, &["fabric-1", "fabric-2"]),
            ],
        )];

        let p = Partitions::try_from(Racks { inner: fetched }).unwrap();

        assert_eq!(p.all.len(), 3);
        // All trays carry their rack provenance
        assert!(p.all.values().all(|t| t.rack_id.as_str() == "rack-1"));
        assert!(
            p.all
                .values()
                .all(|t| t.rack_state == "Validation(Pending)")
        );
        // NVL grouping
        assert_eq!(p.nvl.len(), 2);
        assert_eq!(p.nvl[DOMAIN_A], vec![m1, m2]);
        assert_eq!(p.nvl[DOMAIN_B], vec![m3]);
        // IB grouping -- m3 appears in both fabrics
        assert_eq!(p.ib.len(), 2);
        assert_eq!(p.ib["fabric-1"], vec![m1, m2, m3]);
        assert_eq!(p.ib["fabric-2"], vec![m3]);
    }

    #[test]
    fn test_cross_rack_domain_merging() {
        // DOMAIN_X and fabric-x each span both racks; racks carry different states.
        let (m1, m2) = (mid(1), mid(2));
        let fetched = vec![
            rack(
                "rack-1",
                "Validation(Pending)",
                vec![tray(m1, Some(DOMAIN_X), 4, &["fabric-x"])],
            ),
            rack(
                "rack-2",
                "Validation(Pending)",
                vec![tray(m2, Some(DOMAIN_X), 4, &["fabric-x"])],
            ),
        ];

        let p = Partitions::try_from(Racks { inner: fetched }).unwrap();

        assert_eq!(p.all.len(), 2);
        assert_eq!(p.all[&m1].rack_id.as_str(), "rack-1");
        assert_eq!(p.all[&m2].rack_id.as_str(), "rack-2");
        assert!(
            p.all
                .values()
                .all(|t| t.rack_state == "Validation(Pending)")
        );
        // Both trays land in the same NVL domain and IB fabric
        assert_eq!(p.nvl.len(), 1);
        assert_eq!(p.nvl[DOMAIN_X], vec![m1, m2]);
        assert_eq!(p.ib.len(), 1);
        assert_eq!(p.ib["fabric-x"], vec![m1, m2]);
    }

    #[test]
    fn test_tray_without_partition_data() {
        // A tray with no NVL domain and no IB fabrics should still appear in
        // `all` but not contribute to any partition group.
        let fetched = vec![rack(
            "rack-1",
            "Validation(Pending)",
            vec![tray(mid(1), None, 0, &[])],
        )];

        let p = Partitions::try_from(Racks { inner: fetched }).unwrap();

        assert_eq!(p.all.len(), 1);
        assert!(p.nvl.is_empty());
        assert!(p.ib.is_empty());
    }
}
