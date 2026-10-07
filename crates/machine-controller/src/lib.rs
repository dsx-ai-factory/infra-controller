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

//! State Controller implementation for Machines

use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Duration, Utc};
use db::attestation::ek_cert_verification_status;
use db::db_read::DbReader;
use db::measured_boot::machine::{get_measurement_bundle_state, get_measurement_machine_state};
use eyre::eyre;
use measured_boot::records::{MeasurementBundleState, MeasurementMachineState};
use model::attestation::EkCertVerificationStatus;
use model::machine::{
    FailureCause, FailureDetails, FailureSource, MeasuringState, StateMachineArea,
};
use state_controller::state_handler::StateHandlerError;

pub mod boot_interface;
pub mod config;
pub mod context;
pub mod dpf;
pub mod dpu_service_sync;
pub mod handler;
pub mod health_report;
pub mod io;
pub mod metrics;
pub mod per_object;
pub mod redfish;
pub(crate) mod rpc;
pub(crate) mod scout_firmware_scripts;
pub mod write_ops;

/// Fields of span that should be logged for each message.
pub fn extra_logfmt_logging_fields() -> Vec<String> {
    vec!["object_id".to_string()]
}

/// get_measurement_failure_cause gets the currently associated
/// measurement bundle for a given machine ID (if one exists), and
/// maps the state of the bundle to a FailureCause.
///
/// This is intended to be used when a machine is in MeasuringFailed,
/// and we want to dig into the corresponding bundle state to see why
/// it failed (e.g. retired or revoked).
///
/// If a bundle is not associated, or the associated bundle is not
/// in a retired or revoked state, then this will return an error.
///
/// TODO(chet): There's probably a world where the bundle state
/// could be stored in the journal entry itself (so there's no need
/// for a subsequent query), or a world where I introduce some
/// ComposedState type of thing where I can join across the journal
/// and bundle to do a single query + return a single ComposedState
/// that has everything I want.
async fn get_measurement_failure_cause<DB>(
    db: &mut DB,
    machine_id: &MachineId,
) -> Result<FailureCause, StateHandlerError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let (_, ek_cert_status) = get_measuring_prerequisites(machine_id, &mut *db).await?;
    if !ek_cert_status.signing_ca_found {
        return Ok(FailureCause::MeasurementsCAValidationFailed {
            err: "Measuremets CA Validation Failed".to_string(),
        });
    }

    let state = get_measurement_bundle_state(db, machine_id)
        .await
        .map_err(StateHandlerError::GenericError)?
        .ok_or(StateHandlerError::MissingData {
            object_id: machine_id.to_string(),
            missing: "expected bundle reference from journal for failed measurements",
        })?;

    let failure_cause = match state {
        MeasurementBundleState::Retired => FailureCause::MeasurementsRetired {
            err: "measurements matched retired bundle".to_string(),
        },
        MeasurementBundleState::Revoked => FailureCause::MeasurementsRevoked {
            err: "measurements matched revoked bundle".to_string(),
        },
        _ => {
            return Err(StateHandlerError::MissingData {
                object_id: machine_id.to_string(),
                missing: "expected retired or revoked bundle for failure cause",
            });
        }
    };

    Ok(failure_cause)
}

#[derive(PartialEq, Debug)]
pub enum MeasuringOutcome {
    PassedOk,
    NoChange,
    WaitForGoldenValues,
    WaitForScoutToSendMeasurements,
    Unsuccessful((FailureDetails, MachineId)),
}

async fn get_measuring_prerequisites<DB>(
    machine_id: &MachineId,
    db_reader: &mut DB,
) -> Result<(MeasurementMachineState, EkCertVerificationStatus), StateHandlerError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let machine_state = get_measurement_machine_state(&mut *db_reader, *machine_id)
        .await
        .map_err(StateHandlerError::from)?;

    let ek_cert_verification_status =
        ek_cert_verification_status::get_by_machine_id(&mut *db_reader, *machine_id)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!(
                    "no EkCertVerificationStatus found for MachineId {} due to error: {}",
                    machine_id,
                    e
                ))
            })?
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: machine_id.to_string(),
                missing: "ek_cert_verification_status",
            })?;

    Ok((machine_state, ek_cert_verification_status))
}

