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

//use std::collections::HashSet;

use carbide_uuid::nvlink::NvLinkLogicalPartitionId;
use serde::{Deserialize, Serialize};

use crate::ConfigValidationError;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceNvLinkConfig {
    /// Configures how GPUs are set up
    pub gpu_configs: Vec<InstanceNvLinkGpuConfig>,
}

impl InstanceNvLinkConfig {
    /// Validates the nvlink configuration
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        Ok(())
    }

    pub fn verify_update_allowed_to(
        &self,
        _new_config: &Self,
    ) -> Result<(), ConfigValidationError> {
        Ok(())
    }

    pub fn is_nvlink_config_update_requested(&self, new_config: &Self) -> bool {
        self != new_config
    }
}

/// The configuration that a customer desires for an instances gpus
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceNvLinkGpuConfig {
    // The logical nvlink partition this gpu is attached to
    pub logical_partition_id: Option<NvLinkLogicalPartitionId>,
    /// gpu module id
    pub device_instance: u32,
}
