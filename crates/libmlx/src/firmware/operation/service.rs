// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The socket serves receipts while one owned task executes admitted firmware
//! work. Neither an RPC future nor its connection owns that task's lifetime.

use std::os::fd::OwnedFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::{fs, io};

use carbide_libmlx_model::device::identity::MlxDeviceSelectorError;
use carbide_libmlx_model::firmware::operation::{
    FirmwareOperation, FirmwareOperationState, ObserverExitBehavior,
};
use rpc::protos::firmware_supervisor::firmware_supervisor_server::{
    FirmwareSupervisor, FirmwareSupervisorServer,
};
use rpc::protos::firmware_supervisor::{
    self as wire, CancelFirmwareOperationRequest, CancelFirmwareOperationResponse,
    GetFirmwareOperationRequest, GetFirmwareOperationResponse, GetSupervisorStatusRequest,
    GetSupervisorStatusResponse, ObserveFirmwareOperationResponse, ObserveFirmwareUpdateRequest,
    SubmitFirmwareOperationRequest, SubmitFirmwareOperationResponse, SupervisorIdentity,
    observe_firmware_operation_response,
};
use rpc::protos::mlx_device::FirmwareOperationMissingReceipt;
use sha2::{Digest, Sha256};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore, mpsc, watch};
use tokio::task::{JoinSet, spawn_blocking};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use super::claim::MutationClaim;
use super::client::{MESSAGE_LIMIT, PROTOCOL_VERSION};
use super::execution::run_operation;
use super::identity::{DeviceIdentity, current_boot_id};
use super::journal::{
    Admission, JournalError, JournalRecord, OperationJournal, OperationUpdate, RequestedFirmware,
};
use super::launch::ensure_state_directory;
use super::peer::{ConnectedSocket, PeerInfo, wait_for_exit};
use crate::firmware::config::{FirmwareFlasherProfile, FirmwareUpdateContext};
use crate::firmware::preflight::validate_preflight_request;
use crate::runner::FirmwareToolExecutor;

// Bound journal RPC work across every connection, including blocking closures
// whose callers have disconnected. Excess requests never wait for capacity.
const MAX_JOURNAL_RPC_TASKS: usize = 8;
// Allow several local observers and reconnects without retaining an unbounded
// set of transports. HTTP/2 also caps each connection at eight active streams.
const MAX_CONNECTIONS: usize = 32;
const MAX_REQUESTS_PER_CONNECTION: u32 = 8;

struct Work {
    operation_id: String,
    profile: Option<FirmwareFlasherProfile>,
    observer: Option<Arc<OwnedFd>>,
    claim: MutationClaim,
    canceled: watch::Receiver<Option<String>>,
}

/// Submission and checked observation must interpret and fingerprint the same
/// immutable request. Validation performs no journal, filesystem, or device I/O.
struct ValidatedFirmwareRequest {
    profile: Option<FirmwareFlasherProfile>,
    behavior: ObserverExitBehavior,
    update_context: Option<FirmwareUpdateContext>,
    fingerprint: [u8; 32],
}

impl ValidatedFirmwareRequest {
    fn parse(
        operation_id: &str,
        profile: Option<rpc::protos::mlx_device::FirmwareFlasherProfile>,
        observer_exit_behavior: i32,
        update_context: Option<wire::FirmwareUpdateContext>,
        execution_device: &str,
    ) -> Result<Self, Status> {
        validate_operation_id(operation_id)?;
        if update_context.is_some() && operation_id.starts_with("dpa-firmware:") {
            return Err(Status::invalid_argument(
                "firmware update operation IDs must not use the reserved dpa-firmware: prefix",
            ));
        }
        let behavior: ObserverExitBehavior =
            wire::ObserverExitBehavior::try_from(observer_exit_behavior)
                .map_err(|_| Status::invalid_argument("unknown observer exit behavior"))?
                .into();
        let profile: Option<FirmwareFlasherProfile> = profile
            .map(TryInto::try_into)
            .transpose()
            .map_err(Status::invalid_argument)?;
        if let Some(profile) = &profile {
            profile
                .flash_spec
                .validate_submission_paths()
                .map_err(|error| Status::invalid_argument(error.to_string()))?;
        }
        let update_context = update_context
            .map(|context| {
                let profile = profile.as_ref().ok_or_else(|| {
                    Status::invalid_argument("firmware update requires a profile")
                })?;
                let context = FirmwareUpdateContext {
                    phase_id: context.phase_id,
                    expected_digests: context
                        .expected_digests
                        .ok_or_else(|| {
                            Status::invalid_argument("firmware update requires expected digests")
                        })?
                        .try_into()
                        .map_err(|_| Status::invalid_argument("invalid firmware update digests"))?,
                    expected_device: context
                        .expected_device
                        .map(TryInto::try_into)
                        .transpose()
                        .map_err(|error: MlxDeviceSelectorError| {
                            Status::invalid_argument(error.to_string())
                        })?,
                };
                context
                    .validate(profile)
                    .map_err(|error| Status::invalid_argument(error.to_string()))?;
                validate_preflight_request(execution_device, &profile.firmware_spec)
                    .map_err(|_| Status::invalid_argument("invalid firmware update target"))?;
                Ok::<_, Status>(context)
            })
            .transpose()?;
        // Preserve the legacy `(profile, behavior)` and checked
        // `(profile, behavior, context, execution_device)` tuples. Serde omits
        // an absent card selector so saved receipt fingerprints stay unchanged.
        // Credentials contribute to the fingerprint but are not stored in the journal.
        let encoded = match &update_context {
            Some(context) => serde_json::to_vec(&(&profile, behavior, context, execution_device)),
            None => serde_json::to_vec(&(&profile, behavior)),
        }
        .map_err(|_| Status::internal("cannot encode firmware request"))?;
        Ok(Self {
            profile,
            behavior,
            update_context,
            fingerprint: Sha256::digest(encoded).into(),
        })
    }
}

#[derive(Clone)]
struct Supervisor {
    journal: Arc<Mutex<OperationJournal>>,
    directory: PathBuf,
    root: PathBuf,
    identity: SupervisorIdentity,
    supervisor_instance_id: String,
    work: mpsc::Sender<Work>,
    executor: FirmwareToolExecutor,
    canceled: watch::Sender<Option<String>>,
    admissions: Arc<RwLock<()>>,
    journal_rpc_tasks: Arc<Semaphore>,
}

impl Supervisor {
    /// Check the actual Unix peer, not an identity supplied in an RPC payload.
    fn peer<T>(&self, request: &Request<T>) -> Result<PeerInfo, Status> {
        let peer = request
            .extensions()
            .get::<PeerInfo>()
            .ok_or_else(|| Status::unauthenticated("socket peer credentials are required"))?;
        if peer.uid != nix::unistd::geteuid().as_raw() {
            return Err(Status::permission_denied(
                "firmware supervisor requires the owning user",
            ));
        }
        Ok(peer.clone())
    }

    /// Submit, cancel, and checked observation pin the original handshake even
    /// when Tonic reconnects the channel.
    fn check_identity(&self, expected: Option<&SupervisorIdentity>) -> Result<(), Status> {
        if expected != Some(&self.identity) {
            return Err(Status::failed_precondition(
                "firmware supervisor identity changed; reconnect before submitting work",
            ));
        }
        Ok(())
    }

