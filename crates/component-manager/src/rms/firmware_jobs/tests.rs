// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::task::Poll;

use api_test_helper::mock_rms::MockRmsApi;
use carbide_instrument::testing::capture_logs_async;
use sqlx::PgPool;

use super::*;
use crate::compute_tray_manager::ComputeTrayManager;
use crate::firmware_jobs::FirmwareJobSource;
use crate::mock::{MockComputeTrayManager, MockNvSwitchManager, MockPowerShelfManager};
use crate::nv_switch_manager::NvSwitchManager;
use crate::power_shelf_manager::PowerShelfManager;

const PRIVATE: &str = "private-credentials-manifest-options-body";

/// Supplies only the RMS client and chosen pool; individual tests own tracking setup.
fn backend(pool: PgPool) -> (Arc<MockRmsApi>, RmsBackend) {
    let mock = Arc::new(MockRmsApi::new());
    let backend = RmsBackend::new(mock.clone(), Some(mock.clone()), pool, Arc::default(), true);
    (mock, backend)
}

/// Keeps tests of in-memory tracking independent of database availability.
fn lazy_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://localhost/unused")
        .expect("create lazy pool")
}

/// Includes private free-form fields that a passive observation must discard.
fn firmware_response(id: &str, state: rms::FirmwareJobState) -> rms::GetFirmwareJobStatusResponse {
    rms::GetFirmwareJobStatusResponse {
        job_id: id.into(),
        error_message: PRIVATE.into(),
        state_description: PRIVATE.into(),
        result_json: PRIVATE.into(),
        ..MockRmsApi::firmware_job_status_ok(state)
    }
}

/// Exercises the separate NVOS protocol with the same handling of private diagnostic text.
fn image_response(id: &str, state: &str) -> rms::GetSwitchSystemImageJobStatusResponse {
    rms::GetSwitchSystemImageJobStatusResponse {
        job_id: id.into(),
        message: PRIVATE.into(),
        error_message: PRIVATE.into(),
        result_json: PRIVATE.into(),
        ..MockRmsApi::switch_system_image_job_status_ok(state)
    }
}

