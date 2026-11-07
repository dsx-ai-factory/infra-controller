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

//! Calls the local Weave service over its Unix-domain socket.
//!
//! Each operation has one ten-second deadline, including connection, client
//! readiness, and the complete response. Each request gets its own budget, with
//! no retries here or enclosing reconciliation deadline; an iteration's total
//! wait grows with the number of operations attempted. Timeout or cancellation
//! stops waiting, but does not prove that a mutation failed. Reconciliation must
//! observe service state again before choosing its next action.

pub mod proto {
    tonic::include_proto!("nvidia.weave.controller.v1");
}

use std::time::Duration;

use eyre::WrapErr;
use hyper_util::rt::TokioIo;
use proto::network_isolation_service_client::NetworkIsolationServiceClient;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// Default Unix-domain socket path for the local Weave flow controller.
pub const WEAVE_EW_VPC_FLOW_CONTROLLER_SOCKET_PATH: &str =
    "/var/run/dpf/weave/grpc/flow-controller.sock";

// Ten seconds matches the agent's existing local DPU command budget. This
// bounds each request's wait; it is not a measured Weave service SLA.
const WEAVE_EW_VPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

async fn weave_ew_vpc_connect_uds(socket_path: &str) -> eyre::Result<Channel> {
    let socket_path = socket_path.to_owned();
    let socket_path_for_error = socket_path.clone();
    let channel = Endpoint::try_from("http://[::]:50051")
        .map_err(|e| eyre::eyre!("failed to create endpoint: {e}"))?
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = socket_path.clone();
            async move {
                let stream = UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(TokioIo::new(stream))
            }
        }))
        .await
        .map_err(|e| eyre::eyre!("connect to UDS at {socket_path_for_error}: {e}"))?;
    Ok(channel)
}

// Accept an unawaited connection so dialing and the RPC share one deadline.
async fn weave_ew_vpc_request<R, F>(
    connection: impl Future<Output = eyre::Result<Channel>>,
    operation: &'static str,
    request: impl FnOnce(NetworkIsolationServiceClient<Channel>) -> F,
) -> eyre::Result<R>
where
    F: Future<Output = Result<tonic::Response<R>, tonic::Status>>,
{
    tokio::time::timeout(WEAVE_EW_VPC_REQUEST_TIMEOUT, async {
        let channel = connection.await?;
        let client = NetworkIsolationServiceClient::new(channel);
        let response = request(client)
            .await
            .wrap_err_with(|| format!("{operation} gRPC failed"))?;
        Ok(response.into_inner())
    })
    .await
    .wrap_err_with(|| format!("{operation} timed out after {WEAVE_EW_VPC_REQUEST_TIMEOUT:?}"))?
}

/// Requests creation of a virtual network and returns the service's response.
pub async fn weave_ew_vpc_create_virtual_network(
    socket_path: &str,
    request: proto::CreateVirtualNetworkRequest,
) -> eyre::Result<proto::CreateVirtualNetworkResponse> {
    tracing::info!(?request, "CreateVirtualNetwork request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "CreateVirtualNetwork",
        |mut client| async move { client.create_virtual_network(request).await },
    )
    .await?;
    tracing::info!(?response, "CreateVirtualNetwork response");
    Ok(response)
}

/// Requests an update to a virtual network and returns the service's response.
pub async fn weave_ew_vpc_update_virtual_network(
    socket_path: &str,
    request: proto::UpdateVirtualNetworkRequest,
) -> eyre::Result<proto::UpdateVirtualNetworkResponse> {
    tracing::info!(?request, "UpdateVirtualNetwork request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "UpdateVirtualNetwork",
        |mut client| async move { client.update_virtual_network(request).await },
    )
    .await?;
    tracing::info!(?response, "UpdateVirtualNetwork response");
    Ok(response)
}

/// Requests deletion of a virtual network and returns the service's response.
pub async fn weave_ew_vpc_delete_virtual_network(
    socket_path: &str,
    request: proto::DeleteVirtualNetworkRequest,
) -> eyre::Result<proto::DeleteVirtualNetworkResponse> {
    tracing::info!(?request, "DeleteVirtualNetwork request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "DeleteVirtualNetwork",
        |mut client| async move { client.delete_virtual_network(request).await },
    )
    .await?;
    tracing::info!(?response, "DeleteVirtualNetwork response");
    Ok(response)
}

