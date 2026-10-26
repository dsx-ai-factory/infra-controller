/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#![cfg(feature = "xplane-maintenance-mock")]

use std::time::Duration;

use agent::xplane_proto::xplane_maintenance_client::XplaneMaintenanceClient;
use agent::xplane_proto::{GetLinkMaintenanceRequest, SetLinkMaintenanceRequest};
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::timeout;
use tonic::transport::Endpoint;
use tower::service_fn;

#[tokio::test]
async fn standalone_mock_accepts_commands_preserves_occupied_socket_and_cleans_up() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("xplane.sock");
    let binary = env!("CARGO_BIN_EXE_xplane-maintenance-mock");
    let mut server = Command::new(binary)
        .arg("--socket-path")
        .arg(&socket)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut client = timeout(Duration::from_secs(5), async {
        loop {
            assert!(
                server.try_wait().unwrap().is_none(),
                "mock exited during startup"
            );
            let path = socket.clone();
            let result = Endpoint::from_static("http://localhost")
                .connect_with_connector(service_fn(move |_| {
                    let path = path.clone();
                    async move { UnixStream::connect(path).await.map(TokioIo::new) }
                }))
                .await;
            if let Ok(channel) = result {
                break XplaneMaintenanceClient::new(channel);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let occupied = timeout(
        Duration::from_secs(5),
        Command::new(binary)
            .arg("--socket-path")
            .arg(&socket)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!occupied.status.success());
    assert!(socket.exists());

    // Every Astra port is readable before the agent sends any maintenance update.
    for nic in ["A56", "A43", "A53", "A46", "A6", "A13", "A3", "A16"] {
        for port in 0..4 {
            let pf_name = format!("{nic}p{port}");
            assert!(
                client
                    .get_link_maintenance(GetLinkMaintenanceRequest {
                        pf_name: pf_name.clone(),
                    })
                    .await
                    .unwrap()
                    .into_inner()
                    .enabled,
                "{pf_name} must start in maintenance mode"
            );
        }
    }

    // Multiple commands reach the same standalone server and update its state.
    for enabled in [true, false] {
        client
            .set_link_maintenance(SetLinkMaintenanceRequest {
                pf_name: "A56p0".to_string(),
                enabled: Some(enabled),
            })
            .await
            .unwrap();
        let readback = client
            .get_link_maintenance(GetLinkMaintenanceRequest {
                pf_name: "A56p0".to_string(),
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(readback.enabled, enabled);
    }

    assert!(
        client
            .get_link_maintenance(GetLinkMaintenanceRequest {
                pf_name: "A56p1".to_string(),
            })
            .await
            .unwrap()
            .into_inner()
            .enabled,
        "updating one port must not change another port"
    );

    assert!(
        Command::new("kill")
            .arg("-INT")
            .arg(server.id().unwrap().to_string())
            .status()
            .await
            .unwrap()
            .success()
    );
    assert!(
        timeout(Duration::from_secs(5), server.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!socket.exists());
}
