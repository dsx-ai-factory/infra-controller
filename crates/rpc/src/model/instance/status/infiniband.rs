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

use model::instance::status::infiniband::{InstanceIbInterfaceStatus, InstanceInfinibandStatus};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<InstanceInfinibandStatus> for rpc::InstanceInfinibandStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: InstanceInfinibandStatus) -> Result<Self, Self::Error> {
        let mut ib_interfaces = Vec::with_capacity(status.ib_interfaces.len());
        for iface in status.ib_interfaces {
            ib_interfaces.push(rpc::InstanceIbInterfaceStatus::try_from(iface)?);
        }
        Ok(rpc::InstanceInfinibandStatus {
            ib_interfaces,
            configs_synced: rpc::SyncState::try_from(status.configs_synced)? as i32,
        })
    }
}

impl TryFrom<InstanceIbInterfaceStatus> for rpc::InstanceIbInterfaceStatus {
    type Error = RpcDataConversionError;
    fn try_from(status: InstanceIbInterfaceStatus) -> Result<Self, Self::Error> {
        Ok(Self {
            pf_guid: status.pf_guid.clone(),
            guid: status.guid.clone(),
            lid: status.lid,
        })
    }
}

impl TryFrom<rpc::InstanceIbInterfaceStatus> for InstanceIbInterfaceStatus {
    type Error = RpcDataConversionError;
    fn try_from(status: rpc::InstanceIbInterfaceStatus) -> Result<Self, Self::Error> {
        Ok(Self {
            pf_guid: status.pf_guid.clone(),
            guid: status.guid.clone(),
            lid: status.lid,
        })
    }
}
