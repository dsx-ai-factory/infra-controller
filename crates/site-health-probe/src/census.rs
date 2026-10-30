/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Census rules: how one unit type is read (a census of IDs, then by-ID reads
//! in batches) and how NICo state strings become bounded label values.
//!
//! A count never includes a unit the probe did not read. An ID the by-ID read
//! did not return is looked up once more in a fresh census: gone from it, the
//! unit was deleted mid-sweep and is not counted; still in it, the unit is
//! unresolved and its family keeps the last snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use carbide_site_health_probe_api::Initiator;

use crate::gate::CallError;

/// One unit type's by-ID read, keyed by unit ID.
#[derive(Debug)]
pub(crate) struct CensusRead<R> {
    pub rows: BTreeMap<String, R>,
    /// Missing from the by-ID read and from the second census: deleted.
    pub gone: BTreeSet<String>,
    /// Missing from the by-ID read but still in the second census.
    pub unresolved: BTreeSet<String>,
}

/// Reads every census ID once, `batch` IDs per by-ID call. Rows the call was
/// not asked for, and repeats, are dropped. Misses get one second census
/// (`recensus`) that tells gone from unresolved.
pub(crate) async fn read_family<R, F, Fut, C, CFut>(
    census: Vec<String>,
    batch: usize,
    mut fetch: F,
    id_of: fn(&R) -> String,
    recensus: C,
) -> Result<CensusRead<R>, CallError>
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: Future<Output = Result<Vec<R>, CallError>>,
    C: FnOnce() -> CFut,
    CFut: Future<Output = Result<Vec<String>, CallError>>,
{
    let ids: Vec<String> = census
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut rows = BTreeMap::new();
    for chunk in ids.chunks(batch.max(1)) {
        let asked: BTreeSet<&String> = chunk.iter().collect();
        for row in fetch(chunk.to_vec()).await? {
            let id = id_of(&row);
            if asked.contains(&id) && !rows.contains_key(&id) {
                rows.insert(id, row);
            }
        }
    }
    let missing: Vec<&String> = ids.iter().filter(|id| !rows.contains_key(*id)).collect();
    let mut gone = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    if !missing.is_empty() {
        let again: BTreeSet<String> = recensus().await?.into_iter().collect();
        for id in missing {
            if again.contains(id) {
                unresolved.insert(id.clone());
            } else {
                gone.insert(id.clone());
            }
        }
    }
    Ok(CensusRead {
        rows,
        gone,
        unresolved,
    })
}

/// Host state categories: the leading identifier of the `ManagedHostState`
/// Display string (crates/api-model/src/machine/mod.rs) and its label. The
/// table is pinned: a new top-level state shows up as `other` and counts
/// `unknown_states_total` until it is added here.
pub(crate) const HOST_STATE_CATEGORIES: [(&str, &str); 26] = [
    ("ConfigureAstra", "ConfigureAstra"),
    ("DPUDiscovering", "DPUDiscovering"),
    ("DPUInitializing", "DPUInitializing"),
    ("HostInitializing", "HostInitializing"),
    ("Ready", "Ready"),
    ("Decommissioning", "Decommissioning"),
    ("Reset", "Reset"),
    ("BootConfiguring", "BootConfiguring"),
    // `Maintenance(<operation>)`: the operation never reaches a label.
    ("Maintenance", "Maintenance"),
    ("Assigned", "Assigned"),
    ("WaitingForCleanup", "WaitingForCleanup"),
    ("ForceDeletion", "ForceDeletion"),
    ("Failed", "Failed"),
    // DPU reprovisioning.
    ("Reprovisioning", "Reprovisioning"),
    ("HostReprovisioning", "HostReprovisioning"),
    ("RotatingBmc", "RotatingBmc"),
    ("RotatingHostUefi", "RotatingHostUefi"),
    ("RotatingDpuUefi", "RotatingDpuUefi"),
    ("RotatingNicLockdown", "RotatingNicLockdown"),
    ("Measuring", "Measuring"),
    ("PostAssignedMeasuring", "PostAssignedMeasuring"),
    ("PreAssignedMeasuring", "PreAssignedMeasuring"),
    ("Created", "Created"),
    ("BomValidating", "BomValidating"),
    // Validation prints its `ValidationState` Debug form,
    // `MachineValidation { .. }`.
    ("MachineValidation", "Validation"),
    ("StartAssignmentCycle", "StartAssignmentCycle"),
];

/// The label of state categories that are not in the pinned tables.
pub(crate) const OTHER: &str = "other";

/// The host state category of a `ManagedHostState` Display string, or `None`
/// for a state the pinned table does not know.
pub(crate) fn host_category(display: &str) -> Option<&'static str> {
    let token: String = display
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    HOST_STATE_CATEGORIES
        .iter()
        .find(|(prefix, _)| *prefix == token)
        .map(|(_, category)| *category)
}

