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

use std::collections::HashSet;
use std::sync::Arc;

use carbide_ib_fabric::IbFabricMonitor;
use carbide_ib_fabric::config::IBFabricConfig;
use carbide_ib_fabric::ib::{Filter, IBFabric, IBFabricManager};
use carbide_instrument::testing::MetricsCapture;
use carbide_uuid::instance::InstanceId;
use model::ib::{
    DEFAULT_IB_FABRIC_NAME, IBMtu, IBNetwork, IBQosConf, IBRateLimit, IBServiceLevel, IbMembership,
};
use model::ib_partition::PartitionKey;
use model::machine::ManagedHostState;
use rpc::forge::forge_server::Forge;

use crate::tests::common;
use crate::tests::common::api_fixtures::ib_partition::{DEFAULT_TENANT, create_ib_partition};
use crate::tests::common::api_fixtures::instance::create_instance_with_ib_config;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, TestManagedHost, create_managed_host,
};
use crate::tests::common::postgres::wait_for_blocked_query;

const MANAGED_TEST_PKEY: u16 = 50;
const UNMANAGED_TEST_PKEY: u16 = 0x101;
// `pg_stat_activity.query` can truncate long statements before their locking
// clause, so concurrency tests match the visible start of the `Machine` query.
const MACHINE_LOCK_QUERY_MARKER: &str = "SELECT row_to_json";

/// `enabled_ib_test_config` builds the test-specific enabled IB configuration
/// shared by monitor fixtures.
fn enabled_ib_test_config() -> crate::cfg::file::CarbideConfig {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        ..Default::default()
    });
    config
}

/// `retired_membership` is a test-specific helper that builds one exact record
/// for monitor assertions.
fn retired_membership(fabric: &str, pkey: PartitionKey, guid: &str) -> IbMembership {
    IbMembership {
        fabric: fabric.to_string(),
        pkey,
        guid: guid.to_string(),
    }
}

/// `insert_retired_membership` seeds the monitor's durable input directly so
/// these tests stay focused on monitor behavior rather than lifecycle writers.
async fn insert_retired_membership(pool: &sqlx::PgPool, membership: &IbMembership) {
    sqlx::query("INSERT INTO retired_ib_memberships (fabric, pkey, guid) VALUES ($1, $2, $3)")
        .bind(membership.fabric.clone())
        .bind(i32::from(u16::from(membership.pkey)))
        .bind(membership.guid.clone())
        .execute(pool)
        .await
        .unwrap();
}

/// `retired_memberships` is a test-specific helper that reads every record in
/// a stable order for assertions.
async fn retired_memberships(pool: &sqlx::PgPool) -> Vec<IbMembership> {
    let rows: Vec<(String, i32, String)> = sqlx::query_as(
        "SELECT fabric, pkey, guid FROM retired_ib_memberships \
         ORDER BY fabric, pkey, guid",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(fabric, pkey, guid)| {
            retired_membership(
                &fabric,
                PartitionKey::try_from(u16::try_from(pkey).unwrap()).unwrap(),
                &guid,
            )
        })
        .collect()
}

/// `retired_membership_exists` is a test-specific helper that checks whether
/// one exact record survived a monitor pass.
async fn retired_membership_exists(pool: &sqlx::PgPool, membership: &IbMembership) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM retired_ib_memberships \
         WHERE fabric = $1 AND pkey = $2 AND guid = $3)",
    )
    .bind(&membership.fabric)
    .bind(i32::from(u16::from(membership.pkey)))
    .bind(&membership.guid)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `ib_network` is a test-specific helper that builds the minimal UFM
/// partition used by cleanup tests.
fn ib_network(pkey: PartitionKey) -> IBNetwork {
    IBNetwork {
        name: "retired-membership-test".to_string(),
        pkey: pkey.into(),
        ipoib: false,
        qos_conf: Some(IBQosConf {
            mtu: IBMtu::default(),
            service_level: IBServiceLevel::default(),
            rate_limit: IBRateLimit::default(),
        }),
        associated_guids: None,
        membership: None,
    }
}

/// `membership_is_present` is a test-specific helper that reads mock UFM state
/// for cleanup assertions.
async fn membership_is_present(fabric: &Arc<dyn IBFabric>, pkey: PartitionKey, guid: &str) -> bool {
    fabric
        .find_ib_port(Some(Filter {
            guids: None,
            pkey: Some(pkey.into()),
            state: None,
        }))
        .await
        .unwrap()
        .iter()
        .any(|port| port.guid == guid)
}

/// `new_ib_monitor` is a test-specific helper that builds a fresh monitor over
/// the environment's persistent database and UFM state.
fn new_ib_monitor(env: &TestEnv) -> IbFabricMonitor {
    IbFabricMonitor::new(
        env.pool.clone(),
        env.config.ib_fabrics.clone(),
        env.test_meter.meter(),
        env.ib_fabric_manager.clone(),
        env.config.host_health,
        env.api.work_lock_manager_handle.clone(),
    )
}

