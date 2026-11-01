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
    GetSupervisorStatusResponse, ObserveFirmwareOperationResponse, ObserveFirmwareUpdateRequest,
    ObserverExitBehavior as ObserverExitBehaviorPb, SubmitFirmwareOperationRequest,
    SupervisorIdentity, observe_firmware_operation_response,
};
use rpc::protos::mlx_device::FirmwareOperation as FirmwareOperationPb;
use thiserror::Error;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use crate::firmware::config::{FirmwareFlasherProfile, FirmwareUpdateContext};

pub(super) const PROTOCOL_VERSION: u32 = 4;
pub(super) const MESSAGE_LIMIT: usize = 128 * 1024;
// Local admission and receipt reads are short operations. This cumulative bound
// includes channel readiness and reconnection, not the firmware job's lifetime.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// The protocol, PCI slot, execution target, and boot served by one socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupervisorStatus {
    /// Private supervisor API revision. The client and supervisor must use the
    /// same revision; a mismatch rejects the connection before any operation.
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

/// `FirmwareOperationObservation` is a receipt lookup, never a request to execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirmwareOperationObservation {
    /// The saved receipt, including interrupted and terminal execution states.
    Receipt(FirmwareOperation),
    /// This service's readable journal has no receipt for the requested ID.
    /// Only an initial observation may establish permission for new admission;
    /// a replacement instance cannot refresh a token already frozen by a caller.
    MissingReceipt {
        /// Opaque token of the service that performed this lookup, containing
        /// 1-256 printable nonspace ASCII bytes.
        supervisor_instance_id: String,
    },
}

/// An input, connection, or receipt failure; no error resolves an earlier submission.
#[derive(Debug, Error)]
pub enum FirmwareSupervisorClientError {
    /// Local request validation or path resolution failed; no submission RPC was sent.
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
    /// The supervisor's private API revision does not match this client's revision.
    #[error("unsupported firmware supervisor protocol version {0}")]
    UnsupportedProtocol(u32),
}

/// A typed client for one supervisor, independent of that supervisor's lifetime.
///
/// Connecting and each RPC have a five-second cumulative deadline, including
/// connection establishment or channel readiness. Requests and responses are
/// limited to 128 KiB. Calls do not launch a supervisor or retry an RPC. Dropping
/// this client or a call never requests cancellation of accepted firmware work.
/// Submit, cancel, and observe retain the initial protocol, slot, full BDF, and
/// boot identity so reconnecting cannot silently redirect a checked request.
#[derive(Clone, Debug)]
pub struct FirmwareSupervisorClient {
    client: WireClient<Channel>,
    identity: SupervisorStatus,
}

