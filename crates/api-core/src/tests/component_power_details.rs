// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::rpc::forge::component_power_observation::Outcome;
use ::rpc::forge::forge_server::Forge;
use ::rpc::forge::get_component_power_details_request::Target;
use ::rpc::forge::{self as rpc};
use carbide_redfish::libredfish::test_support::{
    CreateClientCall, RedfishAuthKind, RedfishSim, RedfishSimAuthAttempt,
};
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{
    BmcCredentialType, CompositeCredentialManager, CredentialKey, CredentialReader,
    CredentialWriter, Credentials,
};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use component_manager::component_manager::ComponentManager;
use component_manager::core_compute_manager::CoreComputeTrayManager;
use component_manager::mock::{MockComputeTrayManager, MockNvSwitchManager, MockPowerShelfManager};
use component_manager::rms::RmsBackend;
use libredfish::model::service_root::RedfishVendor;
use librms::protos::rack_manager as rms;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::expected_machine::ExpectedMachineData;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::hardware_info::{DmiData, HardwareInfo};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::machine_interface_address::MachineInterfaceAssociation;
use model::power_manager::PowerState;
use model::power_shelf::NewPowerShelf;
use model::rack_type::RackProfileConfig;
use model::switch::{NewSwitch, SwitchConfig};
use model::test_support::{TEST_RMS_RACK_PROFILE_ID, power_shelf_config, rms_rack_profiles};
use tokio::sync::Notify;
use tonic::Request;