/// Keeps attribution and unavailable evidence distinct at each protocol boundary;
/// existing mapper tests retain the documented mappings, not an exhaustive wire matrix.
#[tokio::test]
async fn protocol_evidence_is_attributed_and_diagnostics_are_private() {
    use FirmwareJobReadError::{JobNotFound, PollFailed, StatusUnavailable};
    use ObservedJobKind::{FirmwareObject as Firmware, SwitchSystemImage as Image};
    #[derive(Debug, Clone, Copy)]
    enum Evidence {
        Failed,
        Completed,
        Missing,
        MissingWrongEcho,
        MissingEmptyEcho,
        NoEcho,
        WrongEcho,
        UnknownState,
        Unspecified,
        RpcFailure,
        TlsFailure,
        RpcNotFound,
        RpcUnimplemented,
        Expired,
        Unconfigured,
    }
    use Evidence::*;
    let cases = [
        (Firmware, Failed, Ok(FirmwareState::Failed)),
        (Image, Completed, Ok(FirmwareState::Completed)),
        (Firmware, Missing, Err(JobNotFound)),
        (Image, Missing, Err(JobNotFound)),
        (Firmware, MissingWrongEcho, Err(StatusUnavailable)),
        (Firmware, MissingEmptyEcho, Err(JobNotFound)),
        (Firmware, NoEcho, Err(StatusUnavailable)),
        (Image, WrongEcho, Err(StatusUnavailable)),
        (Firmware, UnknownState, Err(StatusUnavailable)),
        (Image, UnknownState, Err(StatusUnavailable)),
        (Firmware, Unspecified, Err(StatusUnavailable)),
        (Firmware, RpcFailure, Err(PollFailed)),
        (Image, TlsFailure, Err(PollFailed)),
        (Firmware, RpcNotFound, Err(PollFailed)),
        (Firmware, RpcUnimplemented, Err(StatusUnavailable)),
        (
            Firmware,
            Expired,
            Err(FirmwareJobReadError::DeadlineExceeded),
        ),
        (Image, Unconfigured, Err(StatusUnavailable)),
        (Firmware, Unconfigured, Ok(FirmwareState::Completed)),
    ];
    for (kind, evidence, expected) in cases {
        let mock = MockRmsApi::new();
        let mut firmware = firmware_response("job", rms::FirmwareJobState::Completed);
        let mut image = image_response("job", "completed");
        // Every response includes private diagnostics; mutate only the evidence
        // under test so each row proves its own attribution or failure boundary.
        let transport_error = match evidence {
            Failed => {
                firmware.job_state = rms::FirmwareJobState::Failed as i32;
                None
            }
            Missing | MissingWrongEcho | MissingEmptyEcho => {
                firmware.status = rms::ReturnCode::Failure as i32;
                image.status = rms::ReturnCode::Failure as i32;
                if matches!(evidence, MissingWrongEcho) {
                    firmware.job_id = "wrong-job".into();
                }
                if matches!(evidence, MissingEmptyEcho) {
                    firmware.job_id.clear();
                }
                None
            }
            NoEcho => {
                firmware.job_id.clear();
                None
            }
            WrongEcho => {
                image.job_id = "wrong-job".into();
                None
            }
            UnknownState => {
                firmware.job_state = i32::MAX;
                image.state = PRIVATE.into();
                None
            }
            Unspecified => {
                firmware.status = rms::ReturnCode::Unspecified as i32;
                None
            }
            // PermissionDenied differs from an empty mock queue (Internal), so
            // the safe code assertion also proves this injected failure was read.
            RpcFailure => Some(tonic::Status::permission_denied(PRIVATE).into()),
            TlsFailure => Some(RackManagerError::TlsError(
                librms::RmsTlsClientError::Connection(PRIVATE.into()),
            )),
            RpcNotFound => Some(tonic::Status::not_found(PRIVATE).into()),
            RpcUnimplemented => Some(tonic::Status::unimplemented(PRIVATE).into()),
            Completed | Unconfigured | Expired => None,
        };
        let rpc_failed = transport_error.is_some();
        let job = match kind {
            Firmware => {
                mock.enqueue_get_firmware_job_status(transport_error.map_or(Ok(firmware), Err))
                    .await;
                RmsTrackedFirmwareJob::FirmwareObject("job".into())
            }
            Image => {
                mock.enqueue_get_switch_system_image_job_status(
                    transport_error.map_or(Ok(image), Err),
                )
                .await;
                RmsTrackedFirmwareJob::SwitchSystemImage("job".into())
            }
        };
        let configured = !matches!(evidence, Unconfigured);

        let (result, logs) = capture_logs_async(observe_job(
            &mock,
            configured.then_some(&mock as &dyn RmsSwitchSystemImageStatusApi),
            &job,
            Instant::now()
                + if matches!(evidence, Expired) {
                    Duration::ZERO
                } else {
                    READ_TIMEOUT
                },
        ))
        .await;

        assert_eq!(result, expected, "{kind:?}/{evidence:?}");
        let firmware_calls = mock.get_firmware_job_status_calls().await;
        let image_calls = mock.get_switch_system_image_job_status_calls().await;
        assert!(
            if kind == Firmware {
                image_calls.is_empty()
            } else {
                firmware_calls.is_empty()
            },
            "{kind:?}/{evidence:?}: only the selected status protocol is called"
        );
        let ids: Vec<_> = firmware_calls
            .iter()
            .map(|call| call.job_id.as_str())
            .chain(image_calls.iter().map(|call| call.job_id.as_str()))
            .collect();
        assert_eq!(
            ids,
            if (kind == Firmware || configured) && !matches!(evidence, Expired) {
                vec!["job"]
            } else {
                vec![]
            }
        );
        if matches!(evidence, RpcFailure | RpcNotFound | RpcUnimplemented) {
            let diagnostic = logs
                .iter()
                .find(|log| log.message == "firmware job RPC failed")
                .expect("safe RPC context");
            let code = match evidence {
                RpcFailure => "PermissionDenied",
                RpcNotFound => "NotFound",
                _ => "Unimplemented",
            };
            assert_eq!(diagnostic.field("grpc_status_code"), Some(code));
            assert_eq!(diagnostic.field("job_id"), Some("job"));
            assert_eq!(diagnostic.field("job_kind"), Some("firmware_object"));
        }
        if matches!(evidence, TlsFailure) {
            let diagnostic = logs
                .iter()
                .find(|log| log.message == "firmware job connection failed")
                .expect("safe TLS context");
            assert_eq!(diagnostic.field("failure_class"), Some("tls"));
            assert_eq!(diagnostic.field("job_kind"), Some("switch_system_image"));
            assert_eq!(diagnostic.field("job_id"), Some("job"));
        }
        if matches!(evidence, Expired) {
            assert!(logs.is_empty(), "no RPC or RED admission after expiry");
        }

        assert!(!format!("{result:?}").contains(PRIVATE));
        assert!(logs.iter().all(|log| {
            !log.message.contains(PRIVATE)
                && log
                    .fields
                    .iter()
                    .all(|(name, value)| !name.contains(PRIVATE) && !value.contains(PRIVATE))
        }));
        assert_eq!(
            logs.iter()
                .filter(|log| log.message == "external call failed")
                .count(),
            usize::from(rpc_failed),
            "one RED diagnostic: {kind:?}/{evidence:?}"
        );
    }
}

