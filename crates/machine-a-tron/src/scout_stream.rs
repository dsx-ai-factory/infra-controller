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

use carbide_uuid::machine::MachineId;
use rpc::forge::{
    ScoutStreamAgentPingResponse, ScoutStreamApiBoundMessage, ScoutStreamInitRequest,
    scout_stream_agent_ping_response, scout_stream_api_bound_message,
    scout_stream_scout_bound_message,
};
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use rpc::protos::forge_api_client::ForgeApiClient;
use rpc::protos::mlx_device::{
    MlxDeviceConfigCompareResponse, MlxDeviceConfigQueryResponse, MlxDeviceConfigSetResponse,
    MlxDeviceConfigSyncResponse, MlxDeviceInfoDeviceResponse, MlxDeviceInfoReportResponse,
    MlxDeviceLockdownResponse, MlxDeviceProfileCompareResponse, MlxDeviceProfileSyncResponse,
    MlxDeviceRegistryListResponse, MlxDeviceRegistryShowResponse, MlxDeviceStreamError,
    mlx_device_config_compare_response, mlx_device_config_query_response,
    mlx_device_config_set_response, mlx_device_config_sync_response,
    mlx_device_info_device_response, mlx_device_info_report_response, mlx_device_lockdown_response,
    mlx_device_profile_compare_response, mlx_device_profile_sync_response,
    mlx_device_registry_list_response, mlx_device_registry_show_response,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const STREAM_CHANNEL_CAPACITY: usize = 8;
const UNSUPPORTED_MESSAGE: &str = "machine-a-tron does not simulate Mellanox ScoutStream requests";

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("gRPC error: {0}")]
    Grpc(#[from] tonic::Status),
    #[error("invalid ScoutStream message: {0}")]
    InvalidMessage(String),
}

/// Owns one simulated Scout process's stream task.
///
/// Dropping the handle cancels the stream and aborts any transport operation that
/// has not yet observed cancellation.
#[derive(Debug)]
pub(super) struct Handle {
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

impl Handle {
    pub(super) fn start(
        machine_id: MachineId,
        api_endpoint: String,
        client_config: ForgeClientConfig,
        reconnect_interval: Duration,
    ) -> Self {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            run(
                machine_id,
                api_endpoint,
                client_config,
                reconnect_interval,
                task_cancellation,
            )
            .await;
        });
        Self { cancellation, task }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
    }
}

async fn run(
    machine_id: MachineId,
    api_endpoint: String,
    client_config: ForgeClientConfig,
    reconnect_interval: Duration,
    cancellation: CancellationToken,
) {
    loop {
        tracing::info!(%machine_id, %api_endpoint, "ScoutStream connection attempt started");
        let result = run_connection(machine_id, &api_endpoint, &client_config, &cancellation).await;

        if cancellation.is_cancelled() {
            tracing::info!(%machine_id, "ScoutStream actor stopped");
            return;
        }

        match result {
            Ok(()) => tracing::info!(%machine_id, "ScoutStream connection closed"),
            Err(error) => {
                tracing::warn!(%machine_id, error = %error, "ScoutStream connection failed")
            }
        }
        tracing::info!(
            %machine_id,
            retry_delay_seconds = reconnect_interval.as_secs_f64(),
            "ScoutStream reconnect scheduled",
        );
        tokio::select! {
            () = cancellation.cancelled() => return,
            () = tokio::time::sleep(reconnect_interval) => {}
        }
    }
}

async fn run_connection(
    machine_id: MachineId,
    api_endpoint: &str,
    client_config: &ForgeClientConfig,
    cancellation: &CancellationToken,
) -> Result<(), Error> {
    // A fresh wrapper and underlying HTTP/2 channel per connection model the
    // independent process and transport lifecycle of each simulated Scout.
    let api_config = ApiConfig::new(api_endpoint, client_config);
    let mut client = ForgeApiClient::new(&api_config).connection().await?;
    let (tx, rx) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
    tx.send(ScoutStreamApiBoundMessage {
        flow_uuid: None,
        payload: Some(scout_stream_api_bound_message::Payload::Init(
            ScoutStreamInitRequest {
                machine_id: machine_id.into(),
            },
        )),
    })
    .await
    .map_err(|error| Error::InvalidMessage(format!("failed to queue init request: {error}")))?;

    let request_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut responses = tokio::select! {
        () = cancellation.cancelled() => return Ok(()),
        response = client.scout_stream(request_stream) => response?.into_inner(),
    };
    tracing::info!(%machine_id, "ScoutStream connection established");

    loop {
        let response = tokio::select! {
            () = cancellation.cancelled() => return Ok(()),
            response = responses.message() => response?,
        };
        let Some(response) = response else {
            return Ok(());
        };
        let flow_uuid = response
            .flow_uuid
            .ok_or_else(|| Error::InvalidMessage("API request omitted flow_uuid".to_string()))?
            .try_into()
            .map_err(|error| Error::InvalidMessage(format!("invalid flow_uuid: {error}")))?;
        let payload = response
            .payload
            .ok_or_else(|| Error::InvalidMessage("API request omitted payload".to_string()))?;
        let reply = reply(flow_uuid, machine_id, payload);
        tokio::select! {
            () = cancellation.cancelled() => return Ok(()),
            result = tx.send(reply) => result.map_err(|error| {
                Error::InvalidMessage(format!("failed to queue response: {error}"))
            })?,
        }
    }
}

