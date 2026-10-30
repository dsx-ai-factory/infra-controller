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
use ::rpc::forge::{self as rpc};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use carbide_machine_controller::handler::attestation::{
    SchedulingOutcome, select_machine_attesters, trigger_attestation,
};
use carbide_uuid::machine::MachineId;
use db::ObjectFilter;
use itertools::Itertools;
use model::attestation::spdm as model_spdm;
use model::bmc_info::BmcInfo;
use model::machine::machine_search_config::MachineSearchConfig;
use sqlx::PgPool;
use state_controller::state_handler::StateHandlerError;
use tokio::time as tt;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::CarbideError;
use crate::api::{Api, log_machine_id, log_request_data};

pub(crate) async fn trigger_machine_attestation(
    api: &Api,
    request: Request<rpc::SpdmMachineAttestationTriggerRequest>,
) -> Result<Response<rpc::SpdmMachineAttestationTriggerResponse>, Status> {
    log_request_data(&request);

    // `spdm.enabled` decides whether the SPDM state controller is spawned at
    // all, so with it off there is nothing to process the rows this would
    // insert. Scheduling anyway strands the machine as permanently
    // "under attestation".
    if !api.runtime_config.spdm.enabled {
        return Err(CarbideError::UnavailableError(
            "SPDM attestation is disabled for this site, so no attestation \
             controller is running to carry the request out"
                .to_string(),
        )
        .into());
    }

    let request_payload = request.get_ref();
    let machine_id = request_payload
        .machine_id
        .ok_or(Status::from(CarbideError::Internal {
            message: "No machine id supplied".to_string(),
        }))?;
    let redfish_timeout_duration =
        std::time::Duration::from_secs(request_payload.redfish_timeout_secs as u64);

    log_machine_id(&machine_id);

    let mut db_reader = api.db_reader();

    let machines = db::machine::find(
        &mut db_reader,
        ObjectFilter::List(&[machine_id]),
        MachineSearchConfig::default(),
    )
    .await?;
    let bmc_info = match machines.len() {
        0 => {
            return Err(Status::from(CarbideError::NotFoundError {
                kind: "machine",
                id: format!("{}", machine_id),
            }));
        }
        1 => &machines[0].status.bmc_info,
        _ => {
            return Err(Status::from(CarbideError::Internal {
                message: format!("Found more than one machine for machine id {}", machine_id),
            }));
        }
    };
    let bmc_ip_addr = bmc_info.ip_addr().map_err(|e| CarbideError::Internal {
        message: format!("{}", e),
    })?;

    let bmc_access_info =
        db::machine_interface::lookup_bmc_access_info(&mut db_reader, bmc_ip_addr, bmc_info.port)
            .await?;
    drop(db_reader);

    // Called only if the machine's profile turns out to need the BMC: creating
    // a client authenticates against it, which the profile-only outcomes
    // report without.
    let connect = async || {
        let redfish_client_future = api.redfish_pool.client_by_info(&bmc_access_info);
        match tt::timeout(redfish_timeout_duration, redfish_client_future).await {
            Ok(redfish_result) => redfish_result.map_err(StateHandlerError::from),
            Err(_) => Err(StateHandlerError::GenericError(eyre::eyre!(
                "redfish creation could not finish in {} seconds",
                redfish_timeout_duration.as_secs()
            ))),
        }
    };

    let result = trigger_attestation(
        api.pg_pool(),
        connect,
        bmc_info,
        &machine_id,
        redfish_timeout_duration,
        false, // A gRPC request does not originate from the managed-host controller.
    )
    .await
    .map_err(|error| match error {
        // Reaching the BMC is the only external service this path calls, so
        // an operator gets the reachability mitigation that carries rather
        // than a generic attestation failure.
        StateHandlerError::ExternalServiceError(redfish) => {
            CarbideError::RedfishError(libredfish::RedfishError::GenericError {
                error: redfish.to_string(),
            })
        }
        other => CarbideError::AttestationError(format!("trigger error: {other}")),
    })?;

    Ok(Response::new(rpc::SpdmMachineAttestationTriggerResponse {
        machine_id: Some(machine_id),
        devices_under_attestation: result.devices_scheduled as i32,
        resolved_hardware_class: result.hardware_class,
        outcome: reported_outcome(result.outcome).into(),
        used_any_fallback: result.used_any_fallback,
        profile_version: result.profile_version,
        started_at: result.started_at.map(Into::into),
        job_id: result.job_id.map(|job_id| ::rpc::common::Uuid {
            value: job_id.to_string(),
        }),
    }))
}

