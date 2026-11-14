// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use carbide_instrument::testing::{CapturedLog, capture_logs_async};
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineInterfaceId, MachineType};
use carbide_uuid::network::NetworkSegmentId;
use chrono::{DateTime, Utc};
use db::test_support::postgres::wait_for_blocked_query;
use figment::Figment;
use figment::providers::{Format, Toml};
use model::firmware::{Firmware, FirmwareComponent, FirmwareComponentType, FirmwareEntry};
use model::machine::{
    CURRENT_STATE_MODEL_VERSION, HostReprovisionRequest, InstanceState, ManagedHostState,
};
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;

use super::super::MachineUpdateManager;
use super::super::nic_firmware::NicFirmwareUpdate;
use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn updater(
    version: &str,
    test_meter: &carbide_utils::test_support::test_meter::TestMeter,
) -> Result<HostFirmwareUpdate, Box<dyn std::error::Error>> {
    let config: Arc<CarbideConfig> = Arc::new(
        Figment::new()
            .merge(Toml::file(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/cfg/test_data/full_config.toml"
            )))
            .extract()?,
    );
    let firmware = Firmware {
        vendor: bmc_vendor::BMCVendor::Dell,
        model: "fenced-catalog-host".into(),
        components: HashMap::from([(
            FirmwareComponentType::Bmc,
            FirmwareComponent {
                known_firmware: vec![FirmwareEntry {
                    version: version.into(),
                    default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let firmware_config = FirmwareConfig::new(
        Default::default(),
        &HashMap::from([("fixture".into(), firmware)]),
        &HashMap::new(),
    );
    Ok(HostFirmwareUpdate::new(
        config,
        test_meter.meter(),
        firmware_config,
        carbide_instrument::Instrumentation::new(test_meter.meter()),
    )
    .expect("host update module is enabled"))
}

async fn catalog(pool: &sqlx::PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT versions::text FROM desired_firmware ORDER BY versions::text")
        .fetch_all(pool)
        .await
}

/// Creates the committed inventory rows joined by host firmware discovery.
async fn outdated_host(
    pool: &sqlx::PgPool,
    index: u8,
    state: ManagedHostState,
) -> TestResult<HostMachineId> {
    let host_id: HostMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [index; 32],
        MachineType::Host,
    )
    .try_into()?;
    let mut txn = pool.begin().await?;
    db::machine::create(
        &mut txn,
        None,
        &host_id.into(),
        state,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    sqlx::query(
        "UPDATE machines SET firmware_autoupdate = true, update_complete = true WHERE id = $1",
    )
    .bind(host_id)
    .execute(txn.as_mut())
    .await?;
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version)
         VALUES ($1, 'V1-T0') RETURNING id",
    )
    .bind(format!("host-firmware-{index}"))
    .fetch_one(txn.as_mut())
    .await?;
    let interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
            (machine_id, segment_id, mac_address, primary_interface, hostname,
             association_type, interface_type)
         VALUES ($1, $2, $3, true, 'host-firmware', 'Machine', 'Bmc') RETURNING id",
    )
    .bind(host_id)
    .bind(segment_id)
    .bind(mac_address::MacAddress::new([2, 0, 0, 0, 0, index]))
    .fetch_one(txn.as_mut())
    .await?;
    let address = IpAddr::V4(Ipv4Addr::new(192, 0, 2, index));
    sqlx::query("INSERT INTO machine_interface_addresses (interface_id, address) VALUES ($1, $2)")
        .bind(interface_id)
        .bind(address)
        .execute(txn.as_mut())
        .await?;
    sqlx::query(
        "INSERT INTO explored_endpoints (address, exploration_report, version)
         VALUES ($1, $2, 'V1-T0')",
    )
    .bind(address)
    .bind(sqlx::types::Json(serde_json::json!({
        "Vendor": "Dell",
        "Model": "fenced-catalog-host",
        "Versions": {"bmc": "0.0"},
    })))
    .execute(txn.as_mut())
    .await?;
    txn.commit().await?;
    Ok(host_id)
}

/// Reloads the request and completion flag independently of the updater.
async fn request_and_completion(
    pool: &sqlx::PgPool,
    host_id: HostMachineId,
) -> Result<(Option<sqlx::types::Json<serde_json::Value>>, bool), sqlx::Error> {
    sqlx::query_as(
        "SELECT host_reprovisioning_requested, update_complete FROM machines WHERE id = $1",
    )
    .bind(host_id)
    .fetch_one(pool)
    .await
}

/// Counts one host's progress events within its async log capture.
fn host_progress_count(logs: &[CapturedLog], phase: &str, host_id: HostMachineId) -> usize {
    let machine_id = host_id.to_string();
    logs.iter()
        .filter(|log| {
            log.field("event_name") == Some("firmware_update_progress")
                && log.field("target") == Some("host")
                && log.field("phase") == Some(phase)
                && log.field("machine_id") == Some(machine_id.as_str())
        })
        .count()
}

/// Checks the stored request defaults and the time recorded by automatic admission.
fn assert_automated_request(
    request: Option<sqlx::types::Json<serde_json::Value>>,
    requested_after: DateTime<Utc>,
) -> TestResult {
    let request: HostReprovisionRequest =
        serde_json::from_value(request.expect("automatic request exists").0)?;
    assert_eq!(request.initiator, "Automated");
    assert!(request.requested_at >= requested_after);
    assert!(request.requested_at <= Utc::now());
    assert!(request.started_at.is_none());
    assert!(!request.user_approval_received);
    assert_eq!(request.request_reset, Some(false));
    Ok(())
}

/// Identifies the deferred foreign key that rejects COMMIT after successful writes.
fn assert_commit_failure(error: crate::CarbideError) {
    let crate::CarbideError::DBError(error) = error else {
        panic!("expected the deferred constraint failure, got {error}");
    };
    assert_eq!(
        error
            .source
            .as_database_error()
            .expect("commit failed with a database constraint error")
            .constraint(),
        Some("test_host_update_commit_failure")
    );
}

/// Concurrent requests survive, and only committed admissions emit Started.
#[crate::sqlx_test]
async fn automatic_admission_preserves_a_concurrent_request_and_fills_capacity(
    pool: sqlx::PgPool,
) -> TestResult {
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let occupied = outdated_host(&pool, 1, ManagedHostState::Ready).await?;
    let candidate = outdated_host(
        &pool,
        2,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await?;
    let mut updater = updater("2.0", &test_meter)?;
    Arc::make_mut(&mut updater.config)
        .firmware_global
        .instance_updates_manual_tagging = false;
    let mut tasks = JoinSet::new();
    let locks = db::work_lock_manager::start(
        &mut tasks,
        pool.clone(),
        Default::default(),
        carbide_instrument::Instrumentation::global(),
    )
    .await?;
    let work_lock = locks.try_acquire_lock("host-request-race".into()).await?;
    let updating = HashSet::new();
    let snapshots = HashMap::new();
    // Publish the catalog without admitting either host before the writer race.
    assert!(
        updater
            .start_updates(&pool, &work_lock, 0, &updating, &snapshots)
            .await?
            .is_empty()
    );
    for host_id in [occupied, candidate] {
        assert_eq!(request_and_completion(&pool, host_id).await?, (None, true));
    }

    // This writer holds the row while discovery still sees its committed NULL
    // request. Approval and progress must survive the automatic writer's wait.
    let mut manual = pool.begin().await?;
    db::host_machine_update::trigger_host_reprovisioning_request(
        &mut manual,
        "rack-request-race",
        &occupied,
    )
    .await?;
    let request = HostReprovisionRequest {
        requested_at: "2026-10-30T01:02:03Z".parse()?,
        initiator: "rack-request-race".into(),
        started_at: Some("2026-10-30T01:03:04Z".parse()?),
        user_approval_received: true,
        request_reset: Some(true),
    };
    let expected_request = serde_json::to_value(&request)?;
    sqlx::query(
        "UPDATE machines SET host_reprovisioning_requested = $2, update_complete = true WHERE id = $1",
    )
    .bind(occupied)
    .bind(sqlx::types::Json(request))
    .execute(manual.as_mut())
    .await?;
    let writer_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(manual.as_mut())
        .await?;
    let requested_after = Utc::now();
    let (started, logs) = capture_logs_async(async {
        let admission = updater.start_updates(&pool, &work_lock, 1, &updating, &snapshots);
        tokio::pin!(admission);
        tokio::select! {
            result = &mut admission => panic!("automatic admission did not wait for the request writer: {result:?}"),
            _ = wait_for_blocked_query(&pool, writer_pid, "UPDATE machines") => {},
        }
        manual.commit().await?;
        let started = tokio::time::timeout(Duration::from_secs(5), admission)
            .await
            .expect("admission must finish after the request writer commits")?;
        Ok::<_, Box<dyn std::error::Error>>(started)
    })
    .await;
    assert_eq!(started?, HashSet::from([candidate]));
    assert_eq!(
        request_and_completion(&pool, occupied).await?,
        (Some(sqlx::types::Json(expected_request.clone())), true)
    );
    let (request, complete) = request_and_completion(&pool, candidate).await?;
    assert_automated_request(request, requested_after)?;
    assert!(!complete);
    assert_eq!(host_progress_count(&logs, "started", candidate), 1);
    assert_eq!(host_progress_count(&logs, "started", occupied), 0);

    let mut txn = pool.begin().await?;
    db::host_machine_update::clear_host_reprovisioning_request(&mut txn, &candidate).await?;
    db::machine::update_update_complete(&candidate, true, &mut txn).await?;
    txn.commit().await?;
    // Admission can set update_complete=false, but only COMMIT checks this
    // foreign key in the test's isolated database.
    sqlx::raw_sql(
        "CREATE TABLE allowed_host_update_completion (complete boolean PRIMARY KEY);
         INSERT INTO allowed_host_update_completion VALUES (true);
         ALTER TABLE machines ADD CONSTRAINT test_host_update_commit_failure
         FOREIGN KEY (update_complete) REFERENCES allowed_host_update_completion (complete)
         DEFERRABLE INITIALLY DEFERRED;",
    )
    .execute(&pool)
    .await?;
    let (result, logs) =
        capture_logs_async(updater.start_updates(&pool, &work_lock, 1, &updating, &snapshots))
            .await;
    assert_commit_failure(result.expect_err("the applied request must fail at commit"));
    assert_eq!(
        request_and_completion(&pool, candidate).await?,
        (None, true)
    );
    assert_eq!(
        request_and_completion(&pool, occupied).await?,
        (Some(sqlx::types::Json(expected_request)), true)
    );
    assert_eq!(host_progress_count(&logs, "started", candidate), 0);
    assert_eq!(host_progress_count(&logs, "started", occupied), 0);

    sqlx::query("INSERT INTO allowed_host_update_completion VALUES (false)")
        .execute(&pool)
        .await?;
    let requested_after = Utc::now();
    let (result, logs) =
        capture_logs_async(updater.start_updates(&pool, &work_lock, 1, &updating, &snapshots))
            .await;
    assert_eq!(result?, HashSet::from([candidate]));
    let (request, complete) = request_and_completion(&pool, candidate).await?;
    assert_automated_request(request, requested_after)?;
    assert!(!complete);
    assert_eq!(host_progress_count(&logs, "started", candidate), 1);
    assert_eq!(host_progress_count(&logs, "started", occupied), 0);
    let (result, logs) =
        capture_logs_async(updater.start_updates(&pool, &work_lock, 1, &updating, &snapshots))
            .await;
    assert!(result?.is_empty());
    assert_eq!(host_progress_count(&logs, "started", candidate), 0);
    assert_eq!(host_progress_count(&logs, "started", occupied), 0);

    // Deliberate manual replacement retains the unconditional write path.
    let mut replacement = pool.begin().await?;
    db::host_machine_update::trigger_host_reprovisioning_request(
        &mut replacement,
        "ADMIN_CLI",
        &occupied,
    )
    .await?;
    replacement.commit().await?;
    let (request, complete) = request_and_completion(&pool, occupied).await?;
    let request: HostReprovisionRequest =
        serde_json::from_value(request.expect("manual replacement exists").0)?;
    assert_eq!(request.initiator, "ADMIN_CLI");
    assert!(request.started_at.is_none());
    assert!(!request.user_approval_received);
    assert_eq!(request.request_reset, Some(false));
    assert!(!complete);
    drop(work_lock);
    drop(locks);
    tasks.shutdown().await;
    Ok(())
}

/// Skips leave capacity for one later host, and excess candidates stay unadmitted.
#[crate::sqlx_test]
async fn skipped_candidates_leave_capacity_for_later_hosts(pool: sqlx::PgPool) -> TestResult {
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let skipped = outdated_host(&pool, 3, ManagedHostState::Ready).await?;
    let mut candidates = Vec::new();
    for index in [4, 8] {
        candidates.push(
            outdated_host(
                &pool,
                index,
                ManagedHostState::Assigned {
                    instance_state: InstanceState::Ready,
                },
            )
            .await?,
        );
    }
    let candidate_ids: HashSet<_> = candidates.iter().copied().collect();
    let mut tasks = JoinSet::new();
    let locks = db::work_lock_manager::start(
        &mut tasks,
        pool.clone(),
        Default::default(),
        carbide_instrument::Instrumentation::global(),
    )
    .await?;
    let work_lock = locks
        .try_acquire_lock("host-candidate-skips".into())
        .await?;
    let snapshots = HashMap::new();
    let mut updater = updater("2.0", &test_meter)?;
    Arc::make_mut(&mut updater.config)
        .firmware_global
        .instance_updates_manual_tagging = false;
    // Publish discovery's catalog before checking which hosts each row skips.
    assert!(
        updater
            .start_updates(&pool, &work_lock, 0, &HashSet::new(), &snapshots)
            .await?
            .is_empty()
    );

    // Ready sorts ahead of Assigned, so each case must pass the skipped host
    // before it can use the one available slot for the later candidate.
    for (scenario, disabled, updating) in [
        ("disabled host", vec![skipped.to_string()], HashSet::new()),
        ("other module's host", Vec::new(), HashSet::from([skipped])),
    ] {
        Arc::make_mut(&mut updater.config)
            .firmware_global
            .host_disable_autoupdate = disabled;
        let mut txn = pool.begin().await?;
        for candidate in &candidates {
            db::host_machine_update::clear_host_reprovisioning_request(&mut txn, candidate).await?;
            db::machine::update_update_complete(candidate, true, &mut txn).await?;
        }
        let discovered: HashSet<_> =
            db::host_machine_update::find_upgrade_needed(&mut txn, false, false)
                .await?
                .into_iter()
                .map(|update| update.id)
                .collect();
        assert_eq!(
            discovered,
            candidates
                .iter()
                .copied()
                .chain([skipped])
                .collect::<HashSet<_>>(),
            "{scenario}"
        );
        txn.commit().await?;

        let requested_after = Utc::now();
        let started = updater
            .start_updates(&pool, &work_lock, 1, &updating, &snapshots)
            .await?;
        assert_eq!(started.len(), 1, "{scenario}");
        assert!(started.is_subset(&candidate_ids), "{scenario}");
        assert_eq!(
            request_and_completion(&pool, skipped).await?,
            (None, true),
            "{scenario}"
        );
        let mut persisted_request_count = 0;
        for candidate in &candidates {
            let (request, complete) = request_and_completion(&pool, *candidate).await?;
            if started.contains(candidate) {
                assert_automated_request(request, requested_after)?;
                assert!(!complete, "{scenario}");
                persisted_request_count += 1;
            } else {
                assert_eq!((request, complete), (None, true), "{scenario}");
            }
        }
        assert_eq!(persisted_request_count, 1, "{scenario}");
    }
    drop(work_lock);
    drop(locks);
    tasks.shutdown().await;
    Ok(())
}

/// Aborts the first cleanup after the real host module has written its markers.
struct FailOnceCleanup {
    fail_next: AtomicBool,
}

impl fmt::Display for FailOnceCleanup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FailOnceCleanup")
    }
}

#[async_trait]
impl MachineUpdateModule for FailOnceCleanup {
    async fn get_updates_in_progress(
        &self,
        _txn: &mut PgConnection,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        Ok(HashSet::new())
    }

    async fn start_updates(
        &self,
        _pool: &sqlx::PgPool,
        _work_lock: &WorkLock,
        _available_updates: i32,
        _updating_host_machines: &HashSet<HostMachineId>,
        _snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        Ok(HashSet::new())
    }

    async fn clear_completed_updates(
        &self,
        _txn: &mut PgConnection,
    ) -> CarbideResult<Vec<FirmwareUpdateProgress>> {
        if self.fail_next.swap(false, Ordering::Relaxed) {
            return Err(crate::CarbideError::internal(
                "injected cleanup failure".to_string(),
            ));
        }
        Ok(Vec::new())
    }

    async fn update_metrics(
        &self,
        _pool: &sqlx::PgPool,
        _snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<()> {
        Ok(())
    }
}

/// Reloads completion and the presence of the host-firmware health report.
async fn completion_markers(
    pool: &sqlx::PgPool,
    host_id: HostMachineId,
) -> Result<(bool, bool), sqlx::Error> {
    sqlx::query_as(
        "SELECT update_complete,
            COALESCE(health_reports->'merges' ? 'host-fw-update', false)
         FROM machines WHERE id = $1",
    )
    .bind(host_id)
    .fetch_one(pool)
    .await
}

/// Completion events follow the manager's shared commit and stay absent on rollback.
#[crate::sqlx_test]
async fn completed_events_follow_the_manager_cleanup_commit(pool: sqlx::PgPool) -> TestResult {
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let metrics = carbide_instrument::testing::MetricsCapture::for_registry(test_meter.registry());
    let completed_count = || {
        metrics.counter_delta(
            "carbide_firmware_updates_total",
            &[("target", "host"), ("phase", "completed")],
        )
    };
    let host_id = outdated_host(&pool, 5, ManagedHostState::Ready).await?;
    let updater = updater("2.0", &test_meter)?;
    let mut config = (*updater.config).clone();
    // Keep the completion proof independent of new admission after cleanup.
    config
        .machine_updater
        .max_concurrent_machine_updates_absolute = Some(0);
    config
        .machine_updater
        .max_concurrent_machine_updates_percent = None;
    let mut txn = pool.begin().await?;
    db::machine::insert_health_report(
        &mut txn,
        &host_id,
        health_report::HealthReportApplyMode::Merge,
        &carbide_machine_controller::health_report::create_host_update_health_report_hostfw(),
        false,
    )
    .await?;
    db::machine::update_update_complete(&host_id, false, &mut txn).await?;
    txn.commit().await?;
    let mut tasks = JoinSet::new();
    let locks = db::work_lock_manager::start(
        &mut tasks,
        pool.clone(),
        Default::default(),
        carbide_instrument::Instrumentation::global(),
    )
    .await?;
    let manager = MachineUpdateManager::new_with_modules(
        pool.clone(),
        Arc::new(config),
        vec![
            Box::new(updater),
            Box::new(FailOnceCleanup {
                fail_next: AtomicBool::new(true),
            }),
        ],
        locks.clone(),
        carbide_instrument::Instrumentation::new(test_meter.meter()),
    );
    // Host cleanup succeeds before the later module fails. Both writes and
    // the completion event must wait for the manager's shared commit.
    let (result, logs) = capture_logs_async(manager.run_single_iteration()).await;
    let error = result.expect_err("the later module must abort cleanup");
    assert!(error.to_string().contains("injected cleanup failure"));
    assert_eq!(completion_markers(&pool, host_id).await?, (false, true));
    assert_eq!(host_progress_count(&logs, "completed", host_id), 0);
    assert_eq!(completed_count(), 0.0);

    // Every module can now succeed, but the deferred key rejects only COMMIT
    // after host cleanup removes the report and sets completion to true.
    sqlx::raw_sql(
        "CREATE TABLE allowed_host_update_completion (complete boolean PRIMARY KEY);
         INSERT INTO allowed_host_update_completion VALUES (false);
         ALTER TABLE machines ADD CONSTRAINT test_host_update_commit_failure
         FOREIGN KEY (update_complete) REFERENCES allowed_host_update_completion (complete)
         DEFERRABLE INITIALLY DEFERRED;",
    )
    .execute(&pool)
    .await?;
    let (result, logs) = capture_logs_async(manager.run_single_iteration()).await;
    assert_commit_failure(result.expect_err("cleanup writes must fail at commit"));
    assert_eq!(completion_markers(&pool, host_id).await?, (false, true));
    assert_eq!(host_progress_count(&logs, "completed", host_id), 0);
    assert_eq!(completed_count(), 0.0);

    sqlx::query("INSERT INTO allowed_host_update_completion VALUES (true)")
        .execute(&pool)
        .await?;
    // The retry commits both cleanup markers before emitting completion.
    let (result, logs) = capture_logs_async(manager.run_single_iteration()).await;
    result?;
    assert_eq!(completion_markers(&pool, host_id).await?, (true, false));
    assert_eq!(host_progress_count(&logs, "completed", host_id), 1);
    assert_eq!(completed_count(), 1.0);

    // Committed markers prevent the next pass from reporting completion again.
    let (result, logs) = capture_logs_async(manager.run_single_iteration()).await;
    result?;
    assert_eq!(completion_markers(&pool, host_id).await?, (true, false));
    assert_eq!(host_progress_count(&logs, "completed", host_id), 0);
    assert_eq!(completed_count(), 1.0);
    drop(manager);
    drop(locks);
    tasks.shutdown().await;
    Ok(())
}

/// A persisted NIC workflow excludes host admission while an update slot stays unused.
#[crate::sqlx_test]
async fn nic_work_excludes_host_admission_even_with_spare_capacity(
    pool: sqlx::PgPool,
) -> TestResult {
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let operation = model::test_support::nic_firmware::operation();
    let nic_state = ManagedHostState::NicFirmwareUpdating { operation };
    let occupied = outdated_host(&pool, 6, nic_state).await?;
    let candidate = outdated_host(&pool, 7, ManagedHostState::Ready).await?;
    let mut updater = updater("2.0", &test_meter)?;
    let config = Arc::make_mut(&mut updater.config);
    config.firmware_global.instance_updates_manual_tagging = false;
    config
        .machine_updater
        .max_concurrent_machine_updates_absolute = Some(3);
    config
        .machine_updater
        .max_concurrent_machine_updates_percent = None;
    let config = updater.config.clone();
    let mut tasks = JoinSet::new();
    let locks = db::work_lock_manager::start(
        &mut tasks,
        pool.clone(),
        Default::default(),
        carbide_instrument::Instrumentation::global(),
    )
    .await?;
    let manager = MachineUpdateManager::new_with_modules(
        pool.clone(),
        config,
        vec![Box::new(updater), Box::new(NicFirmwareUpdate)],
        locks.clone(),
        carbide_instrument::Instrumentation::new(test_meter.meter()),
    );

    let requested_after = Utc::now();
    manager.run_single_iteration().await?;

    assert_eq!(request_and_completion(&pool, occupied).await?, (None, true));
    let mut txn = pool.begin().await?;
    let discovered: HashSet<_> =
        db::host_machine_update::find_upgrade_needed(&mut txn, false, false)
            .await?
            .into_iter()
            .map(|update| update.id)
            .collect();
    txn.commit().await?;
    assert_eq!(discovered, HashSet::from([occupied]));
    let (request, complete) = request_and_completion(&pool, candidate).await?;
    assert_automated_request(request, requested_after)?;
    assert!(!complete);
    drop(manager);
    drop(locks);
    tasks.shutdown().await;
    Ok(())
}

/// Creates a host awaiting cleanup with its production firmware health report.
async fn host_awaiting_cleanup(pool: &sqlx::PgPool, index: u8) -> TestResult<HostMachineId> {
    let host: HostMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [index; 32],
        MachineType::Host,
    )
    .try_into()?;
    let mut txn = pool.begin().await?;
    db::machine::create(
        &mut txn,
        None,
        &host.into(),
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    db::machine::update_update_complete(&host, false, &mut txn).await?;
    db::machine::insert_health_report(
        &mut txn,
        &host,
        health_report::HealthReportApplyMode::Merge,
        &carbide_machine_controller::health_report::create_host_update_health_report_hostfw(),
        false,
    )
    .await?;
    txn.commit().await?;
    Ok(host)
}

/// Reloads the full request and health reports from the pool after transactions finish.
async fn completion_state(
    pool: &sqlx::PgPool,
    host: HostMachineId,
) -> Result<(Option<serde_json::Value>, bool, serde_json::Value), sqlx::Error> {
    sqlx::query_as(
        "SELECT host_reprovisioning_requested, update_complete, health_reports
         FROM machines WHERE id = $1",
    )
    .bind(host)
    .fetch_one(pool)
    .await
}

/// Checks that cleanup returned exactly one Host/Completed event for this host.
fn assert_host_completed(events: &[FirmwareUpdateProgress], host: HostMachineId) {
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].machine_id, events[0].phase, events[0].target),
        (
            host,
            FirmwareUpdatePhase::Completed,
            FirmwareUpdateTarget::Host,
        ),
    );
}

