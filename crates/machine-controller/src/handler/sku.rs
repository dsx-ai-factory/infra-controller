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

use chrono::Utc;
use health_report::HealthReport;
use model::machine::{
    BomValidating, BomValidatingContext, MachineState, MachineValidatingState, ManagedHostState,
    ManagedHostStateSnapshot, ValidationState,
};
use model::sku::diff_skus;
use sqlx::{PgConnection, PgTransaction};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::{MachineStateHandlerContextObjects, MachineStateHandlerServices};
use crate::handler::{
    HostHandlerParams, discovered_after_state_transition, trigger_reboot_if_needed,
};

fn get_bom_validation_context(state: &ManagedHostState) -> BomValidatingContext {
    if let ManagedHostState::BomValidating {
        bom_validating_state,
    } = state
    {
        match bom_validating_state {
            BomValidating::MatchingSku(bom_validating_context)
            | BomValidating::UpdatingInventory(bom_validating_context)
            | BomValidating::VerifyingSku(bom_validating_context)
            | BomValidating::SkuVerificationFailed(bom_validating_context)
            | BomValidating::WaitingForSkuAssignment(bom_validating_context)
            | BomValidating::SkuMissing(bom_validating_context) => bom_validating_context.clone(),
        }
    } else {
        BomValidatingContext {
            machine_validation_context: None,
            reboot_retry_count: None,
        }
    }
}