/// Spelled out rather than derived, so adding an outcome fails to compile
/// until it has been given a wire value. Neither type is local to this crate,
/// so this cannot be a `From`.
fn reported_outcome(outcome: SchedulingOutcome) -> rpc::SpdmSchedulingOutcome {
    match outcome {
        SchedulingOutcome::Scheduled => rpc::SpdmSchedulingOutcome::Scheduled,
        SchedulingOutcome::PartiallySatisfied => rpc::SpdmSchedulingOutcome::PartiallySatisfied,
        SchedulingOutcome::AttestationDisabled => rpc::SpdmSchedulingOutcome::AttestationDisabled,
        SchedulingOutcome::NoAttestersFound => rpc::SpdmSchedulingOutcome::NoAttestersFound,
        SchedulingOutcome::PolicyMatchedNothing => rpc::SpdmSchedulingOutcome::PolicyMatchedNothing,
        SchedulingOutcome::ClassNotRecorded => rpc::SpdmSchedulingOutcome::ClassNotRecorded,
        SchedulingOutcome::NoProfile => rpc::SpdmSchedulingOutcome::NoProfile,
    }
}

pub(crate) async fn cancel_attestation(
    api: &Api,
    request: Request<::rpc::Uuid>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let job_id: Uuid = Uuid::try_from(request.into_inner())
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;

    let mut txn = api.txn_begin().await?;
    db::attestation::spdm::cancel_attestation_job(&mut txn, &job_id).await?;
    txn.commit().await?;

    Ok(Response::new(()))
}

fn convert_model_status_to_rpc_status(
    model_status: model_spdm::SpdmAttestationStatus,
) -> rpc::SpdmAttestationStatus {
    match model_status {
        model_spdm::SpdmAttestationStatus::InProgress => {
            rpc::SpdmAttestationStatus::SpdmAttInProgress
        }
        model_spdm::SpdmAttestationStatus::Cancelled => {
            rpc::SpdmAttestationStatus::SpdmAttCancelled
        }
        model_spdm::SpdmAttestationStatus::Passed => rpc::SpdmAttestationStatus::SpdmAttPassed,
        model_spdm::SpdmAttestationStatus::Failed => rpc::SpdmAttestationStatus::SpdmAttFailed,
    }
}

pub(crate) async fn list_attestation(
    api: &Api,
    request: tonic::Request<rpc::SpdmListAttestationRequest>,
) -> Result<Response<rpc::SpdmListAttestationResponse>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let request_selector =
        rpc::SpdmListAttestationRequestSelector::try_from(request.get_ref().selector)
            .map_err(|_| Status::invalid_argument("invalid attestation selector"))?;

    let list_selector = match request_selector {
        rpc::SpdmListAttestationRequestSelector::SpdmListInProgress => {
            model_spdm::SpdmAttestationStatus::InProgress
        }
        rpc::SpdmListAttestationRequestSelector::SpdmListCancelled => {
            model_spdm::SpdmAttestationStatus::Cancelled
        }
        rpc::SpdmListAttestationRequestSelector::SpdmListFailed => {
            model_spdm::SpdmAttestationStatus::Failed
        }
        rpc::SpdmListAttestationRequestSelector::SpdmListPassed => {
            model_spdm::SpdmAttestationStatus::Passed
        }
    };

    let response = db::attestation::spdm::list_all_attestation_statuses(&mut txn, list_selector)
        .await
        .map(|statuses| {
            Response::new(rpc::SpdmListAttestationResponse {
                statuses: statuses
                    .into_iter()
                    .map(
                        |(job_id, status, machine_ids)| rpc::SpdmAttestationJobStatus {
                            job_id: Some(::rpc::common::Uuid {
                                value: job_id.to_string(),
                            }),
                            attestation_status: convert_model_status_to_rpc_status(status).into(),
                            machine_ids,
                        },
                    )
                    .collect(),
            })
        })?;

    txn.commit().await?;

    Ok(response)
}

