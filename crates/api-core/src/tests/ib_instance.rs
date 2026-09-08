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

use std::collections::{HashMap, HashSet};

use carbide_ib_fabric::config::IBFabricConfig;
use carbide_ib_fabric::ib::{Filter, IBFabric, IBFabricManager};
use carbide_instrument::testing::MetricsCapture;
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use common::api_fixtures::ib_partition::{DEFAULT_TENANT, create_ib_partition};
use common::api_fixtures::instance::{config_for_ib_config, create_instance_with_ib_config};
use common::api_fixtures::{TestEnv, create_managed_host};
use config_version::ConfigVersion;
use db::ObjectColumnFilter;
use model::ib::{DEFAULT_IB_FABRIC_NAME, IbMembership};
use model::ib_partition::PartitionKey;
use model::machine::ManagedHostState;
use model::machine::machine_search_config::MachineSearchConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::{IbPartitionStatus, TenantState};
use tonic::Request;

use crate::api::Api;
use crate::tests::common;
use crate::tests::common::api_fixtures::TestEnvOverrides;
use crate::tests::common::postgres::wait_for_blocked_query;

// `pg_stat_activity.query` can truncate the expanded Machine snapshot query
// before its locking clause.
const MACHINE_LOCK_QUERY_MARKER: &str = "SELECT row_to_json";
const INSTANCE_LOCK_QUERY_MARKER: &str = "FOR UPDATE OF i";

/// Aborts a spawned database task if a failed lock probe unwinds the test.
struct TestTaskHandle<T>(tokio::task::JoinHandle<T>);

impl<T> TestTaskHandle<T> {
    fn new(task: tokio::task::JoinHandle<T>) -> Self {
        Self(task)
    }

    fn abort(&self) {
        self.0.abort();
    }

    async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        (&mut self.0).await
    }
}

impl<T> Drop for TestTaskHandle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn backend_pid(txn: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(txn.as_mut())
        .await
        .unwrap()
}

async fn insert_retired_ib_membership(pool: &sqlx::PgPool, membership: &IbMembership) {
    sqlx::query("INSERT INTO retired_ib_memberships (fabric, pkey, guid) VALUES ($1, $2, $3)")
        .bind(&membership.fabric)
        .bind(i32::from(u16::from(membership.pkey)))
        .bind(&membership.guid)
        .execute(pool)
        .await
        .expect("retired IB membership fixture should be insertable");
}

