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

use std::collections::BTreeMap;

use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use itertools::Itertools;
use libredfish::model::component_integrity::{ComponentIntegrities, ComponentIntegrity};
use model::attestation::spdm::{
    SpdmAttestationState, SpdmAttestationStatus, SpdmDeviceAttestation,
    SpdmDeviceAttestationDetails,
};
use model::bmc_info::BmcInfo;
use model::machine::{
    AttestationMode, FailureCause, FailureDetails, FailureSource, MachineState, ManagedHostState,
    ManagedHostStateSnapshot, SpdmMeasuringState, StateMachineArea,
};
use sqlx::PgPool;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::MachineStateHandlerContextObjects;
use crate::handler::MachineStateHandlerServices;

const PRODUCT_GB200: &str = "GB200 NVL";
const PRODUCT_GB300: &str = "GB300 NVL";

pub async fn trigger_attestation(
    db_pool: &PgPool,
    redfish_client: Box<dyn libredfish::Redfish>,
    bmc_info: &BmcInfo,
    machine_id: &MachineId,
    redfish_timeout_duration: std::time::Duration,
) -> Result<Option<(u64, uuid::Uuid)>, StateHandlerError> {
    // retrieve bmc info for a machine and create redfish client
    // get service root
    // - absent -> return NotSupported
    // get component integrities and create/insert device attestations
    // - if none, return NotSupported

    let service_root_future = redfish_client.get_service_root();

    let service_root = match tokio::time::timeout(redfish_timeout_duration, service_root_future)
        .await
    {
        Ok(redfish_result) => redfish_result.map_err(|e| redfish_error("get service root", e))?,
        Err(_) => {
            return Err(StateHandlerError::GenericError(eyre::eyre!(
                "redfish service_root could not finish in {} seconds",
                redfish_timeout_duration.as_secs()
            )));
        }
    };

    if service_root.component_integrity.is_none() {
        // let's treat 0 devices under attestation as NotSupported
        return Ok(None);
    }

    // do we support attestation for a given machine type?
    // check the ServiceRoot.Product
    let product = match service_root.product {
        Some(product_name) => product_name,
        None => {
            tracing::info!(
                %machine_id,
                "ServiceRoot.Product is None; not scheduling SPDM attestation"
            );
            return Ok(None);
        }
    };

    if !is_supported_product(&product) {
        tracing::info!(
            %machine_id,
            %product,
            "ServiceRoot.Product is not supported; not scheduling SPDM attestation"
        );
        return Ok(None);
    }

    let component_integrities_future = redfish_client.get_component_integrities();

    let component_integrities =
        match tokio::time::timeout(redfish_timeout_duration, component_integrities_future).await {
            Ok(redfish_result) => {
                redfish_result.map_err(|e| redfish_error("get component integrities", e))?
            }
            Err(_) => {
                return Err(StateHandlerError::GenericError(eyre::eyre!(
                    "redfish get_component_integrities could not finish in {} seconds",
                    redfish_timeout_duration.as_secs()
                )));
            }
        };

    let components = get_supported_components(&product, &component_integrities);

    if components.is_empty() {
        // let's treat 0 devices under attestation as NotSupported
        return Ok(None);
    }

    // The validation that list is not changed is done by SKU validation. SKU
    // validation checks that the device profile is not changed over time. If any
    // device list is changed and SKU validation is passed, means SRE has approved the
    // change request.
    // Validating again is not needed.
    // Remove existing device list and over-write with this list.
    let time_now = Utc::now();
    let job_id = uuid::Uuid::new_v4();
    let device_attestations = components
        .into_iter()
        .map(|x| from_component_integrity(x.clone(), machine_id, &time_now, bmc_info, &job_id))
        .collect_vec();

    let mut txn = db_pool.begin().await?;

    let records_inserted = db::attestation::spdm::insert_device_attestations(
        &mut txn,
        machine_id,
        device_attestations,
        &job_id,
    )
    .await?;

    txn.commit().await?;

    tracing::info!(
        %machine_id,
        inserted_record_count = records_inserted,
        "SPDM attestation commenced; scheduled SPDM device attestations"
    );

    Ok(Some((records_inserted, job_id)))
}

