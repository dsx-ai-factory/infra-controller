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

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, Uri};
use axum::routing::get;
use clap::Parser;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use super::*;

#[derive(Default)]
struct Calls {
    requires_acknowledgement: bool,
    accepted: bool,
    acknowledgements: usize,
    reports: usize,
    lose_ack_once: bool,
    lose_report_once: bool,
    stall_path: Option<&'static str>,
    stalled: Arc<Notify>,
}

async fn grpc_stub(State(calls): State<Arc<Mutex<Calls>>>, uri: Uri) -> Response<Body> {
    let mut calls = calls.lock().unwrap();
    let status = match uri.path() {
        "/forge.Forge/Version" => None,
        "/forge.Forge/AcknowledgeScoutFirmwareUpgrade" => {
            calls.acknowledgements += 1;
            calls.accepted = true;
            if std::mem::take(&mut calls.lose_ack_once) {
                Some(tonic::Status::unavailable("injected lost ack response"))
            } else {
                None
            }
        }
        "/forge.Forge/ReportScoutFirmwareUpgradeStatus" => {
            calls.reports += 1;
            if calls.requires_acknowledgement && !calls.accepted {
                Some(tonic::Status::failed_precondition(
                    "scout firmware task has not been acknowledged",
                ))
            } else if std::mem::take(&mut calls.lose_report_once) {
                Some(tonic::Status::unavailable("injected lost report response"))
            } else {
                None
            }
        }
        other => panic!("unexpected RPC {other}"),
    };
    if calls.stall_path == Some(uri.path()) {
        calls.stall_path = None;
        let stalled = calls.stalled.clone();
        // Version stalls before its message; acknowledgement sends its message
        // but never finishes the stream or supplies grpc-status trailers.
        let message = (uri.path() == "/forge.Forge/AcknowledgeScoutFirmwareUpgrade")
            .then(|| Ok::<_, std::io::Error>(vec![0_u8; 5]));
        let body =
            futures_util::stream::iter(message).chain(futures_util::stream::once(async move {
                stalled.notify_one();
                std::future::pending::<Result<Vec<u8>, std::io::Error>>().await
            }));
        return Response::builder()
            .header("content-type", "application/grpc")
            .body(Body::from_stream(body))
            .unwrap();
    }
    match status {
        Some(status) => status.into_http::<Body>(),
        None => Response::builder()
            .header("content-type", "application/grpc")
            .header("grpc-status", "0")
            .body(Body::from(vec![0_u8; 5]))
            .unwrap(),
    }
}

async fn setup(
    calls: Arc<Mutex<Calls>>,
) -> (
    Options,
    rpc::scout_firmware_upgrade::ScoutFirmwareUpgradeTask,
    tempfile::TempDir,
    std::path::PathBuf,
) {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("executions");
    let script = format!("#!/bin/sh\nprintf run >> '{}'\n", marker.display());
    let digest = hex::encode(Sha256::digest(script.as_bytes()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = Router::new()
        .route(
            "/upgrade.sh",
            get(move || {
                let script = script.clone();
                async move { script }
            }),
        )
        .fallback(grpc_stub)
        .with_state(calls);
    tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let options = Options::parse_from([
        "forge-scout",
        &format!("--api={base}"),
        concat!(
            "--root-ca=",
            env!("CARGO_MANIFEST_DIR"),
            "/../../dev/certs/localhost/ca.crt"
        ),
        concat!(
            "--client-cert=",
            env!("CARGO_MANIFEST_DIR"),
            "/../../dev/certs/localhost/client.crt"
        ),
        concat!(
            "--client-key=",
            env!("CARGO_MANIFEST_DIR"),
            "/../../dev/certs/localhost/client.key"
        ),
    ]);
    let task = rpc::scout_firmware_upgrade::ScoutFirmwareUpgradeTask {
        upgrade_task_id: "firmware-retry-task".into(),
        script: Some(rpc::scout_firmware_upgrade::FileArtifact {
            url: format!("{base}/upgrade.sh"),
            sha256: digest,
        }),
        execution_timeout_seconds: 5,
        ..Default::default()
    };
    (options, task, directory, marker)
}

fn check_status(result: Result<(), CarbideClientError>, code: tonic::Code) {
    match result {
        Err(CarbideClientError::TonicStatusError(status)) => assert_eq!(status.code(), code),
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

/// The subprocess confines the plaintext test override to this test; other
/// tests retain their normal TLS environment even when libtest runs in parallel.
fn run_in_subprocess(test_name: &str) -> bool {
    const CHILD: &str = "SCOUT_FIRMWARE_RETRY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD, "1")
            .env("DISABLE_TLS_ENFORCEMENT", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return true;
    }
    false
}

/// Run the production action handler and RPC client against loopback stubs.
#[test]
fn firmware_action_recovers_lost_responses() {
    if run_in_subprocess("firmware_upgrade_action_tests::firmware_action_recovers_lost_responses") {
        return;
    }

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            // Each task retains its protocol across retries, including legacy tasks.
            for requires_acknowledgement in [true, false] {
                let calls = Arc::new(Mutex::new(Calls {
                    requires_acknowledgement,
                    lose_ack_once: true,
                    lose_report_once: true,
                    ..Default::default()
                }));
                let (config, task, _directory, marker) = setup(calls.clone()).await;
                let machine = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
                    .parse()
                    .unwrap();
                let mut state = firmware_upgrade::ScoutFirmwareUpgradeState::default();
                let action = fac::FirmwareUpgrade {
                    task: Some(task),
                    requires_acknowledgement,
                };
                if requires_acknowledgement {
                    // Losing the acknowledgement response must prevent execution.
                    check_status(
                        handle_firmware_upgrade_action(
                            &config,
                            &machine,
                            action.clone(),
                            &mut state,
                        )
                        .await,
                        tonic::Code::Unavailable,
                    );
                    assert!(!marker.exists());
                    assert_eq!(calls.lock().unwrap().acknowledgements, 1);
                    assert_eq!(calls.lock().unwrap().reports, 0);
                }

                // A lost completion report must retain the result for redelivery.
                check_status(
                    handle_firmware_upgrade_action(&config, &machine, action.clone(), &mut state)
                        .await,
                    tonic::Code::Unavailable,
                );
                assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run");
                assert_eq!(calls.lock().unwrap().reports, 1);
                handle_firmware_upgrade_action(&config, &machine, action, &mut state)
                    .await
                    .unwrap();
                assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run");
                let calls = calls.lock().unwrap();
                assert_eq!(calls.accepted, requires_acknowledgement);
                assert_eq!(
                    calls.acknowledgements,
                    if requires_acknowledgement { 2 } else { 0 }
                );
                assert_eq!(calls.reports, 2);
            }
        })
        .await
        .expect("firmware action recovery must finish within 30 seconds");
    });
}