async fn retired_membership_count(pool: &sqlx::PgPool, membership: &IbMembership) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM retired_ib_memberships \
         WHERE fabric = $1 AND pkey = $2 AND guid = $3",
    )
    .bind(&membership.fabric)
    .bind(i32::from(u16::from(membership.pkey)))
    .bind(&membership.guid)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn instance_is_deleted(pool: &sqlx::PgPool, instance_id: InstanceId) -> bool {
    sqlx::query_scalar("SELECT deleted IS NOT NULL FROM instances WHERE id = $1")
        .bind(instance_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn get_partition_status(api: &Api, ib_partition_id: IBPartitionId) -> IbPartitionStatus {
    let segment = api
        .find_ib_partitions_by_ids(Request::new(rpc::forge::IbPartitionsByIdsRequest {
            ib_partition_ids: vec![ib_partition_id],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .ib_partitions
        .remove(0);

    segment.status.unwrap()
}

fn assert_successful_ufm_changes(metrics: &MetricsCapture, operation: &str, minimum: f64) {
    let observed = metrics.counter_delta(
        "carbide_ib_monitor_ufm_changes_applied_total",
        &[
            ("fabric", "default"),
            ("operation", operation),
            ("status", "ok"),
        ],
    );
    // Event metrics share one process-global test meter. Focused Event tests
    // pin exact counts; these checks only need to prove the instance lifecycle
    // reached the expected UFM operation.
    assert!(
        observed >= minimum,
        "expected at least {minimum} successful {operation} changes, observed {observed}"
    );
}

/// Test-specific fixture for one live membership and a requested replacement.
struct IbTransitionFixture {
    env: TestEnv,
    machine_id: MachineId,
    instance_id: InstanceId,
    initial_config_version: ConfigVersion,
    requested_config: rpc::InstanceConfig,
    metadata: rpc::Metadata,
    original_partition_id: IBPartitionId,
    requested_partition_id: IBPartitionId,
    original_membership: IbMembership,
    requested_membership: IbMembership,
}

/// Test-specific helper that enables IB for an isolated API fixture.
async fn create_ib_test_env(pool: sqlx::PgPool) -> TestEnv {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        max_partition_per_tenant: 16,
        ..Default::default()
    });
    common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await
}

/// Test-specific helper that requests one physical port on `partition_id`.
fn one_port_ib_config(partition_id: IBPartitionId) -> rpc::InstanceInfinibandConfig {
    rpc::InstanceInfinibandConfig {
        ib_interfaces: vec![rpc::InstanceIbInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            virtual_function_id: None,
            ib_partition_id: Some(partition_id),
            device: "MT2910 Family [ConnectX-7]".to_string(),
            vendor: None,
            device_instance: 0,
        }],
    }
}

/// Test-specific helper that identifies one membership on the default fabric.
fn ib_membership(pkey: PartitionKey, guid: impl Into<String>) -> IbMembership {
    IbMembership {
        fabric: DEFAULT_IB_FABRIC_NAME.to_string(),
        pkey,
        guid: guid.into(),
    }
}

/// Test-specific helper that creates an original membership and a requested
/// replacement on the same physical port.
async fn create_ib_transition_fixture(pool: sqlx::PgPool) -> IbTransitionFixture {
    let env = create_ib_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (original_partition_id, original_partition) = create_ib_partition(
        &env,
        "transition-original".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;
    let (requested_partition_id, requested_partition) = create_ib_partition(
        &env,
        "transition-requested".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;
    let original_pkey = original_partition
        .status
        .as_ref()
        .and_then(|status| status.pkey.as_deref())
        .expect("ready fixture partition must have a PKey")
        .parse()
        .expect("fixture PKey must be valid");
    let requested_pkey = requested_partition
        .status
        .as_ref()
        .and_then(|status| status.pkey.as_deref())
        .expect("ready fixture partition must have a PKey")
        .parse()
        .expect("fixture PKey must be valid");

    let managed_host = create_managed_host(&env).await;
    let (test_instance, instance) = create_instance_with_ib_config(
        &env,
        &managed_host,
        one_port_ib_config(original_partition_id),
        segment_id,
    )
    .await;
    let instance_id = test_instance.id;
    let initial_config_version = instance.config_version();
    let metadata = instance.metadata().clone();
    let mut requested_config = instance.config().inner().clone();
    requested_config.infiniband = Some(one_port_ib_config(requested_partition_id));
    let original_guid = db::instance::find_by_id(&env.pool, instance_id)
        .await
        .unwrap()
        .unwrap()
        .config
        .infiniband
        .ib_interfaces
        .into_iter()
        .next()
        .and_then(|interface| interface.guid)
        .expect("allocated fixture IB interface must have a GUID");

    IbTransitionFixture {
        env,
        machine_id: managed_host.id.into(),
        instance_id,
        initial_config_version,
        requested_config,
        metadata,
        original_partition_id,
        requested_partition_id,
        original_membership: ib_membership(original_pkey, original_guid.clone()),
        requested_membership: ib_membership(requested_pkey, original_guid),
    }
}

/// Test-specific helper that builds the replacement complete config update.
fn config_update_request(fixture: &IbTransitionFixture) -> rpc::forge::InstanceConfigUpdateRequest {
    rpc::forge::InstanceConfigUpdateRequest {
        instance_id: Some(fixture.instance_id),
        if_version_match: None,
        config: Some(fixture.requested_config.clone()),
        metadata: Some(fixture.metadata.clone()),
    }
}

/// Test-specific helper that builds a normal release request.
fn release_request(instance_id: InstanceId) -> rpc::InstanceReleaseRequest {
    rpc::InstanceReleaseRequest {
        id: Some(instance_id),
        issue: None,
        is_repair_tenant: None,
        delete_attribution: None,
    }
}

/// Test-specific helper that bounds a spawned database race task.
async fn join_test_task<T>(task: TestTaskHandle<T>, description: &str) -> T {
    match tokio::time::timeout(std::time::Duration::from_secs(30), task.join()).await {
        Ok(result) => result.unwrap_or_else(|error| panic!("{description} task failed: {error}")),
        Err(_) => panic!("{description} did not finish within 30 seconds"),
    }
}

/// Release records retirement atomically with deletion, then retries without
/// rebuilding membership data for a transition that already committed.
#[crate::sqlx_test]
async fn release_records_atomically_and_retry_skips_resolution(pool: sqlx::PgPool) {
    let fixture = create_ib_transition_fixture(pool).await;
    // Hold an uncommitted copy of the exact retirement. Release can lock and
    // reread the Instance, but its idempotent insert then waits here before it
    // can mark the Instance deleted. Canceling at that boundary must roll back
    // the whole release transaction.
    let mut retirement_guard = fixture.env.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO retired_ib_memberships (fabric, pkey, guid) VALUES ($1, $2, $3)")
        .bind(&fixture.original_membership.fabric)
        .bind(i32::from(u16::from(fixture.original_membership.pkey)))
        .bind(&fixture.original_membership.guid)
        .execute(retirement_guard.as_mut())
        .await
        .unwrap();
    let blocker_pid = backend_pid(&mut retirement_guard).await;

    let release_api = fixture.env.api.clone();
    let instance_id = fixture.instance_id;
    let release_task = TestTaskHandle::new(tokio::spawn(async move {
        release_api
            .release_instance(Request::new(release_request(instance_id)))
            .await
    }));
    wait_for_blocked_query(
        &fixture.env.pool,
        blocker_pid,
        "INSERT INTO retired_ib_memberships",
    )
    .await;

    release_task.abort();
    assert!(release_task.join().await.unwrap_err().is_cancelled());
    retirement_guard.rollback().await.unwrap();

    assert!(!instance_is_deleted(&fixture.env.pool, fixture.instance_id).await);
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.original_membership).await,
        0
    );

    fixture
        .env
        .api
        .release_instance(Request::new(release_request(fixture.instance_id)))
        .await
        .expect("release after the canceled transaction must succeed");
    assert!(instance_is_deleted(&fixture.env.pool, fixture.instance_id).await);
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.original_membership).await,
        1
    );

    sqlx::query("UPDATE ib_partitions SET status = status - 'pkey' WHERE id = $1")
        .bind(fixture.original_partition_id)
        .execute(&fixture.env.pool)
        .await
        .unwrap();
    fixture
        .env
        .api
        .release_instance(Request::new(release_request(fixture.instance_id)))
        .await
        .expect("already deleted retry must not resolve the old PKey");
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.original_membership).await,
        1
    );
}

