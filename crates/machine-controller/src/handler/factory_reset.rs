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

//! Deletion-only, site-gated host BMC factory-reset sub-flow.
//!
//! Tenant release already power-cycles the host, wipes NVMe, and re-verifies
//! BIOS/boot-order. This sub-state machine runs *first*, factory-resetting the
//! host BMC to proactively clear wedged BMC state (bad/rejected boot order seen
//! on GB200, Grace-Grace, SMC) before the existing `PowerCycle` flow takes over.
//!
//! The flow, keyed off [`FactoryResetBmcState`]:
//! 1. [`CheckPreconditions`](FactoryResetBmcState::CheckPreconditions) - run once
//!    on entry: the site gate (transparent pass-through to `PowerCycle` when
//!    disabled) and a one-shot check for a usable `expected_machines` factory-cred
//!    entry, skipping the reset (straight to `PowerCycle`) when it is absent so a
//!    config gap never parks the release. Kept separate from the
//!    acknowledgement-wait below so the `expected_machines` lookup runs once, not
//!    on every controller dispatch during the wait.
//! 2. [`SuppressExploration`](FactoryResetBmcState::SuppressExploration) - use the
//!    shared [`site_explorer_pause`] handshake (the same one BMC rotation uses)
//!    to idempotently suppress site-explorer for the host BMC and wait for it to
//!    acknowledge (proceeding anyway once the shared pause budget elapses).
//! 3. [`ResetToDefaults`](FactoryResetBmcState::ResetToDefaults) - issue
//!    `Manager.ResetToDefaults` with stored credentials.
//! 4. [`WaitForBmc`](FactoryResetBmcState::WaitForBmc) - wait for an anonymous
//!    service-root read to succeed (polls at the controller cadence; never
//!    parks - a BMC that never returns is surfaced by the time-in-state SLA).
//! 5. [`RestoreCredentials`](FactoryResetBmcState::RestoreCredentials) - first a
//!    login-backoff probe of the factory creds (never parks; degrades to hourly
//!    polling to avoid BMC auth-lockout), with a per-device crash-recovery probe;
//!    then, once verified, change the BMC root password from factory default
//!    back to the device's previous per-device credential read from Vault. Vault
//!    is never written or deleted; any site-wide drift is left to the passive
//!    `RotatingBmc` rotation later. A genuine password-change failure parks.
//! 6. [`RemoveSuppression`](FactoryResetBmcState::RemoveSuppression) - delete the
//!    suppression and hand off to `PowerCycle`.
//!
//! Failures in reset / restore park the instance (blocking termination) per the
//! feature's failure-handling requirement. `WaitForBmc` instead keeps waiting
//! (blocking termination too) and leans on the time-in-state SLA to surface a
//! BMC that never returns.
//!
//! ## Per-vendor `bmc_reset_to_defaults` semantics (verified against libredfish)
//!
//! The restore target is always `expected_machines.bmc_password`, so that value
//! must be whatever credential the BMC presents *after* its reset for each SKU:
//! - NVIDIA GB200 (`nvidia_gbx00`), Grace-Grace/GH200 (`nvidia_gh200`), and SMC
//!   AMI/Viking (`ami`), plus Dell (`dell`), Lenovo, and HPE all issue the
//!   standard `Manager.ResetToDefaults` with `ResetType: ResetAll`, landing on a
//!   fixed factory-default credential captured at ingestion.
//! - Supermicro (`supermicro`) instead issues the OEM `SmcManagerConfig.Reset`
//!   with `Option: ClearConfig`, which restores the *unique per-chassis factory
//!   password printed on the chassis label* - not a fixed default. So for a
//!   Supermicro host, `expected_machines.bmc_password` must hold that
//!   chassis-label password, or `RestoreCredentials` degrades to hourly polling
//!   until an operator corrects it (it never parks there, so it self-heals).
//! - Dell's `Manager.ResetToDefaults` is issued with no lockdown unlock (the
//!   admin CLI does the same and no evidence of a lockdown rejection was found).
//!   If a locked-down iDRAC ever rejects it, `ResetToDefaults` returns a
//!   non-`NotSupported` error and the instance parks for operator attention
//!   rather than silently attempting a speculative unlock.

