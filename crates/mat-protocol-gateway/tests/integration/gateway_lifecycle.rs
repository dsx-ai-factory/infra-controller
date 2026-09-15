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

//! Process lifecycle of the gateway against a changing fleet: the exit-and-restart path through
//! [`run`], and the inventory changes that must NOT restart the process (a source that stops
//! answering, a machine-a-tron pod restart behind an unchanged Service).

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use mat_protocol_gateway::{
    ExitReason, Gateway, SOURCE_LIST_CHANGED_EXIT_CODE, SourceListCheck, TlsConfig,
    watch_source_list,
};
use reqwest::header::AUTHORIZATION;
use tokio_util::sync::CancellationToken;
use ufm_mock::FailureAction;

use crate::common::{
    FakeController, FakeMachineATron, GUID_A, GUID_B, GUID_B2, GUID_C, PORT_A, PORT_B, PORT_B2,
    PORT_C, QUIET_PERIOD, auth_token, finished, free_loopback_address, gateway_config, probe,
    serve, source_list_client, spawn_run, ufm_ports, unbound_gateway_config, wait_for_ports,
    wait_until,
};

/// The restart scenario the exit code exists for: the process bound to `{a, b}` exits with code
/// 3 when the controller publishes `{a, b, c}`, and the process the kubelet starts in its place,
/// with the same configuration and the same port, serves the inventory of all three.
#[tokio::test]
async fn run_exits_with_code_3_on_an_added_source_and_the_restart_serves_all_sources() {
    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let mat_b = FakeMachineATron::start("mat-b", &[GUID_B]).await;
    let mat_c = FakeMachineATron::start("mat-c", &[GUID_C]).await;
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 7);
    let listen = free_loopback_address().await;
    let config = gateway_config(controller.serve().await, listen);
    let http = reqwest::Client::new();

    // First process: bound to {a, b}.
    let shutdown = CancellationToken::new();
    let first = spawn_run(config.clone(), shutdown.clone());
    wait_for_ports(&http, listen, &[(PORT_A, "Active"), (PORT_B, "Active")]).await;
    assert_eq!(probe(&http, listen, "/readyz").await, Some(StatusCode::OK));

    // The fleet grows; the controller publishes {a, b, c} as generation 8.
    controller.publish(vec![mat_a.source(), mat_b.source(), mat_c.source()]);
    let reason = finished(first).await;
    let ExitReason::SourceListChanged(change) = &reason else {
        panic!("unexpected exit reason {reason:?}");
    };
    assert_eq!(change.startup, 7);
    assert_eq!(change.current, 8);
    assert_eq!(change.added, vec!["mat-c".to_string()]);
    assert!(change.removed.is_empty());
    assert_eq!(reason.exit_code(), 3);
    assert!(
        shutdown.is_cancelled(),
        "run must cancel its own listener before returning"
    );
    // The listener is released with the process: the port can be bound again.
    assert_eq!(probe(&http, listen, "/livez").await, None);

    // The kubelet restarts the container: same configuration, same port. The controller is
    // replayed as not ready for a moment so the readiness transition is observable.
    controller.ready.store(false, Ordering::SeqCst);
    let shutdown = CancellationToken::new();
    let second = spawn_run(config, shutdown.clone());
    wait_until("the restarted listener answers /livez", || async {
        probe(&http, listen, "/livez").await == Some(StatusCode::OK)
    })
    .await;
    assert_eq!(
        probe(&http, listen, "/readyz").await,
        Some(StatusCode::SERVICE_UNAVAILABLE),
        "the restarted gateway must not report ready before it has a source list"
    );
    assert!(
        ufm_ports(&http, listen).await.is_empty(),
        "inventory is in-memory state and starts empty after a restart"
    );

    controller.ready.store(true, Ordering::SeqCst);
    wait_until("the restarted gateway becomes ready", || async {
        probe(&http, listen, "/readyz").await == Some(StatusCode::OK)
    })
    .await;
    wait_for_ports(
        &http,
        listen,
        &[(PORT_A, "Active"), (PORT_B, "Active"), (PORT_C, "Active")],
    )
    .await;

    shutdown.cancel();
    let reason = finished(second).await;
    assert_eq!(reason, ExitReason::Shutdown);
    assert_eq!(reason.exit_code(), 0);
}

/// SIGTERM must always be exit code 0, whether it arrives while the gateway is serving or while
/// it is still waiting for the controller. Cancelling the shutdown token stops the listener too,
/// so `run` sees two futures complete on one event; the outcome must not depend on which one
/// `select!` reports first. Repeating the shutdown makes a regression of that race visible.
#[tokio::test]
async fn shutdown_is_exit_code_0_whichever_phase_it_interrupts() {
    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let controller = FakeController::new(vec![mat_a.source()], 1);
    let config = unbound_gateway_config(controller.serve().await);

    // While serving: bootstrap has completed and the watch loop is running.
    for _ in 0..50 {
        let fetched = controller.fetches();
        let shutdown = CancellationToken::new();
        let running = spawn_run(config.clone(), shutdown.clone());
        controller.wait_for_fetches(fetched + 1).await;
        shutdown.cancel();
        assert_eq!(finished(running).await, ExitReason::Shutdown);
    }

    // While bootstrapping: the controller is not ready, so `run` is still inside the startup wait.
    controller.ready.store(false, Ordering::SeqCst);
    for _ in 0..50 {
        let fetched = controller.fetches();
        let shutdown = CancellationToken::new();
        let running = spawn_run(config.clone(), shutdown.clone());
        controller.wait_for_fetches(fetched + 1).await;
        shutdown.cancel();
        assert_eq!(finished(running).await, ExitReason::Shutdown);
    }
}

