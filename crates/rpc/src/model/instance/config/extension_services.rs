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

use carbide_uuid::extension_service::ExtensionServiceId;
use config_version::ConfigVersion;
use model::instance::config::extension_services::{
    InstanceExtensionServiceConfig, InstanceExtensionServicesConfig,
};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<rpc::InstanceDpuExtensionServiceConfig> for InstanceExtensionServiceConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::InstanceDpuExtensionServiceConfig) -> Result<Self, Self::Error> {
        let service_id = config
            .service_id
            .parse::<ExtensionServiceId>()
            .map_err(|e| {
                RpcDataConversionError::InvalidUuid("ExtensionServiceId", e.to_string())
            })?;

        let version = config.version.parse::<ConfigVersion>().map_err(|e| {
            RpcDataConversionError::InvalidConfigVersion(format!(
                "Failed to parse version as ConfigVersion: {}",
                e
            ))
        })?;

        Ok(InstanceExtensionServiceConfig {
            dpu_target: None,
            service_id,
            version,
            removed: None,
        })
    }
}

impl From<InstanceExtensionServiceConfig> for rpc::InstanceDpuExtensionServiceConfig {
    fn from(config: InstanceExtensionServiceConfig) -> Self {
        rpc::InstanceDpuExtensionServiceConfig {
            service_id: config.service_id.into(),
            version: config.version.to_string(),
        }
    }
}

impl TryFrom<rpc::InstanceDpuExtensionServicesConfig> for InstanceExtensionServicesConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::InstanceDpuExtensionServicesConfig) -> Result<Self, Self::Error> {
        let service_configs = config
            .service_configs
            .into_iter()
            .map(InstanceExtensionServiceConfig::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(InstanceExtensionServicesConfig { service_configs })
    }
}

impl TryFrom<InstanceExtensionServicesConfig> for rpc::InstanceDpuExtensionServicesConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: InstanceExtensionServicesConfig) -> Result<Self, Self::Error> {
        Ok(rpc::InstanceDpuExtensionServicesConfig {
            service_configs: config
                .service_configs
                .into_iter()
                .map(|config| config.into())
                .collect(),
        })
    }
}