async fn clear_sku_validation_report(
    txn: &mut PgConnection,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<(), StateHandlerError> {
    let health_report = HealthReport::empty(HealthReport::SKU_VALIDATION_SOURCE.to_string());

    Ok(db::machine::update_sku_validation_health_report(
        txn,
        &mh_snapshot.host_snapshot.id,
        &health_report,
    )
    .await?)
}

async fn match_sku_for_machine(
    txn: &mut PgConnection,
    host_handler_params: &HostHandlerParams,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<Option<model::sku::Sku>, StateHandlerError> {
    let sku_status = mh_snapshot.host_snapshot.status.hw_sku.as_ref();
    if sku_status.is_none_or(|ss| {
        ss.last_match_attempt.is_none_or(|t| {
            t < (Utc::now() - host_handler_params.bom_validation.find_match_interval)
        })
    }) {
        let machine_sku =
            db::sku::generate_sku_from_machine(&mut *txn, &mh_snapshot.host_snapshot.id).await?;
        let matching_sku = db::sku::find_matching(txn, &machine_sku).await?;
        if matching_sku.is_none() {
            // only update the last attempt if there is no match
            db::machine::update_sku_status_last_match_attempt(txn, &mh_snapshot.host_snapshot.id)
                .await?;
        }
        Ok(matching_sku)
    } else {
        Ok(None)
    }
}

async fn generate_missing_sku_for_machine(
    txn: &mut PgConnection,
    host_handler_params: &HostHandlerParams,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> bool {
    if !host_handler_params.bom_validation.auto_generate_missing_sku {
        return false;
    }
    let Some(sku_id) = mh_snapshot.host_snapshot.config.hw_sku.as_ref() else {
        tracing::debug!(
            machine_id = %mh_snapshot.host_snapshot.id,
            "No SKU assigned"
        );
        return false;
    };

    // its unlikely we got here without a bmc mac
    let Some(bmc_mac_address) = mh_snapshot.host_snapshot.status.bmc_info.mac else {
        tracing::debug!(
            machine_id = %mh_snapshot.host_snapshot.id,
            "No BMC MAC address configured"
        );
        return false;
    };

    // if there's no expected machine, no SKU in it, or the SKU doesn't match what's assigned to the machine, don't generate a SKU
    if db::expected_machine::find_by_bmc_mac_address(&mut *txn, bmc_mac_address)
        .await
        .ok()
        .flatten()
        .is_none_or(|em| em.data.sku_id.as_ref().is_none_or(|id| id != sku_id))
    {
        tracing::debug!(
            machine_id = %mh_snapshot.host_snapshot.id,
            %bmc_mac_address,
            "No expected machine for bmc"
        );
        return false;
    }

    let sku_status = mh_snapshot.host_snapshot.status.hw_sku.as_ref();
    if sku_status.is_some_and(|ss| {
        ss.last_generate_attempt.is_some_and(|t| {
            t > (Utc::now()
                - host_handler_params
                    .bom_validation
                    .auto_generate_missing_sku_interval)
        })
    }) {
        tracing::debug!(machine_id=%mh_snapshot.host_snapshot.id, "Last generation attempt is too recent");
        return false;
    }

    if let Err(e) =
        db::machine::update_sku_status_last_generate_attempt(txn, &mh_snapshot.host_snapshot.id)
            .await
    {
        tracing::error!(
            machine_id=%mh_snapshot.host_snapshot.id,
            error=%e,
            "Failed to get SKU status for machine",
        );
    } else {
        let generated_sku = match db::sku::generate_sku_from_machine(
            &mut *txn,
            &mh_snapshot.host_snapshot.id,
        )
        .await
        {
            Ok(mut sku) => {
                sku.id = sku_id.clone();
                sku
            }
            Err(e) => {
                tracing::error!(
                    machine_id=%mh_snapshot.host_snapshot.id,
                    error=%e,
                    "Failed to generate SKU for machine",
                );
                return false;
            }
        };
        // Create checks for the existance of a duplicate SKU with a different name under a lock.
        if let Err(e) = db::sku::create(txn, &generated_sku).await {
            tracing::error!(
                machine_id=%mh_snapshot.host_snapshot.id,
                error=%e,
                "Failed to create generated SKU for machine",
            );
        }
    }
    true
}

/// Determine whether a machine with an assigned SKU can allocate despite validation failure.
fn should_allow_allocation_on_validation_failure(host_handler_params: &HostHandlerParams) -> bool {
    host_handler_params
        .bom_validation
        .allow_allocation_on_validation_failure
}

/// Determine whether a machine without an assigned SKU can bypass BOM validation.
fn should_ignore_unassigned_machine(host_handler_params: &HostHandlerParams) -> bool {
    host_handler_params
        .bom_validation
        .ignore_unassigned_machines
}

pub(crate) async fn handle_bom_validation_requested(
    host_handler_params: &HostHandlerParams,
    mh_snapshot: &ManagedHostStateSnapshot,
    services: &MachineStateHandlerServices,
) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
    if !host_handler_params.bom_validation.enabled {
        tracing::debug!("BOM validation disabled");
        return Ok(None);
    }

    let mut txn = services.db_pool.begin().await?;
    // Case 1: Machine has no SKU assigned
    if mh_snapshot.host_snapshot.config.hw_sku.is_none() {
        // Always try to find a matching SKU for machine regardless of configs
        if let Some(sku) = match_sku_for_machine(&mut txn, host_handler_params, mh_snapshot).await?
        {
            // Case 1.1: Found a matching SKU
            tracing::info!(
                machine_id=%mh_snapshot.host_snapshot.id,
                sku_id=%sku.id,
                "Possible SKU match found, attempting verification"
            );
            // A possible match has been found but inventory may be out of date.
            // Update the machine's inventory and do the match again
            return advance_to_updating_inventory(txn, mh_snapshot)
                .await
                .map(Some);
        } else {
            // Case 1.2: Cannot find a matching SKU
            tracing::info!(
                machine_id=%mh_snapshot.host_snapshot.id,
                "Cannot find a matching SKU for machine"
            );

            if should_ignore_unassigned_machine(host_handler_params) {
                // Case 1.2.1: Allow allocation without an assigned SKU
                tracing::info!(
                    machine_id=%mh_snapshot.host_snapshot.id,
                    "ignore_unassigned_machines is true, staying in Ready state"
                );
                txn.commit().await?;
                return Ok(None);
            } else {
                // Case 1.2.2: Block allocation, wait for SKU assignment
                return advance_to_waiting_for_sku_assignment(
                    txn,
                    mh_snapshot,
                    host_handler_params,
                )
                .await
                .map(Some);
            }
        }
    }

    // Case 2: Machine has SKU assigned
    let sku_id = mh_snapshot.host_snapshot.config.hw_sku.as_ref().unwrap();

    // Case 2.1: Verification explicitly requested
    // If there is a request for verification pending, update the inventory regardless of other configs
    if let Some(verify_request_time) = mh_snapshot
        .host_snapshot
        .status
        .hw_sku
        .as_ref()
        .and_then(|ss| ss.verify_request_time)
        && verify_request_time > mh_snapshot.host_snapshot.state.version.timestamp()
    {
        tracing::info!(
            machine_id=%mh_snapshot.host_snapshot.id,
            sku_id=%sku_id,
            "Verify SKU requested, attempting verification"
        );
        return advance_to_updating_inventory(txn, mh_snapshot)
            .await
            .map(Some);
    }

    // Case 2.2: Check if the assigned SKU got deleted from the database
    if db::sku::find(&mut txn, std::slice::from_ref(sku_id))
        .await?
        .is_empty()
    {
        if should_allow_allocation_on_validation_failure(host_handler_params) {
            // Case 2.2.1: Allow allocation despite missing SKU
            tracing::info!(
                machine_id=%mh_snapshot.host_snapshot.id,
                sku_id=%sku_id,
                "Assigned SKU does not exist, but allow_allocation_on_validation_failure is true, staying in Ready state"
            );
            return Ok(None);
        } else {
            // Case 2.2.2: Block allocation, transition to SkuMissing state
            return advance_to_sku_missing(txn, mh_snapshot).await.map(Some);
        }
    }

    // Case 2.3: SKU exists and no verification requested
    // Stay in current state, no action needed
    Ok(None)
}

async fn advance_to_sku_missing(
    mut txn: PgTransaction<'static>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let bom_validation_context =
        get_bom_validation_context(mh_snapshot.host_snapshot.current_state());
    let health_report = HealthReport::sku_missing(
        mh_snapshot
            .host_snapshot
            .config
            .hw_sku
            .as_deref()
            .unwrap_or_default(),
    );

    db::machine::update_sku_validation_health_report(
        &mut txn,
        &mh_snapshot.host_snapshot.id,
        &health_report,
    )
    .await?;

    Ok(
        StateHandlerOutcome::transition(ManagedHostState::BomValidating {
            bom_validating_state: BomValidating::SkuMissing(bom_validation_context),
        })
        .with_txn(txn),
    )
}

async fn advance_to_updating_inventory(
    mut txn: PgTransaction<'static>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let bom_validation_context =
        get_bom_validation_context(mh_snapshot.host_snapshot.current_state());

    db::machine_topology::set_topology_update_needed(&mut txn, &mh_snapshot.host_snapshot.id, true)
        .await?;

    Ok(
        StateHandlerOutcome::transition(ManagedHostState::BomValidating {
            bom_validating_state: BomValidating::UpdatingInventory(bom_validation_context),
        })
        .with_txn(txn),
    )
}

async fn advance_to_waiting_for_sku_assignment(
    txn: PgTransaction<'static>,
    mh_snapshot: &ManagedHostStateSnapshot,
    host_handler_params: &HostHandlerParams,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    if should_ignore_unassigned_machine(host_handler_params)
        && mh_snapshot.host_snapshot.config.hw_sku.is_none()
    {
        skip_bom_validation_and_advance(
            txn,
            host_handler_params,
            mh_snapshot,
            "ignore_unassigned_machine_before_waiting",
        )
        .await
    } else {
        let bom_validation_context =
            get_bom_validation_context(mh_snapshot.host_snapshot.current_state());

        Ok(
            StateHandlerOutcome::transition(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(
                    bom_validation_context,
                ),
            })
            .with_txn(txn),
        )
    }
}

async fn advance_to_machine_validating(
    mut txn: PgTransaction<'static>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    // transitioning to machine validating with a None context is a bug.
    let context = get_bom_validation_context(mh_snapshot.host_snapshot.current_state());

    let Some(context) = context.machine_validation_context else {
        tracing::info!("SKU verification complete; Skipping machine validation");
        return Ok(StateHandlerOutcome::transition(ManagedHostState::HostInit {
            machine_state: MachineState::Discovered {
                skip_reboot_wait: true,
            },
        })
        .with_txn(txn));
    };
    let validation = db::machine_validation::create_new_run(
        &mut txn,
        &mh_snapshot.host_snapshot.id,
        context,
        model::machine::MachineValidationFilter::default(),
    )
    .await?;
    let validation_id = validation.id;
    Ok(
        StateHandlerOutcome::transition(ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::RebootHost { validation_id },
            },
        })
        .with_txn(txn),
    )
}

