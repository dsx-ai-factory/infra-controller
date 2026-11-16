// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_instrument::testing::capture_logs;
use carbide_test_support::{Check, check_values};
use component_manager::firmware_inventory::{FirmwareComponent, FirmwareInventoryEntry};
use component_manager::firmware_jobs::FirmwareJobObservation;
use model::firmware::FirmwareComponentType;

use super::*;

/// The listener's clock includes queue delay. Parsing must never restart the
/// caller's budget at handler entry, and all standard wire units remain valid.
#[tokio::test(start_paused = true)]
async fn caller_timeout_uses_ingress_and_validates_units() {
    let received = Instant::now();
    tokio::time::advance(Duration::from_secs(2)).await;

    for (text, duration) in [
        ("1H", Duration::from_secs(3600)),
        ("1M", Duration::from_secs(60)),
        ("1S", Duration::from_secs(1)),
        ("1m", Duration::from_millis(1)),
        ("1u", Duration::from_micros(1)),
        ("5n", Duration::from_nanos(5)),
        ("0n", Duration::ZERO),
    ] {
        let mut request = Request::new(());
        request.extensions_mut().insert(RequestReceivedAt(received));
        request
            .metadata_mut()
            .insert("grpc-timeout", text.parse().unwrap());
        assert_eq!(
            caller_deadline(&request).unwrap(),
            Some(received + duration),
            "{text}"
        );
    }
    for text in ["S", "123456789S", "1s", "-1S", "1.0S"] {
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("grpc-timeout", text.parse().unwrap());
        assert!(caller_deadline(&request).is_err(), "{text}");
    }
    assert_eq!(caller_deadline(&Request::new(())).unwrap(), None);
    let handler_received = Instant::now();
    let mut direct = Request::new(());
    direct.set_timeout(Duration::from_secs(1));
    assert_eq!(
        caller_deadline(&direct).unwrap(),
        Some(handler_received + Duration::from_secs(1))
    );
}

