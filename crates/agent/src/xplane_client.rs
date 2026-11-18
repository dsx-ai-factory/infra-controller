/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::path::Path;
use std::time::Duration;

use eyre::WrapErr;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

use self::proto::xplane_maintenance_client::XplaneMaintenanceClient;
use self::proto::{GetLinkMaintenanceRequest, SetLinkMaintenanceRequest};

/// Xplane maintenance wire types and generated gRPC client/server bindings.
pub mod proto {
    tonic::include_proto!("xplane.maintenance.v1");
}

pub(crate) const XPLANE_MAINTENANCE_SOCKET_PATH: &str = "/var/run/xplane/core.sock";

// Local unary requests and connection establishment each have a three-second
// deadline, including channel waits. Failed attempts retry on the next config
// poll. Ports share one channel and run concurrently, avoiding 32 serial waits.
pub(crate) const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) async fn connect(socket_path: &Path) -> eyre::Result<XplaneMaintenanceClient<Channel>> {
    tracing::info!(socket_path = %socket_path.display(), timeout_seconds = ATTEMPT_TIMEOUT.as_secs(), "spx-maintenance: [dpu-agent] Connecting to Xplane maintenance service");
    let path = socket_path.to_owned();
    let channel = tokio::time::timeout(
        ATTEMPT_TIMEOUT,
        Endpoint::from_static("http://localhost").connect_with_connector(service_fn(move |_| {
            let path = path.clone();
            async move { UnixStream::connect(path).await.map(TokioIo::new) }
        })),
    )
    .await
    .inspect_err(|error| tracing::info!(socket_path = %socket_path.display(), %error, "spx-maintenance: [dpu-agent] ERROR: Xplane maintenance connection timed out"))
    .wrap_err("xplane connection timed out")?
    .inspect_err(|error| tracing::info!(socket_path = %socket_path.display(), %error, "spx-maintenance: [dpu-agent] ERROR: Xplane maintenance connection failed"))
    .wrap_err_with(|| format!("connecting to xplane at {}", socket_path.display()))?;
    tracing::info!(socket_path = %socket_path.display(), "spx-maintenance: [dpu-agent] Connected to Xplane maintenance service");
    Ok(XplaneMaintenanceClient::new(channel))
}

async fn read(
    client: &mut XplaneMaintenanceClient<Channel>,
    netdev: &str,
) -> eyre::Result<Result<bool, tonic::Status>> {
    let mut request = tonic::Request::new(GetLinkMaintenanceRequest {
        pf_name: netdev.to_string(),
    });
    request.set_timeout(ATTEMPT_TIMEOUT);
    let result = tokio::time::timeout(ATTEMPT_TIMEOUT, client.get_link_maintenance(request))
        .await
        .wrap_err("xplane readback timed out")?
        .map(|response| response.into_inner().enabled);
    Ok(result)
}

/// Read every poll. Without a configured mode, report the current state without
/// changing it. An explicit mode is enforced and verified after any Set.
/// Unlike Weave, Xplane does not store or return our configuration revision.
/// Skipping reads based on an agent-cached revision would miss state changes,
/// including those following an Xplane restart. Compare the actual state on each
/// poll instead, and skip SetLinkMaintenance when it already matches.
pub(crate) async fn reconcile(
    mut client: XplaneMaintenanceClient<Channel>,
    netdev: &str,
    cfg_maintenance_mode: Option<bool>,
) -> eyre::Result<bool> {
    let readback = read(&mut client, netdev).await?;
    let Some(maintenance_mode) = cfg_maintenance_mode else {
        return readback.wrap_err("reading xplane maintenance mode");
    };
    match readback {
        Ok(actual) if actual == maintenance_mode => {
            return Ok(actual);
        }
        Ok(_) => {}
        Err(error) if error.code() == tonic::Code::NotFound => {}
        Err(error) => return Err(error).wrap_err("reading xplane maintenance mode"),
    }
    let mut request = tonic::Request::new(SetLinkMaintenanceRequest {
        pf_name: netdev.to_string(),
        enabled: Some(maintenance_mode),
    });
    request.set_timeout(ATTEMPT_TIMEOUT);

    tokio::time::timeout(ATTEMPT_TIMEOUT, client.set_link_maintenance(request))
        .await
        .wrap_err("xplane maintenance update timed out")?
        .wrap_err("setting xplane maintenance mode")?;

    let actual = read(&mut client, netdev)
        .await?
        .wrap_err("verifying xplane maintenance mode")?;
    tracing::info!(
        pf_name = netdev,
        requested_maintenance_mode = maintenance_mode,
        observed_maintenance_mode = actual,
        "spx-maintenance: [dpu-agent] Xplane maintenance update readback"
    );
    Ok(actual)
}