/// Retrieves a virtual network and returns the service's response.
pub async fn weave_ew_vpc_get_virtual_network(
    socket_path: &str,
    request: proto::GetVirtualNetworkRequest,
) -> eyre::Result<proto::GetVirtualNetworkResponse> {
    tracing::info!(?request, "GetVirtualNetwork request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "GetVirtualNetwork",
        |mut client| async move { client.get_virtual_network(request).await },
    )
    .await?;
    tracing::info!(?response, "GetVirtualNetwork response");
    Ok(response)
}

/// Lists virtual networks and returns the service's response.
pub async fn weave_ew_vpc_list_virtual_networks(
    socket_path: &str,
    request: proto::ListVirtualNetworksRequest,
) -> eyre::Result<proto::ListVirtualNetworksResponse> {
    tracing::info!(?request, "ListVirtualNetworks request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "ListVirtualNetworks",
        |mut client| async move { client.list_virtual_networks(request).await },
    )
    .await?;
    tracing::info!(?response, "ListVirtualNetworks response");
    Ok(response)
}

/// Requests creation of a virtual network attachment and returns the service's response.
pub async fn weave_ew_vpc_create_virtual_network_attachment(
    socket_path: &str,
    request: proto::CreateVirtualNetworkAttachmentRequest,
) -> eyre::Result<proto::CreateVirtualNetworkAttachmentResponse> {
    tracing::info!(?request, "CreateVirtualNetworkAttachment request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "CreateVirtualNetworkAttachment",
        |mut client| async move { client.create_virtual_network_attachment(request).await },
    )
    .await?;
    tracing::info!(?response, "CreateVirtualNetworkAttachment response");
    Ok(response)
}

/// Requests an update to a virtual network attachment and returns the service's response.
pub async fn weave_ew_vpc_update_virtual_network_attachment(
    socket_path: &str,
    request: proto::UpdateVirtualNetworkAttachmentRequest,
) -> eyre::Result<proto::UpdateVirtualNetworkAttachmentResponse> {
    tracing::info!(?request, "UpdateVirtualNetworkAttachment request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "UpdateVirtualNetworkAttachment",
        |mut client| async move { client.update_virtual_network_attachment(request).await },
    )
    .await?;
    tracing::info!(?response, "UpdateVirtualNetworkAttachment response");
    Ok(response)
}

/// Requests deletion of a virtual network attachment and returns the service's response.
pub async fn weave_ew_vpc_delete_virtual_network_attachment(
    socket_path: &str,
    request: proto::DeleteVirtualNetworkAttachmentRequest,
) -> eyre::Result<proto::DeleteVirtualNetworkAttachmentResponse> {
    tracing::info!(?request, "DeleteVirtualNetworkAttachment request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "DeleteVirtualNetworkAttachment",
        |mut client| async move { client.delete_virtual_network_attachment(request).await },
    )
    .await?;
    tracing::info!(?response, "DeleteVirtualNetworkAttachment response");
    Ok(response)
}

/// Retrieves a virtual network attachment and returns the service's response.
pub async fn weave_ew_vpc_get_virtual_network_attachment(
    socket_path: &str,
    request: proto::GetVirtualNetworkAttachmentRequest,
) -> eyre::Result<proto::GetVirtualNetworkAttachmentResponse> {
    tracing::info!(?request, "GetVirtualNetworkAttachment request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "GetVirtualNetworkAttachment",
        |mut client| async move { client.get_virtual_network_attachment(request).await },
    )
    .await?;
    tracing::info!(?response, "GetVirtualNetworkAttachment response");
    Ok(response)
}

