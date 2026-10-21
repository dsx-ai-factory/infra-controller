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

pub mod extension_services;
pub mod infiniband;
pub mod network;
pub mod nvlink;
pub mod spx;
pub mod tenant_config;

use carbide_uuid::network_security_group::NetworkSecurityGroupIdParseError;
use model::instance::config::InstanceConfig;
use model::instance::config::extension_services::{
    InstanceExtensionServiceConfig, InstanceExtensionServicesConfig,
};
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::InstanceNetworkConfig;
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::config::spx::InstanceSpxConfig;
use model::instance::config::tenant_config::TenantConfig;
use model::os::OperatingSystem;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl TryFrom<rpc::InstanceConfig> for InstanceConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::InstanceConfig) -> Result<Self, Self::Error> {
        // VPC selections need handler context to validate and reconcile.
        // Generic conversion must not silently turn them into attachments.
        if config.dpu_extension_services.is_some() {
            return Err(RpcDataConversionError::InvalidArgument(
                "instance extension services require caller-aware conversion".to_string(),
            ));
        }

        let os: OperatingSystem = OperatingSystem::try_from(config.os.ok_or(
            RpcDataConversionError::MissingArgument("InstanceConfig::os"),
        )?)?;

        let tenant = TenantConfig::try_from(config.tenant.ok_or(
            RpcDataConversionError::MissingArgument("InstanceConfig::tenant"),
        )?)?;

        // Network config is optional (for zero-dpu hosts).
        let network = config
            .network
            .map(InstanceNetworkConfig::try_from)
            .transpose()?
            .unwrap_or(InstanceNetworkConfig::default());

        // Infiniband config is optional
        let infiniband = config
            .infiniband
            .map(InstanceInfinibandConfig::try_from)
            .transpose()?
            .unwrap_or(InstanceInfinibandConfig::default());

        let extension_services = InstanceExtensionServicesConfig::default();

        // NvLink config is optional
        let nvlink = config
            .nvlink
            .map(InstanceNvLinkConfig::try_from)
            .transpose()?
            .unwrap_or(InstanceNvLinkConfig::default());

        // Spx config is optional
        let spxconfig = config
            .spxconfig
            .map(InstanceSpxConfig::try_from)
            .transpose()?
            .unwrap_or(InstanceSpxConfig::default());

        Ok(InstanceConfig {
            tenant,
            os,
            network,
            infiniband,
            network_security_group_id: config
                .network_security_group_id
                .map(|nsg| nsg.parse())
                .transpose()
                .map_err(|e: NetworkSecurityGroupIdParseError| {
                    RpcDataConversionError::InvalidNetworkSecurityGroupId(e.value())
                })?,
            extension_services,
            nvlink,
            spxconfig,
            power_profile: config.power_profile,
        })
    }
}

impl TryFrom<InstanceConfig> for rpc::InstanceConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: InstanceConfig) -> Result<rpc::InstanceConfig, Self::Error> {
        let service_interfaces = config.network.service_interfaces.clone();
        let tenant = rpc::forge::TenantConfig::try_from(config.tenant)?;
        let os = rpc::forge::InstanceOperatingSystemConfig::try_from(config.os)?;
        let network = rpc::InstanceNetworkConfig::try_from(config.network)?;
        let infiniband = rpc::InstanceInfinibandConfig::try_from(config.infiniband)?;
        let infiniband = match infiniband.ib_interfaces.is_empty() {
            true => None,
            false => Some(infiniband),
        };
        let nvlink = rpc::forge::InstanceNvLinkConfig::try_from(config.nvlink)?;
        let nvlink = match nvlink.gpu_configs.is_empty() {
            true => None,
            false => Some(nvlink),
        };
        let spxconfig = rpc::forge::InstanceSpxConfig::try_from(config.spxconfig)?;
        let spxconfig = match spxconfig.spx_attachments.is_empty() {
            true => None,
            false => Some(spxconfig),
        };

        // We only show user active extension services, and track terminating services internally.
        let active_extension_services: Vec<InstanceExtensionServiceConfig> = config
            .extension_services
            .active_services()
            .into_iter()
            .cloned()
            .collect();
        let extension_services = match active_extension_services.is_empty() {
            true => None,
            false => Some(extension_services::to_rpc_config(
                InstanceExtensionServicesConfig {
                    service_configs: active_extension_services,
                },
                &service_interfaces,
            )?),
        };

        Ok(rpc::InstanceConfig {
            tenant: Some(tenant),
            os: Some(os),
            network: Some(network),
            infiniband,
            network_security_group_id: config.network_security_group_id.map(|i| i.to_string()),
            dpu_extension_services: extension_services,
            nvlink,
            spxconfig,
            power_profile: config.power_profile,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_uuid::extension_service::ExtensionServiceId;
    use carbide_uuid::vpc::VpcId;
    use config_version::ConfigVersion;
    use model::instance::config::tenant_config::TenantConfig;
    use model::os::{OperatingSystem, OperatingSystemVariant};
    use model::tenant::TenantOrganizationId;
    use uuid::Uuid;

    use super::*;

    /// Verifies generic config conversion cannot bypass the request DTO that
    /// retains service VPC selections for handler validation.
    #[test]
    fn embedded_extension_services_require_caller_aware_conversion() {
        // Include a meaningful VPC selection that the persisted attachment type
        // cannot represent by itself.
        let config = rpc::InstanceConfig {
            dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: ExtensionServiceId::new().to_string(),
                    version: ConfigVersion::initial().to_string(),
                    service_vpc_ids: vec![VpcId::new()],
                }],
            }),
            ..Default::default()
        };

        // Conversion must fail before any selection can be discarded.
        assert!(matches!(
            InstanceConfig::try_from(config),
            Err(RpcDataConversionError::InvalidArgument(message))
                if message == "instance extension services require caller-aware conversion"
        ));
    }

    #[test]
    fn power_profile_round_trips() {
        let config = InstanceConfig {
            tenant: TenantConfig {
                tenant_organization_id: TenantOrganizationId::try_from("TenantA".to_string())
                    .unwrap(),
                tenant_keyset_ids: Vec::new(),
                hostname: None,
            },
            os: OperatingSystem {
                variant: OperatingSystemVariant::OsImage(Uuid::nil()),
                user_data: None,
                phone_home_enabled: false,
                run_provisioning_instructions_on_every_boot: false,
            },
            network: InstanceNetworkConfig::default(),
            infiniband: InstanceInfinibandConfig::default(),
            network_security_group_id: None,
            extension_services: InstanceExtensionServicesConfig::default(),
            nvlink: InstanceNvLinkConfig::default(),
            spxconfig: InstanceSpxConfig::default(),
            power_profile: Some("balanced".to_string()),
        };

        let rpc_config = rpc::InstanceConfig::try_from(config).unwrap();
        assert_eq!(rpc_config.power_profile.as_deref(), Some("balanced"));

        let config = InstanceConfig::try_from(rpc_config).unwrap();
        assert_eq!(config.power_profile.as_deref(), Some("balanced"));
    }
}