/// Incomplete partition state must not strand its Instance. The exact
/// membership is unknowable without a PKey, but release can still mark the
/// Instance deleted and retain any other complete memberships.
#[crate::sqlx_test]
async fn release_continues_when_stored_membership_has_no_pkey(pool: sqlx::PgPool) {
    let fixture = create_ib_transition_fixture(pool).await;
    sqlx::query("UPDATE ib_partitions SET status = status - 'pkey' WHERE id = $1")
        .bind(fixture.original_partition_id)
        .execute(&fixture.env.pool)
        .await
        .unwrap();

    fixture
        .env
        .api
        .release_instance(Request::new(release_request(fixture.instance_id)))
        .await
        .expect("a missing stored PKey must not block release");

    assert!(instance_is_deleted(&fixture.env.pool, fixture.instance_id).await);
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.original_membership).await,
        0
    );
}

/// An implicit version captured before an Instance-lock wait does not silently
/// rebase. The rejected update rolls every membership change back too.
#[crate::sqlx_test]
async fn config_update_does_not_rebase_and_rolls_back_membership_transition(pool: sqlx::PgPool) {
    let fixture = create_ib_transition_fixture(pool).await;
    insert_retired_ib_membership(&fixture.env.pool, &fixture.requested_membership).await;

    // Hold the Instance while the request performs its initial unlocked read.
    // Once the request waits for this record, advance the version in the
    // guarding transaction so its captured optimistic token becomes stale.
    let mut instance_guard = fixture.env.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM instances WHERE id = $1 FOR UPDATE")
        .bind(fixture.instance_id)
        .fetch_one(instance_guard.as_mut())
        .await
        .unwrap();
    let blocker_pid = backend_pid(&mut instance_guard).await;

    let update_api = fixture.env.api.clone();
    let update_request = config_update_request(&fixture);
    let update_task = TestTaskHandle::new(tokio::spawn(async move {
        update_api
            .update_instance_config(Request::new(update_request))
            .await
    }));
    wait_for_blocked_query(&fixture.env.pool, blocker_pid, INSTANCE_LOCK_QUERY_MARKER).await;

    let concurrent_version = fixture.initial_config_version.increment();
    sqlx::query("UPDATE instances SET config_version = $1 WHERE id = $2")
        .bind(concurrent_version)
        .bind(fixture.instance_id)
        .execute(instance_guard.as_mut())
        .await
        .unwrap();
    instance_guard.commit().await.unwrap();

    let error = join_test_task(update_task, "Instance-blocked config update")
        .await
        .expect_err("a waiting update must retain its initially captured version");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    let instance = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(instance.config_version, concurrent_version);
    assert_eq!(
        instance.config.infiniband.ib_interfaces[0].ib_partition_id,
        fixture.original_partition_id
    );
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.original_membership).await,
        0
    );
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.requested_membership).await,
        1
    );
}

/// A replacement can repair stored config from before GUID allocation. The old
/// incomplete membership is skipped, while the fully resolved replacement
/// still consumes its exact retired record.
#[crate::sqlx_test]
async fn config_update_replaces_stored_membership_without_guid(pool: sqlx::PgPool) {
    let fixture = create_ib_transition_fixture(pool).await;
    insert_retired_ib_membership(&fixture.env.pool, &fixture.requested_membership).await;
    sqlx::query(
        "UPDATE instances \
         SET ib_config = ib_config #- ARRAY['ib_interfaces', '0', 'guid'] \
         WHERE id = $1",
    )
    .bind(fixture.instance_id)
    .execute(&fixture.env.pool)
    .await
    .unwrap();

    fixture
        .env
        .api
        .update_instance_config(Request::new(config_update_request(&fixture)))
        .await
        .expect("an incomplete stored membership must not block replacement");

    let instance = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        instance.config.infiniband.ib_interfaces[0].ib_partition_id,
        fixture.requested_partition_id
    );
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.original_membership).await,
        0
    );
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.requested_membership).await,
        0
    );
}

/// Retirement bookkeeping must not make a previously accepted config update
/// fail when the requested partition has no PKey. The known current membership
/// is still retired, and the unresolved requested tuple is left untouched.
#[crate::sqlx_test]
async fn config_update_continues_when_requested_membership_has_no_pkey(pool: sqlx::PgPool) {
    let fixture = create_ib_transition_fixture(pool).await;
    insert_retired_ib_membership(&fixture.env.pool, &fixture.requested_membership).await;
    sqlx::query("UPDATE ib_partitions SET status = status - 'pkey' WHERE id = $1")
        .bind(fixture.requested_partition_id)
        .execute(&fixture.env.pool)
        .await
        .unwrap();

    fixture
        .env
        .api
        .update_instance_config(Request::new(config_update_request(&fixture)))
        .await
        .expect("a missing requested PKey must not block the config update");

    let instance = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        instance.config.infiniband.ib_interfaces[0].ib_partition_id,
        fixture.requested_partition_id
    );
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.original_membership).await,
        1
    );
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.requested_membership).await,
        1
    );
}

/// A config update owns the Instance before it waits for the Machine. If
/// force-delete commits `ForceDeletion` at that boundary, the update must
/// reject without applying the requested Instance or membership changes.
#[crate::sqlx_test]
async fn force_deletion_wins_waiting_ib_config_update(pool: sqlx::PgPool) {
    let fixture = create_ib_transition_fixture(pool).await;
    insert_retired_ib_membership(&fixture.env.pool, &fixture.requested_membership).await;

    let mut force_deletion = fixture.env.pool.begin().await.unwrap();
    let machine = db::machine::find_one(
        force_deletion.as_mut(),
        &fixture.machine_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .expect("fixture Machine must exist");
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
    let blocker_pid = backend_pid(&mut force_deletion).await;

    let update_api = fixture.env.api.clone();
    let update_request = config_update_request(&fixture);
    let update_task = TestTaskHandle::new(tokio::spawn(async move {
        update_api
            .update_instance_config(Request::new(update_request))
            .await
    }));
    wait_for_blocked_query(&fixture.env.pool, blocker_pid, MACHINE_LOCK_QUERY_MARKER).await;
    force_deletion.commit().await.unwrap();

    let error = join_test_task(update_task, "ForceDeletion-blocked config update")
        .await
        .expect_err("an IB update must reject ForceDeletion after its Machine wait");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);

    let instance = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        instance.config.infiniband.ib_interfaces[0].ib_partition_id,
        fixture.original_partition_id
    );
    assert_eq!(instance.config_version, fixture.initial_config_version);
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.original_membership).await,
        0
    );
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.requested_membership).await,
        1
    );
}

