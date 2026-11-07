use std::collections::HashMap;

use carbide_uuid::machine::MachineId;
use uuid::Uuid;

use super::{Report, ValidationJob};
use crate::client::NicoClient;
use crate::error::RvsError;
use crate::partitions::Partitions;
use crate::rack::Tray;

/// Label writes needed to apply for machines.
struct RunIdPlan {
    /// Per-tray label maps to write. Empty when the run ID was reused as-is.
    updates: Vec<(MachineId, HashMap<String, String>)>,
}

/// Determine the run ID for this set of trays and compute any label updates.
///
/// Reuses the existing `rv.run-id` if all trays share the same value.
/// Otherwise generates a fresh UUID and prepares updated labels for every tray.
fn prepare_run_id(trays: &HashMap<MachineId, Tray>) -> RunIdPlan {
    match existing_run_id(trays) {
        Some(_) => RunIdPlan {
            // We don't really need an old ID - let's just say to a client
            // that no updates are required.
            updates: vec![],
        },
        None => {
            let id = Uuid::new_v4().to_string();
            let updates = trays
                .iter()
                .map(|(tray_id, tray)| {
                    let mut labels = tray.rv_labels.clone();
                    labels.insert("rv.run-id".to_string(), id.clone());
                    (*tray_id, labels)
                })
                .collect();
            RunIdPlan { updates }
        }
    }
}

/// Return the shared run ID if every tray already carries the same `rv.run-id`.
fn existing_run_id(trays: &HashMap<MachineId, Tray>) -> Option<&String> {
    // Every tray must carry `rv.run-id` -- a partial match would leave the
    // label-less trays drifting while the rest reuse the shared ID.
    let mut run_ids = trays.values().map(|t| t.rv_labels.get("rv.run-id"));
    let first = run_ids.next().flatten()?;
    if run_ids.all(|id| id == Some(first)) {
        Some(first)
    } else {
        None
    }
}

/// Keep only trays still referenced by at least one retained partition.
///
/// `Partitions::exclude_completed` prunes all-passed partitions from
/// `nvl`/`ib` but leaves the underlying trays in `all`. Re-queuing those
/// completed trays is what we want to avoid.
fn retained_trays(partitions: Partitions) -> HashMap<MachineId, Tray> {
    let Partitions { nvl, ib, all } = partitions;
    let retained_ids: std::collections::HashSet<MachineId> = nvl
        .into_values()
        .chain(ib.into_values())
        .flatten()
        .collect();
    all.into_iter()
        .filter(|(id, _)| retained_ids.contains(id))
        .collect()
}

/// Convert filtered partitions into validation jobs.
pub async fn plan(
    partitions: Partitions,
    nico: &NicoClient,
    os_uri: &str,
) -> Result<Vec<ValidationJob>, RvsError> {
    let trays = retained_trays(partitions);
    if trays.is_empty() {
        return Ok(vec![]);
    }
    assign_run_id(&trays, nico).await?;
    allocate_instances(&trays, os_uri, nico).await?;
    wait_for_boot(&trays, nico).await?;
    Ok(vec![ValidationJob {
        trays: trays.into_values().collect(),
    }])
}

/// Ensure every tray carries a consistent `rv.run-id`, writing it if absent.
async fn assign_run_id(
    trays: &HashMap<MachineId, Tray>,
    nico: &NicoClient,
) -> Result<(), RvsError> {
    let plan = prepare_run_id(trays);
    for (tray_id, labels) in plan.updates {
        nico.update_rv_labels(&tray_id, labels).await?;
    }
    Ok(())
}

/// Allocate a validation OS instance on each tray in the partition.
///
/// TODO[#416]: stub - wire in nico.allocate_machine_instance per tray and collect
/// instance IDs for boot tracking. ValidationJob will carry them once expanded.
async fn allocate_instances(
    _trays: &HashMap<MachineId, Tray>,
    _os_uri: &str,
    _nico: &NicoClient,
) -> Result<(), RvsError> {
    let () = std::future::ready(()).await; // phantom await: keeps async sig for future wiring
    Ok(())
}

/// Wait until every allocated instance has booted and reached READY state.
///
/// TODO[#416]: stub - wire in polling loop with exponential backoff and timeout once
/// allocate_instances populates instance IDs on ValidationJob.
async fn wait_for_boot(
    _trays: &HashMap<MachineId, Tray>,
    _nico: &NicoClient,
) -> Result<(), RvsError> {
    let () = std::future::ready(()).await; // phantom await: keeps async sig for future wiring
    Ok(())
}

/// Run validation against a single job and produce a report.
///
/// Stub: counts trays in the partition as a stand-in for real validation output.
pub async fn validate_partition(job: ValidationJob) -> Result<Report, RvsError> {
    let trays_cnt = job.trays.len() as u32;
    tracing::info!(
        tray_count = trays_cnt,
        "validation: partition validated (stub)"
    );
    Ok(Report { trays_cnt })
}

