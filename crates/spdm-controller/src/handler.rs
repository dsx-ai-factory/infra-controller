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

use std::sync::Arc;

use carbide_instrument::{Event, LabelValue, Outcome, emit};
use carbide_redfish::libredfish::conv::IntoModel;
use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use itertools::Itertools;
use libredfish::Redfish;
use libredfish::model::task::TaskState;
use model::attestation::spdm::{
    DeviceType, SpdmAttestationState, SpdmDeviceAttestation, SpdmHandlerError,
    SpdmMachineDeviceMetadata, SpdmObjectId, Verifier,
};
use model::bmc_info::BmcInfo;
use nras::{DeviceAttestationInfo, EvidenceCertificate, RawAttestationOutcome, VerifierClient};
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SpdmStateHandlerContextObjects;

// Unexpected nonterminal tasks get two minutes from entering PollEvidenceCollection.
// Cancellation cleanup then has its own configurable grace period.
const EVIDENCE_COLLECTION_POLL_TIMEOUT: chrono::Duration = chrono::Duration::minutes(2);

#[derive(Debug, Clone)]
pub struct SpdmAttestationStateHandler {
    verifier: Arc<dyn Verifier>,
    nras_config: nras::Config,
    cancellation_timeout: chrono::Duration,
}

impl SpdmAttestationStateHandler {
    /// Creates a handler with a cleanup timeout measured from persisted cancellation times.
    pub fn new(
        verifier: Arc<dyn Verifier>,
        nras_config: nras::Config,
        cancellation_timeout: chrono::Duration,
    ) -> Self {
        Self {
            verifier,
            nras_config,
            cancellation_timeout,
        }
    }

    async fn handle_cancellation(
        &self,
        object_id: &SpdmObjectId,
        snapshot: &SpdmDeviceAttestation,
        state: &SpdmAttestationState,
        ctx: &mut StateHandlerContext<'_, SpdmStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<SpdmAttestationState>, StateHandlerError> {
        if snapshot.evidence_collection_locked
            && let Some(cancelled_at) = snapshot.cancelled_at
        {
            // Keep the original deadline across retries and controller restarts.
            if chrono::Utc::now() - cancelled_at < self.cancellation_timeout {
                let SpdmAttestationState::PollEvidenceCollection {
                    task_id,
                    retry_count,
                } = state
                else {
                    // An ambiguous collection POST can hold the lock without a task ID.
                    return Ok(StateHandlerOutcome::wait(
                        "Cancellation has no BMC task ID; waiting for the cancellation timeout to release the evidence collection lock"
                            .to_string(),
                    ));
                };
                // The waiting state records that cancellation was already requested;
                // a transient request failure remains here for the next iteration.
                let client = redfish_client(&snapshot.bmc_info, ctx).await?;
                let task = client
                    .get_task(task_id)
                    .await
                    .map_err(|error| redfish_error("get_task_state", error))?;
                if evidence_collection_task_stopped(task.task_state.as_ref()) {
                    return self.complete_cancellation(object_id, state, ctx).await;
                }
                // if cancelling is immediate, then move into Cancelled state,
                // otherwise move into WaitForTaskCancellation and poll there
                match client.cancel_task(task_id).await {
                    Ok(true) => return self.complete_cancellation(object_id, state, ctx).await,
                    Ok(false) => {}
                    Err(libredfish::RedfishError::NotSupported(_)) => {
                        tracing::warn!(%object_id, %task_id, "BMC task cancellation is unsupported; waiting for collection to stop");
                    }
                    Err(error) => return Err(redfish_error("cancel_task", error)),
                }
                return Ok(StateHandlerOutcome::transition(
                    SpdmAttestationState::WaitForTaskCancellation {
                        task_id: task_id.clone(),
                        retry_count: *retry_count,
                    },
                ));
            } else {
                // This is an explicit best-effort cutoff, not proof that the task stopped.
                tracing::warn!(%object_id, "SPDM cancellation timed out; releasing the evidence collection lock without BMC confirmation");
            }
        }

        self.complete_cancellation(object_id, state, ctx).await
    }

    async fn complete_cancellation(
        &self,
        object_id: &SpdmObjectId,
        state: &SpdmAttestationState,
        ctx: &mut StateHandlerContext<'_, SpdmStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<SpdmAttestationState>, StateHandlerError> {
        // Completion and lock release must be committed together, including the timeout path.
        let mut txn = ctx.services.db_pool.begin().await?;
        db::attestation::spdm::release_evidence_collection_lock(&mut txn, object_id).await?;
        db::attestation::spdm::set_completed_at(&mut txn, &object_id.0, &object_id.1, &object_id.2)
            .await?;
        let outcome = if state == &SpdmAttestationState::Cancelled {
            StateHandlerOutcome::do_nothing()
        } else {
            StateHandlerOutcome::transition(SpdmAttestationState::Cancelled)
        };
        Ok(outcome.with_txn(txn))
    }

