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

use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_uuid::instance::InstanceId;
use chrono::{DateTime, Duration, Utc};
use config_version::ConfigVersion;
use model::instance::NewInstance;
use model::instance::config::InstanceConfig;
use model::instance::config::extension_services::InstanceExtensionServicesConfig;
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::InstanceNetworkConfig;
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::config::spx::InstanceSpxConfig;
use model::instance::config::tenant_config::TenantConfig;
use model::machine::{
    CleanupContext, CleanupState, InstanceState, MachineLastRebootRequested,
    MachineLastRebootRequestedMode, MachineState, ManagedHostState, RetryInfo,
};
use model::metadata::Metadata;
use model::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
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

async fn create_instance(env: &TestHarness, host: &TestManagedHost) {
    let config = InstanceConfig {
        tenant: TenantConfig {
            tenant_organization_id: TenantOrganizationId::try_from("scout-retry-test".to_string())
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
    db::instance::batch_persist(
        vec![NewInstance {
            instance_id: InstanceId::new(),
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
    txn.commit().await.unwrap();
}

async fn host_booting_with_discovery_image(env: &TestHarness) -> TestManagedHost {
    let host = managed_host(env).await;
    create_instance(env, &host).await;
    host.advance_state(ManagedHostState::Assigned {
        instance_state: InstanceState::BootingWithDiscoveryImage {
            retry: RetryInfo { count: 0 },
        },
    })
    .await;
    host
}

async fn age_state(
    env: &Env,
    host: &TestManagedHost,
    age: Duration,
    last_cleanup_time: Option<DateTime<Utc>>,
) {
    age_state_with_last_reboot(env, host, age, last_cleanup_time, Duration::seconds(1)).await;
}

async fn age_state_with_last_reboot(
    env: &Env,
    host: &TestManagedHost,
    age: Duration,
    last_cleanup_time: Option<DateTime<Utc>>,
    last_reboot_before_state: Duration,
) {
    let machine = host.host.machine().await;
    let entered_at = Utc::now() - age;
    let stale_version: ConfigVersion = format!(
        "V{}-T{}",
        machine.state.version.version_nr(),
        entered_at.timestamp_micros()
    )
    .parse()
    .unwrap();
    let last_reboot_requested = MachineLastRebootRequested {
        time: entered_at - last_reboot_before_state,
        mode: MachineLastRebootRequestedMode::Reboot,
        restart_verified: None,
        verification_attempts: None,
    };
    let result = sqlx::query(
        "UPDATE machines SET controller_state_version = $1, last_reboot_requested = $2, \
         last_reboot_time = NULL, last_scout_contact_time = NULL, last_cleanup_time = $3 \
         WHERE id = $4",
    )
    .bind(stale_version)
    .bind(sqlx::types::Json(last_reboot_requested))
    .bind(last_cleanup_time)
    .bind(host.host.id)
    .execute(&env.test_harness.api().database_connection)
    .await
    .unwrap();

    assert_eq!(result.rows_affected(), 1);

    let machine = host.host.machine().await;
    assert!(machine.state.version.timestamp() <= Utc::now() - age);
    assert!(machine.status.last_reboot_time.is_none());
    assert!(machine.status.last_scout_contact_time.is_none());
    assert!(
        machine
            .status
            .last_reboot_requested
            .as_ref()
            .is_some_and(|request| request.time < machine.state.version.timestamp())
    );
}

async fn age_after_early_retry(env: &Env, host: &TestManagedHost) {
    let machine = host.host.machine().await;
    let entered_at = Utc::now() - Duration::minutes(101);
    let stale_version: ConfigVersion = format!(
        "V{}-T{}",
        machine.state.version.version_nr(),
        entered_at.timestamp_micros()
    )
    .parse()
    .unwrap();
    let early_retry = MachineLastRebootRequested {
        time: entered_at + Duration::minutes(10),
        mode: MachineLastRebootRequestedMode::Reboot,
        restart_verified: None,
        verification_attempts: None,
    };
    sqlx::query(
        "UPDATE machines SET controller_state_version = $1, last_reboot_requested = $2, \
         last_reboot_time = NULL, last_scout_contact_time = NULL WHERE id = $3",
    )
    .bind(stale_version)
    .bind(sqlx::types::Json(early_retry))
    .bind(host.host.id)
    .execute(&env.test_harness.api().database_connection)
    .await
    .unwrap();
}

async fn enter_host_cleanup(env: &mut Env, host: &TestManagedHost) -> ManagedHostState {
    let request = MachineLastRebootRequested {
        time: Utc::now() - Duration::seconds(1),
        mode: MachineLastRebootRequestedMode::Reboot,
        restart_verified: None,
        verification_attempts: None,
    };
    sqlx::query("UPDATE machines SET last_reboot_requested = $1 WHERE id = $2")
        .bind(sqlx::types::Json(request.clone()))
        .bind(host.host.id)
        .execute(&env.test_harness.api().database_connection)
        .await
        .unwrap();

    host.advance_state(ManagedHostState::WaitingForCleanup {
        cleanup_state: CleanupState::Init,
        cleanup_context: CleanupContext::InitialDiscovery,
    })
    .await;
    env.run_single_iteration().await;

    let state = ManagedHostState::WaitingForCleanup {
        cleanup_state: CleanupState::HostCleanup {
            boss_controller_id: None,
        },
        cleanup_context: CleanupContext::InitialDiscovery,
    };
    let machine = host.host.machine().await;
    assert_eq!(machine.current_state(), &state);
    assert_eq!(
        machine.status.last_reboot_requested.as_ref(),
        Some(&request)
    );
    assert!(request.time < machine.state.version.timestamp());
    state
}

async fn set_last_scout_contact(env: &Env, host: &TestManagedHost, contact: DateTime<Utc>) {
    sqlx::query("UPDATE machines SET last_scout_contact_time = $1 WHERE id = $2")
        .bind(contact)
        .bind(host.host.id)
        .execute(&env.test_harness.api().database_connection)
        .await
        .unwrap();
}

async fn clear_dpu_network_status(env: &Env, host: &TestManagedHost) {
    sqlx::query("UPDATE machines SET network_status_observation = NULL WHERE id = $1")
        .bind(host.first_dpu().id)
        .execute(&env.test_harness.api().database_connection)
        .await
        .unwrap();
}

async fn report_dpu_status_since_boot(host: &TestManagedHost) {
    let boot_requested_at = host
        .host
        .machine()
        .await
        .status
        .last_reboot_requested
        .expect("test host should have a boot request")
        .time;

    host.report_dpu_network_status().await;

    for dpu in &host.dpus {
        let observed_at = dpu
            .machine()
            .await
            .network_status_observation
            .expect("DPU should report network status")
            .observed_at;
        assert!(
            observed_at >= boot_requested_at,
            "DPU status must be observed after the host boot request"
        );
    }
}

async fn assert_force_restart(host: &TestManagedHost, actions: &[RedfishSimAction], reason: &str) {
    let machine = host.host.machine().await;
    assert!(
        actions.contains(&RedfishSimAction::Power(
            libredfish::SystemPowerControl::ForceRestart,
        )),
        "expected {reason}, got {actions:?}; controller outcome: {:?}",
        machine.controller_state_outcome,
    );
}

#[sqlx_test]
async fn booting_with_discovery_image_retries_once_after_ten_minutes(pool: PgPool) {
    let mut env = Env::builder(pool)
        .configure_runtime(|config| {
            config.machine_state_controller.failure_retry_time = Duration::minutes(90);
            config.machine_state_controller.scout_boot_retry_wait = Duration::minutes(10);
        })
        .build()
        .await;
    let host = host_booting_with_discovery_image(&env.test_harness).await;
    age_state(&env, &host, Duration::minutes(11), None).await;
    report_dpu_status_since_boot(&host).await;

    let first = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_force_restart(
        &host,
        &env.redfish_sim.actions_since(&first).all_hosts(),
        "an early host-only restart",
    )
    .await;
    assert!(matches!(
        host.host.machine().await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage {
                retry: RetryInfo { count: 0 },
            },
        }
    ));

    age_after_early_retry(&env, &host).await;
    let second = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_force_restart(
        &host,
        &env.redfish_sim.actions_since(&second).all_hosts(),
        "the later normal recovery restart",
    )
    .await;
    assert!(matches!(
        host.host.machine().await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage {
                retry: RetryInfo { count: 1 },
            },
        }
    ));
}

#[sqlx_test]
async fn normal_recovery_is_not_blocked_before_early_retry(pool: PgPool) {
    let mut env = Env::builder(pool)
        .configure_runtime(|config| {
            config.machine_state_controller.failure_retry_time = Duration::minutes(90);
            config.machine_state_controller.scout_boot_retry_wait = Duration::minutes(10);
        })
        .build()
        .await;
    let host = host_booting_with_discovery_image(&env.test_harness).await;
    age_state_with_last_reboot(
        &env,
        &host,
        Duration::minutes(6),
        None,
        Duration::minutes(91),
    )
    .await;
    report_dpu_status_since_boot(&host).await;

    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;

    assert_force_restart(
        &host,
        &env.redfish_sim.actions_since(&timepoint).all_hosts(),
        "the normal five-minute recovery restart",
    )
    .await;
    assert!(matches!(
        host.host.machine().await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage {
                retry: RetryInfo { count: 1 },
            },
        }
    ));
}

#[sqlx_test]
async fn recent_scout_contact_suppresses_early_retry(pool: PgPool) {
    let mut env = Env::builder(pool)
        .configure_runtime(|config| {
            config.machine_state_controller.failure_retry_time = Duration::minutes(90);
            config.machine_state_controller.scout_boot_retry_wait = Duration::minutes(10);
        })
        .build()
        .await;
    let host = host_booting_with_discovery_image(&env.test_harness).await;
    age_state(&env, &host, Duration::minutes(11), None).await;
    report_dpu_status_since_boot(&host).await;
    set_last_scout_contact(&env, &host, Utc::now()).await;

    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;

    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty(),
        "Scout contact in the current state must suppress the early restart"
    );
}

#[sqlx_test]
async fn stale_dpu_status_suppresses_early_retry(pool: PgPool) {
    let mut env = Env::builder(pool)
        .configure_runtime(|config| {
            config.machine_state_controller.failure_retry_time = Duration::minutes(90);
            config.machine_state_controller.scout_boot_retry_wait = Duration::minutes(10);
        })
        .build()
        .await;
    let host = host_booting_with_discovery_image(&env.test_harness).await;
    age_state(&env, &host, Duration::minutes(11), None).await;
    clear_dpu_network_status(&env, &host).await;

    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;

    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty(),
        "the early restart must wait for a fresh DPU-agent observation"
    );
}

