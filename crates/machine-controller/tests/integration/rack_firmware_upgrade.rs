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
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_uuid::instance::InstanceId;
use config_version::ConfigVersion;
use model::instance::NewInstance;
use model::instance::config::InstanceConfig;
use model::instance::config::extension_services::InstanceExtensionServicesConfig;
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::InstanceNetworkConfig;
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::config::spx::InstanceSpxConfig;
use model::instance::config::tenant_config::TenantConfig;
use model::machine::{HostReprovisionState, InstanceState, ManagedHostState};
use model::metadata::Metadata;
use model::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
use model::rack::{RackFirmwareUpgradeState, RackFirmwareUpgradeStatus};
use model::tenant::TenantOrganizationId;
use model::test_support::ManagedHostConfig;

use crate::env::Env;

async fn managed_host(env: &TestHarness) -> TestManagedHost {
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    env.managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await
        .0
}

async fn create_instance(env: &TestHarness, host: &TestManagedHost) -> InstanceId {
    let instance_id = InstanceId::new();
    let config = InstanceConfig {
        tenant: TenantConfig {
            tenant_organization_id: TenantOrganizationId::try_from("rack-test".to_string())
                .unwrap(),
            tenant_keyset_ids: Vec::new(),
            hostname: None,
        },
        os: OperatingSystem {
            user_data: None,
            variant: OperatingSystemVariant::Ipxe(InlineIpxe {
                ipxe_script: "#!ipxe".to_string(),
            }),
            phone_home_enabled: false,
            run_provisioning_instructions_on_every_boot: false,
        },
        network: InstanceNetworkConfig::default(),
        infiniband: InstanceInfinibandConfig::default(),
        network_security_group_id: None,
        extension_services: InstanceExtensionServicesConfig::default(),
        nvlink: InstanceNvLinkConfig::default(),
        spxconfig: InstanceSpxConfig::default(),
        power_profile: None,
    };
    let version = ConfigVersion::initial();
    let mut txn = env.db_txn().await;
    let instances = db::instance::batch_persist(
        vec![NewInstance {
            instance_id,
            machine_id: host.host.id,
            instance_type_id: None,
            config: &config,
            metadata: Metadata::default(),
            config_version: version,
            network_config_version: version,
            ib_config_version: version,
            extension_services_config_version: version,
            nvlink_config_version: version,
            spx_config_version: version,
        }],
        txn.as_mut(),
    )
    .await
    .unwrap();
    assert_eq!(instances[0].id, instance_id);
    txn.commit().await.unwrap();
    instance_id
}

fn waiting_for_rack_upgrade() -> ManagedHostState {
    ManagedHostState::HostReprovision {
        reprovision_state: HostReprovisionState::WaitingForRackFirmwareUpgrade,
        retry_count: 0,
    }
}

