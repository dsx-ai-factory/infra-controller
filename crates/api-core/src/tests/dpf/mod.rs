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

mod dpu_service_sync;
mod dpu_service_sync_release;
mod happy_path;
mod reprovisioning;
mod stale_labels;
mod waiting_for_ready;

use carbide_dpf::DpuServiceVersion;
use carbide_machine_controller::dpf::MockDpfOperations;
use model::machine::ManagedHostState;

use crate::tests::common::api_fixtures::TestEnv;
use crate::tests::common::api_fixtures::test_managed_host::TestManagedHost;

// The DPF suites all want the same starting point: DPF on, with a BF3 BFB URL to hand out.
// Each of the four files below had grown its own byte-identical copy of this.
fn dpf_config() -> crate::cfg::file::DpfConfig {
    crate::cfg::file::DpfConfig {
        enabled: true,
        deployments: crate::cfg::file::DpfDeploymentsConfig {
            bf3: crate::cfg::file::DpfDeploymentConfig {
                bfb_url: Some("http://example.com/test.bfb".to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Models the successful DPF service lookup after the fixture registers a DPU CR.
fn expect_dpf_service_inventory(mock: &mut MockDpfOperations) {
    mock.expect_get_service_versions_for_dpu().returning(|_| {
        Ok(vec![DpuServiceVersion {
            name: "test-service".to_string(),
            version: "test-version".to_string(),
            url: "https://example.com/test-service".to_string(),
        }])
    });
}

// Reads the host's committed state straight from the database rather than through the RPC
// surface, which is what these suites assert transitions against.
async fn get_host_state(env: &TestEnv, mh: &TestManagedHost) -> ManagedHostState {
    let mut txn = env.db_txn().await;
    let machine = mh.host().db_machine(&mut txn).await;
    machine.state.value
}