/// Submit a completed report.
///
/// Stub: prints tray count to console.
pub async fn submit_report(report: Report) -> Result<(), RvsError> {
    tracing::info!(tray_count = report.trays_cnt, "validation report");
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use carbide_uuid::rack::RackId;

    use super::*;
    use crate::partitions::{IbNode, NvlNode};

    fn mid(seed: u8) -> MachineId {
        MachineId::new(MachineIdSource::Tpm, [seed; 32], MachineType::Host)
    }

    fn tray(rv_labels: &[(&str, &str)]) -> Tray {
        Tray::new(
            RackId::from("rack-1"),
            "Validation(Pending)".to_string(),
            rv_labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            NvlNode::new(0),
            IbNode::new(0, 0),
        )
    }

    fn trays(entries: &[(MachineId, &[(&str, &str)])]) -> HashMap<MachineId, Tray> {
        entries
            .iter()
            .map(|(id, labels)| (*id, tray(labels)))
            .collect()
    }

    #[test]
    fn test_prepare_run_id_reuses_existing() {
        let t = trays(&[
            (mid(1), &[("rv.run-id", "run-abc")]),
            (mid(2), &[("rv.run-id", "run-abc")]),
        ]);
        let plan = prepare_run_id(&t);
        assert!(plan.updates.is_empty());
    }

    #[test]
    fn test_prepare_run_id_assigns_new_when_missing() {
        let t = trays(&[(mid(1), &[]), (mid(2), &[])]);
        let plan = prepare_run_id(&t);
        assert_eq!(plan.updates.len(), 2);
        for (_, labels) in &plan.updates {
            assert!(!labels["rv.run-id"].is_empty());
        }
    }

    #[test]
    fn test_prepare_run_id_assigns_new_when_mixed() {
        // Trays disagree on run-id - treat as missing, assign fresh.
        let t = trays(&[
            (mid(1), &[("rv.run-id", "run-abc")]),
            (mid(2), &[("rv.run-id", "run-xyz")]),
        ]);
        let plan = prepare_run_id(&t);
        assert_eq!(plan.updates.len(), 2);
        for (_, labels) in &plan.updates {
            assert!(!labels["rv.run-id"].is_empty());
        }
    }

    #[test]
    fn test_prepare_run_id_assigns_new_when_partially_missing() {
        // One tray carries run-id, another doesn't -- must not be treated as
        // shared, otherwise the label-less tray never gets written.
        let t = trays(&[(mid(1), &[("rv.run-id", "run-abc")]), (mid(2), &[])]);
        let plan = prepare_run_id(&t);
        assert_eq!(plan.updates.len(), 2);
        for (_, labels) in &plan.updates {
            assert!(!labels["rv.run-id"].is_empty());
        }
    }

    #[test]
    fn test_prepare_run_id_preserves_other_labels() {
        let m1 = mid(1);
        let t = trays(&[(m1, &[("rv.st", "pass")])]);
        let plan = prepare_run_id(&t);
        let (_, labels) = plan.updates.iter().find(|(id, _)| id == &m1).unwrap();
        assert_eq!(labels["rv.st"], "pass");
        assert!(!labels["rv.run-id"].is_empty());
    }

    #[test]
    fn test_prepare_run_id_empty_trays() {
        let t = trays(&[]);
        let plan = prepare_run_id(&t);
        assert!(plan.updates.is_empty());
    }

    fn partitions(
        nvl: &[(&str, &[MachineId])],
        ib: &[(&str, &[MachineId])],
        all: &[MachineId],
    ) -> Partitions {
        Partitions::new(
            nvl.iter()
                .map(|(k, ids)| (k.to_string(), ids.to_vec()))
                .collect(),
            ib.iter()
                .map(|(k, ids)| (k.to_string(), ids.to_vec()))
                .collect(),
            all.iter().map(|id| (*id, tray(&[]))).collect(),
        )
    }

    #[test]
    fn test_retained_trays_drops_trays_from_excluded_partitions() {
        // m1/m2 are in no retained partition (their partition was all-passed
        // and dropped); m3/m4 are still referenced.
        let (m1, m2, m3, m4) = (mid(1), mid(2), mid(3), mid(4));
        let p = partitions(&[("nvl-b", &[m3, m4])], &[], &[m1, m2, m3, m4]);
        let kept = retained_trays(p);
        let mut ids: Vec<_> = kept.keys().copied().collect();
        ids.sort();
        let mut expected = vec![m3, m4];
        expected.sort();
        assert_eq!(ids, expected);
    }

    #[test]
    fn test_retained_trays_drops_orphans() {
        // m1 is in `all` but no partition references it.
        let p = partitions(&[], &[], &[mid(1)]);
        assert!(retained_trays(p).is_empty());
    }

    #[test]
    fn test_retained_trays_keeps_trays_in_any_partition() {
        // m1 is only in ib, m2 only in nvl -- both should survive.
        let (m1, m2) = (mid(1), mid(2));
        let p = partitions(&[("nvl-a", &[m2])], &[("ib-a", &[m1])], &[m1, m2]);
        let kept = retained_trays(p);
        assert_eq!(kept.len(), 2);
    }
}