/// A machine-a-tron pod restart behind an unchanged Service is absorbed by the inventory epoch:
/// the source set is the same, so the process keeps running, and the UFM ports follow the new
/// epoch's inventory. That holds for a restart of a healthy source and for the realistic one,
/// where the source is unreachable long enough to be marked down first and then comes back with
/// a new epoch.
#[tokio::test]
async fn run_keeps_serving_across_a_source_epoch_change_and_follows_its_new_inventory() {
    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let mat_b = FakeMachineATron::start("mat-b", &[GUID_B]).await;
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 3);
    let listen = free_loopback_address().await;
    let mut config = gateway_config(controller.serve().await, listen);
    config.ufm.inventory.failure_grace_period = Duration::from_millis(150);
    config.validate().unwrap();
    let http = reqwest::Client::new();

    let shutdown = CancellationToken::new();
    let running = spawn_run(config, shutdown.clone());
    wait_for_ports(&http, listen, &[(PORT_A, "Active"), (PORT_B, "Active")]).await;

    // mat-b's pod restarts: new epoch_id, generation back at 1, a different port GUID. The
    // controller list is untouched (same name, same Service URL).
    let fetches_before = controller.fetches();
    mat_b.restart(&[GUID_B2]);
    wait_for_ports(&http, listen, &[(PORT_A, "Active"), (PORT_B2, "Active")]).await;

    // The pod goes down for longer than the grace period, so its port is marked down, and comes
    // back under yet another epoch with its original GUID. The marked-down source must not be
    // mistaken for the same epoch: the new inventory replaces the old one.
    mat_b.set_available(false);
    wait_for_ports(&http, listen, &[(PORT_A, "Active"), (PORT_B2, "Down")]).await;
    mat_b.restart(&[GUID_B]);
    wait_for_ports(&http, listen, &[(PORT_A, "Active"), (PORT_B, "Active")]).await;

    // The watch loop is still polling and has found nothing to act on.
    tokio::time::sleep(QUIET_PERIOD).await;
    assert!(
        !running.is_finished(),
        "an epoch change must not end the gateway process"
    );
    assert!(
        controller.fetches() > fetches_before,
        "the source list watch must keep polling"
    );
    assert_eq!(controller.generation.load(Ordering::SeqCst), 3);
    assert_eq!(probe(&http, listen, "/readyz").await, Some(StatusCode::OK));

    shutdown.cancel();
    assert_eq!(finished(running).await, ExitReason::Shutdown);
}

/// With `[tls]` set, the listener must serve HTTPS from the PEM files, the probes
/// and the UFM API must answer over it, plain HTTP must be refused, and the exit-3 restart must
/// bind the same TLS port again.
#[tokio::test]
async fn run_serves_over_tls_and_restarts_on_the_same_tls_port() {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let certs = tempfile::tempdir().unwrap();
    let cert_path = certs.path().join("tls.crt");
    let key_path = certs.path().join("tls.key");
    std::fs::write(&cert_path, certified.cert.pem()).unwrap();
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).unwrap();

    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let mat_b = FakeMachineATron::start("mat-b", &[GUID_B]).await;
    let controller = FakeController::new(vec![mat_a.source()], 1);
    let listen = free_loopback_address().await;
    let mut config = gateway_config(controller.serve().await, listen);
    config.tls = Some(TlsConfig {
        cert_path,
        key_path,
    });
    config.validate().unwrap();
    // The certificate is self-signed, so the test client accepts any certificate.
    let https = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let probe_tls = |path: &'static str| {
        let https = https.clone();
        async move {
            https
                .get(format!("https://localhost:{}{path}", listen.port()))
                .send()
                .await
                .ok()
                .map(|response| response.status())
        }
    };
    let tls_port_guids = || {
        let https = https.clone();
        async move {
            let ports: Vec<serde_json::Value> = https
                .get(format!(
                    "https://localhost:{}/ufmRestV3/resources/ports",
                    listen.port()
                ))
                .header(AUTHORIZATION, format!("Basic {}", crate::common::TOKEN))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            let mut guids: Vec<String> = ports
                .iter()
                .map(|port| port["guid"].as_str().unwrap().to_string())
                .collect();
            guids.sort();
            guids
        }
    };

    let shutdown = CancellationToken::new();
    let first = spawn_run(config.clone(), shutdown.clone());
    wait_until("the TLS listener answers /livez", || async {
        probe_tls("/livez").await == Some(StatusCode::OK)
    })
    .await;
    wait_until("/readyz over TLS", || async {
        probe_tls("/readyz").await == Some(StatusCode::OK)
    })
    .await;
    wait_until("the UFM API over TLS lists mat-a", || async {
        tls_port_guids().await == vec![PORT_A.to_string()]
    })
    .await;
    assert!(
        reqwest::Client::new()
            .get(format!("http://{listen}/livez"))
            .timeout(crate::common::WAIT)
            .send()
            .await
            .is_err(),
        "the TLS listener must not answer plain HTTP"
    );

    // A fleet change ends the process; the replacement binds the same TLS port and serves both.
    controller.publish(vec![mat_a.source(), mat_b.source()]);
    let reason = finished(first).await;
    assert_eq!(reason.exit_code(), SOURCE_LIST_CHANGED_EXIT_CODE);
    assert_eq!(probe_tls("/livez").await, None, "the port is released");

    let shutdown = CancellationToken::new();
    let second = spawn_run(config, shutdown.clone());
    wait_until("the restarted TLS listener is ready", || async {
        probe_tls("/readyz").await == Some(StatusCode::OK)
    })
    .await;
    wait_until(
        "the restarted gateway serves both sources over TLS",
        || async { tls_port_guids().await == vec![PORT_A.to_string(), PORT_B.to_string()] },
    )
    .await;

    shutdown.cancel();
    assert_eq!(finished(second).await, ExitReason::Shutdown);
}

