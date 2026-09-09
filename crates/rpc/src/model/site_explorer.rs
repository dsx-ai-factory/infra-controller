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

use model::errors::{OperatorError, OperatorErrorSchema};
use model::machine_boot_interface::MachineBootInterfaceTarget;
use model::site_explorer::{
    BlueFieldOperatingMode, BootOption, BootOrder, Chassis, ComputerSystem,
    ComputerSystemAttributes, EndpointExplorationReport, EthernetInterface, ExploredDpu,
    ExploredEndpoint, ExploredEndpointSearchFilter, ExploredManagedHost,
    ExploredManagedHostSearchFilter, ExploredMlxDevice, InternalLockdownStatus, Inventory,
    LockdownStatus, MachineSetupDiff, MachineSetupStatus, Manager, MlxDeviceKind, NetworkAdapter,
    PCIeDevice, PowerState, SecureBootStatus, Service, SiteExplorationReport, SiteExplorerLastRun,
    SystemStatus,
};

use crate as rpc;

impl From<rpc::site_explorer::ExploredEndpointSearchFilter> for ExploredEndpointSearchFilter {
    fn from(_filter: rpc::site_explorer::ExploredEndpointSearchFilter) -> Self {
        ExploredEndpointSearchFilter {}
    }
}

impl From<rpc::site_explorer::ExploredManagedHostSearchFilter> for ExploredManagedHostSearchFilter {
    fn from(_filter: rpc::site_explorer::ExploredManagedHostSearchFilter) -> Self {
        ExploredManagedHostSearchFilter {}
    }
}

impl From<SystemStatus> for rpc::site_explorer::SystemStatus {
    fn from(status: SystemStatus) -> Self {
        rpc::site_explorer::SystemStatus {
            health: status.health,
            health_rollup: status.health_rollup,
            state: status.state,
        }
    }
}

impl From<PCIeDevice> for rpc::site_explorer::PcIeDevice {
    fn from(device: PCIeDevice) -> Self {
        rpc::site_explorer::PcIeDevice {
            description: device.description,
            firmware_version: device.firmware_version,
            gpu_vendor: device.gpu_vendor,
            id: device.id,
            manufacturer: device.manufacturer,
            name: device.name,
            part_number: device.part_number,
            serial_number: device.serial_number,
            status: device.status.map(Into::into),
        }
    }
}

impl From<ExploredEndpoint> for rpc::site_explorer::ExploredEndpoint {
    fn from(endpoint: ExploredEndpoint) -> Self {
        rpc::site_explorer::ExploredEndpoint {
            address: endpoint.address.to_string(),
            report: Some(endpoint.report.into()),
            report_version: endpoint.report_version.to_string(),
            exploration_requested: endpoint.exploration_requested,
            preingestion_state: format!("{:?}", endpoint.preingestion_state),
            last_redfish_bmc_reset: endpoint
                .last_redfish_bmc_reset
                .map(|time| time.to_string())
                .unwrap_or_else(|| "no timestamp available".to_string()),
            last_ipmitool_bmc_reset: endpoint
                .last_ipmitool_bmc_reset
                .map(|time| time.to_string())
                .unwrap_or_else(|| "no timestamp available".to_string()),
            last_redfish_reboot: endpoint
                .last_redfish_reboot
                .map(|time| time.to_string())
                .unwrap_or_else(|| "no timestamp available".to_string()),
            last_redfish_powercycle: endpoint
                .last_redfish_powercycle
                .map(|time| time.to_string())
                .unwrap_or_else(|| "no timestamp available".to_string()),
            pause_remediation: endpoint.pause_remediation,
        }
    }
}

impl From<&ExploredDpu> for rpc::site_explorer::ExploredDpu {
    fn from(dpu: &ExploredDpu) -> Self {
        rpc::site_explorer::ExploredDpu {
            bmc_ip: dpu.bmc_ip.to_string(),
            host_pf_mac_address: dpu.host_pf_mac_address.map(|m| m.to_string()),
        }
    }
}

impl From<ExploredManagedHost> for rpc::site_explorer::ExploredManagedHost {
    fn from(host: ExploredManagedHost) -> Self {
        rpc::site_explorer::ExploredManagedHost {
            host_bmc_ip: host.host_bmc_ip.to_string(),
            dpus: host
                .dpus
                .iter()
                .map(rpc::site_explorer::ExploredDpu::from)
                .collect(),
            dpu_bmc_ip: host
                .dpus
                .first()
                .map_or("".to_string(), |d| d.bmc_ip.to_string()),
            host_pf_mac_address: host
                .dpus
                .first()
                .and_then(|d| d.host_pf_mac_address.map(|m| m.to_string())),
        }
    }
}