use carbide_credential_rotation::site_explorer_pause::{self, GateDecision};
use carbide_redfish::libredfish::RedfishAuth;
use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
use eyre::eyre;
use libredfish::RedfishError;
use libredfish::model::service_root::RedfishVendor;
use mac_address::MacAddress;
use model::machine::{
    FactoryResetBmcState, HostPlatformConfigurationState, InstanceState, ManagedHostState,
    ManagedHostStateSnapshot,
};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::MachineStateHandlerContextObjects;

/// The `reason` this flow stamps on the site-explorer suppression it owns.
/// Deletes in [`remove_suppression`] are scoped to it (via the shared
/// [`site_explorer_pause`] module) so a factory reset never removes a
/// rotation's or an operator's suppression for the same BMC, and vice versa.
const FACTORY_RESET_SUPPRESSION_REASON: &str = "factory_reset_bmc";

/// Small intra-tick delay between the two credential probes, mirroring
/// site-explorer's `BMC_AUTH_RETRY_DURATION`, so a BMC that throttles after a
/// failed auth has a moment to recover before the second probe.
const BMC_AUTH_PROBE_DELAY_SECS: u64 = 3;

/// One-shot settle window in [`WaitForBmc`](FactoryResetBmcState::WaitForBmc):
/// how long to let the just-reset BMC actually drop before the first anonymous
/// readiness probe. Hardcoded rather than site-configurable because it is only
/// anti-false-positive spacing: `ResetToDefaults` transitions into `WaitForBmc`,
/// and a transition is requeued immediately, so without this the first probe
/// could catch the BMC still briefly serving on its pre-reset firmware and
/// prematurely advance to `RestoreCredentials`.
const BMC_POST_RESET_SETTLE_SECS: i64 = 30;

/// Linear step for the [`RestoreCredentials`](FactoryResetBmcState::RestoreCredentials)
/// factory-credential verify backoff: retry `N` (for `N` in `1..=4`) waits
/// `RESTORE_VERIFY_BACKOFF_STEP_MINS * N` minutes (5, 10, 15, 20).
const RESTORE_VERIFY_BACKOFF_STEP_MINS: i64 = 5;

/// Cap for the factory-credential verify backoff: once `retry_count` grows past
/// the linear ramp, probes settle to this fixed interval indefinitely, keeping
/// the login rate low enough to avoid a BMC auth-lockout while still blocking
/// termination.
const RESTORE_VERIFY_BACKOFF_CAP_HOURS: i64 = 1;

/// How often (in `RestoreCredentials` factory-verify retries) to run the
/// per-device crash-recovery probe. That probe only matters in the rare window
/// where a prior attempt changed the password but crashed before committing the
/// state transition, so it is wasteful to run on every failed factory check.
/// Because `retry_count` starts at 0 (and `0 % N == 0`), the common crash case,
/// which re-enters at retry 0, is still caught on the very first re-dispatch.
const CRASH_RECOVERY_PROBE_INTERVAL: u32 = 5;

/// Dispatch a `FactoryResetBmc` sub-state. Called from the top of
/// `handle_instance_host_platform_config`, *before* the shared stored-credential
/// Redfish client is built, because that build makes authenticated calls that
/// would fail against a BMC that is mid-reset or on factory credentials. Each
/// sub-state builds exactly the client (if any) it needs.
pub(super) async fn handle_factory_reset_bmc(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    reset_state: FactoryResetBmcState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    match reset_state {
        FactoryResetBmcState::CheckPreconditions => check_preconditions(ctx, mh_snapshot).await,
        FactoryResetBmcState::SuppressExploration => suppress_exploration(ctx, mh_snapshot).await,
        FactoryResetBmcState::ResetToDefaults => reset_to_defaults(ctx, mh_snapshot).await,
        FactoryResetBmcState::WaitForBmc => wait_for_bmc(ctx, mh_snapshot).await,
        FactoryResetBmcState::RestoreCredentials { retry_count } => {
            restore_credentials(ctx, mh_snapshot, retry_count).await
        }
        FactoryResetBmcState::RemoveSuppression => remove_suppression(ctx, mh_snapshot).await,
    }
}