// Rules:
// ComponentIntegrityTypeVersion should be >= 1.1.0.
// ComponentIntegrityType should be SPDM.
// ComponentIntegrityEnabled should be true.
// A device must be of supported type.
// Once these all conditions are true, a device can be proceed with attestation.
fn get_supported_components<'a>(
    product: &str,
    integrities: &'a ComponentIntegrities,
) -> Vec<&'a ComponentIntegrity> {
    let supported_devices = BTreeMap::from([(PRODUCT_GB200, ["HGX_IRoT_GPU"])]);

    let supported_versions = ["1.1.0"]; // This can be configurable value.
    let mut supported_components = vec![];

    for component in &integrities.members {
        if !component.component_integrity_enabled {
            // Component Integrity is not enabled
            continue;
        }

        if component.component_integrity_type != "SPDM" {
            // Not SPDM, may be TPM.
            continue;
        }

        if !supported_versions.contains(&component.component_integrity_type_version.as_str()) {
            continue;
        }

        let is_supported = match supported_devices.get(product) {
            Some(device_id_stems) => device_id_stems
                .iter()
                .any(|device_id_stem| component.id.contains(device_id_stem)),
            None => false,
        };

        if !is_supported {
            continue;
        }

        supported_components.push(component);
    }

    supported_components
}

fn is_supported_product(product: &str) -> bool {
    matches!(product, PRODUCT_GB200 | PRODUCT_GB300)
}

fn from_component_integrity(
    integrity: ComponentIntegrity,
    machine_id: &MachineId,
    time_now: &DateTime<Utc>,
    bmc_info: &BmcInfo,
    job_id: &uuid::Uuid,
) -> SpdmDeviceAttestation {
    let ca_certificate_link = integrity
        .spdm
        .map(|x| x.identity_authentication)
        .map(|x| x.responder_authentication.component_certificate)
        .map(|x| x.odata_id);

    let evidence_target =
        if let Some(Some(data)) = integrity.actions.map(|x| x.get_signed_measurements) {
            Some(data.target)
        } else {
            None
        };

    SpdmDeviceAttestation {
        machine_id: *machine_id,
        device_id: integrity.id,
        nonce: uuid::Uuid::new_v4(),
        bmc_info: bmc_info.clone(),
        state: SpdmAttestationState::FetchMetadata,
        state_version: ConfigVersion::initial(),
        state_outcome: None,
        metadata: None,
        ca_certificate_link,
        ca_certificate: None,
        evidence_target,
        evidence: None,
        evidence_collection_locked: false,
        job_id: *job_id,
        started_at: *time_now,
        cancelled_at: None,
        completed_at: None,
    }
}

/// When SPDM attestation failed, check whether attestation was restarted (admin / status) or
/// disabled in config; if so, transition back to the appropriate measuring state based on
/// [`FailureDetails::source`].
pub(crate) async fn handle_spdm_attestation_failed_recovery(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    host_machine_id: &MachineId,
    details: &FailureDetails,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;
    let should_resume_attestation = if !ctx.services.site_config.spdm_enabled {
        true
    } else {
        let attestation_status = db::attestation::spdm::list_single_machine_attestation_status(
            &mut txn,
            host_machine_id,
        )
        .await?;
        attestation_status == SpdmAttestationStatus::Passed
    };
    if should_resume_attestation {
        match &details.source {
            FailureSource::StateMachineArea(StateMachineArea::HostInit) => {
                Ok(StateHandlerOutcome::transition(ManagedHostState::HostInit {
                    machine_state: MachineState::SpdmMeasuring {
                        spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                    },
                })
                .with_txn(txn))
            }
            FailureSource::StateMachineArea(StateMachineArea::AssignedInstance) => Ok(
                StateHandlerOutcome::transition(ManagedHostState::PostAssignedMeasuring {
                    attestation_mode: AttestationMode::SpdmAttestation {
                        spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                    },
                })
                .with_txn(txn),
            ),
            FailureSource::StateMachineArea(StateMachineArea::MainFlow) => Ok(
                StateHandlerOutcome::transition(ManagedHostState::PreAssignedMeasuring {
                    spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                })
                .with_txn(txn),
            ),
            _ => Ok(StateHandlerOutcome::do_nothing()),
        }
    } else {
        Ok(StateHandlerOutcome::do_nothing())
    }
}