/// `ib_monitor_test_env` is a test-specific helper that enables the production
/// IB monitor in an API test environment.
async fn ib_monitor_test_env(pool: sqlx::PgPool) -> TestEnv {
    common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(enabled_ib_test_config()),
    )
    .await
}

/// `live_ib_instance` is a test-specific helper that builds the exact
/// membership used by reuse and deletion tests.
async fn live_ib_instance(
    pool: sqlx::PgPool,
) -> (TestEnv, TestManagedHost, InstanceId, PartitionKey, String) {
    let env = ib_monitor_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (partition_id, partition) = create_ib_partition(
        &env,
        "retired-membership-live-partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;
    let pkey = partition.status.as_ref().unwrap().pkey().parse().unwrap();
    let managed_host = create_managed_host(&env).await;
    let ib_config = rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: vec![rpc::forge::InstanceIbInterfaceConfig {
            function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
            virtual_function_id: None,
            ib_partition_id: Some(partition_id),
            device: "MT2910 Family [ConnectX-7]".to_string(),
            vendor: None,
            device_instance: 0,
        }],
    };
    let (instance_id, guid) = {
        let (test_instance, instance) =
            create_instance_with_ib_config(&env, &managed_host, ib_config, segment_id).await;
        let guid = instance.status().infiniband().ib_interfaces[0]
            .guid()
            .to_string();
        (test_instance.id, guid)
    };

    (env, managed_host, instance_id, pkey, guid)
}

/// `test_ib_fabric_monitor` covers baseline metrics and insecure partition reporting.
#[crate::sqlx_test]
async fn test_ib_fabric_monitor(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = ib_monitor_test_env(pool.clone()).await;

    let iteration_metrics = MetricsCapture::start();
    env.run_ib_fabric_monitor_iteration().await;
    let iteration_count = iteration_metrics
        .histogram_count_delta("carbide_ib_monitor_iteration_latency_milliseconds", &[]);
    // Other API tests can drive the same process-global Event concurrently.
    // The Event-level test pins exact-once behavior; this integration check
    // only proves the real monitor path reaches the Event registry.
    assert!(
        iteration_count >= 1,
        "expected the monitor pass to record latency, observed {iteration_count}"
    );
    drop(iteration_metrics);
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_fabrics_count")
            .unwrap(),
        "1"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machine_ib_status_updates_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_ufm_version_count")
            .unwrap(),
        r#"{fabric="default",version="mock_ufm_1.0"} 1"#
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_fabric_error_count"),
        None
    );
    // The default partition is found
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_ufm_partitions_count")
            .unwrap(),
        r#"{fabric="default"} 1"#
    );
    // The fabric is configured securely
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_insecure_fabric_configuration_count")
            .unwrap(),
        r#"{fabric="default"} 0"#
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_allow_insecure_fabric_configuration_count")
            .unwrap(),
        r#"{fabric="default"} 0"#
    );

    // Set the default partition to full membership and test again
    // We now except the fabric to be reported as insecure
    env.ib_fabric_manager
        .get_mock_manager()
        .set_default_partition_membership(model::ib::IBPortMembership::Full);
    env.run_ib_fabric_monitor_iteration().await;
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_insecure_fabric_configuration_count")
            .unwrap(),
        r#"{fabric="default"} 1"#
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_allow_insecure_fabric_configuration_count")
            .unwrap(),
        r#"{fabric="default"} 0"#
    );

    Ok(())
}

/// `retired_membership_repairs_a_delayed_bind_after_monitor_restart` is a
/// test-specific regression that simulates a delayed UFM bind and verifies a
/// restarted monitor removes it again.
#[crate::sqlx_test]
async fn retired_membership_repairs_a_delayed_bind_after_monitor_restart(pool: sqlx::PgPool) {
    let env = ib_monitor_test_env(pool.clone()).await;
    let pkey = PartitionKey::try_from(MANAGED_TEST_PKEY).unwrap();
    let guid = "retired-restart-guid";
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, guid);
    let mock = env.ib_fabric_manager.get_mock_manager();
    mock.register_port(guid.to_string());
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    fabric
        .bind_ib_ports(ib_network(pkey), vec![guid.to_string()])
        .await
        .unwrap();
    insert_retired_membership(&pool, &retired).await;

    let first_monitor = new_ib_monitor(&env);
    assert_eq!(first_monitor.run_single_iteration().await.unwrap(), 1);
    assert!(!membership_is_present(&fabric, pkey, guid).await);

    // Simulate an older bind finishing after the first monitor's unbind, then
    // replace the monitor object while retaining its database state.
    fabric
        .bind_ib_ports(ib_network(pkey), vec![guid.to_string()])
        .await
        .unwrap();
    drop(first_monitor);
    let second_monitor = new_ib_monitor(&env);
    assert_eq!(second_monitor.run_single_iteration().await.unwrap(), 1);

    assert!(!membership_is_present(&fabric, pkey, guid).await);
    assert_eq!(retired_memberships(&pool).await, vec![retired]);
}