fn reply(
    flow_uuid: uuid::Uuid,
    machine_id: MachineId,
    payload: scout_stream_scout_bound_message::Payload,
) -> ScoutStreamApiBoundMessage {
    use scout_stream_api_bound_message::Payload as ApiPayload;
    use scout_stream_scout_bound_message::Payload as ScoutPayload;

    let payload = match payload {
        ScoutPayload::ScoutStreamAgentPingRequest(_) => {
            ApiPayload::ScoutStreamAgentPingResponse(ScoutStreamAgentPingResponse {
                reply: Some(scout_stream_agent_ping_response::Reply::Pong(format!(
                    "pong from {machine_id}"
                ))),
            })
        }
        ScoutPayload::MlxDeviceProfileSyncRequest(_) => {
            ApiPayload::MlxDeviceProfileSyncResponse(MlxDeviceProfileSyncResponse {
                reply: Some(mlx_device_profile_sync_response::Reply::Error(mlx_error())),
            })
        }
        ScoutPayload::MlxDeviceProfileCompareRequest(_) => {
            ApiPayload::MlxDeviceProfileCompareResponse(MlxDeviceProfileCompareResponse {
                reply: Some(mlx_device_profile_compare_response::Reply::Error(
                    mlx_error(),
                )),
            })
        }
        ScoutPayload::MlxDeviceLockdownLockRequest(_)
        | ScoutPayload::MlxDeviceLockdownUnlockRequest(_)
        | ScoutPayload::MlxDeviceLockdownStatusRequest(_) => {
            ApiPayload::MlxDeviceLockdownResponse(MlxDeviceLockdownResponse {
                reply: Some(mlx_device_lockdown_response::Reply::Error(mlx_error())),
            })
        }
        ScoutPayload::MlxDeviceInfoDeviceRequest(_) => {
            ApiPayload::MlxDeviceInfoDeviceResponse(MlxDeviceInfoDeviceResponse {
                reply: Some(mlx_device_info_device_response::Reply::Error(mlx_error())),
            })
        }
        ScoutPayload::MlxDeviceInfoReportRequest(_) => {
            ApiPayload::MlxDeviceInfoReportResponse(MlxDeviceInfoReportResponse {
                reply: Some(mlx_device_info_report_response::Reply::Error(mlx_error())),
            })
        }
        ScoutPayload::MlxDeviceRegistryListRequest(_) => {
            ApiPayload::MlxDeviceRegistryListResponse(MlxDeviceRegistryListResponse {
                reply: Some(mlx_device_registry_list_response::Reply::Error(mlx_error())),
            })
        }
        ScoutPayload::MlxDeviceRegistryShowRequest(_) => {
            ApiPayload::MlxDeviceRegistryShowResponse(MlxDeviceRegistryShowResponse {
                reply: Some(mlx_device_registry_show_response::Reply::Error(mlx_error())),
            })
        }
        ScoutPayload::MlxDeviceConfigQueryRequest(_) => {
            ApiPayload::MlxDeviceConfigQueryResponse(MlxDeviceConfigQueryResponse {
                reply: Some(mlx_device_config_query_response::Reply::Error(mlx_error())),
            })
        }
        ScoutPayload::MlxDeviceConfigSetRequest(_) => {
            ApiPayload::MlxDeviceConfigSetResponse(MlxDeviceConfigSetResponse {
                reply: Some(mlx_device_config_set_response::Reply::Error(mlx_error())),
            })
        }
        ScoutPayload::MlxDeviceConfigSyncRequest(_) => {
            ApiPayload::MlxDeviceConfigSyncResponse(MlxDeviceConfigSyncResponse {
                reply: Some(mlx_device_config_sync_response::Reply::Error(mlx_error())),
            })
        }
        ScoutPayload::MlxDeviceConfigCompareRequest(_) => {
            ApiPayload::MlxDeviceConfigCompareResponse(MlxDeviceConfigCompareResponse {
                reply: Some(mlx_device_config_compare_response::Reply::Error(mlx_error())),
            })
        }
    };

    ScoutStreamApiBoundMessage::from_flow(flow_uuid, payload)
}

fn mlx_error() -> MlxDeviceStreamError {
    MlxDeviceStreamError {
        status: 0,
        message: UNSUPPORTED_MESSAGE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use carbide_uuid::machine::{MachineIdSource, MachineType};

    use super::*;

    fn machine_id() -> MachineId {
        MachineId::new(MachineIdSource::Tpm, [1; 32], MachineType::Host)
    }

    #[test]
    fn ping_preserves_flow_and_identifies_machine() {
        let flow_uuid = uuid::Uuid::new_v4();
        let machine_id = machine_id();
        let response = reply(
            flow_uuid,
            machine_id,
            scout_stream_scout_bound_message::Payload::ScoutStreamAgentPingRequest(
                Default::default(),
            ),
        );

        assert_eq!(response.flow_uuid, Some(flow_uuid.into()));
        assert!(matches!(
            response.payload,
            Some(scout_stream_api_bound_message::Payload::ScoutStreamAgentPingResponse(
                ScoutStreamAgentPingResponse {
                    reply: Some(scout_stream_agent_ping_response::Reply::Pong(pong)),
                }
            )) if pong == format!("pong from {machine_id}")
        ));
    }

    #[test]
    fn unsupported_request_returns_structured_error() {
        let flow_uuid = uuid::Uuid::new_v4();
        let machine_id = machine_id();
        let response = reply(
            flow_uuid,
            machine_id,
            scout_stream_scout_bound_message::Payload::MlxDeviceProfileSyncRequest(
                Default::default(),
            ),
        );

        assert!(matches!(
            response.payload,
            Some(scout_stream_api_bound_message::Payload::MlxDeviceProfileSyncResponse(
                MlxDeviceProfileSyncResponse {
                    reply: Some(mlx_device_profile_sync_response::Reply::Error(error)),
                }
            )) if error.message == UNSUPPORTED_MESSAGE
        ));
    }
}
