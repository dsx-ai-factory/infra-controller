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

use carbide_uuid::spx::SpxPartitionId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use crate::StatusValidationError;
use crate::instance::config::spx::{InstanceSpxConfig, SpxAttachmentType};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MachineSpxStatusObservation {
    /// Observed status for each configured interface
    #[serde(default)]
    pub spx_attachments: Vec<MachineSpxAttachmentStatusObservation>,
    pub observed_at: DateTime<Utc>,
}

impl MachineSpxStatusObservation {
    pub fn validate(&self) -> Result<(), StatusValidationError> {
        Ok(())
    }
}

impl Default for MachineSpxStatusObservation {
    fn default() -> Self {
        Self {
            spx_attachments: Vec::new(),
            observed_at: Utc::now(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MachineSpxAttachmentStatusObservation {
    pub mac_address: MacAddress,
    pub partition_id: Option<SpxPartitionId>,
    pub config_version: Option<ConfigVersion>,
    pub attachment_type: Option<SpxAttachmentType>,
    pub virtual_function_id: Option<u32>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SpxConfigNotSyncedReason(pub String);

pub fn spx_config_synced(
    observation: Option<&MachineSpxStatusObservation>,
    config: Option<&InstanceSpxConfig>,
) -> Result<(), SpxConfigNotSyncedReason> {
    let Some(config) = config.as_ref() else {
        return Ok(());
    };
    if config.spx_attachments.is_empty() {
        return Ok(());
    }

    let Some(observation) = observation.as_ref() else {
        return Err(SpxConfigNotSyncedReason("Due to missing SPX status observation, it can't be verified whether the SPX config is applied".to_string()));
    };

    for conf_att in config.spx_attachments.iter() {
        let Some(obs) = observation.spx_attachments.iter().find(|obs_att| {
            conf_att.mac_address.as_deref().unwrap_or_default() == obs_att.mac_address.to_string()
        }) else {
            tracing::error!(
                device_instance = conf_att.device_instance,
                "could not find matching status instance",
            );
            return Err(SpxConfigNotSyncedReason(
                "No matching SPX status observation found for attachment in config".to_string(),
            ));
        };
        if obs.partition_id.is_none() {
            return Err(SpxConfigNotSyncedReason(
                "SPX partition ID not yet applied".to_string(),
            ));
        }
        if conf_att.spx_partition_id != obs.partition_id.unwrap() {
            return Err(SpxConfigNotSyncedReason(
                "SPX partition ID mismatch between config and observation".to_string(),
            ));
        }
    }
    Ok(())
}