/// `retired_membership_outside_managed_range_is_left_alone` is a test-specific
/// regression that verifies a durable record cannot make the monitor manage a
/// PKey outside its configured range.
#[crate::sqlx_test]
async fn retired_membership_outside_managed_range_is_left_alone(pool: sqlx::PgPool) {
    let env = ib_monitor_test_env(pool.clone()).await;
    let pkey = PartitionKey::try_from(UNMANAGED_TEST_PKEY).unwrap();
    let guid = "retired-unmanaged-guid";
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, guid);
    let mock = env.ib_fabric_manager.get_mock_manager();
    mock.register_port(guid.to_string());
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    fabric
        .bind_ib_ports(ib_network(pkey), vec![guid.to_string()])
        .await
        .unwrap();
    insert_retired_membership(&pool, &retired).await;

    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        0
    );
    assert!(membership_is_present(&fabric, pkey, guid).await);
    assert!(retired_membership_exists(&pool, &retired).await);
}

/// `duplicate_fabric_ownership_leaves_retired_membership_alone` is a
/// test-specific regression that verifies duplicate GUID reports cannot select
/// one fabric for cleanup.
#[crate::sqlx_test]
async fn duplicate_fabric_ownership_leaves_retired_membership_alone(pool: sqlx::PgPool) {
    let mut config = enabled_ib_test_config();
    let mut other_fabric = config
        .ib_fabrics
        .get(DEFAULT_IB_FABRIC_NAME)
        .unwrap()
        .clone();
    other_fabric.endpoints = vec!["https://other-fabric.example".to_string()];
    config
        .ib_fabrics
        .insert("other-fabric".to_string(), other_fabric);
    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    // The test manager returns the same mock for both configured fabrics, so
    // each fabric reports this GUID and membership.
    let pkey = PartitionKey::try_from(MANAGED_TEST_PKEY).unwrap();
    let guid = "retired-duplicate-guid";
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, guid);
    let mock = env.ib_fabric_manager.get_mock_manager();
    mock.register_port(guid.to_string());
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    fabric
        .bind_ib_ports(ib_network(pkey), vec![guid.to_string()])
        .await
        .unwrap();
    insert_retired_membership(&pool, &retired).await;

    let duplicate_ownership_metrics = MetricsCapture::start();
    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        0
    );
    let skipped_reconciliations = duplicate_ownership_metrics.counter_delta(
        "carbide_ib_monitor_partial_failures_total",
        &[("failure_stage", "resolve_fabric_ownership")],
    );
    assert!(
        skipped_reconciliations >= 1.0,
        "expected the duplicate fabric Event, observed {skipped_reconciliations}"
    );
    drop(duplicate_ownership_metrics);
    assert!(membership_is_present(&fabric, pkey, guid).await);
    assert!(retired_membership_exists(&pool, &retired).await);
}

/// `duplicate_machine_ownership_leaves_retired_membership_alone` is a
/// test-specific regression that verifies duplicate hardware GUID claims
/// cannot select one `Machine` for cleanup.
#[crate::sqlx_test]
async fn duplicate_machine_ownership_leaves_retired_membership_alone(pool: sqlx::PgPool) {
    let env = ib_monitor_test_env(pool.clone()).await;
    let first = create_managed_host(&env).await;
    let second = create_managed_host(&env).await;
    let guid = {
        let mut topology_update = pool.begin().await.unwrap();
        let first_machine = first.host().db_machine(&mut topology_update).await;
        let guid = first_machine
            .status
            .hardware_info
            .expect("fixture machine should have hardware information")
            .infiniband_interfaces[0]
            .guid
            .clone();
        let second_machine = second.host().db_machine(&mut topology_update).await;
        let mut hardware_info = second_machine
            .status
            .hardware_info
            .expect("fixture machine should have hardware information");
        hardware_info.infiniband_interfaces[0].guid = guid.clone();
        db::machine_topology::set_topology_update_needed(
            topology_update.as_mut(),
            &second_machine.id,
            true,
        )
        .await
        .unwrap();
        db::machine_topology::create_or_update(
            topology_update.as_mut(),
            &second_machine.id,
            &hardware_info,
        )
        .await
        .unwrap();
        topology_update.commit().await.unwrap();
        guid
    };

    let pkey = PartitionKey::try_from(MANAGED_TEST_PKEY).unwrap();
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, &guid);
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    fabric
        .bind_ib_ports(ib_network(pkey), vec![guid.clone()])
        .await
        .unwrap();
    insert_retired_membership(&pool, &retired).await;

    let duplicate_ownership_metrics = MetricsCapture::start();
    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        0
    );
    let skipped_reconciliations = duplicate_ownership_metrics.counter_delta(
        "carbide_ib_monitor_partial_failures_total",
        &[("failure_stage", "resolve_machine_ownership")],
    );
    assert!(
        skipped_reconciliations >= 1.0,
        "expected the duplicate Machine Event, observed {skipped_reconciliations}"
    );
    drop(duplicate_ownership_metrics);
    assert!(membership_is_present(&fabric, pkey, &guid).await);
    assert!(retired_membership_exists(&pool, &retired).await);
}