pub(crate) async fn handle_spdm_trigger_state(
    services: &MachineStateHandlerServices,
    mh_snapshot: &mut ManagedHostStateSnapshot,
    host_machine_id: &MachineId,
) -> Result<Option<uuid::Uuid>, StateHandlerError> {
    // create redfish client
    let redfish_client = services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;

    let attestation_started = trigger_attestation(
        &services.db_pool,
        redfish_client,
        &mh_snapshot.host_snapshot.status.bmc_info,
        host_machine_id,
        std::time::Duration::MAX,
    )
    .await?;

    // if 0 devices scheduled - this means it is unsupported
    // so we just proceed to the next state

    if let Some((_, job_id)) = attestation_started {
        Ok(Some(job_id))
    } else {
        tracing::info!(
            machine_id = %host_machine_id,
            "No devices scheduled for SPDM attestation"
        );
        Ok(None)
    }
}

pub(crate) async fn handle_spdm_poll_state(
    db_pool: &PgPool,
    host_machine_id: &MachineId,
    job_id: &uuid::Uuid,
    failure_source: FailureSource,
    next_skip_state: ManagedHostState,
    restart_state: ManagedHostState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let mut txn = db_pool.begin().await?;

    // A completed job may be removed before the machine controller reads its result.
    // Restart in the caller's lifecycle stage; the next iteration creates a fresh job.
    let attestation_status =
        match db::attestation::spdm::get_attestation_job_status(&mut txn, job_id).await {
            Ok(status) => status,
            Err(db::DatabaseError::NotFoundError { .. }) => {
                tracing::info!(
                    machine_id = %host_machine_id,
                    %job_id,
                    "SPDM attestation job not found; restarting attestation"
                );
                return Ok(StateHandlerOutcome::transition(restart_state).with_txn(txn));
            }
            Err(error) => return Err(error.into()),
        };

    // passed or cancelled -> just move to the next state
    // failed -> get states for all devices and log to the Failed state logging them there
    match attestation_status {
        SpdmAttestationStatus::Passed | SpdmAttestationStatus::Cancelled => {
            Ok(StateHandlerOutcome::transition(next_skip_state).with_txn(txn))
        }
        SpdmAttestationStatus::Failed => {
            // The job can also disappear between the status and details queries.
            let attestation_states =
                match db::attestation::spdm::get_attestations_for_job_id(&mut txn, job_id).await {
                    Ok(states) => states,
                    Err(db::DatabaseError::NotFoundError { .. }) => {
                        tracing::info!(
                            machine_id = %host_machine_id,
                            %job_id,
                            "SPDM attestation job not found; restarting attestation"
                        );
                        return Ok(StateHandlerOutcome::transition(restart_state).with_txn(txn));
                    }
                    Err(error) => return Err(error.into()),
                };
            // here, move to failed state with full details
            Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
                details: FailureDetails {
                    cause: FailureCause::SpdmAttestationFailed {
                        err: attestation_states
                            .iter()
                            .filter(|elem| matches!(elem.state, SpdmAttestationState::Failed(_)))
                            .fold(
                                String::new(),
                                |mut accum, x: &SpdmDeviceAttestationDetails| {
                                    accum.push_str(&x.get_failure_cause().unwrap_or_default());
                                    accum.push_str(". ");
                                    accum
                                },
                            ),
                    },
                    failed_at: Utc::now(),
                    source: failure_source,
                },
                retry_count: 0,
                machine_id: *host_machine_id,
            })
            .with_txn(txn))
        }
        SpdmAttestationStatus::InProgress => Ok(StateHandlerOutcome::do_nothing()),
    }
}