    fn record_metrics(
        &self,
        _state: &mut SpdmDeviceAttestation,
        _ctx: &mut StateHandlerContext<SpdmStateHandlerContextObjects>,
    ) {
    }
}

async fn redfish_client(
    bmc_info: &BmcInfo,
    ctx: &mut StateHandlerContext<'_, SpdmStateHandlerContextObjects>,
) -> Result<Box<dyn Redfish>, StateHandlerError> {
    let ip_addr = bmc_info
        .ip_addr()
        .map_err(StateHandlerError::GenericError)?;
    let bmc_access_info = db::machine_interface::lookup_bmc_access_info(
        &ctx.services.db_pool,
        ip_addr,
        bmc_info.port,
    )
    .await?;

    ctx.services
        .redfish_client_pool
        .client_by_info(&bmc_access_info)
        .await
        .map_err(StateHandlerError::from)
}

#[async_trait::async_trait]
impl StateHandler for SpdmAttestationStateHandler {
    type ObjectId = SpdmObjectId;
    type State = SpdmDeviceAttestation;
    type ControllerState = SpdmAttestationState;
    type ContextObjects = SpdmStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        object_id: &Self::ObjectId,
        snapshot: &mut SpdmDeviceAttestation,
        original_controller_state: &SpdmAttestationState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<SpdmAttestationState>, StateHandlerError> {
        // record metrics irrespective of the state of the machine
        self.record_metrics(snapshot, ctx);

        if snapshot.cancelled_at.is_some()
            && !matches!(
                original_controller_state,
                SpdmAttestationState::WaitForTaskCancellation { .. }
            )
        {
            return self
                .handle_cancellation(object_id, snapshot, original_controller_state, ctx)
                .await;
        }

        let SpdmObjectId(machine_id, device_id, job_id) = object_id;

        match original_controller_state {
            SpdmAttestationState::FetchMetadata => {
                let redfish_client = redfish_client(&snapshot.bmc_info, ctx).await?;

                let firmware_version = match redfish_client
                    .get_firmware_for_component(device_id)
                    .await
                {
                    Ok(x) => x.version,
                    Err(libredfish::RedfishError::NotSupported(msg)) => {
                        tracing::info!(
                            device_id = %device_id,
                            machine_id = %machine_id,
                            reason = %msg,
                            "device attestation is not supported because firmware version is unavailable"
                        );
                        return Ok(StateHandlerOutcome::transition(
                            SpdmAttestationState::Passed,
                        ));
                    }
                    Err(error) => {
                        return Err(redfish_error("fetch firmware version", error));
                    }
                };

                let metadata = SpdmMachineDeviceMetadata { firmware_version };
                let mut txn = ctx.services.db_pool.begin().await?;
                db::attestation::spdm::update_metadata(
                    &mut txn, machine_id, device_id, job_id, &metadata,
                )
                .await?;
                Ok(
                    StateHandlerOutcome::transition(SpdmAttestationState::FetchCertificate)
                        .with_txn(txn),
                )
            }
            SpdmAttestationState::FetchCertificate => {
                let redfish_client = redfish_client(&snapshot.bmc_info, ctx).await?;
                let Some(url) = &snapshot.ca_certificate_link else {
                    // This is an unrecoverable error due to db discrepancy.
                    return Ok(StateHandlerOutcome::transition(
                        SpdmAttestationState::Failed(
                            "Could not get ca_certificate_link from DB".to_string(),
                        ),
                    ));
                };
                let ca_certificate = redfish_client
                    .get_component_ca_certificate(url.as_str())
                    .await
                    .map_err(|error| redfish_error("fetch certificate", error))?;

                let mut txn = ctx.services.db_pool.begin().await?;
                db::attestation::spdm::update_certificate(
                    &mut txn,
                    &object_id.0,
                    device_id,
                    job_id,
                    &ca_certificate.into_model(),
                )
                .await?;
                Ok(StateHandlerOutcome::transition(
                    SpdmAttestationState::TriggerEvidenceCollection {
                        retry_count: 0,
                        redfish_retry_count: 0,
                        last_redfish_failure_tstamp: None,
                    },
                )
                .with_txn(txn))
            }
            SpdmAttestationState::TriggerEvidenceCollection {
                retry_count,
                redfish_retry_count,
                last_redfish_failure_tstamp,
            } => {
                // if last_redfish_failure_tstamp is set - we are retrying due to redfish error
                // - if retry limit exceeded - move to failed
                // - if we haven't waited a full minute yet, return waiting
                // - otherwise let's do the retry, but DON'T acquire the lock as it
                //   is already acquired

                if let Some(last_failure_time) = last_redfish_failure_tstamp {
                    if *redfish_retry_count > 5 {
                        // that's it, we give up, release the lock and move to Failed
                        let mut txn = ctx.services.db_pool.begin().await?;
                        db::attestation::spdm::release_evidence_collection_lock(
                            &mut txn, object_id,
                        )
                        .await?;
                        return Ok(
                            StateHandlerOutcome::transition(SpdmAttestationState::Failed(
                                "Exceeded redfish retry limit while attempting to collect evidence"
                                    .to_string(),
                            ))
                            .with_txn(txn),
                        );
                    }
                    if chrono::Utc::now() < *last_failure_time + chrono::Duration::minutes(1) {
                        return Ok(StateHandlerOutcome::wait(
                        "Collection evidence via redfish has failed, waiting up to one minute to try again"
                            .to_string(),
                    ));
                    }
                }

                // firmware version and certificate are collected. Let's trigger the
                // measurement collection now.
                let redfish_client = redfish_client(&snapshot.bmc_info, ctx).await?;
                let Some(url) = &snapshot.evidence_target else {
                    // This is an unrecoverable error due to db discrepancy.
                    return Ok(StateHandlerOutcome::transition(
                        SpdmAttestationState::Failed(
                            "Could not get evidence_target from DB".to_string(),
                        ),
                    ));
                };
                // only try and obtain the lock if we don't already hold it
                if !snapshot.evidence_collection_locked {
                    let mut txn = ctx.services.db_pool.begin().await?;
                    if !db::attestation::spdm::try_acquire_evidence_collection_lock(
                        &mut txn,
                        object_id,
                        snapshot.state_version,
                    )
                    .await?
                    {
                        return Ok(StateHandlerOutcome::wait(
                            "Evidence collection claim is unavailable or attestation state changed"
                                .to_string(),
                        ));
                    }
                    txn.commit().await?;
                }
                // Ownership must survive both this transaction and a process
                // restart, including an ambiguous failure of the following POST.

                let nonce = snapshot.nonce_hex();
                match redfish_client
                    .trigger_evidence_collection(url.as_str(), nonce.as_str())
                    .await
                    .map_err(|error| redfish_error("trigger measurement collection", error))
                {
                    Ok(task) => Ok(StateHandlerOutcome::transition(
                        SpdmAttestationState::PollEvidenceCollection {
                            task_id: task.id,
                            retry_count: *retry_count,
                        },
                    )),
                    Err(error) => {
                        tracing::warn!(
                            %machine_id,
                            %device_id,
                            %job_id,
                            error = %error,
                            retry_count = *retry_count,
                            redfish_retry_count = *redfish_retry_count + 1,
                            "Failed to trigger SPDM evidence collection; scheduling retry"
                        );
                        Ok(StateHandlerOutcome::transition(
                            SpdmAttestationState::TriggerEvidenceCollection {
                                retry_count: *retry_count,
                                redfish_retry_count: redfish_retry_count + 1,
                                last_redfish_failure_tstamp: Some(chrono::Utc::now()),
                            },
                        ))
                    }
                }
            }
            SpdmAttestationState::PollEvidenceCollection {
                task_id,
                retry_count,
                ..
            } => {
                if !snapshot.evidence_collection_locked {
                    return Err(StateHandlerError::InvalidState(
                        "Cannot collect evidence without owning the device's evidence collection claim"
                            .to_string(),
                    ));
                }
                let redfish_client = redfish_client(&snapshot.bmc_info, ctx).await?;
                let task = match redfish_client.get_task(task_id).await {
                    Ok(task) => task,
                    Err(libredfish::RedfishError::HTTPErrorCode { status_code, .. })
                        if matches!(status_code.as_u16(), 404 | 410) =>
                    {
                        // Cancellation may finish before its state transition is persisted.
                        // Reconcile the missing task through the bounded cleanup state.
                        tracing::warn!(%object_id, %task_id, status_code = status_code.as_u16(), "BMC evidence collection task disappeared; entering cancellation cleanup");
                        return Ok(StateHandlerOutcome::transition(
                            SpdmAttestationState::WaitForTaskCancellation {
                                task_id: task_id.clone(),
                                retry_count: *retry_count,
                            },
                        ));
                    }
                    Err(error) => return Err(redfish_error("get_task_state", error)),
                };
                let task_stopped = evidence_collection_task_stopped(task.task_state.as_ref());

                match classify_evidence_collection_task_state(task.task_state.as_ref()) {
                    EvidenceCollectionTaskStateClass::Completed => {
                        // read the result
                        let Some(url) = &snapshot.evidence_target else {
                            // This is an unrecoverable error due to db discrepancy.
                            let mut txn = ctx.services.db_pool.begin().await?;
                            db::attestation::spdm::release_evidence_collection_lock(
                                &mut txn, object_id,
                            )
                            .await?;
                            return Ok(StateHandlerOutcome::transition(
                                SpdmAttestationState::Failed(
                                    "Could not get evidence target from DB".to_string(),
                                ),
                            )
                            .with_txn(txn));
                        };
                        let evidence = redfish_client
                            .get_evidence(url)
                            .await
                            .map_err(|e| redfish_error("get_task_state", e))?;
                        let mut txn = ctx.services.db_pool.begin().await?;
                        db::attestation::spdm::update_evidence(
                            &mut txn,
                            &object_id.0,
                            device_id,
                            job_id,
                            &evidence.into_model(),
                        )
                        .await?;
                        db::attestation::spdm::release_evidence_collection_lock(
                            &mut txn, object_id,
                        )
                        .await?;
                        Ok(
                            StateHandlerOutcome::transition(SpdmAttestationState::NrasVerification)
                                .with_txn(txn),
                        )
                    }
                    EvidenceCollectionTaskStateClass::InProgress => {
                        Ok(StateHandlerOutcome::wait(format!(
                            "Measurement collection is pending {}%",
                            task.percent_complete.unwrap_or_default(),
                        )))
                    }
                    EvidenceCollectionTaskStateClass::Unexpected(task_state) => {
                        // Only confirmed termination permits another collection. Request
                        // cancellation here before entering the separate polling state.
                        let time_in_state = snapshot.state_version.since_state_change();
                        let next_action = if task_stopped && *retry_count > 4 {
                            EvidenceCollectionNextAction::Fail
                        } else if task_stopped {
                            EvidenceCollectionNextAction::Retry
                        } else if time_in_state >= EVIDENCE_COLLECTION_POLL_TIMEOUT {
                            EvidenceCollectionNextAction::Cancel
                        } else {
                            EvidenceCollectionNextAction::Wait
                        };

                        emit(SpdmEvidenceCollectionTaskStateUnexpected {
                            task_state,
                            next_action,
                            machine_id: machine_id.to_string(),
                            device_id: device_id.clone(),
                            task_id: task_id.clone(),
                            retry_count: *retry_count,
                            error: task.messages.iter().map(|t| t.message.as_str()).join("\n"),
                        });

                        if next_action == EvidenceCollectionNextAction::Wait {
                            return Ok(StateHandlerOutcome::wait(
                                "Waiting for BMC evidence collection to stop before retrying"
                                    .to_string(),
                            ));
                        }
                        if next_action == EvidenceCollectionNextAction::Cancel {
                            // has the task cancellation happen immediately?
                            let is_stopped = match redfish_client.cancel_task(task_id).await {
                                Ok(stopped) => stopped,
                                Err(libredfish::RedfishError::NotSupported(_)) => {
                                    tracing::warn!(%object_id, %task_id, "BMC task cancellation is unsupported; waiting for collection to stop");
                                    false
                                }
                                Err(error) => {
                                    // The task may still be running despite this error. Preserve the
                                    // cleanup grace and claim instead of failing and releasing it early.
                                    tracing::warn!(%object_id, %task_id, error = %error, "BMC task cancellation failed; waiting for collection to stop");
                                    false
                                }
                            };
                            // task cancellation did not happen immediately, so need to wait and poll
                            if !is_stopped {
                                return Ok(StateHandlerOutcome::transition(
                                    SpdmAttestationState::WaitForTaskCancellation {
                                        task_id: task_id.clone(),
                                        retry_count: *retry_count,
                                    },
                                ));
                            }
                        }

                        // at this stage we either retry or fail, so release the lock in either case
                        let mut txn = ctx.services.db_pool.begin().await?;
                        db::attestation::spdm::release_evidence_collection_lock(
                            &mut txn, object_id,
                        )
                        .await?;
                        // A synchronous cancellation also needs to honor the retry budget.
                        if *retry_count > 4 {
                            Ok(
                                StateHandlerOutcome::transition(SpdmAttestationState::Failed(
                                    "Too many retries triggering evidence collection".to_string(),
                                ))
                                .with_txn(txn),
                            )
                        } else {
                            Ok(StateHandlerOutcome::transition(
                                SpdmAttestationState::TriggerEvidenceCollection {
                                    retry_count: retry_count + 1,
                                    redfish_retry_count: 0,
                                    last_redfish_failure_tstamp: None,
                                },
                            )
                            .with_txn(txn))
                        }
                    }
                }
            }
            SpdmAttestationState::WaitForTaskCancellation {
                task_id,
                retry_count,
            } => {
                if !snapshot.evidence_collection_locked {
                    return Err(StateHandlerError::InvalidState(
                        "Cannot poll task cancellation without owning the device's evidence collection claim"
                            .to_string(),
                    ));
                }
                // get the lowest of: user cancellation vs automatic cancellation (i.e. triggered above)
                let cancellation_started_at = snapshot.state_version.timestamp();
                let started_at = snapshot
                    .cancelled_at
                    .map_or(cancellation_started_at, |time| {
                        time.min(cancellation_started_at)
                    });
                let timed_out = chrono::Utc::now() - started_at >= self.cancellation_timeout;
                // Check the deadline before Redfish so transport failures cannot keep
                // either a cancelled or a failed job's claim locked indefinitely.
                if timed_out {
                    tracing::warn!(%object_id, %task_id, "SPDM task cancellation timed out; releasing the evidence collection lock without BMC confirmation");
                } else {
                    let client = redfish_client(&snapshot.bmc_info, ctx).await?;
                    let is_stopped =
                        match client.get_task(task_id).await {
                            Ok(task) => evidence_collection_task_stopped(task.task_state.as_ref()),
                            // Redfish may remove the task when cancellation finishes.
                            Err(libredfish::RedfishError::HTTPErrorCode {
                                status_code, ..
                            }) if matches!(status_code.as_u16(), 404 | 410) => true,
                            Err(error) => return Err(redfish_error("get_task_state", error)),
                        };
                    if !is_stopped {
                        return Ok(StateHandlerOutcome::wait(
                            "Waiting for BMC evidence collection to stop after cancellation"
                                .to_string(),
                        ));
                    }
                }

                // this was a user requested cancellation, so move to the Cancelled state
                if snapshot.cancelled_at.is_some() {
                    return self
                        .complete_cancellation(object_id, original_controller_state, ctx)
                        .await;
                }
                // Automatic recovery must fail rather than waive attestation if cleanup
                // expires. Only a confirmed stop and remaining retries permit a new task.
                let next_state = if timed_out {
                    SpdmAttestationState::Failed(
                        "Timed out waiting for BMC evidence collection to stop".to_string(),
                    )
                } else if *retry_count > 4 {
                    SpdmAttestationState::Failed(
                        "Too many retries triggering evidence collection".to_string(),
                    )
                } else {
                    SpdmAttestationState::TriggerEvidenceCollection {
                        retry_count: retry_count + 1,
                        redfish_retry_count: 0,
                        last_redfish_failure_tstamp: None,
                    }
                };
                let mut txn = ctx.services.db_pool.begin().await?;
                db::attestation::spdm::release_evidence_collection_lock(&mut txn, object_id)
                    .await?;
                Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
            }
            SpdmAttestationState::NrasVerification => {
                let client = self.verifier.client(self.nras_config.clone());
                let raw_attest_outcome = perform_attestation(client.as_ref(), snapshot).await?;

                let processed_response = self
                    .verifier
                    .parse_attestation_outcome(&self.nras_config, &raw_attest_outcome)
                    .await
                    .map_err(SpdmHandlerError::from)?;

                if processed_response.attestation_passed {
                    Ok(StateHandlerOutcome::transition(
                        SpdmAttestationState::ApplyAppraisalPolicy,
                    ))
                } else {
                    Ok(StateHandlerOutcome::transition(
                        SpdmAttestationState::Failed(format!(
                            "Failed NRAS: {:#?}",
                            processed_response.devices
                        )),
                    ))
                }
            }
            SpdmAttestationState::ApplyAppraisalPolicy => {
                // nothing defined here yet, so just move to completed
                Ok(StateHandlerOutcome::transition(
                    SpdmAttestationState::Passed,
                ))
            }
            SpdmAttestationState::Passed => {
                let mut txn = ctx.services.db_pool.begin().await?;
                db::attestation::spdm::set_completed_at(&mut txn, machine_id, device_id, job_id)
                    .await?;
                Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
            }
            SpdmAttestationState::Failed(_reason) => {
                let mut txn = ctx.services.db_pool.begin().await?;
                db::attestation::spdm::set_completed_at(&mut txn, machine_id, device_id, job_id)
                    .await?;
                Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
            }
            SpdmAttestationState::Cancelled => {
                self.complete_cancellation(object_id, original_controller_state, ctx)
                    .await
            }
        }
    }
}

