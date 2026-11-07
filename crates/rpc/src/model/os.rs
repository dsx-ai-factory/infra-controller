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

use model::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
use uuid::Uuid;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl TryFrom<rpc::forge::InlineIpxe> for InlineIpxe {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::forge::InlineIpxe) -> Result<Self, Self::Error> {
        Ok(Self {
            ipxe_script: config.ipxe_script,
        })
    }
}

impl TryFrom<InlineIpxe> for rpc::forge::InlineIpxe {
    type Error = RpcDataConversionError;

    fn try_from(config: InlineIpxe) -> Result<rpc::forge::InlineIpxe, Self::Error> {
        Ok(Self {
            ipxe_script: config.ipxe_script,
        })
    }
}

impl TryFrom<rpc::forge::InstanceOperatingSystemConfig> for OperatingSystem {
    type Error = RpcDataConversionError;

    fn try_from(
        mut config: rpc::forge::InstanceOperatingSystemConfig,
    ) -> Result<Self, Self::Error> {
        let variant = config
            .variant
            .take()
            .ok_or(RpcDataConversionError::MissingArgument(
                "InstanceOperatingSystemConfig::variant",
            ))?;
        let ipxe_user_data = None;
        let variant = match variant {
            rpc::forge::instance_operating_system_config::Variant::Ipxe(ipxe) => {
                OperatingSystemVariant::Ipxe(ipxe.try_into()?)
            }
            rpc::forge::instance_operating_system_config::Variant::OsImageId(id) => {
                OperatingSystemVariant::OsImage(Uuid::try_from(id).map_err(|e| {
                    RpcDataConversionError::InvalidUuid("os_image_id: ", e.to_string())
                })?)
            }
            rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(id) => {
                OperatingSystemVariant::OperatingSystemId(Uuid::from(id))
            }
        };

        Ok(Self {
            variant,
            phone_home_enabled: config.phone_home_enabled,
            run_provisioning_instructions_on_every_boot: config
                .run_provisioning_instructions_on_every_boot,
            user_data: config.user_data.or(ipxe_user_data),
        })
    }
}

impl TryFrom<OperatingSystem> for rpc::forge::InstanceOperatingSystemConfig {
    type Error = RpcDataConversionError;

    fn try_from(
        config: OperatingSystem,
    ) -> Result<rpc::forge::InstanceOperatingSystemConfig, Self::Error> {
        let variant = match config.variant {
            OperatingSystemVariant::Ipxe(ipxe) => {
                let ipxe: rpc::forge::InlineIpxe = ipxe.try_into()?;
                rpc::forge::instance_operating_system_config::Variant::Ipxe(ipxe)
            }
            OperatingSystemVariant::OsImage(id) => {
                rpc::forge::instance_operating_system_config::Variant::OsImageId(id.into())
            }
            OperatingSystemVariant::OperatingSystemId(id) => {
                rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(id.into())
            }
        };

        Ok(Self {
            variant: Some(variant),
            phone_home_enabled: config.phone_home_enabled,
            run_provisioning_instructions_on_every_boot: config
                .run_provisioning_instructions_on_every_boot,
            user_data: config.user_data.clone(),
        })
    }
}
