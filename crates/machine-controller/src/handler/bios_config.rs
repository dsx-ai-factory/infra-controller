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

//! BIOS configuration: machine_setup, Dell job wait/recovery, and PollingBiosSetup escalation.

use std::collections::HashMap;
use std::str::FromStr;

use bmc_platform::{PowerState, ResetType};
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use carbide_redfish::state_handler::state_handler_platform_error as platform_error;
use carbide_uuid::machine::AsMachineId;
use chrono::{DateTime, Utc};
use eyre::eyre;
use futures_util::FutureExt;
use health_report::{HealthProbeAlert, HealthProbeId, HealthReport, HealthReportApplyMode};
use libredfish::model::service_root::RedfishVendor;
use libredfish::{BiosProfileVendor, Redfish, RedfishError};
use model::machine::{
    BiosConfigInfo, BiosConfigState, HostMachine, ManagedHostState, ManagedHostStateSnapshot,
    PowerState as ExpectedPowerState,
};
use state_controller::db_write_batch::WriteOpFn;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::{
    ReachabilityParams, RebootStatus, call_machine_setup_and_handle_no_dpu_error,
    handler_host_power_control, trigger_reboot_if_needed,
};
use crate::config::{
    MachineStateControllerConfig, MachineStateHandlerSiteConfig, NamedBiosProfile,
};
use crate::context::MachineStateHandlerContextObjects;
use crate::redfish::power_state_name;
use crate::write_ops::MachineWriteOp;

const BIOS_PROFILE_LABEL: &str = "bios-profile";
const BIOS_PROFILE_HEALTH_SOURCE: &str = "bios-profile";
const BIOS_PROFILE_DRIFT_HEALTH_SOURCE: &str = "bios-profile-drift";
const BIOS_PROFILE_DRIFT_PROBE: &str = "BiosProfileDrift";
const BIOS_PROFILE_UNKNOWN_ATTRIBUTES_PROBE: &str = "BiosProfileUnknownAttributes";
const BIOS_PROFILE_DRIFT_REMEDIATING_PROBE: &str = "BiosProfileDriftRemediating";
const BIOS_PROFILE_DRIFT_PERSISTENT_PROBE: &str = "BiosProfileDriftPersistent";

#[derive(Debug, PartialEq)]
enum HostBiosProfiles {
    SiteDefault,
    Named(BiosProfileVendor),
    Fallback(String),
}

impl HostBiosProfiles {
    async fn select(
        redfish_client: &dyn Redfish,
        labels: &HashMap<String, String>,
        site_config: &MachineStateHandlerSiteConfig,
    ) -> Result<Self, RedfishError> {
        let Some(name) = labels.get(BIOS_PROFILE_LABEL) else {
            return Ok(Self::SiteDefault);
        };
        let Some(profile) = site_config.named_bios_profiles.get(name) else {
            return Ok(Self::Fallback(format!(
                "bios-profile {name} is not defined in named_bios_profiles"
            )));
        };
        if !matches!(profile.vendor, RedfishVendor::Dell | RedfishVendor::Lenovo) {
            return Ok(Self::Fallback(format!(
                "bios-profile {name} is for vendor {}, which does not apply BIOS profiles",
                profile.vendor
            )));
        }
        let vendor = redfish_client
            .get_service_root()
            .await?
            .vendor()
            .unwrap_or(RedfishVendor::Unknown);
        if vendor != profile.vendor {
            return Ok(Self::Fallback(format!(
                "bios-profile {name} is for vendor {} but the host BMC is {vendor}",
                profile.vendor
            )));
        }
        let model =
            libredfish::model_coerce(&redfish_client.get_system().await?.model.unwrap_or_default());
        if model != profile.model {
            return Ok(Self::Fallback(format!(
                "bios-profile {name} is for model {} but the host is {model}",
                profile.model
            )));
        }
        Ok(Self::Named(HashMap::from([(
            profile.vendor,
            HashMap::from([(
                profile.model.clone(),
                HashMap::from([(site_config.selected_profile, profile.attributes.clone())]),
            )]),
        )])))
    }

    fn bios_profiles<'a>(
        &'a self,
        site_config: &'a MachineStateHandlerSiteConfig,
    ) -> &'a BiosProfileVendor {
        match self {
            Self::Named(bios_profiles) => bios_profiles,
            Self::SiteDefault | Self::Fallback(_) => &site_config.bios_profiles,
        }
    }

    fn health(&self, host: &HostMachine, now: DateTime<Utc>) -> BiosProfileHealth {
        if let Self::Fallback(reason) = self {
            return BiosProfileHealth::Alert(HealthReport {
                source: BIOS_PROFILE_HEALTH_SOURCE.to_string(),
                triggered_by: None,
                observed_at: Some(now),
                alerts: vec![HealthProbeAlert {
                    id: HealthProbeId::from_str("BiosProfileNotApplied").expect("valid probe id"),
                    target: host.metadata.labels.get(BIOS_PROFILE_LABEL).cloned(),
                    in_alert_since: Some(now),
                    message: reason.clone(),
                    tenant_message: None,
                    classifications: vec![],
                }],
                successes: vec![],
            });
        }
        if host
            .health_reports
            .merges
            .contains_key(BIOS_PROFILE_HEALTH_SOURCE)
        {
            BiosProfileHealth::Clear
        } else {
            BiosProfileHealth::Unchanged
        }
    }

    fn record(
        &self,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        mh_snapshot: &ManagedHostStateSnapshot,
    ) {
        let host = &mh_snapshot.host_snapshot;
        let name = host.metadata.labels.get(BIOS_PROFILE_LABEL);
        match self {
            Self::Fallback(reason) => tracing::warn!(
                machine_id = %host.id,
                bios_profile = ?name,
                %reason,
                "named BIOS profile not applied, using site bios_profiles"
            ),
            Self::Named(_) => tracing::info!(
                machine_id = %host.id,
                bios_profile = ?name,
                "applying named BIOS profile"
            ),
            Self::SiteDefault => {}
        }
        match self.health(host, Utc::now()) {
            BiosProfileHealth::Alert(health_report) => {
                ctx.pending_db_writes
                    .push(MachineWriteOp::InsertMachineHealthReport {
                        machine_id: host.id.to_machine_id(),
                        mode: HealthReportApplyMode::Merge,
                        health_report,
                    });
            }
            BiosProfileHealth::Clear => {
                let machine_id = host.id.to_machine_id();
                let write: WriteOpFn = Box::new(move |txn| {
                    async move {
                        db::machine::remove_health_report(
                            txn,
                            &machine_id,
                            HealthReportApplyMode::Merge,
                            BIOS_PROFILE_HEALTH_SOURCE,
                        )
                        .await?;
                        Ok(())
                    }
                    .boxed()
                });
                ctx.pending_db_writes.push(write);
            }
            BiosProfileHealth::Unchanged => {}
        }
    }
}

