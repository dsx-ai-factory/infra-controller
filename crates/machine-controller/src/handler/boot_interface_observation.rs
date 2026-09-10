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

//! Periodic observation of an already-verified host boot interface.
//!
//! This path only reads Redfish and records what it found. A mismatch opens a
//! new pending generation for the same target; the existing Ready reconciler
//! owns every later unlock, write, reboot, and verification step.

use carbide_redfish::boot_interface::BootInterfaceTarget;
use chrono::{DateTime, Duration, Utc};
use config_version::Versioned;
use model::machine::{DpuMachine, HostMachine, ManagedHostState, ManagedHostStateSnapshot};
use model::machine_boot_interface::MachineBootInterfaceTarget;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::host_boot_config::{
    HostBootConfigDecision, decide_host_boot_config, inspect_host_boot_config,
};
use crate::context::MachineStateHandlerContextObjects;

/// Returns the verified desired target after its observation interval elapses.
fn periodic_observation_target(
    host: &HostMachine,
    now: DateTime<Utc>,
    observation_interval: Duration,
) -> Option<&Versioned<MachineBootInterfaceTarget>> {
    let desired_boot_interface = host.config.desired_boot_interface.as_ref()?;
    let boot_interface_observation = host.status.boot_interface_status_observation.as_ref()?;

    (desired_boot_interface.version == boot_interface_observation.config_version
        && now.signed_duration_since(boot_interface_observation.observed_at)
            >= observation_interval)
        .then_some(desired_boot_interface)
}

/// Returns whether a managed DPU has reported recently enough to trust the
/// host's Redfish boot view and after the host entered its current state.
fn dpu_observation_is_current(
    managed_host_snapshot: &ManagedHostStateSnapshot,
    dpu_snapshot: &DpuMachine,
    now: DateTime<Utc>,
    dpu_up_threshold: Duration,
) -> bool {
    super::is_dpu_up(managed_host_snapshot, dpu_snapshot)
        && dpu_snapshot
            .network_status_observation
            .as_ref()
            .is_some_and(|dpu_network_observation| {
                now.signed_duration_since(dpu_network_observation.observed_at) <= dpu_up_threshold
            })
}