// The token carries only the cursor and filter, so page sizes can change without
// keeping server-side state. Versioning lets us reject incompatible encodings.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineAttestationPageToken {
    version: u8,
    after_machine_id: MachineId,
    attestation_status: Option<i32>,
}

pub(crate) async fn list_machine_attestation_statuses(
    api: &Api,
    request: Request<rpc::ListMachineAttestationStatusesRequest>,
) -> Result<Response<rpc::ListMachineAttestationStatusesResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    match request
        .target
        .ok_or_else(|| Status::invalid_argument("provide a fleet or machine target"))?
    {
        rpc::list_machine_attestation_statuses_request::Target::Fleet(request) => {
            list_machine_predictions(api, request).await
        }
        rpc::list_machine_attestation_statuses_request::Target::Machine(request) => {
            assess_single_machine(api, request).await
        }
    }
}

/// The result of selecting current attesters, without scheduling attestation work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttesterSelectionStatus {
    /// All selection requirements were satisfied and at least one attester was selected.
    Selected,
    /// Some allowlist requirements matched nothing; status covers the selected subset.
    PartiallySatisfied,
    /// Site-wide SPDM or the resolved profile disables attestation.
    Disabled,
    /// The BMC offered no eligible attesters under the resolved policy.
    NoAttestersFound,
    /// The policy's requirements matched no eligible attesters.
    PolicyMatchedNothing,
    /// No hardware class was recorded and no `any` profile applied.
    ClassNotRecorded,
    /// Neither the recorded hardware class nor `any` supplied a profile.
    NoProfile,
}

/// A read-only assessment of the current selection and its latest stored results.
struct MachineAttestationAssessment {
    /// Aggregate for selected devices; absent when selection is empty or disabled.
    attestation_status: Option<model_spdm::SpdmAttestationStatus>,
    /// Whether selection was complete, partial, disabled, or unable to select attesters.
    attester_selection_status: AttesterSelectionStatus,
}

/// Selects current attesters and reads their latest results across all job origins.
/// Uses the same policy and inventory selection as scheduling and recovery, without
/// writing work rows or emitting scheduling events. Site-wide disablement returns
/// an empty selection without contacting the BMC. Selection finishes before opening
/// the status transaction; selected devices without results make it `InProgress`.
async fn assess_machine_attestation(
    db_pool: &PgPool,
    connect: impl AsyncFnOnce() -> Result<Box<dyn libredfish::Redfish>, StateHandlerError>,
    bmc_info: &BmcInfo,
    machine_id: &MachineId,
    redfish_timeout_duration: std::time::Duration,
    spdm_enabled: bool,
) -> Result<MachineAttestationAssessment, StateHandlerError> {
    // Disabled sites have no current attestation requirement to assess.
    if !spdm_enabled {
        return Ok(MachineAttestationAssessment {
            attestation_status: None,
            attester_selection_status: AttesterSelectionStatus::Disabled,
        });
    }
    let selection = select_machine_attesters(
        db_pool,
        connect,
        bmc_info,
        machine_id,
        redfish_timeout_duration,
    )
    .await?;
    let mut device_ids = selection
        .attesters
        .into_iter()
        .map(|attester| attester.id)
        .collect_vec();
    device_ids.sort();
    device_ids.dedup();
    // An empty selection has a reason, but cannot supply a passed verdict.
    let attestation_status = if device_ids.is_empty() {
        None
    } else {
        let mut txn = db_pool.begin().await?;
        let status = db::attestation::spdm::list_single_machine_attestation_status(
            &mut txn,
            machine_id,
            Some(&device_ids),
        )
        .await?;
        txn.commit().await?;
        Some(status)
    };
    // Selection success reports what was selected, without implying a job was scheduled.
    let attester_selection_status = match selection.outcome {
        SchedulingOutcome::Scheduled => AttesterSelectionStatus::Selected,
        SchedulingOutcome::PartiallySatisfied => AttesterSelectionStatus::PartiallySatisfied,
        SchedulingOutcome::AttestationDisabled => AttesterSelectionStatus::Disabled,
        SchedulingOutcome::NoAttestersFound => AttesterSelectionStatus::NoAttestersFound,
        SchedulingOutcome::PolicyMatchedNothing => AttesterSelectionStatus::PolicyMatchedNothing,
        SchedulingOutcome::ClassNotRecorded => AttesterSelectionStatus::ClassNotRecorded,
        SchedulingOutcome::NoProfile => AttesterSelectionStatus::NoProfile,
    };
    Ok(MachineAttestationAssessment {
        attestation_status,
        attester_selection_status,
    })
}

