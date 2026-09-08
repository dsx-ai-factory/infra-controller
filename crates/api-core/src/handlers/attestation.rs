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
use carbide_machine_controller::handler::attestation::trigger_attestation;
use carbide_uuid::machine::MachineId;
use db::ObjectFilter;
use model::attestation::spdm as model_spdm;
use model::machine::machine_search_config::MachineSearchConfig;
use tokio::time as tt;
use tonic::{Request, Response, Status};

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

    let redfish_client_future = api.redfish_pool.client_by_info(&bmc_access_info);

    let redfish_client = match tt::timeout(redfish_timeout_duration, redfish_client_future).await {
        Ok(redfish_result) => redfish_result.map_err(|e| CarbideError::RedfishClientCreation {
            inner: Box::new(e),
            machine_id,
        })?,
        Err(_) => {
            return Err(Status::from(CarbideError::Internal {
                message: format!(
                    "redfish creation could not finish in {} seconds",
                    redfish_timeout_duration.as_secs()
                ),
            }));
        }
    };

    let records_inserted = trigger_attestation(
        api.pg_pool(),
        redfish_client,
        bmc_info,
        &machine_id,
        redfish_timeout_duration,
    )
    .await
    .map_err(|e| CarbideError::AttestationError(format!("trigger error: {e}")))?;

    Ok(Response::new(rpc::SpdmMachineAttestationTriggerResponse {
        machine_id: Some(machine_id),
        devices_under_attestation: records_inserted as i32,
    }))
}

pub(crate) async fn cancel_machine_attestation(
    api: &Api,
    request: Request<MachineId>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let machine_id = request.get_ref();
    log_machine_id(machine_id);

    let mut txn = api.txn_begin().await?;
    db::attestation::spdm::cancel_machine_attestation(&mut txn, machine_id).await?;
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

pub(crate) async fn list_attestation_machines(
    api: &Api,
    request: tonic::Request<rpc::SpdmListAttestationMachinesRequest>,
) -> Result<Response<rpc::SpdmListAttestationMachinesResponse>, Status> {
    // if selector is not selected AND machine id is None,
    // just list all machines + their attestation status
    // if machine id not None, print the attestation status for this machine only
    // if machine id is None AND selector is unsuccessful, print failed attestations
    // if machine is None AND selector is in progress, print all machines that are in progress

    let mut txn = api.txn_begin().await?;

    let response = match &request.get_ref().variant {
        None => db::attestation::spdm::list_all_attestation_statuses(
            &mut txn,
            db::attestation::spdm::ListSelector::None,
        )
        .await
        .map(|statuses| {
            Response::new(rpc::SpdmListAttestationMachinesResponse {
                statuses: statuses
                    .iter()
                    .map(|elem| rpc::SpdmMachineAttestationStatus {
                        machine_id: Some(elem.0),
                        attestation_status: convert_model_status_to_rpc_status(elem.1).into(),
                    })
                    .collect(),
            })
        })?,
        Some(variant) => match variant {
            rpc::spdm_list_attestation_machines_request::Variant::MachineId(machine_id) => {
                db::attestation::spdm::list_single_machine_attestation_status(&mut txn, machine_id)
                    .await
                    .map(|status| {
                        Response::new(rpc::SpdmListAttestationMachinesResponse {
                            statuses: vec![rpc::SpdmMachineAttestationStatus {
                                machine_id: Some(*machine_id),
                                attestation_status: convert_model_status_to_rpc_status(status)
                                    .into(),
                            }],
                        })
                    })?
            }
            rpc::spdm_list_attestation_machines_request::Variant::Selector(selector) => {
                let request_selector = rpc::SpdmListAttestationMachinesRequestSelector::try_from(
                    *selector,
                )
                .map_err(|_| Status::invalid_argument("invalid attestation machine selector"))?;

                let list_selector = match request_selector {
                    rpc::SpdmListAttestationMachinesRequestSelector::SpdmListInProgress => {
                        db::attestation::spdm::ListSelector::InProgress
                    }
                    rpc::SpdmListAttestationMachinesRequestSelector::SpdmListUnsuccessful => {
                        db::attestation::spdm::ListSelector::Unsuccessful
                    }
                };
                db::attestation::spdm::list_all_attestation_statuses(&mut txn, list_selector)
                    .await
                    .map(|statuses| {
                        Response::new(rpc::SpdmListAttestationMachinesResponse {
                            statuses: statuses
                                .iter()
                                .map(|elem| rpc::SpdmMachineAttestationStatus {
                                    machine_id: Some(elem.0),
                                    attestation_status: convert_model_status_to_rpc_status(elem.1)
                                        .into(),
                                })
                                .collect(),
                        })
                    })?
            }
        },
    };

    txn.commit().await?;

    Ok(response)
}

pub(crate) async fn get_attestation_machine(
    api: &Api,
    request: tonic::Request<MachineId>,
) -> Result<Response<rpc::SpdmGetAttestationMachineResponse>, Status> {
    log_request_data(&request);

    let machine_id = request.get_ref();
    log_machine_id(machine_id);

    let mut txn = api.txn_begin().await?;
    let attestations_details =
        db::attestation::spdm::get_attestations_for_machine_id(&mut txn, machine_id).await?;
    txn.commit().await?;

    Ok(Response::new(rpc::SpdmGetAttestationMachineResponse {
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
    let machine_id =
        crate::handlers::utils::convert_and_log_machine_id(request.machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;

    let ak_pub_bytes =
        match db::attestation::secret_ak_pub::get_by_secret(&mut txn, &request.credential).await? {
            Some(entry) => entry.ak_pub,
            None => {
                return Err(CarbideError::AttestQuoteError(
                    "could not form SQL query to fetch AK pub".into(),
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
