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

use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use carbide_uuid::machine::MachineId;
use chrono::{DateTime, SubsecRound, Utc};
use config_version::ConfigVersion;
use itertools::Itertools;
use libredfish::model::component_integrity::{ComponentIntegrities, ComponentIntegrity};
use model::attestation::profile::{AttesterSelectionMode, ProfileResolution, SelectionOutcome};
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

/// What scheduling decided for one machine.
///
/// A BMC that cannot be reached is not one of these: it stays an error so the
/// controller retries it, where every value here is a settled answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, carbide_instrument::LabelValue)]
pub enum SchedulingOutcome {
    /// One work row was written per selected attester.
    Scheduled,
    /// The profile's mode is `NONE`. The BMC is not contacted.
    AttestationDisabled,
    /// The BMC offered nothing eligible, under a policy that asserted nothing
    /// about what must be there. Points at the hardware, not the profile.
    NoAttestersFound,
    /// An operator-authored requirement went unsatisfied.
    PolicyMatchedNothing,
    /// No exploration has recorded a hardware class for this machine's BMC.
    ClassNotRecorded,
    /// Neither the machine's class nor `any` has a profile.
    NoProfile,
    /// Classification matched no `HwType`, and no `any` profile is stored.
    ClassUnrecognized,
}

/// One scheduling attempt, in the terms the trigger response reports.
#[derive(Debug)]
pub struct SchedulingResult {
    pub outcome: SchedulingOutcome,
    /// The class recorded for the BMC, empty when none was.
    pub hardware_class: String,
    /// Whether the reserved `any` profile supplied the policy. The class alone
    /// cannot say, and an operator reading a surprising outcome needs to know
    /// whether a policy was written for this hardware or inherited.
    pub used_any_fallback: bool,
    /// Version of the profile that decided, absent when none applied. Pins the
    /// revision behind a surprising outcome, which the class cannot: a profile
    /// may have been edited between the operator's last read and this call.
    pub profile_version: Option<String>,
    /// The `started_at` written on every device row this call scheduled,
    /// absent when it scheduled none. Identifies the scheduling run, so a
    /// caller can tell its own from one that replaced it.
    pub scheduled_at: Option<DateTime<Utc>>,
    pub devices_scheduled: u64,
}

impl SchedulingResult {
    /// A settled answer reached before any profile applied.
    fn unprofiled(outcome: SchedulingOutcome, hardware_class: Option<String>) -> Self {
        Self {
            outcome,
            hardware_class: hardware_class.unwrap_or_default(),
            used_any_fallback: false,
            profile_version: None,
            scheduled_at: None,
            devices_scheduled: 0,
        }
    }
}

/// Counted by outcome so a site accumulating unprofiled hardware, or a profile
/// nothing satisfies, shows up on a graph rather than only in logs.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "attestation_scheduled",
    metric_name = "carbide_attestation_scheduling_total",
    component = "machine-controller",
    log = info,
    metric = counter,
    message = "SPDM attestation scheduling finished",
    describe = "Number of SPDM attestation scheduling attempts, by outcome"
)]
struct AttestationScheduled {
    #[label]
    outcome: SchedulingOutcome,
    #[context]
    machine_id: MachineId,
    #[context]
    hardware_class: String,
    #[context]
    used_any_fallback: bool,
    /// The only durable record of which revision decided: nothing persists it
    /// on the work rows, and a later trigger replaces them.
    #[context]
    profile_version: Option<String>,
    #[context]
    devices_scheduled: u64,
}

/// Schedules SPDM attestation for a machine according to the profile its
/// hardware class resolves to, writing one work row per selected attester.
pub async fn trigger_attestation(
    db_pool: &PgPool,
    redfish_client: Box<dyn libredfish::Redfish>,
    bmc_info: &BmcInfo,
    machine_id: &MachineId,
    redfish_timeout_duration: std::time::Duration,
) -> Result<SchedulingResult, StateHandlerError> {
    let result = schedule(
        db_pool,
        redfish_client,
        bmc_info,
        machine_id,
        redfish_timeout_duration,
    )
    .await?;

    carbide_instrument::emit(AttestationScheduled {
        outcome: result.outcome,
        machine_id: *machine_id,
        hardware_class: result.hardware_class.clone(),
        used_any_fallback: result.used_any_fallback,
        profile_version: result.profile_version.clone(),
        devices_scheduled: result.devices_scheduled,
    });

    Ok(result)
}

