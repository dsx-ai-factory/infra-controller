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

#[derive(Debug, Clone)]
pub struct SpdmAttestationStateHandler {
    verifier: Arc<dyn Verifier>,
    nras_config: nras::Config,
}

impl SpdmAttestationStateHandler {
    pub fn new(verifier: Arc<dyn Verifier>, nras_config: nras::Config) -> Self {
        Self {
            verifier,
            nras_config,
        }
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
        controller_state: &SpdmAttestationState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<SpdmAttestationState>, StateHandlerError> {
        // record metrics irrespective of the state of the machine
        self.record_metrics(snapshot, ctx);

        let transition_to_cancelled;
        let controller_state = if snapshot.cancelled_at.is_some()
            && controller_state != &SpdmAttestationState::Cancelled
        {
            transition_to_cancelled = true;
            &SpdmAttestationState::Cancelled
        } else {
            transition_to_cancelled = false;
            controller_state
        };

        let SpdmObjectId(machine_id, device_id) = object_id;

        match controller_state {
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
                db::attestation::spdm::update_metadata(&mut txn, machine_id, device_id, &metadata)
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
                    &ca_certificate.into_model(),
                )
                .await?;
                Ok(StateHandlerOutcome::transition(
                    SpdmAttestationState::TriggerEvidenceCollection { retry_count: 0 },
                )
                .with_txn(txn))
            }
            SpdmAttestationState::TriggerEvidenceCollection { retry_count } => {
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
                let nonce = snapshot.nonce_hex();
                let task = redfish_client
                    .trigger_evidence_collection(url.as_str(), nonce.as_str())
                    .await
                    .map_err(|error| redfish_error("trigger measurement collection", error))?;

                Ok(StateHandlerOutcome::transition(
                    SpdmAttestationState::PollEvidenceCollection {
                        task_id: task.id,
                        retry_count: *retry_count,
                    },
                ))
            }
            SpdmAttestationState::PollEvidenceCollection {
                task_id,
                retry_count,
            } => {
                let redfish_client = redfish_client(&snapshot.bmc_info, ctx).await?;
                let task = redfish_client
                    .get_task(task_id)
                    .await
                    .map_err(|e| redfish_error("get_task_state", e))?;

                match classify_evidence_collection_task_state(task.task_state.as_ref()) {
                    EvidenceCollectionTaskStateClass::Completed => {
                        // read the result
                        let Some(url) = &snapshot.evidence_target else {
                            // This is an unrecoverable error due to db discrepancy.
                            return Ok(StateHandlerOutcome::transition(
                                SpdmAttestationState::Failed(
                                    "Could not get evidence target from DB".to_string(),
                                ),
                            ));
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
                            &evidence.into_model(),
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
                        let err = task.messages.iter().map(|t| t.message.as_str()).join("\n");
                        let next_action = EvidenceCollectionNextAction::for_retry(*retry_count);

                        emit(SpdmEvidenceCollectionTaskStateUnexpected {
                            task_state,
                            next_action,
                            machine_id: machine_id.to_string(),
                            device_id: device_id.clone(),
                            task_id: task_id.clone(),
                            retry_count: *retry_count,
                            error: err,
                        });

                        if next_action == EvidenceCollectionNextAction::Fail {
                            Ok(StateHandlerOutcome::transition(
                                SpdmAttestationState::Failed(
                                    "Too many retries triggering evidence collection".to_string(),
                                ),
                            ))
                        } else {
                            Ok(StateHandlerOutcome::transition(
                                SpdmAttestationState::TriggerEvidenceCollection {
                                    retry_count: retry_count + 1,
                                },
                            ))
                        }
                    }
                }
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
                db::attestation::spdm::set_completed_at(&mut txn, machine_id, device_id).await?;
                Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
            }
            SpdmAttestationState::Failed(_reason) => {
                let mut txn = ctx.services.db_pool.begin().await?;
                db::attestation::spdm::set_completed_at(&mut txn, machine_id, device_id).await?;
                Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
            }
            SpdmAttestationState::Cancelled => {
                let mut txn = ctx.services.db_pool.begin().await?;
                db::attestation::spdm::set_completed_at(&mut txn, machine_id, device_id).await?;
                if transition_to_cancelled {
                    Ok(
                        StateHandlerOutcome::transition(SpdmAttestationState::Cancelled)
                            .with_txn(txn),
                    )
                } else {
                    Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
                }
            }
        }
    }
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
    Retry,
    Fail,
}

impl EvidenceCollectionNextAction {
    fn for_retry(retry_count: i32) -> Self {
        if retry_count > 4 {
            Self::Fail
        } else {
            Self::Retry
        }
    }
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

    use super::*;

    const METRIC_NAME: &str = "carbide_spdm_evidence_collection_unexpected_task_states_total";

    #[derive(Debug)]
    struct TaskStateCase {
        task_state: Option<TaskState>,
        retry_count: i32,
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
        let next_action = EvidenceCollectionNextAction::for_retry(input.retry_count);
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
                TaskStateCase { task_state: Some(TaskState::New), retry_count: 0 } => no_event(),
                TaskStateCase { task_state: Some(TaskState::Starting), retry_count: 0 } => no_event(),
                TaskStateCase { task_state: Some(TaskState::Running), retry_count: 0 } => no_event(),
                TaskStateCase { task_state: Some(TaskState::Completed), retry_count: 0 } => no_event(),
            }
            "unexpected states retry" {
                TaskStateCase { task_state: Some(TaskState::Suspended), retry_count: 4 } => emitted("suspended", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Interrupted), retry_count: 4 } => emitted("interrupted", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Pending), retry_count: 4 } => emitted("pending", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Stopping), retry_count: 4 } => emitted("stopping", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Killed), retry_count: 4 } => emitted("killed", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Exception), retry_count: 4 } => emitted("exception", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Service), retry_count: 4 } => emitted("service", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Cancelling), retry_count: 4 } => emitted("cancelling", "retry", 4),
                TaskStateCase { task_state: Some(TaskState::Cancelled), retry_count: 4 } => emitted("cancelled", "retry", 4),
                TaskStateCase { task_state: None, retry_count: 4 } => emitted("missing", "retry", 4),
            }
            "retry budget is exhausted" {
                TaskStateCase { task_state: Some(TaskState::Interrupted), retry_count: 5 } => emitted("interrupted", "fail", 5),
                TaskStateCase { task_state: None, retry_count: 5 } => emitted("missing", "fail", 5),
            }
        );
    }
}