#[sqlx_test]
async fn host_cleanup_retries_once_after_ten_minutes(pool: PgPool) {
    let mut env = Env::builder(pool)
        .configure_runtime(|config| {
            config.machine_state_controller.failure_retry_time = Duration::minutes(90);
            config.machine_state_controller.scout_boot_retry_wait = Duration::minutes(10);
        })
        .build()
        .await;
    let host = managed_host(&env.test_harness).await;
    let state = enter_host_cleanup(&mut env, &host).await;
    age_state(&env, &host, Duration::minutes(11), None).await;
    report_dpu_status_since_boot(&host).await;

    let first = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_force_restart(
        &host,
        &env.redfish_sim.actions_since(&first).all_hosts(),
        "an early host-only restart",
    )
    .await;
    assert_eq!(host.host.machine().await.current_state(), &state);

    age_after_early_retry(&env, &host).await;
    let second = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_force_restart(
        &host,
        &env.redfish_sim.actions_since(&second).all_hosts(),
        "the later normal recovery restart",
    )
    .await;
    assert_eq!(host.host.machine().await.current_state(), &state);
}

#[sqlx_test]
async fn completed_host_cleanup_advances_normally(pool: PgPool) {
    let mut env = Env::builder(pool)
        .configure_runtime(|config| {
            config.machine_state_controller.failure_retry_time = Duration::minutes(90);
            config.machine_state_controller.scout_boot_retry_wait = Duration::minutes(10);
        })
        .build()
        .await;
    let host = managed_host(&env.test_harness).await;
    enter_host_cleanup(&mut env, &host).await;
    age_state(&env, &host, Duration::minutes(11), Some(Utc::now())).await;

    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;

    assert_force_restart(
        &host,
        &env.redfish_sim.actions_since(&timepoint).all_hosts(),
        "the normal cleanup-complete restart",
    )
    .await;
    assert_eq!(
        host.host.machine().await.current_state(),
        &ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        }
    );
}