/// `retired_membership_retries_after_ufm_failure` is a test-specific regression
/// that verifies a failed unbind keeps its record for the next monitor pass.
#[crate::sqlx_test]
async fn retired_membership_retries_after_ufm_failure(pool: sqlx::PgPool) {
    let env = ib_monitor_test_env(pool.clone()).await;
    let pkey = PartitionKey::try_from(MANAGED_TEST_PKEY).unwrap();
    let guid = "retired-retry-guid";
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, guid);
    let mock = env.ib_fabric_manager.get_mock_manager();
    mock.register_port(guid.to_string());
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    fabric
        .bind_ib_ports(ib_network(pkey), vec![guid.to_string()])
        .await
        .unwrap();
    insert_retired_membership(&pool, &retired).await;
    mock.set_unbind_failure(true);

    let failure_metrics = MetricsCapture::start();
    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        0
    );
    let failed_unbinds = failure_metrics.counter_delta(
        "carbide_ib_monitor_ufm_changes_applied_total",
        &[
            ("fabric", DEFAULT_IB_FABRIC_NAME),
            ("operation", "unbind_guid_from_pkey"),
            ("status", "error"),
        ],
    );
    assert!(
        failed_unbinds >= 1.0,
        "expected the retired membership failure Event, observed {failed_unbinds}"
    );
    drop(failure_metrics);
    assert!(membership_is_present(&fabric, pkey, guid).await);
    assert_eq!(retired_memberships(&pool).await, vec![retired.clone()]);

    mock.set_unbind_failure(false);
    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        1
    );
    assert!(!membership_is_present(&fabric, pkey, guid).await);
    assert_eq!(retired_memberships(&pool).await, vec![retired]);
}

/// `membership_state_lookup_failure_defers_only_failed_membership` is a
/// test-specific regression that cancels the locked `Machine` reread and
/// verifies that its exact membership stays suppressed while an unrelated UFM
/// cleanup still finishes.
#[crate::sqlx_test]
async fn membership_state_lookup_failure_defers_only_failed_membership(pool: sqlx::PgPool) {
    let (env, managed_host, _, pkey, guid) = live_ib_instance(pool.clone()).await;
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, &guid);
    insert_retired_membership(&pool, &retired).await;

    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    fabric
        .unbind_ib_ports(pkey.into(), vec![guid.clone()])
        .await
        .unwrap();
    let unrelated_pkey_value = if u16::from(pkey) == MANAGED_TEST_PKEY {
        MANAGED_TEST_PKEY + 1
    } else {
        MANAGED_TEST_PKEY
    };
    let unrelated_pkey = PartitionKey::try_from(unrelated_pkey_value).unwrap();
    assert_ne!(unrelated_pkey, pkey);
    fabric
        .bind_ib_ports(ib_network(unrelated_pkey), vec![guid.clone()])
        .await
        .unwrap();
    assert!(!membership_is_present(&fabric, pkey, &guid).await);
    assert!(membership_is_present(&fabric, unrelated_pkey, &guid).await);

    // Pause after the monitor has written its status observation but before it
    // reads the retired record, then make only the later `Machine` reread wait.
    let mut retired_membership_gate = pool.begin().await.unwrap();
    let retired_membership_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(retired_membership_gate.as_mut())
        .await
        .unwrap();
    sqlx::query("LOCK TABLE retired_ib_memberships IN ACCESS EXCLUSIVE MODE")
        .execute(retired_membership_gate.as_mut())
        .await
        .unwrap();

    let lookup_failure_metrics = MetricsCapture::start();
    let monitor = new_ib_monitor(&env);
    let monitor_iteration = tokio::spawn(async move { monitor.run_single_iteration().await });
    wait_for_blocked_query(&pool, retired_membership_gate_pid, "retired_ib_memberships").await;

    let mut machine_gate = pool.begin().await.unwrap();
    let machine_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(machine_gate.as_mut())
        .await
        .unwrap();
    let _: String = sqlx::query_scalar("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
        .bind(managed_host.id)
        .fetch_one(machine_gate.as_mut())
        .await
        .unwrap();
    retired_membership_gate.commit().await.unwrap();

    let monitor_pid =
        wait_for_blocked_query(&pool, machine_gate_pid, MACHINE_LOCK_QUERY_MARKER).await;
    let cancelled: bool = sqlx::query_scalar("SELECT pg_cancel_backend($1)")
        .bind(monitor_pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        cancelled,
        "expected the blocked membership reread to be cancelled"
    );

    assert_eq!(monitor_iteration.await.unwrap().unwrap(), 1);
    machine_gate.rollback().await.unwrap();

    let lookup_failures = lookup_failure_metrics.counter_delta(
        "carbide_ib_monitor_partial_failures_total",
        &[("failure_stage", "resolve_membership_state")],
    );
    assert!(
        lookup_failures >= 1.0,
        "expected the membership state lookup Event, observed {lookup_failures}"
    );
    drop(lookup_failure_metrics);
    assert!(!membership_is_present(&fabric, pkey, &guid).await);
    assert!(!membership_is_present(&fabric, unrelated_pkey, &guid).await);
    assert!(retired_membership_exists(&pool, &retired).await);
}

