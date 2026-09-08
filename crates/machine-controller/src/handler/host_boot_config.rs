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

//! Shared host boot-configuration convergence.
//!
//! The lifecycle state machines retain ownership of their persisted state
//! shapes. [`HostBootConfigStage`] is only a runtime projection of those
//! states, so this module can drive the common BIOS/job/poll/boot-order
//! sequence without introducing another serialized controller wrapper.
//!
//! [`run_host_boot_config_stage`] owns the lifecycle-neutral Redfish work. Its
//! four lifecycle adapters stay beside the state machines that own their
//! persisted state and terminal behavior:
//! - `handle_host_init_boot_config_stage` in `handler.rs`;
//! - `handle_instance_host_boot_config_stage` in `handler.rs`;
//! - `handle_dpu_reprovision_host_boot_config_stage` in `handler.rs`;
//! - `handle_validation_boot_config_stage` in `machine_validation.rs`.

use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use libredfish::Redfish;
use model::machine::{
    BiosConfigInfo, ManagedHostStateSnapshot, SetBootOrderInfo, SetBootOrderState,
};
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use super::bios_config::{
    BiosConfigJobAdvanceOutcome, BiosConfigOutcome, PollingBiosSetupOutcome,
    advance_bios_config_job, advance_polling_bios_setup, configure_host_bios,
};
use super::{
    ReachabilityParams, RequiredBootInterface, SetBootOrderOutcome,
    are_dpus_up_trigger_reboot_if_needed, is_dpu_observed_since, log_host_config,
    require_boot_interface, resolve_boot_interface_for_step, set_host_boot_order,
    trigger_reboot_if_needed,
};
use crate::context::MachineStateHandlerContextObjects;

/// Runtime projection of the persisted BIOS and boot-order states shared by
/// HostInit, assigned platform configuration, DPU reprovision, and validation.
///
/// This type must remain controller-internal: the owning lifecycle maps each
/// `Continue` outcome back to its existing serialized state variant.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum HostBootConfigStage {
    ConfigureBios {
        retry_count: u32,
    },
    WaitingForBiosJob {
        bios_config_info: BiosConfigInfo,
    },
    PollingBiosSetup {
        retry_count: u32,
    },
    SetBootOrder {
        set_boot_order_info: SetBootOrderInfo,
    },
}

/// Result of running one common boot-configuration stage.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum HostBootConfigOutcome {
    Continue(HostBootConfigStage),
    Complete,
    Wait(String),
    Failed { failure: String },
}

/// The next remediation required after inspecting the desired and actual host
/// boot configuration.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum HostBootConfigDecision {
    /// The platform BIOS settings (including the HTTP boot device) drifted.
    ConfigureBios,
    /// BIOS is already correct; only boot order drifted.
    SetBootOrder,
    /// BIOS and every NICo-managed boot-order setting are already correct.
    Complete,
}

/// Result of pacing prerequisites and inspecting the host boot config.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum HostBootConfigCheckOutcome {
    Ready(HostBootConfigDecision),
    /// The host cannot be inspected yet, for example while discovering a
    /// zero-DPU host's boot NIC or waiting for a fresh DPU observation.
    Wait(String),
}

/// DPU observation freshness required before trusting Redfish boot state.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum HostBootConfigDpuFreshness {
    /// The caller already gated on healthy, synchronized DPUs.
    AlreadyValidated,
    /// Require observations associated with the current host lifecycle state.
    CurrentHostState,
    /// Require observations newer than the most recent host reboot request.
    SinceLastHostRebootRequest,
}

/// Actual host boot settings read from Redfish.
///
/// `boot_order_setup` is `None` when BIOS drift already determines the next
/// action, or when boot order is intentionally not managed on this hardware.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct HostBootConfigInspection {
    pub is_bios_setup: bool,
    pub is_boot_order_setup: Option<bool>,
}

/// Convert an inspection into the smallest remediation that can converge it.
/// BIOS repair takes precedence because it may recreate the boot option whose
/// order would otherwise be inspected or changed.
pub(super) fn decide_host_boot_config(
    inspection: HostBootConfigInspection,
) -> HostBootConfigDecision {
    if !inspection.is_bios_setup {
        HostBootConfigDecision::ConfigureBios
    } else if inspection.is_boot_order_setup == Some(false) {
        HostBootConfigDecision::SetBootOrder
    } else {
        HostBootConfigDecision::Complete
    }
}

