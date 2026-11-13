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

use model::firmware::{DriveModel, FirmwareComponentType};

use crate as rpc;
use crate::errors::RpcDataConversionError;

/// Converts an API component type and drive model to a component. A drive
/// needs `drive_model`, the drive's Redfish `Model` as site explorer records
/// it, within the limits of [`DriveModel::new`]. Every other type rejects one.
pub fn firmware_component_from_rpc(
    value: i32,
    drive_model: Option<&str>,
) -> Result<FirmwareComponentType, RpcDataConversionError> {
    let value = rpc::forge::HostFirmwareComponentType::try_from(value).map_err(|_| {
        RpcDataConversionError::InvalidArgument(format!(
            "unknown host firmware component type value {value}"
        ))
    })?;

    match (value, drive_model) {
        (rpc::forge::HostFirmwareComponentType::Drive, Some(model)) => DriveModel::new(model)
            .map(|model| FirmwareComponentType::Drive { model })
            .map_err(|error| {
                RpcDataConversionError::InvalidArgument(format!("drive_model {error}"))
            }),
        (rpc::forge::HostFirmwareComponentType::Drive, None) => {
            Err(RpcDataConversionError::InvalidArgument(
                "drive_model is required for a drive component".to_string(),
            ))
        }
        (_, Some(_)) => Err(RpcDataConversionError::InvalidArgument(format!(
            "drive_model is only allowed for a drive component, not {}",
            value.as_str_name()
        ))),
        (value, None) => FirmwareComponentType::try_from(value),
    }
}

impl From<FirmwareComponentType> for rpc::forge::HostFirmwareComponentRef {
    fn from(value: FirmwareComponentType) -> Self {
        let drive_model = match value {
            FirmwareComponentType::Drive { model } => Some(model.as_str().to_string()),
            _ => None,
        };
        Self {
            r#type: rpc::forge::HostFirmwareComponentType::from(value) as i32,
            drive_model,
        }
    }
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
            rpc::forge::HostFirmwareComponentType::Drive => {
                Err(RpcDataConversionError::InvalidArgument(
                    "a drive component needs its drive_model".to_string(),
                ))
            }
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
            FirmwareComponentType::Drive { .. } => rpc::forge::HostFirmwareComponentType::Drive,
            FirmwareComponentType::Unknown => rpc::forge::HostFirmwareComponentType::Unspecified,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use model::firmware::MAX_DRIVE_MODEL_LEN;
    use rpc::forge::HostFirmwareComponentType as Rpc;

    use super::*;

    fn drive(model: &str) -> FirmwareComponentType {
        FirmwareComponentType::Drive {
            model: DriveModel::new(model).unwrap(),
        }
    }

    #[test]
    fn firmware_component_from_rpc_requires_drive_model_only_for_drives() {
        let longest_model = "M".repeat(MAX_DRIVE_MODEL_LEN);
        let too_long_model = "M".repeat(MAX_DRIVE_MODEL_LEN + 1);
        scenarios!(
            run = |(value, model): (i32, Option<&str>)| firmware_component_from_rpc(value, model)
                .map_err(drop);
            "component types without a model" {
                (Rpc::CombinedBmcUefi as i32, None) => Yields(FirmwareComponentType::CombinedBmcUefi),
                (Rpc::Cx7 as i32, None) => Yields(FirmwareComponentType::Cx7),
            }
            "a drive keeps its model exactly" {
                (Rpc::Drive as i32, Some("Dell Ent NVMe CM6 RI 1.92TB")) =>
                    Yields(drive("Dell Ent NVMe CM6 RI 1.92TB")),
                (Rpc::Drive as i32, Some(longest_model.as_str())) => Yields(drive(&longest_model)),
            }
            "invalid type or model" {
                (Rpc::Unspecified as i32, None) => Fails,
                (i32::MAX, None) => Fails,
                (Rpc::Drive as i32, None) => Fails,
                (Rpc::Drive as i32, Some("")) => Fails,
                (Rpc::Drive as i32, Some(" ST24000NM005H")) => Fails,
                (Rpc::Drive as i32, Some(too_long_model.as_str())) => Fails,
                (Rpc::Bmc as i32, Some("ST24000NM005H")) => Fails,
            }
        );
    }

    #[test]
    fn component_ref_round_trips_drive_model() {
        for component in [FirmwareComponentType::Uefi, drive("ST24000NM005H")] {
            let wire = rpc::forge::HostFirmwareComponentRef::from(component);
            assert_eq!(
                firmware_component_from_rpc(wire.r#type, wire.drive_model.as_deref()).unwrap(),
                component,
                "{wire:?}"
            );
        }
    }
}
