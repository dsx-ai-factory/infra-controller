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

#[cfg(test)]
use config_version::ConfigVersion;
#[cfg(test)]
use model::site_explorer::{EndpointExplorationReport, ExploredEndpoint, PreingestionState};

mod mock_endpoint_explorer;
mod test_site_explorer;

pub use mock_endpoint_explorer::{MockEndpointExplorationBlocker, MockEndpointExplorer};
pub use test_site_explorer::TestSiteExplorer;

#[cfg(test)]
pub(super) fn load_bf2_ep_report() -> EndpointExplorationReport {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/test_data/bf2_report.json");
    let report: EndpointExplorationReport =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(!report.systems.is_empty());
    assert!(!report.managers.is_empty());
    assert!(!report.chassis.is_empty());
    assert!(!report.service.is_empty());
    report
}

#[cfg(test)]
pub(super) fn explored_endpoint(report: EndpointExplorationReport) -> ExploredEndpoint {
    ExploredEndpoint {
        address: "10.0.0.1".parse().unwrap(),
        report,
        report_version: ConfigVersion::initial(),
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