/// Drives one source down and back up under `failure_action`, returning the ports served while
/// the source was down and after it recovered. The source set never changes, so the watch loop
/// has nothing to report either way.
async fn vanished_source_ports(
    failure_action: FailureAction,
) -> (Vec<crate::common::UfmPort>, Vec<crate::common::UfmPort>) {
    let mat_a = FakeMachineATron::start("mat-a", &[GUID_A]).await;
    let mat_b = FakeMachineATron::start("mat-b", &[GUID_B]).await;
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 1);
    let mut config = unbound_gateway_config(controller.serve().await);
    config.ufm.inventory.failure_grace_period = Duration::from_millis(150);
    config.ufm.inventory.failure_action = failure_action;
    config.validate().unwrap();
    let client = source_list_client(&config);

    let mut gateway = Gateway::new(config, &auth_token()).unwrap();
    let address = serve(gateway.router()).await;
    let startup = gateway.bootstrap(&client).await.unwrap();
    let http = reqwest::Client::new();
    wait_for_ports(&http, address, &[(PORT_A, "Active"), (PORT_B, "Active")]).await;

    // mat-b stops answering (its Service has no endpoints). After the grace period the
    // reconciliation applies the failure action; mat-a is unaffected.
    mat_b.set_available(false);
    let while_down = tokio::time::timeout(crate::common::WAIT, async {
        loop {
            let ports = ufm_ports(&http, address).await;
            let b_active = ports
                .iter()
                .any(|port| port.guid == PORT_B && port.logical_state == "Active");
            if !b_active {
                return ports;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the failure action must be applied after the grace period");

    // The controller still lists mat-b: nothing for the watch to report.
    let current = client.fetch().await.unwrap();
    assert_eq!(
        SourceListCheck::compare(&startup, &current),
        SourceListCheck::Unchanged
    );
    let cancellation = CancellationToken::new();
    let watched = tokio::time::timeout(
        QUIET_PERIOD,
        watch_source_list(&client, &startup, Duration::from_millis(20), &cancellation),
    )
    .await;
    assert!(
        watched.is_err(),
        "a source that stops answering must not restart the gateway"
    );

    // mat-b answers again with the same epoch: its ports come back without a restart.
    mat_b.set_available(true);
    let recovered = wait_for_ports(&http, address, &[(PORT_A, "Active"), (PORT_B, "Active")]).await;

    gateway.shutdown().await.unwrap();
    (while_down, recovered)
}

#[tokio::test]
async fn mark_down_keeps_a_vanished_sources_ports_listed_as_down() {
    let (while_down, recovered) = vanished_source_ports(FailureAction::MarkDown).await;

    assert_eq!(
        while_down
            .iter()
            .map(|port| (port.guid.as_str(), port.logical_state.as_str()))
            .collect::<Vec<_>>(),
        vec![(PORT_A, "Active"), (PORT_B, "Down")]
    );
    let down = &while_down[1];
    assert_eq!(down.physical_state, "Link Down");
    assert_eq!(while_down[0].physical_state, "Link Up");
    assert_eq!(recovered[1].logical_state, "Active");
    assert_eq!(recovered[1].physical_state, "Link Up");
}

#[tokio::test]
async fn remove_drops_a_vanished_sources_ports_until_it_returns() {
    let (while_down, recovered) = vanished_source_ports(FailureAction::Remove).await;

    assert_eq!(
        while_down
            .iter()
            .map(|port| port.guid.as_str())
            .collect::<Vec<_>>(),
        vec![PORT_A]
    );
    assert_eq!(
        recovered
            .iter()
            .map(|port| port.guid.as_str())
            .collect::<Vec<_>>(),
        vec![PORT_A, PORT_B]
    );
}
