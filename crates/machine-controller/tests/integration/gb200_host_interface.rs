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

use carbide_test_harness::prelude::*;
use model::machine::{MachineState, ManagedHostState};

use crate::env::Env;

#[sqlx_test]
async fn waiting_for_discovery_only_returns_gb200_to_platform_configuration(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut env = Env::builder(pool).build().await;
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.test_harness.default_test_site_explorer();
    let (managed_host, build_data) = env
        .test_harness
        .managed_host_builder(&site_explorer, underlay_segment)
        .build()
        .await;

    managed_host
        .advance_state(ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        })
        .await;
    env.run_single_iteration().await;

    assert_eq!(
        managed_host.host.machine().await.current_state(),
        &ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        },
        "a non-GB200 host must retain the existing discovery behavior"
    );

    let mut txn = env.test_harness.db_txn().await;
    let mut endpoint =
        db::explored_endpoints::find_by_ips(txn.as_mut(), vec![build_data.host_bmc_ip()])
            .await?
            .pop()
            .expect("host Site Explorer report should exist");
    endpoint
        .report
        .systems
        .first_mut()
        .expect("host Site Explorer report should contain a system")
        .model = Some("GB200 NVL".to_string());
    assert!(
        db::explored_endpoints::try_update(
            endpoint.address,
            endpoint.report_version,
            &endpoint.report,
            endpoint.waiting_for_explorer_refresh,
            txn.as_mut(),
        )
        .await?,
        "host Site Explorer report should still have the expected version"
    );
    txn.commit().await?;

    managed_host
        .advance_state(ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        })
        .await;
    let redfish_timepoint = env.redfish_sim.timepoint();

    env.run_single_iteration().await;

    assert_eq!(
        managed_host.host.machine().await.current_state(),
        &ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForPlatformConfiguration { retry_count: 0 },
        },
    );
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts()
            .is_empty(),
        "the rewind iteration must not change BMC state or reboot the host"
    );

    Ok(())
}