/// Observes an eligible verified target without mutating Redfish.
///
/// A BMC failure leaves the last successful `observed_at` unchanged, so every
/// later Ready or Assigned sweep retries it. The configured interval applies
/// between successful observations, not failed attempts. After a mismatch,
/// Ready enters `BootConfiguring` on its next sweep, while Assigned keeps the
/// pending generation until the host is unassigned.
pub(super) async fn observe_verified_boot_interface(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    managed_host_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host = &managed_host_snapshot.host_snapshot;
    let controller_config = &ctx.services.site_config.machine_state_controller;
    let now = Utc::now();
    let Some(desired_boot_interface) = periodic_observation_target(
        host,
        now,
        controller_config.boot_interface_observation_interval,
    ) else {
        return Ok(StateHandlerOutcome::do_nothing());
    };

    // Locked Supermicro reports a stale boot-order view. The explicit
    // `BootConfiguring` flow can safely unlock and reboot an unassigned host,
    // but a periodic observer must remain non-disruptive (especially while an
    // instance is assigned). Profiles that intentionally leave lockdown off
    // can use the ordinary read-only path.
    if host.bmc_vendor().is_supermicro() && !host.host_profile.disable_lockdown {
        return Ok(StateHandlerOutcome::do_nothing());
    }

    // A managed DPU can disappear from the host's Redfish boot view during a
    // DPU restart. Do not turn that transient view into drift until every DPU
    // has reported network state since the host entered its current state and
    // the report remains inside its health window.
    if managed_host_snapshot
        .dpu_snapshots
        .iter()
        .any(|dpu_snapshot| {
            !dpu_observation_is_current(
                managed_host_snapshot,
                dpu_snapshot,
                now,
                controller_config.dpu_up_threshold,
            )
        })
    {
        return Ok(StateHandlerOutcome::do_nothing());
    }

    let redfish_client = match ctx.services.create_redfish_client_from_machine(host).await {
        Ok(redfish_client) => redfish_client,
        Err(error) => {
            tracing::warn!(
                machine_id = %host.id,
                desired_version = %desired_boot_interface.version,
                operation = "create_redfish_client",
                error = %error,
                "Failed to observe verified host boot configuration",
            );
            return Ok(StateHandlerOutcome::do_nothing());
        }
    };
    let redfish_target: BootInterfaceTarget = desired_boot_interface.value.clone().into();
    let boot_config_inspection = match inspect_host_boot_config(
        redfish_client.as_ref(),
        managed_host_snapshot,
        &redfish_target,
    )
    .await
    {
        Ok(boot_config_inspection) => boot_config_inspection,
        Err(error) => {
            tracing::warn!(
                machine_id = %host.id,
                desired_version = %desired_boot_interface.version,
                operation = "inspect_host_boot_config",
                error = %error,
                "Failed to observe verified host boot configuration",
            );
            return Ok(StateHandlerOutcome::do_nothing());
        }
    };

    let boot_config_decision = decide_host_boot_config(boot_config_inspection);
    let mut observation_txn = ctx.services.db_pool.begin().await?;
    match boot_config_decision {
        HostBootConfigDecision::Complete => {
            let observation_recorded = db::machine_desired_boot_interface::mark_verified(
                observation_txn.as_mut(),
                &host.id,
                desired_boot_interface.version,
                Utc::now(),
            )
            .await?;
            if observation_recorded {
                tracing::debug!(
                    machine_id = %host.id,
                    desired_version = %desired_boot_interface.version,
                    "Verified periodic host boot configuration observation",
                );
            } else {
                tracing::debug!(
                    machine_id = %host.id,
                    desired_version = %desired_boot_interface.version,
                    "Discarded stale host boot configuration observation",
                );
            }
        }
        required_action @ (HostBootConfigDecision::ConfigureBios
        | HostBootConfigDecision::SetBootOrder) => {
            // BIOS inspection covers NICo's whole managed boot profile, not just
            // the boot-order entry. Reopening this target sends either kind of
            // drift through the existing whole-profile flow.
            let pending_boot_interface =
                db::machine_desired_boot_interface::try_reopen_after_observed_drift(
                    observation_txn.as_mut(),
                    &host.id,
                    desired_boot_interface,
                )
                .await?;
            if let Some(pending_boot_interface) = pending_boot_interface {
                tracing::warn!(
                    machine_id = %host.id,
                    desired_version = %pending_boot_interface.version,
                    ?required_action,
                    repair_deferred_until_unassigned = matches!(
                        &managed_host_snapshot.managed_state,
                        ManagedHostState::Assigned { .. }
                    ),
                    "Host boot configuration drift detected",
                );
            } else {
                tracing::debug!(
                    machine_id = %host.id,
                    desired_version = %desired_boot_interface.version,
                    "Discarded stale host boot configuration drift observation",
                );
            }
        }
    }

    Ok(StateHandlerOutcome::do_nothing().with_txn(observation_txn))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use chrono::Duration;
    use config_version::{ConfigVersion, Versioned};
    use mac_address::MacAddress;
    use model::machine_boot_interface::{
        BootInterfaceStatusObservation, MachineBootInterfaceTarget,
    };
    use model::test_support::machine_snapshot::{host_machine, managed_host_state_snapshot};

    use super::*;

    #[test]
    fn observation_requires_elapsed_interval_and_verified_intent() {
        struct PeriodicObservationInput {
            desired_version: Option<ConfigVersion>,
            observed_version: Option<ConfigVersion>,
            observation_age: Duration,
        }

        let now = DateTime::from_timestamp(1_722_000_000, 0).expect("fixture timestamp");
        let observation_interval = Duration::minutes(10);
        let observed_version = ConfigVersion::initial();

        value_scenarios!(
            run = |input: PeriodicObservationInput| {
                let mut host = host_machine();
                host.config.desired_boot_interface = input.desired_version.map(|version| {
                    Versioned::new(
                        MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 0, 1])),
                        version,
                    )
                });
                host.status.boot_interface_status_observation =
                    input.observed_version.map(|config_version| {
                        BootInterfaceStatusObservation {
                            config_version,
                            observed_at: now - input.observation_age,
                            assumed: false,
                        }
                    });

                periodic_observation_target(&host.into(), now, observation_interval).is_some()
            };
            "eligible after the interval" {
                PeriodicObservationInput {
                    desired_version: Some(observed_version),
                    observed_version: Some(observed_version),
                    observation_age: observation_interval,
                } => true,
            }
            "ineligible" {
                PeriodicObservationInput {
                    desired_version: Some(observed_version),
                    observed_version: Some(observed_version),
                    observation_age: observation_interval - Duration::seconds(1),
                } => false,
                PeriodicObservationInput {
                    desired_version: Some(observed_version.increment()),
                    observed_version: Some(observed_version),
                    observation_age: observation_interval,
                } => false,
                PeriodicObservationInput {
                    desired_version: None,
                    observed_version: Some(observed_version),
                    observation_age: observation_interval,
                } => false,
                PeriodicObservationInput {
                    desired_version: Some(observed_version),
                    observed_version: None,
                    observation_age: observation_interval,
                } => false,
            }
        );
    }

    /// A report from the current state is still unsafe after its health window.
    #[test]
    fn managed_dpu_observation_must_also_be_recent() {
        let managed_host_snapshot = managed_host_state_snapshot();
        let dpu_snapshot = &managed_host_snapshot.dpu_snapshots[0];
        let dpu_observation_timestamp = dpu_snapshot
            .network_status_observation
            .as_ref()
            .expect("fixture DPU should have a network observation")
            .observed_at;
        let dpu_up_threshold = Duration::minutes(5);

        assert!(dpu_observation_is_current(
            &managed_host_snapshot,
            dpu_snapshot,
            dpu_observation_timestamp + dpu_up_threshold,
            dpu_up_threshold,
        ));
        assert!(!dpu_observation_is_current(
            &managed_host_snapshot,
            dpu_snapshot,
            dpu_observation_timestamp + dpu_up_threshold + Duration::seconds(1),
            dpu_up_threshold,
        ));
    }
}
