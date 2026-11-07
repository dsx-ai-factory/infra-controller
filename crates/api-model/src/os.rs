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

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ConfigValidationError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineIpxe {
    /// The iPXE script which is booted into
    pub ipxe_script: String,
}

impl InlineIpxe {
    /// Validates the operating system
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.ipxe_script.trim().is_empty() {
            return Err(ConfigValidationError::invalid_value(
                "InlineIpxe::ipxe_script is empty",
            ));
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperatingSystemVariant {
    /// An operating system that is booted into via iPXE (inline script)
    Ipxe(InlineIpxe),
    /// An operating system that references a qcow image
    OsImage(Uuid),
    /// Reference to any operating system definition by ID (any variant except OS image).
    /// On read, the actual type (iPXE / ipxe_os_definition) is resolved from the row.
    OperatingSystemId(Uuid),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatingSystem {
    /// cloud-init user data for any OS variant, preferred
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,
    /// The specific OS variant
    pub variant: OperatingSystemVariant,

    /// If this flag is set to `true` the instance will not transition to a Ready state until
    /// InstancePhoneHomeLastContact is updated
    #[serde(default)]
    pub phone_home_enabled: bool,

    /// If this flag is set to `true`, the instance will run the provisioning instructions
    /// that are specified by the OS on every reboot attempt.
    /// Depending on the type of provisioning instructions, this might
    /// lead the instance to reinstall itself on every reboot.
    ///
    /// E.g. if the instance uses an iPXE script as OS and the iPXE scripts contains
    /// instructions for installing on a local disk, the installation would be repeated
    /// on the reboot.
    ///
    /// If the flag is set to `false` or not specified, Forge will only provide
    /// iPXE instructions that are defined by the OS definition on the first boot attempt.
    /// For every subsequent boot, the instance will use the default boot action - which
    /// is usually to boot from the hard drive.
    ///
    /// If the provisioning instructions should only be used on specific reboots
    /// in order to trigger reinstallation, tenants can use the `InvokeInstancePower`
    /// API to reboot instances with the `boot_with_custom_ipxe` parameter set to
    /// `true`.
    #[serde(default)]
    pub run_provisioning_instructions_on_every_boot: bool,
}

impl OperatingSystem {
    /// Validates the operating system
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        match &self.variant {
            OperatingSystemVariant::Ipxe(ipxe) => ipxe.validate(),
            OperatingSystemVariant::OsImage(_id) => Ok(()),
            OperatingSystemVariant::OperatingSystemId(_id) => Ok(()),
        }
    }

    pub fn verify_update_allowed_to(
        &self,
        _new_config: &Self,
    ) -> Result<(), ConfigValidationError> {
        Ok(())
    }
}