use crate::api::Api;
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON;
use crate::tests::common::api_fixtures::site_explorer::{
    TestRackDbBuilder, create_expected_switch,
};
use crate::tests::common::api_fixtures::{TestEnv, create_test_env};
use crate::tests::common::postgres::wait_for_blocked_query;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Attaches a discovered management interface so reads can reach credentials.
async fn create_bmc(
    txn: &mut sqlx::PgConnection,
    mac: MacAddress,
    association: MachineInterfaceAssociation,
) -> TestResult {
    let underlay = db::network_segment::find_by_name(&mut *txn, "UNDERLAY").await?;
    let interface = db::machine_interface::create(
        &mut *txn,
        std::slice::from_ref(&underlay),
        &mac,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    db::machine_interface::associate_bmc_interface(&interface.id, association, txn).await?;
    Ok(())
}

/// Creates a readable host, deleting the default policy row when absence is requested.
async fn create_host(
    txn: &mut sqlx::PgConnection,
    seed: u8,
    rack_id: Option<RackId>,
    policy: Option<PowerState>,
) -> TestResult<(HostMachineId, MacAddress)> {
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [seed; 32],
        MachineType::Host,
    );
    db::machine::create(
        &mut *txn,
        None,
        &machine_id,
        ManagedHostState::Ready,
        Some(&ExpectedMachineData {
            rack_id,
            ..Default::default()
        }),
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let host_id = HostMachineId::try_from(machine_id)?;
    let mac = MacAddress::new([0x02, 0, 0, 0, 0, seed]);
    create_bmc(txn, mac, MachineInterfaceAssociation::Machine(machine_id)).await?;
    if let Some(policy) = policy {
        let stored = db::power_options::get_by_ids(&[host_id], &mut *txn)
            .await?
            .pop()
            .expect("machine creation inserts a policy");
        db::power_options::update_desired_state(
            &host_id,
            policy,
            &stored.desired_power_state_version,
            txn,
        )
        .await?;
    } else {
        sqlx::query("DELETE FROM power_options WHERE host_id = $1")
            .bind(host_id)
            .execute(txn)
            .await?;
    }
    Ok((host_id, mac))
}

/// Selects the only passive-capable manager in an otherwise unsupported fixture.
#[derive(Clone, Copy)]
enum PowerRole {
    Compute,
    Switch,
    Shelf,
}

/// Matches rack capabilities to the inventory each fixture actually creates.
fn power_profiles(compute: u32, switch: u32, shelf: u32) -> RackProfileConfig {
    let mut profiles = rms_rack_profiles();
    let capabilities = &mut profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap()
        .rack_capabilities;
    capabilities.compute.count = compute;
    capabilities.switch.count = switch;
    capabilities.power_shelf.count = shelf;
    profiles
}

/// Keeps capabilities asymmetric so consulting the wrong role fails visibly.
fn api_with_rms(
    env: &TestEnv,
    pool: sqlx::PgPool,
    profiles: RackProfileConfig,
    role: PowerRole,
) -> Api {
    let backend = Arc::new(RmsBackend::new(
        env.rms_sim.as_rms_client().unwrap(),
        None,
        pool.clone(),
        Arc::new(profiles),
        false,
    ));
    // Only the requested role supports passive reads, so querying another
    // manager's capability cannot accidentally pass a successful read test.
    let mut manager = ComponentManager::new(
        Arc::new(MockNvSwitchManager::default()),
        Arc::new(MockPowerShelfManager),
        Arc::new(MockComputeTrayManager),
        false,
        false,
        false,
    );
    match role {
        PowerRole::Compute => manager.compute_tray = backend,
        PowerRole::Switch => manager.nv_switch = backend,
        PowerRole::Shelf => manager.power_shelf = backend,
    }
    TestApiBuilder::new(
        pool,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_credential_manager(env.api.credential_manager.clone())
    .with_component_manager(Arc::new(manager))
    .build()
}

/// Describes persisted policy independently of whether a hardware read succeeds.
fn stored_policy(state: rpc::PowerState) -> rpc::ComponentPowerPolicy {
    rpc::ComponentPowerPolicy {
        status: rpc::ComponentPowerPolicyStatus::Available as i32,
        state: Some(state as i32),
        source: Some(rpc::ComponentPowerSource::CorePowerPolicy as i32),
        updated_at: None,
    }
}

/// Requires a categorized failure, so an absent result cannot pass an error assertion.
fn observation_error(actual: &rpc::ComponentPowerObservation) -> &rpc::ComponentPowerError {
    let Some(Outcome::Error(error)) = actual.outcome.as_ref() else {
        panic!("expected a power observation error, received {actual:?}");
    };
    error
}

/// Gates lookups and records admissions and drops without timing-based absence checks.
struct GatedPowerCredentialReader {
    gates: Vec<(MacAddress, Notify)>,
    started: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    cancelled: AtomicUsize,
    changed: Notify,
}

impl GatedPowerCredentialReader {
    /// Assigns each requested host a stable gate and admission bit.
    fn new(hosts: &[(HostMachineId, MacAddress)]) -> Self {
        Self {
            gates: hosts.iter().map(|(_, mac)| (*mac, Notify::new())).collect(),
            started: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            cancelled: AtomicUsize::new(0),
            changed: Notify::new(),
        }
    }

    /// Waits for positive admission evidence while the caller polls the request.
    async fn wait_for_started(&self, count: u32) {
        while self.started.load(Ordering::SeqCst).count_ones() < count {
            self.changed.notified().await;
        }
    }

    /// Completes one lookup with missing credentials, freeing a read slot.
    fn release(&self, index: usize) {
        self.gates[index].1.notify_one();
    }
}

/// Distinguishes completed lookups from cancellation when their futures are dropped.
struct ActivePowerRead<'a> {
    reader: &'a GatedPowerCredentialReader,
    completed: bool,
}

impl Drop for ActivePowerRead<'_> {
    fn drop(&mut self) {
        self.reader.active.fetch_sub(1, Ordering::SeqCst);
        if !self.completed {
            self.reader.cancelled.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[async_trait::async_trait]
impl CredentialReader for GatedPowerCredentialReader {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        let CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
        } = key
        else {
            panic!("unexpected credential key: {key:?}");
        };
        let index = self
            .gates
            .iter()
            .position(|(mac, _)| mac == bmc_mac_address)
            .unwrap();
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let mut guard = ActivePowerRead {
            reader: self,
            completed: false,
        };
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.started.fetch_or(1 << index, Ordering::SeqCst);
        self.changed.notify_one();
        self.gates[index].1.notified().await;
        guard.completed = true;
        Ok(None)
    }
}

/// Returns valid credentials after a chosen lookup is released. Recording every
/// key distinguishes the guard after BMC credentials from the later NVOS guard.
struct ExpiringPowerCredentialReader {
    blocked_key: CredentialKey,
    started: Notify,
    release: Notify,
    requested_keys: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl CredentialReader for ExpiringPowerCredentialReader {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        self.requested_keys
            .lock()
            .unwrap()
            .push(key.to_key_str().into_owned());
        if key.to_key_str() == self.blocked_key.to_key_str() {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(Some(Credentials::new("power-reader", "test-password")))
    }
}

/// Makes credential work observable on paths that must short-circuit before it.
#[derive(Default)]
struct CountingCredentialReader(AtomicUsize);

#[async_trait::async_trait]
impl CredentialReader for CountingCredentialReader {
    async fn get_credentials(
        &self,
        _: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}

/// Preserves fixture order so response ordering is checked against the submitted IDs.
fn host_request(
    hosts: &[(HostMachineId, MacAddress)],
) -> Request<rpc::GetComponentPowerDetailsRequest> {
    Request::new(rpc::GetComponentPowerDetailsRequest {
        target: Some(Target::MachineIds(::rpc::common::HostMachineIdList {
            machine_ids: hosts.iter().map(|(id, _)| *id).collect(),
        })),
    })
}

/// Exposes snapshot ownership with one slot and an acquire timeout beyond the API budget.
async fn single_connection_pool(pool: &sqlx::PgPool) -> Result<sqlx::PgPool, sqlx::Error> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(pool.connect_options().as_ref().clone())
        .await
}

/// Reads the seeded interface address independently of the handler's endpoint assembly.
async fn management_ip(
    txn: &mut sqlx::PgConnection,
    mac: MacAddress,
) -> TestResult<std::net::IpAddr> {
    let interface = db::machine_interface::find_by_mac_address(&mut *txn, mac)
        .await?
        .into_iter()
        .next()
        .expect("fixture management interface");
    Ok(
        db::machine_interface_address::find_for_interface(txn, interface.id)
            .await?
            .first()
            .expect("fixture management address")
            .address,
    )
}

/// Detects substitution between BMC, NVOS, and PMC addresses or credentials.
fn assert_rms_endpoint(
    endpoint: &rms::Endpoint,
    ip: std::net::IpAddr,
    mac: MacAddress,
    username: &str,
    password: &str,
) {
    let interface = endpoint.interface.as_ref().expect("management interface");
    assert_eq!(interface.ip_address, ip.to_string());
    assert_eq!(interface.mac_address, mac.to_string());
    let Some(rms::credentials::Auth::UserPass(credentials)) = endpoint
        .credentials
        .as_ref()
        .and_then(|credentials| credentials.auth.as_ref())
    else {
        panic!("supplied management credentials");
    };
    assert_eq!(credentials.username, username);
    assert_eq!(credentials.password, password);
}

/// Enforces eight active reads, work-conserving admission, order, and snapshot release.
#[crate::sqlx_test]
async fn later_targets_start_before_first_credential_lookup_finishes(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let rack_id = RackId::new("power-details-concurrency");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    let mut hosts = Vec::new();
    for seed in 10..19 {
        hosts.push(create_host(&mut txn, seed, Some(rack_id.clone()), Some(PowerState::On)).await?);
    }
    txn.commit().await?;
    let reader = Arc::new(GatedPowerCredentialReader::new(&hosts));
    let limited_pool = single_connection_pool(&pool).await?;
    let mut api = api_with_rms(
        &env,
        limited_pool.clone(),
        power_profiles(9, 0, 0),
        PowerRole::Compute,
    );
    api.credential_manager = Arc::new(CompositeCredentialManager::new(
        reader.clone(),
        env.test_credential_manager.clone(),
    ));
    // Expire before the API's read deadline can release the first occupied slot.
    let response = tokio::time::timeout(Duration::from_secs(10), async {
        let request = api.get_component_power_details(host_request(&hosts));
        tokio::pin!(request);
        tokio::select! {
            _ = reader.wait_for_started(8) => {}
            result = &mut request => panic!("request completed before releasing first read: {result:?}"),
        }
        assert_eq!(reader.started.load(Ordering::SeqCst), (1 << 8) - 1);
        assert_eq!(reader.peak.load(Ordering::SeqCst), 8);
        // A read must not retain the snapshot's sole database connection while
        // it waits on credentials or hardware.
        let connection = tokio::time::timeout(Duration::from_secs(2), limited_pool.acquire())
            .await.expect("snapshot connection must be released before live reads")
            .expect("acquire released snapshot connection");
        drop(connection);
        reader.release(1);
        tokio::select! {
            _ = reader.wait_for_started(9) => {}
            result = &mut request => panic!("request completed while first read remains blocked: {result:?}"),
        }
        assert_eq!(reader.peak.load(Ordering::SeqCst), 8);
        for index in 0..hosts.len() {
            reader.release(index);
        }
        request.await
    })
    .await
    .expect("ninth target must start while first read remains blocked")?
    .into_inner();
    assert_eq!(response.entries.len(), hosts.len());
    for (entry, (id, _)) in response.entries.iter().zip(&hosts) {
        assert_eq!(entry.component_id, id.to_string());
        assert_eq!(
            observation_error(entry.actual.as_ref().unwrap()).code,
            rpc::ComponentPowerErrorCode::CredentialsUnavailable as i32,
        );
    }
    assert_eq!(reader.active.load(Ordering::SeqCst), 0);
    assert_eq!(reader.cancelled.load(Ordering::SeqCst), 0);
    limited_pool.close().await;
    Ok(())
}

/// Delayed admission shares the original deadline; expiry and caller drop own pending work.
#[crate::sqlx_test]
async fn read_deadline_and_caller_cancellation_drop_unfinished_work(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let rack_id = RackId::new("power-details-deadline");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    let mut hosts = Vec::new();
    for seed in 30..41 {
        hosts.push(create_host(&mut txn, seed, Some(rack_id.clone()), Some(PowerState::On)).await?);
    }
    txn.commit().await?;
    let mut api = api_with_rms(&env, pool, power_profiles(11, 0, 0), PowerRole::Compute);
    let reader = Arc::new(GatedPowerCredentialReader::new(&hosts));
    api.credential_manager = Arc::new(CompositeCredentialManager::new(
        reader.clone(),
        env.test_credential_manager.clone(),
    ));
    // One completed result leaves eight reads active and two queued.
    reader.release(0);
    let mut request = Box::pin(api.get_component_power_details(host_request(&hosts)));
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = reader.wait_for_started(9) => {}
            result = &mut request => panic!("request completed before the deadline: {result:?}"),
        }
    })
    .await
    .expect("snapshot and initial credential reads must complete");
    assert_eq!(reader.started.load(Ordering::SeqCst), (1 << 9) - 1);
    // Database setup and snapshot I/O finish before time is paused.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(4)).await;
    reader.release(1);
    tokio::time::timeout(Duration::from_millis(1), async {
        tokio::select! {
            _ = reader.wait_for_started(10) => {}
            result = &mut request => panic!("request completed before delayed admission: {result:?}"),
        }
    })
    .await
    .expect("the tenth lookup must start when the second lookup releases its slot");
    assert_eq!(reader.started.load(Ordering::SeqCst), (1 << 10) - 1);
    assert_eq!(reader.active.load(Ordering::SeqCst), 8);
    // The newly admitted lookup has eleven seconds left, not a fresh fifteen.
    // Allow one millisecond for Tokio's timer rounding at the original deadline.
    tokio::time::advance(Duration::from_secs(11) + Duration::from_millis(1)).await;
    let response = tokio::time::timeout(Duration::from_millis(1), &mut request).await;
    tokio::time::resume();
    let response = response
        .expect("all entries share the original read deadline")?
        .into_inner();
    drop(request);
    assert_eq!(response.entries.len(), hosts.len());
    for (index, (entry, (id, _))) in response.entries.iter().zip(&hosts).enumerate() {
        assert_eq!(entry.component_id, id.to_string());
        assert_eq!(entry.desired, Some(stored_policy(rpc::PowerState::On)));
        let actual = entry.actual.as_ref().unwrap();
        let expected = if index < 2 {
            rpc::ComponentPowerErrorCode::CredentialsUnavailable
        } else {
            rpc::ComponentPowerErrorCode::DeadlineExceeded
        };
        assert_eq!(observation_error(actual).code, expected as i32);
        assert_eq!(
            actual.source,
            (index < 2).then_some(rpc::ComponentPowerSource::Rms as i32)
        );
        assert_eq!(actual.observed_at, None);
    }
    assert_eq!(reader.started.load(Ordering::SeqCst), (1 << 10) - 1);
    assert_eq!(reader.active.load(Ordering::SeqCst), 0);
    assert_eq!(reader.cancelled.load(Ordering::SeqCst), 8);

    // Dropping the caller's future must own the same pending work; a detached
    // task could survive caller cancellation even if its own deadline works.
    let reader = Arc::new(GatedPowerCredentialReader::new(&hosts));
    api.credential_manager = Arc::new(CompositeCredentialManager::new(
        reader.clone(),
        env.test_credential_manager.clone(),
    ));
    let mut request = Box::pin(api.get_component_power_details(host_request(&hosts)));
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = reader.wait_for_started(8) => {}
            result = &mut request => panic!("request completed before caller cancellation: {result:?}"),
        }
    }).await.expect("credential reads must start before cancellation");
    drop(request);
    assert_eq!(reader.started.load(Ordering::SeqCst), (1 << 8) - 1);
    assert_eq!(reader.active.load(Ordering::SeqCst), 0);
    assert_eq!(reader.cancelled.load(Ordering::SeqCst), 8);
    assert!(
        env.rms_sim
            .submitted_batch_get_power_state_requests()
            .await
            .is_empty()
    );
    Ok(())
}

