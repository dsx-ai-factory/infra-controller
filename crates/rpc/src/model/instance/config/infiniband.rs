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

use model::instance::config::infiniband::{InstanceIbInterfaceConfig, InstanceInfinibandConfig};
use model::instance::config::network::{InterfaceFunctionId, InterfaceFunctionType};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<rpc::InstanceInfinibandConfig> for InstanceInfinibandConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::InstanceInfinibandConfig) -> Result<Self, Self::Error> {
        // try_from for ib_interfaces:
        let mut assigned_vfs: u8 = 0;
        let mut ib_interfaces = Vec::with_capacity(config.ib_interfaces.len());
        for iface in config.ib_interfaces.into_iter() {
            let rpc_iface_type = rpc::InterfaceFunctionType::try_from(iface.function_type)
                .map_err(|_| {
                    RpcDataConversionError::InvalidInterfaceFunctionType(iface.function_type)
                })?;
            let iface_type = InterfaceFunctionType::try_from(rpc_iface_type).map_err(|_| {
                RpcDataConversionError::InvalidInterfaceFunctionType(iface.function_type)
            })?;

            let function_id = match iface_type {
                InterfaceFunctionType::Physical => InterfaceFunctionId::Physical {},
                InterfaceFunctionType::Virtual => {
                    let id = assigned_vfs;
                    assigned_vfs = assigned_vfs.saturating_add(1);
                    InterfaceFunctionId::Virtual { id }
                }
            };

            let ib_partition_id =
                iface
                    .ib_partition_id
                    .ok_or(RpcDataConversionError::MissingArgument(
                        "InstanceIbInterfaceConfig::ib_partition_id",
                    ))?;

            ib_interfaces.push(InstanceIbInterfaceConfig {
                function_id,
                ib_partition_id,
                pf_guid: None,
                guid: None,
                device: iface.device,
                vendor: iface.vendor,
                device_instance: iface.device_instance,
            });
        }

        Ok(Self { ib_interfaces })
    }
}

impl TryFrom<InstanceInfinibandConfig> for rpc::InstanceInfinibandConfig {
    type Error = RpcDataConversionError;

    fn try_from(
        config: InstanceInfinibandConfig,
    ) -> Result<rpc::InstanceInfinibandConfig, Self::Error> {
        let mut ib_interfaces = Vec::with_capacity(config.ib_interfaces.len());
        for iface in config.ib_interfaces.into_iter() {
            let function_type = iface.function_id.function_type();

            ib_interfaces.push(rpc::InstanceIbInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::from(function_type) as i32,
                virtual_function_id: None,
                ib_partition_id: Some(iface.ib_partition_id),
                device: iface.device,
                vendor: iface.vendor,
                device_instance: iface.device_instance,
            });
        }

        Ok(rpc::InstanceInfinibandConfig { ib_interfaces })
    }
}