#[derive(Debug, PartialEq)]
enum BiosProfileHealth {
    Alert(HealthReport),
    Clear,
    Unchanged,
}

#[derive(Debug, PartialEq)]
pub(super) struct BiosProfileDrift {
    name: String,
    drifted: Vec<String>,
    unknown: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub(super) enum BiosProfileDriftAction {
    Clear,
    Report,
    Remediate,
    Persistent,
}

pub(super) async fn inspect_bios_profile_drift(
    redfish_client: &dyn Redfish,
    labels: &HashMap<String, String>,
    site_config: &MachineStateHandlerSiteConfig,
) -> Result<Option<BiosProfileDrift>, RedfishError> {
    let Some((name, profile)) = labels
        .get(BIOS_PROFILE_LABEL)
        .and_then(|name| site_config.named_bios_profiles.get_key_value(name))
    else {
        return Ok(None);
    };
    if !matches!(
        HostBiosProfiles::select(redfish_client, labels, site_config).await?,
        HostBiosProfiles::Named(_)
    ) {
        return Ok(None);
    }
    let bios = redfish_client.bios().await?;
    let current = libredfish::jsonmap::get_object(&bios, "Attributes", "Bios")?;
    let pending = redfish_client.pending().await?;
    Ok(Some(compare_bios_profile(name, profile, current, &pending)))
}

fn compare_bios_profile(
    name: &str,
    profile: &NamedBiosProfile,
    current: &serde_json::Map<String, serde_json::Value>,
    pending: &HashMap<String, serde_json::Value>,
) -> BiosProfileDrift {
    let mut drifted = Vec::new();
    let mut unknown = Vec::new();
    for (attribute, want) in &profile.attributes {
        match current.get(attribute) {
            None => unknown.push(attribute.clone()),
            Some(have) if bios_value_matches(have, want) => {}
            Some(_)
                if pending
                    .get(attribute)
                    .is_some_and(|pending| bios_value_matches(pending, want)) => {}
            Some(_) => drifted.push(attribute.clone()),
        }
    }
    drifted.sort();
    unknown.sort();
    BiosProfileDrift {
        name: name.to_string(),
        drifted,
        unknown,
    }
}

fn bios_value_matches(have: &serde_json::Value, want: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (have, want) {
        (Value::Number(have), Value::Number(want)) => have.as_f64() == want.as_f64(),
        (Value::String(text), Value::Number(number))
        | (Value::Number(number), Value::String(text)) => {
            text.trim().parse::<f64>().ok() == number.as_f64()
        }
        (Value::String(text), Value::Bool(flag)) | (Value::Bool(flag), Value::String(text)) => text
            .trim()
            .eq_ignore_ascii_case(if *flag { "true" } else { "false" }),
        _ => have == want,
    }
}

fn bios_profile_drift_action(
    drift: Option<&BiosProfileDrift>,
    persistent: bool,
) -> BiosProfileDriftAction {
    let Some(drift) = drift else {
        return BiosProfileDriftAction::Clear;
    };
    if drift.drifted.is_empty() {
        return if drift.unknown.is_empty() {
            BiosProfileDriftAction::Clear
        } else {
            BiosProfileDriftAction::Report
        };
    }
    if persistent {
        BiosProfileDriftAction::Persistent
    } else {
        BiosProfileDriftAction::Remediate
    }
}

pub(super) async fn bios_profile_needs_reapply(
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> bool {
    let host = &mh_snapshot.host_snapshot;
    match inspect_bios_profile_drift(
        redfish_client,
        &host.metadata.labels,
        &ctx.services.site_config,
    )
    .await
    {
        Ok(drift) => {
            let action = bios_profile_drift_action(drift.as_ref(), bios_profile_persistent(host));
            record_bios_profile_drift(ctx, host, drift.as_ref(), &action);
            action == BiosProfileDriftAction::Remediate
        }
        Err(error) => {
            tracing::warn!(
                machine_id = %host.id,
                %error,
                "Could not compare the host BIOS with its bios-profile; not re-applying it"
            );
            false
        }
    }
}

pub(super) async fn verify_bios_profile_after_reboot(
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) {
    let host = &mh_snapshot.host_snapshot;
    match inspect_bios_profile_drift(
        redfish_client,
        &host.metadata.labels,
        &ctx.services.site_config,
    )
    .await
    {
        Ok(drift) => {
            let action = bios_profile_drift_action(drift.as_ref(), true);
            record_bios_profile_drift(ctx, host, drift.as_ref(), &action);
        }
        Err(error) => tracing::warn!(
            machine_id = %host.id,
            %error,
            "Could not verify the host BIOS against its bios-profile after the reboot"
        ),
    }
}

fn bios_profile_persistent(host: &HostMachine) -> bool {
    alert_since(
        host.health_reports
            .merges
            .get(BIOS_PROFILE_DRIFT_HEALTH_SOURCE),
        BIOS_PROFILE_DRIFT_PERSISTENT_PROBE,
    )
    .is_some()
}

fn alert_since(report: Option<&HealthReport>, probe: &str) -> Option<DateTime<Utc>> {
    report?
        .alerts
        .iter()
        .find(|alert| alert.id.to_string() == probe)?
        .in_alert_since
}

fn bios_profile_drift_report(
    drift: &BiosProfileDrift,
    action: &BiosProfileDriftAction,
    previous: Option<&HealthReport>,
    now: DateTime<Utc>,
) -> Option<HealthReport> {
    let alert = |probe: &str, message: String| HealthProbeAlert {
        id: HealthProbeId::from_str(probe).expect("valid probe id"),
        target: Some(drift.name.clone()),
        in_alert_since: Some(alert_since(previous, probe).unwrap_or(now)),
        message,
        tenant_message: None,
        classifications: vec![],
    };
    let mut alerts = Vec::new();
    if !drift.drifted.is_empty() {
        alerts.push(alert(
            BIOS_PROFILE_DRIFT_PROBE,
            format!(
                "BIOS attributes differ from bios-profile {}: {}",
                drift.name,
                drift.drifted.join(", ")
            ),
        ));
    }
    if !drift.unknown.is_empty() {
        alerts.push(alert(
            BIOS_PROFILE_UNKNOWN_ATTRIBUTES_PROBE,
            format!(
                "bios-profile {} sets attributes the BMC does not report: {}",
                drift.name,
                drift.unknown.join(", ")
            ),
        ));
    }
    match action {
        BiosProfileDriftAction::Remediate | BiosProfileDriftAction::Persistent => {
            alerts.push(alert(
                BIOS_PROFILE_DRIFT_REMEDIATING_PROBE,
                format!("re-applying bios-profile {}", drift.name),
            ));
        }
        BiosProfileDriftAction::Clear | BiosProfileDriftAction::Report => {}
    }
    if *action == BiosProfileDriftAction::Persistent {
        alerts.push(alert(
            BIOS_PROFILE_DRIFT_PERSISTENT_PROBE,
            format!(
                "BIOS attributes still differ from bios-profile {} after it was re-applied and the host rebooted; not re-applying again: {}",
                drift.name,
                drift.drifted.join(", ")
            ),
        ));
    }
    (!alerts.is_empty()).then(|| HealthReport {
        source: BIOS_PROFILE_DRIFT_HEALTH_SOURCE.to_string(),
        triggered_by: None,
        observed_at: Some(now),
        alerts,
        successes: vec![],
    })
}

pub(super) fn record_bios_profile_drift(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    host: &HostMachine,
    drift: Option<&BiosProfileDrift>,
    action: &BiosProfileDriftAction,
) {
    let previous = host
        .health_reports
        .merges
        .get(BIOS_PROFILE_DRIFT_HEALTH_SOURCE);
    match drift.and_then(|drift| bios_profile_drift_report(drift, action, previous, Utc::now())) {
        Some(health_report) => {
            if let Some(drift) = drift {
                tracing::warn!(
                    machine_id = %host.id,
                    bios_profile = %drift.name,
                    drifted = ?drift.drifted,
                    unknown = ?drift.unknown,
                    ?action,
                    "BIOS profile drift detected"
                );
            }
            ctx.pending_db_writes
                .push(MachineWriteOp::InsertMachineHealthReport {
                    machine_id: host.id.to_machine_id(),
                    mode: HealthReportApplyMode::Merge,
                    health_report,
                });
        }
        None if previous.is_some() => {
            let machine_id = host.id.to_machine_id();
            let write: WriteOpFn = Box::new(move |txn| {
                async move {
                    db::machine::remove_health_report(
                        txn,
                        &machine_id,
                        HealthReportApplyMode::Merge,
                        BIOS_PROFILE_DRIFT_HEALTH_SOURCE,
                    )
                    .await?;
                    Ok(())
                }
                .boxed()
            });
            ctx.pending_db_writes.push(write);
        }
        None => {}
    }
}

/// Outcome of configure_host_bios function.
pub(super) enum BiosConfigOutcome {
    Done,
    WaitingForReboot(String),
    /// Dell BIOS PATCH returned a job ID; wait for it to complete before boot order.
    WaitingForBiosJob(BiosConfigInfo),
}

/// Outcome of advancing the BIOS config job state machine (Dell: wait for BIOS PATCH job before boot order).
pub(super) enum BiosConfigJobAdvanceOutcome {
    Continue(BiosConfigInfo),
    /// Dell BIOS job completed; proceed to verify settings via PollingBiosSetup.
    Done,
    Failed {
        failure: String,
    },
    /// Same state, but wait (e.g. waiting for power down or BMC to come back).
    Wait(String),
    /// After successful power/BMC recovery from a failed BIOS job: re-run machine_setup (not PollingBiosSetup).
    RetryPlatformConfiguration {
        retry_count: u32,
    },
}

#[derive(Debug)]
pub(super) enum PollingBiosSetupOutcome {
    Verified,
    Wait(String),
    EnterRecovery(BiosConfigInfo),
    Failed { failure: String },
}

/// Outcome of entering HandleBiosJobFailure recovery, or failing once the budget is exhausted.
enum BiosRecoveryAttemptOutcome {
    Continue(BiosConfigInfo),
    Failed { failure: String },
}

impl From<BiosRecoveryAttemptOutcome> for BiosConfigJobAdvanceOutcome {
    fn from(outcome: BiosRecoveryAttemptOutcome) -> Self {
        match outcome {
            BiosRecoveryAttemptOutcome::Continue(info) => Self::Continue(info),
            BiosRecoveryAttemptOutcome::Failed { failure } => Self::Failed { failure },
        }
    }
}

pub(super) async fn configure_host_bios(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    reachability_params: &ReachabilityParams,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    boot_interface: Option<&BootInterfaceTarget>,
    retry_count: u32,
) -> Result<BiosConfigOutcome, StateHandlerError> {
    let selection = HostBiosProfiles::select(
        redfish_client,
        &mh_snapshot.host_snapshot.metadata.labels,
        &ctx.services.site_config,
    )
    .await
    .map_err(|e| redfish_error("select_bios_profile", e))?;
    selection.record(ctx, mh_snapshot);
    let bios_job_id = match call_machine_setup_and_handle_no_dpu_error(
        redfish_client,
        boot_interface,
        mh_snapshot.host_snapshot.associated_dpu_machine_ids().len(),
        selection.bios_profiles(&ctx.services.site_config),
        &ctx.services.site_config,
    )
    .await
    {
        Err(e) => {
            tracing::warn!(
                machine_id = %mh_snapshot.host_snapshot.id,
                error = %e,
                "redfish machine_setup failed, potentially due to known race condition between UEFI POST and BMC. triggering force-restart if needed"
            );

            // if machine_setup failed, reboot to potentially work around
            // a known race between the DPU UEFI and the BMC, where if
            // the BMC is not up when DPU UEFI runs, then Attributes might
            // not come through. The fix is to force-restart the DPU to
            // re-POST.
            //
            // As of July 2024, Josh Price said there's an NBU FR to fix
            // this, but it wasn't target to a release yet.
            let reboot_status = if mh_snapshot
                .host_snapshot
                .status
                .last_reboot_requested
                .is_none()
            {
                handler_host_power_control(mh_snapshot, ctx, ResetType::ForceRestart).await?;

                RebootStatus {
                    increase_retry_count: true,
                    status: "Restarted host".to_string(),
                }
            } else {
                trigger_reboot_if_needed(
                    &mh_snapshot.host_snapshot,
                    mh_snapshot,
                    None,
                    reachability_params,
                    ctx,
                )
                .await?
            };
            return Ok(BiosConfigOutcome::WaitingForReboot(format!(
                "redfish machine_setup failed: {e}; triggered host reboot: {reboot_status:#?}"
            )));
        }
        Ok(jid) => jid,
    };

    if let Some(job_id) = bios_job_id {
        return Ok(BiosConfigOutcome::WaitingForBiosJob(BiosConfigInfo {
            bios_job_id: Some(job_id),
            bios_config_state: BiosConfigState::WaitForBiosJobScheduled,
            retry_count,
        }));
    }

    // No job to wait for (non-Dell or vendor that doesn't return job); reboot to apply and continue.
    handler_host_power_control(mh_snapshot, ctx, ResetType::ForceRestart).await?;
    Ok(BiosConfigOutcome::Done)
}

/// Advance one step of the BIOS config job wait state machine.
pub(super) async fn advance_bios_config_job(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    info: BiosConfigInfo,
) -> Result<BiosConfigJobAdvanceOutcome, StateHandlerError> {
    let machine_controller_config = &ctx.services.site_config.machine_state_controller;
    match info.bios_config_state {
        BiosConfigState::WaitForBiosJobScheduled => {
            let job_id = info.bios_job_id.as_ref().ok_or_else(|| {
                StateHandlerError::GenericError(eyre!(
                    "WaitForBiosJobScheduled requires bios_job_id for host {}",
                    mh_snapshot.host_snapshot.id
                ))
            })?;
            let job_state = redfish_client
                .get_job_state(job_id)
                .await
                .map_err(|e| redfish_error("get_job_state", e))?;
            if job_state.is_error_state() {
                let failure = format!("BIOS job {} failed with state {job_state:#?}", job_id);
                tracing::warn!(
                    reason = %failure,
                    "transitioning to HandleBiosJobFailure (power cycle + BMC reset)"
                );
                return Ok(try_bios_recovery_attempt(
                    machine_controller_config,
                    info.retry_count,
                    info.bios_job_id,
                    failure,
                )?
                .into());
            }
            if matches!(job_state, libredfish::JobState::Completed) {
                tracing::info!(
                    %job_id,
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "BIOS job completed before scheduling was observed; skipping reboot",
                );
                return Ok(BiosConfigJobAdvanceOutcome::Done);
            }
            if !matches!(job_state, libredfish::JobState::Scheduled) {
                return Err(StateHandlerError::GenericError(eyre!(
                    "waiting for BIOS job {:#?} to be scheduled; current state: {job_state:#?}",
                    job_id
                )));
            }
            Ok(BiosConfigJobAdvanceOutcome::Continue(BiosConfigInfo {
                bios_job_id: info.bios_job_id,
                bios_config_state: BiosConfigState::RebootHost,
                retry_count: info.retry_count,
            }))
        }
        BiosConfigState::RebootHost => {
            handler_host_power_control(mh_snapshot, ctx, ResetType::ForceRestart).await?;
            Ok(BiosConfigJobAdvanceOutcome::Continue(BiosConfigInfo {
                bios_job_id: info.bios_job_id,
                bios_config_state: BiosConfigState::WaitForBiosJobCompletion,
                retry_count: info.retry_count,
            }))
        }
        BiosConfigState::WaitForBiosJobCompletion => {
            const JOB_QUERY_WAIT_MINUTES: i64 = 5;
            let job_id = info.bios_job_id.as_ref().ok_or_else(|| {
                StateHandlerError::GenericError(eyre!(
                    "WaitForBiosJobCompletion requires bios_job_id for host {}",
                    mh_snapshot.host_snapshot.id
                ))
            })?;
            let job_state = match redfish_client.get_job_state(job_id).await {
                Ok(s) => s,
                Err(e) => {
                    let minutes_since_state_change = mh_snapshot
                        .host_snapshot
                        .state
                        .version
                        .since_state_change()
                        .num_minutes();
                    if minutes_since_state_change < JOB_QUERY_WAIT_MINUTES {
                        return Err(redfish_error("get_job_state", e));
                    }
                    let failure = format!(
                        "BIOS config job {} lookup failed after {} min: {}",
                        job_id, minutes_since_state_change, e
                    );
                    tracing::warn!(
                        reason = %failure,
                        "transitioning to HandleBiosJobFailure (power cycle + BMC reset)"
                    );
                    return Ok(try_bios_recovery_attempt(
                        machine_controller_config,
                        info.retry_count,
                        info.bios_job_id,
                        failure,
                    )?
                    .into());
                }
            };
            match job_state {
                libredfish::JobState::Completed => Ok(BiosConfigJobAdvanceOutcome::Done),
                _ if job_state.is_error_state() => {
                    let failure = format!(
                        "BIOS config job {} failed with state {job_state:#?}",
                        job_id
                    );
                    tracing::warn!(
                        reason = %failure,
                        "transitioning to HandleBiosJobFailure (power cycle + BMC reset)"
                    );
                    Ok(try_bios_recovery_attempt(
                        machine_controller_config,
                        info.retry_count,
                        info.bios_job_id,
                        failure,
                    )?
                    .into())
                }
                _ => Err(StateHandlerError::GenericError(eyre!(
                    "waiting for BIOS job {:#?} to complete; current state: {job_state:#?}",
                    job_id
                ))),
            }
        }
        BiosConfigState::HandleBiosJobFailure {
            failure,
            power_state,
        } => {
            let bmc = ctx.services.connect_bmc(&mh_snapshot.host_snapshot).await?;
            let current_power_state = crate::redfish::power_state(&bmc)
                .await
                .map_err(|e| platform_error("get_power_state", e))?;

            match power_state {
                ExpectedPowerState::Off => {
                    if current_power_state != Some(PowerState::Off) {
                        handler_host_power_control(mh_snapshot, ctx, ResetType::ForceOff).await?;
                        return Ok(BiosConfigJobAdvanceOutcome::Wait(format!(
                            "HandleBiosJobFailure: waiting for power down; current power state: {current_power_state}; failure: {failure}",
                            current_power_state = power_state_name(current_power_state)
                        )));
                    }
                    tracing::info!(
                        reason = %failure,
                        "HandleBiosJobFailure: resetting BMC after BIOS job failure"
                    );
                    redfish_client
                        .bmc_reset(None)
                        .await
                        .map_err(|e| redfish_error("bmc_reset", e))?;
                    Ok(BiosConfigJobAdvanceOutcome::Continue(BiosConfigInfo {
                        bios_job_id: info.bios_job_id,
                        bios_config_state: BiosConfigState::HandleBiosJobFailure {
                            failure,
                            power_state: ExpectedPowerState::On,
                        },
                        retry_count: info.retry_count,
                    }))
                }
                ExpectedPowerState::On => {
                    if current_power_state != Some(PowerState::On) {
                        let basetime = mh_snapshot
                            .host_snapshot
                            .status
                            .last_reboot_requested
                            .as_ref()
                            .map(|x| x.time)
                            .unwrap_or(mh_snapshot.host_snapshot.state.version.timestamp());
                        let power_down_wait = ctx
                            .services
                            .site_config
                            .machine_state_controller
                            .power_down_wait;
                        if Utc::now().signed_duration_since(basetime) < power_down_wait {
                            return Ok(BiosConfigJobAdvanceOutcome::Wait(format!(
                                "HandleBiosJobFailure: waiting for BMC to come back online; failure: {failure}"
                            )));
                        }
                        handler_host_power_control(mh_snapshot, ctx, ResetType::On).await?;
                        return Ok(BiosConfigJobAdvanceOutcome::Wait(format!(
                            "HandleBiosJobFailure: powering on after BMC reset; failure: {failure}"
                        )));
                    }
                    tracing::info!(
                        retry_count = info.retry_count,
                        "HandleBiosJobFailure: BMC reset complete; re-running platform configuration (machine_setup) — power cycle does not apply BIOS attributes",
                    );
                    Ok(BiosConfigJobAdvanceOutcome::RetryPlatformConfiguration {
                        retry_count: info.retry_count,
                    })
                }
                _ => Err(StateHandlerError::GenericError(eyre!(
                    "HandleBiosJobFailure: unexpected power state {power_state:#?} for {}",
                    mh_snapshot.host_snapshot.id
                ))),
            }
        }
    }
}

/// Enter HandleBiosJobFailure recovery, or move to Failed when budget is exhausted.
fn try_bios_recovery_attempt(
    machine_controller_config: &MachineStateControllerConfig,
    retry_count: u32,
    bios_job_id: Option<String>,
    failure: String,
) -> Result<BiosRecoveryAttemptOutcome, StateHandlerError> {
    if retry_count >= machine_controller_config.max_bios_config_retries {
        tracing::warn!(
            retry_count,
            max_retries = machine_controller_config.max_bios_config_retries,
            reason = %failure,
            "BIOS recovery budget exhausted; moving host to Failed for manual remediation"
        );
        return Ok(BiosRecoveryAttemptOutcome::Failed {
            failure: format!(
                "{failure} (automated BIOS recovery exhausted after {} retries)",
                machine_controller_config.max_bios_config_retries
            ),
        });
    }
    Ok(BiosRecoveryAttemptOutcome::Continue(BiosConfigInfo {
        bios_job_id,
        bios_config_state: BiosConfigState::HandleBiosJobFailure {
            failure,
            power_state: ExpectedPowerState::Off,
        },
        retry_count: retry_count + 1,
    }))
}

pub(super) async fn advance_polling_bios_setup(
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    retry_count: u32,
    machine_controller_config: &MachineStateControllerConfig,
    boot_interface: Option<&BootInterfaceTarget>,
) -> Result<PollingBiosSetupOutcome, StateHandlerError> {
    let stuck_for = mh_snapshot.host_snapshot.state.version.since_state_change();

    let is_bios_setup_result = match boot_interface {
        Some(target) => {
            target
                .run(|bi| redfish_client.is_bios_setup(Some(bi)))
                .await
        }
        None => redfish_client.is_bios_setup(None).await,
    };
    match is_bios_setup_result {
        Ok(true) => {
            tracing::info!("BIOS setup verified successfully");
            Ok(PollingBiosSetupOutcome::Verified)
        }
        Ok(false) => {
            if let Some(outcome) = escalate_stuck_polling_bios_setup(
                machine_controller_config,
                retry_count,
                stuck_for,
            )? {
                return Ok(outcome);
            }
            Ok(PollingBiosSetupOutcome::Wait(format!(
                "Polling BIOS setup status, waiting for settings to be applied (retry_count={retry_count})"
            )))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to check BIOS setup status, will retry"
            );
            Ok(PollingBiosSetupOutcome::Wait(format!(
                "Failed to check BIOS setup status: {e}. Will retry."
            )))
        }
    }
}

fn escalate_stuck_polling_bios_setup(
    machine_controller_config: &MachineStateControllerConfig,
    retry_count: u32,
    stuck_for: chrono::Duration,
) -> Result<Option<PollingBiosSetupOutcome>, StateHandlerError> {
    if stuck_for <= machine_controller_config.polling_bios_setup_stuck_threshold {
        return Ok(None);
    }

    tracing::warn!(
        ?stuck_for,
        retry_count,
        "PollingBiosSetup stuck; attempting HandleBiosJobFailure recovery (power-off + BMC reset + power-on + re-run machine_setup)"
    );

    let failure = format!(
        "PollingBiosSetup stuck for {} minutes (is_bios_setup returned false)",
        stuck_for.num_minutes()
    );

    Ok(Some(
        match try_bios_recovery_attempt(machine_controller_config, retry_count, None, failure)? {
            BiosRecoveryAttemptOutcome::Continue(info) => {
                PollingBiosSetupOutcome::EnterRecovery(info)
            }
            BiosRecoveryAttemptOutcome::Failed { failure } => {
                PollingBiosSetupOutcome::Failed { failure }
            }
        },
    ))
}

pub(super) async fn handle_bios_setup_failed_recovery(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    explicit_target: Option<&BootInterfaceTarget>,
    recovered_state: ManagedHostState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let boot_interface =
        super::resolve_boot_interface_for_step(ctx, mh_snapshot, explicit_target).await?;
    let boot_interface = boot_interface.into_target();
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;
    let is_bios_setup_result = match &boot_interface {
        Some(target) => {
            target
                .run(|bi| redfish_client.is_bios_setup(Some(bi)))
                .await
        }
        None => redfish_client.is_bios_setup(None).await,
    };
    match is_bios_setup_result {
        Ok(true) => {
            tracing::info!("BIOS setup verified after manual remediation; resuming state machine");
            Ok(StateHandlerOutcome::transition(recovered_state))
        }
        Ok(false) => Ok(StateHandlerOutcome::do_nothing()),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to check BIOS setup status, will retry"
            );
            Ok(StateHandlerOutcome::do_nothing())
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_redfish::libredfish::test_support::RedfishSim;
    use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool};
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, check_cases_async, value_scenarios};
    use chrono::TimeZone;
    use model::test_support::machine_snapshot::managed_host_state_snapshot;
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn host_bios_profiles_follow_the_label_and_host_identity() {
        let attributes =
            HashMap::from([("LogicalProc".to_string(), serde_json::json!("Disabled"))]);
        let mut site_config = MachineStateHandlerSiteConfig::test_default();
        site_config.named_bios_profiles = HashMap::from([
            (
                "gpu-perf".to_string(),
                NamedBiosProfile {
                    vendor: RedfishVendor::Dell,
                    model: "PowerEdge_XE9680".to_string(),
                    attributes: attributes.clone(),
                },
            ),
            (
                "hpe-perf".to_string(),
                NamedBiosProfile {
                    vendor: RedfishVendor::Hpe,
                    model: "ProLiant_XD685".to_string(),
                    attributes: attributes.clone(),
                },
            ),
        ]);
        let named = HostBiosProfiles::Named(HashMap::from([(
            RedfishVendor::Dell,
            HashMap::from([(
                "PowerEdge_XE9680".to_string(),
                HashMap::from([(site_config.selected_profile, attributes)]),
            )]),
        )]));
        let fallback = |reason: &str| Yields(HostBiosProfiles::Fallback(reason.to_string()));
        let sim = RedfishSim::default();
        let client = sim
            .create_client("bios-profile-host", None, RedfishAuth::Anonymous, None)
            .await
            .unwrap();
        let (sim, client, site_config) = (&sim, client.as_ref(), &site_config);

        check_cases_async(
            [
                Case {
                    scenario: "no label keeps the site profiles",
                    input: (None, "Dell", "PowerEdge XE9680"),
                    expect: Yields(HostBiosProfiles::SiteDefault),
                },
                Case {
                    scenario: "label naming a recipe for this host selects it",
                    input: (Some("gpu-perf"), "Dell", "PowerEdge XE9680"),
                    expect: Yields(named),
                },
                Case {
                    scenario: "unknown recipe falls back",
                    input: (Some("missing"), "Dell", "PowerEdge XE9680"),
                    expect: fallback("bios-profile missing is not defined in named_bios_profiles"),
                },
                Case {
                    scenario: "vendor mismatch falls back",
                    input: (Some("gpu-perf"), "Lenovo", "PowerEdge XE9680"),
                    expect: fallback(
                        "bios-profile gpu-perf is for vendor Dell but the host BMC is Lenovo",
                    ),
                },
                Case {
                    scenario: "model mismatch falls back",
                    input: (Some("gpu-perf"), "Dell", "PowerEdge R760"),
                    expect: fallback(
                        "bios-profile gpu-perf is for model PowerEdge_XE9680 but the host is PowerEdge_R760",
                    ),
                },
                Case {
                    scenario: "recipe for a vendor whose setup ignores profiles falls back",
                    input: (Some("hpe-perf"), "HPE", "ProLiant XD685"),
                    expect: fallback(
                        "bios-profile hpe-perf is for vendor Hpe, which does not apply BIOS profiles",
                    ),
                },
            ],
            move |(label, vendor, model): (Option<&str>, &str, &str)| {
                sim.set_service_root_vendor(Some(vendor.to_string()));
                sim.set_system_model(model);
                let labels: HashMap<String, String> = label
                    .map(|name| (BIOS_PROFILE_LABEL.to_string(), name.to_string()))
                    .into_iter()
                    .collect();
                async move {
                    HostBiosProfiles::select(client, &labels, site_config)
                        .await
                        .map_err(|e| e.to_string())
                }
            },
        )
        .await;
    }

    #[test]
    fn bios_profile_alert_is_raised_on_fallback_and_cleared_once_resolved() {
        let now = Utc::now();
        let alert = |reason: &str| {
            BiosProfileHealth::Alert(HealthReport {
                source: BIOS_PROFILE_HEALTH_SOURCE.to_string(),
                triggered_by: None,
                observed_at: Some(now),
                alerts: vec![HealthProbeAlert {
                    id: HealthProbeId::from_str("BiosProfileNotApplied").unwrap(),
                    target: Some("gpu-perf".to_string()),
                    in_alert_since: Some(now),
                    message: reason.to_string(),
                    tenant_message: None,
                    classifications: vec![],
                }],
                successes: vec![],
            })
        };
        let fallback = || HostBiosProfiles::Fallback("vendor mismatch".to_string());
        let named = || HostBiosProfiles::Named(HashMap::new());

        check_cases(
            [
                Case {
                    scenario: "fallback raises the alert",
                    input: (fallback(), Some("gpu-perf"), false),
                    expect: Yields(alert("vendor mismatch")),
                },
                Case {
                    scenario: "fallback keeps an earlier alert raised",
                    input: (fallback(), Some("gpu-perf"), true),
                    expect: Yields(alert("vendor mismatch")),
                },
                Case {
                    scenario: "applying the named profile clears the alert",
                    input: (named(), Some("gpu-perf"), true),
                    expect: Yields(BiosProfileHealth::Clear),
                },
                Case {
                    scenario: "removing the label clears the alert",
                    input: (HostBiosProfiles::SiteDefault, None, true),
                    expect: Yields(BiosProfileHealth::Clear),
                },
                Case {
                    scenario: "applying the named profile without an alert changes nothing",
                    input: (named(), Some("gpu-perf"), false),
                    expect: Yields(BiosProfileHealth::Unchanged),
                },
                Case {
                    scenario: "no label and no alert changes nothing",
                    input: (HostBiosProfiles::SiteDefault, None, false),
                    expect: Yields(BiosProfileHealth::Unchanged),
                },
            ],
            |(selection, label, alerted): (HostBiosProfiles, Option<&str>, bool)| {
                let mut host = managed_host_state_snapshot().host_snapshot;
                if let Some(label) = label {
                    host.metadata
                        .labels
                        .insert(BIOS_PROFILE_LABEL.to_string(), label.to_string());
                }
                if alerted {
                    host.health_reports.merges.insert(
                        BIOS_PROFILE_HEALTH_SOURCE.to_string(),
                        HealthReport::empty(BIOS_PROFILE_HEALTH_SOURCE.to_string()),
                    );
                }
                Ok::<_, ()>(selection.health(&host, now))
            },
        );
    }

    #[test]
    fn escalate_stuck_polling_bios_setup_not_triggered_before_threshold() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let result = escalate_stuck_polling_bios_setup(
            &machine_controller_config,
            0,
            chrono::Duration::minutes(10),
        )
        .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn escalate_stuck_polling_bios_setup_enters_handle_bios_job_failure_when_stuck() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let info = escalate_stuck_polling_bios_setup(
            &machine_controller_config,
            0,
            chrono::Duration::minutes(16),
        )
        .unwrap()
        .expect("recovery should be triggered");
        let PollingBiosSetupOutcome::EnterRecovery(info) = info else {
            panic!("expected EnterRecovery");
        };
        assert_eq!(info.bios_job_id, None);
        assert_eq!(info.retry_count, 1);
        assert!(matches!(
            info.bios_config_state,
            BiosConfigState::HandleBiosJobFailure {
                power_state: ExpectedPowerState::Off,
                ..
            }
        ));
    }

    #[test]
    fn escalate_stuck_polling_bios_setup_respects_shared_retry_budget() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let result = escalate_stuck_polling_bios_setup(
            &machine_controller_config,
            machine_controller_config.max_bios_config_retries,
            chrono::Duration::minutes(20),
        )
        .unwrap()
        .expect("expected Failed outcome");

        assert!(matches!(result, PollingBiosSetupOutcome::Failed { .. }));
    }

    #[test]
    fn try_bios_recovery_attempt_fails_when_budget_exhausted() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let result = try_bios_recovery_attempt(
            &machine_controller_config,
            machine_controller_config.max_bios_config_retries,
            Some("job-1".to_string()),
            "job failed".to_string(),
        )
        .unwrap();

        assert!(matches!(result, BiosRecoveryAttemptOutcome::Failed { .. }));
    }

    #[test]
    fn escalate_stuck_polling_bios_setup_allows_last_budgeted_attempt() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let outcome = escalate_stuck_polling_bios_setup(
            &machine_controller_config,
            machine_controller_config.max_bios_config_retries - 1,
            chrono::Duration::minutes(20),
        )
        .unwrap()
        .expect("last budgeted recovery should be allowed");

        let PollingBiosSetupOutcome::EnterRecovery(info) = outcome else {
            panic!("expected EnterRecovery");
        };
        assert_eq!(
            info.retry_count,
            machine_controller_config.max_bios_config_retries
        );
    }

    type BiosAttributes<'a> = Vec<(&'a str, serde_json::Value)>;
    type DriftInspectionInput<'a> = (
        Option<&'a str>,
        &'a str,
        Vec<(&'a str, &'a str)>,
        Vec<(&'a str, &'a str)>,
    );

    fn drift(drifted: &[&str], unknown: &[&str]) -> BiosProfileDrift {
        BiosProfileDrift {
            name: "lenovo-h200".to_string(),
            drifted: drifted
                .iter()
                .map(|attribute| attribute.to_string())
                .collect(),
            unknown: unknown
                .iter()
                .map(|attribute| attribute.to_string())
                .collect(),
        }
    }

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 24, hour, 0, 0).unwrap()
    }

    #[tokio::test]
    async fn bios_profile_drift_reads_the_labelled_recipe_from_the_bmc() {
        let sim = RedfishSim::default();
        let client = sim
            .create_client(
                "bios-profile-drift-host",
                None,
                RedfishAuth::Anonymous,
                None,
            )
            .await
            .unwrap();
        sim.set_system_model("ThinkSystem SR680a V3");
        let mut site_config = MachineStateHandlerSiteConfig::test_default();
        site_config.named_bios_profiles = HashMap::from([(
            "lenovo-h200".to_string(),
            NamedBiosProfile {
                vendor: RedfishVendor::Lenovo,
                model: "ThinkSystem_SR680a_V3".to_string(),
                attributes: HashMap::from([
                    ("A".to_string(), json!("On")),
                    ("C".to_string(), json!("Off")),
                ]),
            },
        )]);
        let attributes = |values: &[(&str, &str)]| {
            values
                .iter()
                .map(|(attribute, value)| (attribute.to_string(), json!(value)))
                .collect::<HashMap<_, _>>()
        };
        let (sim, client, site_config) = (&sim, client.as_ref(), &site_config);

        check_cases_async(
            [
                Case {
                    scenario: "an unlabelled host is not checked",
                    input: (None, "Lenovo", vec![("A", "Off")], vec![]),
                    expect: Yields(None),
                },
                Case {
                    scenario: "a recipe for another vendor is not checked",
                    input: (Some("lenovo-h200"), "Dell", vec![("A", "Off")], vec![]),
                    expect: Yields(None),
                },
                Case {
                    scenario: "matching attributes are not drift",
                    input: (
                        Some("lenovo-h200"),
                        "Lenovo",
                        vec![("A", "On"), ("C", "Off")],
                        vec![],
                    ),
                    expect: Yields(Some((vec![], vec![]))),
                },
                Case {
                    scenario: "a differing attribute is drift",
                    input: (
                        Some("lenovo-h200"),
                        "Lenovo",
                        vec![("A", "Off"), ("C", "Off")],
                        vec![],
                    ),
                    expect: Yields(Some((vec!["A".to_string()], vec![]))),
                },
                Case {
                    scenario: "a fix pending a reboot is not drift",
                    input: (
                        Some("lenovo-h200"),
                        "Lenovo",
                        vec![("A", "Off"), ("C", "Off")],
                        vec![("A", "On")],
                    ),
                    expect: Yields(Some((vec![], vec![]))),
                },
                Case {
                    scenario: "an attribute the BMC does not report is unknown",
                    input: (Some("lenovo-h200"), "Lenovo", vec![("A", "On")], vec![]),
                    expect: Yields(Some((vec![], vec!["C".to_string()]))),
                },
            ],
            move |(label, vendor, current, pending): DriftInspectionInput| {
                sim.set_service_root_vendor(Some(vendor.to_string()));
                sim.set_bios_attributes(HashMap::from([(
                    "Attributes".to_string(),
                    json!(attributes(&current)),
                )]));
                sim.set_pending_bios_attributes(attributes(&pending));
                let labels: HashMap<String, String> = label
                    .map(|name| (BIOS_PROFILE_LABEL.to_string(), name.to_string()))
                    .into_iter()
                    .collect();
                async move {
                    inspect_bios_profile_drift(client, &labels, site_config)
                        .await
                        .map(|drift| drift.map(|drift| (drift.drifted, drift.unknown)))
                        .map_err(|e| e.to_string())
                }
            },
        )
        .await;
    }

    #[test]
    fn bios_value_matching() {
        value_scenarios!(
            run = |(have, want): (serde_json::Value, serde_json::Value)| bios_value_matches(&have, &want);
            "identical values" {
                (json!("Enabled"), json!("Enabled")) => true,
                (json!(3), json!(3)) => true,
                (json!(true), json!(true)) => true,
            }
            "numbers in either representation" {
                (json!("3"), json!(3)) => true,
                (json!(3), json!("3")) => true,
                (json!(3.0), json!(3)) => true,
                (json!("4"), json!(3)) => false,
            }
            "booleans written as text" {
                (json!("True"), json!(true)) => true,
                (json!(false), json!("false")) => true,
                (json!("Enabled"), json!(true)) => false,
            }
            "enumerations stay case sensitive" {
                (json!("enabled"), json!("Enabled")) => false,
                (json!("Disabled"), json!("Enabled")) => false,
            }
        );
    }

    #[test]
    fn bios_profile_comparison() {
        let profile = NamedBiosProfile {
            vendor: RedfishVendor::Lenovo,
            model: "ThinkSystem_SR680a_V3".to_string(),
            attributes: HashMap::from([
                ("A".to_string(), json!("On")),
                ("B".to_string(), json!(1)),
                ("C".to_string(), json!("Off")),
            ]),
        };
        let attributes = |values: Vec<(&str, serde_json::Value)>| {
            values
                .into_iter()
                .map(|(attribute, value)| (attribute.to_string(), value))
                .collect::<HashMap<_, _>>()
        };
        value_scenarios!(
            run = |(current, pending): (BiosAttributes, BiosAttributes)| {
                let drift = compare_bios_profile(
                    "lenovo-h200",
                    &profile,
                    &attributes(current).into_iter().collect(),
                    &attributes(pending),
                );
                (drift.drifted, drift.unknown)
            };
            "the host matches the recipe" {
                (vec![("A", json!("On")), ("B", json!("1")), ("C", json!("Off"))], vec![])
                    => (vec![], vec![]),
            }
            "recipe attributes differ" {
                (vec![("A", json!("Off")), ("B", json!(2)), ("C", json!("Off"))], vec![])
                    => (vec!["A".to_string(), "B".to_string()], vec![]),
            }
            "the recipe value is already pending a reboot" {
                (vec![("A", json!("Off")), ("B", json!(1)), ("C", json!("Off"))], vec![("A", json!("On"))])
                    => (vec![], vec![]),
            }
            "a different value is pending" {
                (vec![("A", json!("Off")), ("B", json!(1)), ("C", json!("Off"))], vec![("A", json!("Auto"))])
                    => (vec!["A".to_string()], vec![]),
            }
            "the BMC does not report a recipe attribute" {
                (vec![("A", json!("On")), ("B", json!(1))], vec![])
                    => (vec![], vec!["C".to_string()]),
            }
        );
    }

    #[test]
    fn bios_profile_drift_actions() {
        use BiosProfileDriftAction as Action;
        value_scenarios!(
            run = |(drift, persistent): (Option<BiosProfileDrift>, bool)| {
                bios_profile_drift_action(drift.as_ref(), persistent)
            };
            "nothing to re-apply" {
                (None, false) => Action::Clear,
                (None, true) => Action::Clear,
                (Some(drift(&[], &[])), true) => Action::Clear,
            }
            "only unknown attributes are reported" {
                (Some(drift(&[], &["C"])), false) => Action::Report,
            }
            "drift is re-applied" {
                (Some(drift(&["A"], &[])), false) => Action::Remediate,
            }
            "drift that survived a re-apply and reboot is not re-applied again" {
                (Some(drift(&["A"], &[])), true) => Action::Persistent,
            }
        );
    }

    #[test]
    fn bios_profile_drift_reports() {
        use BiosProfileDriftAction as Action;
        let previous = HealthReport {
            source: BIOS_PROFILE_DRIFT_HEALTH_SOURCE.to_string(),
            triggered_by: None,
            observed_at: Some(at(1)),
            alerts: vec![HealthProbeAlert {
                id: HealthProbeId::from_str(BIOS_PROFILE_DRIFT_REMEDIATING_PROBE).unwrap(),
                target: Some("lenovo-h200".to_string()),
                in_alert_since: Some(at(1)),
                message: String::new(),
                tenant_message: None,
                classifications: vec![],
            }],
            successes: vec![],
        };
        value_scenarios!(
            run = |(drift, action): (BiosProfileDrift, BiosProfileDriftAction)| {
                bios_profile_drift_report(&drift, &action, Some(&previous), at(5)).map(|report| {
                    report
                        .alerts
                        .into_iter()
                        .map(|alert| (alert.id.to_string(), alert.in_alert_since))
                        .collect::<Vec<_>>()
                })
            };
            "no alerts clears the report" {
                (drift(&[], &[]), Action::Clear) => None,
            }
            "drift that is not being re-applied" {
                (drift(&["A"], &["C"]), Action::Report) => Some(vec![
                    (BIOS_PROFILE_DRIFT_PROBE.to_string(), Some(at(5))),
                    (BIOS_PROFILE_UNKNOWN_ATTRIBUTES_PROBE.to_string(), Some(at(5))),
                ]),
            }
            "remediation keeps the time it started" {
                (drift(&["A"], &[]), Action::Remediate) => Some(vec![
                    (BIOS_PROFILE_DRIFT_PROBE.to_string(), Some(at(5))),
                    (BIOS_PROFILE_DRIFT_REMEDIATING_PROBE.to_string(), Some(at(1))),
                ]),
            }
            "persistent drift keeps the remediation marker so it is not retried" {
                (drift(&["A"], &[]), Action::Persistent) => Some(vec![
                    (BIOS_PROFILE_DRIFT_PROBE.to_string(), Some(at(5))),
                    (BIOS_PROFILE_DRIFT_REMEDIATING_PROBE.to_string(), Some(at(1))),
                    (BIOS_PROFILE_DRIFT_PERSISTENT_PROBE.to_string(), Some(at(5))),
                ]),
            }
        );
    }
}