/// Poll `advance` once without awaiting its yield: Tokio advances the paused
/// clock synchronously first. Returning ready in that same poll exposes timeout's
/// inner-first behavior deterministically for both success and error evidence.
#[tokio::test(start_paused = true)]
async fn ready_results_after_expiry_are_deadline_errors() {
    for ready in [Ok(()), Err(FirmwareJobReadError::PollFailed)] {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut advance = Box::pin(tokio::time::advance(Duration::from_secs(1)));
        let read = std::future::poll_fn(|cx| {
            assert!(
                advance.as_mut().poll(cx).is_pending(),
                "advance yields after changing the clock"
            );
            assert_eq!(Instant::now(), deadline);
            Poll::Ready(ready)
        });

        let result = before_deadline(deadline, read).await;

        assert_eq!(result, Err(FirmwareJobReadError::DeadlineExceeded));
    }
    let expired = before_deadline::<()>(Instant::now(), async {
        panic!("an expired attempt must not be polled")
    })
    .await;

    assert_eq!(expired, Err(FirmwareJobReadError::DeadlineExceeded));
}

/// One blocked poll must not erase completed jobs for the same device or other devices;
/// dropping the request must also release the poll without rewriting newer tracking.
#[tokio::test(start_paused = true)]
async fn unfinished_polls_are_owned_and_healthy_peers_survive_the_deadline() {
    // The watchdog is independent of the shorter reader deadline, so removing
    // the production timeout fails the proof instead of hanging the test.
    let lifecycle = async {
        assert_eq!(READ_TIMEOUT, Duration::from_secs(10));
        assert_eq!(ENDPOINT_CONCURRENCY * JOB_CONCURRENCY, 16);
        for expire in [true, false] {
            let (mock, backend) = backend(lazy_pool());
            let mac = MacAddress::new([2, 0, 0, 0, 0, 1]);
            let mut macs = vec![mac];
            // More than eight peers proves a completed workflow admits later
            // endpoints while the first poll still occupies its slot.
            for index in 1..=9 {
                let peer = MacAddress::new([2, 0, 0, 0, 0, index + 1]);
                let id = format!("peer-{index}");
                macs.push(peer);
                backend.firmware_jobs.lock().unwrap().insert(
                    peer,
                    vec![RmsTrackedFirmwareJob::FirmwareObject(id.clone())],
                );
                mock.enqueue_get_firmware_job_status(Ok(firmware_response(
                    &id,
                    rms::FirmwareJobState::Completed,
                )))
                .await;
            }
            // Seed only tracking: no identity lookup or submission belongs in a reader.
            backend.firmware_jobs.lock().unwrap().insert(
                mac,
                vec![
                    RmsTrackedFirmwareJob::SwitchSystemImage("image".into()),
                    RmsTrackedFirmwareJob::FirmwareObject("slow".into()),
                ],
            );
            backend.staged_firmware_objects.lock().unwrap().insert(
                mac,
                StagedFirmwareObject::Submitting {
                    system_image_job: "image".into(),
                },
            );
            mock.enqueue_get_switch_system_image_job_status(Ok(image_response("image", "running")))
                .await;
            let (entered, release) = mock.block_next_get_firmware_job_status().await;
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut read = Box::pin(backend.observe_firmware_jobs(&macs, true, deadline));

            tokio::select! {
                result = &mut read => {
                    panic!("completed before blocked poll: {result:?}");
                }
                entered = entered => {
                    entered.expect("poll arrived");
                }
            }

            // A concurrent updater can replace both maps during the RPC. The
            // reader must release its locks and never restore its older snapshot.
            backend
                .firmware_jobs
                .try_lock()
                .expect("tracking lock released")
                .clear();
            backend
                .staged_firmware_objects
                .try_lock()
                .expect("staged lock released")
                .insert(mac, StagedFirmwareObject::Failed(PRIVATE.into()));

            if expire {
                tokio::time::advance(Duration::from_secs(1)).await;
                let results = read.await.expect("per-endpoint observations");
                assert_eq!(Instant::now(), deadline);

                let jobs = results[0].jobs.as_ref().expect("known jobs");
                assert_eq!(
                    jobs.iter()
                        .map(|job| job.job_id.as_str())
                        .collect::<Vec<_>>(),
                    ["slow", "image"]
                );
                assert_eq!(jobs[0].state, Err(FirmwareJobReadError::DeadlineExceeded));
                assert_eq!(jobs[1].state, Ok(FirmwareState::InProgress));
                assert_eq!(
                    results
                        .iter()
                        .map(|result| result.mac_address)
                        .collect::<Vec<_>>(),
                    macs
                );
                assert!(
                    results[1..]
                        .iter()
                        .all(|result| result.jobs.as_ref().expect("peer jobs")[0].state
                            == Ok(FirmwareState::Completed))
                );
                assert_eq!(
                    results[0].staged,
                    Some(StagedFirmwareJob::Submitting {
                        system_image_job_id: "image".into()
                    })
                );
                assert_eq!(
                    mock.get_firmware_job_status_calls()
                        .await
                        .iter()
                        .map(|call| call.job_id.clone())
                        .collect::<Vec<_>>(),
                    std::iter::once("slow".to_owned())
                        .chain((1..=9).map(|index| format!("peer-{index}")))
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    mock.get_switch_system_image_job_status_calls()
                        .await
                        .iter()
                        .map(|call| call.job_id.clone())
                        .collect::<Vec<_>>(),
                    ["image"]
                );
            } else {
                drop(read);
            }

            assert!(release.is_closed(), "unfinished RPC is dropped");
            assert_eq!(
                mock.get_firmware_job_status_calls()
                    .await
                    .first()
                    .unwrap()
                    .job_id,
                "slow"
            );
            assert!(backend.firmware_jobs.lock().unwrap().is_empty());
            assert!(
                matches!(backend.staged_firmware_objects.lock().unwrap().get(&mac), Some(StagedFirmwareObject::Failed(reason)) if reason == PRIVATE)
            );
            assert!(mock.apply_firmware_object_calls().await.is_empty());
        }
    };
    tokio::time::timeout(Duration::from_secs(60), lifecycle)
        .await
        .expect("owned polls finish within watchdog");
}

