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

use std::collections::HashSet;
use std::fmt::Write;

use carbide_uuid::infiniband::IBPartitionId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ib_partition::PartitionKey;
use crate::instance::config::infiniband::InstanceInfinibandConfig;

/// The infiniband status that was last reported by the networking subsystem
/// Stored in a Postgres JSON field
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInfinibandStatusObservation {
    /// Observed status for each configured interface
    #[serde(default)]
    pub ib_interfaces: Vec<MachineIbInterfaceStatusObservation>,

    /// When this status was observed
    pub observed_at: DateTime<Utc>,
}

/// The infiniband interface status that was last reported by the infiniband subsystem
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineIbInterfaceStatusObservation {
    /// The GUID whose status has been monitored
    pub guid: String,
    /// The ocal Identifier observed from UFM. This is set to 0xffff if no status
    /// could be retrieved or if the port is not reported as Active.
    pub lid: u16,
    /// The ID of the fabric on which the GUID has been observed
    /// This is empty if the GUID hasn't been observed on any fabric
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fabric_id: String,
    /// Partition keys currently associated with the interface at UFM
    /// None means the associated pkeys could not be determined
    pub associated_pkeys: Option<HashSet<PartitionKey>>,
    /// Partition IDs currently associated with the interface at UFM
    /// None means the associated pkeys could not be determined.
    /// The amount of IDs can be different than the amount of `associated_pkeys`
    /// in case a pkey that is associated with the port does not map to any
    /// partition ID.
    pub associated_partition_ids: Option<HashSet<IBPartitionId>>,
}

/// The reason why the IB config is not synced
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IbConfigNotSyncedReason {
    /// Port states could not be observed
    PortStateUnobservable { guids: Vec<String>, details: String },
    /// Configuration mismatch between expected and actual
    ConfigurationMismatch { details: String },
    /// Missing observation data entirely
    MissingObservation { reason: String },
}

impl std::fmt::Display for IbConfigNotSyncedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PortStateUnobservable { details, .. } => {
                write!(f, "Port state unobservable: {}", details)
            }
            Self::ConfigurationMismatch { details } => {
                write!(f, "Configuration mismatch: {}", details)
            }
            Self::MissingObservation { reason } => {
                write!(f, "Missing observation: {}", reason)
            }
        }
    }
}