    /// Reserve one journal RPC task without adding a queue of waiting callers.
    /// Execution progress and observer cancellation do not consume this quota.
    fn reserve_journal_rpc(&self) -> Result<OwnedSemaphorePermit, Status> {
        self.journal_rpc_tasks
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("firmware journal RPC capacity reached"))
    }

    /// Read durable state without inferring completion from process liveness.
    async fn snapshot(&self, operation_id: &str) -> Result<FirmwareOperation, Status> {
        self.find_operation(operation_id, None)
            .await?
            .ok_or_else(|| Status::not_found("firmware operation is not recorded"))
    }

    /// Only a readable journal can establish absence. Uncertain persistence
    /// fences this lookup instead of returning permission to submit new work.
    async fn find_operation(
        &self,
        operation_id: &str,
        expected_fingerprint: Option<[u8; 32]>,
    ) -> Result<Option<FirmwareOperation>, Status> {
        validate_operation_id(operation_id)?;
        let permit = self.reserve_journal_rpc()?;
        let journal = self.journal.clone();
        let operation_id = operation_id.to_owned();
        spawn_blocking(move || {
            // Dropping the RPC does not stop a blocking closure. Keep its
            // reservation until the journal access actually finishes.
            let _permit = permit;
            let journal = journal.lock().expect("firmware journal mutex poisoned");
            let Some(record) = journal.record(&operation_id).map_err(journal_status)? else {
                return Ok(None);
            };
            if expected_fingerprint.is_some_and(|expected| expected != record.request_sha256) {
                return Err(Status::already_exists(
                    "firmware operation ID identifies different work",
                ));
            }
            Ok(Some(record.operation.clone()))
        })
        .await
        .map_err(|error| journal_status(error.into()))?
    }

    /// Persist a stop request; the execution task owns safe-boundary handling.
    /// Observer exit bypasses the RPC quota so overloaded clients cannot block it.
    async fn cancel(
        &self,
        operation_id: &str,
        rpc_permit: Option<OwnedSemaphorePermit>,
    ) -> Result<FirmwareOperation, Status> {
        let journal = self.journal.clone();
        let canceled = self.canceled.clone();
        let operation_id = operation_id.to_owned();
        spawn_blocking(move || {
            let _rpc_permit = rpc_permit;
            let mut journal = journal.lock().expect("firmware journal mutex poisoned");
            let operation = request_cancel(&mut journal, &operation_id).map_err(journal_status)?;
            if operation.state == FirmwareOperationState::CancelRequested {
                // The preparation future may stop as soon as this is observed,
                // so its cancellation receipt must already be durable.
                canceled.send_replace(Some(operation_id));
            }
            Ok(operation)
        })
        .await
        .map_err(|error| journal_status(error.into()))?
    }

    /// Detached admissions must finish queueing before shutdown closes the
    /// receiver; their owning RPC may already have been canceled.
    async fn close_admissions(&self, receiver: &mut mpsc::Receiver<Work>) {
        let admissions = self.admissions.write().await;
        receiver.close();
        drop(admissions);
    }

    /// Release host exclusion only after durable completion or a failure or
    /// cancellation before mutation. Losing this task leaves the claim intact.
    async fn execute(&self, work: Work) -> Result<(), JournalError> {
        let Work {
            operation_id,
            profile,
            observer,
            claim,
            canceled,
        } = work;
        let result = async {
            match profile {
                Some(profile) => {
                    run_operation(
                        self.identity.execution_device.clone(),
                        operation_id.clone(),
                        profile,
                        self.directory.clone(),
                        self.journal.clone(),
                        self.executor.clone(),
                        canceled,
                    )
                    .await
                }
                None => {
                    let journal = self.journal.clone();
                    let operation_id = operation_id.clone();
                    spawn_blocking(move || {
                        let mut journal = journal.lock().expect("firmware journal mutex poisoned");
                        let operation = journal
                            .snapshot(&operation_id)?
                            .ok_or_else(|| JournalError::NotFound(operation_id.clone()))?;
                        journal.update(
                            &operation_id,
                            OperationUpdate {
                                state: FirmwareOperationState::Complete,
                                phase: None,
                                report: operation.report,
                                diagnostic: Some("no firmware update was requested".into()),
                                prepared_inputs: None,
                                mutation_started: false,
                            },
                        )?;
                        Ok(())
                    })
                    .await?
                }
            }
        };
        tokio::pin!(result);
        if let Some(observer) = observer {
            tokio::select! {
                result = &mut result => result?,
                exited = wait_for_exit(observer) => {
                    // Loss of the observer or its monitor requests a safe stop;
                    // it never drops an executing mutation future.
                    if let Err(error) = exited {
                        tracing::warn!(%operation_id, %error, "firmware observer monitor failed");
                    }
                    if let Err(error) = self.cancel(&operation_id, None).await {
                        tracing::error!(%operation_id, %error, "failed to record firmware cancellation");
                    }
                    result.await?;
                }
            }
        } else {
            result.await?;
        }
        let journal = self.journal.clone();
        let operation_id = operation_id.clone();
        spawn_blocking(move || {
            let safe_to_release = journal
                .lock()
                .expect("firmware journal mutex poisoned")
                .record(&operation_id)?
                .is_some_and(JournalRecord::is_safe_to_release);
            if safe_to_release && let Err(error) = claim.release() {
                // The completed receipt remains available. A lingering claim must
                // still block later mutation rather than being removed by a retry.
                tracing::error!(%operation_id, %error, "failed to release firmware mutation claim");
            }
            Ok::<_, JournalError>(())
        })
        .await??;
        Ok(())
    }
}

#[tonic::async_trait]
impl FirmwareSupervisor for Supervisor {
    async fn submit_firmware_operation(
        &self,
        request: Request<SubmitFirmwareOperationRequest>,
    ) -> Result<Response<SubmitFirmwareOperationResponse>, Status> {
        let peer = self.peer(&request)?;
        let request = request.into_inner();
        self.check_identity(request.expected_identity.as_ref())?;
        let ValidatedFirmwareRequest {
            profile,
            behavior,
            update_context,
            fingerprint,
        } = ValidatedFirmwareRequest::parse(
            &request.operation_id,
            request.profile,
            request.observer_exit_behavior,
            request.update_context,
            &self.identity.execution_device,
        )?;
        let permit = self.reserve_journal_rpc()?;
        // Shutdown waits for this guard even if the RPC stops awaiting its
        // blocking admission, so accepted work reaches the draining queue.
        let admission = self.admissions.clone().read_owned().await;
        let supervisor = self.clone();
        let operation = spawn_blocking(move || {
            let _permit = permit;
            let _admission = admission;
            let mut journal = supervisor
                .journal
                .lock()
                .expect("firmware journal mutex poisoned");
            if let Some(existing) = journal.record(&request.operation_id).map_err(journal_status)? {
                if existing.request_sha256 != fingerprint {
                    return Err(Status::already_exists(
                        "firmware operation ID identifies different work",
                    ));
                }
                return Ok(existing.operation.clone());
            }
            // A replacement may return a retained receipt, but it cannot infer
            // whether an absent ID ran before its journal disappeared. Keep the
            // caller's original token outside the fingerprint and check it only
            // after duplicate lookup, under the same lock as new admission.
            if request
                .expected_supervisor_instance_id
                .as_ref()
                .is_some_and(|expected| expected != &supervisor.supervisor_instance_id)
            {
                return Err(Status::failed_precondition(
                    "firmware supervisor instance changed; missing operation requires reconciliation",
                ));
            }
            let observer = match behavior {
                ObserverExitBehavior::Continue => None,
                ObserverExitBehavior::Terminate => Some(peer.pidfd.ok_or_else(|| {
                    if let Some(error) = peer.pidfd_error {
                        tracing::warn!(%error, "cannot monitor submitting firmware process");
                    }
                    Status::failed_precondition(
                        "terminating on observer exit requires kernel support for socket peer pidfds",
                    )
                })?),
            };
            let requested = profile.as_ref().map(|profile| RequestedFirmware {
                target: profile.firmware_spec.clone(),
                options: profile.flash_options.clone(),
                update_context,
            });
            let record = JournalRecord {
                operation: FirmwareOperation {
                    operation_id: request.operation_id.clone(),
                    state: FirmwareOperationState::Accepted,
                    phase: None,
                    report: requested
                        .as_ref()
                        .map(RequestedFirmware::initial_report)
                        .unwrap_or_default(),
                    diagnostic: None,
                },
                request_sha256: fingerprint,
                boot_id: supervisor.identity.boot_id.clone(),
                requested,
                observer_exit_behavior: behavior,
                prepared_inputs: None,
                mutation_started: false,
            };
            // Reject capacity and predecessor failures before creating host state.
            // The same mutex covers this check and the eventual durable admission.
            journal.check_admission(&record).map_err(journal_status)?;
            // PCI locators do not establish independent physical reset domains;
            // the admission policy deliberately serializes work host-wide.
            let claim = MutationClaim::acquire(
                &supervisor.root,
                &request.operation_id,
                &supervisor.identity.device_identity,
                &supervisor.identity.boot_id,
            )
            .map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    Status::resource_exhausted(
                        "another firmware operation owns the host or requires reconciliation",
                    )
                } else {
                    tracing::error!(%error, "cannot persist firmware mutation claim");
                    Status::internal("cannot persist firmware mutation claim")
                }
            })?;
            let operation = match journal.admit(record).map_err(journal_status)? {
                Admission::Accepted(operation) | Admission::Existing(operation) => operation,
            };
            // Persistence and queueing share this blocking closure. Dropping its
            // RPC future cannot strand an accepted operation before the handoff.
            supervisor
                .work
                .try_send(Work {
                    operation_id: request.operation_id,
                    profile,
                    observer,
                    claim,
                    canceled: supervisor.canceled.subscribe(),
                })
                .map_err(|_| {
                    Status::unavailable(
                        "firmware executor is unavailable; retrieve the recorded operation",
                    )
                })?;
            Ok(operation)
        })
        .await
        .map_err(|error| journal_status(error.into()))??;
        Ok(Response::new(SubmitFirmwareOperationResponse {
            operation: Some(operation.into()),
        }))
    }

    async fn get_firmware_operation(
        &self,
        request: Request<GetFirmwareOperationRequest>,
    ) -> Result<Response<GetFirmwareOperationResponse>, Status> {
        self.peer(&request)?;
        let operation = self.snapshot(&request.into_inner().operation_id).await?;
        Ok(Response::new(GetFirmwareOperationResponse {
            operation: Some(operation.into()),
        }))
    }

    async fn observe_firmware_update(
        &self,
        request: Request<ObserveFirmwareUpdateRequest>,
    ) -> Result<Response<ObserveFirmwareOperationResponse>, Status> {
        self.peer(&request)?;
        let request = request.into_inner();
        self.check_identity(request.expected_identity.as_ref())?;
        if request.update_context.is_none() {
            return Err(Status::invalid_argument(
                "firmware update requires update context",
            ));
        }
        let validated = ValidatedFirmwareRequest::parse(
            &request.operation_id,
            request.profile,
            request.observer_exit_behavior,
            request.update_context,
            &self.identity.execution_device,
        )?;
        let result = match self
            .find_operation(&request.operation_id, Some(validated.fingerprint))
            .await?
        {
            Some(operation) => {
                observe_firmware_operation_response::Result::Receipt(operation.into())
            }
            None => observe_firmware_operation_response::Result::MissingReceipt(
                FirmwareOperationMissingReceipt {
                    supervisor_instance_id: self.supervisor_instance_id.clone(),
                },
            ),
        };
        Ok(Response::new(ObserveFirmwareOperationResponse {
            result: Some(result),
        }))
    }

    async fn cancel_firmware_operation(
        &self,
        request: Request<CancelFirmwareOperationRequest>,
    ) -> Result<Response<CancelFirmwareOperationResponse>, Status> {
        self.peer(&request)?;
        let request = request.into_inner();
        self.check_identity(request.expected_identity.as_ref())?;
        validate_operation_id(&request.operation_id)?;
        let permit = self.reserve_journal_rpc()?;
        let operation = self.cancel(&request.operation_id, Some(permit)).await?;
        Ok(Response::new(CancelFirmwareOperationResponse {
            operation: Some(operation.into()),
        }))
    }

    async fn get_supervisor_status(
        &self,
        request: Request<GetSupervisorStatusRequest>,
    ) -> Result<Response<GetSupervisorStatusResponse>, Status> {
        self.peer(&request)?;
        Ok(Response::new(GetSupervisorStatusResponse {
            protocol_version: self.identity.protocol_version,
            device_identity: self.identity.device_identity.clone(),
            boot_id: self.identity.boot_id.clone(),
            execution_device: self.identity.execution_device.clone(),
        }))
    }
}