/// A transition back into `FactoryResetBmc` at `reset_state`.
fn transition_to_factory_reset_bmc(
    reset_state: FactoryResetBmcState,
) -> StateHandlerOutcome<ManagedHostState> {
    StateHandlerOutcome::transition(ManagedHostState::Assigned {
        instance_state: InstanceState::HostPlatformConfiguration {
            platform_config_state: HostPlatformConfigurationState::FactoryResetBmc { reset_state },
        },
    })
}

/// Hand off to the existing `PowerCycle` flow, computing the initial power phase
/// from a live power read (the read the deletion branch used to do inline before
/// it delegated to this sub-flow).
async fn transition_to_power_cycle(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let power_on = super::is_host_powered_off(mh_snapshot, ctx).await?;
    Ok(StateHandlerOutcome::transition(
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostPlatformConfiguration {
                platform_config_state: HostPlatformConfigurationState::PowerCycle {
                    power_on,
                    power_on_retry_count: 0,
                },
            },
        },
    ))
}

/// The host BMC MAC keys both the suppression and the credential bookkeeping; a
/// host without one can be neither tracked nor reached.
fn require_bmc_mac(
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<MacAddress, StateHandlerError> {
    mh_snapshot
        .host_snapshot
        .status
        .bmc_info
        .mac
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: mh_snapshot.host_snapshot.id.to_string(),
            missing: "bmc_mac",
        })
}

/// The BMC host string and port for the low-level `RedfishClientPool` calls.
fn bmc_host_port(
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<(String, Option<u16>), StateHandlerError> {
    let addr =
        mh_snapshot
            .host_snapshot
            .bmc_addr()
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: mh_snapshot.host_snapshot.id.to_string(),
                missing: "BMC Endpoint Information (bmc_info.ip)",
            })?;
    Ok((addr.ip().to_string(), Some(addr.port())))
}

/// Outcome of looking up the factory BMC credentials for a host in
/// `expected_machines`. The two unusable cases are kept distinct so callers can
/// log which config problem occurred; note "unusable" here is purely structural
/// (missing row / empty strings), not a live credential check -- whether the BMC
/// actually accepts the credentials is only determined at login in
/// [`restore_credentials`].
enum FactoryCredentialLookup {
    /// A usable entry: non-empty username and password, plus the
    /// `bmc_retain_credentials` flag.
    Usable {
        credentials: Credentials,
        retain: bool,
    },
    /// No `expected_machines` entry exists for this BMC MAC.
    NoEntry,
    /// An `expected_machines` entry exists but its BMC credentials are empty.
    EmptyCredentials,
}

impl FactoryCredentialLookup {
    /// A human-readable reason the credentials are unusable, or `None` when
    /// [`Usable`](FactoryCredentialLookup::Usable). Shared by the skip log in
    /// [`check_preconditions`] and the park error in
    /// [`load_validated_factory_credentials`] so both describe the case the same
    /// way.
    fn unusable_reason(&self) -> Option<&'static str> {
        match self {
            Self::Usable { .. } => None,
            Self::NoEntry => Some("no expected_machines entry exists for the host BMC"),
            Self::EmptyCredentials => {
                Some("the expected_machines entry for the host BMC has empty factory credentials")
            }
        }
    }
}

/// Look up the factory BMC credentials from `expected_machines` for this host,
/// distinguishing a missing entry from an entry with empty credentials (see
/// [`FactoryCredentialLookup`]). `Err` is reserved for a real datastore failure.
async fn lookup_factory_credentials(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    host_bmc_mac: MacAddress,
) -> Result<FactoryCredentialLookup, StateHandlerError> {
    let Some(expected) =
        db::expected_machine::find_by_bmc_mac_address(&ctx.services.db_pool, host_bmc_mac).await?
    else {
        return Ok(FactoryCredentialLookup::NoEntry);
    };

    if expected.data.bmc_username.is_empty() || expected.data.bmc_password.is_empty() {
        return Ok(FactoryCredentialLookup::EmptyCredentials);
    }

    Ok(FactoryCredentialLookup::Usable {
        credentials: Credentials::UsernamePassword {
            username: expected.data.bmc_username,
            password: expected.data.bmc_password,
        },
        retain: expected.data.bmc_retain_credentials.unwrap_or(false),
    })
}

