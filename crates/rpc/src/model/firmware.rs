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

use model::firmware::FirmwareComponentType;

use crate as rpc;
use crate::errors::RpcDataConversionError;

pub fn firmware_component_type_from_rpc(
    value: i32,
) -> Result<FirmwareComponentType, RpcDataConversionError> {
    let value = rpc::forge::HostFirmwareComponentType::try_from(value).map_err(|_| {
        RpcDataConversionError::InvalidArgument(format!(
            "unknown host firmware component type value {value}"
        ))
    })?;

    FirmwareComponentType::try_from(value)
}

impl TryFrom<rpc::forge::HostFirmwareComponentType> for FirmwareComponentType {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::HostFirmwareComponentType) -> Result<Self, Self::Error> {
        match value {
            rpc::forge::HostFirmwareComponentType::Unspecified => Err(
                RpcDataConversionError::InvalidArgument("component type is required".to_string()),
            ),
            rpc::forge::HostFirmwareComponentType::Bmc => Ok(FirmwareComponentType::Bmc),
            rpc::forge::HostFirmwareComponentType::Cec => Ok(FirmwareComponentType::Cec),
            rpc::forge::HostFirmwareComponentType::Uefi => Ok(FirmwareComponentType::Uefi),
            rpc::forge::HostFirmwareComponentType::Nic => Ok(FirmwareComponentType::Nic),
            rpc::forge::HostFirmwareComponentType::CpldMb => Ok(FirmwareComponentType::CpldMb),
            rpc::forge::HostFirmwareComponentType::CpldPdb => Ok(FirmwareComponentType::CpldPdb),
            rpc::forge::HostFirmwareComponentType::HgxBmc => Ok(FirmwareComponentType::HGXBmc),
            rpc::forge::HostFirmwareComponentType::CombinedBmcUefi => {
                Ok(FirmwareComponentType::CombinedBmcUefi)
            }
            rpc::forge::HostFirmwareComponentType::Gpu => Ok(FirmwareComponentType::Gpu),
            rpc::forge::HostFirmwareComponentType::Cx7 => Ok(FirmwareComponentType::Cx7),
        }
    }
}

impl From<FirmwareComponentType> for rpc::forge::HostFirmwareComponentType {
    fn from(value: FirmwareComponentType) -> Self {
        match value {
            FirmwareComponentType::Bmc => rpc::forge::HostFirmwareComponentType::Bmc,
            FirmwareComponentType::Cec => rpc::forge::HostFirmwareComponentType::Cec,
            FirmwareComponentType::Uefi => rpc::forge::HostFirmwareComponentType::Uefi,
            FirmwareComponentType::Nic => rpc::forge::HostFirmwareComponentType::Nic,
            FirmwareComponentType::CpldMb => rpc::forge::HostFirmwareComponentType::CpldMb,
            FirmwareComponentType::CpldPdb => rpc::forge::HostFirmwareComponentType::CpldPdb,
            FirmwareComponentType::HGXBmc => rpc::forge::HostFirmwareComponentType::HgxBmc,
            FirmwareComponentType::CombinedBmcUefi => {
                rpc::forge::HostFirmwareComponentType::CombinedBmcUefi
            }
            FirmwareComponentType::Gpu => rpc::forge::HostFirmwareComponentType::Gpu,
            FirmwareComponentType::Cx7 => rpc::forge::HostFirmwareComponentType::Cx7,
            FirmwareComponentType::Unknown => rpc::forge::HostFirmwareComponentType::Unspecified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_component_type_from_rpc_rejects_unspecified_and_unknown_values() {
        assert_eq!(
            firmware_component_type_from_rpc(
                rpc::forge::HostFirmwareComponentType::CombinedBmcUefi as i32
            )
            .unwrap(),
            FirmwareComponentType::CombinedBmcUefi
        );
        assert_eq!(
            firmware_component_type_from_rpc(rpc::forge::HostFirmwareComponentType::Cx7 as i32)
                .unwrap(),
            FirmwareComponentType::Cx7
        );
        assert!(
            firmware_component_type_from_rpc(
                rpc::forge::HostFirmwareComponentType::Unspecified as i32
            )
            .is_err()
        );
        assert!(firmware_component_type_from_rpc(i32::MAX).is_err());
    }
}