/// Returns whether the desired InfiniBand config for a Machine has been applied
pub fn ib_config_synced(
    observation: Option<&MachineInfinibandStatusObservation>,
    config: Option<&InstanceInfinibandConfig>,
    use_tenant_network: bool,
) -> Result<(), IbConfigNotSyncedReason> {
    let Some(config) = config.as_ref() else {
        // If no IB config is requested, we always treat the config as applied
        // TODO: This is to achieve the same behavior as the current system, where hosts without
        // IB config don't care about what is configured.
        // In the future we should also check here whether all interfaces/ports have **no** pkeys assigned to them.
        // If there are any assigned, the state should be marked as not synced.
        return Ok(());
    };

    if config.ib_interfaces.is_empty() {
        // If no IB config is requested, we always treat the config as applied
        // TODO: This is to achieve the same behavior as the current system, where hosts without
        // IB config don't care about what is configured.
        // In the future we should also check here whether all interfaces/ports have **no** pkeys assigned to them.
        // If there are any assigned, the state should be marked as not synced.
        return Ok(());
    }

    // The tenant requested to use IB. In this case
    // - if the tenant network is still utilized (`use_tenant_config == true`), all interfaces that the tenant wants to use should be on the tenant network
    // - if the tenant network is not utilized, all interfaces that the tenant wants to use should be on no network
    // For interfaces that the tenant does not want to use, we will not perform any checks at the moment
    let Some(observation) = observation.as_ref() else {
        return Err(IbConfigNotSyncedReason::MissingObservation {
            reason: "Due to missing IB status observation, it can't be verified whether the IB config is applied at UFM".to_string(),
        });
    };

    let mut misconfigured_guids = Vec::new();
    let mut unknown_guid_states = Vec::new();

    for iface in config.ib_interfaces.iter() {
        let Some(guid) = iface.guid.as_ref() else {
            continue;
        };
        let expected_partition_id = iface.ib_partition_id;

        let Some(actual_iface_state) = observation
            .ib_interfaces
            .iter()
            .find(|iface| iface.guid == *guid)
        else {
            // We can't look up the observation. This should never happen, as the observation field
            // for each interface is always populated.
            unknown_guid_states.push(guid.to_string());
            continue;
        };

        let Some(associated_pkeys) = actual_iface_state.associated_pkeys.as_ref() else {
            unknown_guid_states.push(guid.to_string());
            continue;
        };

        let Some(associated_partition_ids) = actual_iface_state.associated_partition_ids.as_ref()
        else {
            unknown_guid_states.push(guid.to_string());
            continue;
        };

        if use_tenant_network {
            // The interface should use exactly the partition ID that is requested
            if associated_pkeys.len() != 1
                || associated_partition_ids.len() != 1
                || *associated_partition_ids.iter().next().unwrap() != expected_partition_id
            {
                misconfigured_guids
                    .push((guid.to_string(), format!("[\"{expected_partition_id}\"]")));
            }
        } else {
            // The interface should not be on any partition
            if !associated_pkeys.is_empty() || !associated_partition_ids.is_empty() {
                misconfigured_guids.push((guid.to_string(), "[]".to_string()));
            }
        }
    }

    // TODO: Check here whether all interfaces that are not referenced in the config
    // are set to have exactly 0 pkeys configured
    // This is only possible once we know there's no manually
    // configured pkeys anymore in the system

    // If ports are unreachable (down), return PortStateUnobservable
    // This is critical during termination as we need special retry logic
    if !unknown_guid_states.is_empty() {
        let details = format!(
            "IB status observation for interface with GUIDs {} is missing or incomplete. Ports may be down/unreachable.",
            unknown_guid_states.join(",")
        );
        return Err(IbConfigNotSyncedReason::PortStateUnobservable {
            guids: unknown_guid_states,
            details,
        });
    }

    // If there are configuration mismatches, return ConfigurationMismatch
    if !misconfigured_guids.is_empty() {
        let mut errors = String::new();
        for (guid, expectation) in misconfigured_guids.iter() {
            if !errors.is_empty() {
                errors.push('\n');
            }
            write!(
                &mut errors,
                "Interface with GUID {guid} should be assigned to partition IDs {expectation}"
            )
            .unwrap();
        }
        return Err(IbConfigNotSyncedReason::ConfigurationMismatch { details: errors });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios};

    use super::*;

    #[test]
    fn deserialize_legacy_ib_status_observation() {
        // Legacy IB status observations should deserialize, defaulting the fields
        // that older payloads omit. We project to the first interface's
        // (fabric_id_is_empty, associated_pkeys_is_none) so the asserted defaults
        // are comparable.
        scenarios!(
            // Deserialize, then project the first interface's defaulted fields.
            // serde_json::Error is not PartialEq, so a failing row would discard it.
            run = |s| {
                serde_json::from_str::<MachineInfinibandStatusObservation>(s)
                    .map(|obs| {
                        let iface = obs.ib_interfaces.first().expect("row supplies interfaces");
                        (iface.fabric_id.is_empty(), iface.associated_pkeys.is_none())
                    })
                    .map_err(drop)
            };
            "interfaces without fabric_id or pkeys default them" {
                r#"{"observed_at": "2025-06-06T19:47:16.597282585Z", "ib_interfaces": [{"lid": 65535, "guid": "1070fd0300bd7574"}, {"lid": 65535, "guid": "1070fd0300bd7575"}]}"# => Yields((true, true)),
            }
        );
    }

    // An empty interface list deserializes cleanly -- older payloads may omit
    // interfaces entirely.
    #[test]
    fn deserialize_legacy_ib_status_empty_interfaces() {
        let obs: MachineInfinibandStatusObservation = serde_json::from_str(
            r#"{"observed_at": "2024-12-18T23:17:57.919166804Z", "ib_interfaces": []}"#,
        )
        .expect("empty interface list should deserialize");
        assert!(obs.ib_interfaces.is_empty());
    }

    #[test]
    fn serialize_ib_status_observation() {
        let obs = MachineInfinibandStatusObservation {
            ib_interfaces: vec![MachineIbInterfaceStatusObservation {
                guid: "Aguid".to_string(),
                lid: 0x10,
                fabric_id: "default".to_string(),
                associated_pkeys: Some([0x13.try_into().unwrap()].into_iter().collect()),
                associated_partition_ids: Some(
                    [uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1200").into()]
                        .into_iter()
                        .collect(),
                ),
            }],
            observed_at: "2025-06-06T19:47:16.597282585Z".parse().unwrap(),
        };
        let serialized = serde_json::to_string(&obs).unwrap();
        assert_eq!(
            serialized,
            r#"{"ib_interfaces":[{"guid":"Aguid","lid":16,"fabric_id":"default","associated_pkeys":["0x13"],"associated_partition_ids":["91609f10-c91d-470d-a260-6293ea0c1200"]}],"observed_at":"2025-06-06T19:47:16.597282585Z"}"#
        );
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(obs, deserialized);
    }

    #[test]
    fn test_ib_config_synced() {
        use carbide_uuid::infiniband::IBPartitionId;

        use crate::instance::config::network::InterfaceFunctionId;

        let partition_id: IBPartitionId =
            uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1200").into();
        let pkey: PartitionKey = 0x13.try_into().unwrap();

        // A single-interface config requesting the partition above.
        let config = || InstanceInfinibandConfig {
            ib_interfaces: vec![
                crate::instance::config::infiniband::InstanceIbInterfaceConfig {
                    function_id: InterfaceFunctionId::Physical {},
                    ib_partition_id: partition_id,
                    pf_guid: None,
                    guid: Some("946dae03006104f8".to_string()),
                    device: "MT2910 Family [ConnectX-7]".to_string(),
                    vendor: None,
                    device_instance: 1,
                },
            ],
        };

        // Observation where the port is down/unobservable (no pkeys/partition ids).
        let unobservable = MachineInfinibandStatusObservation {
            ib_interfaces: vec![MachineIbInterfaceStatusObservation {
                guid: "946dae03006104f8".to_string(),
                lid: 0xffff,
                fabric_id: "".to_string(),
                associated_pkeys: None,
                associated_partition_ids: None,
            }],
            observed_at: chrono::Utc::now(),
        };

        // Observation where the interface is on exactly the requested partition.
        let synced = MachineInfinibandStatusObservation {
            ib_interfaces: vec![MachineIbInterfaceStatusObservation {
                guid: "946dae03006104f8".to_string(),
                lid: 0x10,
                fabric_id: "default".to_string(),
                associated_pkeys: Some([pkey].into_iter().collect()),
                associated_partition_ids: Some([partition_id].into_iter().collect()),
            }],
            observed_at: chrono::Utc::now(),
        };

        // Observation where the interface is observable but sits on a different
        // partition than the config requests, so the config is not synced.
        let other_partition_id: IBPartitionId =
            uuid::uuid!("00000000-0000-0000-0000-0000deadbeef").into();
        let other_pkey: PartitionKey = 0x42.try_into().unwrap();
        let mismatched = MachineInfinibandStatusObservation {
            ib_interfaces: vec![MachineIbInterfaceStatusObservation {
                guid: "946dae03006104f8".to_string(),
                lid: 0x10,
                fabric_id: "default".to_string(),
                associated_pkeys: Some([other_pkey].into_iter().collect()),
                associated_partition_ids: Some([other_partition_id].into_iter().collect()),
            }],
            observed_at: chrono::Utc::now(),
        };

        // ib_config_synced over (observation, config, use_tenant_network). The
        // error variant's `details` are built dynamically, so rather than assert
        // the exact reason we project the result to a comparable summary:
        // - Ok            -> a static "ok" tag, no guids, no substrings
        // - the error variant tag, the affected guids, and whether `details`
        //   carries the expected substrings (only the PortStateUnobservable row
        //   asserts those).
        type Summary = (&'static str, Vec<String>, bool, bool);
        let cfg = config();
        check_cases(
            [
                Case {
                    scenario: "missing observation",
                    input: (None, Some(&cfg), true),
                    expect: Yields(("MissingObservation", Vec::new(), false, false)),
                },
                Case {
                    scenario: "port state unobservable",
                    input: (Some(&unobservable), Some(&cfg), true),
                    expect: Yields((
                        "PortStateUnobservable",
                        vec!["946dae03006104f8".to_string()],
                        true,
                        true,
                    )),
                },
                Case {
                    scenario: "ok when synced",
                    input: (Some(&synced), Some(&cfg), true),
                    expect: Yields(("ok", Vec::new(), false, false)),
                },
                Case {
                    scenario: "configuration mismatch (interface on a different partition)",
                    input: (Some(&mismatched), Some(&cfg), true),
                    expect: Yields(("ConfigurationMismatch", Vec::new(), false, false)),
                },
            ],
            |(observation, config, use_tenant_network)| -> Result<Summary, ()> {
                Ok(
                    match ib_config_synced(observation, config, use_tenant_network) {
                        Ok(()) => ("ok", Vec::new(), false, false),
                        Err(IbConfigNotSyncedReason::MissingObservation { .. }) => {
                            ("MissingObservation", Vec::new(), false, false)
                        }
                        Err(IbConfigNotSyncedReason::ConfigurationMismatch { .. }) => {
                            ("ConfigurationMismatch", Vec::new(), false, false)
                        }
                        Err(IbConfigNotSyncedReason::PortStateUnobservable { guids, details }) => (
                            "PortStateUnobservable",
                            guids,
                            details.contains("946dae03006104f8"),
                            details.contains("missing or incomplete"),
                        ),
                    },
                )
            },
        );
    }
}