/// Load the factory BMC credentials from `expected_machines`, parking if there
/// is no usable entry - without them we can never log back in to restore the
/// device. Returns the credentials and the `bmc_retain_credentials` flag.
///
/// Used by `restore_credentials`, where a missing entry is anomalous: the host
/// has already been factory-reset, so `check_preconditions` must have seen a
/// usable entry earlier, and losing it now blocks the restore.
async fn load_validated_factory_credentials(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    host_bmc_mac: MacAddress,
) -> Result<(Credentials, bool), StateHandlerError> {
    match lookup_factory_credentials(ctx, host_bmc_mac).await? {
        FactoryCredentialLookup::Usable {
            credentials,
            retain,
        } => Ok((credentials, retain)),
        unusable => Err(StateHandlerError::ManualInterventionRequired(format!(
            "BMC factory reset for machine {} cannot recover the factory password: {} ({host_bmc_mac})",
            mh_snapshot.host_snapshot.id,
            unusable
                .unusable_reason()
                .unwrap_or("no usable factory credentials"),
        ))),
    }
}

/// Read the device's previous per-device BMC root credential from Vault
/// (`BmcRoot { mac }`). Read-only: this flow never writes or deletes Vault.
async fn read_per_device_bmc_cred(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    host_bmc_mac: MacAddress,
) -> Result<Option<Credentials>, StateHandlerError> {
    let key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: host_bmc_mac,
        },
    };
    ctx.services
        .credential_manager
        .get_credentials(&key)
        .await
        .map_err(|e| StateHandlerError::GenericError(eyre!("read per-device BMC secret: {e}")))
}

/// Flush any cached BMC Redfish session so later logins re-authenticate cleanly
/// against the just-changed credential. Hygiene rather than required (the reset
/// already invalidates sessions), mirroring `rotate_bmc`.
async fn flush_bmc_session(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    host_bmc_mac: MacAddress,
) -> Result<(), StateHandlerError> {
    let mut conn = ctx.services.db_pool.acquire().await?;
    db::bmc_redfish_session::delete_by_mac(&mut conn, host_bmc_mac).await?;
    Ok(())
}

/// Backoff before the next factory-credential login probe, derived from the
/// state version timestamp and `retry_count`: immediate first probe, then
/// 5/10/15/20 min, then a fixed hour. Never parks - degrades to hourly polling.
fn verify_backoff(retry_count: u32) -> chrono::Duration {
    match retry_count {
        0 => chrono::Duration::zero(),
        1..=4 => {
            chrono::Duration::minutes(RESTORE_VERIFY_BACKOFF_STEP_MINS * i64::from(retry_count))
        }
        _ => chrono::Duration::hours(RESTORE_VERIFY_BACKOFF_CAP_HOURS),
    }
}

/// Entry point, run once: gate the whole sub-flow and confirm the reset is
/// recoverable before anything touches the hardware or suppresses exploration.
///
/// Two checks, in order, either of which routes straight to the existing
/// `PowerCycle` deletion path without entering the reset:
/// - **Site gate.** When the feature is disabled this sub-flow is a transparent
///   pass-through, so non-opted-in sites behave exactly as before apart from this
///   one cheap tick.
/// - **Factory-credential pre-flight.** If there is no usable `expected_machines`
///   entry to recover the factory password afterward, skip the reset (best-effort
///   feature) and log, rather than parking the tenant's release on a config gap.
///   A real datastore error still propagates.
///
/// This lives in its own one-shot state (rather than on every
/// [`suppress_exploration`] dispatch) so the `expected_machines` lookup stays off
/// the hot acknowledgement-wait loop. `RestoreCredentials` re-loads and, there,
/// *parks* if the entry vanishes after we have already reset -- anomalous, unlike
/// the pre-reset skip here.
async fn check_preconditions(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    if !ctx
        .services
        .site_config
        .bmc_factory_reset_on_instance_termination_enabled
    {
        return transition_to_power_cycle(ctx, mh_snapshot).await;
    }

    let host_bmc_mac = require_bmc_mac(mh_snapshot)?;

    // Only enter the reset when we can actually recover the factory password
    // afterward. With no usable expected_machines entry, skip the whole sub-flow
    // and continue the normal deletion path instead of parking the release. The
    // two unusable cases (no entry vs. empty credentials) are logged distinctly
    // so the specific config gap is diagnosable.
    if let Some(reason) = lookup_factory_credentials(ctx, host_bmc_mac)
        .await?
        .unusable_reason()
    {
        tracing::warn!(
            %host_bmc_mac,
            reason,
            "cannot recover host BMC factory password; skipping BMC factory reset and continuing tenant release"
        );
        return transition_to_power_cycle(ctx, mh_snapshot).await;
    }

    Ok(transition_to_factory_reset_bmc(
        FactoryResetBmcState::SuppressExploration,
    ))
}

