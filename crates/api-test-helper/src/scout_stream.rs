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

use std::net::SocketAddr;
use std::time::Duration;

use carbide_uuid::machine::MachineId;
use rpc::forge::{
    ScoutStreamAdminPingRequest, ScoutStreamConnectionInfo, ScoutStreamDisconnectRequest,
    ScoutStreamShowConnectionsRequest,
};
use rpc::protos::mlx_device::MlxAdminLockdownStatusRequest;

use crate::api_client;

const STATE_CHANGE_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const UNSUPPORTED_MESSAGE: &str = "machine-a-tron does not simulate Mellanox ScoutStream requests";

/// Returns ScoutStream connections registered on one of the supplied API servers.
pub async fn connections(addrs: &[SocketAddr]) -> eyre::Result<Vec<ScoutStreamConnectionInfo>> {
    api_client::call(
        addrs,
        "ScoutStreamShowConnections",
        |mut client| async move {
            client
                .scout_stream_show_connections(ScoutStreamShowConnectionsRequest {})
                .await
        },
    )
    .await
    .map(|response| response.scout_stream_connections)
}

/// Pings a simulated Scout through its registered stream.
pub async fn ping(addrs: &[SocketAddr], machine_id: MachineId) -> eyre::Result<String> {
    api_client::call(addrs, "ScoutStreamPing", |mut client| async move {
        client
            .scout_stream_ping(ScoutStreamAdminPingRequest {
                machine_id: machine_id.into(),
            })
            .await
    })
    .await
    .map(|response| response.pong)
}

/// Administratively disconnects a simulated Scout stream.
pub async fn disconnect(addrs: &[SocketAddr], machine_id: MachineId) -> eyre::Result<bool> {
    api_client::call(addrs, "ScoutStreamDisconnect", |mut client| async move {
        client
            .scout_stream_disconnect(ScoutStreamDisconnectRequest {
                machine_id: machine_id.into(),
            })
            .await
    })
    .await
    .map(|response| response.success)
}

/// Confirms unsupported device operations receive a response instead of timing out.
pub async fn check_unsupported_request(
    addrs: &[SocketAddr],
    machine_id: MachineId,
) -> eyre::Result<()> {
    let result = api_client::call(addrs, "MlxAdminLockdownStatus", |mut client| async move {
        client
            .mlx_admin_lockdown_status(MlxAdminLockdownStatusRequest {
                machine_id: machine_id.into(),
                device_id: "0000:00:00.0".to_string(),
            })
            .await
    })
    .await;

    match result {
        Ok(_) => Err(eyre::eyre!(
            "unsupported ScoutStream request unexpectedly succeeded"
        )),
        Err(error) if format!("{error:#}").contains(UNSUPPORTED_MESSAGE) => Ok(()),
        Err(error) => Err(eyre::eyre!(
            "unsupported ScoutStream request returned the wrong error: {error:#}"
        )),
    }
}

/// Waits until a machine's ScoutStream registration matches `connected`.
pub async fn wait_for_connection_state(
    addrs: &[SocketAddr],
    machine_id: MachineId,
    connected: bool,
) -> eyre::Result<()> {
    tokio::time::timeout(STATE_CHANGE_TIMEOUT, async {
        loop {
            let is_connected = connections(addrs)
                .await?
                .iter()
                .any(|connection| connection.machine_id == Some(machine_id));
            if is_connected == connected {
                return Ok(());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| {
        eyre::eyre!(
            "ScoutStream for {machine_id} did not become {} within {STATE_CHANGE_TIMEOUT:?}",
            if connected {
                "connected"
            } else {
                "disconnected"
            }
        )
    })?
}