// Only these states establish that the task is no longer collecting evidence.
// In particular, a missing state or a request to stop does not prove completion.
fn evidence_collection_task_stopped(task_state: Option<&TaskState>) -> bool {
    matches!(
        task_state,
        Some(
            TaskState::Completed | TaskState::Killed | TaskState::Exception | TaskState::Cancelled
        )
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceCollectionTaskStateClass {
    Completed,
    InProgress,
    Unexpected(UnexpectedEvidenceCollectionTaskState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum UnexpectedEvidenceCollectionTaskState {
    Suspended,
    Interrupted,
    Pending,
    Stopping,
    Killed,
    Exception,
    Service,
    Cancelling,
    Cancelled,
    Missing,
}

fn classify_evidence_collection_task_state(
    task_state: Option<&TaskState>,
) -> EvidenceCollectionTaskStateClass {
    match task_state {
        Some(TaskState::Completed) => EvidenceCollectionTaskStateClass::Completed,
        Some(TaskState::New) | Some(TaskState::Starting) | Some(TaskState::Running) => {
            EvidenceCollectionTaskStateClass::InProgress
        }
        Some(TaskState::Suspended) => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Suspended,
        ),
        Some(TaskState::Interrupted) => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Interrupted,
        ),
        Some(TaskState::Pending) => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Pending,
        ),
        Some(TaskState::Stopping) => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Stopping,
        ),
        Some(TaskState::Killed) => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Killed,
        ),
        Some(TaskState::Exception) => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Exception,
        ),
        Some(TaskState::Service) => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Service,
        ),
        Some(TaskState::Cancelling) => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Cancelling,
        ),
        Some(TaskState::Cancelled) => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Cancelled,
        ),
        None => EvidenceCollectionTaskStateClass::Unexpected(
            UnexpectedEvidenceCollectionTaskState::Missing,
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum EvidenceCollectionNextAction {
    Wait,
    Cancel,
    Retry,
    Fail,
}

#[derive(Event)]
#[event(
    event_name = "spdm_evidence_collection_task_state_unexpected",
    metric_name = "carbide_spdm_evidence_collection_unexpected_task_states_total",
    component = "carbide-spdm-controller",
    log = error,
    metric = counter,
    message = "measurement collection task entered an unexpected state",
    describe = "Number of unexpected SPDM evidence collection task states, by task state and next action."
)]
struct SpdmEvidenceCollectionTaskStateUnexpected {
    #[label]
    task_state: UnexpectedEvidenceCollectionTaskState,
    #[label]
    next_action: EvidenceCollectionNextAction,
    #[context]
    machine_id: String,
    #[context]
    device_id: String,
    #[context]
    task_id: String,
    #[context]
    retry_count: i32,
    #[context]
    error: String,
}

