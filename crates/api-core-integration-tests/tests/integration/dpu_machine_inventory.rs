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

use ::rpc::forge as rpc;
use carbide_test_harness::prelude::*;
use tonic::Request;

#[sqlx_test]
async fn test_create_inventory(pool: PgPool) -> Result<(), eyre::Report> {
    let env = TestHarness::builder(pool).build().await;
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let (managed_host, _) = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .build()
        .await;
    let dpu = managed_host.first_dpu();
    let expected_inventory = rpc::MachineInventory {
        components: vec![
            rpc::MachineInventorySoftwareComponent {
                name: "doca-hbn".to_string(),
                version: "1.5.0-doca2.2.0".to_string(),
                url: "nvcr.io/nvidia/doca/".to_string(),
            },
            rpc::MachineInventorySoftwareComponent {
                name: "doca-telemetry".to_string(),
                version: "1.14.2-doca2.2.0".to_string(),
                url: "nvcr.io/nvidia/doca/".to_string(),
            },
        ],
    };

    env.api()
        .update_agent_reported_inventory(Request::new(rpc::DpuAgentInventoryReport {
            machine_id: Some(dpu.id),
            inventory: Some(expected_inventory.clone()),
        }))
        .await?;

    let dpu_machine = dpu.rpc_machine().await;

    assert_eq!(dpu_machine.inventory, Some(expected_inventory));

    Ok(())
}
