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

use config_version::Versioned;
use serde::{Deserialize, Serialize};

use crate::instance::config::infiniband::InstanceInfinibandConfig;
use crate::instance::status::SyncState;
use crate::machine::infiniband::{MachineInfinibandStatusObservation, ib_config_synced};

/// Status of the infiniband subsystem of an instance
///
/// The status report is only valid against one particular version of
/// [InstanceInterfaceConfig](crate::model::instance::config::network::InstanceInterfaceConfig). It can not be interpreted without it, since
/// e.g. the amount and configuration of ib interfaces can change between
/// configs.
///
/// Since the user can change the configuration at any point in time for an instance,
/// we can not directly store this status in the database - it might not match
/// the newest config anymore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceInfinibandStatus {
    /// Status for each configured interface
    ///
    /// Each entry in this status array maps to its corresponding entry in the
    /// Config section. E.g. `instance.status.infiniband.ib_interface_status[1]`
    /// would map to `instance.config.infiniband.ib_interface_configs[1]`.
    pub ib_interfaces: Vec<InstanceIbInterfaceStatus>,

    /// Whether all desired network changes that the user has applied have taken effect
    /// This includes:
    /// - Whether `InstanceNetworkConfig` is of exactly the same version as the
    ///   version the user desires.
    /// - Whether the version of each security policy that is either directly referenced
    ///   as part of an `InstanceInterfaceConfig` or indirectly referenced via the
    ///   the security policies that are applied to the VPC or NetworkSegment
    ///   is exactly the same version as the version the user desires.
    ///
    /// Note for the implementation: We need to monitor all these config versions
    /// on the feedback path from DPU to carbide in order to know whether the
    /// changes have indeed taken effect.
    /// TODO: Do we also want to show all applied versions here, or just track them
    /// internally? Probably not helpful for tenants at all - but it could be helpful
    /// for the Forge operating team to debug settings that to do do not go in-sync
    /// without having to attach to the database.
    pub configs_synced: SyncState,
}

impl InstanceInfinibandStatus {
    /// Derives an Instances infiniband status from the users desired config
    /// and status that we observed from the infiniband subsystem.
    ///
    /// This mechanism guarantees that the status we return to the user always
    /// matches the latest `Config` set by the user. We can not directly
    /// forwarding the last observed status without taking `Config` into account,
    /// because the observation might have been related to a different config,
    /// and the interfaces therefore won't match.
    pub fn from_config_and_observation(
        config: Versioned<&InstanceInfinibandConfig>,
        observations: Option<&MachineInfinibandStatusObservation>,
    ) -> Self {
        if config.ib_interfaces.is_empty() {
            return Self {
                ib_interfaces: Vec::new(),
                configs_synced: SyncState::Synced,
            };
        }

        let ib_config_sync_state = ib_config_synced(observations, Some(config.value), true);

        // Config version check is not used fo Infiniband Instance configuration.
        // There is no asynchronous process. It's actually the state handler itself which checks the
        // desired config, applies it against UFM, and then observes the status reports from UFM.
        // No discrepancy between instance configuration and observed statuses.
        let observations = match observations {
            Some(observations) => observations,
            None => return Self::unsynchronized_for_config(&config),
        };

        let ib_interfaces = config
            .ib_interfaces
            .iter()
            .map(|config| {
                // TODO: This isn't super efficient. We could do it better if there would be a guarantee
                // that interfaces in the observation are in the same order as in the config.
                // But it isn't obvious at the moment whether we can achieve this while
                // not mixing up order for users.
                let observation = observations
                    .ib_interfaces
                    .iter()
                    .find(|iface| Some(iface.guid.clone()) == config.guid);
                match observation {
                    Some(observation) => InstanceIbInterfaceStatus {
                        pf_guid: config.pf_guid.clone(),
                        guid: Some(observation.guid.clone()),
                        lid: observation.lid as u32,
                    },
                    None => {
                        tracing::error!(
                            function_id = ?config.function_id,
                            ?config,
                            ?observation,
                            "Could not find matching status for interface",
                        );
                        // TODO: Might also be worthwhile to return an error?
                        // On the other hand the error is also visible via returning no IPs - and at least we don't break
                        // all other interfaces this way
                        InstanceIbInterfaceStatus {
                            pf_guid: None,
                            guid: None,
                            lid: 0,
                        }
                    }
                }
            })
            .collect();

        Self {
            ib_interfaces,
            configs_synced: if ib_config_sync_state.is_ok() {
                SyncState::Synced
            } else {
                SyncState::Pending
            },
        }
    }

    /// Creates a `InstanceNetworkStatus` report for cases there the configuration
    /// has not been synchronized.
    ///
    /// This status report will contain an interface for each requested interface,
    /// but all interfaces will have no addresses assigned to them.
    fn unsynchronized_for_config(config: &InstanceInfinibandConfig) -> Self {
        Self {
            ib_interfaces: config
                .ib_interfaces
                .iter()
                .map(|iface| InstanceIbInterfaceStatus {
                    pf_guid: iface.pf_guid.clone(),
                    guid: iface.guid.clone(),
                    lid: 0,
                })
                .collect(),
            configs_synced: SyncState::Pending,
        }
    }
}

/// The network status that was last reported by the infiniband subsystem
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceIbInterfaceStatus {
    /// The GUID of the hardware device that this interface is attached to
    pub pf_guid: Option<String>,
    /// The GUID which has been assigned to this interface
    /// In case the interface is a PF interface, the GUID will be equivalent to
    /// `pf_guid` - which is the GUID that is stored on the hardware device.
    /// For a VF interface, this is a GUID that has been allocated by Forge in order
    /// be used for the VF.
    // Tenants have to configure the VF device on their instances to use this GUID.
    pub guid: Option<String>,
    /// The local id of this IB interface
    /// If interface configuration has not been completed, the value is 0
    pub lid: u32,
}