/// Host categories in which an attached DPU has finished its own setup.
const DPU_DONE_UNDER: [&str; 19] = [
    "HostInitializing",
    "Validation",
    "BomValidating",
    "Measuring",
    "Ready",
    "Assigned",
    "BootConfiguring",
    "Maintenance",
    "HostReprovisioning",
    "Reprovisioning",
    "RotatingBmc",
    "RotatingHostUefi",
    "RotatingDpuUefi",
    "RotatingNicLockdown",
    "PreAssignedMeasuring",
    "PostAssignedMeasuring",
    "StartAssignmentCycle",
    "WaitingForCleanup",
    "Reset",
];

/// Whether a DPU attached to a host in `host_category` is done. `Created`
/// and `ConfigureAstra` come before DPU discovery; failed, leaving and
/// unknown hosts carry no finished DPUs.
pub(crate) fn dpu_done_under(host_category: &str) -> bool {
    DPU_DONE_UNDER.contains(&host_category)
}

/// The state tag of a switch, power shelf or rack lifecycle state, which
/// nico-api sends as the controller state's JSON (`{"state": "ready", ..}`).
pub(crate) fn lifecycle_tag(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = value.get("state")?.as_str()?;
    // Tags are serde variant names. Anything else must not become a label.
    (!tag.is_empty() && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then(|| tag.to_string())
}

/// The initiator of a host firmware request. Requests made through the API
/// carry the `UpdateInitiator` name ("Automatic", "AdminCli"); the machine
/// update manager writes "Automated".
pub(crate) fn initiator(raw: &str) -> Initiator {
    match raw {
        "Automatic" | "Automated" => Initiator::Automatic,
        "AdminCli" => Initiator::Manual,
        _ => Initiator::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use carbide_test_support::{Check, check_values};

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Row {
        id: String,
        n: u32,
    }

    fn row(id: &str, n: u32) -> Row {
        Row {
            id: id.to_string(),
            n,
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| id.to_string()).collect()
    }

    #[tokio::test]
    async fn census_counts_each_id_once() {
        let asked = Mutex::new(Vec::new());
        let read = read_family(
            ids(&["a", "b", "a", "c", "b"]),
            2,
            |batch| {
                asked.lock().unwrap().push(batch.clone());
                async move { Ok(batch.iter().map(|id| row(id, 1)).collect()) }
            },
            |r: &Row| r.id.clone(),
            || async { Ok(vec![]) },
        )
        .await
        .expect("read");
        assert_eq!(read.rows.len(), 3);
        assert_eq!(
            *asked.lock().unwrap(),
            vec![ids(&["a", "b"]), ids(&["c"])],
            "each ID asked once, in batches"
        );
        assert!(read.gone.is_empty() && read.unresolved.is_empty());
    }

    #[tokio::test]
    async fn by_id_extra_and_repeated_rows_dropped() {
        let read = read_family(
            ids(&["a", "b"]),
            100,
            |_batch| async {
                Ok(vec![
                    row("a", 1),
                    row("a", 2),
                    row("stranger", 1),
                    row("b", 1),
                ])
            },
            |r: &Row| r.id.clone(),
            || async { Ok(vec![]) },
        )
        .await
        .expect("read");
        assert_eq!(read.rows.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(read.rows["a"].n, 1, "the first row of a repeated ID wins");
    }

    #[tokio::test]
    async fn missing_id_gone_vs_unresolved() {
        let recensus_calls = Mutex::new(0);
        let read = read_family(
            ids(&["a", "deleted", "stuck"]),
            100,
            |_batch| async { Ok(vec![row("a", 1)]) },
            |r: &Row| r.id.clone(),
            || {
                *recensus_calls.lock().unwrap() += 1;
                async { Ok(ids(&["a", "stuck", "new"])) }
            },
        )
        .await
        .expect("read");
        assert_eq!(read.rows.len(), 1);
        assert_eq!(read.gone, BTreeSet::from(["deleted".to_string()]));
        assert_eq!(read.unresolved, BTreeSet::from(["stuck".to_string()]));
        assert_eq!(*recensus_calls.lock().unwrap(), 1);

        // No misses, no second census.
        read_family(
            ids(&["a"]),
            100,
            |_batch| async { Ok(vec![row("a", 1)]) },
            |r: &Row| r.id.clone(),
            || async { Err::<Vec<String>, _>(CallError::Timeout) },
        )
        .await
        .expect("no recensus needed");
    }

    #[test]
    fn host_state_categories_pinned() {
        // One Display string per top-level ManagedHostState variant, as
        // nico-api renders them.
        check_values(
            [
                ("ConfigureAstra/Configuring", Some("ConfigureAstra")),
                ("DPUDiscovering/Initializing", Some("DPUDiscovering")),
                (
                    "DPUInitializing/WaitingForNetworkConfig",
                    Some("DPUInitializing"),
                ),
                (
                    "HostInitializing/WaitingForDiscovery",
                    Some("HostInitializing"),
                ),
                ("Ready", Some("Ready")),
                ("Decommissioning/Cleanup", Some("Decommissioning")),
                ("Reset/Start", Some("Reset")),
                ("BootConfiguring/Start", Some("BootConfiguring")),
                ("Maintenance(PowerCycle)", Some("Maintenance")),
                ("Assigned/Ready", Some("Assigned")),
                ("Assigned/Reprovision/Ready", Some("Assigned")),
                ("WaitingForCleanup/HostCleanup", Some("WaitingForCleanup")),
                ("ForceDeletion", Some("ForceDeletion")),
                ("Failed/MachineValidation", Some("Failed")),
                (
                    "Reprovisioning/WaitingForNetworkConfig",
                    Some("Reprovisioning"),
                ),
                (
                    "HostReprovisioning/CheckingFirmware",
                    Some("HostReprovisioning"),
                ),
                ("RotatingBmc", Some("RotatingBmc")),
                ("RotatingHostUefi/Start", Some("RotatingHostUefi")),
                ("RotatingDpuUefi/fm100dsx", Some("RotatingDpuUefi")),
                ("RotatingNicLockdown", Some("RotatingNicLockdown")),
                ("Measuring/WaitingForMeasurements", Some("Measuring")),
                (
                    "PostAssignedMeasuring/MeasuredBoot/Pending",
                    Some("PostAssignedMeasuring"),
                ),
                ("PreAssignedMeasuring/Pending", Some("PreAssignedMeasuring")),
                ("Created", Some("Created")),
                ("BomValidating/MatchingSku", Some("BomValidating")),
                (
                    "MachineValidation { machine_validation: Start }",
                    Some("Validation"),
                ),
                ("StartAssignmentCycle", Some("StartAssignmentCycle")),
                ("SomethingNew/Sub", None),
                ("", None),
            ]
            .map(|(input, expect)| Check {
                scenario: input,
                input,
                expect,
            }),
            host_category,
        );
        assert_eq!(
            HOST_STATE_CATEGORIES.len(),
            HOST_STATE_CATEGORIES
                .iter()
                .map(|(token, _)| token)
                .collect::<BTreeSet<_>>()
                .len()
        );
        for category in DPU_DONE_UNDER {
            assert!(
                HOST_STATE_CATEGORIES.iter().any(|(_, c)| *c == category),
                "{category} is a host category"
            );
        }
    }

    #[test]
    fn host_state_maintenance_and_validation_normalized() {
        assert_eq!(
            host_category("Maintenance(FirmwareUpgrade { component: Bmc })"),
            Some("Maintenance")
        );
        assert_eq!(
            host_category(
                "MachineValidation { machine_validation: RebootHost { validation_id: 7 } }"
            ),
            Some("Validation")
        );
    }

    #[test]
    fn lifecycle_tags_and_initiators() {
        assert_eq!(
            lifecycle_tag(r#"{"state":"ready"}"#).as_deref(),
            Some("ready")
        );
        assert_eq!(
            lifecycle_tag(r#"{"state":"initializing","initializing_state":"x"}"#).as_deref(),
            Some("initializing")
        );
        assert_eq!(lifecycle_tag("ready"), None);
        assert_eq!(lifecycle_tag(r#"{"state":"has space"}"#), None);
        assert_eq!(initiator("Automatic"), Initiator::Automatic);
        assert_eq!(initiator("Automated"), Initiator::Automatic);
        assert_eq!(initiator("AdminCli"), Initiator::Manual);
        assert_eq!(initiator("someone"), Initiator::Other);
    }
}
