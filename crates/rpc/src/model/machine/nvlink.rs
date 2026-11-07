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

use model::machine::nvlink::{MachineNvLinkGpuStatusObservation, MachineNvLinkStatusObservation};

use crate as rpc;

impl From<MachineNvLinkStatusObservation> for rpc::forge::MachineNvLinkStatusObservation {
    fn from(value: MachineNvLinkStatusObservation) -> Self {
        rpc::forge::MachineNvLinkStatusObservation {
            gpu_status: value
                .nvlink_gpus
                .into_iter()
                .map(rpc::forge::MachineNvLinkGpuStatusObservation::from)
                .collect(),
        }
    }
}

impl From<MachineNvLinkGpuStatusObservation> for rpc::forge::MachineNvLinkGpuStatusObservation {
    fn from(value: MachineNvLinkGpuStatusObservation) -> Self {
        rpc::forge::MachineNvLinkGpuStatusObservation {
            gpu_id: value.gpu_id,
            partition_id: value.partition_id,
            logical_partition_id: value.logical_partition_id,
            device_instance: value.device_instance,
            domain_id: Some(value.domain_id),
            guid: value.guid,
        }
    }
}
