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

//! Legacy libredfish compatibility with the generic AMI mock BMC.

use std::sync::Arc;
use std::time::Duration;

use bmc_mock::test_support::{TEST_MAC_POOL, TestCallbacks, serve_https};
use bmc_mock::{
    BmcEvent, HardwareType, HostMachineInfo, MachineInfo, MachineRouterOptions, machine_router,
};
use libredfish::{EnabledDisabled, Endpoint, RedfishClientPool};

#[tokio::test]
async fn generic_ami_reports_and_updates_lockdown_policy() {
    let host = {
        let mut pool = TEST_MAC_POOL.lock().unwrap();
        let hardware_macs = pool.allocate_range_config().unwrap();
        HostMachineInfo::new(HardwareType::GenericAmi, vec![], &mut pool, hardware_macs)
    };
    let (router, state) = machine_router(
        &MachineInfo::Host(host),
        Arc::new(TestCallbacks::default()),
        "generic-lockdown".to_string(),
        false,
        MachineRouterOptions::default(),
    );
    let (_server, url) = serve_https("generic-lockdown", router);
    let client = RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .create_client(Endpoint {
            host: url.host_str().unwrap().to_owned(),
            port: url.port(),
            user: None,
            password: None,
        })
        .await
        .unwrap();
    assert!(client.lockdown_status().await.unwrap().is_fully_disabled());
    for mode in [EnabledDisabled::Enabled, EnabledDisabled::Disabled] {
        client.lockdown(mode).await.unwrap();
        state.on_event(&BmcEvent::PowerOn);
        let status = client.lockdown_status().await.unwrap();
        match mode {
            EnabledDisabled::Enabled => assert!(status.is_fully_enabled()),
            EnabledDisabled::Disabled => assert!(status.is_fully_disabled()),
        }
    }
}
