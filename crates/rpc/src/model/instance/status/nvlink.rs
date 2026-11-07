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

use model::instance::status::nvlink::{InstanceNvLinkGpuStatus, InstanceNvLinkStatus};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<InstanceNvLinkStatus> for rpc::InstanceNvLinkStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: InstanceNvLinkStatus) -> Result<Self, Self::Error> {
        let mut gpu_statuses: Vec<rpc::InstanceNvLinkGpuStatus> = Vec::new();
        for gpu in status.nvlink_gpus.iter() {
            let g = rpc::InstanceNvLinkGpuStatus::try_from(gpu.clone())?;
            gpu_statuses.push(g);
        }
        Ok(Self {
            gpu_statuses,
            configs_synced: rpc::SyncState::try_from(status.configs_synced)? as i32,
        })
    }
}

impl TryFrom<InstanceNvLinkGpuStatus> for rpc::InstanceNvLinkGpuStatus {
    type Error = RpcDataConversionError;
    fn try_from(status: InstanceNvLinkGpuStatus) -> Result<Self, Self::Error> {
        Ok(Self {
            logical_partition_id: status.logical_partition_id,
            gpu_guid: Some(status.gpu_guid.clone()),
            domain_id: Some(status.domain_id),
        })
    }
}

impl TryFrom<rpc::InstanceNvLinkGpuStatus> for InstanceNvLinkGpuStatus {
    type Error = RpcDataConversionError;
    fn try_from(status: rpc::InstanceNvLinkGpuStatus) -> Result<Self, Self::Error> {
        Ok(Self {
            logical_partition_id: status.logical_partition_id,
            gpu_guid: status.gpu_guid.unwrap_or_default(),
            domain_id: status.domain_id.unwrap_or_default(),
        })
    }
}
