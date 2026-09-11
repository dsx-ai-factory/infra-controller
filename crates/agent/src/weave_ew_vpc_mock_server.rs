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

use std::path::Path;

use proto::network_isolation_service_server::{
    NetworkIsolationService, NetworkIsolationServiceServer,
};
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

use crate::weave_ew_vpc_client::proto;

pub struct DummyNetworkIsolationService;

#[tonic::async_trait]
impl NetworkIsolationService for DummyNetworkIsolationService {
    async fn create_virtual_network(
        &self,
        request: Request<proto::CreateVirtualNetworkRequest>,
    ) -> Result<Response<proto::CreateVirtualNetworkResponse>, Status> {
        let req = request.into_inner();

        let spec = req
            .spec
            .ok_or_else(|| Status::invalid_argument("spec is required"))?;

        tracing::info!(?spec, "Creating virtual network");

        let mut metadata = req.metadata.unwrap_or_default();
        if metadata.id.is_none() {
            metadata.id = Some(uuid::Uuid::new_v4().to_string());
        }
        metadata.creation_timestamp =
            Some(prost_types::Timestamp::date_time_nanos(2026, 1, 1, 0, 0, 0, 0).unwrap());

        let vn = proto::VirtualNetwork {
            metadata: Some(metadata),
            spec: Some(spec),
            status: Some(proto::VirtualNetworkStatus {
                state: Some(proto::State {
                    phase: proto::state::Phase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
            }),
        };

        Ok(Response::new(proto::CreateVirtualNetworkResponse {
            virtual_network: Some(vn),
        }))
    }

    async fn delete_virtual_network(
        &self,
        request: Request<proto::DeleteVirtualNetworkRequest>,
    ) -> Result<Response<proto::DeleteVirtualNetworkResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(virtual_network_id = %req.id, "Deleting virtual network");
        Ok(Response::new(proto::DeleteVirtualNetworkResponse {}))
    }

    async fn get_virtual_network(
        &self,
        request: Request<proto::GetVirtualNetworkRequest>,
    ) -> Result<Response<proto::GetVirtualNetworkResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(virtual_network_id = %req.id, "Getting virtual network");

        let vn = proto::VirtualNetwork {
            metadata: Some(proto::ObjectMetadata {
                id: Some(req.id),
                creation_timestamp: Some(
                    prost_types::Timestamp::date_time_nanos(2026, 1, 1, 0, 0, 0, 0).unwrap(),
                ),
                ..Default::default()
            }),
            spec: Some(proto::VirtualNetworkSpec {
                vni: 100,
                subnet_ipv4: Some("10.0.0.0/8".to_string()),
                subnet_ipv6: None,
            }),
            status: Some(proto::VirtualNetworkStatus {
                state: Some(proto::State {
                    phase: proto::state::Phase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
            }),
        };

        Ok(Response::new(proto::GetVirtualNetworkResponse {
            virtual_network: Some(vn),
        }))
    }

    async fn update_virtual_network(
        &self,
        request: Request<proto::UpdateVirtualNetworkRequest>,
    ) -> Result<Response<proto::UpdateVirtualNetworkResponse>, Status> {
        let req = request.into_inner();
        let metadata = req
            .metadata
            .ok_or_else(|| Status::invalid_argument("metadata is required"))?;
        tracing::info!(virtual_network_id = ?metadata.id, "Updating virtual network");

        let vn = proto::VirtualNetwork {
            metadata: Some(metadata),
            spec: None,
            status: Some(proto::VirtualNetworkStatus {
                state: Some(proto::State {
                    phase: proto::state::Phase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
            }),
        };

        Ok(Response::new(proto::UpdateVirtualNetworkResponse {
            virtual_network: Some(vn),
        }))
    }

    async fn list_virtual_networks(
        &self,
        request: Request<proto::ListVirtualNetworksRequest>,
    ) -> Result<Response<proto::ListVirtualNetworksResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(vni = ?req.vni, "Listing virtual networks");

        let vn = proto::VirtualNetwork {
            metadata: Some(proto::ObjectMetadata {
                id: Some("02:aa:bb:cc:dd:ee_100".to_string()),
                creation_timestamp: Some(
                    prost_types::Timestamp::date_time_nanos(2026, 1, 1, 0, 0, 0, 0).unwrap(),
                ),
                ..Default::default()
            }),
            spec: Some(proto::VirtualNetworkSpec {
                vni: 100,
                subnet_ipv4: Some("10.0.0.0/8".to_string()),
                subnet_ipv6: None,
            }),
            status: Some(proto::VirtualNetworkStatus {
                state: Some(proto::State {
                    phase: proto::state::Phase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
            }),
        };

        Ok(Response::new(proto::ListVirtualNetworksResponse {
            virtual_networks: vec![vn],
        }))
    }

    async fn create_virtual_network_attachment(
        &self,
        request: Request<proto::CreateVirtualNetworkAttachmentRequest>,
    ) -> Result<Response<proto::CreateVirtualNetworkAttachmentResponse>, Status> {
        let req = request.into_inner();

        let spec = req
            .spec
            .ok_or_else(|| Status::invalid_argument("spec is required"))?;

        tracing::info!(?spec, "Creating virtual network attachment");

        let mut metadata = req.metadata.unwrap_or_default();
        if metadata.id.is_none() {
            metadata.id = Some(uuid::Uuid::new_v4().to_string());
        }
        metadata.creation_timestamp =
            Some(prost_types::Timestamp::date_time_nanos(2026, 1, 1, 0, 0, 0, 0).unwrap());

        let vna = proto::VirtualNetworkAttachment {
            metadata: Some(metadata),
            spec: Some(spec),
            status: Some(proto::VirtualNetworkAttachmentStatus {
                state: Some(proto::State {
                    phase: proto::state::Phase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
                host_ipv4: Some("10.0.0.1".to_string()),
                host_ipv6: None,
            }),
        };

        Ok(Response::new(
            proto::CreateVirtualNetworkAttachmentResponse {
                virtual_network_attachment: Some(vna),
            },
        ))
    }

    async fn delete_virtual_network_attachment(
        &self,
        request: Request<proto::DeleteVirtualNetworkAttachmentRequest>,
    ) -> Result<Response<proto::DeleteVirtualNetworkAttachmentResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(
            virtual_network_attachment_id = %req.id,
            "Deleting virtual network attachment"
        );
        Ok(Response::new(
            proto::DeleteVirtualNetworkAttachmentResponse {},
        ))
    }

    async fn get_virtual_network_attachment(
        &self,
        request: Request<proto::GetVirtualNetworkAttachmentRequest>,
    ) -> Result<Response<proto::GetVirtualNetworkAttachmentResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(
            virtual_network_attachment_id = %req.id,
            "Getting virtual network attachment"
        );

        let vna = proto::VirtualNetworkAttachment {
            metadata: Some(proto::ObjectMetadata {
                id: Some(req.id),
                creation_timestamp: Some(
                    prost_types::Timestamp::date_time_nanos(2026, 1, 1, 0, 0, 0, 0).unwrap(),
                ),
                ..Default::default()
            }),
            spec: Some(proto::VirtualNetworkAttachmentSpec {
                vnet_id: "02:aa:bb:cc:dd:ee_100".to_string(),
                nic_id: "02:aa:bb:cc:dd:ee".to_string(),
                attachment_type: proto::AttachmentType::Pf.into(),
                attachment_pf: Some(proto::AttachmentPf {
                    pf_id: "02:aa:bb:cc:dd:ee".to_string(),
                }),
                attachment_vf: None,
                attachment_ovs: None,
            }),
            status: Some(proto::VirtualNetworkAttachmentStatus {
                state: Some(proto::State {
                    phase: proto::state::Phase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
                host_ipv4: Some("10.0.0.1".to_string()),
                host_ipv6: None,
            }),
        };

        Ok(Response::new(proto::GetVirtualNetworkAttachmentResponse {
            virtual_network_attachment: Some(vna),
        }))
    }

    async fn update_virtual_network_attachment(
        &self,
        request: Request<proto::UpdateVirtualNetworkAttachmentRequest>,
    ) -> Result<Response<proto::UpdateVirtualNetworkAttachmentResponse>, Status> {
        let req = request.into_inner();
        let metadata = req
            .metadata
            .ok_or_else(|| Status::invalid_argument("metadata is required"))?;
        tracing::info!(
            virtual_network_attachment_id = ?metadata.id,
            "Updating virtual network attachment"
        );

        let vna = proto::VirtualNetworkAttachment {
            metadata: Some(metadata),
            spec: None,
            status: Some(proto::VirtualNetworkAttachmentStatus {
                state: Some(proto::State {
                    phase: proto::state::Phase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
                host_ipv4: Some("10.0.0.1".to_string()),
                host_ipv6: None,
            }),
        };

        Ok(Response::new(
            proto::UpdateVirtualNetworkAttachmentResponse {
                virtual_network_attachment: Some(vna),
            },
        ))
    }

    async fn list_virtual_network_attachments(
        &self,
        request: Request<proto::ListVirtualNetworkAttachmentsRequest>,
    ) -> Result<Response<proto::ListVirtualNetworkAttachmentsResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(vnet_id = ?req.vnet_id, nic_id = ?req.nic_id, "Listing virtual network attachments");

        let vna = proto::VirtualNetworkAttachment {
            metadata: Some(proto::ObjectMetadata {
                id: Some("02:aa:bb:cc:dd:ee_pf0".to_string()),
                creation_timestamp: Some(
                    prost_types::Timestamp::date_time_nanos(2026, 1, 1, 0, 0, 0, 0).unwrap(),
                ),
                ..Default::default()
            }),
            spec: Some(proto::VirtualNetworkAttachmentSpec {
                vnet_id: "02:aa:bb:cc:dd:ee_100".to_string(),
                nic_id: "02:aa:bb:cc:dd:ee".to_string(),
                attachment_type: proto::AttachmentType::Pf.into(),
                attachment_pf: Some(proto::AttachmentPf {
                    pf_id: "02:aa:bb:cc:dd:ee".to_string(),
                }),
                attachment_vf: None,
                attachment_ovs: None,
            }),
            status: Some(proto::VirtualNetworkAttachmentStatus {
                state: Some(proto::State {
                    phase: proto::state::Phase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
                host_ipv4: Some("10.0.0.1".to_string()),
                host_ipv6: None,
            }),
        };

        Ok(Response::new(
            proto::ListVirtualNetworkAttachmentsResponse {
                virtual_network_attachments: vec![vna],
            },
        ))
    }
}

/// Start the mock server listening on the given UDS path.
/// Removes the socket file if it already exists.
pub async fn serve(socket_path: &Path) -> eyre::Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    let stream = UnixListenerStream::new(listener);

    tracing::info!(path = %socket_path.display(), "Weave EW VPC mock server listening");

    tonic::transport::Server::builder()
        .add_service(NetworkIsolationServiceServer::new(
            DummyNetworkIsolationService,
        ))
        .serve_with_incoming(stream)
        .await
        .map_err(|e| eyre::eyre!("weave EW VPC mock server error: {e}"))?;

    Ok(())
}
