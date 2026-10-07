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

pub mod proto {
    tonic::include_proto!("nvidia.weave.controller.v1");
}

use hyper_util::rt::TokioIo;
use proto::network_isolation_service_client::NetworkIsolationServiceClient;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

pub const WEAVE_EW_VPC_FLOW_CONTROLLER_SOCKET_PATH: &str =
    "/var/run/dpf/weave/grpc/flow-controller.sock";

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

pub async fn weave_ew_vpc_create_virtual_network(
    socket_path: &str,
    request: proto::CreateVirtualNetworkRequest,
) -> eyre::Result<proto::CreateVirtualNetworkResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "CreateVirtualNetwork request");
    let response = client
        .create_virtual_network(request)
        .await
        .map_err(|s| eyre::eyre!("CreateVirtualNetwork gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "CreateVirtualNetwork response");
    Ok(response)
}

pub async fn weave_ew_vpc_update_virtual_network(
    socket_path: &str,
    request: proto::UpdateVirtualNetworkRequest,
) -> eyre::Result<proto::UpdateVirtualNetworkResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "UpdateVirtualNetwork request");
    let response = client
        .update_virtual_network(request)
        .await
        .map_err(|s| eyre::eyre!("UpdateVirtualNetwork gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "UpdateVirtualNetwork response");
    Ok(response)
}

pub async fn weave_ew_vpc_delete_virtual_network(
    socket_path: &str,
    request: proto::DeleteVirtualNetworkRequest,
) -> eyre::Result<proto::DeleteVirtualNetworkResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "DeleteVirtualNetwork request");
    let response = client
        .delete_virtual_network(request)
        .await
        .map_err(|s| eyre::eyre!("DeleteVirtualNetwork gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "DeleteVirtualNetwork response");
    Ok(response)
}

pub async fn weave_ew_vpc_get_virtual_network(
    socket_path: &str,
    request: proto::GetVirtualNetworkRequest,
) -> eyre::Result<proto::GetVirtualNetworkResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "GetVirtualNetwork request");
    let response = client
        .get_virtual_network(request)
        .await
        .map_err(|s| eyre::eyre!("GetVirtualNetwork gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "GetVirtualNetwork response");
    Ok(response)
}

pub async fn weave_ew_vpc_list_virtual_networks(
    socket_path: &str,
    request: proto::ListVirtualNetworksRequest,
) -> eyre::Result<proto::ListVirtualNetworksResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "ListVirtualNetworks request");
    let response = client
        .list_virtual_networks(request)
        .await
        .map_err(|s| eyre::eyre!("ListVirtualNetworks gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "ListVirtualNetworks response");
    Ok(response)
}

pub async fn weave_ew_vpc_create_virtual_network_attachment(
    socket_path: &str,
    request: proto::CreateVirtualNetworkAttachmentRequest,
) -> eyre::Result<proto::CreateVirtualNetworkAttachmentResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "CreateVirtualNetworkAttachment request");
    let response = client
        .create_virtual_network_attachment(request)
        .await
        .map_err(|s| eyre::eyre!("CreateVirtualNetworkAttachment gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "CreateVirtualNetworkAttachment response");
    Ok(response)
}

pub async fn weave_ew_vpc_update_virtual_network_attachment(
    socket_path: &str,
    request: proto::UpdateVirtualNetworkAttachmentRequest,
) -> eyre::Result<proto::UpdateVirtualNetworkAttachmentResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "UpdateVirtualNetworkAttachment request");
    let response = client
        .update_virtual_network_attachment(request)
        .await
        .map_err(|s| eyre::eyre!("UpdateVirtualNetworkAttachment gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "UpdateVirtualNetworkAttachment response");
    Ok(response)
}

pub async fn weave_ew_vpc_delete_virtual_network_attachment(
    socket_path: &str,
    request: proto::DeleteVirtualNetworkAttachmentRequest,
) -> eyre::Result<proto::DeleteVirtualNetworkAttachmentResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "DeleteVirtualNetworkAttachment request");
    let response = client
        .delete_virtual_network_attachment(request)
        .await
        .map_err(|s| eyre::eyre!("DeleteVirtualNetworkAttachment gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "DeleteVirtualNetworkAttachment response");
    Ok(response)
}

pub async fn weave_ew_vpc_get_virtual_network_attachment(
    socket_path: &str,
    request: proto::GetVirtualNetworkAttachmentRequest,
) -> eyre::Result<proto::GetVirtualNetworkAttachmentResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "GetVirtualNetworkAttachment request");
    let response = client
        .get_virtual_network_attachment(request)
        .await
        .map_err(|s| eyre::eyre!("GetVirtualNetworkAttachment gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "GetVirtualNetworkAttachment response");
    Ok(response)
}

pub async fn weave_ew_vpc_list_virtual_network_attachments(
    socket_path: &str,
    request: proto::ListVirtualNetworkAttachmentsRequest,
) -> eyre::Result<proto::ListVirtualNetworkAttachmentsResponse> {
    let channel = weave_ew_vpc_connect_uds(socket_path).await?;
    let mut client = NetworkIsolationServiceClient::new(channel);
    tracing::info!(?request, "ListVirtualNetworkAttachments request");
    let response = client
        .list_virtual_network_attachments(request)
        .await
        .map_err(|s| eyre::eyre!("ListVirtualNetworkAttachments gRPC failed: {s}"))?
        .into_inner();
    tracing::info!(?response, "ListVirtualNetworkAttachments response");
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
