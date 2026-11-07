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

use carbide_uuid::network_security_group::NetworkSecurityGroupId;
use serde::{Deserialize, Serialize};

use crate::ConfigValidationError;
use crate::instance::config::extension_services::InstanceExtensionServicesConfig;
use crate::instance::config::infiniband::InstanceInfinibandConfig;
use crate::instance::config::network::InstanceNetworkConfig;
use crate::instance::config::nvlink::InstanceNvLinkConfig;
use crate::instance::config::spx::InstanceSpxConfig;
use crate::instance::config::tenant_config::TenantConfig;
use crate::os::OperatingSystem;

/// Instance configuration
///
/// This represents the desired state of an Instance.
/// The instance might not yet be in that state, but work would be underway
/// to get the Instance into this state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    /// Tenant related configuation.
    pub tenant: TenantConfig,

    /// Operating system that is used by the instance
    pub os: OperatingSystem,

    /// Configures instance networking
    #[serde(default)]
    pub network: InstanceNetworkConfig,

    /// Configures instance infiniband
    pub infiniband: InstanceInfinibandConfig,

    /// Configures the security group
    pub network_security_group_id: Option<NetworkSecurityGroupId>,

    /// Configures instance extension services
    #[serde(default)]
    pub extension_services: InstanceExtensionServicesConfig,

    /// configure instance nvlink
    pub nvlink: InstanceNvLinkConfig,

    /// Configures instance spx
    pub spxconfig: InstanceSpxConfig,

    /// Power profile managed by the external power provisioning service.
    pub power_profile: Option<String>,
}

impl InstanceConfig {
    /// Validates the instances configuration
    pub fn validate(
        &self,
        validate_network: bool,
        allow_instance_vf: bool,
    ) -> Result<(), ConfigValidationError> {
        self.tenant.validate()?;

        self.os.validate()?;

        if validate_network {
            self.network.validate(allow_instance_vf)?;
        }

        self.infiniband.validate()?;

        self.nvlink.validate()?;

        Ok(())
    }

    /// Validates whether the configuration of a running instance (`self`) can be updated
    /// to a new configuration
    ///
    /// This check validates that certain unchangeable fields never change. These include
    /// - Tenant ID
    pub fn verify_update_allowed_to(
        &self,
        new_config: &InstanceConfig,
    ) -> Result<(), ConfigValidationError> {
        self.tenant.verify_update_allowed_to(&new_config.tenant)?;

        self.os.verify_update_allowed_to(&new_config.os)?;

        self.network.verify_update_allowed_to(&new_config.network)?;

        self.infiniband
            .verify_update_allowed_to(&new_config.infiniband)?;

        self.extension_services
            .verify_update_allowed_to(&new_config.extension_services)?;
        self.nvlink.verify_update_allowed_to(&new_config.nvlink)?;

        Ok(())
    }
}