/// The device kind an attestation covered, as a bounded metric label. Only the
/// kinds an attestation is actually dispatched for appear here; an unknown
/// device type returns before any attestation runs, so it never reaches a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum AttestedDeviceType {
    Gpu,
    Cx7,
}

/// One device attestation. Every attempt is audited; each variant keeps the
/// level that result already had.
#[derive(Event)]
#[event(
    event_name = "attestation_performed",
    metric_name = "carbide_attestation_total",
    component = "carbide-spdm-controller",
    metric = counter,
    describe = "Number of device attestations performed, by device type and outcome.",
    labels(outcome: Outcome, device_type: AttestedDeviceType),
)]
enum AttestationPerformed {
    #[event(
        labels(outcome = Outcome::Ok),
        log = info,
        message = "Device attestation performed"
    )]
    Ok {
        #[label]
        device_type: AttestedDeviceType,
        #[context]
        machine_id: String,
        #[context]
        device_id: String,
    },

    #[event(
        labels(outcome = Outcome::Error),
        log = warn,
        message = "Device attestation performed"
    )]
    Error {
        #[label]
        device_type: AttestedDeviceType,
        #[context]
        machine_id: String,
        #[context]
        device_id: String,
    },
}
async fn perform_attestation(
    client: &dyn VerifierClient,
    device: &SpdmDeviceAttestation,
) -> Result<RawAttestationOutcome, SpdmHandlerError> {
    let Some(ca_certificate) = &device.ca_certificate else {
        return Err(SpdmHandlerError::MissingData {
            field: "ca certificate".to_string(),
            machine_id: device.machine_id,
            device_id: device.device_id.clone(),
        });
    };

    let Some(evidence) = &device.evidence else {
        return Err(SpdmHandlerError::MissingData {
            field: "evidence".to_string(),
            machine_id: device.machine_id,
            device_id: device.device_id.clone(),
        });
    };

    let firmware_version = device
        .metadata
        .as_ref()
        .and_then(|m| m.firmware_version.clone())
        .ok_or_else(|| SpdmHandlerError::MissingData {
            field: "firmware_version".to_string(),
            machine_id: device.machine_id,
            device_id: device.device_id.clone(),
        })?;

    let device_attestation_info = DeviceAttestationInfo {
        ec: vec![EvidenceCertificate {
            evidence: evidence.signed_measurements.clone(),
            certificate: nras::certificate_to_base64(&ca_certificate.certificate_string),
            firmware_version,
        }],
        architecture: nras::MachineArchitecture::Blackwell,
        nonce: device.nonce_hex(),
    };

    let device_type: DeviceType = device.device_id.parse()?;
    let (attested_device_type, response) = match device_type {
        DeviceType::Gpu => (
            AttestedDeviceType::Gpu,
            client.attest_gpu(&device_attestation_info).await,
        ),
        // `attest_cx7` is not implemented yet, so every Cx7 call records
        // `outcome = error` -- deliberately, so the audit trail surfaces that we
        // were asked to attest a Cx7 device and couldn't. TODO: once Cx7
        // attestation lands, this becomes a real outcome; if the not-implemented
        // errors prove noisy before then, skip the emit for Cx7 as we do for
        // `Unknown`.
        DeviceType::Cx7 => (
            AttestedDeviceType::Cx7,
            client.attest_cx7(&device_attestation_info).await,
        ),
        DeviceType::Unknown => {
            return Err(SpdmHandlerError::VerifierNotImplemented {
                module: "state_handler".to_string(),
                machine_id: device.machine_id,
                device_id: device.device_id.clone(),
            });
        }
    };

    // Security audit trail. `nras`'s RED wrapper times the verifier call and warns
    // on transport failures but counts successes silently; attestation needs a
    // real audit record, so emit one for every call -- the one place a successful
    // attestation is logged (INFO), and a failed call is re-surfaced (WARN) with
    // its device type and outcome. device_type and outcome are the only labels;
    // the machine and device ids are log-only context. Attestation evidence, JWTs,
    // certificates, and the nonce never reach a label or the log line.
    let (machine_id, device_id) = (device.machine_id.to_string(), device.device_id.clone());
    emit(match Outcome::from(&response) {
        Outcome::Ok => AttestationPerformed::Ok {
            device_type: attested_device_type,
            machine_id,
            device_id,
        },
        Outcome::Error => AttestationPerformed::Error {
            device_type: attested_device_type,
            machine_id,
            device_id,
        },
    });

    match response {
        Ok(res) => Ok(res),
        Err(nras::NrasError::NotImplemented) => Err(SpdmHandlerError::VerifierNotImplemented {
            module: "verifier".to_string(),
            machine_id: device.machine_id,
            device_id: device.device_id.clone(),
        }),
        Err(err) => Err(SpdmHandlerError::NrasError(err)),
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;

    use super::EvidenceCollectionNextAction::{Cancel, Fail, Retry, Wait};
    use super::*;

    const METRIC_NAME: &str = "carbide_spdm_evidence_collection_unexpected_task_states_total";

    #[derive(Debug)]
    struct TaskStateCase {
        task_state: Option<TaskState>,
        retry_count: i32,
        next_action: EvidenceCollectionNextAction,
    }

    #[derive(Debug, PartialEq)]
    struct TaskStateObservation {
        log_count: usize,
        level: Option<tracing::Level>,
        event_name: Option<String>,
        metric_name: Option<String>,
        task_state: Option<String>,
        next_action: Option<String>,
        machine_id: Option<String>,
        device_id: Option<String>,
        task_id: Option<String>,
        retry_count: Option<String>,
        error: Option<String>,
        counter_delta: f64,
    }

    fn observe_task_state(input: TaskStateCase) -> TaskStateObservation {
        const MACHINE_ID: &str = "00000000-0000-0000-0000-000000000001";
        const DEVICE_ID: &str = "GPU-1";
        const TASK_ID: &str = "task-1";
        const ERROR: &str = "task failed";

        let task_state = match classify_evidence_collection_task_state(input.task_state.as_ref()) {
            EvidenceCollectionTaskStateClass::Unexpected(task_state) => Some(task_state),
            EvidenceCollectionTaskStateClass::Completed
            | EvidenceCollectionTaskStateClass::InProgress => None,
        };
        let next_action = input.next_action;
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            if let Some(task_state) = task_state {
                emit(SpdmEvidenceCollectionTaskStateUnexpected {
                    task_state,
                    next_action,
                    machine_id: MACHINE_ID.to_string(),
                    device_id: DEVICE_ID.to_string(),
                    task_id: TASK_ID.to_string(),
                    retry_count: input.retry_count,
                    error: ERROR.to_string(),
                });
            }
        });
        let counter_delta = match task_state {
            Some(task_state) => {
                let task_state = task_state.label_value();
                let next_action = next_action.label_value();
                metrics.counter_delta(
                    METRIC_NAME,
                    &[
                        ("task_state", task_state.as_str()),
                        ("next_action", next_action.as_str()),
                    ],
                )
            }
            None => 0.0,
        };
        let log = logs.first();

        TaskStateObservation {
            log_count: logs.len(),
            level: log.map(|log| log.level),
            event_name: log
                .and_then(|log| log.field("event_name"))
                .map(str::to_string),
            metric_name: log
                .and_then(|log| log.field("metric_name"))
                .map(str::to_string),
            task_state: log
                .and_then(|log| log.field("task_state"))
                .map(str::to_string),
            next_action: log
                .and_then(|log| log.field("next_action"))
                .map(str::to_string),
            machine_id: log
                .and_then(|log| log.field("machine_id"))
                .map(str::to_string),
            device_id: log
                .and_then(|log| log.field("device_id"))
                .map(str::to_string),
            task_id: log.and_then(|log| log.field("task_id")).map(str::to_string),
            retry_count: log
                .and_then(|log| log.field("retry_count"))
                .map(str::to_string),
            error: log.and_then(|log| log.field("error")).map(str::to_string),
            counter_delta,
        }
    }

    fn no_event() -> TaskStateObservation {
        TaskStateObservation {
            log_count: 0,
            level: None,
            event_name: None,
            metric_name: None,
            task_state: None,
            next_action: None,
            machine_id: None,
            device_id: None,
            task_id: None,
            retry_count: None,
            error: None,
            counter_delta: 0.0,
        }
    }

    fn emitted(task_state: &str, next_action: &str, retry_count: i32) -> TaskStateObservation {
        TaskStateObservation {
            log_count: 1,
            level: Some(tracing::Level::ERROR),
            event_name: Some("spdm_evidence_collection_task_state_unexpected".to_string()),
            metric_name: Some(METRIC_NAME.to_string()),
            task_state: Some(task_state.to_string()),
            next_action: Some(next_action.to_string()),
            machine_id: Some("00000000-0000-0000-0000-000000000001".to_string()),
            device_id: Some("GPU-1".to_string()),
            task_id: Some("task-1".to_string()),
            retry_count: Some(retry_count.to_string()),
            error: Some("task failed".to_string()),
            counter_delta: 1.0,
        }
    }

    #[test]
    fn evidence_collection_task_states_log_and_count_when_unexpected() {
        value_scenarios!(
            run = observe_task_state;
            "normal states do not emit" {
                TaskStateCase { task_state: Some(TaskState::New), retry_count: 0, next_action: Wait } => no_event(),
                TaskStateCase { task_state: Some(TaskState::Starting), retry_count: 0, next_action: Wait } => no_event(),
                TaskStateCase { task_state: Some(TaskState::Running), retry_count: 0, next_action: Wait } => no_event(),
                TaskStateCase { task_state: Some(TaskState::Completed), retry_count: 0, next_action: Wait } => no_event(),
            }
            "unexpected states retry only after stopping" {
                TaskStateCase { task_state: Some(TaskState::Suspended), retry_count: 4, next_action: Wait } => emitted("suspended", "wait", 4),
                TaskStateCase { task_state: Some(TaskState::Interrupted), retry_count: 4, next_action: Wait } => emitted("interrupted", "wait", 4),
                TaskStateCase { task_state: Some(TaskState::Pending), retry_count: 4, next_action: Wait } => emitted("pending", "wait", 4),
                TaskStateCase { task_state: Some(TaskState::Stopping), retry_count: 4, next_action: Wait } => emitted("stopping", "wait", 4),
                TaskStateCase { task_state: Some(TaskState::Killed), retry_count: 4, next_action: Retry } => emitted("killed", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Exception), retry_count: 4, next_action: Retry } => emitted("exception", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Service), retry_count: 4, next_action: Wait } => emitted("service", "wait", 4),
                TaskStateCase { task_state: Some(TaskState::Cancelling), retry_count: 4, next_action: Wait } => emitted("cancelling", "wait", 4),
                TaskStateCase { task_state: Some(TaskState::Cancelled), retry_count: 4, next_action: Retry } => emitted("cancelled", "retry", 4),
                TaskStateCase { task_state: None, retry_count: 4, next_action: Wait } => emitted("missing", "wait", 4),
            }
            "failure is observable" {
                TaskStateCase { task_state: Some(TaskState::Exception), retry_count: 5, next_action: Fail } => emitted("exception", "fail", 5),
            }
            "cancellation is observable" {
                TaskStateCase { task_state: Some(TaskState::Cancelling), retry_count: 0, next_action: Cancel } => emitted("cancelling", "cancel", 0),
            }
        );
    }
}
