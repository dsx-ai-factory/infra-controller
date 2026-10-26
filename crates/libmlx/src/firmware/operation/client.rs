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

//! Bounded, typed calls to an independently owned per-card firmware supervisor.

use std::path::Path;
use std::time::Duration;

use carbide_libmlx_model::firmware::operation::{FirmwareOperation, ObserverExitBehavior};
use hyper_util::rt::TokioIo;
use rpc::protos::firmware_supervisor::firmware_supervisor_client::FirmwareSupervisorClient as WireClient;
use rpc::protos::firmware_supervisor::{
    CancelFirmwareOperationRequest, GetFirmwareOperationRequest, GetSupervisorStatusRequest,
    GetSupervisorStatusResponse, ObserverExitBehavior as ObserverExitBehaviorPb,
    SubmitFirmwareOperationRequest, SupervisorIdentity,
};
use rpc::protos::mlx_device::FirmwareOperation as FirmwareOperationPb;
use thiserror::Error;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use crate::firmware::config::FirmwareFlasherProfile;

pub(super) const PROTOCOL_VERSION: u32 = 1;
pub(super) const MESSAGE_LIMIT: usize = 128 * 1024;
// Local admission and receipt reads are short operations. This cumulative bound
// includes channel readiness and reconnection, not the firmware job's lifetime.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// The protocol, PCI slot, execution target, and boot served by one supervisor socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupervisorStatus {
    /// Supported private protocol version, exactly 1.
    pub protocol_version: u32,
    /// Normalized PCI slot `dddd:bb:ss`, not proof of physical-card
    /// identity or an independently isolated reset domain.
    pub device_identity: String,
    /// Nonempty Linux boot ID supplied by the supervisor.
    pub boot_id: String,
    /// Exact normalized full PCI BDF `dddd:bb:ss.f` used for tool execution.
    /// Sharing a slot does not permit silently targeting another function.
    pub execution_device: String,
}