/// Ready credential futures must not start the next operation after expiry.
/// A switch exercises both the shared BMC guard and the separate NVOS guard.
#[crate::sqlx_test]
async fn credentials_finishing_after_deadline_do_not_start_more_work(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let rack_id = RackId::new("power-details-credential-expiry");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    let switch =
        db::test_support::switch::create_seeded_discovered(&mut txn, 60, "power-credential-expiry")
            .await?;
    sqlx::query("UPDATE switches SET rack_id = $1 WHERE id = $2")
        .bind(rack_id)
        .bind(switch.id)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;
    let bmc_mac_address = switch.bmc_mac_address.expect("discovered switch BMC");
    let bmc_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
    };
    let nvos_key = CredentialKey::SwitchNvosAdmin { bmc_mac_address };
    let mut api = api_with_rms(&env, pool, power_profiles(0, 1, 0), PowerRole::Switch);

    for (blocked_key, expected_keys) in [
        // The first guard must stop before even asking for NVOS credentials.
        (bmc_key.clone(), vec![bmc_key.to_key_str().into_owned()]),
        // The second guard must stop before the completed endpoint reaches RMS.
        (
            nvos_key.clone(),
            vec![
                bmc_key.to_key_str().into_owned(),
                nvos_key.to_key_str().into_owned(),
            ],
        ),
    ] {
        let reader = Arc::new(ExpiringPowerCredentialReader {
            blocked_key,
            started: Notify::new(),
            release: Notify::new(),
            requested_keys: Mutex::new(Vec::new()),
        });
        api.credential_manager = Arc::new(CompositeCredentialManager::new(
            reader.clone(),
            env.test_credential_manager.clone(),
        ));
        let mut request = Box::pin(api.get_component_power_details(Request::new(
            rpc::GetComponentPowerDetailsRequest {
                target: Some(Target::SwitchIds(rpc::SwitchIdList {
                    ids: vec![switch.id],
                })),
            },
        )));
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                _ = reader.started.notified() => {}
                result = &mut request => panic!("request completed before gated credentials: {result:?}"),
            }
        }).await.expect("snapshot must finish before pausing the clock");

        // Leave the request unpolled until the credential is ready after expiry.
        // `timeout_at` polls that ready inner future before checking its timer.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(15) + Duration::from_millis(1)).await;
        reader.release.notify_one();
        let response = tokio::time::timeout(Duration::from_millis(1), &mut request).await;
        tokio::time::resume();
        let response = response
            .expect("ready credential must finish within the expired budget")?
            .into_inner();
        drop(request);

        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.entries[0].component_id, switch.id.to_string());
        let actual = response.entries[0]
            .actual
            .as_ref()
            .expect("deadline observation");
        assert_eq!(
            observation_error(actual).code,
            rpc::ComponentPowerErrorCode::DeadlineExceeded as i32
        );
        assert_eq!(actual.source, Some(rpc::ComponentPowerSource::Rms as i32));
        assert_eq!(actual.observed_at, None);
        assert_eq!(*reader.requested_keys.lock().unwrap(), expected_keys);
        assert!(
            env.rms_sim
                .submitted_batch_get_power_state_requests()
                .await
                .is_empty()
        );
    }

    Ok(())
}

/// A policy change committed between the machine and policy queries must not
/// replace the policy from the request's original database snapshot.
#[crate::sqlx_test]
async fn power_policy_uses_the_machine_snapshot_during_concurrent_updates(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let mut txn = pool.begin().await?;
    let host = create_host(&mut txn, 61, None, Some(PowerState::Off)).await?;
    let policy_before = db::power_options::get_by_ids(&[host.0], &mut txn)
        .await?
        .pop()
        .expect("seeded host policy");
    txn.commit().await?;
    let reader_pool = single_connection_pool(&pool).await?;
    let reader_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&reader_pool)
        .await?;
    let api = TestApiBuilder::new(
        reader_pool.clone(),
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .build();

    // A table lock blocks the plain policy SELECT; a row lock would not. The
    // machine query does not access this table, so it establishes the snapshot.
    let mut writer = pool.begin().await?;
    sqlx::query("LOCK TABLE power_options IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *writer)
        .await?;
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    let mut request = Box::pin(api.get_component_power_details(host_request(&[host])));
    let blocked_pid = tokio::select! {
        result = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_blocked_query(&pool, writer_pid, "FROM power_options"),
        ) => result.expect("policy query must block within the five-second snapshot budget"),
        result = &mut request => panic!("request completed before the policy query blocked: {result:?}"),
    };
    assert_eq!(blocked_pid, reader_pid);

    // Commit a different desired state while the request retains its snapshot.
    db::power_options::update_desired_state(
        &host.0,
        PowerState::On,
        &policy_before.desired_power_state_version,
        &mut writer,
    )
    .await?;
    writer.commit().await?;
    let response = tokio::time::timeout(Duration::from_secs(3), &mut request)
        .await
        .expect("snapshot request must finish after the writer commits")?
        .into_inner();
    drop(request);

    assert_eq!(response.entries.len(), 1);
    assert_eq!(response.entries[0].component_id, host.0.to_string());
    assert_eq!(
        response.entries[0].desired,
        Some(stored_policy(rpc::PowerState::Off))
    );
    let policy_after = db::power_options::get_by_ids(&[host.0], pool.acquire().await?.as_mut())
        .await?
        .pop()
        .expect("updated host policy");
    assert_eq!(policy_after.desired_power_state, PowerState::On);
    assert_ne!(
        policy_after.desired_power_state_version,
        policy_before.desired_power_state_version
    );
    reader_pool.close().await;

    Ok(())
}

