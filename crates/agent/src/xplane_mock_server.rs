/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Stateful Xplane maintenance mock for DPU agent tests and manual verification. State is kept per PF
//! netdev for the lifetime of the server; an unset PF returns NotFound.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::sync::Arc;

#[cfg(feature = "xplane-maintenance-mock")]
use eyre::WrapErr;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

use crate::xplane_client::proto::xplane_maintenance_server::{
    XplaneMaintenance, XplaneMaintenanceServer,
};
use crate::xplane_client::proto::{
    GetLinkMaintenanceRequest, GetLinkMaintenanceResponse, SetLinkMaintenanceRequest,
};

#[derive(Default)]
pub(crate) struct MockXplaneMaintenance {
    maintenance_by_pf: Mutex<HashMap<String, bool>>,
    pub(crate) set_calls: Mutex<Vec<(String, bool)>>,
    pub(crate) fail_set_for: Mutex<HashSet<String>>,
    pub(crate) readback_override: Mutex<HashMap<String, bool>>,
    pub(crate) stall_get_for: Mutex<HashSet<String>>,
}

#[tonic::async_trait]
impl XplaneMaintenance for MockXplaneMaintenance {
    async fn set_link_maintenance(
        &self,
        request: Request<SetLinkMaintenanceRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();
        if request.pf_name.is_empty() {
            tracing::info!(
                "spx-maintenance: [xplane-mock] ERROR: Mock Xplane rejected SetLinkMaintenance: pf_name is empty"
            );
            return Err(Status::invalid_argument("pf_name is required"));
        }
        let enabled = request
            .enabled
            .ok_or_else(|| {
                tracing::info!(pf_name = %request.pf_name, "spx-maintenance: [xplane-mock] ERROR: Mock Xplane rejected SetLinkMaintenance: enabled is missing");
                Status::invalid_argument("enabled is required")
            })?;
        self.set_calls
            .lock()
            .await
            .push((request.pf_name.clone(), enabled));
        if self.fail_set_for.lock().await.contains(&request.pf_name) {
            tracing::warn!(pf_name = %request.pf_name, enabled, "spx-maintenance: [xplane-mock] ERROR: Mock Xplane injecting SetLinkMaintenance failure");
            return Err(Status::unavailable("mock xplane maintenance failure"));
        }
        tracing::info!(pf_name = %request.pf_name, enabled, "spx-maintenance: [xplane-mock] Mock Xplane maintenance update");
        self.maintenance_by_pf
            .lock()
            .await
            .insert(request.pf_name, enabled);
        Ok(Response::new(()))
    }

    async fn get_link_maintenance(
        &self,
        request: Request<GetLinkMaintenanceRequest>,
    ) -> Result<Response<GetLinkMaintenanceResponse>, Status> {
        let request = request.into_inner();

        if request.pf_name.is_empty() {
            return Err(Status::invalid_argument("pf_name is required"));
        }
        if self.stall_get_for.lock().await.contains(&request.pf_name) {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
        if let Some(enabled) = self
            .readback_override
            .lock()
            .await
            .get(&request.pf_name)
            .copied()
        {
            return Ok(Response::new(GetLinkMaintenanceResponse { enabled }));
        }
        let enabled = self
            .maintenance_by_pf
            .lock()
            .await
            .get(&request.pf_name)
            .copied()
            .ok_or_else(|| Status::not_found("PF maintenance state has not been set"))?;

        Ok(Response::new(GetLinkMaintenanceResponse { enabled }))
    }
}

/// Start a mock on a fresh Unix socket path. Existing paths are left intact.
#[cfg(test)]
pub(crate) async fn serve(socket_path: &Path) -> eyre::Result<()> {
    serve_with_service(socket_path, Arc::new(MockXplaneMaintenance::default())).await
}

#[cfg(test)]
pub(crate) async fn serve_with_service(
    socket_path: &Path,
    service: Arc<MockXplaneMaintenance>,
) -> eyre::Result<()> {
    let listener = UnixListener::bind(socket_path)?;
    tracing::info!(path = %socket_path.display(), "spx-maintenance: [xplane-mock] Xplane maintenance mock server listening");
    serve_with_listener(listener, service, std::future::pending()).await
}

/// Run an interactive mock until Ctrl-C, removing its socket after shutdown.
/// All 32 Astra PF netdevs start in maintenance mode; restarting resets their state.
/// Binding fails if the path already exists; an existing Xplane socket is preserved.
#[cfg(feature = "xplane-maintenance-mock")]
pub async fn run_standalone(socket_path: &Path) -> eyre::Result<()> {
    let listener = UnixListener::bind(socket_path)
        .wrap_err_with(|| format!("binding xplane mock socket at {}", socket_path.display()))?;
    // Match the Astra flavor's initial maintenance state. These are PF netdev
    // names, since Xplane receives resolved netdevs rather than logical names.
    let maintenance_by_pf = ["A56", "A43", "A53", "A46", "A6", "A13", "A3", "A16"]
        .into_iter()
        .flat_map(|nic| (0..4).map(move |port| (format!("{nic}p{port}"), true)))
        .collect::<HashMap<_, _>>();
    tracing::info!(path = %socket_path.display(), interface_count = maintenance_by_pf.len(), maintenance_mode = true, "spx-maintenance: [xplane-mock] Xplane maintenance mock server listening; all planes initialized in maintenance mode until changed");
    let service = Arc::new(MockXplaneMaintenance {
        maintenance_by_pf: Mutex::new(maintenance_by_pf),
        ..Default::default()
    });
    let result = serve_with_listener(listener, service, async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "spx-maintenance: [xplane-mock] ERROR: Could not wait for Xplane mock shutdown signal");
        }
    })
    .await;
    tokio::fs::remove_file(socket_path)
        .await
        .wrap_err_with(|| format!("removing xplane mock socket at {}", socket_path.display()))?;
    result
}