/// An input, connection, or receipt failure; no error resolves an earlier submission.
#[derive(Debug, Error)]
pub enum FirmwareSupervisorClientError {
    /// Caller-local paths could not be resolved; no submission RPC was sent.
    #[error("invalid firmware supervisor request: {0}")]
    InvalidRequest(String),
    /// The Unix-socket connection or HTTP/2 handshake failed.
    #[error("failed to connect to firmware supervisor: {0}")]
    Transport(#[from] tonic::transport::Error),
    /// The supervisor returned a gRPC failure, retaining its code and details.
    #[error("firmware supervisor request failed: {0}")]
    Rpc(#[from] tonic::Status),
    /// Connection, channel readiness, or a response exceeded five seconds overall.
    #[error("firmware supervisor request exceeded its five-second deadline")]
    Deadline,
    /// A successful response omitted or contradicted required receipt data.
    #[error("invalid firmware supervisor response: {0}")]
    InvalidResponse(String),
    /// The supervisor reported a private protocol version this client cannot use.
    #[error("unsupported firmware supervisor protocol version {0}")]
    UnsupportedProtocol(u32),
}

/// A typed client for one supervisor, independent of that supervisor's lifetime.
///
/// Connecting and each RPC have a five-second cumulative deadline, including
/// connection establishment or channel readiness. Requests and responses are
/// limited to 128 KiB. Calls do not launch a supervisor or retry an RPC. Dropping
/// this client or a call never requests cancellation of accepted firmware work.
/// Submit and cancel retain the initial protocol, slot, full BDF, and boot
/// identity so reconnecting cannot silently redirect a mutation to another identity.
#[derive(Clone, Debug)]
pub struct FirmwareSupervisorClient {
    client: WireClient<Channel>,
    identity: SupervisorStatus,
}

impl FirmwareSupervisorClient {
    /// Connect to `socket` and validate its protocol and nonempty identities.
    ///
    /// The five-second deadline covers connection plus the status handshake.
    /// The caller must compare [`Self::status`] with its expected PCI-device slot,
    /// full BDF, and boot before submitting work; a socket path is not device identity.
    pub async fn connect(socket: &Path) -> Result<Self, FirmwareSupervisorClientError> {
        let socket = socket.to_path_buf();
        tokio::time::timeout(RPC_TIMEOUT, async {
            // Tonic requires a URI even though the connector only opens a UDS.
            let channel = Endpoint::from_static("http://[::]:50051")
                .connect_with_connector(service_fn(move |_: Uri| {
                    let socket = socket.clone();
                    async move {
                        let stream = UnixStream::connect(socket).await?;
                        Ok::<_, std::io::Error>(TokioIo::new(stream))
                    }
                }))
                .await?;
            let mut client = WireClient::new(channel)
                .max_decoding_message_size(MESSAGE_LIMIT)
                .max_encoding_message_size(MESSAGE_LIMIT);
            let identity = SupervisorStatus::try_from(
                client
                    .get_supervisor_status(GetSupervisorStatusRequest {})
                    .await?
                    .into_inner(),
            )?;
            Ok(Self { client, identity })
        })
        .await
        .map_err(|_| FirmwareSupervisorClientError::Deadline)?
    }

    /// Submit immutable work or retrieve its existing receipt within five seconds.
    ///
    /// `operation_id` must contain 1-256 printable nonspace ASCII bytes; other
    /// values return a gRPC `InvalidArgument` error. A missing `profile` requests
    /// a durable no-work completion with `flashed=false`, still subject to
    /// predecessor and host mutation-claim checks. The server rejects conflicting
    /// reuse of an ID.
    /// A lost response leaves acceptance uncertain;
    /// resolve it with the same ID. `observer_exit_behavior` applies to the
    /// submitting process's death, never to this RPC's connection or deadline.
    /// Local image, configuration, and SSH-key paths are resolved against the
    /// submitting process's current directory before request fingerprinting.
    /// Relative remote SSH paths keep their remote-account meaning.
    /// Omitted SSH usernames use the submitter's `USER`, then `LOGNAME`, then
    /// `root`; omitted SSH credentials use `$HOME/.ssh/id_rsa`, with `/root`
    /// when `HOME` is unavailable. Captured usernames cannot contain `@` or `:`.
    /// Explicit SSH-agent authentication
    /// uses the supervisor's `SSH_AUTH_SOCK`; host verification uses its
    /// default known-hosts file. Those environments are not forwarded per request.
    pub async fn submit(
        &mut self,
        operation_id: &str,
        mut profile: Option<FirmwareFlasherProfile>,
        observer_exit_behavior: ObserverExitBehavior,
    ) -> Result<FirmwareOperation, FirmwareSupervisorClientError> {
        if let Some(profile) = &mut profile {
            // Absolute inputs and remote URLs remain usable after the caller's
            // working directory is removed. With no directory available, any
            // relative local field remains relative and fails validation below.
            let directory = std::env::current_dir().unwrap_or_default();
            profile
                .flash_spec
                .normalize_submission_paths(&directory)
                .map_err(|error| {
                    FirmwareSupervisorClientError::InvalidRequest(error.to_string())
                })?;
        }
        let request = SubmitFirmwareOperationRequest {
            operation_id: operation_id.to_string(),
            profile: profile.map(Into::into),
            observer_exit_behavior: ObserverExitBehaviorPb::from(observer_exit_behavior).into(),
            expected_identity: Some(self.identity.clone().into()),
        };
        tokio::time::timeout(RPC_TIMEOUT, async {
            let response = self
                .client
                .submit_firmware_operation(request)
                .await?
                .into_inner();
            decode_operation(response.operation, operation_id)
        })
        .await
        .map_err(|_| FirmwareSupervisorClientError::Deadline)?
    }

    /// Read an operation receipt within five seconds without resuming execution.
    ///
    /// `operation_id` must contain 1-256 printable nonspace ASCII bytes; other
    /// values return a gRPC `InvalidArgument` error. A valid unknown ID returns
    /// a gRPC `NotFound` error. Reading a receipt does not resume or replay
    /// execution, including an interrupted operation.
    pub async fn get(
        &mut self,
        operation_id: &str,
    ) -> Result<FirmwareOperation, FirmwareSupervisorClientError> {
        tokio::time::timeout(RPC_TIMEOUT, async {
            let response = self
                .client
                .get_firmware_operation(GetFirmwareOperationRequest {
                    operation_id: operation_id.to_string(),
                })
                .await?
                .into_inner();
            decode_operation(response.operation, operation_id)
        })
        .await
        .map_err(|_| FirmwareSupervisorClientError::Deadline)?
    }

    /// Request a safe-boundary stop for an existing ID within five seconds.
    ///
    /// `operation_id` must contain 1-256 printable nonspace ASCII bytes; other
    /// values return a gRPC `InvalidArgument` error. A valid unknown ID returns
    /// a gRPC `NotFound` error.
    /// The returned receipt can still be `CancelRequested`; this does not prove
    /// that a running hardware mutation has stopped or undo completed work.
    pub async fn cancel(
        &mut self,
        operation_id: &str,
    ) -> Result<FirmwareOperation, FirmwareSupervisorClientError> {
        tokio::time::timeout(RPC_TIMEOUT, async {
            let response = self
                .client
                .cancel_firmware_operation(CancelFirmwareOperationRequest {
                    operation_id: operation_id.to_string(),
                    expected_identity: Some(self.identity.clone().into()),
                })
                .await?
                .into_inner();
            decode_operation(response.operation, operation_id)
        })
        .await
        .map_err(|_| FirmwareSupervisorClientError::Deadline)?
    }

    /// Read validated protocol, slot, execution target, and boot identities within five seconds.
    pub async fn status(&mut self) -> Result<SupervisorStatus, FirmwareSupervisorClientError> {
        tokio::time::timeout(RPC_TIMEOUT, async {
            self.client
                .get_supervisor_status(GetSupervisorStatusRequest {})
                .await?
                .into_inner()
                .try_into()
        })
        .await
        .map_err(|_| FirmwareSupervisorClientError::Deadline)?
    }
}

impl From<SupervisorStatus> for SupervisorIdentity {
    fn from(status: SupervisorStatus) -> Self {
        Self {
            protocol_version: status.protocol_version,
            device_identity: status.device_identity,
            boot_id: status.boot_id,
            execution_device: status.execution_device,
        }
    }
}

impl TryFrom<GetSupervisorStatusResponse> for SupervisorStatus {
    type Error = FirmwareSupervisorClientError;

    fn try_from(response: GetSupervisorStatusResponse) -> Result<Self, Self::Error> {
        if response.protocol_version != PROTOCOL_VERSION {
            return Err(FirmwareSupervisorClientError::UnsupportedProtocol(
                response.protocol_version,
            ));
        }
        if response.device_identity.is_empty()
            || response.boot_id.is_empty()
            || response.execution_device.is_empty()
        {
            return Err(FirmwareSupervisorClientError::InvalidResponse(
                "supervisor slot, execution device, and boot identities must be nonempty"
                    .to_string(),
            ));
        }
        Ok(Self {
            protocol_version: response.protocol_version,
            device_identity: response.device_identity,
            boot_id: response.boot_id,
            execution_device: response.execution_device,
        })
    }
}

/// A successful transport response must still identify the requested operation;
/// another receipt cannot establish this request's progress or completion.
fn decode_operation(
    operation: Option<FirmwareOperationPb>,
    expected_id: &str,
) -> Result<FirmwareOperation, FirmwareSupervisorClientError> {
    let operation: FirmwareOperation = operation
        .ok_or_else(|| {
            FirmwareSupervisorClientError::InvalidResponse("missing operation receipt".to_string())
        })?
        .try_into()
        .map_err(FirmwareSupervisorClientError::InvalidResponse)?;
    if operation.operation_id != expected_id {
        return Err(FirmwareSupervisorClientError::InvalidResponse(
            "operation receipt does not match the requested ID".to_string(),
        ));
    }
    Ok(operation)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use rpc::protos::mlx_device::{FirmwareFlashReport, FirmwareOperationState};
    use tokio::net::UnixListener;

    use super::*;

    /// An accepted socket alone is not readiness: a silent HTTP/2 peer must
    /// consume the same bounded deadline as connection establishment.
    #[tokio::test]
    async fn connection_deadline_includes_an_unresponsive_socket_peer() {
        let directory = tempfile::tempdir().expect("create socket directory");
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket).expect("bind fixture socket");
        let connection = FirmwareSupervisorClient::connect(&socket);
        tokio::pin!(connection);
        // Keep the peer open without speaking HTTP/2, so accepting a socket
        // cannot accidentally satisfy the cumulative connection deadline.
        let (_peer, _) = tokio::select! {
            accepted = listener.accept() => accepted.expect("accept client socket"),
            result = &mut connection => panic!("connection ended before accept: {result:?}"),
        };
        let result = tokio::time::timeout(RPC_TIMEOUT + Duration::from_secs(1), connection)
            .await
            .expect("client must bound its own handshake");
        assert!(matches!(
            result,
            Err(FirmwareSupervisorClientError::Deadline)
        ));
    }

    /// A supported handshake must identify the slot, boot, and exact execution function.
    #[test]
    fn handshake_requires_and_preserves_the_complete_identity() {
        let identity = SupervisorIdentity {
            protocol_version: PROTOCOL_VERSION,
            device_identity: "0000:03:00".to_string(),
            boot_id: "boot".to_string(),
            execution_device: "0000:03:00.1".to_string(),
        };
        check_values(
            [
                Check {
                    scenario: "the full function is retained in mutation identity",
                    input: (PROTOCOL_VERSION, "0000:03:00", "boot", "0000:03:00.1"),
                    expect: Some(identity),
                },
                Check {
                    scenario: "an unsupported protocol cannot authorize execution",
                    input: (PROTOCOL_VERSION + 1, "0000:03:00", "boot", "0000:03:00.1"),
                    expect: None,
                },
                Check {
                    scenario: "a missing slot cannot authorize execution",
                    input: (PROTOCOL_VERSION, "", "boot", "0000:03:00.1"),
                    expect: None,
                },
                Check {
                    scenario: "a missing boot cannot authorize execution",
                    input: (PROTOCOL_VERSION, "0000:03:00", "", "0000:03:00.1"),
                    expect: None,
                },
                Check {
                    scenario: "a slot-only supervisor cannot authorize execution",
                    input: (PROTOCOL_VERSION, "0000:03:00", "boot", ""),
                    expect: None,
                },
            ],
            |(protocol_version, device_identity, boot_id, execution_device)| {
                SupervisorStatus::try_from(GetSupervisorStatusResponse {
                    protocol_version,
                    device_identity: device_identity.to_string(),
                    boot_id: boot_id.to_string(),
                    execution_device: execution_device.to_string(),
                })
                .map(SupervisorIdentity::from)
                .ok()
            },
        );
    }

    /// A missing or unrelated receipt must not turn transport success into
    /// evidence that the caller's operation was accepted or finished.
    #[test]
    fn receipts_must_exist_and_match_the_requested_operation() {
        let receipt = FirmwareOperationPb {
            operation_id: "expected".to_string(),
            state: FirmwareOperationState::Accepted.into(),
            phase: None,
            report: Some(FirmwareFlashReport::default()),
            diagnostic: None,
        };
        check_values(
            [
                Check {
                    scenario: "the requested receipt is accepted",
                    input: (Some(receipt.clone()), "expected"),
                    expect: true,
                },
                Check {
                    scenario: "an empty successful response is rejected",
                    input: (None, "expected"),
                    expect: false,
                },
                Check {
                    scenario: "another operation's receipt is rejected",
                    input: (Some(receipt), "different"),
                    expect: false,
                },
            ],
            |(receipt, expected_id)| decode_operation(receipt, expected_id).is_ok(),
        );
    }
}