/// A request committed during selection keeps its exact contents,
/// `update_complete=false`, and firmware health report, while another eligible
/// host completes in the same pass.
#[crate::sqlx_test]
async fn completion_rechecks_host_request_after_waiting_for_writer(
    pool: sqlx::PgPool,
) -> TestResult {
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let host = host_awaiting_cleanup(&pool, 23).await?;
    let unaffected = host_awaiting_cleanup(&pool, 24).await?;
    let (_, _, health_before) = completion_state(&pool, host).await?;
    assert!(
        health_before["merges"]
            .get(HOST_FW_UPDATE_HEALTH_REPORT_SOURCE)
            .is_some()
    );
    let updater = updater("1.0", &test_meter)?;

    // Keep the real request writer uncommitted so selection sees the old
    // completed candidate and has to wait before deciding it is eligible.
    let mut writer = pool.begin().await?;
    db::host_machine_update::trigger_host_reprovisioning_request(&mut writer, "Manual", &host)
        .await?;
    let request: serde_json::Value =
        sqlx::query_scalar("SELECT host_reprovisioning_requested FROM machines WHERE id = $1")
            .bind(host)
            .fetch_one(writer.as_mut())
            .await?;
    let writer_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(writer.as_mut())
        .await?;

    let events = tokio::time::timeout(Duration::from_secs(60), async {
        let cleanup = async {
            let mut txn = pool.begin().await?;
            let events = updater.clear_completed_updates(txn.as_mut()).await?;
            txn.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(events)
        };
        tokio::pin!(cleanup);
        tokio::select! {
            result = &mut cleanup => match result {
                Ok(events) => panic!("completion selection returned {} events before the request writer committed", events.len()),
                Err(error) => panic!("completion selection failed before the request writer committed: {error}"),
            },
            _ = wait_for_blocked_query(&pool, writer_pid, "SELECT id FROM machines") => {},
        }
        writer.commit().await?;
        cleanup.await
    })
    .await
    .expect("cleanup must finish after the request writer commits")?;

    let (stored_request, update_complete, health_after) = completion_state(&pool, host).await?;
    assert_eq!(stored_request, Some(request));
    assert!(!update_complete);
    assert_eq!(health_after, health_before);
    assert_host_completed(&events, unaffected);
    assert_eq!(completion_markers(&pool, unaffected).await?, (true, false));
    Ok(())
}

