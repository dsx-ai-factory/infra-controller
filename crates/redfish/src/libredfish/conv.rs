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

use bmc_vendor::BMCVendor;
use model::attestation::spdm::{CaCertificate, Evidence};
use model::firmware::FirmwareComponentType;
use model::machine::{MachineLastRebootRequestedMode, PowerState as MachinePowerState};
use model::site_explorer::{
    BlueFieldOperatingMode, BootOption, PCIeDevice, PowerState, SystemStatus,
};

pub trait IntoLibredfish<T> {
    fn into_libredfish(self) -> T;
}

impl IntoLibredfish<libredfish::model::update_service::ComponentType> for FirmwareComponentType {
    fn into_libredfish(self) -> libredfish::model::update_service::ComponentType {
        use libredfish::model::update_service::ComponentType;
        match self {
            FirmwareComponentType::Bmc => ComponentType::BMC,
            FirmwareComponentType::Uefi => ComponentType::UEFI,
            FirmwareComponentType::Cec => ComponentType::Unknown,
            FirmwareComponentType::Nic => ComponentType::Unknown,
            FirmwareComponentType::CpldMb => ComponentType::CPLDMB,
            FirmwareComponentType::CpldPdb => ComponentType::CPLDPDB,
            FirmwareComponentType::HGXBmc => ComponentType::HGXBMC,
            FirmwareComponentType::CombinedBmcUefi => ComponentType::Unknown,
            FirmwareComponentType::Gpu => ComponentType::Unknown,
            FirmwareComponentType::Cx7 => ComponentType::Unknown,
            FirmwareComponentType::Unknown => ComponentType::Unknown,
        }
    }
}

pub trait IntoModel<T> {
    fn into_model(self) -> T;
}

impl IntoModel<PowerState> for libredfish::PowerState {
    fn into_model(self) -> PowerState {
        match self {
            libredfish::PowerState::Off => PowerState::Off,
            libredfish::PowerState::On => PowerState::On,
            libredfish::PowerState::PoweringOff => PowerState::PoweringOff,
            libredfish::PowerState::PoweringOn => PowerState::PoweringOn,
            libredfish::PowerState::Paused => PowerState::Paused,
            libredfish::PowerState::Reset => PowerState::PoweringOn,
            libredfish::PowerState::Unknown => PowerState::Unknown,
        }
    }
}

impl IntoModel<MachinePowerState> for libredfish::PowerState {
    fn into_model(self) -> MachinePowerState {
        match self {
            libredfish::PowerState::Off => MachinePowerState::Off,
            libredfish::PowerState::On => MachinePowerState::On,
            libredfish::PowerState::PoweringOff => MachinePowerState::PoweringOff,
            libredfish::PowerState::PoweringOn => MachinePowerState::PoweringOn,
            libredfish::PowerState::Paused => MachinePowerState::Paused,
            libredfish::PowerState::Reset => MachinePowerState::Reset,
            libredfish::PowerState::Unknown => MachinePowerState::Unknown,
        }
    }
}

impl IntoModel<PCIeDevice> for libredfish::PCIeDevice {
    fn into_model(self) -> PCIeDevice {
        PCIeDevice {
            description: self.description,
            firmware_version: self.firmware_version,
            id: self.id,
            manufacturer: self.manufacturer,
            name: self.name,
            part_number: self.part_number,
            serial_number: self.serial_number,
            status: self.status.map(|s| s.into_model()),
            gpu_vendor: self.gpu_vendor,
        }
    }
}

impl IntoModel<SystemStatus> for libredfish::model::SystemStatus {
    fn into_model(self) -> SystemStatus {
        SystemStatus {
            health: self.health,
            health_rollup: self.health_rollup,
            state: self.state.unwrap_or("".to_string()),
        }
    }
}

impl IntoModel<BootOption> for libredfish::model::BootOption {
    fn into_model(self) -> BootOption {
        BootOption {
            display_name: self.display_name,
            id: self.id,
            boot_option_enabled: self.boot_option_enabled,
            uefi_device_path: self.uefi_device_path,
        }
    }
}

impl IntoModel<BlueFieldOperatingMode> for libredfish::model::oem::nvidia_dpu::NicMode {
    fn into_model(self) -> BlueFieldOperatingMode {
        match self {
            Self::Dpu => BlueFieldOperatingMode::Dpu,
            Self::Nic => BlueFieldOperatingMode::Nic,
        }
    }
}

impl IntoLibredfish<libredfish::model::oem::nvidia_dpu::NicMode> for BlueFieldOperatingMode {
    fn into_libredfish(self) -> libredfish::model::oem::nvidia_dpu::NicMode {
        match self {
            Self::Dpu => libredfish::model::oem::nvidia_dpu::NicMode::Dpu,
            Self::Nic => libredfish::model::oem::nvidia_dpu::NicMode::Nic,
        }
    }
}