impl From<SiteExplorationReport> for rpc::site_explorer::SiteExplorationReport {
    fn from(report: SiteExplorationReport) -> Self {
        rpc::site_explorer::SiteExplorationReport {
            last_run: report.last_run.map(Into::into),
            endpoints: report.endpoints.into_iter().map(Into::into).collect(),
            managed_hosts: report.managed_hosts.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<SiteExplorerLastRun> for rpc::site_explorer::SiteExplorerLastRun {
    fn from(run: SiteExplorerLastRun) -> Self {
        rpc::site_explorer::SiteExplorerLastRun {
            started_at: run.started_at.to_rfc3339(),
            finished_at: run.finished_at.to_rfc3339(),
            success: run.success,
            error: run.error,
            failure_category: run.failure_category,
            endpoint_explorations: run.endpoint_explorations,
            endpoint_explorations_success: run.endpoint_explorations_success,
            endpoint_explorations_failed: run.endpoint_explorations_failed,
            last_successful_finished_at: run
                .last_successful_finished_at
                .map(|time| time.to_rfc3339()),
            last_failed_finished_at: run.last_failed_finished_at.map(|time| time.to_rfc3339()),
        }
    }
}

impl From<MlxDeviceKind> for rpc::site_explorer::MlxDeviceKind {
    fn from(kind: MlxDeviceKind) -> Self {
        match kind {
            MlxDeviceKind::Bf3NicMode => rpc::site_explorer::MlxDeviceKind::Bf3NicMode,
            MlxDeviceKind::Bf3DpuMode => rpc::site_explorer::MlxDeviceKind::Bf3DpuMode,
            MlxDeviceKind::Bf3SuperNic => rpc::site_explorer::MlxDeviceKind::Bf3SuperNic,
            MlxDeviceKind::Bf2Dpu => rpc::site_explorer::MlxDeviceKind::Bf2Dpu,
            MlxDeviceKind::Unknown => rpc::site_explorer::MlxDeviceKind::Unknown,
        }
    }
}

impl From<BlueFieldOperatingMode> for rpc::site_explorer::BlueFieldOperatingMode {
    fn from(mode: BlueFieldOperatingMode) -> Self {
        match mode {
            BlueFieldOperatingMode::Dpu => Self::Dpu,
            BlueFieldOperatingMode::Nic => Self::Nic,
        }
    }
}

impl From<ExploredMlxDevice> for rpc::site_explorer::ExploredMlxDevice {
    fn from(device: ExploredMlxDevice) -> Self {
        rpc::site_explorer::ExploredMlxDevice {
            host_bmc_ip: device.host_bmc_ip.to_string(),
            machine_id: device.machine_id.map(|id| id.to_string()),
            device_kind: rpc::site_explorer::MlxDeviceKind::from(device.device_kind) as i32,
            pcie_id: device.pcie_id,
            part_number: device.part_number,
            serial_number: device.serial_number,
            firmware_version: device.firmware_version,
            description: device.description,
            dpu_bmc_ip: device.dpu_bmc_ip.map(|ip| ip.to_string()),
            nic_mode: device
                .nic_mode
                .map(|mode| rpc::site_explorer::BlueFieldOperatingMode::from(mode) as i32),
        }
    }
}

impl From<ComputerSystemAttributes> for rpc::site_explorer::ComputerSystemAttributes {
    fn from(attributes: ComputerSystemAttributes) -> Self {
        rpc::site_explorer::ComputerSystemAttributes {
            nic_mode: attributes
                .nic_mode
                .map(|mode| rpc::site_explorer::BlueFieldOperatingMode::from(mode).into()),
        }
    }
}

impl From<ComputerSystem> for rpc::site_explorer::ComputerSystem {
    fn from(system: ComputerSystem) -> Self {
        rpc::site_explorer::ComputerSystem {
            id: system.id,
            manufacturer: system.manufacturer,
            model: system.model,
            serial_number: system.serial_number,
            ethernet_interfaces: system
                .ethernet_interfaces
                .into_iter()
                .map(Into::into)
                .collect(),
            attributes: Some(rpc::site_explorer::ComputerSystemAttributes::from(
                system.attributes,
            )),
            pcie_devices: system.pcie_devices.into_iter().map(Into::into).collect(),
            power_state: rpc::site_explorer::PowerState::from(system.power_state) as _,
            boot_order: system.boot_order.map(|order| order.into()),
            base_mac: system.base_mac.map(|mac| mac.to_string()),
        }
    }
}

impl From<PowerState> for rpc::site_explorer::PowerState {
    fn from(state: PowerState) -> Self {
        match state {
            PowerState::Off => rpc::site_explorer::PowerState::Off,
            PowerState::On => rpc::site_explorer::PowerState::On,
            PowerState::PoweringOff => rpc::site_explorer::PowerState::PoweringOff,
            PowerState::PoweringOn => rpc::site_explorer::PowerState::PoweringOn,
            PowerState::Paused => rpc::site_explorer::PowerState::Paused,
            PowerState::Hibernating => rpc::site_explorer::PowerState::Hibernating,
            PowerState::Sleeping => rpc::site_explorer::PowerState::Sleeping,
            PowerState::Unknown => rpc::site_explorer::PowerState::Unknown,
        }
    }
}

impl From<Manager> for rpc::site_explorer::Manager {
    fn from(manager: Manager) -> Self {
        rpc::site_explorer::Manager {
            id: manager.id,
            ethernet_interfaces: manager
                .ethernet_interfaces
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<EthernetInterface> for rpc::site_explorer::EthernetInterface {
    fn from(interface: EthernetInterface) -> Self {
        rpc::site_explorer::EthernetInterface {
            id: interface.id,
            description: interface.description,
            interface_enabled: interface.interface_enabled,
            mac_address: interface.mac_address.map(|mac| mac.to_string()),
            link_status: interface.link_status,
        }
    }
}

impl From<Chassis> for rpc::site_explorer::Chassis {
    fn from(chassis: Chassis) -> Self {
        rpc::site_explorer::Chassis {
            id: chassis.id,
            manufacturer: chassis.manufacturer,
            model: chassis.model,
            part_number: chassis.part_number,
            serial_number: chassis.serial_number,
            network_adapters: chassis
                .network_adapters
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<NetworkAdapter> for rpc::site_explorer::NetworkAdapter {
    fn from(adapter: NetworkAdapter) -> Self {
        rpc::site_explorer::NetworkAdapter {
            id: adapter.id,
            manufacturer: adapter.manufacturer,
            model: adapter.model,
            part_number: adapter.part_number,
            serial_number: adapter.serial_number,
            port_mac_addresses: adapter
                .port_mac_addresses
                .into_iter()
                .map(|mac_address| mac_address.to_string())
                .collect(),
        }
    }
}

impl From<SecureBootStatus> for rpc::site_explorer::SecureBootStatus {
    fn from(secure_boot_status: SecureBootStatus) -> Self {
        rpc::site_explorer::SecureBootStatus {
            is_enabled: secure_boot_status.is_enabled,
        }
    }
}

impl From<LockdownStatus> for rpc::site_explorer::LockdownStatus {
    fn from(lockdown_status: LockdownStatus) -> Self {
        rpc::site_explorer::LockdownStatus {
            status: rpc::site_explorer::InternalLockdownStatus::from(lockdown_status.status) as _,
            message: lockdown_status.message,
        }
    }
}

impl From<InternalLockdownStatus> for rpc::site_explorer::InternalLockdownStatus {
    fn from(state: InternalLockdownStatus) -> Self {
        match state {
            InternalLockdownStatus::Enabled => rpc::site_explorer::InternalLockdownStatus::Enabled,
            InternalLockdownStatus::Partial => rpc::site_explorer::InternalLockdownStatus::Partial,
            InternalLockdownStatus::Disabled => {
                rpc::site_explorer::InternalLockdownStatus::Disabled
            }
        }
    }
}

impl From<Service> for rpc::site_explorer::Service {
    fn from(service: Service) -> Self {
        rpc::site_explorer::Service {
            id: service.id,
            inventories: service.inventories.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<Inventory> for rpc::site_explorer::Inventory {
    fn from(inventory: Inventory) -> Self {
        rpc::site_explorer::Inventory {
            id: inventory.id,
            description: inventory.description,
            version: inventory.version,
            release_date: inventory.release_date,
        }
    }
}

impl From<MachineBootInterfaceTarget> for rpc::site_explorer::MachineBootInterfaceTarget {
    fn from(target: MachineBootInterfaceTarget) -> Self {
        use rpc::site_explorer::machine_boot_interface_target::Target;

        let target = match target {
            MachineBootInterfaceTarget::Pair(boot_interface) => {
                Target::Pair(rpc::site_explorer::MachineBootInterfacePair {
                    mac_address: boot_interface.mac_address.to_string(),
                    interface_id: boot_interface.interface_id,
                })
            }
            MachineBootInterfaceTarget::MacOnly(mac_address) => {
                Target::MacOnly(mac_address.to_string())
            }
        };
        Self {
            target: Some(target),
        }
    }
}

impl From<MachineSetupStatus> for rpc::site_explorer::MachineSetupStatus {
    fn from(machine_setup_status: MachineSetupStatus) -> Self {
        rpc::site_explorer::MachineSetupStatus {
            is_done: machine_setup_status.is_done,
            diffs: machine_setup_status
                .diffs
                .into_iter()
                .map(Into::into)
                .collect(),
            evaluated_boot_interface: machine_setup_status
                .evaluated_boot_interface
                .map(Into::into),
        }
    }
}

impl From<BootOrder> for rpc::site_explorer::BootOrder {
    fn from(order: BootOrder) -> Self {
        rpc::site_explorer::BootOrder {
            boot_order: order.boot_order.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<MachineSetupDiff> for rpc::site_explorer::MachineSetupDiff {
    fn from(machine_setup_diff: MachineSetupDiff) -> Self {
        rpc::site_explorer::MachineSetupDiff {
            key: machine_setup_diff.key,
            expected: machine_setup_diff.expected,
            actual: machine_setup_diff.actual,
        }
    }
}

impl From<BootOption> for rpc::site_explorer::BootOption {
    fn from(boot_option: BootOption) -> Self {
        rpc::site_explorer::BootOption {
            display_name: boot_option.display_name,
            id: boot_option.id,
            boot_option_enabled: boot_option.boot_option_enabled,
            uefi_device_path: boot_option.uefi_device_path,
        }
    }
}

impl From<EndpointExplorationReport> for rpc::site_explorer::EndpointExplorationReport {
    fn from(report: EndpointExplorationReport) -> Self {
        let last_exploration_error_schema = report
            .last_exploration_error
            .as_ref()
            .map(|error| error.operator_error_schema())
            .map(Into::into);

        rpc::site_explorer::EndpointExplorationReport {
            endpoint_type: format!("{:?}", report.endpoint_type),
            last_exploration_error: report.last_exploration_error.map(|error| {
                serde_json::to_string(&error).unwrap_or_else(|_| "Unserializable error".to_string())
            }),
            last_exploration_latency: report.last_exploration_latency.map(Into::into),
            machine_id: report.machine_id.map(|id| id.to_string()),
            vendor: report.vendor.map(|v| v.to_string()),
            managers: report.managers.into_iter().map(Into::into).collect(),
            systems: report.systems.into_iter().map(Into::into).collect(),
            chassis: report.chassis.into_iter().map(Into::into).collect(),
            service: report.service.into_iter().map(Into::into).collect(),
            machine_setup_status: report.machine_setup_status.map(Into::into),
            secure_boot_status: report.secure_boot_status.map(Into::into),
            lockdown_status: report.lockdown_status.map(Into::into),
            firmware_versions: serde_json::to_value(&report.versions)
                .and_then(serde_json::from_value)
                .unwrap_or_default(),
            last_exploration_error_schema,
        }
    }
}

impl From<OperatorErrorSchema> for rpc::site_explorer::OperatorErrorSchema {
    fn from(schema: OperatorErrorSchema) -> Self {
        Self {
            // The wire/proto contract is the rendered `SYSTEM-SUBSYSTEM-CODE` string.
            error_code: schema.error_code.to_string(),
            mitigation: schema.mitigation,
            text: schema.text,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use carbide_test_support::value_scenarios;
    use carbide_uuid::machine::MachineId;
    use chrono::{DateTime, TimeZone as _, Utc};
    use model::firmware::FirmwareComponentType;
    use model::machine_boot_interface::MachineBootInterface;
    use model::site_explorer::{EndpointExplorationError, EndpointType, PreingestionState};
    use prost::Message;

    use super::*;

    const MACHINE_ID: &str = "fm100htv4fu8fpktl0e0qrg4dl58g2bc2g7naq0l6c15ruc22po1i5rfsq0";
    const NO_TIMESTAMP: &str = "no timestamp available";

    fn timestamp(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 23, 12, 0, second)
            .single()
            .expect("valid test timestamp")
    }

    fn explored_dpu(address: &str, mac_address: Option<&str>) -> ExploredDpu {
        ExploredDpu {
            bmc_ip: address.parse().expect("valid DPU BMC IP"),
            host_pf_mac_address: mac_address.map(|mac| mac.parse().expect("valid test MAC")),
            host_chassis_id: None,
            report: Arc::new(EndpointExplorationReport::default()),
        }
    }

    fn minimal_endpoint(address: &str) -> ExploredEndpoint {
        ExploredEndpoint {
            address: address.parse().expect("valid endpoint IP"),
            report: EndpointExplorationReport::default(),
            report_version: "V1-T0".parse().expect("valid config version"),
            preingestion_state: PreingestionState::Initial,
            waiting_for_explorer_refresh: false,
            exploration_requested: false,
            last_redfish_bmc_reset: None,
            last_ipmitool_bmc_reset: None,
            last_redfish_reboot: None,
            last_redfish_powercycle: None,
            pause_ingestion_and_poweron: false,
            pause_remediation: false,
            boot_interface_mac: None,
            boot_interface_id: None,
        }
    }

    fn successful_last_run() -> SiteExplorerLastRun {
        SiteExplorerLastRun {
            started_at: timestamp(0),
            finished_at: timestamp(1),
            success: true,
            error: None,
            failure_category: None,
            endpoint_explorations: 2,
            endpoint_explorations_success: 2,
            endpoint_explorations_failed: 0,
            last_successful_finished_at: Some(timestamp(1)),
            last_failed_finished_at: None,
        }
    }

    #[derive(Debug, PartialEq)]
    struct EndpointSummary {
        address: String,
        report_version: String,
        report_type: Option<String>,
        vendor: Option<String>,
        exploration_requested: bool,
        preingestion_state: String,
        last_redfish_bmc_reset: String,
        last_ipmitool_bmc_reset: String,
        last_redfish_reboot: String,
        last_redfish_powercycle: String,
        pause_remediation: bool,
    }

    fn summarize_endpoint(endpoint: ExploredEndpoint) -> EndpointSummary {
        let endpoint = rpc::site_explorer::ExploredEndpoint::from(endpoint);
        EndpointSummary {
            address: endpoint.address,
            report_version: endpoint.report_version,
            report_type: endpoint
                .report
                .as_ref()
                .map(|report| report.endpoint_type.clone()),
            vendor: endpoint.report.and_then(|report| report.vendor),
            exploration_requested: endpoint.exploration_requested,
            preingestion_state: endpoint.preingestion_state,
            last_redfish_bmc_reset: endpoint.last_redfish_bmc_reset,
            last_ipmitool_bmc_reset: endpoint.last_ipmitool_bmc_reset,
            last_redfish_reboot: endpoint.last_redfish_reboot,
            last_redfish_powercycle: endpoint.last_redfish_powercycle,
            pause_remediation: endpoint.pause_remediation,
        }
    }

    #[derive(Debug, PartialEq)]
    struct ErrorSchemaSummary {
        error_code: String,
        mitigation: Option<String>,
        text: String,
    }

    #[derive(Debug, PartialEq)]
    struct ManagerSummary {
        id: String,
        interface_count: usize,
        interface_id: Option<String>,
    }

    #[derive(Debug, PartialEq)]
    struct SystemSummary {
        id: String,
        manufacturer: Option<String>,
        model: Option<String>,
        serial_number: Option<String>,
        base_mac: Option<String>,
        nic_mode: Option<i32>,
        interface_count: usize,
        interface_id: Option<String>,
        pcie_device_count: usize,
        pcie_id: Option<String>,
        power_state: i32,
        boot_option_count: usize,
        boot_option_id: Option<String>,
    }

    #[derive(Debug, PartialEq)]
    struct ChassisSummary {
        id: String,
        manufacturer: Option<String>,
        model: Option<String>,
        part_number: Option<String>,
        serial_number: Option<String>,
        adapter_count: usize,
        adapter_id: Option<String>,
    }

    #[derive(Debug, PartialEq)]
    struct ServiceSummary {
        id: String,
        inventory_count: usize,
        inventory_id: Option<String>,
    }

    #[derive(Debug, PartialEq)]
    struct MachineSetupSummary {
        is_done: bool,
        diff_count: usize,
        diff_key: Option<String>,
    }

    #[derive(Debug, PartialEq)]
    struct EndpointReportSummary {
        endpoint_type: String,
        serialized_error: Option<EndpointExplorationError>,
        error_schema: Option<ErrorSchemaSummary>,
        latency: Option<(i64, i32)>,
        machine_id: Option<String>,
        vendor: Option<String>,
        child_counts: (usize, usize, usize, usize),
        manager: Option<ManagerSummary>,
        system: Option<SystemSummary>,
        chassis: Option<ChassisSummary>,
        service: Option<ServiceSummary>,
        machine_setup: Option<MachineSetupSummary>,
        secure_boot_enabled: Option<bool>,
        lockdown: Option<(i32, String)>,
        firmware_versions: HashMap<String, String>,
    }

    fn summarize_endpoint_report(report: EndpointExplorationReport) -> EndpointReportSummary {
        let report = rpc::site_explorer::EndpointExplorationReport::from(report);
        let manager = report.managers.first().map(|manager| {
            let interface = manager.ethernet_interfaces.first();
            ManagerSummary {
                id: manager.id.clone(),
                interface_count: manager.ethernet_interfaces.len(),
                interface_id: interface.and_then(|interface| interface.id.clone()),
            }
        });
        let system = report.systems.first().map(|system| {
            let pcie_device = system.pcie_devices.first();
            SystemSummary {
                id: system.id.clone(),
                manufacturer: system.manufacturer.clone(),
                model: system.model.clone(),
                serial_number: system.serial_number.clone(),
                base_mac: system.base_mac.clone(),
                nic_mode: system
                    .attributes
                    .as_ref()
                    .and_then(|attributes| attributes.nic_mode),
                interface_count: system.ethernet_interfaces.len(),
                interface_id: system
                    .ethernet_interfaces
                    .first()
                    .and_then(|interface| interface.id.clone()),
                pcie_device_count: system.pcie_devices.len(),
                pcie_id: pcie_device.and_then(|device| device.id.clone()),
                power_state: system.power_state,
                boot_option_count: system
                    .boot_order
                    .as_ref()
                    .map_or(0, |order| order.boot_order.len()),
                boot_option_id: system
                    .boot_order
                    .as_ref()
                    .and_then(|order| order.boot_order.first())
                    .map(|option| option.id.clone()),
            }
        });
        let chassis = report.chassis.first().map(|chassis| ChassisSummary {
            id: chassis.id.clone(),
            manufacturer: chassis.manufacturer.clone(),
            model: chassis.model.clone(),
            part_number: chassis.part_number.clone(),
            serial_number: chassis.serial_number.clone(),
            adapter_count: chassis.network_adapters.len(),
            adapter_id: chassis
                .network_adapters
                .first()
                .map(|adapter| adapter.id.clone()),
        });
        let service = report.service.first().map(|service| ServiceSummary {
            id: service.id.clone(),
            inventory_count: service.inventories.len(),
            inventory_id: service
                .inventories
                .first()
                .map(|inventory| inventory.id.clone()),
        });
        let machine_setup =
            report
                .machine_setup_status
                .as_ref()
                .map(|status| MachineSetupSummary {
                    is_done: status.is_done,
                    diff_count: status.diffs.len(),
                    diff_key: status.diffs.first().map(|diff| diff.key.clone()),
                });
        let serialized_error = report.last_exploration_error.map(|error| {
            serde_json::from_str(&error).expect("RPC error remains serialized model JSON")
        });
        let error_schema = report
            .last_exploration_error_schema
            .map(|schema| ErrorSchemaSummary {
                error_code: schema.error_code,
                mitigation: schema.mitigation,
                text: schema.text,
            });

        EndpointReportSummary {
            endpoint_type: report.endpoint_type,
            serialized_error,
            error_schema,
            latency: report
                .last_exploration_latency
                .map(|duration| (duration.seconds, duration.nanos)),
            machine_id: report.machine_id,
            vendor: report.vendor,
            child_counts: (
                report.managers.len(),
                report.systems.len(),
                report.chassis.len(),
                report.service.len(),
            ),
            manager,
            system,
            chassis,
            service,
            machine_setup,
            secure_boot_enabled: report.secure_boot_status.map(|status| status.is_enabled),
            lockdown: report
                .lockdown_status
                .map(|status| (status.status, status.message)),
            firmware_versions: report.firmware_versions,
        }
    }

    #[test]
    fn mlx_device_kinds_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::MlxDeviceKind::from;
            "BlueField-3 NIC-mode SKU" {
                MlxDeviceKind::Bf3NicMode => rpc::site_explorer::MlxDeviceKind::Bf3NicMode,
            }

            "BlueField-3 DPU-mode SKU" {
                MlxDeviceKind::Bf3DpuMode => rpc::site_explorer::MlxDeviceKind::Bf3DpuMode,
            }

            "BlueField-3 SuperNIC SKU" {
                MlxDeviceKind::Bf3SuperNic => rpc::site_explorer::MlxDeviceKind::Bf3SuperNic,
            }

            "BlueField-2 DPU" {
                MlxDeviceKind::Bf2Dpu => rpc::site_explorer::MlxDeviceKind::Bf2Dpu,
            }

            "unknown device kind" {
                MlxDeviceKind::Unknown => rpc::site_explorer::MlxDeviceKind::Unknown,
            }
        );
    }

    #[test]
    fn bluefield_operating_modes_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::BlueFieldOperatingMode::from;
            "DPU mode" {
                BlueFieldOperatingMode::Dpu => rpc::site_explorer::BlueFieldOperatingMode::Dpu,
            }

            "NIC mode" {
                BlueFieldOperatingMode::Nic => rpc::site_explorer::BlueFieldOperatingMode::Nic,
            }
        );
    }

    #[test]
    fn power_states_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::PowerState::from;
            "off" {
                PowerState::Off => rpc::site_explorer::PowerState::Off,
            }

            "on" {
                PowerState::On => rpc::site_explorer::PowerState::On,
            }

            "powering off" {
                PowerState::PoweringOff => rpc::site_explorer::PowerState::PoweringOff,
            }

            "powering on" {
                PowerState::PoweringOn => rpc::site_explorer::PowerState::PoweringOn,
            }

            "paused" {
                PowerState::Paused => rpc::site_explorer::PowerState::Paused,
            }

            "unknown" {
                PowerState::Unknown => rpc::site_explorer::PowerState::Unknown,
            }
        );
    }

    #[test]
    fn lockdown_states_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::InternalLockdownStatus::from;
            "enabled" {
                InternalLockdownStatus::Enabled => rpc::site_explorer::InternalLockdownStatus::Enabled,
            }

            "partial" {
                InternalLockdownStatus::Partial => rpc::site_explorer::InternalLockdownStatus::Partial,
            }

            "disabled" {
                InternalLockdownStatus::Disabled => rpc::site_explorer::InternalLockdownStatus::Disabled,
            }
        );
    }

    #[test]
    fn managed_hosts_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::ExploredManagedHost::from;
            "no DPUs" {
                ExploredManagedHost {
                    host_bmc_ip: "192.0.2.10".parse().unwrap(),
                    dpus: vec![],
                } => rpc::site_explorer::ExploredManagedHost {
                    host_bmc_ip: "192.0.2.10".to_string(),
                    dpu_bmc_ip: String::new(),
                    host_pf_mac_address: None,
                    dpus: vec![],
                },
            }

            "one DPU populates the repeated and legacy fields" {
                ExploredManagedHost {
                    host_bmc_ip: "192.0.2.11".parse().unwrap(),
                    dpus: vec![explored_dpu(
                        "192.0.2.21",
                        Some("02:00:00:00:00:01"),
                    )],
                } => rpc::site_explorer::ExploredManagedHost {
                    host_bmc_ip: "192.0.2.11".to_string(),
                    dpu_bmc_ip: "192.0.2.21".to_string(),
                    host_pf_mac_address: Some("02:00:00:00:00:01".to_string()),
                    dpus: vec![rpc::site_explorer::ExploredDpu {
                        bmc_ip: "192.0.2.21".to_string(),
                        host_pf_mac_address: Some("02:00:00:00:00:01".to_string()),
                    }],
                },
            }

            "multiple DPUs preserve every DPU and derive legacy fields from the first" {
                ExploredManagedHost {
                    host_bmc_ip: "192.0.2.12".parse().unwrap(),
                    dpus: vec![
                        explored_dpu("192.0.2.22", None),
                        explored_dpu("192.0.2.23", Some("02:00:00:00:00:03")),
                    ],
                } => rpc::site_explorer::ExploredManagedHost {
                    host_bmc_ip: "192.0.2.12".to_string(),
                    dpu_bmc_ip: "192.0.2.22".to_string(),
                    host_pf_mac_address: None,
                    dpus: vec![
                        rpc::site_explorer::ExploredDpu {
                            bmc_ip: "192.0.2.22".to_string(),
                            host_pf_mac_address: None,
                        },
                        rpc::site_explorer::ExploredDpu {
                            bmc_ip: "192.0.2.23".to_string(),
                            host_pf_mac_address: Some("02:00:00:00:00:03".to_string()),
                        },
                    ],
                },
            }
        );
    }

    #[test]
    fn explored_endpoints_convert_to_rpc() {
        let sparse_version: config_version::ConfigVersion =
            "V2-T0".parse().expect("valid config version");
        let sparse = ExploredEndpoint {
            report_version: sparse_version,
            ..minimal_endpoint("192.0.2.30")
        };

        let reset = timestamp(2);
        let ipmi_reset = timestamp(3);
        let reboot = timestamp(4);
        let powercycle = timestamp(5);
        let populated_version: config_version::ConfigVersion =
            "V9-T1000000".parse().expect("valid config version");
        let populated = ExploredEndpoint {
            address: "2001:db8::30".parse().unwrap(),
            report: EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                vendor: Some("nvidia".into()),
                ..Default::default()
            },
            report_version: populated_version,
            preingestion_state: PreingestionState::RecheckVersions,
            waiting_for_explorer_refresh: false,
            exploration_requested: true,
            last_redfish_bmc_reset: Some(reset),
            last_ipmitool_bmc_reset: Some(ipmi_reset),
            last_redfish_reboot: Some(reboot),
            last_redfish_powercycle: Some(powercycle),
            pause_ingestion_and_poweron: false,
            pause_remediation: true,
            boot_interface_mac: None,
            boot_interface_id: None,
        };

        value_scenarios!(run = summarize_endpoint;
            "absent timestamps use the existing sentinel" {
                sparse => EndpointSummary {
                    address: "192.0.2.30".to_string(),
                    report_version: sparse_version.to_string(),
                    report_type: Some("Unknown".to_string()),
                    vendor: None,
                    exploration_requested: false,
                    preingestion_state: "Initial".to_string(),
                    last_redfish_bmc_reset: NO_TIMESTAMP.to_string(),
                    last_ipmitool_bmc_reset: NO_TIMESTAMP.to_string(),
                    last_redfish_reboot: NO_TIMESTAMP.to_string(),
                    last_redfish_powercycle: NO_TIMESTAMP.to_string(),
                    pause_remediation: false,
                },
            }

            "populated endpoint metadata" {
                populated => EndpointSummary {
                    address: "2001:db8::30".to_string(),
                    report_version: populated_version.to_string(),
                    report_type: Some("Bmc".to_string()),
                    vendor: Some("nvidia".to_string()),
                    exploration_requested: true,
                    preingestion_state: "RecheckVersions".to_string(),
                    last_redfish_bmc_reset: reset.to_string(),
                    last_ipmitool_bmc_reset: ipmi_reset.to_string(),
                    last_redfish_reboot: reboot.to_string(),
                    last_redfish_powercycle: powercycle.to_string(),
                    pause_remediation: true,
                },
            }
        );
    }

    #[test]
    fn explored_mlx_devices_convert_to_rpc() {
        let machine_id: MachineId = MACHINE_ID.parse().expect("valid machine ID");

        value_scenarios!(run = rpc::site_explorer::ExploredMlxDevice::from;
            "optional fields absent" {
                ExploredMlxDevice {
                    host_bmc_ip: "192.0.2.40".parse().unwrap(),
                    machine_id: None,
                    device_kind: MlxDeviceKind::Unknown,
                    pcie_id: None,
                    part_number: None,
                    serial_number: None,
                    firmware_version: None,
                    description: None,
                    dpu_bmc_ip: None,
                    nic_mode: None,
                } => rpc::site_explorer::ExploredMlxDevice {
                    host_bmc_ip: "192.0.2.40".to_string(),
                    machine_id: None,
                    device_kind: rpc::site_explorer::MlxDeviceKind::Unknown as i32,
                    pcie_id: None,
                    part_number: None,
                    serial_number: None,
                    firmware_version: None,
                    description: None,
                    dpu_bmc_ip: None,
                    nic_mode: None,
                },
            }

            "optional fields populated" {
                ExploredMlxDevice {
                    host_bmc_ip: "2001:db8::40".parse().unwrap(),
                    machine_id: Some(machine_id),
                    device_kind: MlxDeviceKind::Bf3DpuMode,
                    pcie_id: Some("188-0".to_string()),
                    part_number: Some("900-9D3B6-00CN-PA0".to_string()),
                    serial_number: Some("MT2403X00984".to_string()),
                    firmware_version: Some("32.42.1000".to_string()),
                    description: Some("NVIDIA BlueField-3 DPU".to_string()),
                    dpu_bmc_ip: Some("2001:db8::41".parse().unwrap()),
                    nic_mode: Some(BlueFieldOperatingMode::Nic),
                } => rpc::site_explorer::ExploredMlxDevice {
                    host_bmc_ip: "2001:db8::40".to_string(),
                    machine_id: Some(MACHINE_ID.to_string()),
                    device_kind: rpc::site_explorer::MlxDeviceKind::Bf3DpuMode as i32,
                    pcie_id: Some("188-0".to_string()),
                    part_number: Some("900-9D3B6-00CN-PA0".to_string()),
                    serial_number: Some("MT2403X00984".to_string()),
                    firmware_version: Some("32.42.1000".to_string()),
                    description: Some("NVIDIA BlueField-3 DPU".to_string()),
                    dpu_bmc_ip: Some("2001:db8::41".to_string()),
                    nic_mode: Some(rpc::site_explorer::BlueFieldOperatingMode::Nic as i32),
                },
            }
        );
    }

    #[test]
    fn ethernet_interfaces_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::EthernetInterface::from;
            "optional fields absent" {
                EthernetInterface::default() => rpc::site_explorer::EthernetInterface::default(),
            }

            "optional fields populated" {
                EthernetInterface {
                    id: Some("ethernet-1".to_string()),
                    description: Some("host interface".to_string()),
                    interface_enabled: Some(true),
                    mac_address: Some("02:00:00:00:20:01".parse().expect("valid test MAC")),
                    link_status: Some("LinkUp".to_string()),
                    ..Default::default()
                } => rpc::site_explorer::EthernetInterface {
                    id: Some("ethernet-1".to_string()),
                    description: Some("host interface".to_string()),
                    interface_enabled: Some(true),
                    mac_address: Some("02:00:00:00:20:01".to_string()),
                    link_status: Some("LinkUp".to_string()),
                },
            }
        );
    }

    #[test]
    fn pcie_devices_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::PcIeDevice::from;
            "optional fields absent" {
                PCIeDevice {
                    description: None,
                    firmware_version: None,
                    gpu_vendor: None,
                    id: None,
                    manufacturer: None,
                    name: None,
                    part_number: None,
                    serial_number: None,
                    status: None,
                } => rpc::site_explorer::PcIeDevice {
                    description: None,
                    firmware_version: None,
                    gpu_vendor: None,
                    id: None,
                    manufacturer: None,
                    name: None,
                    part_number: None,
                    serial_number: None,
                    status: None,
                },
            }

            "optional fields and status populated" {
                PCIeDevice {
                    description: Some("BlueField adapter".to_string()),
                    firmware_version: Some("32.42.1000".to_string()),
                    gpu_vendor: Some("GPU vendor".to_string()),
                    id: Some("188-0".to_string()),
                    manufacturer: Some("PCIe manufacturer".to_string()),
                    name: Some("Network Adapter".to_string()),
                    part_number: Some("900-9D3B6".to_string()),
                    serial_number: Some("PCIE-SERIAL".to_string()),
                    status: Some(SystemStatus {
                        health: Some("OK".to_string()),
                        health_rollup: Some("Warning".to_string()),
                        state: "Enabled".to_string(),
                    }),
                } => rpc::site_explorer::PcIeDevice {
                    description: Some("BlueField adapter".to_string()),
                    firmware_version: Some("32.42.1000".to_string()),
                    gpu_vendor: Some("GPU vendor".to_string()),
                    id: Some("188-0".to_string()),
                    manufacturer: Some("PCIe manufacturer".to_string()),
                    name: Some("Network Adapter".to_string()),
                    part_number: Some("900-9D3B6".to_string()),
                    serial_number: Some("PCIE-SERIAL".to_string()),
                    status: Some(rpc::site_explorer::SystemStatus {
                        health: Some("OK".to_string()),
                        health_rollup: Some("Warning".to_string()),
                        state: "Enabled".to_string(),
                    }),
                },
            }
        );
    }

    #[test]
    fn boot_options_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::BootOption::from;
            "optional fields absent" {
                BootOption::default() => rpc::site_explorer::BootOption::default(),
            }

            "optional fields populated" {
                BootOption {
                    display_name: "PXE".to_string(),
                    id: "Boot0001".to_string(),
                    boot_option_enabled: Some(true),
                    uefi_device_path: Some("PciRoot(0x0)".to_string()),
                } => rpc::site_explorer::BootOption {
                    display_name: "PXE".to_string(),
                    id: "Boot0001".to_string(),
                    boot_option_enabled: Some(true),
                    uefi_device_path: Some("PciRoot(0x0)".to_string()),
                },
            }
        );
    }

    #[test]
    fn network_adapters_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::NetworkAdapter::from;
            "optional fields absent" {
                NetworkAdapter::default() => rpc::site_explorer::NetworkAdapter::default(),
            }

            "optional fields populated" {
                NetworkAdapter {
                    id: "adapter-1".to_string(),
                    manufacturer: Some("NVIDIA".to_string()),
                    model: Some("ConnectX-7".to_string()),
                    part_number: Some("ADAPTER-PN".to_string()),
                    serial_number: Some("ADAPTER-SERIAL".to_string()),
                    port_mac_addresses: vec!["94:6d:ae:53:cb:9b".parse().unwrap()],
                } => rpc::site_explorer::NetworkAdapter {
                    id: "adapter-1".to_string(),
                    manufacturer: Some("NVIDIA".to_string()),
                    model: Some("ConnectX-7".to_string()),
                    part_number: Some("ADAPTER-PN".to_string()),
                    serial_number: Some("ADAPTER-SERIAL".to_string()),
                    port_mac_addresses: vec!["94:6D:AE:53:CB:9B".to_string()],
                },
            }
        );
    }

    #[test]
    fn inventories_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::Inventory::from;
            "optional fields absent" {
                Inventory::default() => rpc::site_explorer::Inventory::default(),
            }

            "optional fields populated" {
                Inventory {
                    id: "inventory-1".to_string(),
                    description: Some("BMC firmware".to_string()),
                    version: Some("25.06-2".to_string()),
                    release_date: Some("2026-06-01".to_string()),
                } => rpc::site_explorer::Inventory {
                    id: "inventory-1".to_string(),
                    description: Some("BMC firmware".to_string()),
                    version: Some("25.06-2".to_string()),
                    release_date: Some("2026-06-01".to_string()),
                },
            }
        );
    }

    #[test]
    fn machine_setup_diffs_convert_to_rpc() {
        value_scenarios!(run = rpc::site_explorer::MachineSetupDiff::from;
            "empty fields" {
                MachineSetupDiff::default() => rpc::site_explorer::MachineSetupDiff::default(),
            }

            "distinct fields" {
                MachineSetupDiff {
                    key: "boot-order".to_string(),
                    expected: "PXE".to_string(),
                    actual: "Disk".to_string(),
                } => rpc::site_explorer::MachineSetupDiff {
                    key: "boot-order".to_string(),
                    expected: "PXE".to_string(),
                    actual: "Disk".to_string(),
                },
            }
        );
    }

    #[test]
    fn machine_setup_statuses_include_the_evaluated_boot_interface() {
        let mac_address = "02:00:00:00:10:03".parse().expect("valid test MAC");

        value_scenarios!(run = rpc::site_explorer::MachineSetupStatus::from;
            "report written before target capture" {
                MachineSetupStatus::default() => rpc::site_explorer::MachineSetupStatus::default(),
            }

            "complete pair" {
                MachineSetupStatus {
                    is_done: true,
                    diffs: Vec::new(),
                    evaluated_boot_interface: Some(MachineBootInterfaceTarget::Pair(
                        MachineBootInterface {
                            mac_address,
                            interface_id: "NIC.Slot.7-1-1".to_string(),
                        },
                    )),
                } => rpc::site_explorer::MachineSetupStatus {
                    is_done: true,
                    diffs: Vec::new(),
                    evaluated_boot_interface: Some(
                        rpc::site_explorer::MachineBootInterfaceTarget {
                            target: Some(
                                rpc::site_explorer::machine_boot_interface_target::Target::Pair(
                                    rpc::site_explorer::MachineBootInterfacePair {
                                        mac_address: "02:00:00:00:10:03".to_string(),
                                        interface_id: "NIC.Slot.7-1-1".to_string(),
                                    },
                                ),
                            ),
                        },
                    ),
                },
            }

            "legacy MAC only" {
                MachineSetupStatus {
                    is_done: false,
                    diffs: Vec::new(),
                    evaluated_boot_interface: Some(MachineBootInterfaceTarget::MacOnly(mac_address)),
                } => rpc::site_explorer::MachineSetupStatus {
                    is_done: false,
                    diffs: Vec::new(),
                    evaluated_boot_interface: Some(
                        rpc::site_explorer::MachineBootInterfaceTarget {
                            target: Some(
                                rpc::site_explorer::machine_boot_interface_target::Target::MacOnly(
                                    "02:00:00:00:10:03".to_string(),
                                ),
                            ),
                        },
                    ),
                },
            }
        );
    }

    #[test]
    fn endpoint_reports_convert_to_rpc() {
        let error = EndpointExplorationError::MissingVendor { observed: None };
        let expected_schema = error.operator_error_schema();
        let machine_id: MachineId = MACHINE_ID.parse().expect("valid machine ID");
        let manager_mac = "02:00:00:00:10:01".parse().expect("valid test MAC");
        let system_mac = "02:00:00:00:10:02".parse().expect("valid test MAC");
        let system_base_mac = "02:00:00:00:10:03".parse().expect("valid base MAC");

        let populated = EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: Some(error),
            last_exploration_latency: Some(std::time::Duration::from_millis(1250)),
            vendor: Some("nvidia".into()),
            managers: vec![Manager {
                id: "manager-1".to_string(),
                ethernet_interfaces: vec![EthernetInterface {
                    id: Some("manager-eth-1".to_string()),
                    description: Some("manager interface".to_string()),
                    interface_enabled: Some(true),
                    mac_address: Some(manager_mac),
                    link_status: Some("LinkUp".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            systems: vec![ComputerSystem {
                id: "system-1".to_string(),
                manufacturer: Some("NVIDIA".to_string()),
                model: Some("DGX".to_string()),
                serial_number: Some("HOST-SERIAL".to_string()),
                base_mac: Some(system_base_mac),
                ethernet_interfaces: vec![EthernetInterface {
                    id: Some("system-eth-1".to_string()),
                    description: Some("host interface".to_string()),
                    interface_enabled: Some(false),
                    mac_address: Some(system_mac),
                    link_status: Some("NoLink".to_string()),
                    ..Default::default()
                }],
                attributes: ComputerSystemAttributes {
                    nic_mode: Some(BlueFieldOperatingMode::Dpu),
                    is_infinite_boot_enabled: Some(true),
                },
                pcie_devices: vec![PCIeDevice {
                    description: Some("BlueField adapter".to_string()),
                    firmware_version: Some("32.42.1000".to_string()),
                    gpu_vendor: Some("GPU vendor".to_string()),
                    id: Some("188-0".to_string()),
                    manufacturer: Some("PCIe manufacturer".to_string()),
                    name: Some("Network Adapter".to_string()),
                    part_number: Some("900-9D3B6".to_string()),
                    serial_number: Some("PCIE-SERIAL".to_string()),
                    status: Some(SystemStatus {
                        health: Some("OK".to_string()),
                        health_rollup: Some("Warning".to_string()),
                        state: "Enabled".to_string(),
                    }),
                }],
                power_state: PowerState::PoweringOn,
                boot_order: Some(BootOrder {
                    boot_order: vec![BootOption {
                        display_name: "PXE".to_string(),
                        id: "Boot0001".to_string(),
                        boot_option_enabled: Some(true),
                        uefi_device_path: Some("PciRoot(0x0)".to_string()),
                    }],
                }),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                id: "chassis-1".to_string(),
                manufacturer: Some("NVIDIA".to_string()),
                model: Some("GB200".to_string()),
                part_number: Some("CHASSIS-PN".to_string()),
                serial_number: Some("CHASSIS-SERIAL".to_string()),
                network_adapters: vec![NetworkAdapter {
                    id: "adapter-1".to_string(),
                    manufacturer: Some("NVIDIA".to_string()),
                    model: Some("ConnectX-7".to_string()),
                    part_number: Some("ADAPTER-PN".to_string()),
                    serial_number: Some("ADAPTER-SERIAL".to_string()),
                    port_mac_addresses: Vec::new(),
                }],
                ..Default::default()
            }],
            service: vec![Service {
                id: "update-service".to_string(),
                inventories: vec![Inventory {
                    id: "inventory-1".to_string(),
                    description: Some("BMC firmware".to_string()),
                    version: Some("25.06-2".to_string()),
                    release_date: Some("2026-06-01".to_string()),
                }],
            }],
            machine_id: Some(machine_id),
            versions: HashMap::from([(FirmwareComponentType::Bmc, "25.06-2".to_string())]),
            machine_setup_status: Some(MachineSetupStatus {
                is_done: true,
                diffs: vec![MachineSetupDiff {
                    key: "boot-order".to_string(),
                    expected: "PXE".to_string(),
                    actual: "Disk".to_string(),
                }],
                evaluated_boot_interface: None,
            }),
            secure_boot_status: Some(SecureBootStatus { is_enabled: true }),
            lockdown_status: Some(LockdownStatus {
                status: InternalLockdownStatus::Partial,
                message: "one setting remains".to_string(),
            }),
            ..Default::default()
        };

        value_scenarios!(run = summarize_endpoint_report;
            "sparse report" {
                EndpointExplorationReport::default() => EndpointReportSummary {
                    endpoint_type: "Unknown".to_string(),
                    serialized_error: None,
                    error_schema: None,
                    latency: None,
                    machine_id: None,
                    vendor: None,
                    child_counts: (0, 0, 0, 0),
                    manager: None,
                    system: None,
                    chassis: None,
                    service: None,
                    machine_setup: None,
                    secure_boot_enabled: None,
                    lockdown: None,
                    firmware_versions: HashMap::new(),
                },
            }

            "populated report invokes each Redfish child conversion" {
                populated => EndpointReportSummary {
                    endpoint_type: "Bmc".to_string(),
                    serialized_error: Some(EndpointExplorationError::MissingVendor {
                        observed: None,
                    }),
                    error_schema: Some(ErrorSchemaSummary {
                        error_code: "NICO-SITEEXPLORER-122".to_string(),
                        mitigation: expected_schema.mitigation,
                        text: expected_schema.text,
                    }),
                    latency: Some((1, 250_000_000)),
                    machine_id: Some(MACHINE_ID.to_string()),
                    vendor: Some("nvidia".to_string()),
                    child_counts: (1, 1, 1, 1),
                    manager: Some(ManagerSummary {
                        id: "manager-1".to_string(),
                        interface_count: 1,
                        interface_id: Some("manager-eth-1".to_string()),
                    }),
                    system: Some(SystemSummary {
                        id: "system-1".to_string(),
                        manufacturer: Some("NVIDIA".to_string()),
                        model: Some("DGX".to_string()),
                        serial_number: Some("HOST-SERIAL".to_string()),
                        base_mac: Some("020000001003".to_string()),
                        nic_mode: Some(
                            rpc::site_explorer::BlueFieldOperatingMode::Dpu as i32,
                        ),
                        interface_count: 1,
                        interface_id: Some("system-eth-1".to_string()),
                        pcie_device_count: 1,
                        pcie_id: Some("188-0".to_string()),
                        power_state: rpc::site_explorer::PowerState::PoweringOn as i32,
                        boot_option_count: 1,
                        boot_option_id: Some("Boot0001".to_string()),
                    }),
                    chassis: Some(ChassisSummary {
                        id: "chassis-1".to_string(),
                        manufacturer: Some("NVIDIA".to_string()),
                        model: Some("GB200".to_string()),
                        part_number: Some("CHASSIS-PN".to_string()),
                        serial_number: Some("CHASSIS-SERIAL".to_string()),
                        adapter_count: 1,
                        adapter_id: Some("adapter-1".to_string()),
                    }),
                    service: Some(ServiceSummary {
                        id: "update-service".to_string(),
                        inventory_count: 1,
                        inventory_id: Some("inventory-1".to_string()),
                    }),
                    machine_setup: Some(MachineSetupSummary {
                        is_done: true,
                        diff_count: 1,
                        diff_key: Some("boot-order".to_string()),
                    }),
                    secure_boot_enabled: Some(true),
                    lockdown: Some((
                        rpc::site_explorer::InternalLockdownStatus::Partial as i32,
                        "one setting remains".to_string(),
                    )),
                    firmware_versions: HashMap::from([(
                        "bmc".to_string(),
                        "25.06-2".to_string(),
                    )]),
                },
            }
        );
    }

    #[test]
    fn last_runs_convert_to_rpc() {
        let success_started = timestamp(10);
        let success_finished = timestamp(11);
        let failed_started = timestamp(12);
        let failed_finished = timestamp(13);

        value_scenarios!(run = rpc::site_explorer::SiteExplorerLastRun::from;
            "successful run" {
                SiteExplorerLastRun {
                    started_at: success_started,
                    finished_at: success_finished,
                    success: true,
                    error: None,
                    failure_category: None,
                    endpoint_explorations: 4,
                    endpoint_explorations_success: 4,
                    endpoint_explorations_failed: 0,
                    last_successful_finished_at: Some(success_finished),
                    last_failed_finished_at: None,
                } => rpc::site_explorer::SiteExplorerLastRun {
                    started_at: success_started.to_rfc3339(),
                    finished_at: success_finished.to_rfc3339(),
                    success: true,
                    error: None,
                    endpoint_explorations: 4,
                    endpoint_explorations_success: 4,
                    endpoint_explorations_failed: 0,
                    failure_category: None,
                    last_successful_finished_at: Some(success_finished.to_rfc3339()),
                    last_failed_finished_at: None,
                },
            }

            "failed run" {
                SiteExplorerLastRun {
                    started_at: failed_started,
                    finished_at: failed_finished,
                    success: false,
                    error: Some("endpoint timed out".to_string()),
                    failure_category: Some("exploration".to_string()),
                    endpoint_explorations: 3,
                    endpoint_explorations_success: 2,
                    endpoint_explorations_failed: 1,
                    last_successful_finished_at: None,
                    last_failed_finished_at: Some(failed_finished),
                } => rpc::site_explorer::SiteExplorerLastRun {
                    started_at: failed_started.to_rfc3339(),
                    finished_at: failed_finished.to_rfc3339(),
                    success: false,
                    error: Some("endpoint timed out".to_string()),
                    endpoint_explorations: 3,
                    endpoint_explorations_success: 2,
                    endpoint_explorations_failed: 1,
                    failure_category: Some("exploration".to_string()),
                    last_successful_finished_at: None,
                    last_failed_finished_at: Some(failed_finished.to_rfc3339()),
                },
            }
        );
    }

    #[test]
    fn site_reports_convert_to_rpc() {
        value_scenarios!(
            run = |report| {
                let report = rpc::site_explorer::SiteExplorationReport::from(report);
                (
                    report.last_run.is_some(),
                    report.endpoints.len(),
                    report.managed_hosts.len(),
                )
            };
            "empty site report" {
                SiteExplorationReport {
                    last_run: None,
                    endpoints: vec![],
                    managed_hosts: vec![],
                } => (false, 0, 0),
            }

            "populated site report" {
                SiteExplorationReport {
                    last_run: Some(successful_last_run()),
                    endpoints: vec![minimal_endpoint("192.0.2.50")],
                    managed_hosts: vec![ExploredManagedHost {
                        host_bmc_ip: "192.0.2.51".parse().unwrap(),
                        dpus: vec![explored_dpu("192.0.2.52", None)],
                    }],
                } => (true, 1, 1),
            }
        );
    }

    /// Reflection-backed and generated clients retain the legacy protobuf type
    /// while new Rust callers use the observed-state alias.
    #[test]
    fn bluefield_operating_mode_preserves_legacy_protojson_descriptor() {
        let descriptor_set =
            prost_types::FileDescriptorSet::decode(rpc::REFLECTION_API_SERVICE_DESCRIPTOR).unwrap();
        let site_explorer = descriptor_set
            .file
            .iter()
            .find(|file| file.package.as_deref() == Some("site_explorer"))
            .unwrap();

        let operating_mode = site_explorer
            .enum_type
            .iter()
            .find(|enumeration| enumeration.name.as_deref() == Some("NicMode"))
            .unwrap();
        let names_and_numbers = operating_mode
            .value
            .iter()
            .map(|value| (value.name.as_deref().unwrap(), value.number.unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(names_and_numbers, [("DPU", 0), ("NIC", 1)]);

        let explored_device = site_explorer
            .message_type
            .iter()
            .find(|message| message.name.as_deref() == Some("ExploredMlxDevice"))
            .unwrap();
        let mode_field = explored_device
            .field
            .iter()
            .find(|field| field.number == Some(10))
            .unwrap();
        assert_eq!(mode_field.name.as_deref(), Some("nic_mode"));
        assert_eq!(mode_field.json_name.as_deref(), Some("nicMode"));
        assert_eq!(
            mode_field.type_name.as_deref(),
            Some(".site_explorer.NicMode")
        );

        assert_eq!(
            rpc::site_explorer::BlueFieldOperatingMode::Nic.as_str_name(),
            "NIC"
        );
    }
}