/// An IB update owns the Instance and then the Machine while it retires the
/// original membership. Release waits for the Instance, rereads the committed
/// replacement, and retires that membership with deletion.
#[crate::sqlx_test]
async fn ib_update_finishes_before_waiting_release(pool: sqlx::PgPool) {
    let fixture = create_ib_transition_fixture(pool).await;
    insert_retired_ib_membership(&fixture.env.pool, &fixture.requested_membership).await;
    // Pause reuse after the config update owns both records, then start release
    // and verify it waits for the Instance before rereading the replacement.
    let mut membership_guard = fixture.env.pool.begin().await.unwrap();
    sqlx::query(
        "SELECT fabric FROM retired_ib_memberships \
         WHERE fabric = $1 AND pkey = $2 AND guid = $3 FOR UPDATE",
    )
    .bind(&fixture.requested_membership.fabric)
    .bind(i32::from(u16::from(fixture.requested_membership.pkey)))
    .bind(&fixture.requested_membership.guid)
    .fetch_one(membership_guard.as_mut())
    .await
    .unwrap();
    let membership_blocker_pid = backend_pid(&mut membership_guard).await;

    let update_api = fixture.env.api.clone();
    let update_request = config_update_request(&fixture);
    let update_task = TestTaskHandle::new(tokio::spawn(async move {
        update_api
            .update_instance_config(Request::new(update_request))
            .await
    }));
    let update_pid = wait_for_blocked_query(
        &fixture.env.pool,
        membership_blocker_pid,
        "DELETE FROM retired_ib_memberships",
    )
    .await;

    let release_api = fixture.env.api.clone();
    let instance_id = fixture.instance_id;
    let release_task = TestTaskHandle::new(tokio::spawn(async move {
        release_api
            .release_instance(Request::new(release_request(instance_id)))
            .await
    }));
    wait_for_blocked_query(&fixture.env.pool, update_pid, INSTANCE_LOCK_QUERY_MARKER).await;

    membership_guard.commit().await.unwrap();
    join_test_task(update_task, "IB config update")
        .await
        .expect("update holding the Instance must complete first");
    join_test_task(release_task, "waiting release")
        .await
        .expect("release must reread and retire the replacement membership");

    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.original_membership).await,
        1
    );
    assert_eq!(
        retired_membership_count(&fixture.env.pool, &fixture.requested_membership).await,
        1
    );
    assert!(instance_is_deleted(&fixture.env.pool, fixture.instance_id).await);
}