/// Establish the site-explorer suppression for the host BMC and wait for it to
/// be acknowledged before advancing to the reset.
///
/// The gate and factory-credential pre-flight already ran once in
/// [`check_preconditions`], so this state is purely the suppress/acknowledge/
/// budget handshake, delegated to the shared [`site_explorer_pause`] module --
/// the same barrier BMC credential rotation uses -- scoped to
/// [`FACTORY_RESET_SUPPRESSION_REASON`]. It idempotently ensures the suppression
/// row exists, returns [`GateDecision::Proceed`] once site-explorer has
/// acknowledged it (or once the pause budget elapses, so a disabled/`listen_only`
/// explorer that can never ack does not wedge us -- the row alone already makes
/// the next sweep skip this BMC), and otherwise [`GateDecision::Wait`].
async fn suppress_exploration(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host_bmc_mac = require_bmc_mac(mh_snapshot)?;

    match site_explorer_pause::gate_before_credential_change(
        &ctx.services.db_pool,
        &[host_bmc_mac],
        FACTORY_RESET_SUPPRESSION_REASON,
    )
    .await?
    {
        GateDecision::Wait => Ok(StateHandlerOutcome::wait(format!(
            "awaiting site-explorer acknowledgement of BMC {host_bmc_mac} suppression before factory reset"
        ))),
        GateDecision::Proceed => Ok(transition_to_factory_reset_bmc(
            FactoryResetBmcState::ResetToDefaults,
        )),
    }
}

async fn reset_to_defaults(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host_bmc_mac = require_bmc_mac(mh_snapshot)?;

    // The BMC is still on its normal (per-device / site-wide) credential here, so
    // a stored-credential client authenticates.
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;

    match redfish_client.bmc_reset_to_defaults().await {
        Ok(()) => {
            tracing::info!(
                %host_bmc_mac,
                "issued host BMC factory reset; waiting for the BMC to return"
            );
            Ok(transition_to_factory_reset_bmc(
                FactoryResetBmcState::WaitForBmc,
            ))
        }
        // A BMC that does not support factory reset cannot be sanitized this way;
        // skip the restore and continue the release rather than blocking it.
        Err(RedfishError::NotSupported(msg)) => {
            tracing::warn!(
                %host_bmc_mac,
                %msg,
                "BMC does not support factory reset; skipping BMC sanitize and continuing tenant release"
            );
            Ok(transition_to_factory_reset_bmc(
                FactoryResetBmcState::RemoveSuppression,
            ))
        }
        Err(e) => Err(redfish_error("bmc_reset_to_defaults", e)),
    }
}

async fn wait_for_bmc(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host_bmc_mac = require_bmc_mac(mh_snapshot)?;

    // Give the just-reset BMC one interval to drop and start coming back before
    // the first readiness probe. `entered_at` is stable (set on entry to this
    // state), so this is a one-shot settle window, not a per-probe gate.
    let settle = chrono::Duration::seconds(BMC_POST_RESET_SETTLE_SECS);
    let entered_at = mh_snapshot.host_snapshot.state.version.timestamp();
    if super::wait(&entered_at, settle) {
        return Ok(StateHandlerOutcome::wait(format!(
            "settling {settle} before probing BMC {host_bmc_mac} after factory reset"
        )));
    }

    let (host, port) = bmc_host_port(mh_snapshot)?;

    // Readiness = a successful anonymous service-root read. The `Unknown` path
    // does no I/O at client creation and works against a just-reset BMC on its
    // factory password without consuming an authenticated login attempt.
    let redfish = ctx.services.bmc_credential_ops.clone();
    let ready = match redfish
        .create_client(
            &host,
            port,
            RedfishAuth::Anonymous,
            Some(RedfishVendor::Unknown),
        )
        .await
    {
        Ok(client) => client.get_service_root().await.is_ok(),
        Err(_) => false,
    };

    if ready {
        tracing::info!(
            %host_bmc_mac,
            "host BMC returned after factory reset; restoring credentials"
        );
        return Ok(transition_to_factory_reset_bmc(
            FactoryResetBmcState::RestoreCredentials { retry_count: 0 },
        ));
    }

    // Not back yet. The anonymous probe consumes no auth attempt, so there is no
    // lockout risk and nothing to back off - just keep waiting. The controller
    // re-dispatches on its normal cadence, and a BMC that never returns is
    // surfaced by the `HostPlatformConfiguration` time-in-state SLA rather than
    // parked here (this flow already blocks termination until it proceeds).
    Ok(StateHandlerOutcome::wait(format!(
        "waiting for BMC {host_bmc_mac} to return after factory reset"
    )))
}