pub fn machine_last_reboot_requested_mode(
    action: libredfish::SystemPowerControl,
) -> MachineLastRebootRequestedMode {
    match action {
        libredfish::SystemPowerControl::On => MachineLastRebootRequestedMode::PowerOn,
        libredfish::SystemPowerControl::GracefulShutdown => {
            MachineLastRebootRequestedMode::PowerOff
        }
        libredfish::SystemPowerControl::ForceOff => MachineLastRebootRequestedMode::PowerOff,
        libredfish::SystemPowerControl::GracefulRestart => MachineLastRebootRequestedMode::Reboot,
        libredfish::SystemPowerControl::ForceRestart => MachineLastRebootRequestedMode::Reboot,
        libredfish::SystemPowerControl::ACPowercycle => MachineLastRebootRequestedMode::Reboot,
        libredfish::SystemPowerControl::PowerCycle => MachineLastRebootRequestedMode::Reboot,
    }
}

pub fn bmc_vendor(r: libredfish::model::service_root::RedfishVendor) -> BMCVendor {
    use libredfish::model::service_root::RedfishVendor;
    match r {
        RedfishVendor::AMI
        | RedfishVendor::NvidiaDpu
        | RedfishVendor::NvidiaGBx00
        | RedfishVendor::NvidiaGH200
        | RedfishVendor::NvidiaGBSwitch
        | RedfishVendor::P3809
        | RedfishVendor::VeraRubin => BMCVendor::Nvidia,
        RedfishVendor::Dell => BMCVendor::Dell,
        RedfishVendor::Hpe => BMCVendor::Hpe,
        RedfishVendor::Lenovo => BMCVendor::Lenovo,
        RedfishVendor::LenovoAMI | RedfishVendor::LenovoGB300 => BMCVendor::LenovoAMI,
        RedfishVendor::LiteOnPowerShelf => BMCVendor::Liteon,
        RedfishVendor::DeltaPowerShelf => BMCVendor::Delta,
        RedfishVendor::Supermicro => BMCVendor::Supermicro,
        RedfishVendor::Sushy => BMCVendor::Sushy,
        RedfishVendor::Unknown => BMCVendor::Unknown,
    }
}

impl IntoModel<CaCertificate> for libredfish::model::component_integrity::CaCertificate {
    fn into_model(self) -> CaCertificate {
        CaCertificate {
            certificate_string: self.certificate_string,
            certificate_type: self.certificate_type,
            certificate_usage_types: self.certificate_usage_types,
            id: self.id,
            name: self.name,
            spdm: model::attestation::spdm::SlotInfo {
                slot_id: self.spdm.slot_id,
            },
        }
    }
}

impl IntoModel<Evidence> for libredfish::model::component_integrity::Evidence {
    fn into_model(self) -> Evidence {
        Evidence {
            hashing_algorithm: self.hashing_algorithm,
            signed_measurements: self.signed_measurements,
            signing_algorithm: self.signing_algorithm,
            version: self.version,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use libredfish::model::service_root::RedfishVendor;

    use super::*;

    #[test]
    fn redfish_vendors_map_to_bmc_vendors() {
        value_scenarios!(bmc_vendor:
            "NVIDIA Redfish implementations" {
                RedfishVendor::AMI => BMCVendor::Nvidia,
                RedfishVendor::NvidiaDpu => BMCVendor::Nvidia,
                RedfishVendor::NvidiaGBx00 => BMCVendor::Nvidia,
                RedfishVendor::NvidiaGH200 => BMCVendor::Nvidia,
                RedfishVendor::NvidiaGBSwitch => BMCVendor::Nvidia,
                RedfishVendor::P3809 => BMCVendor::Nvidia,
                RedfishVendor::VeraRubin => BMCVendor::Nvidia,
            }

            "vendor-specific Redfish implementations" {
                RedfishVendor::Dell => BMCVendor::Dell,
                RedfishVendor::Hpe => BMCVendor::Hpe,
                RedfishVendor::Lenovo => BMCVendor::Lenovo,
                RedfishVendor::LenovoAMI => BMCVendor::LenovoAMI,
                RedfishVendor::LenovoGB300 => BMCVendor::LenovoAMI,
                RedfishVendor::LiteOnPowerShelf => BMCVendor::Liteon,
                RedfishVendor::DeltaPowerShelf => BMCVendor::Delta,
                RedfishVendor::Supermicro => BMCVendor::Supermicro,
                RedfishVendor::Sushy => BMCVendor::Sushy,
                RedfishVendor::Unknown => BMCVendor::Unknown,
            }
        );
    }
}