/// Snapshot failures abort the RPC without exposing causes or doing live work.
#[crate::sqlx_test]
async fn snapshot_deadline_aborts_before_live_reads(pool: sqlx::PgPool) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let limited_pool = single_connection_pool(&pool).await?;
    let held_connection = limited_pool.acquire().await?;
    let reader = Arc::new(CountingCredentialReader::default());
    let mut api = api_with_rms(
        &env,
        limited_pool.clone(),
        power_profiles(0, 0, 0),
        PowerRole::Compute,
    );
    api.credential_manager = Arc::new(CompositeCredentialManager::new(
        reader.clone(),
        env.test_credential_manager.clone(),
    ));
    let host = HostMachineId::try_from(MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [40; 32],
        MachineType::Host,
    ))?;
    // The pool's own thirty-second timeout must not mask the API's five-second
    // snapshot budget. No database I/O remains after holding its only slot.
    tokio::time::pause();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        api.get_component_power_details(host_request(&[(
            host,
            MacAddress::new([2, 0, 0, 0, 0, 40]),
        )])),
    )
    .await;
    let finished_at = tokio::time::Instant::now();
    tokio::time::resume();
    drop(held_connection);
    limited_pool.close().await;
    assert!((deadline..=deadline + Duration::from_millis(1)).contains(&finished_at));
    let error = response
        .expect("snapshot budget must expire before watchdog")
        .expect_err("an incomplete snapshot must abort the RPC");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(
        error.message(),
        "internal error: power details database snapshot failed"
    );
    // A closed dedicated pool exercises the dependency-error branch separately
    // from elapsed time, without exposing its SQLx source on the wire.
    let error = api
        .get_component_power_details(host_request(&[(
            host,
            MacAddress::new([2, 0, 0, 0, 0, 40]),
        )]))
        .await
        .expect_err("a closed snapshot pool must abort the RPC");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(
        error.message(),
        "internal error: power details database snapshot failed"
    );
    assert_eq!(reader.0.load(Ordering::SeqCst), 0);
    assert!(
        env.rms_sim
            .submitted_batch_get_power_state_requests()
            .await
            .is_empty()
    );
    Ok(())
}

/// Disabled management preserves policy distinctions and still rejects invalid request shapes.
#[crate::sqlx_test]
async fn persisted_policy_remains_visible_when_component_manager_is_disabled(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let mut txn = pool.begin().await?;
    let (disabled_policy_host, _) =
        create_host(&mut txn, 1, None, Some(PowerState::PowerManagerDisabled)).await?;
    let (missing_policy_host, _) = create_host(&mut txn, 2, None, None).await?;
    // Missing discovery must be reported before the disabled-manager check.
    let endpointless_machine = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [4; 32],
        MachineType::Host,
    );
    db::machine::create(
        &mut txn,
        None,
        &endpointless_machine,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let endpointless_host = HostMachineId::try_from(endpointless_machine)?;
    txn.commit().await?;
    let absent_host = HostMachineId::try_from(MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [3; 32],
        MachineType::Host,
    ))?;
    let api = TestApiBuilder::new(
        pool,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .build();

    for (scenario, request) in [
        (
            "missing target",
            rpc::GetComponentPowerDetailsRequest::default(),
        ),
        (
            "duplicate machine IDs",
            rpc::GetComponentPowerDetailsRequest {
                target: Some(Target::MachineIds(::rpc::common::HostMachineIdList {
                    machine_ids: vec![disabled_policy_host, disabled_policy_host],
                })),
            },
        ),
    ] {
        assert_eq!(
            api.get_component_power_details(Request::new(request))
                .await
                .expect_err(scenario)
                .code(),
            tonic::Code::InvalidArgument,
            "{scenario}",
        );
    }
    let requested = [
        missing_policy_host,
        absent_host,
        disabled_policy_host,
        endpointless_host,
    ];
    let response = api
        .get_component_power_details(Request::new(rpc::GetComponentPowerDetailsRequest {
            target: Some(Target::MachineIds(::rpc::common::HostMachineIdList {
                machine_ids: requested.to_vec(),
            })),
        }))
        .await?
        .into_inner();
    assert_eq!(response.entries.len(), requested.len());
    for (entry, (id, error, desired)) in response.entries.iter().zip([
        (
            missing_policy_host,
            rpc::ComponentPowerErrorCode::Disabled,
            rpc::ComponentPowerPolicy::default(),
        ),
        (
            absent_host,
            rpc::ComponentPowerErrorCode::NotFound,
            rpc::ComponentPowerPolicy::default(),
        ),
        (
            disabled_policy_host,
            rpc::ComponentPowerErrorCode::Disabled,
            stored_policy(rpc::PowerState::PowerManagerDisabled),
        ),
        (
            endpointless_host,
            rpc::ComponentPowerErrorCode::EndpointUnavailable,
            stored_policy(rpc::PowerState::On),
        ),
    ]) {
        assert_eq!(entry.component_id, id.to_string());
        assert_eq!(entry.desired.as_ref(), Some(&desired));
        let actual = entry.actual.as_ref().expect("actual result");
        assert_eq!(observation_error(actual).code, error as i32);
        assert_eq!(actual.source, None);
        assert_eq!(actual.observed_at, None);
    }
    Ok(())
}