/// A selected host stays locked through cleanup commit, then its next request
/// commits with `update_complete=false`.
#[crate::sqlx_test]
async fn completion_holds_host_lock_until_cleanup_commits(pool: sqlx::PgPool) -> TestResult {
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let host = host_awaiting_cleanup(&pool, 23).await?;
    let updater = updater("1.0", &test_meter)?;
    let mut cleanup = pool.begin().await?;
    // Select before any cleanup writes so the request writer must wait for
    // the selection lock itself, rather than a later UPDATE's row lock.
    assert_eq!(
        db::host_machine_update::find_completed_updates(cleanup.as_mut()).await?,
        vec![host],
    );
    let cleanup_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(cleanup.as_mut())
        .await?;

    let request = tokio::time::timeout(Duration::from_secs(60), async {
        let writer = async {
            let mut txn = pool.begin().await?;
            db::host_machine_update::trigger_host_reprovisioning_request(
                &mut txn, "rack-test", &host,
            )
            .await?;
            let request: serde_json::Value = sqlx::query_scalar(
                "SELECT host_reprovisioning_requested FROM machines WHERE id = $1",
            )
            .bind(host)
            .fetch_one(txn.as_mut())
            .await?;
            txn.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(request)
        };
        tokio::pin!(writer);
        tokio::select! {
            result = &mut writer => panic!("request writer escaped the completion lock: {result:?}"),
            _ = wait_for_blocked_query(&pool, cleanup_pid, "UPDATE machines SET host_reprovisioning_requested") => {},
        }

        // Run the production cleanup while the next request waits, then let
        // that writer replace the completion flag only after cleanup commits.
        let events = updater.clear_completed_updates(cleanup.as_mut()).await?;
        assert_host_completed(&events, host);
        cleanup.commit().await?;
        writer.await
    })
    .await
    .expect("request writer must finish after cleanup commits")?;

    let (stored_request, update_complete, health_after) = completion_state(&pool, host).await?;
    assert_eq!(stored_request, Some(request));
    assert!(!update_complete);
    assert!(
        !health_after["merges"]
            .as_object()
            .expect("cleanup leaves the health report merges object")
            .contains_key(HOST_FW_UPDATE_HEALTH_REPORT_SOURCE)
    );
    Ok(())
}