/// `live_membership_is_unbound_after_instance_deletion_while_retired_record_remains`
/// is a test-specific regression that follows exact reuse through `Instance`
/// deletion and verifies cleanup resumes.
#[crate::sqlx_test]
async fn live_membership_is_unbound_after_instance_deletion_while_retired_record_remains(
    pool: sqlx::PgPool,
) {
    let (env, _, instance_id, pkey, guid) = live_ib_instance(pool.clone()).await;

    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    assert!(membership_is_present(&fabric, pkey, &guid).await);
    fabric
        .unbind_ib_ports(pkey.into(), vec![guid.clone()])
        .await
        .unwrap();
    assert!(!membership_is_present(&fabric, pkey, &guid).await);

    let exact = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, &guid);
    let unrelated = [
        retired_membership("other-fabric", pkey, &guid),
        retired_membership(
            DEFAULT_IB_FABRIC_NAME,
            PartitionKey::try_from(u16::from(pkey) + 1).unwrap(),
            &guid,
        ),
        retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, "other-guid"),
    ];
    for membership in std::iter::once(&exact).chain(&unrelated) {
        insert_retired_membership(&pool, membership).await;
    }
    let all_records = std::iter::once(exact.clone())
        .chain(unrelated)
        .collect::<HashSet<_>>();

    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        1
    );

    assert!(membership_is_present(&fabric, pkey, &guid).await);
    assert_eq!(
        retired_memberships(&pool)
            .await
            .into_iter()
            .collect::<HashSet<_>>(),
        all_records
    );

    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        0
    );
    assert!(membership_is_present(&fabric, pkey, &guid).await);
    assert!(retired_membership_exists(&pool, &exact).await);

    let mut deletion = pool.begin().await.unwrap();
    db::instance::mark_as_deleted(instance_id, deletion.as_mut())
        .await
        .unwrap();
    deletion.commit().await.unwrap();

    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        1
    );
    assert!(!membership_is_present(&fabric, pkey, &guid).await);
    assert!(retired_membership_exists(&pool, &exact).await);
}

/// `membership_reused_after_snapshot_is_not_unbound` is a test-specific
/// regression that restores live configuration after an older snapshot and
/// verifies the locked reread suppresses its unbind.
#[crate::sqlx_test]
async fn membership_reused_after_snapshot_is_not_unbound(pool: sqlx::PgPool) {
    let (env, _, instance_id, pkey, guid) = live_ib_instance(pool.clone()).await;
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, &guid);
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    assert!(membership_is_present(&fabric, pkey, &guid).await);

    let instance = env.one_instance(instance_id).await;
    let original_config = instance.config().inner().clone();
    let mut config_without_ib = original_config.clone();
    config_without_ib.infiniband = Some(rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: Vec::new(),
    });
    env.api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: instance_id.into(),
                if_version_match: None,
                config: Some(config_without_ib),
                metadata: Some(instance.metadata().clone()),
            },
        ))
        .await
        .unwrap();

    // Hold only this retired record. The reuse transaction can acquire the
    // Instance and then its owning Machine, then pause when it removes the
    // retirement.
    let mut membership_guard = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT fabric FROM retired_ib_memberships \
         WHERE fabric = $1 AND pkey = $2 AND guid = $3 FOR UPDATE",
    )
    .bind(&retired.fabric)
    .bind(i32::from(u16::from(retired.pkey)))
    .bind(&retired.guid)
    .fetch_one(membership_guard.as_mut())
    .await
    .unwrap();
    let membership_guard_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(membership_guard.as_mut())
        .await
        .unwrap();

    let reuse_request = rpc::forge::InstanceConfigUpdateRequest {
        instance_id: instance_id.into(),
        if_version_match: None,
        config: Some(original_config),
        metadata: Some(instance.metadata().clone()),
    };
    let reuse_api = env.api.clone();
    let (reuse_result, monitor_result) =
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            tokio::join!(
                async {
                    reuse_api
                        .update_instance_config(tonic::Request::new(reuse_request))
                        .await
                },
                async {
                    let reuse_pid = wait_for_blocked_query(
                        &pool,
                        membership_guard_pid,
                        "DELETE FROM retired_ib_memberships",
                    )
                    .await;

                    // The monitor snapshots the still-committed empty
                    // configuration, then its locked reread waits for the reuse
                    // transaction that owns the Machine after its Instance.
                    let monitor = new_ib_monitor(&env);
                    let (monitor_result, ()) =
                        tokio::join!(monitor.run_single_iteration(), async {
                            wait_for_blocked_query(&pool, reuse_pid, MACHINE_LOCK_QUERY_MARKER)
                                .await;
                            membership_guard.commit().await.unwrap();
                        });
                    monitor_result
                }
            )
        })
        .await
        .expect("membership reuse race should finish after both lock waits");

    reuse_result.unwrap();
    assert_eq!(monitor_result.unwrap(), 0);
    assert!(membership_is_present(&fabric, pkey, &guid).await);
    assert!(!retired_membership_exists(&pool, &retired).await);
}