/// Core reads use stored credentials and the direct pool; MNNVL hosts keep
/// their configured backend even before rack association is complete.
#[crate::sqlx_test]
async fn core_power_details_use_stored_credentials_and_the_direct_pool(
    pool: sqlx::PgPool,
) -> TestResult {
    /// Distinguishes observations, rejected secrets, and a backend with no passive reader.
    #[derive(Clone, Copy)]
    enum ExpectedCoreRead {
        Observed,
        CredentialRejected,
        Unsupported,
    }

    /// Each row protects a routing decision, credential check, or hardware classification.
    struct CoreReadScenario {
        name: &'static str,
        seed: u8,
        rack_id: Option<RackId>,
        use_core_backend: bool,
        password: &'static str,
        hardware_info: Option<HardwareInfo>,
        vendor: Option<RedfishVendor>,
        expected: ExpectedCoreRead,
    }

    let env = create_test_env(pool.clone()).await;
    let rack_id = RackId::new("power-details-core");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    txn.commit().await?;
    let mnnvl_hardware: HardwareInfo = serde_json::from_slice(GB200_COMPUTE_TRAY_1_INFO_JSON)?;
    assert!(mnnvl_hardware.is_mnnvl_capable());

    for scenario in [
        // Standalone routing must also preserve the discovered BMC vendor.
        CoreReadScenario {
            name: "standalone Dell host with non-Core backend",
            seed: 9,
            rack_id: None,
            use_core_backend: false,
            password: "stored-power-password",
            hardware_info: Some(HardwareInfo {
                dmi_data: Some(DmiData {
                    sys_vendor: "Dell Inc.".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            vendor: Some(RedfishVendor::Dell),
            expected: ExpectedCoreRead::Observed,
        },
        // Production configures Core with the direct pool for rack hosts too.
        CoreReadScenario {
            name: "rack host with Core backend",
            seed: 10,
            rack_id: Some(rack_id.clone()),
            use_core_backend: true,
            password: "stored-power-password",
            hardware_info: None,
            vendor: None,
            expected: ExpectedCoreRead::Observed,
        },
        // Rejection proves that successful reads depend on the supplied secret.
        CoreReadScenario {
            name: "stored credential rejected by BMC",
            seed: 11,
            rack_id: Some(rack_id),
            use_core_backend: true,
            password: "stale-power-password",
            hardware_info: None,
            vendor: None,
            expected: ExpectedCoreRead::CredentialRejected,
        },
        // GPU inventory establishes rack-scale routing even without a rack ID.
        CoreReadScenario {
            name: "MNNVL host without a rack keeps unsupported configured backend",
            seed: 12,
            rack_id: None,
            use_core_backend: false,
            password: "stored-power-password",
            hardware_info: Some(mnnvl_hardware),
            vendor: None,
            expected: ExpectedCoreRead::Unsupported,
        },
    ] {
        // The stored policy differs from the live state. Keep its version so
        // a read cannot quietly rewrite the desired policy.
        let mut txn = pool.begin().await?;
        let (host_id, bmc_mac) = create_host(
            &mut txn,
            scenario.seed,
            scenario.rack_id,
            Some(PowerState::Off),
        )
        .await?;
        if let Some(hardware_info) = scenario.hardware_info {
            db::machine_topology::create_or_update(&mut txn, &host_id.into(), &hardware_info)
                .await?;
        }
        let bmc_ip = management_ip(&mut txn, bmc_mac).await?;
        let policy_before = db::power_options::get_by_ids(&[host_id], &mut txn)
            .await?
            .pop()
            .expect("seeded host policy");
        txn.commit().await?;
        let credentials = Credentials::new("stored-power-reader", scenario.password);
        env.api
            .credential_manager
            .set_credentials(
                &CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot {
                        bmc_mac_address: bmc_mac,
                    },
                },
                &credentials,
            )
            .await
            .expect("seed stored Core power-read credentials");

        // Keep the general pool distinct and failing, so any accidental use
        // cannot produce the same observation as the direct BMC connection.
        let direct_pool = Arc::new(RedfishSim::default());
        direct_pool.set_enforce_auth(true);
        direct_pool.seed_user("stored-power-reader", "stored-power-password");
        direct_pool.set_power_state(&bmc_ip.to_string(), libredfish::PowerState::On);
        let before_read = direct_pool.timepoint();
        let general_pool = Arc::new(RedfishSim::default());
        general_pool.set_create_client_error("general Redfish pool must not serve power details");
        let mut manager = ComponentManager::new(
            Arc::new(MockNvSwitchManager::default()),
            Arc::new(MockPowerShelfManager),
            Arc::new(MockComputeTrayManager),
            false,
            false,
            false,
        );
        if scenario.use_core_backend {
            manager.compute_tray = Arc::new(CoreComputeTrayManager::new(direct_pool.clone()));
        }
        let mut api = TestApiBuilder::new(
            pool.clone(),
            env.common_pools.clone(),
            env.api.work_lock_manager_handle.clone(),
        )
        .with_credential_manager(env.api.credential_manager.clone())
        .with_redfish_pool(direct_pool.clone())
        .with_component_manager(Arc::new(manager))
        .build();
        api.redfish_pool = general_pool.clone();
        let reader = Arc::new(CountingCredentialReader::default());
        if matches!(scenario.expected, ExpectedCoreRead::Unsupported) {
            api.credential_manager = Arc::new(CompositeCredentialManager::new(
                reader.clone(),
                env.test_credential_manager.clone(),
            ));
        }

        // The public RPC must load both the topology and the management endpoint.
        let response = api
            .get_component_power_details(host_request(&[(host_id, bmc_mac)]))
            .await?
            .into_inner();
        assert_eq!(response.entries.len(), 1, "{}", scenario.name);
        let entry = &response.entries[0];
        assert_eq!(entry.component_id, host_id.to_string(), "{}", scenario.name);
        assert_eq!(entry.desired, Some(stored_policy(rpc::PowerState::Off)));
        let actual = entry.actual.as_ref().expect("Core power observation");
        let expected_outcome = match scenario.expected {
            ExpectedCoreRead::Observed => Outcome::State(rpc::ComponentPowerState::On as i32),
            ExpectedCoreRead::CredentialRejected => Outcome::Error(rpc::ComponentPowerError {
                code: rpc::ComponentPowerErrorCode::ReadFailed as i32,
                message: "backend power read failed".into(),
            }),
            ExpectedCoreRead::Unsupported => Outcome::Error(rpc::ComponentPowerError {
                code: rpc::ComponentPowerErrorCode::Unsupported as i32,
                message: "backend does not support passive live power reads".into(),
            }),
        };
        assert_eq!(actual.outcome, Some(expected_outcome), "{}", scenario.name);
        assert_eq!(
            actual.observed_at.is_some(),
            matches!(scenario.expected, ExpectedCoreRead::Observed),
        );

        // Exact client and authentication records detect substituted endpoints,
        // credentials, and vendors. Unsupported reads must stop before lookup.
        if matches!(scenario.expected, ExpectedCoreRead::Unsupported) {
            assert_eq!(actual.source, None);
            assert!(direct_pool.create_client_calls().is_empty());
            assert!(direct_pool.auth_attempts().is_empty());
            assert_eq!(reader.0.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(
                actual.source,
                Some(rpc::ComponentPowerSource::Redfish as i32)
            );
            assert_eq!(
                direct_pool.create_client_calls(),
                vec![CreateClientCall {
                    host: bmc_ip.to_string(),
                    vendor: scenario.vendor,
                    auth: RedfishAuthKind::Direct,
                    auth_key: None,
                }],
                "{}",
                scenario.name
            );
            assert_eq!(
                direct_pool.auth_attempts(),
                vec![RedfishSimAuthAttempt {
                    credentials,
                    authorized: matches!(scenario.expected, ExpectedCoreRead::Observed),
                }],
                "{}",
                scenario.name
            );
        }
        assert!(general_pool.create_client_calls().is_empty());
        assert!(
            direct_pool
                .actions_since(&before_read)
                .all_hosts()
                .is_empty()
        );
        assert!(direct_pool.platform_actions().is_empty());
        let policy_after =
            db::power_options::get_by_ids(&[host_id], pool.acquire().await?.as_mut())
                .await?
                .pop()
                .expect("host policy remains present");
        assert_eq!(
            policy_after.desired_power_state,
            policy_before.desired_power_state
        );
        assert_eq!(
            policy_after.desired_power_state_version,
            policy_before.desired_power_state_version
        );
    }

    Ok(())
}

/// Actual reads are independent of policy; missing endpoints and backend failures preserve it.
#[crate::sqlx_test]
async fn rms_compute_partial_failure_preserves_persisted_policy(pool: sqlx::PgPool) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let rack_id = RackId::new("power-details-compute");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    let (observed_host, observed_mac) =
        create_host(&mut txn, 4, Some(rack_id.clone()), Some(PowerState::Off)).await?;
    let (failed_host, failed_mac) =
        create_host(&mut txn, 5, Some(rack_id.clone()), Some(PowerState::On)).await?;
    let (missing_policy_host, missing_policy_mac) =
        create_host(&mut txn, 6, Some(rack_id.clone()), None).await?;
    let (disabled_policy_host, disabled_policy_mac) = create_host(
        &mut txn,
        7,
        Some(rack_id.clone()),
        Some(PowerState::PowerManagerDisabled),
    )
    .await?;
    // This managed host has policy but no discovered BMC. Keep it outside the
    // rack so its absent endpoint does not change the four-device RMS inventory.
    let no_bmc_machine = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [8; 32],
        MachineType::Host,
    );
    db::machine::create(
        &mut txn,
        None,
        &no_bmc_machine,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let no_bmc_host = HostMachineId::try_from(no_bmc_machine)?;
    let read_hosts = [
        failed_host,
        missing_policy_host,
        observed_host,
        disabled_policy_host,
    ];
    let requested = [
        failed_host,
        missing_policy_host,
        observed_host,
        disabled_policy_host,
        no_bmc_host,
    ];
    let policies_before = db::power_options::get_by_ids(&requested, &mut txn).await?;
    txn.commit().await?;
    for mac in [
        observed_mac,
        failed_mac,
        missing_policy_mac,
        disabled_policy_mac,
    ] {
        env.api
            .credential_manager
            .set_credentials(
                &CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot {
                        bmc_mac_address: mac,
                    },
                },
                &Credentials::UsernamePassword {
                    username: "power-reader".into(),
                    password: "test-password".into(),
                },
            )
            .await
            .expect("seed compute power-read BMC credentials");
    }
    let api = api_with_rms(
        &env,
        pool.clone(),
        power_profiles(4, 0, 0),
        PowerRole::Compute,
    );
    let partial_response = rms::BatchGetPowerStateResponse {
        response: Some(rms::NodeBatchResponse {
            status: rms::ReturnCode::Failure as i32,
            node_results: vec![
                rms::NodeOperationResult {
                    node_id: observed_host.to_string(),
                    status: rms::ReturnCode::Success as i32,
                    ..Default::default()
                },
                rms::NodeOperationResult {
                    node_id: failed_host.to_string(),
                    status: rms::ReturnCode::Failure as i32,
                    error_message: "injected private backend detail".into(),
                },
                rms::NodeOperationResult {
                    node_id: missing_policy_host.to_string(),
                    status: rms::ReturnCode::Success as i32,
                    ..Default::default()
                },
                rms::NodeOperationResult {
                    node_id: disabled_policy_host.to_string(),
                    status: rms::ReturnCode::Success as i32,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        node_power_states: [
            (observed_host, "PoweringOn"),
            (missing_policy_host, "ON"),
            (disabled_policy_host, "OFF"),
        ]
        .into_iter()
        .map(|(id, state)| rms::NodePowerState {
            node_id: id.to_string(),
            pstate: state.into(),
        })
        .collect(),
    };
    // Policy absence and disabled automation must not suppress a live read.
    // Each response includes all results so matching must use node identity.
    for _ in &read_hosts {
        env.rms_sim
            .queue_batch_get_power_state_response(Ok(partial_response.clone()))
            .await;
    }
    let response = api
        .get_component_power_details(Request::new(rpc::GetComponentPowerDetailsRequest {
            target: Some(Target::MachineIds(::rpc::common::HostMachineIdList {
                machine_ids: requested.to_vec(),
            })),
        }))
        .await?
        .into_inner();
    assert_eq!(response.entries.len(), requested.len());
    let failed = &response.entries[0];
    assert_eq!(failed.component_id, failed_host.to_string());
    assert_eq!(failed.desired, Some(stored_policy(rpc::PowerState::On)));
    let actual = failed.actual.as_ref().expect("failed actual");
    assert_eq!(actual.source, Some(rpc::ComponentPowerSource::Rms as i32));
    assert_eq!(actual.observed_at, None);
    assert_eq!(
        actual.outcome,
        Some(Outcome::Error(rpc::ComponentPowerError {
            code: rpc::ComponentPowerErrorCode::ReadFailed as i32,
            message: "backend power read failed".into(),
        }))
    );
    for (entry, (id, desired, state)) in response.entries[1..4].iter().zip([
        (
            missing_policy_host,
            rpc::ComponentPowerPolicy::default(),
            rpc::ComponentPowerState::On,
        ),
        (
            observed_host,
            stored_policy(rpc::PowerState::Off),
            rpc::ComponentPowerState::PoweringOn,
        ),
        (
            disabled_policy_host,
            stored_policy(rpc::PowerState::PowerManagerDisabled),
            rpc::ComponentPowerState::Off,
        ),
    ]) {
        assert_eq!(entry.component_id, id.to_string());
        assert_eq!(entry.desired, Some(desired));
        let actual = entry.actual.as_ref().expect("observed actual");
        assert_eq!(actual.outcome, Some(Outcome::State(state as i32)));
        assert_eq!(actual.source, Some(rpc::ComponentPowerSource::Rms as i32));
        assert!(actual.observed_at.is_some());
    }
    let no_bmc = &response.entries[4];
    assert_eq!(no_bmc.component_id, no_bmc_host.to_string());
    assert_eq!(no_bmc.desired, Some(stored_policy(rpc::PowerState::On)));
    assert_eq!(
        no_bmc.actual,
        Some(rpc::ComponentPowerObservation {
            source: None,
            observed_at: None,
            outcome: Some(Outcome::Error(rpc::ComponentPowerError {
                code: rpc::ComponentPowerErrorCode::EndpointUnavailable as i32,
                message: "management endpoint is unavailable".into(),
            })),
        })
    );

    let reads = env.rms_sim.submitted_batch_get_power_state_requests().await;
    assert_eq!(
        reads.len(),
        read_hosts.len(),
        "a missing BMC must not cause an RMS read"
    );
    let mut node_ids = Vec::new();
    for read in &reads {
        let nodes = &read.nodes.as_ref().expect("RMS read descriptors").nodes;
        assert_eq!(nodes.len(), 1);
        let node = &nodes[0];
        assert_eq!(node.rack_id, rack_id.to_string());
        assert_eq!(node.r#type, Some(rms::NodeType::ComputeGb200Nvidia as i32));
        node_ids.push(node.node_id.clone());
    }
    node_ids.sort();
    let mut expected_ids: Vec<_> = read_hosts.iter().map(ToString::to_string).collect();
    expected_ids.sort();
    assert_eq!(node_ids, expected_ids);
    assert!(
        env.rms_sim
            .submitted_batch_set_power_state_requests()
            .await
            .is_empty()
    );
    let policies_after =
        db::power_options::get_by_ids(&requested, pool.acquire().await?.as_mut()).await?;
    assert_eq!(
        policies_after.len(),
        policies_before.len(),
        "a missing policy must stay absent"
    );
    for before in policies_before {
        let after = policies_after
            .iter()
            .find(|after| after.host_id == before.host_id)
            .unwrap();
        assert_eq!(after.desired_power_state, before.desired_power_state);
        assert_eq!(
            after.desired_power_state_version,
            before.desired_power_state_version
        );
    }
    Ok(())
}

/// Isolates role wiring, endpoint assembly, and prerequisite failures with healthy peers.
#[crate::sqlx_test]
async fn rms_switch_and_shelf_reads_use_their_roles_without_host_policy(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let rack_id = RackId::new("power-details-components");
    let switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let incomplete_switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let unresolved_switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let absent_switch_id = SwitchId::from(uuid::Uuid::new_v4());
    let shelf_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let unresolved_shelf_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let absent_shelf_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let shelf_mac = "02:00:00:00:00:08".parse()?;
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    let expected_switch = create_expected_switch(&mut txn, 0).await;
    let incomplete_switch = create_expected_switch(&mut txn, 1).await;
    let switch_mac = expected_switch.bmc_mac_address;
    let nvos_mac = expected_switch.nvos_mac_addresses[0];
    for (id, expected) in [
        (switch_id, Some(&expected_switch)),
        (incomplete_switch_id, Some(&incomplete_switch)),
        (unresolved_switch_id, None),
    ] {
        let mut switch = db::switch::create(
            &mut txn,
            &NewSwitch {
                id,
                config: SwitchConfig {
                    name: id.to_string(),
                    enable_nmxc: false,
                    fabric_manager_config: None,
                },
                bmc_mac_address: expected.map(|expected| expected.bmc_mac_address),
                metadata: None,
                rack_id: expected.map(|_| rack_id.clone()),
                slot_number: None,
                tray_index: None,
            },
        )
        .await?;
        if id == switch_id {
            // A deletion mark does not remove the managed identity or its read endpoint.
            db::switch::mark_as_deleted(&mut switch, &mut txn).await?;
        }
        if let Some(expected) = expected {
            let interface =
                db::machine_interface::find_by_mac_address(txn.as_mut(), expected.bmc_mac_address)
                    .await?
                    .into_iter()
                    .next()
                    .expect("expected switch BMC interface");
            db::machine_interface::associate_bmc_interface(
                &interface.id,
                MachineInterfaceAssociation::Switch(id),
                &mut txn,
            )
            .await?;
        }
    }
    // The second switch has a valid BMC but no NVOS endpoint. Its NVOS
    // credentials are also withheld for a separate check after discovery.
    db::expected_switch::update_nvos_mac_addresses(
        &mut txn,
        incomplete_switch.bmc_mac_address,
        &[],
    )
    .await?;
    let switch_ip = management_ip(&mut txn, switch_mac).await?;
    let nvos_ip = management_ip(&mut txn, nvos_mac).await?;
    sqlx::query(
        "UPDATE machine_interfaces SET hostname = $1, domain_id = NULL WHERE mac_address = $2",
    )
    .bind("power-switch-nvos.example.test")
    .bind(nvos_mac)
    .execute(&mut *txn)
    .await?;
    db::expected_power_shelf::create(
        &mut txn,
        ExpectedPowerShelf {
            bmc_mac_address: shelf_mac,
            serial_number: "power-details-shelf".into(),
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        },
    )
    .await?;
    for (id, mac, rack) in [
        (shelf_id, Some(shelf_mac), Some(rack_id.clone())),
        (unresolved_shelf_id, None, None),
    ] {
        let mut shelf = db::power_shelf::create(
            &mut txn,
            &NewPowerShelf {
                id,
                config: power_shelf_config(&id.to_string()),
                bmc_mac_address: mac,
                metadata: None,
                rack_id: rack,
            },
        )
        .await?;
        if id == shelf_id {
            // Keep the ordinary observation checks on a shelf awaiting final removal.
            db::power_shelf::mark_as_deleted(&mut shelf, &mut txn).await?;
        }
    }
    create_bmc(
        &mut txn,
        shelf_mac,
        MachineInterfaceAssociation::PowerShelf(shelf_id),
    )
    .await?;
    let shelf_ip = management_ip(&mut txn, shelf_mac).await?;
    txn.commit().await?;

    // Both prerequisites are absent: report missing BMC credentials before
    // the missing NVOS endpoint, without attempting a second credential read.
    let profiles = power_profiles(0, 2, 1);
    let reader = Arc::new(CountingCredentialReader::default());
    let mut incomplete_api = api_with_rms(&env, pool.clone(), profiles.clone(), PowerRole::Switch);
    incomplete_api.credential_manager = Arc::new(CompositeCredentialManager::new(
        reader.clone(),
        env.test_credential_manager.clone(),
    ));
    let response = incomplete_api
        .get_component_power_details(Request::new(rpc::GetComponentPowerDetailsRequest {
            target: Some(Target::SwitchIds(rpc::SwitchIdList {
                ids: vec![incomplete_switch_id],
            })),
        }))
        .await?
        .into_inner();
    assert_eq!(response.entries.len(), 1);
    assert_eq!(
        response.entries[0].component_id,
        incomplete_switch_id.to_string()
    );
    let actual = response.entries[0]
        .actual
        .as_ref()
        .expect("missing BMC credential result");
    assert_eq!(
        observation_error(actual).code,
        rpc::ComponentPowerErrorCode::CredentialsUnavailable as i32
    );
    assert_eq!(actual.source, Some(rpc::ComponentPowerSource::Rms as i32));
    assert_eq!(actual.observed_at, None);
    assert_eq!(reader.0.load(Ordering::SeqCst), 1);
    assert!(
        env.rms_sim
            .submitted_batch_get_power_state_requests()
            .await
            .is_empty()
    );

    // No fallback credentials: a missing NVOS key must remain distinguishable
    // from the BMC key, and each successful endpoint has a distinct secret.
    let credentials = Arc::new(TestCredentialManager::default());
    for (key, username, password) in [
        (
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: switch_mac,
                },
            },
            "switch-bmc",
            "switch-bmc-password",
        ),
        (
            CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: switch_mac,
            },
            "switch-nvos",
            "switch-nvos-password",
        ),
        (
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: shelf_mac,
                },
            },
            "shelf-pmc",
            "shelf-pmc-password",
        ),
        (
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: incomplete_switch.bmc_mac_address,
                },
            },
            "incomplete-bmc",
            "incomplete-bmc-password",
        ),
    ] {
        credentials
            .set_credentials(
                &key,
                &Credentials::UsernamePassword {
                    username: username.into(),
                    password: password.into(),
                },
            )
            .await
            .expect("seed role-specific power-read credentials");
    }
    let cases = [
        (
            PowerRole::Switch,
            Target::SwitchIds(rpc::SwitchIdList {
                ids: vec![
                    absent_switch_id,
                    switch_id,
                    unresolved_switch_id,
                    incomplete_switch_id,
                ],
            }),
            switch_id.to_string(),
            vec![
                (
                    absent_switch_id.to_string(),
                    Some(rpc::ComponentPowerErrorCode::NotFound),
                    None,
                ),
                (
                    switch_id.to_string(),
                    None,
                    Some(rpc::ComponentPowerSource::Rms),
                ),
                (
                    unresolved_switch_id.to_string(),
                    Some(rpc::ComponentPowerErrorCode::EndpointUnavailable),
                    None,
                ),
                (
                    incomplete_switch_id.to_string(),
                    Some(rpc::ComponentPowerErrorCode::EndpointUnavailable),
                    Some(rpc::ComponentPowerSource::Rms),
                ),
            ],
        ),
        (
            PowerRole::Shelf,
            Target::PowerShelfIds(rpc::PowerShelfIdList {
                ids: vec![absent_shelf_id, shelf_id, unresolved_shelf_id],
            }),
            shelf_id.to_string(),
            vec![
                (
                    absent_shelf_id.to_string(),
                    Some(rpc::ComponentPowerErrorCode::NotFound),
                    None,
                ),
                (
                    shelf_id.to_string(),
                    None,
                    Some(rpc::ComponentPowerSource::Rms),
                ),
                (
                    unresolved_shelf_id.to_string(),
                    Some(rpc::ComponentPowerErrorCode::EndpointUnavailable),
                    None,
                ),
            ],
        ),
    ];
    for (role, target, component_id, expected) in &cases {
        let mut api = api_with_rms(&env, pool.clone(), profiles.clone(), *role);
        api.credential_manager = credentials.clone();
        env.rms_sim
            .queue_batch_get_power_state_response(Ok(rms::BatchGetPowerStateResponse {
                response: Some(rms::NodeBatchResponse {
                    status: rms::ReturnCode::Success as i32,
                    ..Default::default()
                }),
                node_power_states: vec![rms::NodePowerState {
                    node_id: component_id.clone(),
                    pstate: "ON".into(),
                }],
            }))
            .await;
        let response = api
            .get_component_power_details(Request::new(rpc::GetComponentPowerDetailsRequest {
                target: Some(target.clone()),
            }))
            .await?
            .into_inner();
        assert_eq!(response.entries.len(), expected.len());
        for (entry, (id, error, source)) in response.entries.iter().zip(expected) {
            assert_eq!(&entry.component_id, id);
            let status = if *error == Some(rpc::ComponentPowerErrorCode::NotFound) {
                rpc::ComponentPowerPolicyStatus::Unavailable
            } else {
                rpc::ComponentPowerPolicyStatus::NotApplicable
            };
            assert_eq!(
                entry.desired,
                Some(rpc::ComponentPowerPolicy {
                    status: status as i32,
                    ..Default::default()
                })
            );
            let actual = entry.actual.as_ref().expect("actual component state");
            if let Some(code) = error {
                assert_eq!(observation_error(actual).code, *code as i32);
            } else {
                assert_eq!(
                    actual.outcome,
                    Some(Outcome::State(rpc::ComponentPowerState::On as i32))
                );
            }
            assert_eq!(actual.source, source.map(|source| source as i32));
            assert_eq!(actual.observed_at.is_some(), error.is_none());
        }
    }
    let reads = env.rms_sim.submitted_batch_get_power_state_requests().await;
    assert_eq!(
        reads.len(),
        2,
        "unresolved and absent entries must not reach RMS"
    );
    for (read, (id, mac, ip, node_type, role, username, password)) in reads.iter().zip([
        (
            switch_id.to_string(),
            switch_mac,
            switch_ip,
            rms::NodeType::SwitchGb200Nvidia,
            "switch",
            "switch-bmc",
            "switch-bmc-password",
        ),
        (
            shelf_id.to_string(),
            shelf_mac,
            shelf_ip,
            rms::NodeType::PowershelfGb200Liteon,
            "power_shelf",
            "shelf-pmc",
            "shelf-pmc-password",
        ),
    ]) {
        let [node] = read
            .nodes
            .as_ref()
            .expect("RMS node descriptors")
            .nodes
            .as_slice()
        else {
            panic!("one resolved component per read");
        };
        assert_eq!(node.node_id, id);
        assert_eq!(node.rack_id, rack_id.to_string());
        assert_eq!(node.r#type, Some(node_type as i32));
        assert_eq!(
            node.node_descriptor
                .as_ref()
                .expect("RMS role descriptor")
                .attributes
                .get("role")
                .map(String::as_str),
            Some(role),
        );
        assert_rms_endpoint(
            node.bmc_endpoint.as_ref().unwrap(),
            ip,
            mac,
            username,
            password,
        );
        if role == "switch" {
            assert_rms_endpoint(
                node.host_endpoint.as_ref().unwrap(),
                nvos_ip,
                nvos_mac,
                "switch-nvos",
                "switch-nvos-password",
            );
            assert_eq!(
                node.host_endpoint
                    .as_ref()
                    .unwrap()
                    .interface
                    .as_ref()
                    .unwrap()
                    .host_name
                    .as_deref(),
                Some("power-switch-nvos.example.test"),
            );
        } else {
            assert!(node.host_endpoint.is_none());
        }
    }

    let mut txn = pool.begin().await?;
    db::expected_switch::update_nvos_mac_addresses(
        &mut txn,
        incomplete_switch.bmc_mac_address,
        &incomplete_switch.nvos_mac_addresses,
    )
    .await?;
    txn.commit().await?;
    let mut api = api_with_rms(&env, pool.clone(), profiles, PowerRole::Switch);
    api.credential_manager = credentials;
    env.rms_sim
        .queue_batch_get_power_state_response(Ok(rms::BatchGetPowerStateResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                ..Default::default()
            }),
            node_power_states: vec![rms::NodePowerState {
                node_id: switch_id.to_string(),
                pstate: "ON".into(),
            }],
        }))
        .await;
    let response = api
        .get_component_power_details(Request::new(rpc::GetComponentPowerDetailsRequest {
            target: Some(Target::SwitchIds(rpc::SwitchIdList {
                ids: vec![incomplete_switch_id, switch_id],
            })),
        }))
        .await?
        .into_inner();
    assert_eq!(response.entries.len(), 2);
    for (entry, (id, error)) in response.entries.iter().zip([
        (
            incomplete_switch_id,
            Some(rpc::ComponentPowerErrorCode::CredentialsUnavailable),
        ),
        (switch_id, None),
    ]) {
        assert_eq!(entry.component_id, id.to_string());
        assert_eq!(
            entry.desired,
            Some(rpc::ComponentPowerPolicy {
                status: rpc::ComponentPowerPolicyStatus::NotApplicable as i32,
                ..Default::default()
            })
        );
        let actual = entry.actual.as_ref().unwrap();
        if let Some(code) = error {
            assert_eq!(observation_error(actual).code, code as i32);
        } else {
            assert_eq!(
                actual.outcome,
                Some(Outcome::State(rpc::ComponentPowerState::On as i32))
            );
        }
        assert_eq!(actual.source, Some(rpc::ComponentPowerSource::Rms as i32));
        assert_eq!(actual.observed_at.is_some(), error.is_none());
    }
    let reads = env.rms_sim.submitted_batch_get_power_state_requests().await;
    assert_eq!(
        reads.len(),
        3,
        "missing NVOS credentials must stop before RMS"
    );
    assert_eq!(
        reads[2].nodes.as_ref().unwrap().nodes[0].node_id,
        switch_id.to_string()
    );

    // The shelf uses a None capability while the switch still advertises RMS.
    // Explicitly count credentials so a builder default cannot weaken the gate.
    let reader = Arc::new(CountingCredentialReader::default());
    let unsupported_api = TestApiBuilder::new(
        pool,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_component_manager(Arc::new(api.component_manager.as_ref().unwrap().clone()))
    .with_credential_manager(Arc::new(CompositeCredentialManager::new(
        reader.clone(),
        env.test_credential_manager.clone(),
    )))
    .build();
    let response = unsupported_api
        .get_component_power_details(Request::new(rpc::GetComponentPowerDetailsRequest {
            target: Some(Target::PowerShelfIds(rpc::PowerShelfIdList {
                ids: vec![shelf_id],
            })),
        }))
        .await?
        .into_inner();
    assert_eq!(response.entries.len(), 1);
    let actual = response.entries[0].actual.as_ref().unwrap();
    assert_eq!(
        actual.outcome,
        Some(Outcome::Error(rpc::ComponentPowerError {
            code: rpc::ComponentPowerErrorCode::Unsupported as i32,
            message: "backend does not support passive live power reads".into(),
        }))
    );
    assert_eq!(actual.source, None);
    assert_eq!(actual.observed_at, None);
    assert_eq!(reader.0.load(Ordering::SeqCst), 0);
    assert!(
        env.rms_sim
            .submitted_batch_set_power_state_requests()
            .await
            .is_empty()
    );
    Ok(())
}
