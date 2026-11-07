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
use rpc::protos::mlx_device as mlx_device_pb;
use tokio::sync::mpsc;

use crate::cfg::Options;
use crate::metrics::{ScoutStreamConnection, ScoutStreamReconnect, ScoutStreamResponseDropped};
use crate::mlx_access::{DirectMlxAccess, DirectMlxAccessError};
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
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_profile_sync(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceProfileSyncResponse {
                reply: Some(mlx_device_pb::mlx_device_profile_sync_response::Reply::Error(error)),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceProfileSyncResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceProfileCompareRequest(req) => {
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_profile_compare(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceProfileCompareResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_profile_compare_response::Reply::Error(error),
                ),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceProfileCompareResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceLockdownLockRequest(req) => {
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_lockdown_lock(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(
                    error,
                )),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceLockdownUnlockRequest(req) => {
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_lockdown_unlock(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(
                    error,
                )),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceLockdownStatusRequest(req) => {
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_lockdown_status(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(
                    error,
                )),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceInfoDeviceRequest(req) => {
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_info_device(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceInfoDeviceResponse {
                reply: Some(mlx_device_pb::mlx_device_info_device_response::Reply::Error(error)),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceInfoDeviceResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceInfoReportRequest(req) => {
            let response = with_direct_mlx_access("inventory", |access| {
                mlx_device::handle_info_report(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceInfoReportResponse {
                reply: Some(mlx_device_pb::mlx_device_info_report_response::Reply::Error(error)),
            });
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
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_config_query(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceConfigQueryResponse {
                reply: Some(mlx_device_pb::mlx_device_config_query_response::Reply::Error(error)),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceConfigQueryResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigSetRequest(req) => {
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_config_set(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceConfigSetResponse {
                reply: Some(mlx_device_pb::mlx_device_config_set_response::Reply::Error(
                    error,
                )),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceConfigSetResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigSyncRequest(req) => {
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_config_sync(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceConfigSyncResponse {
                reply: Some(mlx_device_pb::mlx_device_config_sync_response::Reply::Error(error)),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceConfigSyncResponse(response),
            )
        }
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigCompareRequest(req) => {
            let device = req.device_id.clone();
            let response = with_direct_mlx_access(&device, |access| {
                mlx_device::handle_config_compare(req, access)
            })
            .unwrap_or_else(|error| mlx_device_pb::MlxDeviceConfigCompareResponse {
                reply: Some(mlx_device_pb::mlx_device_config_compare_response::Reply::Error(error)),
            });
            ScoutStreamApiBoundMessage::from_flow(
                flow_uuid,
                scout_stream_api_bound_message::Payload::MlxDeviceConfigCompareResponse(response),
            )
        }
    }
}

// These handlers are synchronous: cancellation cannot drop their claim midway
// through a command. Finish only after the handler has inspected typed errors.
fn with_direct_mlx_access<T>(
    device: &str,
    handle: impl FnOnce(&mut DirectMlxAccess) -> T,
) -> Result<T, mlx_device_pb::MlxDeviceStreamError> {
    let run = || {
        let config = libmlx::firmware::operation::SupervisorConfig::from_env()
            .map_err(|error| error.to_string())?;
        let mut access =
            DirectMlxAccess::acquire(&config, device).map_err(|error| error.to_string())?;
        let response = handle(&mut access);
        match access.finish() {
            Ok(()) => {}
            Err(DirectMlxAccessError::Release(error)) => {
                // The command has stopped. Losing its response could make
                // the caller repeat a mutation that already succeeded.
                tracing::warn!(
                    device_id = device,
                    error = %error,
                    "direct MLX command completed but claim release failed",
                );
            }
            Err(error) => return Err(error.to_string()),
        }
        Ok(response)
    };
    run().map_err(|message| mlx_device_pb::MlxDeviceStreamError {
        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
        message,
    })
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Stdio;

    use carbide_instrument::testing::capture_logs;
    use libmlx::firmware::operation::{DirectMlxClaim, SupervisorConfig};
    use tokio::process::Command;
    use tokio::time::timeout;

    use super::*;

    const CHILD_DIRECTORY: &str = "SCOUT_STREAM_CLAIM_TEST_DIRECTORY";
    const CHILD_TIMEOUT: Duration = Duration::from_secs(10);

    /// Give each exact test its own environment and private claim directory.
    /// The parent checks the child result; only the child runs the test body.
    async fn isolated_stream_test_directory(test_name: &str) -> Option<PathBuf> {
        if let Some(directory) = std::env::var_os(CHILD_DIRECTORY) {
            return Some(directory.into());
        }

        // Environment overrides belong only to the child. It also creates
        // its executable fixture without concurrent tests forking during
        // the write and retaining a descriptor that can cause ETXTBSY.
        let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
            Some(root) => tempfile::tempdir_in(root),
            None => tempfile::tempdir(),
        }
        .expect("create private stream fixture");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(directory.path().join("tools")).unwrap();
        let log_path = directory.path().join("child.log");
        let log = fs::File::create(&log_path).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD_DIRECTORY, directory.path())
            .env(
                "MLX_FIRMWARE_STATE_DIRECTORY",
                directory.path().join("state"),
            )
            .env("MLX_FIRMWARE_SUPERVISOR_LAUNCH", "standalone")
            .env("PATH", directory.path().join("tools"))
            .env("TMPDIR", directory.path())
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .expect("start isolated stream test");
        let result = timeout(CHILD_TIMEOUT, child.wait()).await;
        let status = match result {
            Ok(Ok(status)) => status,
            error => {
                // Reap even if the process exits between timeout and kill.
                let stop_result = child.start_kill();
                let reap_result = timeout(CHILD_TIMEOUT, child.wait()).await;
                panic!(
                    "isolated stream test did not finish: {error:?}; stop: {stop_result:?}; reap: {reap_result:?}\n{}",
                    fs::read_to_string(&log_path).unwrap()
                );
            }
        };
        let output = fs::read_to_string(log_path).unwrap();
        assert!(
            status.success() && output.contains("1 passed; 0 failed"),
            "isolated stream test failed: {status}\n{output}"
        );
        None
    }

    /// An occupied host claim must stop the actual admin stream before even
    /// probing flint, including when the requested PCI function is different.
    #[tokio::test]
    async fn occupied_claim_blocks_admin_stream_lock_before_tool_execution() {
        let Some(directory) = isolated_stream_test_directory(
            "stream::tests::occupied_claim_blocks_admin_stream_lock_before_tool_execution",
        )
        .await
        else {
            return;
        };

        // A guard regression must still select this fake instead of falling
        // through flint's absolute-path candidates. Every probe is recorded.
        let tool = directory.join("tools/flint");
        fs::write(
            &tool,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$SCOUT_STREAM_CLAIM_TEST_DIRECTORY/tool-calls"
if [ "$#" -eq 1 ] && [ "$1" = '--version' ]; then
    printf '%s\n' 'flint, fixture'
    exit 0
fi
printf '%s\n' 'unexpected fake flint command' >&2
exit 1
"#,
        )
        .unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();

        let config = SupervisorConfig::from_env().unwrap();
        let claim = DirectMlxClaim::acquire(&config, "ffff:fe:1e.0").unwrap();
        let marker = config.state_directory.join("mutation.json");
        let original_claim = fs::read(&marker).unwrap();
        let flow_uuid = uuid::Uuid::new_v4();
        let machine_id = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
            .parse()
            .unwrap();
        let request = scout_stream_scout_bound_message::Payload::MlxDeviceLockdownLockRequest(
            mlx_device_pb::MlxDeviceLockdownLockRequest {
                device_id: "ffff:fe:1f.0".into(),
                key: "0123456789abcdef".into(),
            },
        );
        let response =
            handle_scout_stream_api_bound_message(flow_uuid, machine_id, request.clone());

        assert_eq!(response.flow_uuid, Some(flow_uuid.into()));
        let Some(scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(response)) =
            response.payload
        else {
            panic!("expected the matching lockdown response");
        };
        let Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(error)) = response.reply
        else {
            panic!("occupied claim must reject the stream mutation");
        };
        assert_eq!(
            error.status,
            mlx_device_pb::MlxDeviceStreamErrorStatus::Internal as i32
        );
        assert!(
            error
                .message
                .contains("cannot acquire direct MLX hardware access")
        );
        assert!(!directory.join("tool-calls").exists());
        assert_eq!(fs::read(&marker).unwrap(), original_claim);
        claim.release().unwrap();

        // Releasing the owner must allow this same request to reach flint.
        // A settled vendor failure must then remove the new direct claim.
        let response = handle_scout_stream_api_bound_message(flow_uuid, machine_id, request);
        assert_eq!(response.flow_uuid, Some(flow_uuid.into()));
        let Some(scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(response)) =
            response.payload
        else {
            panic!("expected the matching lockdown response after release");
        };
        let Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(error)) = response.reply
        else {
            panic!("the fake vendor command must fail");
        };
        assert_eq!(
            error.status,
            mlx_device_pb::MlxDeviceStreamErrorStatus::Internal as i32
        );
        assert_eq!(
            error.message,
            "lockdown lock failed: command execution failed: hw_access disable failed: exit status: 1"
        );
        assert_eq!(
            fs::read_to_string(directory.join("tool-calls")).unwrap(),
            "--version\n-d ffff:fe:1f.0 hw_access disable 0123456789abcdef\n"
        );
        assert!(!marker.exists());
    }

    /// A failed unlink cannot replace a completed handler's success response
    /// with an error that could prompt the caller to repeat its mutation.
    #[tokio::test]
    async fn claim_release_failure_preserves_completed_stream_response() {
        let Some(directory) = isolated_stream_test_directory(
            "stream::tests::claim_release_failure_preserves_completed_stream_response",
        )
        .await
        else {
            return;
        };

        let config = SupervisorConfig::from_env().unwrap();
        let retained_state = directory.join("retained-state");
        let expected = mlx_device_pb::MlxDeviceLockdownResponse {
            reply: Some(
                mlx_device_pb::mlx_device_lockdown_response::Reply::StatusReport(
                    mlx_device_pb::StatusReport {
                        device_id: "ffff:fe:1f.0".into(),
                        status: mlx_device_pb::LockStatus::Locked.into(),
                        timestamp: "2026-10-23T00:00:00Z".into(),
                    },
                ),
            ),
        };

        let logs = capture_logs(|| {
            let response = with_direct_mlx_access("ffff:fe:1f.0", |_| {
                assert!(config.state_directory.join("mutation.json").is_file());
                // Moving the directory preserves the claim but makes unlink at
                // its original path fail, even when the test runs as root.
                fs::rename(&config.state_directory, &retained_state).unwrap();
                expected.clone()
            })
            .unwrap();
            assert_eq!(response, expected);
        });

        assert!(!config.state_directory.exists());
        assert!(retained_state.join("mutation.json").is_file());
        let warning = logs
            .iter()
            .find(|log| log.message == "direct MLX command completed but claim release failed")
            .expect("release failure is logged without replacing the response");
        assert_eq!(warning.level, tracing::Level::WARN);
        assert_eq!(warning.field("device_id"), Some("ffff:fe:1f.0"));
        assert!(
            warning
                .field("error")
                .expect("release error field")
                .contains("No such file or directory")
        );
    }
}