async fn assess_single_machine(
    api: &Api,
    request: rpc::SpdmMachineAttestationMachineRequest,
) -> Result<Response<rpc::ListMachineAttestationStatusesResponse>, Status> {
    let machine_id = request
        .machine_id
        .ok_or_else(|| Status::invalid_argument("machine ID is required"))?;
    let timeout_secs = request.redfish_timeout_secs.unwrap_or(30);
    if timeout_secs == 0 {
        return Err(Status::invalid_argument("redfish timeout must be positive"));
    }
    let duration = std::time::Duration::from_secs(u64::from(timeout_secs));
    // Bound the complete assessment, including client creation and database reads.
    let assessment = tt::timeout(duration, async {
        let mut reader = api.db_reader();
        let machines = db::machine::find(
            &mut reader,
            ObjectFilter::List(&[machine_id]),
            MachineSearchConfig::default(),
        )
        .await?;
        let machine = machines
            .into_iter()
            .next()
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            })?;
        drop(reader);
        let bmc_info = &machine.status.bmc_info;
        // Resolve credentials and authenticate only when selection requires the BMC.
        let connect = async || {
            let address = bmc_info
                .ip_addr()
                .map_err(StateHandlerError::GenericError)?;
            let mut reader = api.db_reader();
            let access =
                db::machine_interface::lookup_bmc_access_info(&mut reader, address, bmc_info.port)
                    .await?;
            drop(reader);
            api.redfish_pool
                .client_by_info(&access)
                .await
                .map_err(StateHandlerError::from)
        };
        assess_machine_attestation(
            api.pg_pool(),
            connect,
            bmc_info,
            &machine_id,
            duration,
            api.runtime_config.spdm.enabled,
        )
        .await
        .map_err(|error| match error {
            StateHandlerError::ExternalServiceError(redfish) => {
                CarbideError::RedfishError(libredfish::RedfishError::GenericError {
                    error: redfish.to_string(),
                })
            }
            StateHandlerError::DBError(error) => CarbideError::from(*error),
            StateHandlerError::TransactionError(error) => CarbideError::DBError(
                db::AnnotatedSqlxError::new("assess machine attestation", *error),
            ),
            other => CarbideError::AttestationError(format!("assessment error: {other}")),
        })
    })
    .await
    .map_err(|_| Status::deadline_exceeded("machine attestation assessment timed out"))??;
    let attester_selection_status = match assessment.attester_selection_status {
        AttesterSelectionStatus::Selected => rpc::SpdmAttesterSelectionStatus::Selected,
        AttesterSelectionStatus::PartiallySatisfied => {
            rpc::SpdmAttesterSelectionStatus::PartiallySatisfied
        }
        AttesterSelectionStatus::Disabled => rpc::SpdmAttesterSelectionStatus::Disabled,
        AttesterSelectionStatus::NoAttestersFound => {
            rpc::SpdmAttesterSelectionStatus::NoAttestersFound
        }
        AttesterSelectionStatus::PolicyMatchedNothing => {
            rpc::SpdmAttesterSelectionStatus::PolicyMatchedNothing
        }
        AttesterSelectionStatus::ClassNotRecorded => {
            rpc::SpdmAttesterSelectionStatus::ClassNotRecorded
        }
        AttesterSelectionStatus::NoProfile => rpc::SpdmAttesterSelectionStatus::NoProfile,
    };
    Ok(Response::new(rpc::ListMachineAttestationStatusesResponse {
        statuses: vec![rpc::SpdmMachineAttestationStatus {
            machine_id: Some(machine_id),
            attestation_status: assessment
                .attestation_status
                .map(|status| convert_model_status_to_rpc_status(status).into()),
            attester_selection_status: Some(attester_selection_status.into()),
        }],
        next_page_token: String::new(),
    }))
}