/// Verify the factory credentials, then change the BMC root password from the
/// factory default back to the device's previous per-device credential.
///
/// The verification and the mutation live in one state because the verify is
/// purely a precondition of the restore. They keep two deliberately different
/// failure semantics, keyed off which step failed:
/// - The factory-credential probe **never parks**. A rejected factory login
///   just means the BMC is not ready yet (or the creds are wrong), so it
///   degrades to the [`verify_backoff`] schedule (immediate, 5/10/15/20 min,
///   then hourly forever) to avoid a BMC auth-lockout and to auto-recover once
///   an operator fixes the creds. `retry_count` drives that backoff.
/// - Once the factory credentials verify, a genuine password-change failure is
///   anomalous and **parks** for operator attention (after a crash-recovery
///   probe that catches an already-restored device).
async fn restore_credentials(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    retry_count: u32,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host_bmc_mac = require_bmc_mac(mh_snapshot)?;
    let (factory_creds, retain_credentials) =
        load_validated_factory_credentials(ctx, mh_snapshot, host_bmc_mac).await?;

    // A retain-credentials host is left on factory default, which is exactly what
    // Vault already holds for it, so there is nothing to verify or restore.
    if retain_credentials {
        return Ok(transition_to_factory_reset_bmc(
            FactoryResetBmcState::RemoveSuppression,
        ));
    }

    // Login backoff gate derived from the state version timestamp.
    let entered_at = mh_snapshot.host_snapshot.state.version.timestamp();
    if super::wait(&entered_at, verify_backoff(retry_count)) {
        return Ok(StateHandlerOutcome::do_nothing());
    }

    let (host, port) = bmc_host_port(mh_snapshot)?;
    let redfish = ctx.services.bmc_credential_ops.clone();

    // Step 1: verify the factory credentials. Until they authenticate we never
    // mutate and never park.
    if !redfish
        .bmc_credentials_valid(&host, port, factory_creds.clone())
        .await?
    {
        // Factory creds rejected: normally the BMC just is not ready yet. Rarely,
        // a prior attempt already restored the per-device credential and crashed
        // before committing the transition (the password change and the state
        // commit are not atomic). That crash-recovery case is an edge case, so
        // probe the per-device credential only every `CRASH_RECOVERY_PROBE_INTERVAL`
        // retries rather than on every failed factory check; the cross-tick verify
        // backoff already spaces these probes, so no extra intra-tick delay is
        // needed.
        if retry_count.is_multiple_of(CRASH_RECOVERY_PROBE_INTERVAL)
            && let Some(per_device) = read_per_device_bmc_cred(ctx, host_bmc_mac).await?
            && redfish
                .bmc_credentials_valid(&host, port, per_device)
                .await?
        {
            tracing::warn!(
                %host_bmc_mac,
                "factory credentials invalid but per-device credential already valid; treating BMC as already restored (prior attempt succeeded before advancing)"
            );
            return Ok(transition_to_factory_reset_bmc(
                FactoryResetBmcState::RemoveSuppression,
            ));
        }

        // Neither credential authenticated. Bump `retry_count` (refreshing the
        // state version timestamp, which arms the next backoff window). Never
        // park: this degrades to hourly polling, which blocks termination while
        // staying clear of BMC auth-lockout and auto-recovers once the operator
        // fixes the creds.
        tracing::warn!(
            %host_bmc_mac,
            retry_count,
            "factory BMC credentials not yet valid after reset; scheduling a backed-off retry"
        );
        return Ok(transition_to_factory_reset_bmc(
            FactoryResetBmcState::RestoreCredentials {
                retry_count: retry_count + 1,
            },
        ));
    }

    // Step 2: factory creds verified, so restore the device to its previous
    // per-device credential. A missing Vault entry here is anomalous for an
    // ingested host and parks (we can never restore without it).
    let per_device = read_per_device_bmc_cred(ctx, host_bmc_mac).await?.ok_or_else(|| {
        StateHandlerError::ManualInterventionRequired(format!(
            "BMC factory reset for machine {} cannot restore credentials: the per-device BmcRoot \
             Vault entry for {host_bmc_mac} is missing (anomalous for an ingested host)",
            mh_snapshot.host_snapshot.id
        ))
    })?;
    let Credentials::UsernamePassword {
        password: per_device_password,
        ..
    } = per_device.clone();

    // Resolve the exact dispatch vendor, then change the device from factory
    // default back to its previous per-device password.
    let vendor = redfish
        .probe_bmc_vendor(&host, port, factory_creds.clone())
        .await?;

    match redfish
        .set_bmc_root_password(&host, port, vendor, factory_creds, per_device_password)
        .await
    {
        Ok(()) => {
            flush_bmc_session(ctx, host_bmc_mac).await?;
            tracing::info!(
                %host_bmc_mac,
                "restored host BMC to its previous per-device credential after factory reset"
            );
            Ok(transition_to_factory_reset_bmc(
                FactoryResetBmcState::RemoveSuppression,
            ))
        }
        Err(change_err) => {
            // Crash-recovery / same-value guard, mirroring `rotate_bmc`: the
            // change failed, but if the device already carries the per-device
            // credential a prior attempt succeeded and we crashed before
            // advancing. Verify before parking.
            tokio::time::sleep(std::time::Duration::from_secs(BMC_AUTH_PROBE_DELAY_SECS)).await;
            if redfish
                .bmc_credentials_valid(&host, port, per_device)
                .await?
            {
                flush_bmc_session(ctx, host_bmc_mac).await?;
                tracing::warn!(
                    %host_bmc_mac,
                    "password change reported an error but the per-device credential is already valid; treating BMC as already restored (prior attempt succeeded before advancing)"
                );
                return Ok(transition_to_factory_reset_bmc(
                    FactoryResetBmcState::RemoveSuppression,
                ));
            }
            Err(StateHandlerError::from(change_err))
        }
    }
}

