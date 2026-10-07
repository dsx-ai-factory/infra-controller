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

use std::time::Duration;

use carbide_instrument::{Outcome, emit};
use carbide_uuid::machine::MachineId;
use libmlx::profile::error::MlxProfileError;
use rpc::forge::ScoutStreamApiBoundMessage;
use rpc::protos::forge::{scout_stream_api_bound_message, scout_stream_scout_bound_message};
use tokio::sync::mpsc;

use crate::cfg::Options;
use crate::metrics::{ScoutStreamConnection, ScoutStreamReconnect, ScoutStreamResponseDropped};
use crate::{client, mlx_device};

// ScoutStreamError represents errors that can
// occur during the life of a scout stream connection.
#[derive(Debug, thiserror::Error)]
enum ScoutStreamError {
    #[error("gRPC error: {0}")]
    Grpc(#[from] tonic::Status),
    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("profile error: {0}")]
    Profile(#[from] MlxProfileError),
    #[error("connection lost")]
    ConnectionLost,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid URI: {0}")]
    InvalidUri(#[from] http::uri::InvalidUri),
    #[error("client initialization error: {0}")]
    ClientError(String),
}

// start_scout_stream spawns a background task that manages the streaming
// gRPC connection to carbide-api for scout stream operations.
pub(super) fn start_scout_stream(
    machine_id: MachineId,
    options: &Options,
) -> tokio::task::JoinHandle<()> {
    let options = options.clone();
    tokio::spawn(async move {
        loop {
            tracing::info!(
                api_endpoint = %options.api,
                %machine_id,
                "scout stream starting",
            );

            match run_scout_stream_loop(machine_id, &options).await {
                Ok(_) => {
                    tracing::info!(
                        api_endpoint = %options.api,
                        %machine_id,
                        "scout stream closed",
                    );
                }
                Err(e) => {
                    tracing::error!(
                        api_endpoint = %options.api,
                        %machine_id,
                        error = %e,
                        "scout stream error",
                    );
                }
            }
            emit(ScoutStreamReconnect {
                api_endpoint: options.api.clone(),
                machine_id,
            });
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    })
}

// run_scout_stream_loop establishes the scout stream connection,
// processing requests and reconnecting if the connection is closed.
async fn run_scout_stream_loop(
    machine_id: MachineId,
    options: &Options,
) -> Result<(), ScoutStreamError> {
    let mut client = client::create_forge_client(options).await.map_err(|e| {
        emit(ScoutStreamConnection {
            outcome: Outcome::Error,
        });
        ScoutStreamError::ClientError(e.to_string())
    })?;

    // Create channels for bidirectional streaming.
    let (tx, rx) = mpsc::channel::<ScoutStreamApiBoundMessage>(100);
    let request_stream = tokio_stream::wrappers::ReceiverStream::new(rx);

    // Send initial request with machine_id.
    let init_request = ScoutStreamApiBoundMessage {
        // Init doesn't take a flow_uuid.
        flow_uuid: None,
        payload: Some(scout_stream_api_bound_message::Payload::Init(
            rpc::protos::forge::ScoutStreamInitRequest {
                machine_id: machine_id.into(),
            },
        )),
    };

    tx.send(init_request).await.map_err(|e| {
        ScoutStreamError::InvalidRequest(format!("scout stream failed to send init request: {e}"))
    })?;

    // Now create the response handler.
    let mut response_stream = client
        .scout_stream(request_stream)
        .await
        .map_err(|e| {
            emit(ScoutStreamConnection {
                outcome: Outcome::Error,
            });
            ScoutStreamError::from(e)
        })?
        .into_inner();

    emit(ScoutStreamConnection {
        outcome: Outcome::Ok,
    });

    tracing::info!(
        api_endpoint = %options.api,
        %machine_id,
        "scout stream connection established",
    );

    // ...and start processing streaming updates.
    while let Some(response) = response_stream.message().await? {
        if let Some(request) = response.payload {
            // In the current model, an incoming message must have a flow_uuid associated
            // with it. If it doesn't, reject the message.
            let flow_uuid_pb = response.flow_uuid.ok_or(ScoutStreamError::InvalidRequest(
                "cannot determine flow, flow_uuid empty from API".to_string(),
            ))?;

            let flow_uuid = flow_uuid_pb.try_into().map_err(|e| {
                ScoutStreamError::ClientError(format!("failed to convert flow_uuid: {e}"))
            })?;

            // Handle the oneof message type from the ScoutStreamScoutBoundMessage,
            // generating a follow-up ScoutStreamApiBoundMessage "response".
            let payload = handle_scout_stream_api_bound_message(flow_uuid, machine_id, request);

            // And then send the response back to carbide-api.
            if let Err(e) = tx.send(payload).await {
                emit(ScoutStreamResponseDropped {
                    api_endpoint: options.api.clone(),
                    machine_id,
                    error: e.to_string(),
                });
                break;
            }
        }
    }

    Err(ScoutStreamError::ConnectionLost)
}

// handle_scout_stream_api_bound_message routes incoming oneof-based requests
// to the appropriate handler.
fn handle_scout_stream_api_bound_message(
    flow_uuid: uuid::Uuid,
    machine_id: MachineId,
    request: scout_stream_scout_bound_message::Payload,
) -> ScoutStreamApiBoundMessage {
    tracing::info!(
        %flow_uuid,
        "[scout_stream] processing incoming request",
    );
    match request {
        scout_stream_scout_bound_message::Payload::ScoutStreamAgentPingRequest(req) => {
            let response = handle_ping(machine_id, req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::ScoutStreamAgentPingResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceProfileSyncRequest(req) => {
            let response = mlx_device::handle_profile_sync(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceProfileSyncResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceProfileCompareRequest(req) => {
            let response = mlx_device::handle_profile_compare(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceProfileCompareResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceLockdownLockRequest(req) => {
            let response = mlx_device::handle_lockdown_lock(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceLockdownUnlockRequest(req) => {
            let response = mlx_device::handle_lockdown_unlock(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceLockdownStatusRequest(req) => {
            let response = mlx_device::handle_lockdown_status(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceInfoDeviceRequest(req) => {
            let response = mlx_device::handle_info_device(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceInfoDeviceResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceInfoReportRequest(req) => {
            let response = mlx_device::handle_info_report(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceInfoReportResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceRegistryListRequest(req) => {
            let response = mlx_device::handle_registry_list(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceRegistryListResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceRegistryShowRequest(req) => {
            let response = mlx_device::handle_registry_show(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceRegistryShowResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigQueryRequest(req) => {
            let response = mlx_device::handle_config_query(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceConfigQueryResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigSetRequest(req) => {
            let response = mlx_device::handle_config_set(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceConfigSetResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigSyncRequest(req) => {
            let response = mlx_device::handle_config_sync(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceConfigSyncResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigCompareRequest(req) => {
            let response = mlx_device::handle_config_compare(req);
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceConfigCompareResponse(response),
            )
        }
    }
}

// handle_ping handles a scout stream agent ping
fn handle_ping(
    machine_id: MachineId,
    _request: rpc::forge::ScoutStreamAgentPingRequest,
) -> rpc::forge::ScoutStreamAgentPingResponse {
    tracing::info!("[scout_stream::ping] ping requested",);

    rpc::forge::ScoutStreamAgentPingResponse {
        reply: Some(rpc::forge::scout_stream_agent_ping_response::Reply::Pong(
            format!("pong from {machine_id}"),
        )),
    }
}