/// The real admission owner queues this request behind a blocked handler. The
/// listener receipt layer must stamp it first, so its shorter RPC budget is
/// already exhausted when admission grants the slot.
#[tokio::test]
async fn admission_queue_time_consumes_the_original_rpc_budget() {
    use axum::http::StatusCode;
    use tower::ServiceExt;

    use crate::admission::{ApiAdmissionControl, enforce_grpc};
    use crate::cfg::file::ApiAdmissionControlConfig;
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut tasks = tokio::task::JoinSet::new();
    let control = ApiAdmissionControl::from_config(
        &ApiAdmissionControlConfig {
            max_work_in_flight: 1,
            max_pending: 4,
            max_work_in_flight_per_client: 1,
            max_pending_per_client: 4,
            pending_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        &test_meter.meter(),
        carbide_instrument::Instrumentation::new(test_meter.meter()),
        shutdown.clone(),
        &mut tasks,
    )
    .unwrap()
    .unwrap();
    let arrived = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handler_entered = entered.clone();
    let router = axum::Router::new()
        .route(
            ::rpc::service_path!("Hold"),
            axum::routing::get({
                let arrived = arrived.clone();
                let release = release.clone();
                move || {
                    let arrived = arrived.clone();
                    let release = release.clone();
                    async move {
                        arrived.notify_one();
                        release.notified().await;
                        StatusCode::OK
                    }
                }
            }),
        )
        .route(
            ::rpc::service_path!("GetComponentFirmwareDetails"),
            axum::routing::get(move |request: axum::extract::Request| async move {
                handler_entered.store(true, std::sync::atomic::Ordering::SeqCst);
                let request = Request::from_http(request);
                assert!(
                    caller_deadline(&request)
                        .unwrap()
                        .is_some_and(|deadline| deadline <= Instant::now())
                );
                StatusCode::OK
            }),
        )
        .layer(axum::middleware::from_fn_with_state(control, enforce_grpc))
        .layer(crate::api::grpc_receipt_layer());
    let holding = tokio::spawn(
        router.clone().oneshot(
            axum::http::Request::builder()
                .uri(::rpc::service_path!("Hold"))
                .body(axum::body::Body::empty())
                .unwrap(),
        ),
    );
    tokio::time::timeout(Duration::from_secs(10), arrived.notified())
        .await
        .unwrap();
    let mut queued = Box::pin(
        router.oneshot(
            axum::http::Request::builder()
                .uri(::rpc::service_path!("GetComponentFirmwareDetails"))
                .header("grpc-timeout", "1S")
                .body(axum::body::Body::empty())
                .unwrap(),
        ),
    );
    assert!(futures::poll!(&mut queued).is_pending());

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    release.notify_one();
    let response = queued.await.unwrap();
    tokio::time::resume();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(entered.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!response.headers().contains_key("grpc-status"));
    assert_eq!(holding.await.unwrap().unwrap().status(), StatusCode::OK);
    shutdown.cancel();
    tasks.shutdown().await;
}

/// This public wire validation is distinct from the loader's repeated-slot
/// support; duplicate managed IDs must never reach preparation.
#[test]
fn targets_require_bounded_unique_typed_ids() {
    let target = |count: u128| {
        Target::SwitchIds(rpc::SwitchIdList {
            ids: (0..count)
                .map(|id| uuid::Uuid::from_u128(id).into())
                .collect(),
        })
    };
    let duplicate = Target::PowerShelfIds(rpc::PowerShelfIdList {
        ids: vec![uuid::Uuid::nil().into(); 2],
    });

    check_values(
        [
            Check {
                scenario: "empty",
                input: target(0),
                expect: false,
            },
            Check {
                scenario: "maximum",
                input: target(64),
                expect: true,
            },
            Check {
                scenario: "oversized",
                input: target(65),
                expect: false,
            },
            Check {
                scenario: "typed host selection",
                input: Target::MachineIds(::rpc::common::HostMachineIdList {
                    machine_ids: vec![
                        carbide_uuid::machine::HostMachineId::try_from(
                            carbide_uuid::machine::MachineId::new(
                                carbide_uuid::machine::MachineIdSource::ProductBoardChassisSerial,
                                [9; 32],
                                carbide_uuid::machine::MachineType::Host,
                            ),
                        )
                        .expect("typed host fixture"),
                    ],
                }),
                expect: true,
            },
            Check {
                scenario: "duplicate typed shelves",
                input: duplicate,
                expect: false,
            },
        ],
        |target| validate_target(&target).is_ok(),
    );
}

/// Only matching caller identity may publish the collection. Member failure
/// preserves lookup identity and healthy peers; no name or version is inferred.
#[test]
fn inventory_projection_preserves_members_and_rejects_foreign_results() {
    let mac = MacAddress::new([2, 0, 0, 0, 0, 1]);
    let entries = vec![
        FirmwareInventoryEntry {
            inventory_id: Some("lookup".into()),
            firmware: Ok(FirmwareComponent {
                id: None,
                name: Some("BMC".into()),
                version: Some(String::new()),
                resource_uri: Some("private-artifact-location".into()),
                update_target: None,
                description: None,
                release_date: None,
            }),
        },
        FirmwareInventoryEntry {
            inventory_id: Some("unreadable".into()),
            firmware: Err(FirmwareReadError::ReadFailed),
        },
    ];
    let result = installed_from_inventory(
        Ok(vec![ComponentFirmwareInventoryResult {
            mac_address: mac,
            inventory: Ok(entries),
        }]),
        mac,
        Source::Rms,
        Instant::now() + READ_TIMEOUT,
    );

    let Some(InstalledResult::Inventory(inventory)) = result.result else {
        panic!("expected inventory")
    };
    assert!(result.observed_at.is_some());
    assert_eq!(result.source, Some(Source::Rms as i32));
    assert_eq!(inventory.entries.len(), 2);
    assert_eq!(
        inventory.entries[0].result,
        Some(MemberResult::Component(
            rpc::ComponentFirmwareInstalledComponent {
                id: None,
                name: Some("BMC".into()),
                version: Some(String::new())
            }
        ))
    );
    assert_eq!(
        inventory.entries[1].result,
        Some(MemberResult::Error(firmware_error(ErrorCode::ReadFailed)))
    );
    assert_eq!(
        inventory.entries[1].inventory_id.as_deref(),
        Some("unreadable")
    );
    assert!(!format!("{inventory:?}").contains("private-artifact-location"));

    let empty = installed_from_inventory(
        Ok(vec![ComponentFirmwareInventoryResult {
            mac_address: mac,
            inventory: Ok(vec![]),
        }]),
        mac,
        Source::Rms,
        Instant::now() + READ_TIMEOUT,
    );
    assert!(
        matches!(empty.result, Some(InstalledResult::Inventory(ref rows)) if rows.entries.is_empty())
    );
    assert!(empty.observed_at.is_some());

    for results in [
        vec![],
        vec![ComponentFirmwareInventoryResult {
            mac_address: MacAddress::new([2; 6]),
            inventory: Ok(vec![]),
        }],
    ] {
        let result = super::installed_from_inventory(
            Ok(results),
            mac,
            Source::Rms,
            Instant::now() + READ_TIMEOUT,
        );
        assert!(
            matches!(result.result, Some(InstalledResult::Error(error)) if error.code == ErrorCode::ReadFailed as i32)
        );
        assert!(result.observed_at.is_none());
    }
}

/// Frozen host targets and configured defaults must use the same component
/// names so a client can compare them without translating display labels.
#[test]
fn host_targets_use_configuration_component_names() {
    use model::firmware::DriveModel;

    check_values(
        [
            // The human display label is BMC, but configuration stores bmc.
            Check {
                scenario: "ordinary component uses its stored slug",
                input: FirmwareComponentType::Bmc,
                expect: Some("bmc".to_string()),
            },
            // A generic drive slug would discard the configured model selector.
            Check {
                scenario: "drive retains the exact model",
                input: FirmwareComponentType::Drive {
                    model: DriveModel::new("ACME Disk").expect("valid fixture model"),
                },
                expect: Some("drive-ACME Disk".to_string()),
            },
        ],
        |component| {
            assert_eq!(
                serde_json::to_value(component).unwrap(),
                serde_json::Value::String(component.config_name().into_owned())
            );
            let target = work_target(FirmwareWork::Host {
                component: Some(component),
                version: None,
            });
            match target.target {
                Some(rpc::component_firmware_work_target::Target::Host(target)) => target.component,
                _ => None,
            }
        },
    );
}

/// Wire conversion preserves recorded targets and times; replacement requests
/// do not inherit current-work metadata, and NVOS recovery keeps its own handle.
#[test]
fn stored_actions_preserve_recorded_metadata_and_target_variants() {
    use self::rpc::component_firmware_work_target::Target as WorkTarget;
    use super::super::{CurrentFirmwareWork, FirmwareRequestEvidence};

    let started_at: chrono::DateTime<chrono::Utc> =
        "2026-11-01T01:02:03.123456789Z".parse().unwrap();
    let requested_at: chrono::DateTime<chrono::Utc> =
        "2026-11-02T04:05:06.987654321Z".parse().unwrap();

    check_values(
        [
            Check {
                scenario: "host replacement retains its own version and request metadata",
                input: (
                    CurrentFirmwareWork {
                        work: FirmwareWork::Host {
                            component: Some(FirmwareComponentType::Bmc),
                            version: Some("current-host-version".into()),
                        },
                        job_id: Some("scout-task".into()),
                        started_at: Some(started_at),
                        password_recovery_job_id: None,
                    },
                    FirmwareRequestEvidence {
                        source: FirmwareRequestSource::HostReprovision,
                        work: FirmwareWork::Host {
                            component: None,
                            version: Some("requested-host-version".into()),
                        },
                        requested_at: Some(requested_at),
                        initiator: Some("host-operator".into()),
                    },
                ),
                expect: rpc::ComponentStoredFirmwareActions {
                    current: vec![rpc::ComponentCurrentFirmwareWork {
                        target: Some(rpc::ComponentFirmwareWorkTarget {
                            target: Some(WorkTarget::Host(rpc::ComponentHostFirmwareTarget {
                                component: Some("bmc".into()),
                                version: Some("current-host-version".into()),
                            })),
                        }),
                        job_id: Some("scout-task".into()),
                        started_at: Some(started_at.into()),
                        password_recovery_job_id: None,
                    }],
                    outstanding_requests: vec![rpc::ComponentFirmwareRequest {
                        source: rpc::ComponentFirmwareRequestSource::HostReprovision as i32,
                        target: Some(rpc::ComponentFirmwareWorkTarget {
                            target: Some(WorkTarget::Host(rpc::ComponentHostFirmwareTarget {
                                component: None,
                                version: Some("requested-host-version".into()),
                            })),
                        }),
                        requested_at: Some(requested_at.into()),
                        initiator: Some("host-operator".into()),
                    }],
                    ..Default::default()
                },
            },
            Check {
                scenario: "firmware-object request preserves absent legacy metadata",
                input: (
                    CurrentFirmwareWork {
                        work: FirmwareWork::FirmwareObject {
                            object_id: Some("submitted-object".into()),
                        },
                        job_id: Some("firmware-job".into()),
                        started_at: None,
                        password_recovery_job_id: None,
                    },
                    FirmwareRequestEvidence {
                        source: FirmwareRequestSource::RackMaintenance,
                        work: FirmwareWork::FirmwareObject {
                            object_id: Some("requested-object".into()),
                        },
                        requested_at: None,
                        initiator: None,
                    },
                ),
                expect: rpc::ComponentStoredFirmwareActions {
                    current: vec![rpc::ComponentCurrentFirmwareWork {
                        target: Some(rpc::ComponentFirmwareWorkTarget {
                            target: Some(WorkTarget::FirmwareObject(
                                rpc::ComponentFirmwareObjectTarget {
                                    object_id: Some("submitted-object".into()),
                                },
                            )),
                        }),
                        job_id: Some("firmware-job".into()),
                        started_at: None,
                        password_recovery_job_id: None,
                    }],
                    outstanding_requests: vec![rpc::ComponentFirmwareRequest {
                        source: rpc::ComponentFirmwareRequestSource::RackMaintenance as i32,
                        target: Some(rpc::ComponentFirmwareWorkTarget {
                            target: Some(WorkTarget::FirmwareObject(
                                rpc::ComponentFirmwareObjectTarget {
                                    object_id: Some("requested-object".into()),
                                },
                            )),
                        }),
                        requested_at: None,
                        initiator: None,
                    }],
                    ..Default::default()
                },
            },
            Check {
                scenario: "NVOS keeps image and recovery handles and a recorded empty initiator",
                input: (
                    CurrentFirmwareWork {
                        work: FirmwareWork::Nvos {
                            object_id: Some("submitted-image".into()),
                            version: Some("current-nvos-version".into()),
                        },
                        job_id: Some("image-job".into()),
                        started_at: Some(started_at),
                        password_recovery_job_id: Some("recovery-job".into()),
                    },
                    FirmwareRequestEvidence {
                        source: FirmwareRequestSource::SwitchReprovision,
                        work: FirmwareWork::Nvos {
                            object_id: Some("requested-image".into()),
                            version: None,
                        },
                        requested_at: Some(requested_at),
                        initiator: Some(String::new()),
                    },
                ),
                expect: rpc::ComponentStoredFirmwareActions {
                    current: vec![rpc::ComponentCurrentFirmwareWork {
                        target: Some(rpc::ComponentFirmwareWorkTarget {
                            target: Some(WorkTarget::Nvos(rpc::ComponentNvosFirmwareTarget {
                                object_id: Some("submitted-image".into()),
                                version: Some("current-nvos-version".into()),
                            })),
                        }),
                        job_id: Some("image-job".into()),
                        started_at: Some(started_at.into()),
                        password_recovery_job_id: Some("recovery-job".into()),
                    }],
                    outstanding_requests: vec![rpc::ComponentFirmwareRequest {
                        source: rpc::ComponentFirmwareRequestSource::SwitchReprovision as i32,
                        target: Some(rpc::ComponentFirmwareWorkTarget {
                            target: Some(WorkTarget::Nvos(rpc::ComponentNvosFirmwareTarget {
                                object_id: Some("requested-image".into()),
                                version: None,
                            })),
                        }),
                        requested_at: Some(requested_at.into()),
                        initiator: Some(String::new()),
                    }],
                    ..Default::default()
                },
            },
        ],
        |(current, request)| {
            stored_actions(StoredFirmwareActions {
                current: vec![current],
                outstanding_requests: vec![request],
            })
        },
    );
}

/// Terminal tracking is history; unavailable references establish no active
/// operation. A sampled stage survives a failure reading persistent tracking.
#[test]
fn backend_projection_excludes_history_and_keeps_uncertainty() {
    let mac = MacAddress::new([2; 6]);
    let job = |id: &str, state| FirmwareJobObservation {
        kind: FirmwareJobKind::FirmwareObject,
        job_id: id.into(),
        state,
    };
    let result = backend_work(
        ComponentFirmwareJobsResult {
            mac_address: mac,
            jobs: Ok(vec![
                job("active", Ok(FirmwareState::InProgress)),
                job("terminal", Ok(FirmwareState::Completed)),
                job("missing", Err(FirmwareJobReadError::JobNotFound)),
            ]),
            staged: Some(StagedFirmwareJob::Failed),
        },
        Source::Rms,
        Instant::now() + READ_TIMEOUT,
    );

    assert_eq!(result.source, Some(Source::Rms as i32));
    assert!(result.observed_at.is_some());
    assert_eq!(
        result.active_jobs[0].kind,
        rpc::ComponentFirmwareJobKind::FirmwareObject as i32
    );
    assert_eq!(
        result.active_jobs[0].state,
        rpc::ComponentFirmwareActiveJobState::InProgress as i32
    );
    assert_eq!(
        result
            .active_jobs
            .iter()
            .map(|job| job.job_id.as_str())
            .collect::<Vec<_>>(),
        ["active"]
    );
    assert_eq!(result.unresolved_references[0].job_id, "missing");
    assert_eq!(
        result.unresolved_references[0].error.as_ref().unwrap().code,
        ErrorCode::JobNotFound as i32
    );
    assert!(result.local_stage.is_none());
    assert!(!format!("{result:?}").contains("terminal"));

    let result = backend_work(
        ComponentFirmwareJobsResult {
            mac_address: mac,
            jobs: Err(FirmwareJobReadError::TrackingReadFailed),
            staged: Some(StagedFirmwareJob::Submitting {
                system_image_job_id: "preceding-image".into(),
            }),
        },
        Source::Rms,
        Instant::now() + READ_TIMEOUT,
    );

    assert_eq!(
        result.error.as_ref().unwrap().code,
        ErrorCode::TrackingReadFailed as i32
    );
    assert!(result.observed_at.is_some());
    let stage = result.local_stage.unwrap();
    assert_eq!(stage.system_image_job_id, "preceding-image");
    assert_eq!(
        stage.state,
        rpc::ComponentFirmwareLocalStageState::Submitting as i32
    );

    let failed_reply = backend_work(
        ComponentFirmwareJobsResult {
            mac_address: mac,
            jobs: Err(FirmwareJobReadError::TrackingReadFailed),
            staged: None,
        },
        Source::Rms,
        Instant::now() + READ_TIMEOUT,
    );

    assert_eq!(
        failed_reply.error.as_ref().unwrap().code,
        ErrorCode::TrackingReadFailed as i32
    );
    assert!(failed_reply.observed_at.is_some());
    assert!(failed_reply.local_stage.is_none());
    assert!(
        backend_error(ErrorCode::DeadlineExceeded, Some(Source::Rms))
            .observed_at
            .is_none()
    );

    let logs = capture_logs(|| {
        assert_eq!(
            backend_failure(ComponentManagerError::Internal("private-payload".into())),
            ErrorCode::ReadFailed
        )
    });
    assert!(
        logs.iter()
            .any(|log| log.field("error_category") == Some("ReadFailed"))
    );
    assert!(
        logs.iter()
            .all(|log| !format!("{log:?}").contains("private-payload"))
    );
}

/// The new public comparison retains unknown classification and proposal
/// absence, but does not publish opaque targets or backend diagnostic text.
#[test]
fn comparisons_preserve_unknown_values_without_exposing_targets() {
    let response = Ok(vec![DeviceFirmwareExpectations {
        node_id: "requested-host".into(),
        expectations: Ok(vec![
            component_manager::FirmwareExpectation {
                component: Some("BMC".into()),
                proposed_version: None,
                sku_id: Some("sku-a".into()),
                reported_target: Some("https://private-target?token=private".into()),
                status: FirmwareExpectationStatus::Unknown(74),
            };
            2
        ]),
    }]);
    let result =
        expected_from_comparisons(&response, "requested-host", Instant::now() + READ_TIMEOUT);

    let Some(ExpectedResult::Comparisons(rows)) = result.result else {
        panic!("expected comparison")
    };
    assert_eq!(result.source, Some(Source::Rms as i32));
    assert!(result.observed_at.is_some());
    assert_eq!(
        rows.entries[0].status,
        rpc::ComponentFirmwareComparisonStatus::Unrecognized as i32
    );
    assert_eq!(rows.entries[0].component.as_deref(), Some("BMC"));
    assert_eq!(rows.entries[0].sku_id.as_deref(), Some("sku-a"));
    assert_eq!(rows.entries.len(), 2);
    assert_eq!(rows.entries[0], rows.entries[1]);
    assert_eq!(rows.entries[0].unrecognized_status, Some(74));
    assert!(rows.entries[0].proposed_version.is_none());
    assert!(!format!("{rows:?}").contains("private-target"));
    let foreign = expected_from_comparisons(&response, "other-host", Instant::now() + READ_TIMEOUT);
    assert!(
        matches!(foreign.result, Some(ExpectedResult::Error(error)) if error.code == ErrorCode::ReadFailed as i32)
    );
}

/// A gated result released at virtual expiry must not bypass the final check.
/// Dropping a pending caller also drops the owned read.
#[tokio::test(start_paused = true)]
async fn deadlines_reject_late_ready_results_and_drop_cancelled_work() {
    let result = until_deadline(Instant::now(), async { Ok(()) }).await;
    assert_eq!(result, Err(ErrorCode::DeadlineExceeded));

    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let mut late = Box::pin(until_deadline(Instant::now() + READ_TIMEOUT, async {
        wait.await.expect("late result is released");
        Ok(())
    }));
    assert!(futures::poll!(&mut late).is_pending());

    tokio::time::advance(READ_TIMEOUT).await;
    release.send(()).unwrap();

    assert_eq!(late.await, Err(ErrorCode::DeadlineExceeded));

    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let mut read = Box::pin(until_deadline(Instant::now() + READ_TIMEOUT, async {
        wait.await.expect("caller retains release");
        Ok(())
    }));
    assert!(futures::poll!(&mut read).is_pending());

    drop(read);

    assert!(release.is_closed());
}

/// Host defaults distinguish unavailable configuration from a matched model with
/// no defaults. Synchronous projection still gives expiry precedence over absence.
#[test]
fn host_default_projection_distinguishes_absence_and_expiry() {
    use std::collections::HashMap;

    use bmc_vendor::BMCVendor;
    use carbide_firmware::FirmwareConfig;
    use model::firmware::{Firmware, FirmwareComponent, FirmwareEntry};
    use model::site_explorer::EndpointExplorationReport;

    let host: model::machine::HostMachine =
        model::test_support::machine_snapshot::host_machine().into();
    let expected = |result| rpc::ComponentFirmwareExpected {
        source: Some(Source::EffectiveHostConfiguration as i32),
        observed_at: None,
        result: Some(result),
    };
    // An available version without a default is a known model with empty
    // expectations, rather than a missing configuration.
    let configured = Firmware {
        vendor: BMCVendor::Dell,
        model: "configured-host".into(),
        components: HashMap::from([(
            FirmwareComponentType::Bmc,
            FirmwareComponent {
                known_firmware: vec![FirmwareEntry {
                    version: "available-only".into(),
                    default: false,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let config = FirmwareConfig::new(
        Default::default(),
        &HashMap::from([("host".into(), configured)]),
        &HashMap::new(),
    );

    check_values(
        [
            Check {
                scenario: "expiry wins over unavailable configuration",
                input: (None, true),
                expect: expected(ExpectedResult::Error(firmware_error(
                    ErrorCode::DeadlineExceeded,
                ))),
            },
            Check {
                scenario: "nonexpired absent catalog is unavailable",
                input: (None, false),
                expect: expected(ExpectedResult::Error(firmware_error(
                    ErrorCode::ConfigurationUnavailable,
                ))),
            },
            Check {
                scenario: "nonexpired unmatched report is unavailable",
                input: (Some("unconfigured-host"), false),
                expect: expected(ExpectedResult::Error(firmware_error(
                    ErrorCode::ConfigurationUnavailable,
                ))),
            },
            Check {
                scenario: "matched model without defaults has empty expectations",
                input: (Some("configured-host"), false),
                expect: expected(ExpectedResult::Host(
                    rpc::ComponentHostFirmwareExpectations {
                        vendor: "dell".into(),
                        model: "configured-host".into(),
                        entries: vec![],
                    },
                )),
            },
        ],
        |(model, expired)| {
            let mut context = FirmwareReadContext::default();
            if let Some(model) = model {
                context.host_catalog = Some(config.create_snapshot());
                context.host_reports.insert(
                    host.status.bmc_info.ip.expect("fixture BMC IP"),
                    EndpointExplorationReport {
                        vendor: Some(BMCVendor::Dell),
                        model: Some(model.into()),
                        ..Default::default()
                    },
                );
            }
            let deadline = if expired {
                Instant::now()
            } else {
                Instant::now() + READ_TIMEOUT
            };

            host_expected(&context, &host, deadline)
        },
    );
}