pub async fn handle_measuring_state<DB>(
    measuring_state: &MeasuringState,
    machine_id: &MachineId,
    db: &mut DB,
    attestation_enabled: bool,
    state_entered_at: DateTime<Utc>,
    waiting_for_measurements_timeout: Duration,
) -> Result<MeasuringOutcome, StateHandlerError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    if !attestation_enabled {
        return Ok(MeasuringOutcome::PassedOk);
    }
    let (machine_state, ek_cert_verification_status) =
        get_measuring_prerequisites(machine_id, &mut *db).await?;

    if !ek_cert_verification_status.signing_ca_found {
        return Ok(MeasuringOutcome::Unsuccessful((
            FailureDetails {
                cause: FailureCause::MeasurementsCAValidationFailed {
                    err: format!("The EK for MachineId {machine_id} has not been CA verified"),
                },
                failed_at: chrono::Utc::now(),
                source: FailureSource::StateMachineArea(StateMachineArea::Default),
            },
            *machine_id,
        )));
    }

    match measuring_state {
        // In this state, the machine has been discovered, and the
        // API is now waiting for a measurement report, which is up
        // to Scout to send (as part of reacting to an Action::Measure
        // response from the API).
        //
        // Once a measurement report is received, it will be verified
        // (where if that fails, will move to ManagedHostState::Failed
        // with a MeasurementsFailedSignatureCheck reason), and matched
        // against a bundle.
        //
        // If no match is found, then the machine will hang out in
        // MeasuringState::PendingBundle.
        MeasuringState::WaitingForMeasurements => {
            Ok(match machine_state {
                // "Discovered" is the MeasurementMachineState equivalent of
                // "no measurements have been sent yet". If that's the case,
                // then continue waiting — unless the timeout has elapsed, in
                // which case escalate to Failed so the host becomes visible.
                MeasurementMachineState::Discovered => {
                    let elapsed = Utc::now().signed_duration_since(state_entered_at);
                    if elapsed >= waiting_for_measurements_timeout {
                        MeasuringOutcome::Unsuccessful((
                            FailureDetails {
                                cause: FailureCause::MeasurementsNotReceived {
                                    err: format!(
                                        "no measurement report received after {elapsed}; \
                                         host may not be network-booting"
                                    ),
                                },
                                failed_at: Utc::now(),
                                source: FailureSource::StateMachineArea(StateMachineArea::Default),
                            },
                            *machine_id,
                        ))
                    } else {
                        MeasuringOutcome::NoChange
                    }
                }
                MeasurementMachineState::PendingBundle => MeasuringOutcome::WaitForGoldenValues,
                MeasurementMachineState::Measured => MeasuringOutcome::PassedOk,
                MeasurementMachineState::MeasuringFailed => MeasuringOutcome::Unsuccessful((
                    FailureDetails {
                        cause: get_measurement_failure_cause(db, machine_id).await?,
                        failed_at: chrono::Utc::now(),
                        source: FailureSource::StateMachineArea(StateMachineArea::Default),
                    },
                    *machine_id,
                )),
            })
        }
        // In this state, a measurement report of PCR values has been
        // received (and verified), but the values don't match a bundle
        // yet. Check to see if they match one now (which happens via
        // bundle promotion). If not, keep on waiting.
        MeasuringState::PendingBundle => {
            Ok(match machine_state {
                // "PendingBundle" is the current state, so if this is returned,
                // just keep on waiting for a matching bundle.
                MeasurementMachineState::PendingBundle => MeasuringOutcome::NoChange,
                // "Discovered" is the MeasurementMachineState equivalent of
                // "no measurements have been sent yet". If this is happens,
                // it means measurements must have been wiped, so lets transition
                // *back* to WaitingForMeasurements (which will tell the API to
                // ask Scout for measurements again).
                MeasurementMachineState::Discovered => {
                    MeasuringOutcome::WaitForScoutToSendMeasurements
                }
                MeasurementMachineState::Measured => MeasuringOutcome::PassedOk,
                MeasurementMachineState::MeasuringFailed => MeasuringOutcome::Unsuccessful((
                    FailureDetails {
                        cause: get_measurement_failure_cause(db, machine_id).await?,
                        failed_at: chrono::Utc::now(),
                        source: FailureSource::StateMachineArea(StateMachineArea::Default),
                    },
                    *machine_id,
                )),
            })
        }
    }
}
