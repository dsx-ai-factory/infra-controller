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

use model::instance::config::nvlink::{InstanceNvLinkConfig, InstanceNvLinkGpuConfig};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<rpc::InstanceNvLinkConfig> for InstanceNvLinkConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::InstanceNvLinkConfig) -> Result<Self, Self::Error> {
        let mut gpu_configs = Vec::with_capacity(config.gpu_configs.len());
        for gpu in config.gpu_configs.into_iter() {
            gpu_configs.push(InstanceNvLinkGpuConfig {
                logical_partition_id: gpu.logical_partition_id,
                device_instance: gpu.device_instance,
            });
        }

        Ok(Self { gpu_configs })
    }
}

impl TryFrom<InstanceNvLinkConfig> for rpc::InstanceNvLinkConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: InstanceNvLinkConfig) -> Result<rpc::InstanceNvLinkConfig, Self::Error> {
        let mut gpu_configs = Vec::with_capacity(config.gpu_configs.len());
        for gpu in config.gpu_configs.into_iter() {
            gpu_configs.push(rpc::InstanceNvLinkGpuConfig {
                device_instance: gpu.device_instance,
                logical_partition_id: gpu.logical_partition_id,
            });
        }

        Ok(rpc::InstanceNvLinkConfig { gpu_configs })
    }
}