/// Saturating all eight endpoint slots must queue later work without resetting
/// its deadline. One completion admits the ninth endpoint; the tenth expires
/// without an RPC while already-observed NVOS peers remain useful.
#[tokio::test(start_paused = true)]
async fn endpoint_admission_keeps_the_original_deadline() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let (mock, backend) = backend(lazy_pool());
        let macs: Vec<_> = (0..10)
            .map(|index| MacAddress::new([2, 0, 0, 0, 1, index]))
            .collect();
        for (index, mac) in macs.iter().enumerate() {
            backend.firmware_jobs.lock().unwrap().insert(
                *mac,
                vec![
                    RmsTrackedFirmwareJob::FirmwareObject(format!("fw-{index}")),
                    RmsTrackedFirmwareJob::SwitchSystemImage(format!("image-{index}")),
                ],
            );
            mock.enqueue_get_switch_system_image_job_status(Ok(image_response(
                &format!("image-{index}"),
                "completed",
            )))
            .await;
        }
        let mut gates = mock
            .block_next_firmware_job_status_requests(9)
            .await
            .into_iter();
        let mut releases = Vec::new();
        let started = Instant::now();
        let deadline = started + READ_TIMEOUT;
        let mut read = Box::pin(backend.observe_firmware_jobs(&macs, true, deadline));

        for _ in 0..8 {
            let (entered, release) = gates.next().expect("initial gate");
            releases.push(release);
            tokio::select! {
                result = &mut read => panic!("completed before saturated polls: {result:?}"),
                entered = entered => entered.expect("initial poll arrived"),
            }
        }
        assert_eq!(mock.get_firmware_job_status_calls().await.len(), 8);
        assert_eq!(
            mock.get_switch_system_image_job_status_calls().await.len(),
            8
        );
        tokio::time::advance(Duration::from_secs(5)).await;
        mock.enqueue_get_firmware_job_status(Ok(firmware_response(
            "fw-0",
            rms::FirmwareJobState::Completed,
        )))
        .await;
        releases.remove(0).send(()).expect("complete one workflow");
        let (entered, release) = gates.next().expect("ninth gate");
        releases.push(release);
        tokio::select! {
            result = &mut read => panic!("completed before ninth admission: {result:?}"),
            entered = entered => entered.expect("ninth poll arrived"),
        }
        assert_eq!(Instant::now(), started + Duration::from_secs(5));
        tokio::time::advance(Duration::from_secs(5)).await;

        let results = read.await.expect("observations at original deadline");

        assert_eq!(Instant::now(), deadline);
        assert_eq!(
            results
                .iter()
                .map(|result| result.mac_address)
                .collect::<Vec<_>>(),
            macs
        );
        for (index, result) in results.iter().enumerate() {
            let jobs = result.jobs.as_ref().expect("memory tracking");
            assert_eq!(
                jobs[0].state,
                if index == 0 {
                    Ok(FirmwareState::Completed)
                } else {
                    Err(FirmwareJobReadError::DeadlineExceeded)
                }
            );
            assert_eq!(
                jobs[1].state,
                if index < 9 {
                    Ok(FirmwareState::Completed)
                } else {
                    Err(FirmwareJobReadError::DeadlineExceeded)
                }
            );
        }
        assert!(releases.iter().all(tokio::sync::oneshot::Sender::is_closed));
        assert_eq!(
            mock.get_firmware_job_status_calls()
                .await
                .iter()
                .map(|call| call.job_id.clone())
                .collect::<Vec<_>>(),
            (0..9)
                .map(|index| format!("fw-{index}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            mock.get_switch_system_image_job_status_calls()
                .await
                .iter()
                .map(|call| call.job_id.clone())
                .collect::<Vec<_>>(),
            (0..9)
                .map(|index| format!("image-{index}"))
                .collect::<Vec<_>>()
        );
    })
    .await
    .expect("admission scenario finishes within watchdog");
}