/// `membership_removed_from_hardware_after_snapshot_stays_retired` is a
/// test-specific regression that removes the GUID after an older snapshot and
/// verifies the retired record still controls UFM cleanup.
#[crate::sqlx_test]
async fn membership_removed_from_hardware_after_snapshot_stays_retired(pool: sqlx::PgPool) {
    let (env, managed_host, _, pkey, guid) = live_ib_instance(pool.clone()).await;
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, &guid);
    insert_retired_membership(&pool, &retired).await;
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    assert!(membership_is_present(&fabric, pkey, &guid).await);

    // Hold the retired membership query after the monitor has captured the
    // original hardware information and live `Instance` configuration.
    let mut query_gate = pool.begin().await.unwrap();
    let query_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(query_gate.as_mut())
        .await
        .unwrap();
    sqlx::query("LOCK TABLE retired_ib_memberships IN ACCESS EXCLUSIVE MODE")
        .execute(query_gate.as_mut())
        .await
        .unwrap();

    let monitor = new_ib_monitor(&env);
    let monitor_iteration = tokio::spawn(async move { monitor.run_single_iteration().await });
    wait_for_blocked_query(&pool, query_gate_pid, "retired_ib_memberships").await;

    let mut topology_update = pool.begin().await.unwrap();
    let machine = managed_host.host().db_machine(&mut topology_update).await;
    let mut hardware_info = machine
        .status
        .hardware_info
        .expect("fixture machine should have hardware information");
    hardware_info
        .infiniband_interfaces
        .retain(|interface| interface.guid != guid);
    db::machine_topology::set_topology_update_needed(topology_update.as_mut(), &machine.id, true)
        .await
        .unwrap();
    db::machine_topology::create_or_update(topology_update.as_mut(), &machine.id, &hardware_info)
        .await
        .unwrap();
    topology_update.commit().await.unwrap();
    query_gate.commit().await.unwrap();

    assert_eq!(monitor_iteration.await.unwrap().unwrap(), 1);
    assert!(!membership_is_present(&fabric, pkey, &guid).await);
    assert!(retired_membership_exists(&pool, &retired).await);
}

/// `retired_membership_checks_force_deletion_after_machine_wait` is a
/// test-specific regression that lets `ForceDeletion` finish during the
/// monitor's `Machine` wait and blocks a stale bind.
#[crate::sqlx_test]
async fn retired_membership_checks_force_deletion_after_machine_wait(pool: sqlx::PgPool) {
    let (env, managed_host, _, pkey, guid) = live_ib_instance(pool.clone()).await;
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, &guid);
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    fabric
        .unbind_ib_ports(pkey.into(), vec![guid.clone()])
        .await
        .unwrap();
    assert!(!membership_is_present(&fabric, pkey, &guid).await);
    insert_retired_membership(&pool, &retired).await;

    // Hold the retired membership table until the monitor has read the live
    // `Instance` and recorded the missing membership. This fixes the stale
    // candidate and its ordinary bind action before `ForceDeletion` begins.
    let mut query_gate = pool.begin().await.unwrap();
    let query_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(query_gate.as_mut())
        .await
        .unwrap();
    sqlx::query("LOCK TABLE retired_ib_memberships IN ACCESS EXCLUSIVE MODE")
        .execute(query_gate.as_mut())
        .await
        .unwrap();

    let monitor = new_ib_monitor(&env);
    let monitor_iteration = tokio::spawn(async move { monitor.run_single_iteration().await });
    wait_for_blocked_query(&pool, query_gate_pid, "retired_ib_memberships").await;

    let mut force_deletion = pool.begin().await.unwrap();
    let force_deletion_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(force_deletion.as_mut())
        .await
        .unwrap();
    let machine = managed_host.host().db_machine(&mut force_deletion).await;
    assert!(
        db::machine::advance(
            &machine,
            force_deletion.as_mut(),
            &ManagedHostState::ForceDeletion,
            None,
        )
        .await
        .unwrap()
    );

    query_gate.commit().await.unwrap();
    wait_for_blocked_query(&pool, force_deletion_pid, MACHINE_LOCK_QUERY_MARKER).await;
    force_deletion.commit().await.unwrap();

    assert_eq!(monitor_iteration.await.unwrap().unwrap(), 0);
    assert!(!membership_is_present(&fabric, pkey, &guid).await);
    assert!(retired_membership_exists(&pool, &retired).await);

    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        0
    );
    assert!(!membership_is_present(&fabric, pkey, &guid).await);
    assert!(retired_membership_exists(&pool, &retired).await);
}