/// Bound caller-chosen receipt keys without treating them as file paths.
fn validate_operation_id(operation_id: &str) -> Result<(), Status> {
    if operation_id.is_empty()
        || operation_id.len() > 256
        || !operation_id.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(Status::invalid_argument(
            "operation ID must contain 1..256 printable nonspace ASCII bytes",
        ));
    }
    Ok(())
}

/// Preserve actionable admission classifications without exposing stored data.
fn journal_status(error: JournalError) -> Status {
    match error {
        JournalError::ConflictingRequest(_) => {
            Status::already_exists("operation ID identifies different work")
        }
        JournalError::NotFound(_) => Status::not_found("firmware operation is not recorded"),
        JournalError::ReceiptLimit | JournalError::SizeLimit => Status::failed_precondition(
            "firmware receipt capacity reached; operator maintenance is required",
        ),
        JournalError::UnresolvedPredecessor(_) | JournalError::BootMismatch => {
            Status::failed_precondition("prior firmware work requires reconciliation")
        }
        JournalError::ReloadRequired => Status::unavailable(
            "firmware journal must be reopened after an uncertain save; do not repeat device work",
        ),
        error => {
            tracing::error!(%error, "firmware journal operation failed");
            Status::internal("firmware journal is unavailable; do not repeat device work")
        }
    }
}

/// Cancellation cannot erase settled outcomes or authorize a new mutation.
fn request_cancel(
    journal: &mut OperationJournal,
    operation_id: &str,
) -> Result<FirmwareOperation, JournalError> {
    let operation = journal
        .snapshot(operation_id)?
        .ok_or_else(|| JournalError::NotFound(operation_id.into()))?;
    if !matches!(
        operation.state,
        FirmwareOperationState::Accepted | FirmwareOperationState::Running
    ) {
        return Ok(operation);
    }
    journal.update(
        operation_id,
        OperationUpdate {
            state: FirmwareOperationState::CancelRequested,
            phase: operation.phase,
            report: operation.report,
            diagnostic: Some("cancellation requested; waiting for a safe phase boundary".into()),
            prepared_inputs: None,
            mutation_started: false,
        },
    )
}

/// Reject excess sockets immediately; an accepted socket retains its capacity
/// until Tonic drops the transport, independently of individual RPC lifetimes.
fn accept_connection(
    stream: io::Result<UnixStream>,
    connections: &Arc<Semaphore>,
) -> Option<io::Result<ConnectedSocket>> {
    let stream = match stream {
        Ok(stream) => stream,
        Err(error) => return Some(Err(error)),
    };
    let Ok(permit) = connections.clone().try_acquire_owned() else {
        tracing::debug!("firmware supervisor connection capacity reached");
        return None;
    };
    Some(ConnectedSocket::new(stream, permit))
}