#[crate::sqlx_test]
async fn test_create_instance_with_ib_config(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        mtu: carbide_ib_fabric::ib::IBMtu(2),
        rate_limit: carbide_ib_fabric::ib::IBRateLimit(10),
        max_partition_per_tenant: 16,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let (ib_partition_id, ib_partition) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;
    let hex_pkey = ib_partition.status.as_ref().unwrap().pkey().to_string();
    let pkey_u16: u16 = u16::from_str_radix(
        hex_pkey
            .strip_prefix("0x")
            .expect("Pkey needs to be in hex format"),
        16,
    )
    .expect("Failed to parse string to integer");

    env.run_ib_partition_controller_iteration().await;

    let ib_partition_status = get_partition_status(&env.api, ib_partition_id).await;
    assert_eq!(
        TenantState::try_from(ib_partition_status.state).unwrap(),
        TenantState::Ready
    );
    assert_eq!(
        ib_partition.status.as_ref().unwrap().state,
        ib_partition_status.state
    );
    assert_eq!(&hex_pkey, ib_partition_status.pkey.as_ref().unwrap());
    assert!(ib_partition_status.mtu.is_none());
    assert!(ib_partition_status.rate_limit.is_none());
    assert!(ib_partition_status.service_level.is_none());

    let mh = create_managed_host(&env).await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    assert_eq!(discovery_info.infiniband_interfaces.len(), 6);
    assert!(
        machine
            .status
            .as_ref()
            .unwrap()
            .infiniband
            .as_ref()
            .is_some()
    );
    assert_eq!(
        machine
            .status
            .as_ref()
            .unwrap()
            .infiniband
            .as_ref()
            .unwrap()
            .ib_interfaces
            .len(),
        6
    );

    // select the second MT2910 Family [ConnectX-7] and the first MT27800 Family [ConnectX-5] which are sorted by slots
    let ib_config = rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: vec![
            rpc::forge::InstanceIbInterfaceConfig {
                function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
                virtual_function_id: None,
                ib_partition_id: Some(ib_partition_id),
                device: "MT2910 Family [ConnectX-7]".to_string(),
                vendor: None,
                device_instance: 1,
            },
            rpc::forge::InstanceIbInterfaceConfig {
                function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
                virtual_function_id: None,
                ib_partition_id: Some(ib_partition_id),
                device: "MT27800 Family [ConnectX-5]".to_string(),
                vendor: None,
                device_instance: 0,
            },
        ],
    };

    // Check which GUIDs these device/device_instance combinations should map to
    let machine_guids = guids_by_device(&machine);
    let guid_cx7 = machine_guids.get("MT2910 Family [ConnectX-7]").unwrap()[1].clone();
    let guid_cx5 = machine_guids.get("MT27800 Family [ConnectX-5]").unwrap()[0].clone();
    let reused_membership =
        ib_membership(PartitionKey::try_from(pkey_u16).unwrap(), guid_cx7.clone());
    // Allocation must consume the exact retirement for the membership it is
    // about to make live again.
    insert_retired_ib_membership(&env.pool, &reused_membership).await;

    let creation_metrics = MetricsCapture::start();
    let (tinstance, instance) =
        create_instance_with_ib_config(&env, &mh, ib_config.clone(), segment_id).await;
    assert_eq!(
        retired_membership_count(&env.pool, &reused_membership).await,
        0
    );
    assert_successful_ufm_changes(&creation_metrics, "bind_guid_to_pkey", 2.0);
    drop(creation_metrics);

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_missing_pkeys_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_unexpected_pkeys_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_ib_monitor_machines_with_unknown_pkeys_count")
            .unwrap(),
        "0"
    );
    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh.id.into());
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    let applied_ib_config = check_instance.config().infiniband();
    assert_eq!(*applied_ib_config, ib_config);

    let ib_status = check_instance.status().infiniband();
    assert_eq!(ib_status.configs_synced(), rpc::SyncState::Synced);
    assert_eq!(ib_status.ib_interfaces.len(), 2);

    if let Some(iface) = ib_status.ib_interfaces.first() {
        assert_eq!(iface.pf_guid, Some(guid_cx7.clone()));
        assert_eq!(iface.guid, Some(guid_cx7.clone()));
    } else {
        panic!("ib configuration is incorrect.");
    }

    if let Some(iface) = ib_status.ib_interfaces.get(1) {
        assert_eq!(iface.pf_guid, Some(guid_cx5.clone()));
        assert_eq!(iface.guid, Some(guid_cx5.clone()));
    } else {
        panic!("ib configuration is incorrect.");
    }

    // Check if ports have been registered at UFM
    let ib_conn = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    verify_pkey_guids(
        ib_conn.clone(),
        &[(pkey_u16, vec![guid_cx5.clone(), guid_cx7.clone()])],
    )
    .await;

    let ports = ib_conn
        .find_ib_port(Some(Filter {
            guids: None,
            pkey: Some(pkey_u16),
            state: None,
        }))
        .await
        .unwrap();
    assert_eq!(
        ports.len(),
        2,
        "The expected amount of ports for pkey {hex_pkey} has not been registered"
    );

    let deletion_metrics = MetricsCapture::start();
    tinstance.delete().await;
    assert_successful_ufm_changes(&deletion_metrics, "unbind_guid_from_pkey", 2.0);
    drop(deletion_metrics);

    // Check whether the IB ports are still bound to the partition
    verify_pkey_guids(ib_conn.clone(), &[(pkey_u16, vec![])]).await;
}

#[crate::sqlx_test]
async fn test_can_not_create_instance_for_not_enough_ib_device(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;

    let (ib_partition_id, _ib_partition) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let result = try_allocate_instance(
        &env,
        &host_machine_id,
        rpc::forge::InstanceInfinibandConfig {
            ib_interfaces: vec![rpc::forge::InstanceIbInterfaceConfig {
                function_type: rpc::forge::InterfaceFunctionType::Physical as _,
                virtual_function_id: None,
                ib_partition_id: Some(ib_partition_id),
                device: "MT2910 Family [ConnectX-7]".to_string(),
                vendor: None,
                device_instance: 10, // not enough devices
            }],
        },
    )
    .await;

    let error = result.expect_err("expected allocation to fail").to_string();
    assert!(
        error.contains("not enough ib device"),
        "Error message should contain 'not enough ib device', but is {error}"
    );
}

#[crate::sqlx_test]
async fn test_can_not_create_instance_for_no_ib_device(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;

    let (ib_partition_id, _ib_partition) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let result = try_allocate_instance(
        &env,
        &host_machine_id,
        rpc::forge::InstanceInfinibandConfig {
            ib_interfaces: vec![rpc::forge::InstanceIbInterfaceConfig {
                function_type: rpc::forge::InterfaceFunctionType::Physical as _,
                virtual_function_id: None,
                ib_partition_id: Some(ib_partition_id),
                device: "MT28908  Family [ConnectX-6]".to_string(), // no ib devices
                vendor: None,
                device_instance: 0,
            }],
        },
    )
    .await;

    let error = result.expect_err("expected allocation to fail").to_string();
    assert!(
        error.contains("no ib device"),
        "Error message should contain 'no ib device', but is {error}"
    );
}

#[crate::sqlx_test]
async fn test_can_not_create_instance_for_reuse_ib_device(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;

    let (ib_partition_id, _ib_partition) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let result = try_allocate_instance(
        &env,
        &host_machine_id,
        rpc::forge::InstanceInfinibandConfig {
            ib_interfaces: vec![
                rpc::forge::InstanceIbInterfaceConfig {
                    function_type: rpc::forge::InterfaceFunctionType::Physical as _,
                    virtual_function_id: None,
                    ib_partition_id: Some(ib_partition_id),
                    device: "MT2910 Family [ConnectX-7]".to_string(), // no ib devices
                    vendor: None,
                    device_instance: 0,
                },
                rpc::forge::InstanceIbInterfaceConfig {
                    function_type: rpc::forge::InterfaceFunctionType::Physical as _,
                    virtual_function_id: None,
                    ib_partition_id: Some(ib_partition_id),
                    device: "MT2910 Family [ConnectX-7]".to_string(), // no ib devices
                    vendor: None,
                    device_instance: 0,
                },
            ],
        },
    )
    .await;

    let error = result.expect_err("expected allocation to fail").to_string();
    assert!(
        error.contains("is configured more than once"),
        "Error message should contain 'is configured more than once', but is {error}"
    );
}