/// Inspect a host whose boot interface has already been resolved.
///
/// Keeping interface resolution outside this primitive lets validation retain
/// its existing distinction between a missing interface (hard error), an
/// undiscovered zero-DPU NIC (pace the reboot), and a Redfish read error (log
/// and retry).
pub(super) async fn inspect_host_boot_config(
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    boot_interface: &BootInterfaceTarget,
) -> Result<HostBootConfigInspection, StateHandlerError> {
    let is_bios_setup = boot_interface
        .run(|bi| redfish_client.is_bios_setup(Some(bi)))
        .await
        .map_err(|e| redfish_error("is_bios_setup", e))?;

    if !is_bios_setup || should_skip_boot_order_remediation(mh_snapshot) {
        return Ok(HostBootConfigInspection {
            is_bios_setup,
            is_boot_order_setup: None,
        });
    }

    let is_boot_order_setup = boot_interface
        .run(|bi| redfish_client.is_boot_order_setup(bi))
        .await
        .map_err(|e| redfish_error("is_boot_order_setup", e))?;

    Ok(HostBootConfigInspection {
        is_bios_setup,
        is_boot_order_setup: Some(is_boot_order_setup),
    })
}

/// Check whether host BIOS or boot order needs remediation.
pub(super) async fn check_host_boot_config(
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    dpu_freshness: HostBootConfigDpuFreshness,
    explicit_target: Option<&BootInterfaceTarget>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<HostBootConfigCheckOutcome, StateHandlerError> {
    if should_wait_for_dpus_before_host_boot_config(
        mh_snapshot,
        reachability_params,
        dpu_freshness,
        ctx,
    )
    .await
    {
        return Ok(HostBootConfigCheckOutcome::Wait(
            "Waiting for DPUs to come up.".to_string(),
        ));
    }

    let resolution = resolve_boot_interface_for_step(ctx, mh_snapshot, explicit_target).await?;
    let boot_interface = match require_boot_interface(
        &mh_snapshot.host_snapshot.id,
        resolution,
        "configuring boot",
        HostBootConfigCheckOutcome::Wait,
    )? {
        RequiredBootInterface::Ready(target) => target,
        RequiredBootInterface::Wait(outcome) => return Ok(outcome),
    };

    log_host_config(redfish_client, mh_snapshot).await;

    let inspection = inspect_host_boot_config(redfish_client, mh_snapshot, &boot_interface).await?;
    let decision = decide_host_boot_config(inspection);
    let vendor = mh_snapshot.host_snapshot.bmc_vendor();

    match &decision {
        HostBootConfigDecision::ConfigureBios => tracing::warn!(
            machine_id = %mh_snapshot.host_snapshot.id,
            bmc_vendor = %vendor,
            "Host BIOS setup is not configured properly"
        ),
        HostBootConfigDecision::SetBootOrder => tracing::warn!(
            machine_id = %mh_snapshot.host_snapshot.id,
            bmc_vendor = %vendor,
            "Host BIOS setup is correct but boot order is not configured properly"
        ),
        HostBootConfigDecision::Complete if should_skip_boot_order_remediation(mh_snapshot) => {
            tracing::info!(
                machine_id = %mh_snapshot.host_snapshot.id,
                bmc_vendor = %vendor,
                "Host BIOS setup is configured; skipping boot order remediation on Viking"
            );
        }
        HostBootConfigDecision::Complete => tracing::info!(
            machine_id = %mh_snapshot.host_snapshot.id,
            bmc_vendor = %vendor,
            "Host BIOS setup and boot order are configured properly"
        ),
    }

    Ok(HostBootConfigCheckOutcome::Ready(decision))
}

/// Runs one lifecycle-neutral host boot-configuration stage.
///
/// Called by the host-init, assigned-instance, DPU-reprovision, and validation
/// adapters. This function performs the Redfish BIOS, vendor-job, polling, or
/// boot-order work for one [`HostBootConfigStage`] and returns a
/// [`HostBootConfigOutcome`]. The caller owns the persisted lifecycle state and
/// maps the outcome back into that state.
pub(super) async fn run_host_boot_config_stage(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    reachability_params: &ReachabilityParams,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    explicit_target: Option<&BootInterfaceTarget>,
    stage: HostBootConfigStage,
) -> Result<HostBootConfigOutcome, StateHandlerError> {
    match stage {
        HostBootConfigStage::ConfigureBios { retry_count } => {
            let boot_interface =
                resolve_boot_interface_for_step(ctx, mh_snapshot, explicit_target).await?;
            let boot_interface = boot_interface.into_target();
            match configure_host_bios(
                ctx,
                reachability_params,
                redfish_client,
                mh_snapshot,
                boot_interface.as_ref(),
                retry_count,
            )
            .await?
            {
                BiosConfigOutcome::Done => Ok(HostBootConfigOutcome::Continue(
                    HostBootConfigStage::PollingBiosSetup { retry_count },
                )),
                BiosConfigOutcome::WaitingForBiosJob(bios_config_info) => {
                    Ok(HostBootConfigOutcome::Continue(
                        HostBootConfigStage::WaitingForBiosJob { bios_config_info },
                    ))
                }
                BiosConfigOutcome::WaitingForReboot(reason) => {
                    Ok(HostBootConfigOutcome::Wait(reason))
                }
            }
        }
        HostBootConfigStage::WaitingForBiosJob { bios_config_info } => {
            let retry_count = bios_config_info.retry_count;
            match advance_bios_config_job(ctx, redfish_client, mh_snapshot, bios_config_info)
                .await?
            {
                BiosConfigJobAdvanceOutcome::Continue(bios_config_info) => {
                    Ok(HostBootConfigOutcome::Continue(
                        HostBootConfigStage::WaitingForBiosJob { bios_config_info },
                    ))
                }
                BiosConfigJobAdvanceOutcome::Done => Ok(HostBootConfigOutcome::Continue(
                    HostBootConfigStage::PollingBiosSetup { retry_count },
                )),
                BiosConfigJobAdvanceOutcome::Failed { failure } => {
                    Ok(HostBootConfigOutcome::Failed { failure })
                }
                BiosConfigJobAdvanceOutcome::Wait(reason) => {
                    Ok(HostBootConfigOutcome::Wait(reason))
                }
                BiosConfigJobAdvanceOutcome::RetryPlatformConfiguration { retry_count } => {
                    Ok(HostBootConfigOutcome::Continue(
                        HostBootConfigStage::ConfigureBios { retry_count },
                    ))
                }
            }
        }
        HostBootConfigStage::PollingBiosSetup { retry_count } => {
            let boot_interface =
                resolve_boot_interface_for_step(ctx, mh_snapshot, explicit_target).await?;
            let boot_interface = boot_interface.into_target();
            match advance_polling_bios_setup(
                redfish_client,
                mh_snapshot,
                retry_count,
                &ctx.services.site_config.machine_state_controller,
                boot_interface.as_ref(),
            )
            .await?
            {
                PollingBiosSetupOutcome::Verified => {
                    if should_skip_boot_order_remediation(mh_snapshot) {
                        Ok(HostBootConfigOutcome::Complete)
                    } else {
                        Ok(HostBootConfigOutcome::Continue(
                            HostBootConfigStage::SetBootOrder {
                                set_boot_order_info: set_boot_order_info(retry_count),
                            },
                        ))
                    }
                }
                PollingBiosSetupOutcome::Wait(reason) => Ok(HostBootConfigOutcome::Wait(reason)),
                PollingBiosSetupOutcome::EnterRecovery(bios_config_info) => {
                    Ok(HostBootConfigOutcome::Continue(
                        HostBootConfigStage::WaitingForBiosJob { bios_config_info },
                    ))
                }
                PollingBiosSetupOutcome::Failed { failure } => {
                    Ok(HostBootConfigOutcome::Failed { failure })
                }
            }
        }
        HostBootConfigStage::SetBootOrder {
            set_boot_order_info,
        } => {
            let retry_count = set_boot_order_info.retry_count;
            match set_host_boot_order(
                ctx,
                reachability_params,
                redfish_client,
                mh_snapshot,
                explicit_target,
                set_boot_order_info,
            )
            .await?
            {
                SetBootOrderOutcome::Continue(set_boot_order_info) => Ok(
                    HostBootConfigOutcome::Continue(HostBootConfigStage::SetBootOrder {
                        set_boot_order_info,
                    }),
                ),
                SetBootOrderOutcome::ConfigureBios => {
                    let max_retries = ctx
                        .services
                        .site_config
                        .machine_state_controller
                        .max_bios_config_retries;
                    let Some(retry_count) = next_boot_config_retry_count(retry_count, max_retries)
                    else {
                        return Ok(HostBootConfigOutcome::Failed {
                            failure: format!(
                                "BIOS settings repeatedly drifted during boot-order repair; automated boot-config convergence exhausted after {max_retries} retries"
                            ),
                        });
                    };

                    Ok(HostBootConfigOutcome::Continue(
                        HostBootConfigStage::ConfigureBios { retry_count },
                    ))
                }
                SetBootOrderOutcome::Done => Ok(HostBootConfigOutcome::Complete),
                SetBootOrderOutcome::WaitingForReboot(reason)
                | SetBootOrderOutcome::Wait(reason) => Ok(HostBootConfigOutcome::Wait(reason)),
            }
        }
    }
}

pub(super) fn initial_set_boot_order_info() -> SetBootOrderInfo {
    set_boot_order_info(0)
}

fn set_boot_order_info(retry_count: u32) -> SetBootOrderInfo {
    SetBootOrderInfo {
        set_boot_order_jid: None,
        set_boot_order_state: SetBootOrderState::SetBootOrder,
        retry_count,
    }
}

/// Carry one bounded convergence budget when boot-order work discovers that
/// BIOS must be repaired again. Existing owner states already persist this
/// counter through both phases, so no serialized wrapper is required.
fn next_boot_config_retry_count(retry_count: u32, max_retries: u32) -> Option<u32> {
    (retry_count < max_retries).then(|| retry_count + 1)
}

/// Viking BMC firmware cannot safely run boot-order remediation; BIOS repair
/// still applies.
pub(super) fn should_skip_boot_order_remediation(mh_snapshot: &ManagedHostStateSnapshot) -> bool {
    mh_snapshot
        .host_snapshot
        .status
        .hardware_info
        .as_ref()
        .is_some_and(|hw| hw.is_dgx_h100())
}

async fn should_wait_for_dpus_before_host_boot_config(
    mh_snapshot: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    dpu_freshness: HostBootConfigDpuFreshness,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> bool {
    if !mh_snapshot.has_managed_dpus() {
        return false;
    }

    match dpu_freshness {
        HostBootConfigDpuFreshness::AlreadyValidated => false,
        HostBootConfigDpuFreshness::CurrentHostState => {
            !are_dpus_up_trigger_reboot_if_needed(mh_snapshot, reachability_params, ctx).await
        }
        HostBootConfigDpuFreshness::SinceLastHostRebootRequest => {
            let Some(last_reboot_requested) =
                mh_snapshot.host_snapshot.status.last_reboot_requested
            else {
                tracing::warn!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "No host reboot request timestamp found before post-reboot host boot config check"
                );
                return false;
            };

            for dpu_snapshot in &mh_snapshot.dpu_snapshots {
                if !is_dpu_observed_since(dpu_snapshot, last_reboot_requested.time) {
                    match trigger_reboot_if_needed(
                        dpu_snapshot,
                        mh_snapshot,
                        None,
                        reachability_params,
                        ctx,
                    )
                    .await
                    {
                        Ok(_) => {}
                        Err(e) => tracing::warn!(
                            dpu_machine_id = %dpu_snapshot.id,
                            error = %e,
                            "Could not reboot DPU while waiting to check host boot config"
                        ),
                    }
                    return true;
                }
            }

            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use carbide_health_metrics::PerObjectMetricsRegistry;
    use carbide_redfish::libredfish::test_support::{RedfishSim, RedfishSimBootInterfaceRef};
    use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool};
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use carbide_test_support::value_scenarios;
    use model::machine_boot_interface::MachineBootInterface;
    use model::test_support::machine_snapshot::managed_host_state_snapshot;
    use state_controller::db_write_batch::DbWriteBatch;

    use super::*;
    use crate::config::MachineStateHandlerSiteConfig;
    use crate::context::MachineStateHandlerServices;
    use crate::metrics::MachineMetrics;

    fn reachability_params() -> ReachabilityParams {
        ReachabilityParams {
            dpu_wait_time: chrono::Duration::zero(),
            power_down_wait: chrono::Duration::zero(),
            failure_retry_time: chrono::Duration::zero(),
            scout_reporting_timeout: chrono::Duration::zero(),
            waiting_for_measurements_timeout: chrono::Duration::zero(),
            uefi_boot_wait: chrono::Duration::zero(),
        }
    }

    fn change_observed_boot_interface(
        mh_snapshot: &mut ManagedHostStateSnapshot,
        mac_address: &str,
    ) {
        let interface = mh_snapshot
            .host_snapshot
            .status
            .interfaces
            .iter_mut()
            .find(|interface| interface.primary_interface)
            .expect("fixture host has a primary interface");
        interface.mac_address = mac_address.parse().unwrap();
        interface.boot_interface_id = Some(format!("mutable-{mac_address}"));
    }

    #[tokio::test]
    async fn explicit_target_is_stable_across_boot_config_stages() {
        const REDFISH_HOST: &str = "boot-config-test";
        let explicit_boot_interface = MachineBootInterface {
            mac_address: "10:00:00:00:00:01".parse().unwrap(),
            interface_id: "NIC.Slot.5-1".to_string(),
        };
        let explicit_target = BootInterfaceTarget::Pair(explicit_boot_interface.clone());
        let expected_redfish_target = RedfishSimBootInterfaceRef::Pair {
            mac_address: explicit_boot_interface.mac_address,
            interface_id: explicit_boot_interface.interface_id.clone(),
        };

        let redfish_sim = Arc::new(RedfishSim::default());
        let redfish_client = redfish_sim
            .create_client(REDFISH_HOST, None, RedfishAuth::Anonymous, None)
            .await
            .unwrap();
        redfish_sim.set_machine_setup_bios_job_id(Some("bios-job".to_string()));

        // Explicit-target stages do not need prediction data. A short-timeout
        // unreachable pool makes an accidental lookup fail promptly.
        let db_pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let mut services = MachineStateHandlerServices {
            db_pool: db_pool.clone(),
            db_reader: db_pool.into(),
            redfish_client_pool: redfish_sim.clone(),
            bmc_credential_ops: redfish_sim.clone(),
            ipmi_tool: carbide_ipmi::test_support(),
            site_config: Arc::new(MachineStateHandlerSiteConfig::test_default()),
            component_manager: None,
            credential_manager: Arc::new(TestCredentialManager::default()),
            per_object_metrics_registry: PerObjectMetricsRegistry::new(
                Vec::new(),
                Duration::from_secs(60),
            ),
            per_object_info: None,
            bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::Bmc,
            ),
            host_uefi_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::HostUefi,
            ),
            dpu_uefi_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::DpuUefi,
            ),
            dpu_bmc_service_rotation_gate:
                carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::DpuBmcService,
                ),
            nic_lockdown_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::LockdownIkm,
            ),
        };
        let mut metrics = MachineMetrics::default();
        let mut pending_db_writes = DbWriteBatch::new();
        let mut ctx = StateHandlerContext {
            services: &mut services,
            metrics: &mut metrics,
            pending_db_writes: &mut pending_db_writes,
        };
        let reachability_params = reachability_params();
        let mut mh_snapshot = managed_host_state_snapshot();

        change_observed_boot_interface(&mut mh_snapshot, "20:00:00:00:00:01");
        assert_eq!(
            check_host_boot_config(
                redfish_client.as_ref(),
                &mh_snapshot,
                &reachability_params,
                HostBootConfigDpuFreshness::AlreadyValidated,
                Some(&explicit_target),
                &mut ctx,
            )
            .await
            .unwrap(),
            HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::Complete),
        );

        macro_rules! run_stage {
            ($stage:expr) => {
                run_host_boot_config_stage(
                    &mut ctx,
                    &reachability_params,
                    redfish_client.as_ref(),
                    &mh_snapshot,
                    Some(&explicit_target),
                    $stage,
                )
                .await
                .unwrap()
            };
        }
        let set_boot_order_stage = |set_boot_order_state| HostBootConfigStage::SetBootOrder {
            set_boot_order_info: SetBootOrderInfo {
                set_boot_order_jid: None,
                set_boot_order_state,
                retry_count: 0,
            },
        };

        change_observed_boot_interface(&mut mh_snapshot, "30:00:00:00:00:01");
        assert!(matches!(
            run_stage!(HostBootConfigStage::ConfigureBios { retry_count: 0 }),
            HostBootConfigOutcome::Continue(HostBootConfigStage::WaitingForBiosJob { .. }),
        ));

        change_observed_boot_interface(&mut mh_snapshot, "40:00:00:00:00:01");
        assert!(matches!(
            run_stage!(HostBootConfigStage::PollingBiosSetup { retry_count: 0 }),
            HostBootConfigOutcome::Continue(HostBootConfigStage::SetBootOrder { .. }),
        ));

        redfish_sim.set_is_boot_order_setup(false);
        change_observed_boot_interface(&mut mh_snapshot, "50:00:00:00:00:01");
        assert!(matches!(
            run_stage!(set_boot_order_stage(SetBootOrderState::SetBootOrder)),
            HostBootConfigOutcome::Continue(HostBootConfigStage::SetBootOrder {
                set_boot_order_info: SetBootOrderInfo {
                    set_boot_order_state: SetBootOrderState::WaitForSetBootOrderJobScheduled,
                    ..
                },
            }),
        ));

        change_observed_boot_interface(&mut mh_snapshot, "60:00:00:00:00:01");
        assert!(matches!(
            run_stage!(set_boot_order_stage(
                SetBootOrderState::WaitForHttpBootDeviceApplied
            )),
            HostBootConfigOutcome::Continue(HostBootConfigStage::SetBootOrder {
                set_boot_order_info: SetBootOrderInfo {
                    set_boot_order_state: SetBootOrderState::SetBootOrder,
                    ..
                },
            }),
        ));

        change_observed_boot_interface(&mut mh_snapshot, "70:00:00:00:00:01");
        assert_eq!(
            run_stage!(set_boot_order_stage(SetBootOrderState::CheckBootOrder)),
            HostBootConfigOutcome::Complete,
        );

        let observed_targets = redfish_sim.boot_interface_targets(REDFISH_HOST);
        assert_eq!(
            observed_targets.len(),
            10,
            "each target-consuming call in the exercised stages must be recorded",
        );
        for (call_index, observed_target) in observed_targets.into_iter().enumerate() {
            assert_eq!(
                observed_target,
                Some(expected_redfish_target.clone()),
                "boot-config target drifted at Redfish call {call_index}",
            );
        }
    }

    #[test]
    fn boot_config_decision_chooses_the_smallest_remediation() {
        value_scenarios!(decide_host_boot_config:
            "BIOS drift takes precedence" {
                HostBootConfigInspection {
                    is_bios_setup: false,
                    is_boot_order_setup: None,
                } => HostBootConfigDecision::ConfigureBios,
                HostBootConfigInspection {
                    is_bios_setup: false,
                    is_boot_order_setup: Some(false),
                } => HostBootConfigDecision::ConfigureBios,
                HostBootConfigInspection {
                    is_bios_setup: false,
                    is_boot_order_setup: Some(true),
                } => HostBootConfigDecision::ConfigureBios,
            }

            "configured BIOS chooses the narrowest remaining action" {
                HostBootConfigInspection {
                    is_bios_setup: true,
                    is_boot_order_setup: Some(false),
                } => HostBootConfigDecision::SetBootOrder,
                HostBootConfigInspection {
                    is_bios_setup: true,
                    is_boot_order_setup: Some(true),
                } => HostBootConfigDecision::Complete,
                HostBootConfigInspection {
                    is_bios_setup: true,
                    is_boot_order_setup: None,
                } => HostBootConfigDecision::Complete,
            }
        );
    }

    #[test]
    fn boot_config_retry_count_is_bounded_across_bios_and_order() {
        struct RetryCount {
            retry_count: u32,
            max_retries: u32,
        }

        value_scenarios!(
            run = |RetryCount {
                       retry_count,
                       max_retries,
                   }: RetryCount| next_boot_config_retry_count(retry_count, max_retries);
            "retry budget remains" {
                RetryCount {
                    retry_count: 0,
                    max_retries: 1,
                } => Some(1),
                RetryCount {
                    retry_count: 4,
                    max_retries: 5,
                } => Some(5),
            }

            "retry budget is unavailable" {
                RetryCount {
                    retry_count: 1,
                    max_retries: 1,
                } => None,
                RetryCount {
                    retry_count: 0,
                    max_retries: 0,
                } => None,
                RetryCount {
                    retry_count: 4,
                    max_retries: 1,
                } => None,
            }
        );
    }
}