#[test]
fn firmware_action_times_out_stalled_acknowledgement_and_retries() {
    if run_in_subprocess(
        "firmware_upgrade_action_tests::firmware_action_times_out_stalled_acknowledgement_and_retries",
    ) {
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for (path, acknowledgements_before_retry) in [
                ("/forge.Forge/Version", 0),
                ("/forge.Forge/AcknowledgeScoutFirmwareUpgrade", 1),
            ] {
                let calls = Arc::new(Mutex::new(Calls {
                    requires_acknowledgement: true,
                    stall_path: Some(path),
                    ..Default::default()
                }));
                let stalled = calls.lock().unwrap().stalled.clone();
                let (config, task, _directory, marker) = setup(calls.clone()).await;
                let machine = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
                    .parse()
                    .unwrap();
                let mut state = firmware_upgrade::ScoutFirmwareUpgradeState::default();
                let action = fac::FirmwareUpgrade {
                    task: Some(task),
                    requires_acknowledgement: true,
                };
                {
                    let attempt = handle_firmware_upgrade_action(
                        &config,
                        &machine,
                        action.clone(),
                        &mut state,
                    );
                    tokio::pin!(attempt);
                    tokio::select! {
                        result = &mut attempt => panic!("{path}: returned before stall: {result:?}"),
                        () = stalled.notified() => {},
                        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("{path}: no RPC"),
                    }
                    // Advance only after real transport I/O reaches the stall,
                    // so the regression exercises the complete production caller.
                    tokio::time::pause();
                    tokio::time::advance(FIRMWARE_ACKNOWLEDGEMENT_TIMEOUT).await;
                    let result = tokio::time::timeout(Duration::from_secs(1), attempt)
                        .await
                        .expect("acknowledgement attempt must return at its deadline");
                    tokio::time::resume();
                    let Err(CarbideClientError::TonicStatusError(status)) = result else {
                        panic!("{path}: expected deadline error, got {result:?}");
                    };
                    assert_eq!(status.code(), tonic::Code::DeadlineExceeded, "{path}");
                    assert_eq!(status.message(), "firmware task acknowledgement attempt timed out");
                }
                assert!(!marker.exists(), "{path}: must not execute before acceptance");
                assert_eq!(calls.lock().unwrap().reports, 0, "{path}");
                assert_eq!(calls.lock().unwrap().acknowledgements, acknowledgements_before_retry, "{path}");

                tokio::time::timeout(Duration::from_secs(5), handle_firmware_upgrade_action(
                    &config, &machine, action, &mut state,
                ))
                .await
                .expect("retry must finish")
                .unwrap();
                assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run", "{path}");
                let calls = calls.lock().unwrap();
                assert_eq!(calls.acknowledgements, acknowledgements_before_retry + 1, "{path}");
                assert_eq!(calls.reports, 1, "{path}");
            }
        });
}