#[crate::sqlx_test]
async fn test_can_not_create_instance_with_inconsistent_tenant(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;

    let (ib_partition_id, _ib_partition) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        "FAKE_TENANT".to_string(),
    )
    .await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let result = try_allocate_instance(
        &env,
        &host_machine_id,
        rpc::forge::InstanceInfinibandConfig {
            ib_interfaces: vec![
                rpc::forge::InstanceIbInterfaceConfig {
                    function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
                    virtual_function_id: None,
                    ib_partition_id: Some(ib_partition_id),
                    device: "MT2910 Family [ConnectX-7]".to_string(),
                    vendor: None,
                    device_instance: 1,
                },
                rpc::forge::InstanceIbInterfaceConfig {
                    function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
                    virtual_function_id: None,
                    ib_partition_id: Some(ib_partition_id),
                    device: "MT27800 Family [ConnectX-5]".to_string(),
                    vendor: None,
                    device_instance: 0,
                },
            ],
        },
    )
    .await;

    let error = result.expect_err("expected allocation to fail").to_string();
    let expected_err =
        format!("IB partition {ib_partition_id} is not owned by the tenant {DEFAULT_TENANT}",);
    assert!(
        error.contains(&expected_err),
        "Error message should contain '{expected_err}', but is {error}"
    );
}

#[crate::sqlx_test]
async fn test_can_not_create_instance_for_inactive_ib_device(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        mtu: carbide_ib_fabric::ib::IBMtu(2),
        rate_limit: carbide_ib_fabric::ib::IBRateLimit(100),
        max_partition_per_tenant: 8,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;

    let (ib_partition_id, _ib_partition) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;

    env.run_ib_partition_controller_iteration().await;

    let mh = create_managed_host(&env).await;
    let machine = mh.host().rpc_machine().await;

    // Assign a SKU to the machine (required for IbPortDown alert tracking)
    // BOM validation is disabled in test config, so manually assign a SKU
    {
        let mut txn = env.pool.begin().await.unwrap();
        let sku = db::sku::generate_sku_from_machine(txn.as_mut(), &mh.id)
            .await
            .unwrap();
        db::sku::create(&mut txn, &sku).await.unwrap();
        db::machine::assign_sku(txn.as_mut(), &mh.id, &sku.id)
            .await
            .unwrap();
        txn.commit().await.unwrap();
    }

    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    // Use only CX7 interfaces in this test
    let device_name = "MT2910 Family [ConnectX-7]".to_string();
    let mut cx7_ifaces: Vec<_> = discovery_info
        .infiniband_interfaces
        .iter()
        .filter(|iface| {
            iface
                .pci_properties
                .as_ref()
                .unwrap()
                .description
                .as_ref()
                .unwrap()
                == &device_name
        })
        .collect();
    cx7_ifaces.sort_by_key(|iface| iface.pci_properties.as_ref().unwrap().slot());

    // Find the first IB Port of the Machine in order to down it
    let guids = [cx7_ifaces[0].guid.clone(), cx7_ifaces[1].guid.clone()];

    env.ib_fabric_manager
        .get_mock_manager()
        .set_port_state(&guids[1], false);
    env.run_ib_fabric_monitor_iteration().await;

    let result = try_allocate_instance(
        &env,
        &mh.id,
        rpc::forge::InstanceInfinibandConfig {
            ib_interfaces: vec![
                // guids[0]
                rpc::forge::InstanceIbInterfaceConfig {
                    function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
                    virtual_function_id: None,
                    ib_partition_id: Some(ib_partition_id),
                    device: device_name.clone(),
                    vendor: None,
                    device_instance: 0,
                },
                // guids[1]
                rpc::forge::InstanceIbInterfaceConfig {
                    function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
                    virtual_function_id: None,
                    ib_partition_id: Some(ib_partition_id),
                    device: device_name.clone(),
                    vendor: None,
                    device_instance: 1,
                },
            ],
        },
    )
    .await;

    let expected_err = "host is not available for allocation due to health probe alert";
    assert!(result.is_err());
    let error = result.expect_err("expected allocation to fail").to_string();
    assert!(
        error.contains(expected_err),
        "Error message should contain '{expected_err}', but is '{error}'"
    );
}

#[crate::sqlx_test]
async fn test_ib_skip_update_infiniband_status(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: false,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;

    let mh = create_managed_host(&env).await;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env
        .pool
        .clone()
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let machine = mh.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();

    assert_eq!(machine.current_state(), &ManagedHostState::Ready);
    assert!(!machine.is_dpu());
    assert!(machine.status.hardware_info.as_ref().is_some());
    assert_eq!(
        machine
            .status
            .hardware_info
            .as_ref()
            .unwrap()
            .infiniband_interfaces
            .len(),
        6
    );
    assert!(
        machine
            .status
            .infiniband_status_observation
            .as_ref()
            .is_none()
    );
}

/// Tries to create an Instance using the Forge API
/// This does not drive the instance state machine until the ready state.
pub(in crate::tests) async fn try_allocate_instance(
    env: &TestEnv,
    host_machine_id: &MachineId,
    ib_config: rpc::forge::InstanceInfinibandConfig,
) -> Result<(uuid::Uuid, rpc::forge::Instance), tonic::Status> {
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let config = config_for_ib_config(ib_config, segment_id);

    let instance = env
        .api
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            instance_id: None,
            machine_id: Some(*host_machine_id),
            instance_type_id: None,
            config: Some(config),
            metadata: Some(rpc::forge::Metadata {
                name: "test_instance".to_string(),
                description: "tests/ib_instance".to_string(),
                labels: Vec::new(),
            }),
            allow_unhealthy_machine: false,
        }))
        .await?;

    let instance = instance.into_inner();
    let instance_id: uuid::Uuid = instance.id.expect("Missing instance ID").into();
    Ok((instance_id, instance))
}