/// Fallback owns its session through commit, releases it before RMS, and
/// discards it on cancellation. One real row supports all three lifecycle phases.
#[carbide_macros::sqlx_test]
async fn tracking_sessions_bound_acquisition_drop_blocked_reads_and_release_before_rms(
    pool: PgPool,
) {
    let lifecycle = async {
        use db::test_support::postgres::wait_for_blocked_query;
        let macs = [MacAddress::new([2, 0, 0, 0, 2, 1])];
        db::direct_dispatch_rms_job::save(&pool, macs[0], StoredJobKind::FirmwareObject, "job")
            .await
            .expect("persist job");
        let reader_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(pool.connect_options().as_ref().clone())
            .await
            .expect("reader pool");
        let (mock, backend) = backend(reader_pool.clone());
        let held = reader_pool.acquire().await.expect("hold only connection");
        let mut requested = vec![macs[0]];
        requested.extend((1..8).map(|index| MacAddress::new([2, 0, 0, 0, 3, index])));
        let cached_mac = MacAddress::new([2, 0, 0, 0, 4, 1]);
        requested.push(cached_mac);
        backend.firmware_jobs.lock().unwrap().insert(
            cached_mac,
            vec![RmsTrackedFirmwareJob::FirmwareObject("cached".into())],
        );
        mock.enqueue_get_firmware_job_status(Ok(firmware_response(
            "cached",
            rms::FirmwareJobState::Completed,
        )))
        .await;
        let (entered, release) = mock.block_next_get_firmware_job_status().await;

        // Eight fallbacks appear first in caller order. They must not occupy
        // every workflow slot and hide the cached ninth job behind this pool wait.
        tokio::time::pause();
        let started = Instant::now();
        let mut read = Box::pin(ComputeTrayManager::get_firmware_jobs(&backend, &requested));
        let arrival = tokio::select! {
            arrived = tokio::time::timeout(Duration::from_secs(2), entered) => arrived,
            result = &mut read => {
                tokio::time::resume();
                panic!("completed before cached poll admission: {result:?}");
            }
        };
        if !matches!(arrival, Ok(Ok(()))) {
            tokio::time::resume();
            panic!("cached job did not reach RMS while the pool was held: {arrival:?}");
        }
        if release.send(()).is_err() {
            tokio::time::resume();
            panic!("cached poll disappeared before release");
        }
        let expired = tokio::time::timeout(Duration::from_secs(20), &mut read).await;
        let finished = Instant::now();
        drop(read);
        tokio::time::resume();

        let results = expired
            .expect("independent completion watchdog")
            .expect("endpoint observations");
        let deadline = started + Duration::from_secs(10);
        // Real setup leaves a fractional millisecond; Tokio rounds up one tick.
        assert!((deadline..=deadline + Duration::from_millis(1)).contains(&finished));
        assert_eq!(
            results
                .iter()
                .map(|result| result.mac_address)
                .collect::<Vec<_>>(),
            requested
        );
        assert!(
            results[..8]
                .iter()
                .all(|result| result.jobs == Err(FirmwareJobReadError::DeadlineExceeded))
        );
        let cached = results[8]
            .jobs
            .as_ref()
            .expect("cached observation survives fallback expiry");
        assert_eq!(
            cached
                .iter()
                .map(|job| (job.kind, job.job_id.as_str(), job.state))
                .collect::<Vec<_>>(),
            [(
                ObservedJobKind::FirmwareObject,
                "cached",
                Ok(FirmwareState::Completed)
            )]
        );
        // Completed evidence passed the post-deadline check while the pool's
        // only connection remained held throughout the request.
        assert_eq!(
            mock.get_firmware_job_status_calls()
                .await
                .iter()
                .map(|call| call.job_id.as_str())
                .collect::<Vec<_>>(),
            ["cached"]
        );
        drop(held);
        // Later phases exercise database recovery without cached tracking.
        backend.firmware_jobs.lock().unwrap().remove(&cached_mac);

        let reader_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&reader_pool)
            .await
            .expect("reader PID");
        let mut blocker = pool.begin().await.expect("block tracking table");
        sqlx::query("LOCK TABLE direct_dispatch_rms_jobs IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .expect("table lock");
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await
            .expect("blocker PID");
        let mut read = Box::pin(ComputeTrayManager::get_firmware_jobs(&backend, &macs));
        let blocked_pid = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(2), wait_for_blocked_query(&pool, blocker_pid, "FROM direct_dispatch_rms_jobs")) => {
                result.expect("get_all entered table wait")
            }
            result = &mut read => {
                panic!("read completed before table wait: {result:?}");
            }
        };
        assert_eq!(blocked_pid, reader_pid);

        drop(read);

        assert_eq!(
            reader_pool.size(),
            0,
            "caller drop releases slot synchronously"
        );
        let replacement: i32 = tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::query_scalar("SELECT 1").fetch_one(&reader_pool),
        )
        .await
        .expect("replacement while lock remains held")
        .expect("replacement SELECT");
        assert_eq!(replacement, 1);
        blocker.rollback().await.expect("release table lock");

        let healthy_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&reader_pool)
            .await
            .expect("replacement PID");
        mock.enqueue_get_firmware_job_status(Ok(firmware_response(
            "job",
            rms::FirmwareJobState::Completed,
        )))
        .await;
        let (entered, release) = mock.block_next_get_firmware_job_status().await;
        let mut read = Box::pin(ComputeTrayManager::get_firmware_jobs(&backend, &macs));
        tokio::select! {
            entered = tokio::time::timeout(Duration::from_secs(2), entered) => {
                entered.expect("hardware admission deadline").expect("RMS poll entered");
            }
            result = &mut read => {
                panic!("read completed before hardware gate: {result:?}");
            }
        }

        let reused_pid: i32 = tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&reader_pool),
        )
        .await
        .expect("snapshot released before RMS")
        .expect("healthy pool borrower");

        assert_eq!(
            reused_pid, healthy_pid,
            "healthy snapshot committed and reused the connection"
        );
        release.send(()).expect("release hardware poll");
        assert_eq!(
            read.await.expect("healthy observation")[0]
                .jobs
                .as_ref()
                .expect("known job")[0]
                .state,
            Ok(FirmwareState::Completed)
        );
        assert!(backend.firmware_jobs.lock().unwrap().is_empty());
        reader_pool.close().await;
    };
    tokio::time::timeout(Duration::from_secs(30), lifecycle)
        .await
        .expect("real database lifecycle stays within watchdog");
}

