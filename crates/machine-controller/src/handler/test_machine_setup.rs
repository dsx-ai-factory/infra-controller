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
        }
    );
}