async fn serve_with_listener(
    listener: UnixListener,
    service: Arc<MockXplaneMaintenance>,
    shutdown: impl Future<Output = ()>,
) -> eyre::Result<()> {
    tonic::transport::Server::builder()
        .add_service(XplaneMaintenanceServer::from_arc(service))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hyper_util::rt::TokioIo;
    use tokio::net::UnixStream;
    use tonic::Code;
    use tonic::transport::Endpoint;
    use tower::service_fn;

    use super::*;
    use crate::xplane_client::proto::xplane_maintenance_client::XplaneMaintenanceClient;

    #[tokio::test]
    async fn maintenance_round_trip_over_unix_socket() {
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("xplane.sock");
        let server_path = socket_path.clone();
        let server = tokio::spawn(async move { serve(&server_path).await });
        let channel = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let path = socket_path.clone();
                let result = Endpoint::from_static("http://localhost")
                    .connect_with_connector(service_fn(move |_| {
                        let path = path.clone();
                        async move { UnixStream::connect(path).await.map(TokioIo::new) }
                    }))
                    .await;
                if let Ok(channel) = result {
                    break channel;
                }
                assert!(
                    !server.is_finished(),
                    "mock server stopped before connecting"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let mut client = XplaneMaintenanceClient::new(channel);

        let unknown = client
            .get_link_maintenance(GetLinkMaintenanceRequest {
                pf_name: "A56p0".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(unknown.code(), Code::NotFound);

        for (pf_name, enabled) in [("", Some(true)), ("A56p0", None)] {
            let error = client
                .set_link_maintenance(SetLinkMaintenanceRequest {
                    pf_name: pf_name.into(),
                    enabled,
                })
                .await
                .unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument);
        }
        let error = client
            .get_link_maintenance(GetLinkMaintenanceRequest {
                pf_name: String::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);

        for (pf_name, enabled) in [
            ("A56p0", true),
            ("A56p1", true),
            ("A56p0", false),
            ("A56p0", false),
        ] {
            client
                .set_link_maintenance(SetLinkMaintenanceRequest {
                    pf_name: pf_name.into(),
                    enabled: Some(enabled),
                })
                .await
                .unwrap();
            let response = client
                .get_link_maintenance(GetLinkMaintenanceRequest {
                    pf_name: pf_name.into(),
                })
                .await
                .unwrap()
                .into_inner();
            assert_eq!(response.enabled, enabled);
        }
        assert!(
            client
                .get_link_maintenance(GetLinkMaintenanceRequest {
                    pf_name: "A56p1".into()
                })
                .await
                .unwrap()
                .into_inner()
                .enabled
        );

        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
}