fn guids_by_device(machine: &rpc::forge::Machine) -> HashMap<String, Vec<String>> {
    let mut ib_ifaces = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap()
        .infiniband_interfaces
        .clone();
    ib_ifaces.sort_by_key(|iface| iface.pci_properties.as_ref().unwrap().slot().to_string());

    let mut guids: HashMap<String, Vec<String>> = HashMap::new();
    for iface in ib_ifaces.iter() {
        let device = iface
            .pci_properties
            .as_ref()
            .unwrap()
            .description()
            .to_string();
        guids.entry(device).or_default().push(iface.guid.clone());
    }

    guids
}

async fn verify_pkey_guids(
    ib_conn: std::sync::Arc<dyn IBFabric>,
    pkey_to_guids: &[(u16, Vec<String>)],
) {
    for (pkey_u16, expected_guids) in pkey_to_guids {
        let ports = ib_conn
            .find_ib_port(Some(Filter {
                guids: None,
                pkey: Some(*pkey_u16),
                state: None,
            }))
            .await
            .unwrap();
        let actual_guids: HashSet<String> = ports.into_iter().map(|port| port.guid).collect();
        let expected_guids: HashSet<String> = expected_guids.iter().cloned().collect();
        assert_eq!(actual_guids, expected_guids);
    }
}

/// Tests that `count_instances_referencing_partition` correctly counts instances
/// whose `ib_config` references a given IB partition.
#[crate::sqlx_test]
async fn test_count_instances_referencing_partition(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        mtu: carbide_ib_fabric::ib::IBMtu(2),
        rate_limit: carbide_ib_fabric::ib::IBRateLimit(10),
        max_partition_per_tenant: 16,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let (ib_partition_id, _ib_partition) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;

    // No instances yet — count should be 0
    let count = db::ib_partition::count_instances_referencing_partition(&env.pool, ib_partition_id)
        .await
        .unwrap();
    assert_eq!(count, 0, "No instances should reference the partition yet");

    // Create a managed host and an instance with two IB interfaces both
    // referencing the same partition. The count query uses containment (@>)
    // so it should still return 1 (one instance), not 2.
    let mh = create_managed_host(&env).await;
    let ib_config = rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: vec![
            rpc::forge::InstanceIbInterfaceConfig {
                function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
                virtual_function_id: None,
                ib_partition_id: Some(ib_partition_id),
                device: "MT2910 Family [ConnectX-7]".to_string(),
                vendor: None,
                device_instance: 0,
            },
            rpc::forge::InstanceIbInterfaceConfig {
                function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
                virtual_function_id: None,
                ib_partition_id: Some(ib_partition_id),
                device: "MT2910 Family [ConnectX-7]".to_string(),
                vendor: None,
                device_instance: 1,
            },
        ],
    };
    let (tinstance, _instance) =
        create_instance_with_ib_config(&env, &mh, ib_config, segment_id).await;

    // Two interfaces reference the partition, but it's one instance — count should be 1
    let count = db::ib_partition::count_instances_referencing_partition(&env.pool, ib_partition_id)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "One instance (with two IB interfaces) should be counted once"
    );

    // Mark the instance as deleted (set the deleted timestamp).
    let mut txn = env.pool.begin().await.unwrap();
    sqlx::query("UPDATE instances SET deleted = NOW() WHERE id = $1::uuid")
        .bind(tinstance.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // After mark_as_deleted, count should still be 1 (instance row still exists)
    let count = db::ib_partition::count_instances_referencing_partition(&env.pool, ib_partition_id)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "Instance marked as deleted should still be counted (cleanup not finished)"
    );

    let mut txn = env.pool.begin().await.unwrap();
    sqlx::query("DELETE FROM instance_addresses WHERE instance_id = $1::uuid")
        .bind(tinstance.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    sqlx::query("DELETE FROM instances WHERE id = $1::uuid")
        .bind(tinstance.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let count = db::ib_partition::count_instances_referencing_partition(&env.pool, ib_partition_id)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "No instances should reference the partition after final delete"
    );
}