/// Completion can commit while an uncommitted console row references its machine.
#[crate::sqlx_test]
async fn completion_allows_an_open_machine_foreign_key_check(pool: sqlx::PgPool) -> TestResult {
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let host = host_awaiting_cleanup(&pool, 25).await?;
    let updater = updater("1.0", &test_meter)?;
    let mut reference = pool.begin().await?;
    sqlx::query(
        "INSERT INTO machine_console_metadata (machine_id, username, role, password)
         VALUES ($1, 'cleanup-test', 'user', 'test-password')",
    )
    .bind(host)
    .execute(reference.as_mut())
    .await?;

    // The foreign key check holds a key-share lock until this insert commits.
    // Completion only changes non-key fields and must finish while it stays open.
    let events = tokio::time::timeout(Duration::from_secs(60), async {
        let mut cleanup = pool.begin().await?;
        let events = updater.clear_completed_updates(cleanup.as_mut()).await?;
        cleanup.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(events)
    })
    .await
    .expect("cleanup must finish while the referencing insert remains uncommitted")?;
    assert_host_completed(&events, host);
    reference.commit().await?;

    assert_eq!(completion_markers(&pool, host).await?, (true, false));
    let referenced_host: HostMachineId = sqlx::query_scalar(
        "SELECT machine_id FROM machine_console_metadata WHERE username = 'cleanup-test'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(referenced_host, host);
    Ok(())
}

// A fresh updater has a changed nonempty catalog to publish. Losing ownership
// must leave the previously committed catalog intact.
#[crate::sqlx_test]
async fn stale_work_lock_cannot_publish_host_firmware(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let mut tasks = JoinSet::new();
    let locks = db::work_lock_manager::start(
        &mut tasks,
        pool.clone(),
        Default::default(),
        carbide_instrument::Instrumentation::global(),
    )
    .await?;
    let work_key = "host-firmware-admission";
    let work_lock = locks.try_acquire_lock(work_key.into()).await?;
    let updating = HashSet::new();
    let snapshots = HashMap::new();
    updater("1.0", &test_meter)?
        .start_updates(&pool, &work_lock, 1, &updating, &snapshots)
        .await?;
    let before = catalog(&pool).await?;
    assert_eq!(before.len(), 1);
    assert!(before[0].contains("1.0"));

    let changed =
        sqlx::query("UPDATE work_locks SET worker_id = gen_random_uuid() WHERE work_key = $1")
            .bind(work_key)
            .execute(&pool)
            .await?;
    assert_eq!(changed.rows_affected(), 1);
    let fresh_updater = updater("2.0", &test_meter)?;
    let error = fresh_updater
        .start_updates(&pool, &work_lock, 1, &updating, &snapshots)
        .await
        .expect_err("the previous worker must not replace the catalog");
    assert!(matches!(error, crate::CarbideError::FailedPrecondition(_)));
    assert_eq!(catalog(&pool).await?, before);
    assert!(
        fresh_updater
            .firmware_catalog_last_read
            .lock()
            .await
            .is_none()
    );
    drop(work_lock);
    drop(locks);
    tasks.shutdown().await;
    Ok(())
}

// A failed pass must roll back its catalog and retry the same marker. On retry,
// takeover waits until the transaction that publishes the catalog commits.
#[crate::sqlx_test]
async fn catalog_retry_keeps_ownership_through_publication_commit(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let writer_pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout = '1500ms'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let updater = updater("2.0", &test_meter)?;
    let mut tasks = JoinSet::new();
    let locks = db::work_lock_manager::start(
        &mut tasks,
        pool.clone(),
        Default::default(),
        carbide_instrument::Instrumentation::global(),
    )
    .await?;
    let work_key = "host-catalog-publication";
    let work_lock = locks.try_acquire_lock(work_key.into()).await?;
    let updating = HashSet::new();
    let snapshots = HashMap::new();

    // Discovery runs after catalog publication. Blocking this relation lets
    // the pass write the catalog before a later statement forces rollback.
    let mut blocker = pool.begin().await?;
    sqlx::query("LOCK TABLE explored_endpoints IN ACCESS EXCLUSIVE MODE")
        .execute(blocker.as_mut())
        .await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await?;
    {
        let admission = updater.start_updates(&writer_pool, &work_lock, 1, &updating, &snapshots);
        tokio::pin!(admission);
        tokio::select! {
            result = &mut admission => panic!("catalog pass did not wait for discovery: {result:?}"),
            _ = wait_for_blocked_query(&pool, blocker_pid, "FROM explored_endpoints") => {},
        }
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), admission)
            .await
            .expect("the shorter operator statement limit must be preserved")
            .expect_err("blocked discovery must abort the catalog transaction");
        assert!(matches!(error, crate::CarbideError::DBError(_)));
    }
    assert!(catalog(&pool).await?.is_empty());
    assert!(updater.firmware_catalog_last_read.lock().await.is_none());
    let limit: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&writer_pool)
        .await?;
    assert_eq!(limit, "1500ms");

    let (admitted, taken_over) = {
        let admission = updater.start_updates(&writer_pool, &work_lock, 1, &updating, &snapshots);
        tokio::pin!(admission);
        let admission_pid = tokio::select! {
            result = &mut admission => panic!("retry did not reach blocked discovery: {result:?}"),
            pid = wait_for_blocked_query(&pool, blocker_pid, "FROM explored_endpoints") => pid,
        };
        let takeover =
            sqlx::query("UPDATE work_locks SET worker_id = gen_random_uuid() WHERE work_key = $1")
                .bind(work_key)
                .execute(&pool);
        tokio::pin!(takeover);
        tokio::select! {
            result = &mut admission => panic!("retry escaped the discovery lock: {result:?}"),
            result = &mut takeover => panic!("ownership changed before catalog commit: {result:?}"),
            _ = wait_for_blocked_query(&pool, admission_pid, "UPDATE work_locks SET worker_id") => {},
        }
        blocker.commit().await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(admission, takeover)
        })
        .await
        .expect("publication and takeover must finish after releasing discovery")
    };
    assert!(admitted?.is_empty());
    assert_eq!(taken_over?.rows_affected(), 1);
    let committed = catalog(&pool).await?;
    assert_eq!(committed.len(), 1);
    assert!(committed[0].contains("2.0"));
    assert!(updater.firmware_catalog_last_read.lock().await.is_some());
    drop(work_lock);
    drop(locks);
    writer_pool.close().await;
    tasks.shutdown().await;
    Ok(())
}