async fn remove_suppression(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host_bmc_mac = require_bmc_mac(mh_snapshot)?;

    // Compute the PowerCycle hand-off (needs a live power read) before opening
    // the deletion transaction, so no row lock is held across the Redfish call.
    let power_cycle = transition_to_power_cycle(ctx, mh_snapshot).await?;

    let mut txn = ctx.services.db_pool.begin().await?;
    site_explorer_pause::resume_after_credential_change(
        &mut txn,
        &[host_bmc_mac],
        FACTORY_RESET_SUPPRESSION_REASON,
    )
    .await?;
    Ok(power_cycle.with_txn(txn))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use chrono::Duration;

    use super::*;

    /// The factory-credential verify backoff must probe immediately on entry,
    /// ramp linearly 5/10/15/20 min for the first four retries, then flatten to
    /// hourly polling forever so a wedged BMC never triggers an auth-lockout and
    /// never parks the instance.
    #[test]
    fn verify_backoff_ramps_then_caps_hourly() {
        value_scenarios!(
            run = |retry_count: u32| verify_backoff(retry_count);
            "immediate first probe" {
                0u32 => Duration::zero(),
            }
            "linear ramp for the first four retries" {
                1u32 => Duration::minutes(5),
                2u32 => Duration::minutes(10),
                3u32 => Duration::minutes(15),
                4u32 => Duration::minutes(20),
            }
            "flattens to hourly polling to avoid auth-lockout" {
                5u32 => Duration::hours(1),
                6u32 => Duration::hours(1),
                100u32 => Duration::hours(1),
                u32::MAX => Duration::hours(1),
            }
        );
    }
}