impl FirmwareSupervisorClient {
    /// Connect to `socket` and require the current API revision and nonempty identities.
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
    /// A lost response leaves acceptance uncertain; read its receipt with the
    /// same ID. An unguarded retry cannot prevent repetition after runtime state loss.
    /// `observer_exit_behavior` applies to the
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
            update_context: None,
            expected_supervisor_instance_id: None,
        };
        self.submit_request(request).await
    }

    /// `submit_update` admits one digest-bound firmware workflow or returns its
    /// matching existing receipt.
    /// Submission deliveries use this method or [`Self::submit_update_on_instance`],
    /// so changed profile, artifact digests, or dispatch token conflict with
    /// accepted work. Use the guarded method for automatic retries; this method
    /// cannot prevent repetition after runtime state loss.
    /// The supervisor owns staging and checks both digests before mutation.
    ///
    /// [`FirmwareUpdateContext::validate`] checks the dispatch token, digest
    /// requirements, optional device selector, and unauthenticated HTTP(S) source
    /// policy before any RPC.
    /// The submission RPC has a five-second cumulative deadline, including
    /// channel readiness and reconnection, not the firmware operation's lifetime.
    /// A deadline or lost response leaves admission uncertain and does not cancel
    /// accepted work; resolve it using the same operation ID and unchanged inputs.
    pub async fn submit_update(
        &mut self,
        operation_id: &str,
        profile: FirmwareFlasherProfile,
        context: FirmwareUpdateContext,
        observer_exit_behavior: ObserverExitBehavior,
    ) -> Result<FirmwareOperation, FirmwareSupervisorClientError> {
        self.submit_update_with_instance(
            operation_id,
            profile,
            context,
            observer_exit_behavior,
            None,
        )
        .await
    }

    /// `submit_update_on_instance` guards [`Self::submit_update`] with the token
    /// previously obtained from [`Self::observe_update`].
    ///
    /// The token contains 1-256 printable nonspace ASCII bytes and is preserved
    /// across retries and reconnection.
    /// A replacement service can return a matching existing receipt, but cannot
    /// admit missing work. All profile and digest validation remains unchanged.
    pub async fn submit_update_on_instance(
        &mut self,
        operation_id: &str,
        profile: FirmwareFlasherProfile,
        context: FirmwareUpdateContext,
        observer_exit_behavior: ObserverExitBehavior,
        expected_supervisor_instance_id: &str,
    ) -> Result<FirmwareOperation, FirmwareSupervisorClientError> {
        self.submit_update_with_instance(
            operation_id,
            profile,
            context,
            observer_exit_behavior,
            Some(expected_supervisor_instance_id),
        )
        .await
    }

    async fn submit_update_with_instance(
        &mut self,
        operation_id: &str,
        profile: FirmwareFlasherProfile,
        context: FirmwareUpdateContext,
        observer_exit_behavior: ObserverExitBehavior,
        expected_supervisor_instance_id: Option<&str>,
    ) -> Result<FirmwareOperation, FirmwareSupervisorClientError> {
        if expected_supervisor_instance_id.is_some_and(|id| !is_valid_supervisor_instance_id(id)) {
            return Err(FirmwareSupervisorClientError::InvalidRequest(
                "expected supervisor instance ID must contain 1..256 printable nonspace ASCII bytes".into(),
            ));
        }
        context
            .validate(&profile)
            .map_err(|error| FirmwareSupervisorClientError::InvalidRequest(error.to_string()))?;
        self.submit_request(SubmitFirmwareOperationRequest {
            operation_id: operation_id.into(),
            profile: Some(profile.into()),
            observer_exit_behavior: ObserverExitBehaviorPb::from(observer_exit_behavior).into(),
            expected_identity: Some(self.identity.clone().into()),
            update_context: Some(context.into()),
            expected_supervisor_instance_id: expected_supervisor_instance_id.map(str::to_owned),
        })
        .await
    }

    async fn submit_request(
        &mut self,
        request: SubmitFirmwareOperationRequest,
    ) -> Result<FirmwareOperation, FirmwareSupervisorClientError> {
        let operation_id = request.operation_id.clone();
        tokio::time::timeout(RPC_TIMEOUT, async {
            let response = self
                .client
                .submit_firmware_operation(request)
                .await?
                .into_inner();
            decode_operation(response.operation, &operation_id)
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

    /// `observe_update` reads a receipt only for the unchanged checked request.
    /// The supervisor validates the profile, context and
    /// observer policy against its saved fingerprint; conflicting reuse returns
    /// `AlreadyExists`. Absence returns an instance token without admitting work,
    /// acquiring a mutation claim, probing hardware, or downloading artifacts.
    /// The token identifies the service answering this observation, including
    /// after a reconnect; it does not refresh a previously saved admission token.
    /// A failed or fenced journal is an error, not a missing receipt. This call
    /// never resumes interrupted work. Its five-second cumulative deadline
    /// includes channel readiness and reconnection, not the firmware job's lifetime.
    pub async fn observe_update(
        &mut self,
        operation_id: &str,
        profile: FirmwareFlasherProfile,
        context: FirmwareUpdateContext,
        observer_exit_behavior: ObserverExitBehavior,
    ) -> Result<FirmwareOperationObservation, FirmwareSupervisorClientError> {
        context
            .validate(&profile)
            .map_err(|error| FirmwareSupervisorClientError::InvalidRequest(error.to_string()))?;
        tokio::time::timeout(RPC_TIMEOUT, async {
            let response = self
                .client
                .observe_firmware_update(ObserveFirmwareUpdateRequest {
                    operation_id: operation_id.into(),
                    profile: Some(profile.into()),
                    observer_exit_behavior: ObserverExitBehaviorPb::from(observer_exit_behavior)
                        .into(),
                    expected_identity: Some(self.identity.clone().into()),
                    update_context: Some(context.into()),
                })
                .await?
                .into_inner();
            decode_observation(response, operation_id)
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

    /// Read validated protocol, slot, execution target, and boot within five seconds.
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

impl From<FirmwareUpdateContext> for rpc::protos::firmware_supervisor::FirmwareUpdateContext {
    fn from(context: FirmwareUpdateContext) -> Self {
        Self {
            phase_id: context.phase_id,
            expected_digests: Some(context.expected_digests.into()),
            expected_device: context.expected_device.map(Into::into),
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

fn is_valid_supervisor_instance_id(instance_id: &str) -> bool {
    !instance_id.is_empty()
        && instance_id.len() <= 256
        && instance_id.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Observation must identify either this operation or a usable admission token;
/// a successful transport alone cannot establish that the journal was readable.
fn decode_observation(
    response: ObserveFirmwareOperationResponse,
    operation_id: &str,
) -> Result<FirmwareOperationObservation, FirmwareSupervisorClientError> {
    match response.result {
        Some(observe_firmware_operation_response::Result::Receipt(receipt)) => {
            decode_operation(Some(receipt), operation_id).map(FirmwareOperationObservation::Receipt)
        }
        Some(observe_firmware_operation_response::Result::MissingReceipt(missing))
            if is_valid_supervisor_instance_id(&missing.supervisor_instance_id) =>
        {
            Ok(FirmwareOperationObservation::MissingReceipt {
                supervisor_instance_id: missing.supervisor_instance_id,
            })
        }
        _ => Err(FirmwareSupervisorClientError::InvalidResponse(
            "missing observation result or invalid supervisor instance ID".into(),
        )),
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_test_support::{Check, check_values};
    use rpc::protos::firmware_supervisor::firmware_supervisor_server::{
        FirmwareSupervisor, FirmwareSupervisorServer,
    };
    use rpc::protos::firmware_supervisor::{
        CancelFirmwareOperationResponse, GetFirmwareOperationResponse,
        ObserveFirmwareOperationResponse, SubmitFirmwareOperationResponse,
    };
    use rpc::protos::mlx_device::{
        FirmwareFlashReport, FirmwareOperationMissingReceipt, FirmwareOperationPhase,
        FirmwareOperationState,
    };
    use tokio::net::UnixListener;
    use tokio::sync::watch;
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::{Request, Response, Status};

    use super::*;
    use crate::firmware::config::FirmwareArtifactDigests;

    /// Expose retained work and check cancellation identity without running
    /// firmware tools. Count operation calls to prove local validation order.
    struct SupervisorFixture {
        identity: SupervisorIdentity,
        receipt: FirmwareOperationPb,
        operation_calls: Arc<AtomicUsize>,
    }

    #[tonic::async_trait]
    impl FirmwareSupervisor for SupervisorFixture {
        async fn submit_firmware_operation(
            &self,
            _request: Request<SubmitFirmwareOperationRequest>,
        ) -> Result<Response<SubmitFirmwareOperationResponse>, Status> {
            self.operation_calls.fetch_add(1, Ordering::Relaxed);
            Err(Status::unimplemented(
                "the fixture must not receive submissions",
            ))
        }

        async fn get_firmware_operation(
            &self,
            request: Request<GetFirmwareOperationRequest>,
        ) -> Result<Response<GetFirmwareOperationResponse>, Status> {
            self.operation_calls.fetch_add(1, Ordering::Relaxed);
            if request.into_inner().operation_id != self.receipt.operation_id {
                return Err(Status::not_found("unknown fixture operation"));
            }
            Ok(Response::new(GetFirmwareOperationResponse {
                operation: Some(self.receipt.clone()),
            }))
        }

        async fn observe_firmware_update(
            &self,
            _request: Request<ObserveFirmwareUpdateRequest>,
        ) -> Result<Response<ObserveFirmwareOperationResponse>, Status> {
            self.operation_calls.fetch_add(1, Ordering::Relaxed);
            Err(Status::unimplemented(
                "the fixture must not receive observations",
            ))
        }

        async fn cancel_firmware_operation(
            &self,
            request: Request<CancelFirmwareOperationRequest>,
        ) -> Result<Response<CancelFirmwareOperationResponse>, Status> {
            self.operation_calls.fetch_add(1, Ordering::Relaxed);
            let request = request.into_inner();
            if request.expected_identity.as_ref() != Some(&self.identity) {
                return Err(Status::failed_precondition("cancellation identity changed"));
            }
            if request.operation_id != self.receipt.operation_id {
                return Err(Status::not_found("unknown fixture operation"));
            }
            Ok(Response::new(CancelFirmwareOperationResponse {
                operation: Some(FirmwareOperationPb {
                    state: FirmwareOperationState::CancelRequested.into(),
                    ..self.receipt.clone()
                }),
            }))
        }

        async fn get_supervisor_status(
            &self,
            _request: Request<GetSupervisorStatusRequest>,
        ) -> Result<Response<GetSupervisorStatusResponse>, Status> {
            Ok(Response::new(GetSupervisorStatusResponse {
                protocol_version: self.identity.protocol_version,
                device_identity: self.identity.device_identity.clone(),
                boot_id: self.identity.boot_id.clone(),
                execution_device: self.identity.execution_device.clone(),
            }))
        }
    }

    /// Receipt recovery retains the handshake identity, while an invalid
    /// instance token must stop guarded submission before reaching transport.
    #[tokio::test]
    async fn receipt_recovery_does_not_bypass_submission_guards() {
        let directory = tempfile::tempdir().expect("create socket directory");
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket).expect("bind fixture socket");
        let identity = SupervisorIdentity {
            protocol_version: PROTOCOL_VERSION,
            device_identity: "0000:03:00".to_string(),
            boot_id: "boot".to_string(),
            execution_device: "0000:03:00.1".to_string(),
        };
        let receipt = FirmwareOperationPb {
            operation_id: "existing-operation".to_string(),
            state: FirmwareOperationState::Running.into(),
            phase: Some(FirmwareOperationPhase::VerifyImage.into()),
            report: Some(FirmwareFlashReport {
                flashed: true,
                reset: Some(true),
                verified_image: Some(false),
                ..Default::default()
            }),
            diagnostic: None,
        };
        let operation_calls = Arc::new(AtomicUsize::new(0));
        let supervisor = SupervisorFixture {
            identity: identity.clone(),
            receipt: receipt.clone(),
            operation_calls: operation_calls.clone(),
        };
        let (shutdown, mut stopping) = watch::channel(false);
        let server = tonic::transport::Server::builder()
            .add_service(FirmwareSupervisorServer::new(supervisor))
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async move {
                stopping.changed().await.expect("request fixture shutdown");
            });

        // Join both sides, with a fixture-wide bound in addition to each RPC's
        // deadline, so transport or shutdown regressions cannot strand a task.
        let (server_result, ()) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(server, async {
                let mut client = FirmwareSupervisorClient::connect(&socket)
                    .await
                    .expect("connect to supervisor");
                assert_eq!(SupervisorIdentity::from(client.identity.clone()), identity);
                let observed: FirmwareOperationPb = client
                    .get(&receipt.operation_id)
                    .await
                    .expect("read existing receipt")
                    .into();
                assert_eq!(observed, receipt);

                // The peer requires the complete identity from the handshake.
                let canceled: FirmwareOperationPb = client
                    .cancel(&receipt.operation_id)
                    .await
                    .expect("cancel using handshake identity")
                    .into();
                assert_eq!(
                    canceled,
                    FirmwareOperationPb {
                        state: FirmwareOperationState::CancelRequested.into(),
                        ..receipt
                    }
                );
                assert_eq!(operation_calls.load(Ordering::Relaxed), 2);

                // Guard validation must reject malformed tokens locally, even
                // though this connection can read and cancel existing work.
                let profile = FirmwareFlasherProfile::from_toml(
                    r#"
part_number = "part"
psid = "psid"
version = "version"
firmware_url = "https://host/image.bin"
"#,
                )
                .expect("parse valid checked profile");
                let context = FirmwareUpdateContext {
                    phase_id: "dispatch".into(),
                    expected_digests: FirmwareArtifactDigests {
                        image_sha256: [0; 32],
                        configuration_sha256: None,
                    },
                    expected_device: None,
                };
                context.validate(&profile).expect("valid checked context");
                assert!(matches!(
                    client
                        .submit_update_on_instance(
                            "new-operation",
                            profile,
                            context,
                            ObserverExitBehavior::Continue,
                            "instance token",
                        )
                        .await,
                    Err(FirmwareSupervisorClientError::InvalidRequest(_))
                ));
                assert_eq!(operation_calls.load(Ordering::Relaxed), 2);
                shutdown.send(true).expect("stop supervisor fixture");
            })
        })
        .await
        .expect("supervisor fixture must stop within thirty seconds");
        server_result.expect("serve supervisor fixture");
    }

    /// A reachable daemon with another API revision must fail the handshake;
    /// receipt reads and cancellation are not a fallback for a mismatched client.
    #[tokio::test]
    async fn mismatched_protocol_is_rejected_during_connection() {
        let directory = tempfile::tempdir().expect("create socket directory");
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket).expect("bind fixture socket");
        let operation_calls = Arc::new(AtomicUsize::new(0));
        let supervisor = SupervisorFixture {
            identity: SupervisorIdentity {
                protocol_version: PROTOCOL_VERSION - 1,
                device_identity: "0000:03:00".into(),
                boot_id: "boot".into(),
                execution_device: "0000:03:00.1".into(),
            },
            receipt: FirmwareOperationPb::default(),
            operation_calls: operation_calls.clone(),
        };
        let (shutdown, mut stopping) = watch::channel(false);
        let server = tonic::transport::Server::builder()
            .add_service(FirmwareSupervisorServer::new(supervisor))
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async move {
                stopping.changed().await.expect("request fixture shutdown");
            });

        let (server_result, ()) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(server, async {
                assert!(matches!(
                    FirmwareSupervisorClient::connect(&socket).await,
                    Err(FirmwareSupervisorClientError::UnsupportedProtocol(version))
                        if version == PROTOCOL_VERSION - 1
                ));
                assert_eq!(operation_calls.load(Ordering::Relaxed), 0);
                shutdown.send(true).expect("stop supervisor fixture");
            })
        })
        .await
        .expect("supervisor fixture must stop within thirty seconds");
        server_result.expect("serve supervisor fixture");
    }

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
        // Accept the real socket before letting paused runtime time reach the
        // client's deadline. The accepted peer stays silent throughout.
        tokio::time::pause();
        let result = tokio::time::timeout(RPC_TIMEOUT + Duration::from_secs(1), connection).await;
        tokio::time::resume();
        let result = result.expect("client must bound its own handshake");
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

    /// Missing-receipt observations must supply usable tokens before callers
    /// can retain them for guarded admission. No token comes from the handshake.
    #[test]
    fn missing_receipts_require_valid_instance_tokens() {
        let maximum = "x".repeat(256);
        let oversized = "x".repeat(257);
        check_values(
            [
                Check {
                    scenario: "one byte",
                    input: "!",
                    expect: Some("!"),
                },
                Check {
                    scenario: "maximum length",
                    input: maximum.as_str(),
                    expect: Some(maximum.as_str()),
                },
                Check {
                    scenario: "empty token",
                    input: "",
                    expect: None,
                },
                Check {
                    scenario: "over maximum length",
                    input: oversized.as_str(),
                    expect: None,
                },
                Check {
                    scenario: "embedded whitespace",
                    input: "instance token",
                    expect: None,
                },
                Check {
                    scenario: "non-ASCII",
                    input: "instancé",
                    expect: None,
                },
            ],
            |token| {
                let response = ObserveFirmwareOperationResponse {
                    result: Some(observe_firmware_operation_response::Result::MissingReceipt(
                        FirmwareOperationMissingReceipt {
                            supervisor_instance_id: token.into(),
                        },
                    )),
                };
                match decode_observation(response, "expected") {
                    Ok(FirmwareOperationObservation::MissingReceipt {
                        supervisor_instance_id,
                    }) => {
                        assert_eq!(supervisor_instance_id, token);
                        Some(token)
                    }
                    Err(FirmwareSupervisorClientError::InvalidResponse(_)) => None,
                    unexpected => panic!("unexpected missing-receipt result: {unexpected:?}"),
                }
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

    /// Transport success cannot substitute an empty observation or another
    /// operation's receipt for the caller's requested operation.
    #[test]
    fn observation_requires_a_result_for_the_requested_operation() {
        let unrelated_receipt = FirmwareOperationPb {
            operation_id: "different".into(),
            state: FirmwareOperationState::Accepted.into(),
            phase: None,
            report: Some(FirmwareFlashReport::default()),
            diagnostic: None,
        };
        check_values(
            [
                Check {
                    scenario: "missing result is not absence",
                    input: None,
                    expect: true,
                },
                Check {
                    scenario: "another operation's receipt is rejected",
                    input: Some(observe_firmware_operation_response::Result::Receipt(
                        unrelated_receipt,
                    )),
                    expect: true,
                },
            ],
            |result| {
                matches!(
                    decode_observation(ObserveFirmwareOperationResponse { result }, "expected"),
                    Err(FirmwareSupervisorClientError::InvalidResponse(_))
                )
            },
        );
    }
}