/// Serve one PCI slot until shutdown, then finish already admitted work.
///
/// The state root must be private and shared by all local firmware callers.
/// A host-wide durable claim serializes mutations because PCI addresses do not
/// certify independent physical reset domains. Unfinished work is never replayed
/// after helper restart. SIGTERM/SIGINT stop admission, not an active burn.
pub async fn serve(device: &str, state_directory: &Path) -> io::Result<()> {
    // The child inherits this loaded executable before exec. Linux resolves
    // /proc/self/exe to that image even if its installed path was replaced,
    // keeping Serve and per-command modes on the same worker protocol.
    let executor = FirmwareToolExecutor::with_supervisor_path("/proc/self/exe");
    ensure_state_directory(state_directory)?;
    let device = DeviceIdentity::parse(device)?;
    let boot_id = current_boot_id()?;
    let directory = state_directory.join(&device.key);
    let journal =
        OperationJournal::open(&directory, &device.device, &boot_id).map_err(io::Error::other)?;
    let socket = directory.join("control.sock");
    match fs::symlink_metadata(&socket) {
        Ok(metadata)
            if metadata.file_type().is_socket()
                && metadata.uid() == nix::unistd::geteuid().as_raw() =>
        {
            fs::remove_file(&socket)?
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe firmware socket path",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    // Only one host mutation can be admitted; one slot accommodates the
    // handoff before the executor task starts. Producers never wait for space.
    let (work, mut receiver) = mpsc::channel(1);
    let (canceled, _) = watch::channel(None);
    let supervisor = Supervisor {
        journal: Arc::new(Mutex::new(journal)),
        directory,
        root: state_directory.to_path_buf(),
        identity: SupervisorIdentity {
            protocol_version: PROTOCOL_VERSION,
            device_identity: device.key,
            boot_id,
            execution_device: device.device,
        },
        supervisor_instance_id: Uuid::new_v4().to_string(),
        work,
        executor,
        canceled,
        admissions: Arc::new(RwLock::new(())),
        journal_rpc_tasks: Arc::new(Semaphore::new(MAX_JOURNAL_RPC_TASKS)),
    };
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let incoming = UnixListenerStream::new(listener)
        .filter_map(move |stream| accept_connection(stream, &connections));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let shutdown = async move {
        tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {} }
    };
    // This transport limit complements the global journal RPC quota. The
    // durable claim separately serializes hardware mutation across the host.
    let server = tonic::transport::Server::builder()
        .concurrency_limit_per_connection(MAX_REQUESTS_PER_CONNECTION as usize)
        .max_concurrent_streams(MAX_REQUESTS_PER_CONNECTION)
        .add_service(
            FirmwareSupervisorServer::new(supervisor.clone())
                .max_decoding_message_size(MESSAGE_LIMIT)
                .max_encoding_message_size(MESSAGE_LIMIT),
        )
        .serve_with_incoming_shutdown(incoming, shutdown);
    tokio::pin!(server);
    let mut tasks = JoinSet::new();
    let result = loop {
        tokio::select! {
            result = &mut server => break result,
            Some(work) = receiver.recv() => {
                let supervisor = supervisor.clone();
                tasks.spawn(async move { supervisor.execute(work).await });
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => check_task_result(result)?,
        }
    };
    supervisor.close_admissions(&mut receiver).await;
    while let Some(work) = receiver.recv().await {
        let supervisor = supervisor.clone();
        tasks.spawn(async move { supervisor.execute(work).await });
    }
    while let Some(result) = tasks.join_next().await {
        check_task_result(result)?;
    }
    result.map_err(io::Error::other)
}

/// Keep persistence failures observable while retaining the last saved receipt.
fn check_task_result(
    result: Result<Result<(), JournalError>, tokio::task::JoinError>,
) -> io::Result<()> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            // Execution has ended, but its outcome could not be saved. Exit
            // rather than serve an apparently running receipt forever. Reopen
            // records interruption; the host claim still prohibits replay.
            tracing::error!(%error, "firmware execution could not persist progress");
            Err(io::Error::other(error))
        }
        Err(error) => Err(io::Error::other(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use std::time::Duration;

    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::*;

    /// Use a private ledger and an absent helper, so service tests can exercise
    /// admission and known launch failures without running a hardware tool.
    fn supervisor_fixture() -> (tempfile::TempDir, Supervisor, mpsc::Receiver<Work>) {
        let root = tempfile::tempdir().expect("create supervisor fixture");
        let directory = root.path().join("0000:01:00");
        let journal = OperationJournal::open(&directory, "0000:01:00.0", "boot")
            .expect("open supervisor journal");
        let (work, receiver) = mpsc::channel(1);
        let (canceled, _) = watch::channel(None);
        let supervisor = Supervisor {
            journal: Arc::new(Mutex::new(journal)),
            directory,
            root: root.path().to_path_buf(),
            identity: SupervisorIdentity {
                protocol_version: PROTOCOL_VERSION,
                device_identity: "0000:01:00".into(),
                boot_id: "boot".into(),
                execution_device: "0000:01:00.0".into(),
            },
            supervisor_instance_id: Uuid::new_v4().to_string(),
            work,
            executor: FirmwareToolExecutor::with_supervisor_path(root.path().join("absent-helper")),
            canceled,
            admissions: Arc::new(RwLock::new(())),
            journal_rpc_tasks: Arc::new(Semaphore::new(MAX_JOURNAL_RPC_TASKS)),
        };
        (root, supervisor, receiver)
    }

    /// Supply the same peer credentials that the Unix socket attaches in production.
    fn peer_request<T>(message: T) -> Request<T> {
        let mut request = Request::new(message);
        request.extensions_mut().insert(PeerInfo {
            uid: nix::unistd::geteuid().as_raw(),
            pidfd: None,
            pidfd_error: None,
        });
        request
    }

    /// Submit no hardware work unless a test explicitly supplies a profile.
    fn submission(supervisor: &Supervisor, operation_id: &str) -> SubmitFirmwareOperationRequest {
        SubmitFirmwareOperationRequest {
            operation_id: operation_id.into(),
            profile: None,
            observer_exit_behavior: wire::ObserverExitBehavior::Continue.into(),
            expected_identity: Some(supervisor.identity.clone()),
            update_context: None,
            expected_supervisor_instance_id: None,
        }
    }

    fn firmware_submission(
        supervisor: &Supervisor,
        operation_id: &str,
    ) -> SubmitFirmwareOperationRequest {
        SubmitFirmwareOperationRequest {
            profile: Some(
                FirmwareFlasherProfile {
                    firmware_spec: carbide_libmlx_model::firmware::FirmwareSpec {
                        part_number: "part".into(),
                        psid: "psid".into(),
                        version: "version".into(),
                    },
                    flash_spec: crate::firmware::config::FlashSpec {
                        firmware_url: supervisor
                            .root
                            .join("unused-image.bin")
                            .to_string_lossy()
                            .into_owned(),
                        firmware_credentials: None,
                        device_conf_url: None,
                        device_conf_credentials: None,
                        verify_from_cache: false,
                        cache_dir: None,
                    },
                    flash_options: crate::firmware::config::FlashOptions::default(),
                }
                .into(),
            ),
            ..submission(supervisor, operation_id)
        }
    }

    /// Keep checked fixtures valid before each case changes one admission field.
    fn checked_submission(
        supervisor: &Supervisor,
        operation_id: &str,
    ) -> SubmitFirmwareOperationRequest {
        let mut request = firmware_submission(supervisor, operation_id);
        request
            .profile
            .as_mut()
            .unwrap()
            .flash_spec
            .as_mut()
            .unwrap()
            .firmware_url = "https://host/image.bin".into();
        request.update_context = Some(wire::FirmwareUpdateContext {
            phase_id: "dispatch-1".into(),
            expected_digests: Some(rpc::protos::mlx_device::FirmwareArtifactDigests {
                image_sha256: "00".repeat(32),
                configuration_sha256: None,
            }),
            expected_device: None,
        });
        request
    }

    /// Observation repeats immutable work but cannot request new admission.
    fn checked_observation(
        request: SubmitFirmwareOperationRequest,
    ) -> ObserveFirmwareUpdateRequest {
        ObserveFirmwareUpdateRequest {
            operation_id: request.operation_id,
            profile: request.profile,
            observer_exit_behavior: request.observer_exit_behavior,
            expected_identity: request.expected_identity,
            update_context: request.update_context,
        }
    }

    /// An omitted card selector must not change saved request fingerprints.
    /// Freeze the serialized tuple independently of the current Rust types.
    #[test]
    fn omitted_selector_preserves_saved_checked_fingerprint() {
        let (_root, supervisor, _receiver) = supervisor_fixture();
        let request = checked_submission(&supervisor, "checked");
        let validated = ValidatedFirmwareRequest::parse(
            &request.operation_id,
            request.profile,
            request.observer_exit_behavior,
            request.update_context,
            &supervisor.identity.execution_device,
        )
        .unwrap();
        let predecessor = concat!(
            r#"[{"part_number":"part","psid":"psid","version":"version","#,
            r#""firmware_url":"https://host/image.bin","firmware_credentials":null,"#,
            r#""device_conf_url":null,"device_conf_credentials":null,"verify_from_cache":false,"cache_dir":null,"#,
            r#""verify_image":false,"verify_version":false,"reset":false,"reset_level":3},"Continue","#,
            r#"{"phase_id":"dispatch-1","expected_digests":{"image_sha256":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"configuration_sha256":null}},"0000:01:00.0"]"#,
        );
        let expected: [u8; 32] = Sha256::digest(predecessor.as_bytes()).into();
        assert_eq!(validated.fingerprint, expected);
    }

    /// Omitted retry settings must preserve the exact predecessor request hash,
    /// so upgrading the daemon does not turn an identical delivery into a conflict.
    #[tokio::test]
    async fn omitted_retry_settings_preserve_predecessor_request_identity() {
        let (_root, supervisor, _receiver) = supervisor_fixture();
        let mut request = firmware_submission(&supervisor, "legacy-request");
        request
            .profile
            .as_mut()
            .unwrap()
            .flash_spec
            .as_mut()
            .unwrap()
            .firmware_url = "/unused-image.bin".into();

        supervisor
            .submit_firmware_operation(peer_request(request.clone()))
            .await
            .expect("admit profile without retry fields");

        // Freeze the old serialization, including struct field order. Building
        // this expected value with today's types would hide identity changes.
        let predecessor = concat!(
            r#"[{"part_number":"part","psid":"psid","version":"version","#,
            r#""firmware_url":"/unused-image.bin","firmware_credentials":null,"#,
            r#""device_conf_url":null,"device_conf_credentials":null,"verify_from_cache":false,"cache_dir":null,"#,
            r#""verify_image":false,"verify_version":false,"reset":false,"reset_level":3},"Continue"]"#,
        );
        let expected: [u8; 32] = Sha256::digest(predecessor.as_bytes()).into();
        assert_eq!(
            supervisor
                .journal
                .lock()
                .unwrap()
                .record("legacy-request")
                .unwrap()
                .unwrap()
                .request_sha256,
            expected,
        );
        supervisor
            .submit_firmware_operation(peer_request(request))
            .await
            .expect("identical omitted policy returns the retained receipt");
    }

    /// Checked observation and admission share immutable request identity,
    /// including the phase ID, without changing legacy receipt fingerprints.
    #[tokio::test]
    async fn checked_observation_and_admission_preserve_request_identity() {
        let (_root, supervisor, mut receiver) = supervisor_fixture();
        supervisor
            .submit_firmware_operation(peer_request(submission(&supervisor, "legacy")))
            .await
            .unwrap();
        let expected: [u8; 32] = Sha256::digest(br#"[null,"Continue"]"#).into();
        assert_eq!(
            supervisor
                .journal
                .lock()
                .unwrap()
                .record("legacy")
                .unwrap()
                .unwrap()
                .request_sha256,
            expected
        );
        supervisor
            .execute(receiver.recv().await.unwrap())
            .await
            .unwrap();

        let mut request = checked_submission(&supervisor, "checked");
        request.expected_supervisor_instance_id = Some(supervisor.supervisor_instance_id.clone());
        request
            .profile
            .as_mut()
            .unwrap()
            .flash_spec
            .as_mut()
            .unwrap()
            .device_conf_url = Some("https://host/config.bin".into());
        request
            .update_context
            .as_mut()
            .unwrap()
            .expected_digests
            .as_mut()
            .unwrap()
            .configuration_sha256 = Some("22".repeat(32));

        // The same profile cannot upgrade an existing legacy receipt into a
        // checked update. Only a new operation ID may select that contract.
        let mut legacy = request.clone();
        legacy.operation_id = "legacy-profile".into();
        legacy.update_context = None;
        supervisor
            .submit_firmware_operation(peer_request(legacy.clone()))
            .await
            .unwrap();
        let legacy_work = receiver.recv().await.unwrap();
        let legacy_saved = fs::read(supervisor.directory.join("operations.json")).unwrap();
        legacy.update_context = request.update_context.clone();
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(legacy))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::AlreadyExists
        );
        assert_eq!(
            fs::read(supervisor.directory.join("operations.json")).unwrap(),
            legacy_saved
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        supervisor.execute(legacy_work).await.unwrap();

        // An exact checked lookup can establish absence without creating work,
        // even when its source is not reachable from this test process.
        let before_observation = fs::read(supervisor.directory.join("operations.json")).unwrap();
        let observed = supervisor
            .observe_firmware_update(peer_request(checked_observation(request.clone())))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            observed.result,
            Some(observe_firmware_operation_response::Result::MissingReceipt(
                FirmwareOperationMissingReceipt {
                    supervisor_instance_id: supervisor.supervisor_instance_id.clone()
                }
            ))
        );
        assert_eq!(
            fs::read(supervisor.directory.join("operations.json")).unwrap(),
            before_observation
        );
        assert!(!supervisor.root.join("mutation.json").exists());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        let accepted = supervisor
            .submit_firmware_operation(peer_request(request.clone()))
            .await
            .unwrap()
            .into_inner();
        let _work = receiver.recv().await.unwrap();
        let saved = fs::read(supervisor.directory.join("operations.json")).unwrap();
        assert_eq!(
            supervisor
                .observe_firmware_update(peer_request(checked_observation(request.clone())))
                .await
                .unwrap()
                .into_inner()
                .result,
            Some(observe_firmware_operation_response::Result::Receipt(
                accepted.operation.clone().unwrap()
            ))
        );
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(request.clone()))
                .await
                .unwrap()
                .into_inner(),
            accepted
        );
        // Once admitted, only the immutable work decides whether this is the
        // same operation. A stale instance token cannot turn it into new work.
        let mut stale_instance = request.clone();
        stale_instance.expected_supervisor_instance_id = Some("previous-instance".into());
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(stale_instance))
                .await
                .unwrap()
                .into_inner(),
            accepted
        );
        struct Case {
            label: &'static str,
            change: fn(&mut SubmitFirmwareOperationRequest),
        }
        for case in [
            Case {
                label: "whole-workflow phase ID",
                change: |request| {
                    request.update_context.as_mut().unwrap().phase_id = "dispatch-2".into();
                },
            },
            Case {
                label: "image digest",
                change: |request| {
                    request
                        .update_context
                        .as_mut()
                        .unwrap()
                        .expected_digests
                        .as_mut()
                        .unwrap()
                        .image_sha256 = "11".repeat(32);
                },
            },
            Case {
                label: "configuration digest",
                change: |request| {
                    request
                        .update_context
                        .as_mut()
                        .unwrap()
                        .expected_digests
                        .as_mut()
                        .unwrap()
                        .configuration_sha256 = Some("33".repeat(32));
                },
            },
            Case {
                label: "requested firmware version",
                change: |request| {
                    request
                        .profile
                        .as_mut()
                        .unwrap()
                        .firmware_spec
                        .as_mut()
                        .unwrap()
                        .version = "other-version".into();
                },
            },
            Case {
                label: "stable device identity",
                change: |request| {
                    request.update_context.as_mut().unwrap().expected_device = Some(
                        carbide_libmlx_model::device::identity::MlxDeviceSelector::Guid(
                            "another-card".into(),
                        )
                        .into(),
                    );
                },
            },
            Case {
                label: "checked receipt reused as legacy",
                change: |request| request.update_context = None,
            },
        ] {
            let mut conflicting = request.clone();
            (case.change)(&mut conflicting);
            let observation_code = if conflicting.update_context.is_some() {
                tonic::Code::AlreadyExists
            } else {
                tonic::Code::InvalidArgument
            };
            assert_eq!(
                supervisor
                    .observe_firmware_update(peer_request(checked_observation(conflicting.clone())))
                    .await
                    .unwrap_err()
                    .code(),
                observation_code,
                "{} observed",
                case.label
            );
            assert_eq!(
                supervisor
                    .submit_firmware_operation(peer_request(conflicting))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::AlreadyExists,
                "{}",
                case.label
            );
        }
        assert_eq!(
            fs::read(supervisor.directory.join("operations.json")).unwrap(),
            saved
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    /// Checked requests cannot become the legacy missing-profile no-op, nor
    /// create an exclusion marker before required context validation finishes.
    #[tokio::test]
    async fn invalid_update_context_has_no_admission_effects() {
        struct Case {
            label: &'static str,
            invalidate: fn(&mut SubmitFirmwareOperationRequest),
        }
        for case in [
            Case {
                label: "missing profile",
                invalidate: |request| request.profile = None,
            },
            Case {
                label: "missing digests",
                invalidate: |request| {
                    request.update_context.as_mut().unwrap().expected_digests = None;
                },
            },
            Case {
                label: "invalid phase",
                invalidate: |request| {
                    request.update_context.as_mut().unwrap().phase_id = "has space".into();
                },
            },
            Case {
                label: "missing device selector variant",
                invalidate: |request| {
                    request.update_context.as_mut().unwrap().expected_device =
                        Some(rpc::protos::mlx_device::MlxDeviceSelector { identity: None });
                },
            },
            Case {
                label: "padded target version",
                invalidate: |request| {
                    request
                        .profile
                        .as_mut()
                        .unwrap()
                        .firmware_spec
                        .as_mut()
                        .unwrap()
                        .version = " version".into();
                },
            },
            Case {
                label: "URL credentials",
                invalidate: |request| {
                    request
                        .profile
                        .as_mut()
                        .unwrap()
                        .flash_spec
                        .as_mut()
                        .unwrap()
                        .firmware_url = "https://user:secret@host/image.bin".into();
                },
            },
            Case {
                label: "reserved DPA operation prefix",
                invalidate: |request| request.operation_id = "dpa-firmware:reserved".into(),
            },
        ] {
            let (root, supervisor, mut receiver) = supervisor_fixture();
            let mut request = checked_submission(&supervisor, "rejected");
            (case.invalidate)(&mut request);
            let scenario = case.label;
            let saved = fs::read(supervisor.directory.join("operations.json")).unwrap();
            let error = supervisor
                .submit_firmware_operation(peer_request(request))
                .await
                .unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument, "{scenario}");
            assert!(!error.message().contains("secret"), "{scenario}");
            assert_eq!(
                fs::read(supervisor.directory.join("operations.json")).unwrap(),
                saved,
                "{scenario}"
            );
            assert!(!root.path().join("mutation.json").exists(), "{scenario}");
            assert!(
                matches!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "{scenario}"
            );
        }
    }

    /// An absent receipt never permits a caller to cross service instances.
    /// Reject before creating either the host claim or an admission record.
    #[tokio::test]
    async fn guarded_admission_rejects_another_instance_without_effects() {
        for expected in ["", "previous-instance"] {
            let (root, supervisor, mut receiver) = supervisor_fixture();
            let path = supervisor.directory.join("operations.json");
            let saved = fs::read(&path).unwrap();
            let mut request = checked_submission(&supervisor, "missing");
            request.expected_supervisor_instance_id = Some(expected.into());
            let error = supervisor
                .submit_firmware_operation(peer_request(request))
                .await
                .unwrap_err();
            assert_eq!(
                error.code(),
                tonic::Code::FailedPrecondition,
                "expected instance {expected:?}"
            );
            assert_eq!(
                error.message(),
                "firmware supervisor instance changed; missing operation requires reconciliation",
                "expected instance {expected:?}"
            );
            assert_eq!(fs::read(&path).unwrap(), saved);
            assert!(!root.path().join("mutation.json").exists());
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
    }

    /// Removing a file cannot make a live service forget a prior admission.
    /// Its process token remains safe because receipt lookup uses the cache.
    #[tokio::test]
    async fn live_instance_keeps_receipts_when_the_journal_file_is_removed() {
        let (_root, supervisor, mut receiver) = supervisor_fixture();
        let mut request = checked_submission(&supervisor, "saved");
        request.expected_supervisor_instance_id = Some(supervisor.supervisor_instance_id.clone());
        let accepted = supervisor
            .submit_firmware_operation(peer_request(request.clone()))
            .await
            .unwrap()
            .into_inner()
            .operation
            .unwrap();
        // Admission alone must remain visible; leave queued work unexecuted.
        let _work = receiver.recv().await.unwrap();
        fs::remove_file(supervisor.directory.join("operations.json")).unwrap();

        let observed = supervisor
            .observe_firmware_update(peer_request(checked_observation(request.clone())))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            observed.result,
            Some(observe_firmware_operation_response::Result::Receipt(
                accepted.clone()
            ))
        );
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(request))
                .await
                .unwrap()
                .into_inner()
                .operation,
            Some(accepted)
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    /// The reserved prefix still belongs to legacy DPA callers; checked
    /// admission must not make their existing operation IDs invalid.
    #[tokio::test]
    async fn legacy_submission_accepts_the_reserved_dpa_operation_prefix() {
        let (root, supervisor, mut receiver) = supervisor_fixture();
        let request = firmware_submission(&supervisor, "dpa-firmware:legacy");
        let accepted = supervisor
            .submit_firmware_operation(peer_request(request.clone()))
            .await
            .expect("admit legacy DPA operation")
            .into_inner();
        let work = receiver.recv().await.unwrap();
        assert_eq!(work.operation_id, request.operation_id);
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(request))
                .await
                .unwrap()
                .into_inner(),
            accepted
        );
        assert!(root.path().join("mutation.json").exists());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    /// Checked submission without an instance guard must preserve its profile
    /// and context through the public client, work queue, and durable journal.
    /// Observation with a different observer policy must not change that work.
    #[tokio::test]
    async fn checked_submission_without_instance_guard_is_durable() {
        use super::super::client::{FirmwareSupervisorClient, FirmwareSupervisorClientError};
        use crate::firmware::config::FirmwareArtifactDigests;

        let (_root, supervisor, mut receiver) = supervisor_fixture();
        let socket = supervisor.directory.join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let incoming = UnixListenerStream::new(listener)
            .filter_map(move |stream| accept_connection(stream, &connections));
        let (stop, mut stopped) = watch::channel(false);
        let server = tonic::transport::Server::builder()
            .add_service(FirmwareSupervisorServer::new(supervisor.clone()))
            .serve_with_incoming_shutdown(incoming, async move {
                stopped
                    .wait_for(|stop| *stop)
                    .await
                    .expect("fixture shutdown requested");
            });
        let mut tasks = JoinSet::new();
        tasks.spawn(server);
        timeout(Duration::from_secs(10), async {
            let mut client = FirmwareSupervisorClient::connect(&socket).await.unwrap();
            let profile: FirmwareFlasherProfile = checked_submission(&supervisor, "new")
                .profile
                .unwrap()
                .try_into()
                .unwrap();
            let context = FirmwareUpdateContext {
                phase_id: "dispatch-1".into(),
                expected_digests: FirmwareArtifactDigests {
                    image_sha256: [0; 32],
                    configuration_sha256: None,
                },
                expected_device: None,
            };
            let accepted = client
                .submit_update(
                    "new",
                    profile.clone(),
                    context.clone(),
                    ObserverExitBehavior::Continue,
                )
                .await
                .expect("admit checked work");
            assert_eq!(accepted.state, FirmwareOperationState::Accepted);
            let queued = receiver.recv().await.expect("checked work reaches queue");
            assert_eq!(queued.operation_id, "new");
            let queued_profile = queued
                .profile
                .clone()
                .map(rpc::protos::mlx_device::FirmwareFlasherProfile::from);
            assert_eq!(queued_profile, Some(profile.clone().into()));
            let queued_context = supervisor
                .journal
                .lock()
                .unwrap()
                .record(&queued.operation_id)
                .unwrap()
                .unwrap()
                .requested
                .as_ref()
                .unwrap()
                .update_context
                .clone();
            assert_eq!(queued_context, Some(context.clone()));

            // Check committed bytes independently of the service cache.
            // Leave the admitted firmware work unexecuted.
            let journal_path = supervisor.directory.join("operations.json");
            let saved = fs::read(&journal_path).unwrap();
            let ledger: serde_json::Value = serde_json::from_slice(&saved).unwrap();
            let records: Vec<JournalRecord> =
                serde_json::from_value(ledger["records"].clone()).unwrap();
            let persisted = records
                .iter()
                .find(|record| record.operation.operation_id == queued.operation_id)
                .expect("admitted checked work is durable");
            assert_eq!(persisted.operation, accepted);
            assert_eq!(
                persisted.requested.as_ref().unwrap().update_context,
                Some(context.clone())
            );

            // The public client must transmit the requested observer policy;
            // otherwise this conflicting lookup would return the saved receipt.
            assert!(matches!(
                client
                    .observe_update("new", profile, context, ObserverExitBehavior::Terminate)
                    .await,
                Err(FirmwareSupervisorClientError::Rpc(status))
                    if status.code() == tonic::Code::AlreadyExists
            ));
            assert_eq!(fs::read(&journal_path).unwrap(), saved);
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            drop(client);
            stop.send(true).expect("request fixture shutdown");
            tasks.join_next().await.unwrap().unwrap().unwrap();
        })
        .await
        .expect("checked submission test finishes within watchdog");
    }

    /// The file may already contain an admission or cancellation when its
    /// directory sync fails. Every receipt read must wait for a reopen.
    #[tokio::test]
    async fn uncertain_rpc_saves_fence_receipt_reads_until_reopen() {
        for cancel_existing in [false, true] {
            let (root, supervisor, mut receiver) = supervisor_fixture();
            supervisor
                .submit_firmware_operation(peer_request(submission(&supervisor, "completed")))
                .await
                .unwrap();
            supervisor
                .execute(receiver.recv().await.unwrap())
                .await
                .unwrap();
            let work = if cancel_existing {
                supervisor
                    .submit_firmware_operation(peer_request(submission(&supervisor, "uncertain")))
                    .await
                    .unwrap();
                Some(receiver.recv().await.unwrap())
            } else {
                None
            };
            supervisor.journal.lock().unwrap().fail_directory_sync();
            let error = if cancel_existing {
                supervisor
                    .cancel_firmware_operation(peer_request(CancelFirmwareOperationRequest {
                        operation_id: "uncertain".into(),
                        expected_identity: Some(supervisor.identity.clone()),
                    }))
                    .await
                    .unwrap_err()
            } else {
                supervisor
                    .submit_firmware_operation(peer_request(submission(&supervisor, "uncertain")))
                    .await
                    .unwrap_err()
            };
            assert_eq!(error.code(), tonic::Code::Internal);
            let saved = fs::read(supervisor.directory.join("operations.json")).unwrap();
            let ledger: serde_json::Value = serde_json::from_slice(&saved).unwrap();
            assert_eq!(
                ledger["records"][1]["operation"]["state"],
                if cancel_existing {
                    "CancelRequested"
                } else {
                    "Accepted"
                }
            );
            assert!(root.path().join("mutation.json").exists());
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            assert!(supervisor.canceled.borrow().is_none());

            assert_eq!(
                supervisor
                    .get_firmware_operation(peer_request(GetFirmwareOperationRequest {
                        operation_id: "uncertain".into()
                    }))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::Unavailable
            );
            // After an uncertain save, even an absent ID cannot establish
            // permission for new admission from the fenced cache.
            assert_eq!(
                supervisor
                    .observe_firmware_update(peer_request(checked_observation(checked_submission(
                        &supervisor,
                        "missing"
                    ))))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::Unavailable
            );
            // Duplicate submission and terminal cancellation otherwise return
            // directly from the cached receipt, without attempting a save.
            assert_eq!(
                supervisor
                    .submit_firmware_operation(peer_request(submission(&supervisor, "completed")))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::Unavailable
            );
            assert_eq!(
                supervisor
                    .cancel_firmware_operation(peer_request(CancelFirmwareOperationRequest {
                        operation_id: "completed".into(),
                        expected_identity: Some(supervisor.identity.clone()),
                    }))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::Unavailable
            );
            assert_eq!(
                fs::read(supervisor.directory.join("operations.json")).unwrap(),
                saved
            );
            let directory = supervisor.directory.clone();
            drop(work);
            drop(supervisor);
            let reopened = OperationJournal::open(&directory, "0000:01:00.0", "boot").unwrap();
            assert_eq!(
                reopened.snapshot("uncertain").unwrap().unwrap().state,
                FirmwareOperationState::Interrupted
            );
            assert!(root.path().join("mutation.json").exists());
            assert!(reopened.snapshot("missing").unwrap().is_none());
        }
    }

    /// A closed executor cannot undo durable admission. Its receipt and claim
    /// remain available for recovery, and retrying the ID never queues work.
    #[tokio::test]
    async fn closed_executor_retains_admission_without_replay() {
        let (root, supervisor, mut receiver) = supervisor_fixture();
        receiver.close();
        let request = submission(&supervisor, "unstarted");
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(request.clone()))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::Unavailable
        );
        let saved = fs::read(supervisor.directory.join("operations.json")).unwrap();
        let repeated = supervisor
            .submit_firmware_operation(peer_request(request))
            .await
            .unwrap()
            .into_inner()
            .operation
            .unwrap();
        let expected: rpc::protos::mlx_device::FirmwareOperation =
            supervisor.snapshot("unstarted").await.unwrap().into();
        assert_eq!(repeated, expected);
        assert_eq!(
            fs::read(supervisor.directory.join("operations.json")).unwrap(),
            saved
        );
        assert!(receiver.recv().await.is_none());
        assert!(root.path().join("mutation.json").exists());
        let directory = supervisor.directory.clone();
        drop(supervisor);
        let reopened = OperationJournal::open(&directory, "0000:01:00.0", "boot").unwrap();
        assert_eq!(
            reopened.snapshot("unstarted").unwrap().unwrap().state,
            FirmwareOperationState::Interrupted
        );
    }

    /// Receipt capacity needs operator maintenance, not a retry. Reject before
    /// acquiring exclusion for new work.
    #[tokio::test]
    async fn receipt_capacity_rejects_submission_without_claiming_host() {
        let (root, mut supervisor, mut receiver) = supervisor_fixture();
        let path = supervisor.directory.join("operations.json");
        let mut ledger: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let record = JournalRecord {
            operation: FirmwareOperation {
                operation_id: "completed".into(),
                state: FirmwareOperationState::Complete,
                phase: None,
                report: Default::default(),
                diagnostic: None,
            },
            request_sha256: [1; 32],
            boot_id: "boot".into(),
            requested: None,
            observer_exit_behavior: ObserverExitBehavior::Continue,
            prepared_inputs: None,
            mutation_started: false,
        };
        ledger["records"] = serde_json::Value::Array(
            (0..128)
                .map(|index| {
                    let mut record = record.clone();
                    record.operation.operation_id = format!("completed-{index}");
                    serde_json::to_value(record).unwrap()
                })
                .collect(),
        );
        let saved = serde_json::to_vec(&ledger).unwrap();
        fs::write(&path, &saved).unwrap();
        drop(supervisor.journal);
        supervisor.journal = Arc::new(Mutex::new(
            OperationJournal::open(&supervisor.directory, "0000:01:00.0", "boot").unwrap(),
        ));
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(submission(&supervisor, "next")))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::FailedPrecondition
        );
        assert_eq!(fs::read(path).unwrap(), saved);
        assert!(!root.path().join("mutation.json").exists());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    /// Raw RPC input is rejected before it can create a receipt or host claim.
    #[tokio::test]
    async fn invalid_submission_fields_leave_no_admission() {
        for (scenario, operation_id, policy) in [
            (
                "empty ID",
                String::new(),
                wire::ObserverExitBehavior::Continue as i32,
            ),
            (
                "space in ID",
                "has space".into(),
                wire::ObserverExitBehavior::Continue as i32,
            ),
            (
                "non-ASCII ID",
                "café".into(),
                wire::ObserverExitBehavior::Continue as i32,
            ),
            (
                "control byte in ID",
                "has\nnewline".into(),
                wire::ObserverExitBehavior::Continue as i32,
            ),
            (
                "oversized ID",
                "x".repeat(257),
                wire::ObserverExitBehavior::Continue as i32,
            ),
            ("unknown observer policy", "operation".into(), i32::MAX),
        ] {
            let (root, supervisor, mut receiver) = supervisor_fixture();
            let path = supervisor.directory.join("operations.json");
            let saved = fs::read(&path).unwrap();
            let request = SubmitFirmwareOperationRequest {
                operation_id,
                observer_exit_behavior: policy,
                ..submission(&supervisor, "unused")
            };
            assert_eq!(
                supervisor
                    .submit_firmware_operation(peer_request(request))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::InvalidArgument,
                "{scenario}"
            );
            assert_eq!(fs::read(path).unwrap(), saved, "{scenario}");
            assert!(!root.path().join("mutation.json").exists(), "{scenario}");
            assert!(
                matches!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "{scenario}"
            );
        }
    }

    /// Only the observer-exit policy needs a pidfd; `Continue` remains usable
    /// on kernels where the peer supplied no process descriptor.
    #[tokio::test]
    async fn missing_peer_pidfd_only_rejects_terminate_policy() {
        for (policy, operation_id) in [
            (
                wire::ObserverExitBehavior::Terminate,
                "terminate".to_string(),
            ),
            (wire::ObserverExitBehavior::Continue, "x".repeat(256)),
        ] {
            let (root, supervisor, mut receiver) = supervisor_fixture();
            let path = supervisor.directory.join("operations.json");
            let saved = fs::read(&path).unwrap();
            let request = SubmitFirmwareOperationRequest {
                observer_exit_behavior: policy.into(),
                ..submission(&supervisor, &operation_id)
            };
            let result = supervisor
                .submit_firmware_operation(peer_request(request))
                .await;
            if policy == wire::ObserverExitBehavior::Terminate {
                assert_eq!(result.unwrap_err().code(), tonic::Code::FailedPrecondition);
                assert_eq!(fs::read(path).unwrap(), saved);
                assert!(!root.path().join("mutation.json").exists());
                assert!(matches!(
                    receiver.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
            } else {
                assert_eq!(
                    result.unwrap().into_inner().operation.unwrap().operation_id,
                    operation_id
                );
                let work = receiver.recv().await.unwrap();
                assert!(work.observer.is_none());
                supervisor.execute(work).await.unwrap();
                assert!(!root.path().join("mutation.json").exists());
            }
        }
    }

    /// Invalid options must identify the field before duplicate lookup, without
    /// changing the accepted request, its exclusion, or the executor queue.
    #[tokio::test]
    async fn invalid_profile_options_are_rejected_before_duplicate_lookup() {
        let (root, supervisor, mut receiver) = supervisor_fixture();
        let request = firmware_submission(&supervisor, "operation");
        assert_eq!(
            request
                .profile
                .as_ref()
                .unwrap()
                .flash_options
                .as_ref()
                .unwrap()
                .reset_level,
            3
        );
        supervisor
            .submit_firmware_operation(peer_request(request.clone()))
            .await
            .unwrap();
        let work = receiver.recv().await.unwrap();
        let path = supervisor.directory.join("operations.json");
        let saved = fs::read(&path).unwrap();
        let claim_path = root.path().join("mutation.json");
        let saved_claim = fs::read(&claim_path).unwrap();
        for (name, reset_level, retry_interval, message) in [
            // Truncating 259 to u8 would alias the accepted reset level of 3.
            (
                "overflowing reset level",
                259,
                None,
                "reset_level must be in 0..=255",
            ),
            // Timing validation also applies when the optional phases are off.
            (
                "zero retry interval",
                3,
                Some(0),
                "configuration error: post_reset_verify_retry_interval_seconds must be in 1..=1800 seconds",
            ),
        ] {
            let mut invalid = request.clone();
            let options = invalid
                .profile
                .as_mut()
                .unwrap()
                .flash_options
                .as_mut()
                .unwrap();
            options.reset_level = reset_level;
            options.post_reset_verify_retry_interval_seconds = retry_interval;
            let error = supervisor
                .submit_firmware_operation(peer_request(invalid))
                .await
                .unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument, "{name}");
            assert_eq!(error.message(), message, "{name}");
            assert_eq!(fs::read(&path).unwrap(), saved, "{name}");
            assert_eq!(fs::read(&claim_path).unwrap(), saved_claim, "{name}");
            assert!(
                matches!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "{name}"
            );
        }
        drop(work);
    }

    /// A failed claim creation cannot admit work into the independent journal.
    #[tokio::test]
    async fn claim_creation_failure_preserves_empty_journal() {
        let (root, mut supervisor, mut receiver) = supervisor_fixture();
        supervisor.root = root.path().join("file-in-place-of-root");
        fs::write(&supervisor.root, b"preserve").unwrap();
        let path = supervisor.directory.join("operations.json");
        let saved = fs::read(&path).unwrap();
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(submission(&supervisor, "operation")))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::Internal
        );
        assert_eq!(fs::read(path).unwrap(), saved);
        assert_eq!(fs::read(&supervisor.root).unwrap(), b"preserve");
        assert!(
            supervisor
                .journal
                .lock()
                .unwrap()
                .snapshot("operation")
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    /// Drop an RPC only after its first poll has handed work to the blocking
    /// pool, leaving the real journal mutex to control completion.
    async fn drop_pending_request(request: impl Future) {
        tokio::pin!(request);
        poll_fn(|context| {
            assert!(request.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    /// An idle socket still owns transport capacity; rejecting another socket
    /// must not keep a waiter or prevent replacement after the first closes.
    #[tokio::test]
    async fn connection_capacity_is_released_with_the_transport() {
        let connections = Arc::new(Semaphore::new(1));
        let (first, _first_peer) = UnixStream::pair().expect("create first socket pair");
        let accepted = accept_connection(Ok(first), &connections)
            .expect("first connection fits")
            .expect("capture first peer credentials");
        assert_eq!(connections.available_permits(), 0);

        let (excess, _excess_peer) = UnixStream::pair().expect("create excess socket pair");
        assert!(accept_connection(Ok(excess), &connections).is_none());
        drop(accepted);
        assert_eq!(connections.available_permits(), 1);

        let (replacement, _replacement_peer) = UnixStream::pair().expect("create replacement pair");
        let replacement = accept_connection(Ok(replacement), &connections)
            .expect("closed transport released capacity")
            .expect("capture replacement peer credentials");
        drop(replacement);
        assert_eq!(connections.available_permits(), 1);
    }

    /// Disconnected callers still consume the shared journal quota until their
    /// blocking work ends; handshake and internal cancellation remain available.
    #[tokio::test]
    async fn dropped_journal_rpcs_retain_capacity_until_blocking_work_finishes() {
        const WATCHDOG: Duration = Duration::from_secs(10);
        let (_root, mut supervisor, mut receiver) = supervisor_fixture();
        // Saturate lookup, cancel, and submit without depending on the
        // production cap. Checked observation shares lookup's `find_operation`.
        supervisor.journal_rpc_tasks = Arc::new(Semaphore::new(3));
        supervisor
            .submit_firmware_operation(peer_request(submission(&supervisor, "saved")))
            .await
            .expect("admit fixture operation");
        supervisor
            .execute(receiver.recv().await.expect("receive fixture work"))
            .await
            .expect("complete fixture without hardware work");

        let journal = supervisor.journal.clone();
        let (locked, ready) = oneshot::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let blocker = spawn_blocking(move || {
            let _journal = journal.lock().expect("lock fixture journal");
            locked.send(()).expect("report paused journal");
            resume.recv_timeout(WATCHDOG).expect("resume journal");
        });
        timeout(WATCHDOG, ready)
            .await
            .expect("pause journal within watchdog")
            .expect("journal lock acquired");

        drop_pending_request(supervisor.snapshot("saved")).await;
        drop_pending_request(supervisor.cancel_firmware_operation(peer_request(
            CancelFirmwareOperationRequest {
                operation_id: "saved".into(),
                expected_identity: Some(supervisor.identity.clone()),
            },
        )))
        .await;
        drop_pending_request(
            supervisor.submit_firmware_operation(peer_request(submission(&supervisor, "saved"))),
        )
        .await;
        assert_eq!(supervisor.journal_rpc_tasks.available_permits(), 0);

        // All journal RPCs reject immediately instead of creating another
        // mutex waiter, even though their original RPC futures are gone.
        assert_eq!(
            supervisor.snapshot("saved").await.unwrap_err().code(),
            tonic::Code::ResourceExhausted,
        );
        assert_eq!(
            supervisor
                .cancel_firmware_operation(peer_request(CancelFirmwareOperationRequest {
                    operation_id: "saved".into(),
                    expected_identity: Some(supervisor.identity.clone()),
                }))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::ResourceExhausted,
        );
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(submission(&supervisor, "saved")))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::ResourceExhausted,
        );
        assert_eq!(
            supervisor
                .observe_firmware_update(peer_request(checked_observation(checked_submission(
                    &supervisor,
                    "checked",
                ))))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::ResourceExhausted,
        );
        assert_eq!(
            supervisor
                .get_supervisor_status(peer_request(GetSupervisorStatusRequest {}))
                .await
                .expect("handshake does not require journal capacity")
                .into_inner()
                .device_identity,
            supervisor.identity.device_identity,
        );

        // Observer exit must still request a stop when RPC clients fill their
        // quota. Its owner waits for persistence independently of that quota.
        let internal_cancel = supervisor.cancel("saved", None);
        tokio::pin!(internal_cancel);
        poll_fn(|context| {
            assert!(internal_cancel.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        release.send(()).expect("release fixture journal");
        timeout(WATCHDOG, blocker)
            .await
            .expect("release journal within watchdog")
            .expect("journal blocker finished");
        assert_eq!(
            timeout(WATCHDOG, internal_cancel)
                .await
                .expect("internal cancellation completes")
                .expect("read settled operation")
                .state,
            FirmwareOperationState::Complete,
        );
        timeout(WATCHDOG, async {
            while supervisor.journal_rpc_tasks.available_permits() != 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached journal work releases all capacity");
        assert_eq!(
            supervisor.snapshot("saved").await.unwrap().state,
            FirmwareOperationState::Complete,
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    /// Submit, cancel, and checked observation require authentication and the
    /// pinned handshake before changing or disclosing journal state.
    #[tokio::test]
    async fn submit_cancel_and_observe_reject_missing_or_stale_identity() {
        let (_root, supervisor, mut receiver) = supervisor_fixture();
        supervisor
            .submit_firmware_operation(peer_request(submission(&supervisor, "saved")))
            .await
            .expect("admit fixture operation");
        let accepted_work = receiver
            .recv()
            .await
            .expect("remove accepted work from queue");
        let before = fs::read(supervisor.directory.join("operations.json")).unwrap();
        let mut stale_identity = supervisor.identity.clone();
        stale_identity.boot_id = "previous-boot".into();
        let current_uid = nix::unistd::geteuid().as_raw();
        struct Case {
            label: &'static str,
            identity: Option<SupervisorIdentity>,
            peer_uid: Option<u32>,
            expected: tonic::Code,
        }
        for case in [
            // None of these methods permits a client to skip the handshake.
            Case {
                label: "missing handshake",
                identity: None,
                peer_uid: Some(current_uid),
                expected: tonic::Code::FailedPrecondition,
            },
            // A reconnected channel must not reuse an earlier helper identity.
            Case {
                label: "stale handshake",
                identity: Some(stale_identity),
                peer_uid: Some(current_uid),
                expected: tonic::Code::FailedPrecondition,
            },
            // Payload identity alone never substitutes for socket credentials.
            Case {
                label: "missing Unix peer",
                identity: Some(supervisor.identity.clone()),
                peer_uid: None,
                expected: tonic::Code::Unauthenticated,
            },
            // Another UID cannot use the ledger even with the right handshake.
            Case {
                label: "different Unix user",
                identity: Some(supervisor.identity.clone()),
                peer_uid: Some(current_uid.wrapping_add(1)),
                expected: tonic::Code::PermissionDenied,
            },
        ] {
            let peer = case.peer_uid.map(|uid| PeerInfo {
                uid,
                pidfd: None,
                pidfd_error: None,
            });
            let mut submit = Request::new(SubmitFirmwareOperationRequest {
                expected_identity: case.identity.clone(),
                ..submission(&supervisor, "rejected")
            });
            let mut observe = Request::new(checked_observation(SubmitFirmwareOperationRequest {
                expected_identity: case.identity.clone(),
                ..checked_submission(&supervisor, "rejected")
            }));
            let mut cancel = Request::new(CancelFirmwareOperationRequest {
                operation_id: "saved".into(),
                expected_identity: case.identity,
            });
            if let Some(peer) = peer {
                submit.extensions_mut().insert(peer.clone());
                observe.extensions_mut().insert(peer.clone());
                cancel.extensions_mut().insert(peer);
            }
            assert_eq!(
                supervisor
                    .submit_firmware_operation(submit)
                    .await
                    .unwrap_err()
                    .code(),
                case.expected,
                "{} submit",
                case.label,
            );
            assert_eq!(
                supervisor
                    .cancel_firmware_operation(cancel)
                    .await
                    .unwrap_err()
                    .code(),
                case.expected,
                "{} cancel",
                case.label,
            );
            assert_eq!(
                supervisor
                    .observe_firmware_update(observe)
                    .await
                    .unwrap_err()
                    .code(),
                case.expected,
                "{} observe",
                case.label,
            );
            assert_eq!(
                fs::read(supervisor.directory.join("operations.json")).unwrap(),
                before,
                "{} preserves the ledger",
                case.label,
            );
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            assert!(supervisor.canceled.borrow().is_none());
        }
        supervisor
            .execute(accepted_work)
            .await
            .expect("finish fixture work");
    }

    /// A known failure before any tool starts frees host admission, but retries
    /// of that same operation ID return its durable failure without execution.
    #[tokio::test]
    async fn failed_inspection_releases_claim_without_replaying_the_operation() {
        let (root, supervisor, mut receiver) = supervisor_fixture();
        let request = firmware_submission(&supervisor, "failed");
        supervisor
            .submit_firmware_operation(peer_request(request.clone()))
            .await
            .expect("admit inspect failure fixture");
        assert!(root.path().join("mutation.json").exists());
        supervisor
            .execute(receiver.recv().await.expect("receive failing work"))
            .await
            .expect("persist known launch failure");
        let failed = supervisor.snapshot("failed").await.unwrap();
        assert_eq!(failed.state, FirmwareOperationState::Failed);
        assert_eq!(
            failed.phase,
            Some(carbide_libmlx_model::firmware::operation::FirmwareOperationPhase::Inspect),
        );
        assert!(
            !supervisor
                .journal
                .lock()
                .expect("read failed receipt")
                .record("failed")
                .unwrap()
                .expect("saved failure")
                .mutation_started
        );
        assert!(!root.path().join("mutation.json").exists());

        let before = fs::read(supervisor.directory.join("operations.json")).unwrap();
        let repeated = supervisor
            .submit_firmware_operation(peer_request(request))
            .await
            .expect("retrieve failed receipt")
            .into_inner()
            .operation
            .expect("saved operation");
        let expected: rpc::protos::mlx_device::FirmwareOperation = failed.into();
        assert_eq!(repeated, expected);
        assert_eq!(
            fs::read(supervisor.directory.join("operations.json")).unwrap(),
            before
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        supervisor
            .submit_firmware_operation(peer_request(submission(&supervisor, "next")))
            .await
            .expect("new operation is eligible after settled failure");
        let next = receiver.recv().await.expect("new work reaches queue");
        assert_eq!(next.operation_id, "next");
        supervisor
            .execute(next)
            .await
            .expect("complete next operation");
        assert!(!root.path().join("mutation.json").exists());
    }

    /// A malformed helper response cannot establish process settlement, even
    /// during inspection. Preserve exclusion and checked identity through reopen.
    #[tokio::test]
    async fn malformed_helper_response_retains_claim_and_checked_receipt_without_replay() {
        use carbide_libmlx_model::firmware::operation::FirmwareOperationPhase;

        use crate::firmware::config::FirmwareArtifactDigests;

        let (root, mut supervisor, mut receiver) = supervisor_fixture();
        let helper = root.path().join("malformed-helper");
        let contents = format!(
            r#"#!/bin/sh
set -eu
printf '%s' '{hello}'
set -- $(/usr/bin/od -An -N4 -tu1)
length=$((($1 << 24) | ($2 << 16) | ($3 << 8) | $4))
/bin/dd bs=1 count="$length" status=none >> "$0.requests"
printf '\n' >> "$0.requests"
printf '\000\000\000\001x'
"#,
            hello = std::str::from_utf8(crate::runner::HELLO).unwrap(),
        );
        // A separate writer keeps writable executable descriptors out of
        // concurrent test forks. The helper records, but never runs, the tool.
        let status = timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "set -eu; umask 077; printf '%s' \"$2\" > \"$1\"",
                    "fixture-writer",
                ])
                .arg(&helper)
                .arg(contents)
                .kill_on_drop(true)
                .status(),
        )
        .await
        .expect("fixture writer completes within its watchdog")
        .expect("write malformed helper");
        assert!(status.success());
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        supervisor.executor = FirmwareToolExecutor::with_supervisor_path(helper);

        let request = checked_submission(&supervisor, "uncertain-inspection");
        supervisor
            .submit_firmware_operation(peer_request(request.clone()))
            .await
            .unwrap();
        let claim_path = root.path().join("mutation.json");
        let saved_claim = fs::read(&claim_path).unwrap();
        timeout(
            Duration::from_secs(10),
            supervisor.execute(receiver.recv().await.unwrap()),
        )
        .await
        .expect("malformed frame finishes within watchdog")
        .expect("persist uncertain inspection");
        let operation = supervisor.snapshot(&request.operation_id).await.unwrap();
        assert_eq!(operation.state, FirmwareOperationState::ManualIntervention);
        assert_eq!(operation.phase, Some(FirmwareOperationPhase::Inspect));
        assert!(!operation.report.flashed);
        let expected: rpc::protos::mlx_device::FirmwareOperation = operation.into();
        let ledger_path = supervisor.directory.join("operations.json");
        let saved = fs::read(&ledger_path).unwrap();
        assert_eq!(fs::read(&claim_path).unwrap(), saved_claim);

        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(request.clone()))
                .await
                .unwrap()
                .into_inner()
                .operation,
            Some(expected.clone())
        );
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(checked_submission(&supervisor, "next")))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::FailedPrecondition
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(fs::read(&ledger_path).unwrap(), saved);

        // Reopen the committed ledger, rather than relying on the live cache.
        // The checked context and uncertain-settlement state must survive
        // unchanged; neither duplicate nor new IDs may authorize work.
        drop(supervisor.journal);
        let reopened =
            OperationJournal::open(&supervisor.directory, "0000:01:00.0", "boot").unwrap();
        let record = reopened.record(&request.operation_id).unwrap().unwrap();
        assert!(!record.mutation_started);
        assert!(!record.is_safe_to_release());
        assert_eq!(
            record.requested.as_ref().unwrap().update_context,
            Some(FirmwareUpdateContext {
                phase_id: "dispatch-1".into(),
                expected_digests: FirmwareArtifactDigests {
                    image_sha256: [0; 32],
                    configuration_sha256: None,
                },
                expected_device: None,
            })
        );
        supervisor.journal = Arc::new(Mutex::new(reopened));
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(request))
                .await
                .unwrap()
                .into_inner()
                .operation,
            Some(expected)
        );
        assert_eq!(
            supervisor
                .submit_firmware_operation(peer_request(checked_submission(&supervisor, "next")))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::FailedPrecondition
        );
        assert_eq!(fs::read(&ledger_path).unwrap(), saved);
        assert_eq!(fs::read(&claim_path).unwrap(), saved_claim);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let requests = fs::read_to_string(root.path().join("malformed-helper.requests")).unwrap();
        assert_eq!(requests.lines().count(), 1);
        let request: crate::runner::Request = serde_json::from_str(requests.trim()).unwrap();
        assert_eq!(request.program, "mlxfwmanager");
        assert_eq!(
            request.args,
            ["--dev", "0000:01:00.0", "--query-format", "xml"]
        );
    }

    /// A dropped submit RPC must retain its admission guard until persistence
    /// and queueing finish, so graceful shutdown can still drain the work.
    #[tokio::test]
    async fn shutdown_waits_for_detached_admission_before_closing_work_queue() {
        const WATCHDOG: Duration = Duration::from_secs(10);
        let (root, supervisor, mut receiver) = supervisor_fixture();

        // Pause the real admission at its existing journal mutex without
        // blocking this single-thread runtime or adding a production fault hook.
        let journal = supervisor.journal.clone();
        let (locked, ready) = oneshot::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let blocker = spawn_blocking(move || {
            let _journal = journal.lock().expect("lock fixture journal");
            locked.send(()).expect("report paused journal");
            resume.recv_timeout(WATCHDOG).expect("resume journal");
        });
        timeout(WATCHDOG, ready)
            .await
            .expect("pause journal within watchdog")
            .expect("journal lock acquired");

        let request = peer_request(submission(&supervisor, "detached-admission"));
        let submitting = supervisor.clone();
        let submission =
            tokio::spawn(async move { submitting.submit_firmware_operation(request).await });
        timeout(WATCHDOG, async {
            loop {
                match supervisor.admissions.try_write() {
                    Ok(guard) => drop(guard),
                    Err(_) => break,
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("submission acquired its admission guard");
        submission.abort();
        assert!(submission.await.unwrap_err().is_cancelled());

        let closing = supervisor.clone();
        let shutdown = async move {
            closing.close_admissions(&mut receiver).await;
            receiver
        };
        tokio::pin!(shutdown);
        poll_fn(|context| {
            assert!(shutdown.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        release
            .send(())
            .expect("allow detached admission to finish");
        timeout(WATCHDOG, blocker)
            .await
            .expect("release journal within watchdog")
            .expect("journal blocker finished");
        let mut receiver = timeout(WATCHDOG, shutdown)
            .await
            .expect("shutdown observed admission completion");
        let work = receiver.recv().await.expect("accepted work reached queue");
        assert_eq!(work.operation_id, "detached-admission");
        assert!(receiver.recv().await.is_none());
        assert_eq!(
            supervisor
                .snapshot("detached-admission")
                .await
                .unwrap()
                .state,
            FirmwareOperationState::Accepted,
        );
        supervisor.execute(work).await.expect("drain admitted work");
        assert_eq!(
            supervisor
                .snapshot("detached-admission")
                .await
                .unwrap()
                .state,
            FirmwareOperationState::Complete,
        );
        assert!(!root.path().join("mutation.json").exists());
    }
}