async fn schedule(
    db_pool: &PgPool,
    redfish_client: Box<dyn libredfish::Redfish>,
    bmc_info: &BmcInfo,
    machine_id: &MachineId,
    redfish_timeout_duration: std::time::Duration,
) -> Result<SchedulingResult, StateHandlerError> {
    let bmc_address = bmc_info
        .ip_addr()
        .map_err(StateHandlerError::GenericError)?;
    let mut conn = db_pool.acquire().await?;

    let recorded_class =
        db::explored_endpoints::lookup_hardware_class_by_ip(bmc_address, &mut *conn)
            .await?
            .flatten();

    let resolution =
        db::attestation_profile::resolve(&mut *conn, recorded_class.as_deref()).await?;
    // The BMC round trip below can take as long as the caller's timeout
    // allows, which is no reason to hold a pool connection.
    drop(conn);

    let (profile, used_any_fallback) = match resolution {
        ProfileResolution::Resolved {
            profile,
            used_any_fallback,
        } => (profile, used_any_fallback),
        ProfileResolution::ClassNotRecorded => {
            return Ok(SchedulingResult::unprofiled(
                SchedulingOutcome::ClassNotRecorded,
                recorded_class,
            ));
        }
        ProfileResolution::NoProfile => {
            return Ok(SchedulingResult::unprofiled(
                SchedulingOutcome::NoProfile,
                recorded_class,
            ));
        }
        ProfileResolution::ClassUnrecognized => {
            return Ok(SchedulingResult::unprofiled(
                SchedulingOutcome::ClassUnrecognized,
                recorded_class,
            ));
        }
    };

    let hardware_class = recorded_class.unwrap_or_default();
    let profile_version = profile.version.to_string();
    let settled = |outcome| SchedulingResult {
        outcome,
        hardware_class: hardware_class.clone(),
        used_any_fallback,
        profile_version: Some(profile_version.clone()),
        scheduled_at: None,
        devices_scheduled: 0,
    };
    let selection = &profile.policy_document.selection;

    // NONE is answered from the profile alone, so the BMC is never contacted.
    if selection.mode == AttesterSelectionMode::None {
        return Ok(settled(SchedulingOutcome::AttestationDisabled));
    }

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

    // A BMC without the collection has nothing to list, and asking anyway only
    // buys a 404.
    if service_root.component_integrity.is_none() {
        return Ok(settled(SchedulingOutcome::NoAttestersFound));
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

    let eligible = eligible_attesters(&component_integrities);
    let eligible_ids = eligible.iter().map(|c| c.id.as_str()).collect_vec();

    let selected = match selection.evaluate(&eligible_ids) {
        SelectionOutcome::Scheduled(selected) => selected,
        SelectionOutcome::AttestationDisabled => {
            return Ok(settled(SchedulingOutcome::AttestationDisabled));
        }
        SelectionOutcome::NoAttestersFound => {
            return Ok(settled(SchedulingOutcome::NoAttestersFound));
        }
        SelectionOutcome::PolicyMatchedNothing(requirement) => {
            tracing::warn!(
                %machine_id,
                %hardware_class,
                ?requirement,
                reported = ?eligible_ids,
                "attestation profile matched no eligible attester"
            );
            return Ok(settled(SchedulingOutcome::PolicyMatchedNothing));
        }
    };

    // The validation that list is not changed is done by SKU validation. SKU
    // validation checks that the device profile is not changed over time. If any
    // device list is changed and SKU validation is passed, means SRE has approved the
    // change request.
    // Validating again is not needed.
    // Remove existing device list and over-write with this list.
    // Truncated to what `timestamptz` stores, so the value reported back is
    // the one a caller finds on the rows rather than a sub-microsecond miss.
    let time_now = Utc::now().trunc_subsecs(6);
    let device_attestations = eligible
        .into_iter()
        .filter(|component| selected.contains(&component.id))
        .map(|x| from_component_integrity(x.clone(), machine_id, &time_now, bmc_info))
        .collect_vec();

    let mut txn = db_pool.begin().await?;

    let records_inserted = db::attestation::spdm::insert_device_attestations(
        &mut txn,
        machine_id,
        device_attestations,
    )
    .await?;

    txn.commit().await?;

    Ok(SchedulingResult {
        outcome: SchedulingOutcome::Scheduled,
        hardware_class,
        used_any_fallback,
        profile_version: Some(profile_version),
        scheduled_at: Some(time_now),
        devices_scheduled: records_inserted,
    })
}

/// The attesters a profile's patterns may select: those the BMC reports as
/// enabled and as speaking SPDM.
///
/// `ComponentIntegrityTypeVersion` is recorded rather than filtered on, so a
/// BMC reporting a newer version than this build knew about still attests.
fn eligible_attesters(integrities: &ComponentIntegrities) -> Vec<&ComponentIntegrity> {
    integrities
        .members
        .iter()
        .filter(|component| {
            component.component_integrity_enabled && component.component_integrity_type == "SPDM"
        })
        .collect()
}

fn from_component_integrity(
    integrity: ComponentIntegrity,
    machine_id: &MachineId,
    time_now: &DateTime<Utc>,
    bmc_info: &BmcInfo,
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
        attestation_status == SpdmAttestationStatus::InProgress
            || attestation_status == SpdmAttestationStatus::Cancelled
            || attestation_status == SpdmAttestationStatus::Passed
    };
    if should_resume_attestation {
        match &details.source {
            FailureSource::StateMachineArea(StateMachineArea::HostInit) => {
                Ok(StateHandlerOutcome::transition(ManagedHostState::HostInit {
                    machine_state: MachineState::SpdmMeasuring {
                        spdm_measuring_state: SpdmMeasuringState::PollResult,
                    },
                })
                .with_txn(txn))
            }
            FailureSource::StateMachineArea(StateMachineArea::AssignedInstance) => Ok(
                StateHandlerOutcome::transition(ManagedHostState::PostAssignedMeasuring {
                    attestation_mode: AttestationMode::SpdmAttestation {
                        spdm_measuring_state: SpdmMeasuringState::PollResult,
                    },
                })
                .with_txn(txn),
            ),
            FailureSource::StateMachineArea(StateMachineArea::MainFlow) => Ok(
                StateHandlerOutcome::transition(ManagedHostState::PreAssignedMeasuring {
                    spdm_measuring_state: SpdmMeasuringState::PollResult,
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
    next_spdm_state: ManagedHostState,
    next_skip_state: ManagedHostState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    // create redfish client
    let redfish_client = services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;

    let result = trigger_attestation(
        &services.db_pool,
        redfish_client,
        &mh_snapshot.host_snapshot.status.bmc_info,
        host_machine_id,
        std::time::Duration::MAX,
    )
    .await?;

    // Every outcome other than Scheduled left no work to poll for, whether
    // because the operator asked for none or because none could be selected.
    // `trigger_attestation` has already reported which, so the machine
    // proceeds rather than waiting on results that will never arrive.
    if result.outcome == SchedulingOutcome::Scheduled {
        Ok(StateHandlerOutcome::transition(next_spdm_state))
    } else {
        Ok(StateHandlerOutcome::transition(next_skip_state))
    }
}

pub(crate) async fn handle_spdm_poll_state(
    db_pool: &PgPool,
    host_machine_id: &MachineId,
    failure_source: FailureSource,
    next_skip_state: ManagedHostState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let mut txn = db_pool.begin().await?;

    // get attestation status for the entire machine
    let attestation_status =
        db::attestation::spdm::list_single_machine_attestation_status(&mut txn, host_machine_id)
            .await?;

    // passed or cancelled -> just move to the next state
    // failed -> get states for all devices and log to the Failed state logging them there
    match attestation_status {
        SpdmAttestationStatus::Passed | SpdmAttestationStatus::Cancelled => {
            Ok(StateHandlerOutcome::transition(next_skip_state).with_txn(txn))
        }
        SpdmAttestationStatus::Failed => {
            let attestation_states =
                db::attestation::spdm::get_attestations_for_machine_id(&mut txn, host_machine_id)
                    .await?;
            // here, move to failed state with a full details
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