async fn list_machine_predictions(
    api: &Api,
    request: rpc::SpdmMachineAttestationFleetRequest,
) -> Result<Response<rpc::ListMachineAttestationStatusesResponse>, Status> {
    // Presence matters: enum zero means in-progress, not an omitted filter.
    let status = request
        .attestation_status
        .map(|value| {
            rpc::SpdmAttestationStatus::try_from(value)
                .map(|status| match status {
                    rpc::SpdmAttestationStatus::SpdmAttInProgress => {
                        model_spdm::SpdmAttestationStatus::InProgress
                    }
                    rpc::SpdmAttestationStatus::SpdmAttCancelled => {
                        model_spdm::SpdmAttestationStatus::Cancelled
                    }
                    rpc::SpdmAttestationStatus::SpdmAttPassed => {
                        model_spdm::SpdmAttestationStatus::Passed
                    }
                    rpc::SpdmAttestationStatus::SpdmAttFailed => {
                        model_spdm::SpdmAttestationStatus::Failed
                    }
                })
                .map_err(|_| Status::invalid_argument("invalid attestation status"))
        })
        .transpose()?;
    let cap = api.runtime_config.max_find_by_ids;
    if cap == 0 {
        return Err(Status::internal(
            "machine attestation page limit must be positive",
        ));
    }
    let page_size = if request.page_size == 0 {
        cap
    } else {
        request.page_size.min(cap)
    };
    // A continuation resumes the same search; changing its filter could skip matches.
    let after_machine_id = if request.page_token.is_empty() {
        None
    } else {
        let bytes = URL_SAFE_NO_PAD
            .decode(&request.page_token)
            .map_err(|_| Status::invalid_argument("invalid machine attestation page token"))?;
        let token: MachineAttestationPageToken = serde_json::from_slice(&bytes)
            .map_err(|_| Status::invalid_argument("invalid machine attestation page token"))?;
        if token.version != 1 || token.attestation_status != request.attestation_status {
            return Err(Status::invalid_argument(
                "machine attestation page token is incompatible with this search",
            ));
        }
        Some(token.after_machine_id)
    };
    // One statement computes matching statuses and reads one extra row for lookahead.
    let mut txn = api.txn_begin().await?;
    let mut summaries = db::attestation::spdm::find_machine_attestation_statuses(
        &mut txn,
        after_machine_id,
        status,
        page_size,
    )
    .await?;
    txn.commit().await?;
    let has_more = summaries.len() > page_size as usize;
    summaries.truncate(page_size as usize);
    let next_page_token = if let Some(last) = summaries.last().filter(|_| has_more) {
        let token = MachineAttestationPageToken {
            version: 1,
            after_machine_id: last.machine_id,
            attestation_status: request.attestation_status,
        };
        let bytes = serde_json::to_vec(&token).map_err(|error| {
            tracing::error!(%error, "failed to encode machine attestation page token");
            Status::internal("failed to encode machine attestation page token")
        })?;
        URL_SAFE_NO_PAD.encode(bytes)
    } else {
        String::new()
    };
    Ok(Response::new(rpc::ListMachineAttestationStatusesResponse {
        statuses: summaries
            .into_iter()
            .map(|summary| rpc::SpdmMachineAttestationStatus {
                machine_id: Some(summary.machine_id),
                attester_selection_status: None,
                attestation_status: summary
                    .attestation_status
                    .map(|status| convert_model_status_to_rpc_status(status).into()),
            })
            .collect(),
        next_page_token,
    }))
}