/// A recovery permit must gate session acquisition even when the pool has
/// spare capacity. Polling the queued reader once makes that boundary explicit.
#[carbide_macros::sqlx_test]
async fn tracking_recovery_permit_gates_sessions_independently_of_pool_size(pool: PgPool) {
    let lifecycle = async {
        use db::test_support::postgres::wait_for_blocked_query;
        // connect_with eagerly opens min_connections; no asynchronous return
        // from a borrowed connection is needed to establish the spare slot.
        let reader_pool = sqlx::postgres::PgPoolOptions::new()
            .min_connections(2)
            .max_connections(2)
            .connect_with(pool.connect_options().as_ref().clone())
            .await
            .expect("two-slot reader pool");
        let (mock, backend) = backend(reader_pool.clone());
        let reads = Semaphore::new(1);
        let first_mac = MacAddress::new([2, 0, 0, 0, 5, 1]);
        let second_mac = MacAddress::new([2, 0, 0, 0, 5, 2]);
        let deadline = Instant::now() + READ_TIMEOUT;
        let mut blocker = pool.begin().await.expect("first tracking blocker");
        sqlx::query("LOCK TABLE direct_dispatch_rms_jobs IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .expect("lock tracking table");
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await
            .expect("blocker PID");
        let mut first = Box::pin(backend.read_known_firmware_jobs(
            first_mac,
            Vec::new(),
            false,
            &reads,
            deadline,
        ));
        tokio::select! {
            observed = tokio::time::timeout(Duration::from_secs(2), wait_for_blocked_query(&pool, blocker_pid, "FROM direct_dispatch_rms_jobs")) => {
                observed.expect("first recovery query must reach its lock wait");
            }
            result = &mut first => {
                panic!("first recovery completed before its table wait: {result:?}");
            }
        }
        assert_eq!(
            reader_pool.num_idle(),
            1,
            "first query leaves spare pool capacity"
        );
        let mut second = Box::pin(backend.read_known_firmware_jobs(
            second_mac,
            Vec::new(),
            false,
            &reads,
            deadline,
        ));

        std::future::poll_fn(|cx| {
            assert!(
                second.as_mut().poll(cx).is_pending(),
                "second recovery waits for its permit"
            );
            Poll::Ready(())
        })
        .await;

        // If the helper acquires a session before its permit, this poll consumes
        // the idle connection even though PostgreSQL has not executed its query.
        assert_eq!(
            reader_pool.num_idle(),
            1,
            "queued recovery must not acquire a session"
        );
        blocker.rollback().await.expect("release first query");
        assert!(
            tokio::time::timeout(Duration::from_secs(2), first)
                .await
                .expect("first recovery finishes")
                .expect("empty first tracking")
                .is_empty()
        );

        // Reblock before polling the queued future: its observed lock wait now
        // proves it was admitted after the first recovery released its permit.
        let mut blocker = pool.begin().await.expect("second tracking blocker");
        sqlx::query("LOCK TABLE direct_dispatch_rms_jobs IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .expect("relock tracking table");
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await
            .expect("second blocker PID");
        tokio::select! {
            observed = tokio::time::timeout(Duration::from_secs(2), wait_for_blocked_query(&pool, blocker_pid, "FROM direct_dispatch_rms_jobs")) => {
                observed.expect("queued recovery eventually acquires a session");
            }
            result = &mut second => {
                panic!("second recovery completed before its table wait: {result:?}");
            }
        }
        blocker.rollback().await.expect("release second query");
        assert!(
            tokio::time::timeout(Duration::from_secs(2), second)
                .await
                .expect("second recovery finishes")
                .expect("empty second tracking")
                .is_empty()
        );
        assert!(mock.get_firmware_job_status_calls().await.is_empty());
        reader_pool.close().await;
    };
    tokio::time::timeout(Duration::from_secs(30), lifecycle)
        .await
        .expect("recovery admission proof finishes within watchdog");
}

/// Capability and admission refusals happen before tracking or hardware I/O.
#[tokio::test]
async fn unsupported_defaults_and_input_bounds_do_not_start_work() {
    let compute = MockComputeTrayManager;
    let switch = MockNvSwitchManager::default();
    let shelf = MockPowerShelfManager;
    assert_eq!(compute.firmware_job_source(), None);
    assert_eq!(switch.firmware_job_source(), None);
    assert_eq!(shelf.firmware_job_source(), None);

    let results = [
        ComputeTrayManager::get_firmware_jobs(&compute, &[]).await,
        NvSwitchManager::get_firmware_jobs(&switch, &[]).await,
        PowerShelfManager::get_firmware_jobs(&shelf, &[]).await,
    ];

    assert!(
        results
            .iter()
            .all(|result| matches!(result, Err(ComponentManagerError::Unsupported(_))))
    );
    let (mock, backend) = backend(lazy_pool());
    assert_eq!(
        ComputeTrayManager::firmware_job_source(&backend),
        Some(FirmwareJobSource::Rms)
    );
    assert_eq!(
        NvSwitchManager::firmware_job_source(&backend),
        Some(FirmwareJobSource::Rms)
    );
    assert_eq!(
        PowerShelfManager::firmware_job_source(&backend),
        Some(FirmwareJobSource::Rms)
    );
    assert!(
        backend
            .observe_firmware_jobs(&[], true, Instant::now())
            .await
            .expect("empty input")
            .is_empty()
    );
    assert_eq!(ENDPOINT_LIMIT, 64);
    assert_eq!(STATEMENT_TIMEOUT, Duration::from_secs(5));
    let mac = MacAddress::new([2, 0, 0, 0, 0, 1]);
    // Known tracking makes accidental admission observable as an RPC, rather
    // than a failed database lookup on the intentionally unavailable pool.
    backend.firmware_jobs.lock().unwrap().insert(
        mac,
        vec![RmsTrackedFirmwareJob::FirmwareObject("boundary-job".into())],
    );
    let macs = vec![mac; 65];

    let oversized = backend
        .observe_firmware_jobs(&macs, true, Instant::now() + READ_TIMEOUT)
        .await;

    assert!(matches!(
        oversized,
        Err(ComponentManagerError::InvalidArgument(_))
    ));
    assert!(mock.get_firmware_job_status_calls().await.is_empty());
    assert!(
        mock.get_switch_system_image_job_status_calls()
            .await
            .is_empty()
    );

    // The exact accepted boundary retains every repeated slot; rejecting 64
    // or silently deduplicating output would violate the public contract.
    for _ in 0..64 {
        mock.enqueue_get_firmware_job_status(Ok(firmware_response(
            "boundary-job",
            rms::FirmwareJobState::Completed,
        )))
        .await;
    }

    let accepted = ComputeTrayManager::get_firmware_jobs(&backend, &macs[..64])
        .await
        .expect("64 management MACs are accepted");

    assert_eq!(
        accepted
            .iter()
            .map(|result| result.mac_address)
            .collect::<Vec<_>>(),
        vec![mac; 64]
    );
    for result in accepted {
        let jobs = result.jobs.expect("known boundary tracking");
        assert_eq!(
            jobs.iter()
                .map(|job| (job.kind, job.job_id.as_str(), job.state))
                .collect::<Vec<_>>(),
            [(
                ObservedJobKind::FirmwareObject,
                "boundary-job",
                Ok(FirmwareState::Completed)
            )]
        );
    }
    assert_eq!(
        mock.get_firmware_job_status_calls()
            .await
            .iter()
            .map(|call| call.job_id.clone())
            .collect::<Vec<_>>(),
        vec!["boundary-job".to_owned(); 64]
    );
}