/// Lists virtual network attachments and returns the service's response.
pub async fn weave_ew_vpc_list_virtual_network_attachments(
    socket_path: &str,
    request: proto::ListVirtualNetworkAttachmentsRequest,
) -> eyre::Result<proto::ListVirtualNetworkAttachmentsResponse> {
    tracing::info!(?request, "ListVirtualNetworkAttachments request");
    let response = weave_ew_vpc_request(
        weave_ew_vpc_connect_uds(socket_path),
        "ListVirtualNetworkAttachments",
        |mut client| async move { client.list_virtual_network_attachments(request).await },
    )
    .await?;
    tracing::info!(?response, "ListVirtualNetworkAttachments response");
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::task::Poll;

    use tokio::io::AsyncReadExt;
    use tokio::net::UnixListener;

    use super::*;
    use crate::weave_ew_vpc_mock_server;

    async fn start_mock_server() -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let _keep = dir.keep();
        let path_clone = socket_path.clone();
        tokio::spawn(async move {
            weave_ew_vpc_mock_server::serve(&path_clone).await.unwrap();
        });
        // Give the server a moment to bind
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        socket_path
    }

    /// Hold connection setup open so this fails if the deadline starts only
    /// after a channel is ready.
    #[tokio::test(start_paused = true)]
    async fn request_deadline_includes_pending_connection() {
        let request = weave_ew_vpc_request(
            std::future::pending::<eyre::Result<Channel>>(),
            "DeleteVirtualNetwork",
            |_| std::future::ready(Ok(tonic::Response::new(()))),
        );
        tokio::pin!(request);
        let started = tokio::time::Instant::now();
        assert!(futures_util::poll!(&mut request).is_pending());

        tokio::time::advance(Duration::from_millis(9999)).await;
        assert!(
            futures_util::poll!(&mut request).is_pending(),
            "request returned before its ten-second deadline"
        );
        assert_eq!(
            tokio::time::Instant::now(),
            started + Duration::from_millis(9999)
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        // Poll directly so a missing deadline fails here instead of hanging.
        let Poll::Ready(result) = futures_util::poll!(&mut request) else {
            panic!("pending connection exceeded its ten-second deadline");
        };
        assert_eq!(
            tokio::time::Instant::now(),
            started + Duration::from_secs(10)
        );
        let error = result.unwrap_err();
        assert_eq!(
            error.to_string(),
            "DeleteVirtualNetwork timed out after 10s"
        );
        assert!(
            error
                .downcast_ref::<tokio::time::error::Elapsed>()
                .is_some()
        );
    }

    /// A real socket checks that the public delete wrapper uses the shared
    /// deadline when its peer never responds.
    #[tokio::test(start_paused = true)]
    async fn request_deadline_bounds_stalled_http2_peer() {
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("unresponsive.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let request = weave_ew_vpc_delete_virtual_network(
            socket_path.to_str().unwrap(),
            proto::DeleteVirtualNetworkRequest {
                id: "network-to-delete".to_string(),
            },
        );
        tokio::pin!(request);

        // Tonic can finish connecting before server SETTINGS arrive. Keep this
        // peer silent so the public wrapper times out waiting for an RPC response.
        let accept_peer = async {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut preface = [0; 24];
            peer.read_exact(&mut preface).await.unwrap();
            assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
            peer
        };
        tokio::pin!(accept_peer);
        let started = tokio::time::Instant::now();
        let mut accepted_peer = None;
        // Keep the test runnable while polling I/O. An ordinary await could
        // auto-advance Tokio's paused clock and conceal a wrong deadline.
        for _ in 0..1000 {
            assert!(futures_util::poll!(&mut request).is_pending());
            if let Poll::Ready(peer) = futures_util::poll!(&mut accept_peer) {
                accepted_peer = Some(peer);
                break;
            }
            tokio::task::yield_now().await;
        }
        let peer = accepted_peer.expect("client did not begin HTTP/2 setup");
        assert_eq!(tokio::time::Instant::now(), started);

        tokio::time::advance(Duration::from_millis(9999)).await;
        for _ in 0..1000 {
            assert!(
                futures_util::poll!(&mut request).is_pending(),
                "request returned before its ten-second deadline"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(
            tokio::time::Instant::now(),
            started + Duration::from_millis(9999)
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        let mut result = None;
        for _ in 0..1000 {
            if let Poll::Ready(response) = futures_util::poll!(&mut request) {
                result = Some(response);
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            tokio::time::Instant::now(),
            started + Duration::from_secs(10)
        );
        let error = result
            .expect("request exceeded its ten-second deadline")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "DeleteVirtualNetwork timed out after 10s"
        );
        assert!(
            error
                .downcast_ref::<tokio::time::error::Elapsed>()
                .is_some()
        );
        drop(peer);
    }

    #[tokio::test]
    async fn test_create_virtual_network() {
        let socket_path = start_mock_server().await;
        let path_str = socket_path.to_str().unwrap();

        let resp = weave_ew_vpc_create_virtual_network(
            path_str,
            proto::CreateVirtualNetworkRequest {
                metadata: None,
                spec: Some(proto::VirtualNetworkSpec {
                    vni: 100,
                    subnet_ipv4: Some("10.0.0.0/8".to_string()),
                    subnet_ipv6: None,
                }),
            },
        )
        .await
        .unwrap();

        let vn = resp.virtual_network.unwrap();
        assert!(vn.metadata.unwrap().id.is_some());
        assert_eq!(vn.spec.unwrap().vni, 100);
        assert_eq!(
            vn.status.unwrap().state.unwrap().phase,
            proto::state::Phase::Ready as i32,
        );
    }

    #[tokio::test]
    async fn test_delete_virtual_network() {
        let socket_path = start_mock_server().await;
        let path_str = socket_path.to_str().unwrap();

        let _resp = weave_ew_vpc_delete_virtual_network(
            path_str,
            proto::DeleteVirtualNetworkRequest {
                id: "test-id".to_string(),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_get_virtual_network() {
        let socket_path = start_mock_server().await;
        let path_str = socket_path.to_str().unwrap();

        let resp = weave_ew_vpc_get_virtual_network(
            path_str,
            proto::GetVirtualNetworkRequest {
                id: "my-vnet".to_string(),
            },
        )
        .await
        .unwrap();

        let vn = resp.virtual_network.unwrap();
        assert_eq!(vn.metadata.unwrap().id.unwrap(), "my-vnet");
        assert_eq!(vn.spec.unwrap().vni, 100);
    }

    #[tokio::test]
    async fn test_list_virtual_networks() {
        let socket_path = start_mock_server().await;
        let path_str = socket_path.to_str().unwrap();

        let resp = weave_ew_vpc_list_virtual_networks(
            path_str,
            proto::ListVirtualNetworksRequest { vni: None },
        )
        .await
        .unwrap();

        assert_eq!(resp.virtual_networks.len(), 1);
        let vn = &resp.virtual_networks[0];
        assert_eq!(
            vn.metadata.as_ref().unwrap().id.as_deref(),
            Some("02:aa:bb:cc:dd:ee_100")
        );
    }

    #[tokio::test]
    async fn test_create_virtual_network_attachment() {
        let socket_path = start_mock_server().await;
        let path_str = socket_path.to_str().unwrap();

        let resp = weave_ew_vpc_create_virtual_network_attachment(
            path_str,
            proto::CreateVirtualNetworkAttachmentRequest {
                metadata: None,
                spec: Some(proto::VirtualNetworkAttachmentSpec {
                    vnet_id: "vnet-1".to_string(),
                    nic_id: "02:aa:bb:cc:dd:ee".to_string(),
                    attachment_type: proto::AttachmentType::Pf.into(),
                    attachment_pf: Some(proto::AttachmentPf {
                        pf_id: "02:aa:bb:cc:dd:ee".to_string(),
                    }),
                    attachment_vf: None,
                    attachment_ovs: None,
                }),
            },
        )
        .await
        .unwrap();

        let vna = resp.virtual_network_attachment.unwrap();
        assert!(vna.metadata.unwrap().id.is_some());
        assert_eq!(vna.status.unwrap().host_ipv4, Some("10.0.0.1".to_string()),);
    }

    #[tokio::test]
    async fn test_delete_virtual_network_attachment() {
        let socket_path = start_mock_server().await;
        let path_str = socket_path.to_str().unwrap();

        let _resp = weave_ew_vpc_delete_virtual_network_attachment(
            path_str,
            proto::DeleteVirtualNetworkAttachmentRequest {
                id: "attach-id".to_string(),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_get_virtual_network_attachment() {
        let socket_path = start_mock_server().await;
        let path_str = socket_path.to_str().unwrap();

        let resp = weave_ew_vpc_get_virtual_network_attachment(
            path_str,
            proto::GetVirtualNetworkAttachmentRequest {
                id: "my-attach".to_string(),
            },
        )
        .await
        .unwrap();

        let vna = resp.virtual_network_attachment.unwrap();
        assert_eq!(
            vna.metadata.as_ref().unwrap().id.as_deref(),
            Some("my-attach")
        );
        assert_eq!(vna.spec.as_ref().unwrap().vnet_id, "02:aa:bb:cc:dd:ee_100");
        assert_eq!(
            vna.status.as_ref().unwrap().host_ipv4,
            Some("10.0.0.1".to_string())
        );
    }

    #[tokio::test]
    async fn test_list_virtual_network_attachments() {
        let socket_path = start_mock_server().await;
        let path_str = socket_path.to_str().unwrap();

        let resp = weave_ew_vpc_list_virtual_network_attachments(
            path_str,
            proto::ListVirtualNetworkAttachmentsRequest {
                vnet_id: None,
                nic_id: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(resp.virtual_network_attachments.len(), 1);
        let vna = &resp.virtual_network_attachments[0];
        assert_eq!(
            vna.metadata.as_ref().unwrap().id.as_deref(),
            Some("02:aa:bb:cc:dd:ee_pf0"),
        );
    }
}
