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

//! Test-only helpers. Gated behind the `test-support` feature so this never
//! compiles into a production build.

use std::sync::Arc;
use std::time::Duration;

use bmc_mock::test_support::TestBmc;
use bmc_mock::test_support::axum_http_client::Error as TestBmcError;
use bmc_mock::{DpuMachineInfo, DpuSettings, HardwareType, HostMachineInfo, MachineInfo};
use model::site_explorer::EndpointExplorationReport;
use nv_redfish::core::ODataId;
use nv_redfish::{Bmc, ServiceRoot};

use crate::chassis::ExploredChassisCollection;
use crate::computer_system::{self, ExploredComputerSystem};
use crate::{
    Config, Error, ErrorClass, build_chassis_explore_config, hw, hw_type, is_bluefield_system_id,
    nv_generate_exploration_report,
};

/// Resolve the [`hw::HwType`] for an endpoint, running only the chassis +
/// computer-system exploration that detection depends on.
///
/// This performs the same detection as `nv_generate_exploration_report` but
/// returns the resolved platform type directly. Tests use it to assert the
/// detected platform: the full report only surfaces the derived BMC vendor (via
/// [`hw::HwType::bmc_vendor`]), and several distinct platforms share a vendor
/// (e.g. `Gb200` and `DgxGb300` both map to `Nvidia`, `Supermicro` and
/// `SupermicroGb300` both map to `Supermicro`), so a vendor assertion cannot
/// prove which detection arm was taken.
///
/// Not part of the production API: it re-runs the chassis/system exploration and
/// is only meaningful against a mock BMC.
pub async fn detect_hw_type<B: Bmc>(
    mut root: Arc<ServiceRoot<B>>,
    config: &Config<'_, B>,
) -> Result<Option<hw::HwType>, Error<B>> {
    let chassis_explore_config = build_chassis_explore_config(&root);
    let explored_chassis =
        ExploredChassisCollection::explore(&root, &chassis_explore_config).await?;

    if explored_chassis.is_bluefield2() {
        root = root.as_ref().clone().restrict_expand().into();
    }

    // Mirrors nv_generate_exploration_report's system selection.
    let mut systems_iter = root
        .systems()
        .await
        .map_err(Error::nv_redfish("systems"))?
        .ok_or_else(Error::bmc_not_provided("systems"))?
        .members()
        .await
        .map_err(Error::nv_redfish("systems members"))?
        .into_iter();
    let first_system = systems_iter
        .next()
        .ok_or_else(Error::bmc_not_provided("at least one computer system"))?;
    let other_system_with_bios = systems_iter.find(|system| system.raw().bios.is_some());
    let system = other_system_with_bios.unwrap_or(first_system);

    let is_bluefield_system = is_bluefield_system_id(&system.raw().id);
    let system_explore_config = computer_system::Config {
        need_oem_nvidia_bluefield: is_bluefield_system,
        ignore_500_on_bios_fetch: is_bluefield_system,
        retry_404_on_eth_interfaces: is_bluefield_system,
        explore: config,
    };
    let explored_system = ExploredComputerSystem::explore(system, &system_explore_config).await?;

    Ok(hw_type(&root, &explored_system, &explored_chassis))
}

/// Fetches supplemental adapter Port data for selected chassis without running
/// the platform-specific report pipeline.
pub async fn explore_network_adapter_ports<B: Bmc>(
    root: &ServiceRoot<B>,
    linked_chassis_ids: &[ODataId],
) -> Result<Vec<model::site_explorer::Chassis>, Error<B>> {
    let config = build_chassis_explore_config(root);
    let mut explored_chassis = ExploredChassisCollection::explore(root, &config).await?;
    explored_chassis
        .fetch_network_adapter_ports(linked_chassis_ids)
        .await;
    Ok(explored_chassis.to_model())
}

pub type MockExplorerError = Error<TestBmc>;

#[derive(Clone, Debug)]
pub struct GeneratedEndpointReport<T> {
    pub machine_info: T,
    pub report: EndpointExplorationReport,
}

#[derive(Clone, Debug)]
pub struct GeneratedManagedHostReports {
    pub host: GeneratedEndpointReport<HostMachineInfo>,
    pub dpus: Vec<GeneratedEndpointReport<DpuMachineInfo>>,
}

pub async fn generate_report_for_machine(
    machine_info: MachineInfo,
) -> Result<EndpointExplorationReport, MockExplorerError> {
    let bmc = bmc_mock::test_support::bmc_for_machine(machine_info).await;
    nv_generate_exploration_report(bmc.service_root, &explorer_config()).await
}

pub async fn generate_managed_host_reports(
    hw_type: HardwareType,
) -> Result<GeneratedManagedHostReports, MockExplorerError> {
    generate_managed_host_reports_from_info(host_info_for_hw_type(hw_type)).await
}

pub async fn generate_managed_host_reports_from_info(
    host_info: HostMachineInfo,
) -> Result<GeneratedManagedHostReports, MockExplorerError> {
    let host_report = generate_report_for_machine(MachineInfo::Host(host_info.clone())).await?;
    let mut dpus = Vec::with_capacity(host_info.dpus.len());

    for dpu_info in host_info.dpus.iter().cloned() {
        let report = generate_report_for_machine(MachineInfo::Dpu(dpu_info.clone())).await?;
        dpus.push(GeneratedEndpointReport {
            machine_info: dpu_info,
            report,
        });
    }

    Ok(GeneratedManagedHostReports {
        host: GeneratedEndpointReport {
            machine_info: host_info,
            report: host_report,
        },
        dpus,
    })
}

fn host_info_for_hw_type(hw_type: HardwareType) -> HostMachineInfo {
    let dpu_count = hw_type.fixed_number_of_dpu().unwrap_or(0);
    let mut pool = bmc_mock::test_support::TEST_MAC_POOL.lock().unwrap();
    let hw_mac_addr_pool = pool.allocate_range_config().unwrap();
    let dpus = (0..dpu_count)
        .map(|_| DpuMachineInfo::new(hw_type, &mut pool, DpuSettings::default()))
        .collect();

    HostMachineInfo::new(hw_type, dpus, &mut pool, hw_mac_addr_pool)
}

fn explorer_config() -> Config<'static, TestBmc> {
    Config {
        boot_interface_mac: None,
        error_classifier: &error_classifier,
        retry_timeout: Duration::from_millis(0),
    }
}

fn error_classifier(err: &TestBmcError) -> Option<ErrorClass> {
    match err {
        TestBmcError::InvalidResponse { status, .. } => match status.as_u16() {
            404 => Some(ErrorClass::NotFound),
            500 => Some(ErrorClass::InternalServerError),
            _ => None,
        },
        _ => None,
    }
}