/// Skip BOM validation and proceed to machine validation (or Ready if machine validation is disabled)
/// Used when BOM validation is disabled or a configured policy bypasses BOM validation
async fn skip_bom_validation_and_advance(
    txn: PgTransaction<'static>,
    host_handler_params: &HostHandlerParams,
    mh_snapshot: &ManagedHostStateSnapshot,
    reason: &str,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    tracing::info!(
        bom_validation=?host_handler_params.bom_validation,
        machine_id=%mh_snapshot.host_snapshot.id,
        assigned_sku_id=%mh_snapshot.host_snapshot.config.hw_sku.as_deref().unwrap_or_default(),
        reason=%reason,
        "Skipping BOM validation"
    );

    advance_to_machine_validating(txn, mh_snapshot).await
}

pub(crate) async fn handle_bom_validation_state(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    host_handler_params: &HostHandlerParams,
    mh_snapshot: &mut ManagedHostStateSnapshot,
    bom_validating_state: &BomValidating,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let outcome = if !host_handler_params.bom_validation.enabled {
        let txn = ctx.services.db_pool.begin().await?;
        skip_bom_validation_and_advance(
            txn,
            host_handler_params,
            mh_snapshot,
            "BOM validation disabled",
        )
        .await
    } else {
        match bom_validating_state {
            BomValidating::MatchingSku(bom_validating_context) => {
                if mh_snapshot.host_snapshot.config.hw_sku.is_none() {
                    let mut txn = ctx.services.db_pool.begin().await?;
                    if let Some(sku) =
                        match_sku_for_machine(&mut txn, host_handler_params, mh_snapshot).await?
                    {
                        db::machine::assign_sku(&mut txn, &mh_snapshot.host_snapshot.id, &sku.id)
                            .await?;
                        // finding a match uses the same check as verifying the sku, so consider it verified.
                        advance_to_machine_validating(txn, mh_snapshot).await
                    } else {
                        advance_to_waiting_for_sku_assignment(txn, mh_snapshot, host_handler_params)
                            .await
                    }
                } else {
                    Ok(StateHandlerOutcome::transition(
                        ManagedHostState::BomValidating {
                            bom_validating_state: BomValidating::VerifyingSku(
                                bom_validating_context.clone(),
                            ),
                        },
                    ))
                }
            }
            BomValidating::UpdatingInventory(bom_validating_context) => {
                if !discovered_after_state_transition(
                    mh_snapshot.host_snapshot.state.version,
                    mh_snapshot.host_snapshot.status.last_discovery_time,
                ) {
                    match trigger_reboot_if_needed(
                        &mh_snapshot.host_snapshot,
                        mh_snapshot,
                        bom_validating_context.reboot_retry_count,
                        &host_handler_params.reachability_params,
                        ctx,
                    )
                    .await
                    {
                        Ok(status) => {
                            if status.increase_retry_count {
                                let reboot_retry_count = Some(
                                    bom_validating_context
                                        .reboot_retry_count
                                        .unwrap_or_default()
                                        + 1,
                                );
                                Ok(StateHandlerOutcome::transition(
                                    ManagedHostState::BomValidating {
                                        bom_validating_state: BomValidating::UpdatingInventory(
                                            BomValidatingContext {
                                                machine_validation_context: bom_validating_context
                                                    .machine_validation_context,
                                                reboot_retry_count,
                                            },
                                        ),
                                    },
                                ))
                            } else {
                                Ok(StateHandlerOutcome::do_nothing())
                            }
                        }
                        Err(e) => Ok(StateHandlerOutcome::wait(format!(
                            "Failed to reboot host: {e}"
                        ))),
                    }
                } else if mh_snapshot.host_snapshot.config.hw_sku.is_none() {
                    Ok(StateHandlerOutcome::transition(
                        ManagedHostState::BomValidating {
                            bom_validating_state: BomValidating::MatchingSku(
                                bom_validating_context.clone(),
                            ),
                        },
                    ))
                } else {
                    Ok(StateHandlerOutcome::transition(
                        ManagedHostState::BomValidating {
                            bom_validating_state: BomValidating::VerifyingSku(
                                bom_validating_context.clone(),
                            ),
                        },
                    ))
                }
            }
            BomValidating::VerifyingSku(bom_validating_context) => {
                let Some(sku_id) = mh_snapshot.host_snapshot.config.hw_sku.clone() else {
                    // the sku got removed before it could be verified.  start over
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::BomValidating {
                            bom_validating_state: BomValidating::MatchingSku(
                                bom_validating_context.clone(),
                            ),
                        },
                    ));
                };

                let mut txn = ctx.services.db_pool.begin().await?;

                let Some(expected_sku) = db::sku::find(&mut txn, std::slice::from_ref(&sku_id))
                    .await?
                    .pop()
                else {
                    return advance_to_sku_missing(txn, mh_snapshot).await;
                };

                let actual_sku = db::sku::generate_sku_from_machine_at_version(
                    txn.as_mut(),
                    &mh_snapshot.host_snapshot.id,
                    expected_sku.schema_version,
                )
                .await?;

                let diffs = diff_skus(&actual_sku, &expected_sku);
                for diff in &diffs {
                    tracing::error!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        difference = %diff,
                        "SKU validation mismatch",
                    );
                }

                if diffs.is_empty() {
                    clear_sku_validation_report(&mut txn, mh_snapshot).await?;
                    advance_to_machine_validating(txn, mh_snapshot).await
                } else if should_allow_allocation_on_validation_failure(host_handler_params) {
                    tracing::info!(
                        machine_id=%mh_snapshot.host_snapshot.id,
                        sku_id=%mh_snapshot.host_snapshot.config.hw_sku.as_deref().unwrap_or_default(),
                        "SKU mismatch, but allow_allocation_on_validation_failure is true, proceeding to machine validation"
                    );
                    advance_to_machine_validating(txn, mh_snapshot).await
                } else {
                    let health_report = HealthReport::sku_mismatch(diffs);
                    db::machine::update_sku_validation_health_report(
                        &mut txn,
                        &mh_snapshot.host_snapshot.id,
                        &health_report,
                    )
                    .await?;

                    Ok(
                        StateHandlerOutcome::transition(ManagedHostState::BomValidating {
                            bom_validating_state: BomValidating::SkuVerificationFailed(
                                bom_validating_context.clone(),
                            ),
                        })
                        .with_txn(txn),
                    )
                }
            }
            BomValidating::SkuVerificationFailed(bom_validating_context) => {
                // If SKU was unassigned, transition to waiting for SKU assignment
                let txn = ctx.services.db_pool.begin().await?;
                if mh_snapshot.host_snapshot.config.hw_sku.is_none() {
                    Ok(
                        StateHandlerOutcome::transition(ManagedHostState::BomValidating {
                            bom_validating_state: BomValidating::WaitingForSkuAssignment(
                                bom_validating_context.clone(),
                            ),
                        })
                        .with_txn(txn),
                    )
                } else if mh_snapshot
                    .host_snapshot
                    .status
                    .hw_sku
                    .as_ref()
                    .is_some_and(|ss| {
                        ss.verify_request_time.is_some_and(|t| {
                            t > mh_snapshot.host_snapshot.state.version.timestamp()
                        })
                    })
                {
                    // New verification requested, try again
                    advance_to_updating_inventory(txn, mh_snapshot).await
                } else if should_allow_allocation_on_validation_failure(host_handler_params) {
                    // Allow machine to proceed despite verification failure
                    tracing::info!(
                        machine_id=%mh_snapshot.host_snapshot.id,
                        sku_id=%mh_snapshot.host_snapshot.config.hw_sku.as_deref().unwrap_or_default(),
                        "SKU verification failed, but allow_allocation_on_validation_failure is true, proceeding to machine validation"
                    );
                    advance_to_machine_validating(txn, mh_snapshot).await
                } else {
                    // Stay in failed state, waiting for manual intervention
                    Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
                }
            }
            BomValidating::WaitingForSkuAssignment(_) => {
                // Check if SKU was assigned or a matching SKU was found
                let mut txn = ctx.services.db_pool.begin().await?;
                if mh_snapshot.host_snapshot.config.hw_sku.is_some()
                    || match_sku_for_machine(&mut txn, host_handler_params, mh_snapshot)
                        .await?
                        .is_some()
                {
                    advance_to_updating_inventory(txn, mh_snapshot).await
                } else if should_ignore_unassigned_machine(host_handler_params) {
                    // Allow machine to proceed without SKU assignment
                    skip_bom_validation_and_advance(
                        txn,
                        host_handler_params,
                        mh_snapshot,
                        "ignore_unassigned_machine_while_waiting",
                    )
                    .await
                } else {
                    // Stay in waiting state until SKU is assigned
                    Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
                }
            }
            BomValidating::SkuMissing(_) => {
                let mut txn = ctx.services.db_pool.begin().await?;
                let mut outcome = if let Some(sku_id) =
                    mh_snapshot.host_snapshot.config.hw_sku.clone()
                {
                    // SKU is still assigned, check if it now exists or can be auto-generated
                    if db::sku::find(&mut txn, std::slice::from_ref(&sku_id))
                        .await?
                        .pop()
                        .is_some()
                        || generate_missing_sku_for_machine(
                            &mut txn,
                            host_handler_params,
                            mh_snapshot,
                        )
                        .await
                    {
                        advance_to_updating_inventory(txn, mh_snapshot).await
                    } else if should_allow_allocation_on_validation_failure(host_handler_params) {
                        // Allow machine to proceed despite missing SKU
                        tracing::info!(
                            machine_id=%mh_snapshot.host_snapshot.id,
                            sku_id=%sku_id,
                            "Assigned SKU does not exist, but allow_allocation_on_validation_failure is true, proceeding to machine validation"
                        );
                        advance_to_machine_validating(txn, mh_snapshot).await
                    } else {
                        Ok(StateHandlerOutcome::wait(
                            "Assigned SKU does not exist. Create the SKU or assign a different one"
                                .to_string(),
                        )
                        .with_txn(txn))
                    }
                } else {
                    // SKU was unassigned, transition to waiting for SKU assignment
                    advance_to_waiting_for_sku_assignment(txn, mh_snapshot, host_handler_params)
                        .await
                };

                // Clear health report for internal transitions within BomValidating
                // External transitions are handled by the generic cleanup below
                if let Ok(StateHandlerOutcome::Transition {
                    next_state: ManagedHostState::BomValidating { .. },
                    txn: Some(txn),
                    ..
                }) = &mut outcome
                {
                    clear_sku_validation_report(txn, mh_snapshot).await?;
                }
                outcome
            }
        }
    };

    match outcome {
        Ok(mut outcome) => {
            let mut txn = if let Some(txn) = outcome.take_transaction() {
                txn
            } else {
                ctx.services.db_pool.begin().await?
            };

            // if leaving BOM validation states, clear any health reports
            if let StateHandlerOutcome::Transition { next_state, .. } = &outcome
                && !matches!(next_state, ManagedHostState::BomValidating { .. })
            {
                clear_sku_validation_report(&mut txn, mh_snapshot).await?;
            }

            Ok(outcome.with_txn(txn))
        }
        Err(e) => Err(e),
    }
}