/// Tests the full IB partition deletion postponement behaviour.
///
/// Scenario:
/// 1. Partition is in Ready state, instance references it in ib_config
/// 2. Instance's GUIDs are unbound from UFM (simulating the race condition)
/// 3. Partition is marked for deletion
/// 4. Controller transitions partition to Deleting
/// 5. Mock UFM returns NotFound (GUIDs were removed)
/// 6. Controller checks instance count → > 0 → waits instead of deleting
/// 7. Instance soft-deleted (marked as deleted, row still in DB)
/// 8. Controller checks instance count → still > 0 → continues to wait
/// 9. Instance fully deleted (row removed from DB after cleanup completes)
/// 10. Controller checks instance count → 0 → deletes the partition
#[crate::sqlx_test]
async fn test_postpone_partition_deletion_while_instances_reference_it(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        mtu: carbide_ib_fabric::ib::IBMtu(2),
        rate_limit: carbide_ib_fabric::ib::IBRateLimit(10),
        max_partition_per_tenant: 16,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let (ib_partition_id, ib_partition) = create_ib_partition(
        &env,
        "test_ib_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;

    let hex_pkey = ib_partition.status.as_ref().unwrap().pkey().to_string();
    let pkey_u16: u16 = u16::from_str_radix(
        hex_pkey
            .strip_prefix("0x")
            .expect("Pkey needs to be in hex format"),
        16,
    )
    .expect("Failed to parse pkey");

    let mh = create_managed_host(&env).await;
    let ib_config = rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: vec![rpc::forge::InstanceIbInterfaceConfig {
            function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
            virtual_function_id: None,
            ib_partition_id: Some(ib_partition_id),
            device: "MT2910 Family [ConnectX-7]".to_string(),
            vendor: None,
            device_instance: 0,
        }],
    };
    let (tinstance, _instance) =
        create_instance_with_ib_config(&env, &mh, ib_config, segment_id).await;

    // Simulate the race condition: the instance state handler has already
    // removed GUIDs from UFM, causing the partition to vanish in UFM,
    // but the instance row is still in the DB (not yet fully cleaned up).
    let ib_conn = env
        .ib_fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .unwrap();
    let ports = ib_conn
        .find_ib_port(Some(Filter {
            guids: None,
            pkey: Some(pkey_u16),
            state: None,
        }))
        .await
        .unwrap();
    let guids: Vec<String> = ports.into_iter().map(|p| p.guid).collect();
    if !guids.is_empty() {
        ib_conn.unbind_ib_ports(pkey_u16, guids).await.unwrap();
    }

    // Mark the partition for deletion directly in the DB, bypassing the API
    // handler which rejects deletion when instances are still bound.
    // This test exercises the controller's postponement logic, not the handler guard.
    {
        let mut txn = env.pool.begin().await.unwrap();
        let db_partition = db::ib_partition::find_by(
            &mut *txn,
            ObjectColumnFilter::One(db::ib_partition::IdColumn, &ib_partition_id),
        )
        .await
        .unwrap()
        .remove(0);
        db::ib_partition::mark_as_deleted(&db_partition, &mut txn)
            .await
            .unwrap();
        txn.commit().await.unwrap();
    }

    // Controller iteration 1: Ready + is_marked_as_deleted → transitions to Deleting
    env.run_ib_partition_controller_iteration().await;
    // Controller iteration 2: Deleting → get_ib_network → NotFound (GUIDs removed)
    //   → count_instances_referencing_partition returns 1 → WAIT
    env.run_ib_partition_controller_iteration().await;

    // Partition should still exist because instance still references it
    let partitions = env
        .api
        .find_ib_partitions_by_ids(Request::new(rpc::forge::IbPartitionsByIdsRequest {
            ib_partition_ids: vec![ib_partition_id],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .ib_partitions;
    assert!(
        !partitions.is_empty(),
        "Partition must NOT be deleted while an instance still references it"
    );

    // Soft-delete the instance (mark as deleted). The row is still in the DB
    // so the controller must continue to wait — the instance state handler
    // may still need to unbind IB ports during its cleanup sequence.
    let mut txn = env.pool.begin().await.unwrap();
    sqlx::query("UPDATE instances SET deleted = NOW() WHERE id = $1::uuid")
        .bind(tinstance.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Controller should still wait — soft-deleted instance row still exists
    env.run_ib_partition_controller_iteration().await;

    let partitions = env
        .api
        .find_ib_partitions_by_ids(Request::new(rpc::forge::IbPartitionsByIdsRequest {
            ib_partition_ids: vec![ib_partition_id],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .ib_partitions;
    assert!(
        !partitions.is_empty(),
        "Partition must NOT be deleted while a soft-deleted instance row still exists"
    );

    let mut txn = env.pool.begin().await.unwrap();
    sqlx::query("DELETE FROM instance_addresses WHERE instance_id = $1::uuid")
        .bind(tinstance.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    sqlx::query("DELETE FROM instances WHERE id = $1::uuid")
        .bind(tinstance.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Now the controller should proceed with deletion
    env.run_ib_partition_controller_iteration().await;
    env.run_ib_partition_controller_iteration().await;

    let partitions = env
        .api
        .find_ib_partitions_by_ids(Request::new(rpc::forge::IbPartitionsByIdsRequest {
            ib_partition_ids: vec![ib_partition_id],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .ib_partitions;
    assert!(
        partitions.is_empty(),
        "Partition should be deleted once no instances reference it"
    );
}

#[crate::sqlx_test]
async fn test_delete_ib_partition_rejected_with_active_instances(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    config.ib_config = Some(IBFabricConfig {
        enabled: true,
        mtu: carbide_ib_fabric::ib::IBMtu(2),
        rate_limit: carbide_ib_fabric::ib::IBRateLimit(10),
        max_partition_per_tenant: 16,
        ..Default::default()
    });

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config),
    )
    .await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let (ib_partition_id, _ib_partition) = create_ib_partition(
        &env,
        "guarded_partition".to_string(),
        DEFAULT_TENANT.to_string(),
    )
    .await;

    let mh = create_managed_host(&env).await;
    let ib_config = rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: vec![rpc::forge::InstanceIbInterfaceConfig {
            function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
            virtual_function_id: None,
            ib_partition_id: Some(ib_partition_id),
            device: "MT2910 Family [ConnectX-7]".to_string(),
            vendor: None,
            device_instance: 0,
        }],
    };
    let (tinstance, _instance) =
        create_instance_with_ib_config(&env, &mh, ib_config, segment_id).await;

    let err = env
        .api
        .delete_ib_partition(Request::new(rpc::forge::IbPartitionDeletionRequest {
            id: Some(ib_partition_id),
        }))
        .await
        .expect_err("delete should be rejected when instances are bound");

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().contains("instance(s) are still using it"),
        "Error message should mention active instances, got: {}",
        err.message()
    );

    mh.delete_instance(&env, tinstance.id).await;

    env.api
        .delete_ib_partition(Request::new(rpc::forge::IbPartitionDeletionRequest {
            id: Some(ib_partition_id),
        }))
        .await
        .expect("delete should succeed once no instances reference the partition");
}
