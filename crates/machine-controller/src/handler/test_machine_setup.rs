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

use std::collections::HashMap;

use carbide_redfish::libredfish::test_support::{RedfishSim, RedfishSimAction};
use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool};
use carbide_secrets::credentials::{CredentialKey, CredentialType};
use libredfish::BiosProfileType;
use libredfish::model::service_root::RedfishVendor;

use super::{MachineStateHandlerSiteConfig, call_machine_setup_and_handle_no_dpu_error};

/// Verify that `oem_manager_profiles` from the site config is forwarded to `machine_setup`.
///
/// This test catches regressions where the argument gets dropped or replaced with an empty map.
#[tokio::test]
async fn test_oem_manager_profiles_passed_to_machine_setup() {
    let mut config = MachineStateHandlerSiteConfig::test_default();

    // Build an oem_manager_profiles map with a Dell R760 PSU Hot Spare setting.
    // This mirrors the fix for the Dell R760 PSU fan issue (nvbugs-5834644).
    config.oem_manager_profiles = HashMap::from([(
        RedfishVendor::Dell,
        HashMap::from([(
            "r760".to_string(),
            HashMap::from([(
                BiosProfileType::Performance,
                HashMap::from([(
                    "ServerPwr.1.PSRapidOn".to_string(),
                    serde_json::Value::String("Disabled".to_string()),
                )]),
            )]),
        )]),
    )]);

    let sim = RedfishSim::default();
    let timepoint = sim.timepoint();
    let client = sim
        .create_client(
            "test-host",
            None,
            RedfishAuth::Key(CredentialKey::HostRedfish {
                credential_type: CredentialType::SiteDefault,
            }),
            None,
        )
        .await
        .unwrap();

    let result =
        call_machine_setup_and_handle_no_dpu_error(client.as_ref(), None, 1, &config).await;

    assert!(result.is_ok());

    let actions = sim.actions_since(&timepoint).all_hosts();
    assert_eq!(actions.len(), 1);
    assert_eq!(
        actions[0],
        RedfishSimAction::MachineSetup {
            oem_manager_profiles: config.oem_manager_profiles,
            boot_interface_mac: None,
            // One managed DPU, so the host's onboard management NIC is disabled.
            disable_onboard_nic: true,
        }
    );
}

/// `call_machine_setup_and_handle_no_dpu_error` must derive `disable_onboard_nic`
/// from the expected DPU count: managed-DPU hosts (count > 0) disable the onboard
/// NIC, while zero-DPU hosts (`Nic`/`Ignore`, count 0) keep it as their data path.
#[tokio::test]
async fn test_disable_onboard_nic_derived_from_expected_dpu_count() {
    let config = MachineStateHandlerSiteConfig::test_default();

    for (expected_dpu_count, expected_disable) in [(0usize, false), (1usize, true)] {
        let sim = RedfishSim::default();
        let timepoint = sim.timepoint();
        let client = sim
            .create_client(
                "test-host",
                None,
                RedfishAuth::Key(CredentialKey::HostRedfish {
                    credential_type: CredentialType::SiteDefault,
                }),
                None,
            )
            .await
            .unwrap();

        call_machine_setup_and_handle_no_dpu_error(
            client.as_ref(),
            None,
            expected_dpu_count,
            &config,
        )
        .await
        .expect("machine_setup succeeds");

        let actions = sim.actions_since(&timepoint).all_hosts();
        let disable_onboard_nic = actions
            .iter()
            .find_map(|action| match action {
                RedfishSimAction::MachineSetup {
                    disable_onboard_nic,
                    ..
                } => Some(*disable_onboard_nic),
                _ => None,
            })
            .expect("machine_setup should have been called");
        assert_eq!(
            disable_onboard_nic, expected_disable,
            "expected_dpu_count={expected_dpu_count}"
        );
    }
}