async fn prepare_rack_upgrade(
    env: &TestHarness,
    host: &TestManagedHost,
    managed_state: ManagedHostState,
    status: RackFirmwareUpgradeState,
    started_at_offset: chrono::Duration,
    ended_at_offset: Option<chrono::Duration>,
) {
    let mut txn = env.db_txn().await;
    db::host_machine_update::trigger_host_reprovisioning_request(
        txn.as_mut(),
        "rack-test",
        &host.host.id,
    )
    .await
    .unwrap();
    let machine = host.host.db_machine(&mut txn).await;
    let requested_at = machine
        .host_reprovision_requested
        .as_ref()
        .expect("rack reprovision request should exist")
        .requested_at;
    machine.update_state(&mut txn, managed_state).await;
    db::machine::update_rack_fw_details(
        txn.as_mut(),
        &host.host.id,
        Some(&RackFirmwareUpgradeStatus {
            task_id: "rack-job".to_string(),
            status,
            started_at: Some(requested_at + started_at_offset),
            ended_at: ended_at_offset.map(|offset| requested_at + offset),
        }),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
}

#[sqlx_test]
async fn waits_for_terminal_status(pool: PgPool) {
    let mut env = Env::builder(pool).build().await;
    let host = managed_host(&env.test_harness).await;
    prepare_rack_upgrade(
        &env.test_harness,
        &host,
        waiting_for_rack_upgrade(),
        RackFirmwareUpgradeState::InProgress,
        chrono::Duration::zero(),
        None,
    )
    .await;

    env.run_single_iteration().await;

    let machine = host.host.machine().await;
    assert!(matches!(
        machine.current_state(),
        ManagedHostState::HostReprovision {
            reprovision_state: HostReprovisionState::WaitingForRackFirmwareUpgrade,
            ..
        }
    ));
    assert!(machine.host_reprovision_requested.is_some());
}

#[sqlx_test]
async fn advances_on_completion(pool: PgPool) {
    let mut env = Env::builder(pool).build().await;
    let host = managed_host(&env.test_harness).await;
    prepare_rack_upgrade(
        &env.test_harness,
        &host,
        waiting_for_rack_upgrade(),
        RackFirmwareUpgradeState::Completed,
        chrono::Duration::zero(),
        Some(chrono::Duration::zero()),
    )
    .await;

    env.run_single_iteration().await;

    let machine = host.host.machine().await;
    assert!(matches!(machine.current_state(), ManagedHostState::Ready));
    assert!(machine.host_reprovision_requested.is_none());
}

#[sqlx_test]
async fn rack_reset_clears_host_reprovision_retry_count(pool: PgPool) {
    let mut env = Env::builder(pool).build().await;
    let host = managed_host(&env.test_harness).await;
    prepare_rack_upgrade(
        &env.test_harness,
        &host,
        ManagedHostState::HostReprovision {
            reprovision_state: HostReprovisionState::CheckingFirmwareV2 {
                firmware_type: None,
                firmware_number: None,
            },
            retry_count: 3,
        },
        RackFirmwareUpgradeState::InProgress,
        chrono::Duration::zero(),
        None,
    )
    .await;

    let mut txn = env.test_harness.db_txn().await;
    db::host_machine_update::reset_host_reprovisioning_request(txn.as_mut(), &host.host.id, false)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    env.run_single_iteration().await;

    let machine = host.host.machine().await;
    assert!(matches!(
        machine.current_state(),
        ManagedHostState::HostReprovision {
            reprovision_state: HostReprovisionState::WaitingForRackFirmwareUpgrade,
            retry_count: 0,
        }
    ));
    assert_eq!(
        machine
            .host_reprovision_requested
            .as_ref()
            .and_then(|request| request.request_reset),
        Some(false)
    );
}

#[sqlx_test]
async fn assigned_host_bypasses_legacy_check_and_returns_ready_on_completion(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut env = Env::builder(pool).build().await;
    let host = managed_host(&env.test_harness).await;
    let instance_id = create_instance(&env.test_harness, &host).await;
    prepare_rack_upgrade(
        &env.test_harness,
        &host,
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostReprovision {
                reprovision_state: HostReprovisionState::CheckingFirmwareV2 {
                    firmware_type: None,
                    firmware_number: None,
                },
            },
        },
        RackFirmwareUpgradeState::Completed,
        chrono::Duration::zero(),
        Some(chrono::Duration::zero()),
    )
    .await;

    env.run_single_iteration().await;

    let machine = host.host.machine().await;
    assert!(matches!(
        machine.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostReprovision {
                reprovision_state: HostReprovisionState::WaitingForRackFirmwareUpgrade,
            },
        }
    ));
    assert!(machine.host_reprovision_requested.is_some());

    env.run_single_iteration().await;

    let machine = host.host.machine().await;
    assert!(matches!(
        machine.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    ));
    assert!(machine.host_reprovision_requested.is_none());

    let mut txn = env.test_harness.db_txn().await;
    let attached_instance =
        db::instance::find_id_by_machine_id(txn.as_mut(), &host.host.id).await?;
    assert_eq!(attached_instance, Some(instance_id));
    txn.rollback().await?;

    Ok(())
}

#[sqlx_test]
async fn accepts_completion_when_only_ended_at_is_current(pool: PgPool) {
    let mut env = Env::builder(pool).build().await;
    let host = managed_host(&env.test_harness).await;
    prepare_rack_upgrade(
        &env.test_harness,
        &host,
        waiting_for_rack_upgrade(),
        RackFirmwareUpgradeState::Completed,
        -chrono::Duration::seconds(1),
        Some(chrono::Duration::seconds(1)),
    )
    .await;

    env.run_single_iteration().await;

    let machine = host.host.machine().await;
    assert!(matches!(machine.current_state(), ManagedHostState::Ready));
    assert!(machine.host_reprovision_requested.is_none());
}