pub(crate) async fn get_attestation(
    api: &Api,
    request: tonic::Request<::rpc::Uuid>,
) -> Result<Response<rpc::SpdmGetAttestationResponse>, Status> {
    log_request_data(&request);

    let job_id: Uuid = Uuid::try_from(request.into_inner())
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;

    let mut txn = api.txn_begin().await?;
    let attestations_details =
        db::attestation::spdm::get_attestations_for_job_id(&mut txn, &job_id).await?;
    txn.commit().await?;

    Ok(Response::new(rpc::SpdmGetAttestationResponse {
        attestations_details: attestations_details
            .iter()
            .map(|elem| {
                std::convert::Into::<::rpc::forge::SpdmAttestationDetails>::into((*elem).clone())
            })
            .collect(),
    }))
}

#[cfg(feature = "linux-build")]
pub(crate) async fn attest_quote(
    api: &Api,
    request: Request<rpc::AttestQuoteRequest>,
) -> std::result::Result<Response<rpc::AttestQuoteResponse>, Status> {
    log_request_data(&request);

    let mut request = request.into_inner();

    // TODO: consider if this code can be turned into a templated function and reused
    // in bind_attest_key
    let machine_id: MachineId =
        crate::handlers::utils::convert_and_log_machine_id(request.machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;

    let ak_pub_bytes =
        match db::attestation::secret_ak_pub::get_by_secret(&mut txn, &request.credential).await? {
            Some(entry) => entry.ak_pub,
            None => {
                return Err(CarbideError::AttestQuoteError(
                    "no attestation key found for the provided credential".into(),
                )
                .into());
            }
        };

    // Make sure sure the signature can at least be verified
    // as valid or invalid. If it can't be verified in any
    // way at all, return an error.
    let signature_valid = crate::attestation::verify_signature(
        &ak_pub_bytes,
        &request.attestation,
        &request.signature,
    )
    .inspect_err(|e| {
        carbide_instrument::emit(crate::attestation::MeasuredBootVerificationFailed {
            cause: crate::attestation::MeasuredBootVerificationFailureCause::VerificationError,
            event_log: crate::attestation::event_log_to_string(&request.event_log),
            error: format!("PCR signature verification failed: {e}"),
        });
    })?;

    // Make sure we can verify the the PCR hash one way
    // or another. If it can't be, return an error.
    let pcr_hash_matches =
        crate::attestation::verify_pcr_hash(&request.attestation, &request.pcr_values)
            .inspect_err(|e| {
                carbide_instrument::emit(crate::attestation::MeasuredBootVerificationFailed {
                    cause:
                        crate::attestation::MeasuredBootVerificationFailureCause::VerificationError,
                    event_log: crate::attestation::event_log_to_string(&request.event_log),
                    error: format!("PCR hash verification failed: {e}"),
                });
            })?;

    // And now pass on through the computed signature
    // validity and PCR hash match to see if execution can
    // continue (the event log goes with, since it will be
    // logged in the event of an invalid signature or PCR
    // hash mismatch).
    crate::attestation::verify_quote_state(signature_valid, pcr_hash_matches, &request.event_log)?;

    // If we've reached this point, we can now clean up
    // now ephemeral secret data from the database, and send
    // off the PCR values as a MeasurementReport.
    db::attestation::secret_ak_pub::delete(&mut txn, &request.credential).await?;

    let pcr_values: ::measured_boot::pcr::PcrRegisterValueVec = request
        .pcr_values
        .drain(..)
        .map(hex::encode)
        .collect::<Vec<String>>()
        .into();

    // In this case, we're not doing anything with
    // the resulting report (at least not yet), so just
    // throw it away.
    let report = db::measured_boot::report::new(&mut txn, machine_id, &pcr_values.0)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!(
                "Failed storing measurement report: (machine_id: {}, err: {})",
                machine_id, e
            ),
        })?;

    // if the attestation was successful and enabled, we can now vend the certs
    // - get attestation result
    // - if enabled and not successful, send response without certs
    // - else send response with certs
    let attestation_failed = if api.runtime_config.attestation_enabled {
        !crate::attestation::has_passed_attestation(&mut txn, &machine_id, &report.report_id)
            .await?
    } else {
        false
    };

    txn.commit().await?;

    if attestation_failed {
        tracing::info!(
            machine_id = %machine_id,
            "Attestation failed; not vending any certificates",
        );
        return Ok(Response::new(rpc::AttestQuoteResponse {
            success: false,
            machine_certificate: None,
        }));
    }

    let id_str = machine_id.to_string();
    let certificate = if std::env::var("UNSUPPORTED_CERTIFICATE_PROVIDER").is_ok() {
        carbide_secrets::certificates::Certificate::default()
    } else {
        api.certificate_provider
            .get_certificate(id_str.as_str(), None, None)
            .await
            .map_err(|err| CarbideError::ClientCertificateError(err.to_string()))?
    };

    tracing::info!(
        machine_id = %machine_id,
        attestation_enabled = api.runtime_config.attestation_enabled,
        "Attestation succeeded; sending a certificate",
    );
    Ok(Response::new(rpc::AttestQuoteResponse {
        success: true,
        machine_certificate: Some(certificate.into()),
    }))
}

