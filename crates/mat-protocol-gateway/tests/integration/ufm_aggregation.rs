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

//! Wire tests for UFM aggregation and the source-list watch: fake machine-a-tron instances and
//! a fake controller on loopback, the gateway's router served over plain HTTP, and assertions
//! against the UFM API it exposes.

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use mat_protocol_gateway::{ControllerConfig, Gateway, wait_for_source_list, watch_source_list};
use tokio_util::sync::CancellationToken;

use crate::common::{
    FakeController, FakeMachineATron, GUID_A, GUID_B, PORT_A, PORT_B, QUIET_PERIOD, auth_token,
    probe, serve, source_list_client, unbound_gateway_config, wait_for_ports,
};

#[tokio::test]
async fn ufm_api_reports_inventory_merged_from_all_controller_sources() {
    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let mat_b = FakeMachineATron::start("mat-b", &[GUID_B]).await;
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 3);
    let config = unbound_gateway_config(controller.serve().await);
    let client = source_list_client(&config);

    let mut gateway = Gateway::new(config, &auth_token()).unwrap();
    let address = serve(gateway.router()).await;
    let http = reqwest::Client::new();

    // Probes are served before bootstrap: alive, but not ready.
    assert_eq!(probe(&http, address, "/livez").await, Some(StatusCode::OK));
    assert_eq!(
        probe(&http, address, "/readyz").await,
        Some(StatusCode::SERVICE_UNAVAILABLE)
    );

    let source_list = gateway.bootstrap(&client).await.unwrap();
    assert_eq!(source_list.generation, 3);
    assert_eq!(source_list.names(), vec!["mat-a", "mat-b"]);
    assert_eq!(probe(&http, address, "/readyz").await, Some(StatusCode::OK));

    wait_for_ports(&http, address, &[(PORT_A, "Active"), (PORT_B, "Active")]).await;

    // The UFM routes keep their token requirement behind the gateway.
    assert_eq!(
        probe(&http, address, "/ufmRestV3/resources/ports").await,
        Some(StatusCode::UNAUTHORIZED)
    );

    gateway.shutdown().await.unwrap();
}

#[tokio::test]
async fn source_watch_reports_an_added_source_and_ignores_unchanged_polls() {
    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let mat_b = FakeMachineATron::start("mat-b", &[GUID_B]).await;
    let controller = FakeController::new(vec![mat_a.source()], 10);
    let config = unbound_gateway_config(controller.serve().await);
    let client = source_list_client(&config);
    let cancellation = CancellationToken::new();
    let startup = wait_for_source_list(&client, &config.controller)
        .await
        .unwrap();

    // Unchanged list: the watch keeps running until cancelled.
    let unchanged = tokio::time::timeout(
        QUIET_PERIOD,
        watch_source_list(&client, &startup, Duration::from_millis(20), &cancellation),
    )
    .await;
    assert!(
        unchanged.is_err(),
        "watch must not return while the source set is unchanged"
    );

    // A second machine-a-tron appears; the controller bumps its generation as usual.
    controller.publish(vec![mat_a.source(), mat_b.source()]);
    let change = tokio::time::timeout(
        Duration::from_secs(5),
        watch_source_list(&client, &startup, Duration::from_millis(20), &cancellation),
    )
    .await
    .unwrap()
    .expect("an added source must be reported");
    assert_eq!(change.startup, 10);
    assert_eq!(change.current, 11);
    assert_eq!(change.added, vec!["mat-b".to_string()]);
    assert!(change.removed.is_empty());

    cancellation.cancel();
    let cancelled =
        watch_source_list(&client, &startup, Duration::from_millis(20), &cancellation).await;
    assert_eq!(cancelled, None);
}

#[tokio::test]
async fn source_watch_survives_a_controller_restart_with_an_unchanged_set() {
    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let mat_b = FakeMachineATron::start("mat-b", &[GUID_B]).await;
    let fleet = vec![mat_a.source(), mat_b.source()];
    let controller = FakeController::new(fleet.clone(), 42);
    let config = unbound_gateway_config(controller.serve().await);
    let client = source_list_client(&config);
    let cancellation = CancellationToken::new();
    let startup = wait_for_source_list(&client, &config.controller)
        .await
        .unwrap();
    assert_eq!(startup.generation, 42);

    // The controller container restarts: generation 0 and not ready for several watch ticks,
    // then the same fleet republished as generation 1.
    let restart = controller.restart_with(fleet, Duration::from_millis(100));
    let watch = watch_source_list(&client, &startup, Duration::from_millis(20), &cancellation);
    let (_, watched) = tokio::join!(restart, tokio::time::timeout(QUIET_PERIOD, watch));
    assert!(
        watched.is_err(),
        "a controller restart with the same source set must not restart the gateway"
    );
    assert_eq!(controller.generation.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn source_watch_detects_a_removed_source_when_the_generation_repeats() {
    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let mat_b = FakeMachineATron::start("mat-b", &[GUID_B]).await;
    // A fresh pod: the controller's first publication is generation 1.
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 1);
    let config = unbound_gateway_config(controller.serve().await);
    let client = source_list_client(&config);
    let cancellation = CancellationToken::new();
    let startup = wait_for_source_list(&client, &config.controller)
        .await
        .unwrap();

    // mat-b disappears while the controller container is down, so the restarted controller
    // publishes the smaller set as generation 1 again. Comparing counters alone would miss it.
    let restart = controller.restart_with(vec![mat_a.source()], Duration::from_millis(50));
    let watch = tokio::time::timeout(
        Duration::from_secs(5),
        watch_source_list(&client, &startup, Duration::from_millis(20), &cancellation),
    );
    let (_, watched) = tokio::join!(restart, watch);
    let change = watched.unwrap().expect("a removed source must be reported");
    assert_eq!(change.startup, 1);
    assert_eq!(change.current, 1);
    assert!(change.added.is_empty());
    assert_eq!(change.removed, vec!["mat-b".to_string()]);
}

#[tokio::test]
async fn startup_waits_for_readiness_and_gives_up_after_bounded_attempts() {
    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let controller = FakeController::new(vec![mat_a.source()], 1);
    controller.ready.store(false, Ordering::SeqCst);
    let mut config = unbound_gateway_config(controller.serve().await);
    config.controller.startup_attempts = 5;
    let client = source_list_client(&config);

    let error = wait_for_source_list(&client, &config.controller)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("after 5 attempts"),
        "unexpected error: {error}"
    );

    // A controller that becomes ready during the retry window unblocks startup. The window
    // (attempts x retry interval) is far longer than the readiness delay so that scheduling
    // jitter on a loaded test host cannot exhaust the attempts first.
    let ready = controller.ready.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        ready.store(true, Ordering::SeqCst);
    });
    let patient = ControllerConfig {
        startup_attempts: 250,
        ..config.controller.clone()
    };
    let list = wait_for_source_list(&client, &patient).await.unwrap();
    assert_eq!(list.names(), vec!["mat-a"]);
}