/// `force_deletion_without_retirement_keeps_the_existing_membership` is a
/// test-specific regression that verifies `ForceDeletion` alone does not
/// change the monitor's expected PKeys.
#[crate::sqlx_test]
async fn force_deletion_without_retirement_keeps_the_existing_membership(pool: sqlx::PgPool) {
    let (env, managed_host, _, pkey, guid) = live_ib_instance(pool).await;
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    let machine = managed_host.host().db_machine(&mut txn).await;
    assert!(
        db::machine::advance(
            &machine,
            txn.as_mut(),
            &ManagedHostState::ForceDeletion,
            None,
        )
        .await
        .unwrap()
    );
    txn.commit().await.unwrap();

    assert_eq!(
        new_ib_monitor(&env).run_single_iteration().await.unwrap(),
        0
    );
    assert!(membership_is_present(&fabric, pkey, &guid).await);
}

/// `deleted_instance_keeps_the_retired_membership` is a test-specific
/// regression that proves stale configuration from a deleted `Instance` cannot
/// supersede the retired record.
#[crate::sqlx_test]
async fn deleted_instance_keeps_the_retired_membership(pool: sqlx::PgPool) {
    let (env, _, instance_id, pkey, guid) = live_ib_instance(pool.clone()).await;
    let retired = retired_membership(DEFAULT_IB_FABRIC_NAME, pkey, &guid);
    let fabric = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    assert!(membership_is_present(&fabric, pkey, &guid).await);

    // Model legacy or incomplete state where an `Instance` was marked deleted
    // without atomically recording its membership. Until the membership is
    // retired, the monitor still treats its IB configuration as expected.
    let mut txn = pool.begin().await.unwrap();
    db::instance::mark_as_deleted(instance_id, txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let monitor = new_ib_monitor(&env);
    assert_eq!(monitor.run_single_iteration().await.unwrap(), 0);
    assert!(membership_is_present(&fabric, pkey, &guid).await);

    // The durable record, rather than terminal `Instance` state by itself,
    // makes the monitor remove the membership.
    insert_retired_membership(&pool, &retired).await;
    assert_eq!(monitor.run_single_iteration().await.unwrap(), 1);

    assert!(!membership_is_present(&fabric, pkey, &guid).await);
    assert!(retired_membership_exists(&pool, &retired).await);

    assert_eq!(monitor.run_single_iteration().await.unwrap(), 0);
    assert!(!membership_is_present(&fabric, pkey, &guid).await);
    assert!(retired_membership_exists(&pool, &retired).await);
}

/// `test_ib_port_down_sets_prevent_allocations_alert` covers the production
/// allocation guard when a required IB port goes down and recovers. The
/// `PreventAllocations` alert keeps tenant allocation from reaching UFM while
/// the port cannot accept its PKey membership.
#[crate::sqlx_test]
async fn test_ib_port_down_sets_prevent_allocations_alert(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ib_monitor_test_env(pool.clone()).await;

    // Create a managed host with IB interfaces
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    // Assign a SKU to the machine (required for IB port down tracking)
    // Since BOM validation is disabled in test config, we need to manually assign a SKU
    {
        let mut txn = pool.begin().await?;
        let sku = db::sku::generate_sku_from_machine(txn.as_mut(), &host_machine_id).await?;
        db::sku::create(&mut txn, &sku).await?;
        db::machine::assign_sku(txn.as_mut(), &host_machine_id, &sku.id).await?;
        txn.commit().await?;
    }

    let machine = env.find_machine(&host_machine_id).await.remove(0);
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    let guid1 = discovery_info.infiniband_interfaces[0].guid.clone();

    let machine = env.find_machine(&host_machine_id).await.remove(0);
    let health = machine
        .status
        .as_ref()
        .unwrap()
        .health
        .as_ref()
        .expect("Machine should have health");
    let has_ib_port_down_alert = health.alerts.iter().any(|alert| alert.id == "IbPortDown");
    assert!(
        !has_ib_port_down_alert,
        "Machine should not have IbPortDown alert initially"
    );

    let ib_manager = env.ib_fabric_manager.get_mock_manager();
    ib_manager.set_port_state(&guid1, false);

    env.run_ib_fabric_monitor_iteration().await;

    let machine = env.find_machine(&host_machine_id).await.remove(0);
    let health = machine
        .status
        .as_ref()
        .unwrap()
        .health
        .as_ref()
        .expect("Machine should have health");
    let ib_port_down_alert = health.alerts.iter().find(|alert| alert.id == "IbPortDown");
    assert!(
        ib_port_down_alert.is_some(),
        "Machine should have IbPortDown alert after port goes down"
    );

    let alert = ib_port_down_alert.unwrap();
    assert!(
        alert
            .classifications
            .contains(&"PreventAllocations".to_string()),
        "IbPortDown alert should have PreventAllocations classification"
    );

    assert!(
        alert.message.contains(&guid1),
        "Alert message should contain the down GUID"
    );

    ib_manager.set_port_state(&guid1, true);

    env.run_ib_fabric_monitor_iteration().await;

    // Verify IbPortDown alert is cleared
    let machine = env.find_machine(&host_machine_id).await.remove(0);
    let health = machine
        .status
        .as_ref()
        .unwrap()
        .health
        .as_ref()
        .expect("Machine should have health");
    let has_ib_port_down_alert = health.alerts.iter().any(|alert| alert.id == "IbPortDown");
    assert!(
        !has_ib_port_down_alert,
        "IbPortDown alert should be cleared after port recovers"
    );

    Ok(())
}

/// `test_ib_multiple_ports_down` covers alert counts and partial recovery when
/// more than one required IB port goes down.
#[crate::sqlx_test]
async fn test_ib_multiple_ports_down(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = ib_monitor_test_env(pool.clone()).await;

    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    // Assign a SKU to the machine (required for IB port down tracking)
    {
        let mut txn = pool.begin().await?;
        let sku = db::sku::generate_sku_from_machine(txn.as_mut(), &host_machine_id).await?;
        db::sku::create(&mut txn, &sku).await?;
        db::machine::assign_sku(txn.as_mut(), &host_machine_id, &sku.id).await?;
        txn.commit().await?;
    }

    let machine = env.find_machine(&host_machine_id).await.remove(0);
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    let guid1 = discovery_info.infiniband_interfaces[0].guid.clone();
    let guid2 = discovery_info.infiniband_interfaces[1].guid.clone();
    let total_ports = discovery_info.infiniband_interfaces.len();

    let ib_manager = env.ib_fabric_manager.get_mock_manager();
    ib_manager.set_port_state(&guid1, false);
    ib_manager.set_port_state(&guid2, false);

    env.run_ib_fabric_monitor_iteration().await;

    let machine = env.find_machine(&host_machine_id).await.remove(0);
    let health = machine
        .status
        .as_ref()
        .unwrap()
        .health
        .as_ref()
        .expect("Machine should have health");
    let ib_port_down_alert = health
        .alerts
        .iter()
        .find(|alert| alert.id == "IbPortDown")
        .expect("Machine should have IbPortDown alert");

    assert!(
        ib_port_down_alert.message.contains("2 of"),
        "Alert should indicate 2 ports are down"
    );
    assert!(
        ib_port_down_alert
            .message
            .contains(&format!("{total_ports}")),
        "Alert should indicate total port count"
    );

    assert!(
        ib_port_down_alert.message.contains(&guid1),
        "Alert message should contain first down GUID"
    );
    assert!(
        ib_port_down_alert.message.contains(&guid2),
        "Alert message should contain second down GUID"
    );

    ib_manager.set_port_state(&guid1, true);
    env.run_ib_fabric_monitor_iteration().await;

    let machine = env.find_machine(&host_machine_id).await.remove(0);
    let health = machine
        .status
        .as_ref()
        .unwrap()
        .health
        .as_ref()
        .expect("Machine should have health");
    let ib_port_down_alert = health
        .alerts
        .iter()
        .find(|alert| alert.id == "IbPortDown")
        .expect("Machine should still have IbPortDown alert with one port down");

    assert!(
        ib_port_down_alert.message.contains("1 of"),
        "Alert should now indicate 1 port is down"
    );
    assert!(
        !ib_port_down_alert.message.contains(&guid1),
        "Alert should no longer contain recovered GUID"
    );
    assert!(
        ib_port_down_alert.message.contains(&guid2),
        "Alert should still contain down GUID"
    );

    ib_manager.set_port_state(&guid2, true);
    env.run_ib_fabric_monitor_iteration().await;

    let machine = env.find_machine(&host_machine_id).await.remove(0);
    let health = machine
        .status
        .as_ref()
        .unwrap()
        .health
        .as_ref()
        .expect("Machine should have health");
    let ib_port_down_alert = health.alerts.iter().find(|alert| alert.id == "IbPortDown");

    assert!(
        ib_port_down_alert.is_none(),
        "IbPortDown alert should be cleared when all ports are up"
    );

    Ok(())
}