#[cfg(not(feature = "linux-build"))]
#[allow(clippy::unused_async)]
pub(crate) async fn attest_quote(
    _api: &Api,
    _request: Request<rpc::AttestQuoteRequest>,
) -> std::result::Result<Response<rpc::AttestQuoteResponse>, Status> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::{SchedulingOutcome, reported_outcome, rpc};

    /// One outcome is spelled twice: `LabelValue` snake-cases the variant for
    /// the metric, and protobuf screaming-cases it for the wire. Two derives,
    /// no common source, so renaming either side moves one spelling and
    /// leaves the other, and an operator reading a metric no longer finds the
    /// same word on the API.
    #[test]
    fn the_metric_label_and_the_wire_name_stay_one_vocabulary() {
        const PREFIX: &str = "SPDM_SCHEDULING_OUTCOME_";

        let covered = [
            SchedulingOutcome::Scheduled,
            SchedulingOutcome::PartiallySatisfied,
            SchedulingOutcome::AttestationDisabled,
            SchedulingOutcome::NoAttestersFound,
            SchedulingOutcome::PolicyMatchedNothing,
            SchedulingOutcome::ClassNotRecorded,
            SchedulingOutcome::NoProfile,
        ]
        .map(|outcome| {
            let wire = reported_outcome(outcome);
            assert_eq!(
                carbide_instrument::LabelValue::label_value(&outcome).as_str(),
                wire.as_str_name()
                    .strip_prefix(PREFIX)
                    .expect("every value carries the enum prefix")
                    .to_lowercase(),
                "{outcome:?}"
            );
            wire
        });

        // An outcome added to the schema but never listed above would
        // otherwise go unchecked, since nothing here iterates the enum.
        for value in 1.. {
            let Ok(wire) = rpc::SpdmSchedulingOutcome::try_from(value) else {
                break;
            };
            assert!(covered.contains(&wire), "{wire:?} is not covered above");
        }
    }
}
