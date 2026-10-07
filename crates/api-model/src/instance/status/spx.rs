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

use std::collections::HashMap;

use carbide_uuid::spx::SpxPartitionId;
use config_version::Versioned;
use serde::{Deserialize, Serialize};

use crate::instance::config::spx::{InstanceSpxConfig, SpxAttachmentType};
use crate::instance::status::SyncState;
use crate::machine::spx::MachineSpxStatusObservation;

#[derive(Clone, Debug)]
pub struct InstanceSpxStatus {
    /// each entry here maps to the corresponding entry in the config Vec<InstanceSpxConfig>
    pub spx_attachments: Vec<InstanceSpxAttachmentStatus>,
    /// similar to InstanceNetworkStatus
    pub configs_synced: SyncState,
}

impl InstanceSpxStatus {
    pub fn from_config_and_observation(
        config: Versioned<&InstanceSpxConfig>,
        observations: Option<&MachineSpxStatusObservation>,
    ) -> Self {
        if config.spx_attachments.is_empty() {
            return Self {
                spx_attachments: Vec::new(),
                configs_synced: SyncState::Synced,
            };
        }

        let Some(observations) = observations else {
            return Self::unsynchronized_for_config(&config);
        };

        let mut configs_synced = SyncState::Synced;

        let mut spx_attachments: Vec<InstanceSpxAttachmentStatus> =
            Vec::with_capacity(config.spx_attachments.len());
        let obs_by_mac_address: HashMap<_, _> = observations
            .spx_attachments
            .iter()
            .map(|obs| (obs.mac_address.to_string(), obs))
            .collect();
        for cfg in &config.spx_attachments {
            let mac_addr = cfg.mac_address.as_deref().unwrap_or_default();
            let status = match obs_by_mac_address.get(mac_addr) {
                Some(obs) => {
                    if cfg.spx_partition_id != obs.partition_id.unwrap_or_default() {
                        configs_synced = SyncState::Pending;
                    }
                    InstanceSpxAttachmentStatus {
                        mac_address: mac_addr.to_string(),
                        attachment_type: cfg.attachment_type.clone(),
                        spx_partition_id: cfg.spx_partition_id,
                        attachment_vf: cfg.attachment_vf.as_ref().map(|vf| SpxAttachmentVfStatus {
                            vf_index: vf.vf_index,
                        }),
                        attachment_ovs: cfg.attachment_ovs.as_ref().map(|ovs| {
                            SpxAttachmentOvsStatus {
                                bridge_name: ovs.bridge_name.clone(),
                                ovn_network_name: ovs.ovn_network_name.clone(),
                            }
                        }),
                    }
                }
                None => {
                    tracing::error!(
                        device_instance = cfg.device_instance,
                        "could not find matching status spx attachment",
                    );
                    configs_synced = SyncState::Pending;
                    InstanceSpxAttachmentStatus {
                        mac_address: mac_addr.to_string(),
                        attachment_type: cfg.attachment_type.clone(),
                        spx_partition_id: cfg.spx_partition_id,
                        attachment_vf: cfg.attachment_vf.as_ref().map(|vf| SpxAttachmentVfStatus {
                            vf_index: vf.vf_index,
                        }),
                        attachment_ovs: cfg.attachment_ovs.as_ref().map(|ovs| {
                            SpxAttachmentOvsStatus {
                                bridge_name: ovs.bridge_name.clone(),
                                ovn_network_name: ovs.ovn_network_name.clone(),
                            }
                        }),
                    }
                }
            };
            spx_attachments.push(status);
        }
        Self {
            spx_attachments,
            configs_synced,
        }
    }

    fn unsynchronized_for_config(config: &InstanceSpxConfig) -> Self {
        Self {
            spx_attachments: config
                .spx_attachments
                .iter()
                .map(|cfg| InstanceSpxAttachmentStatus {
                    mac_address: cfg.mac_address.as_deref().unwrap_or_default().to_string(),
                    attachment_type: SpxAttachmentType::Physical,
                    spx_partition_id: SpxPartitionId::default(),
                    attachment_vf: None,
                    attachment_ovs: None,
                })
                .collect(),
            configs_synced: SyncState::Pending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceSpxAttachmentStatus {
    pub mac_address: String,
    pub attachment_type: SpxAttachmentType,
    pub spx_partition_id: SpxPartitionId,
    pub attachment_vf: Option<SpxAttachmentVfStatus>,
    pub attachment_ovs: Option<SpxAttachmentOvsStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpxAttachmentVfStatus {
    pub vf_index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpxAttachmentOvsStatus {
    pub bridge_name: String,
    pub ovn_network_name: Option<String>,
}
