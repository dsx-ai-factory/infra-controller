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

//! Shared per-device credential rotation engine.
//!
//! `RotateCredential` only *stages* a site-wide rotation: it writes the
//! rotate-TO secret at the next version and bumps
//! `sitewide_credential_rotation.target_version`. This crate houses the engine
//! that *converges* an individual device to that staged target, so the
//! machine-, switch-, and power-shelf-controllers share one implementation
//! instead of each re-deriving the BMC password dance.
//!
//! Today it implements BMC root rotation ([`rotate_bmc`]) and BF4 DPU BMC
//! `service`-account rotation ([`rotate_dpu_bmc_service`]); Host/DPU UEFI
//! rotation reuses the controllers' existing job-polling UEFI-setup state
//! machines and lives there.
//!
//! # BMC flow (single synchronous step + crash marker)
//!
//! The BMC password primitive
//! ([`carbide_redfish::libredfish::BmcCredentialOps::set_bmc_root_password`])
//! is synchronous (no BIOS job to poll), so BMC rotation is one step guarded by
//! the `rotating_to_version` crash marker:
//!
//! 1. Read [`device_rotation_status`]; skip converged / quarantined / orphaned
//!    rows.
//! 2. [`mark_device_rotating_to_version`] to the live target (crash marker).
//! 3. Change the password with **change-then-verify recovery**: authenticate
//!    with the current per-device secret and change to the rotate-TO value. On
//!    failure, ask the BMC whether the rotate-TO value *already* authenticates
//!    ([`BmcCredentialOps::bmc_credentials_valid`]) -- if so, a prior attempt
//!    changed the hardware before crashing and the device is already at target.
//!    This never re-issues a same-value (`new -> new`) change, which some BMCs
//!    reject under a password-reuse policy, and costs at most one extra failed
//!    login so it stays clear of BMC lockout.
//! 4. On success (or an already-at-target verify): persist the per-device secret
//!    at the new password, flush cached BMC Redfish sessions, and
//!    [`promote_rotating_to_current`].
//! 5. On failure: [`increment_rotate_attempt`] with a redacted error and an
//!    exponential [`backoff_until`] window, and report
//!    [`RotateOutcome::Quarantined`].
//!
//! Crash safety: a crash between step 2 and step 4 leaves `rotating_to_version`
//! set and `current_version` behind the target, so the next tick re-enters and
//! the change-then-verify recovery reconciles the hardware regardless of which
//! side of the change the crash landed on. A target that advanced in the
//! meantime is superseded automatically: step 2 re-marks to the *live* target
//! every tick.
//!
//! The recovery path intentionally does not re-apply the vendor password policy
//! ([`carbide_redfish::libredfish::BmcCredentialOps::set_bmc_root_password`]
//! applies it on every change). That policy is a *static* per-vendor setting,
//! and every password the device has ever carried -- including its initial
//! provisioning value -- was set through `set_bmc_root_password`, so a device
//! already at the rotate-TO value already has the policy in effect; recovery has
//! nothing to restore.
//!
//! # Entry gate ([`RotationGate`])
//!
//! Controllers must not pay a per-device `device_rotation_status` query on every
//! 30-second sweep. [`RotationGate`] caches the cheap per-type aggregate
//! ([`rotation_status`]) with a short TTL, so a controller's per-object entry
//! guard ([`RotationGate::rotation_needed`]) hits the database per-device
//! only when the site-wide aggregate says some device actually lags. In steady
//! state (nothing staged, or fully converged) the gate is one cached aggregate
//! query per TTL window, not O(devices).

use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use carbide_redfish::libredfish::BmcCredentialOps;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, Credentials,
};
use chrono::{DateTime, Utc};
use db::DatabaseError;
use db::credential_rotation::{
    CredentialRotationType, DeviceRotationStatus, backoff_until, device_rotation_status,
    increment_rotate_attempt, mark_device_rotating_to_version, promote_rotating_to_current,
    rotation_status,
};
use libredfish::model::service_root::RedfishVendor;
use mac_address::MacAddress;
use model::bmc_info::BmcInfo;
use model::machine::Machine;
use model::power_shelf::PowerShelf;
use model::switch::Switch;
use sqlx::PgPool;

pub mod site_explorer_pause;

/// The `bmc` credential family (BMC root password), the bulk of this crate's
/// work.
const BMC: CredentialRotationType = CredentialRotationType::Bmc;

/// The `dpu_bmc_service` credential family (BF4 DPU BMC `service` account
/// password), converged by [`rotate_dpu_bmc_service`].
const DPU_BMC_SERVICE: CredentialRotationType = CredentialRotationType::DpuBmcService;

/// The persisted result of one BMC credential rotation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum BmcCredentialRotationResult {
    Converged,
    Recovered,
    Quarantined,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_bmc_credential_rotation_results_total",
    kind = counter,
    component = "credential-rotation",
    describe = "Number of persisted BMC credential rotation results, by result"
)]
struct BmcCredentialRotationResults {
    result: BmcCredentialRotationResult,
}

#[derive(Event)]
#[event(
    event_name = "bmc_credential_rotation_converged",
    metric_family = BmcCredentialRotationResults,
    log = info,
    message = "BMC credential rotated and converged"
)]
struct BmcCredentialRotationConverged {
    #[label]
    result: BmcCredentialRotationResult,
    #[context]
    mac: MacAddress,
    #[context(value)]
    target_version: i64,
}

impl BmcCredentialRotationConverged {
    fn new(mac: MacAddress, target_version: u32) -> Self {
        Self {
            result: BmcCredentialRotationResult::Converged,
            mac,
            target_version: i64::from(target_version),
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "bmc_credential_rotation_recovered",
    metric_family = BmcCredentialRotationResults,
    log = warn,
    message = "BMC already at rotate-to credential; recovered from an interrupted prior rotation"
)]
struct BmcCredentialRotationRecovered {
    #[label]
    result: BmcCredentialRotationResult,
    #[context]
    mac: MacAddress,
    #[context(value)]
    target_version: i64,
    #[context]
    change_error: String,
}

impl BmcCredentialRotationRecovered {
    fn new(mac: MacAddress, target_version: u32, change_error: String) -> Self {
        Self {
            result: BmcCredentialRotationResult::Recovered,
            mac,
            target_version: i64::from(target_version),
            change_error,
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "bmc_credential_rotation_quarantined",
    metric_family = BmcCredentialRotationResults,
    log = warn,
    message = "BMC credential rotation attempt failed; quarantining"
)]
struct BmcCredentialRotationQuarantined {
    #[label]
    result: BmcCredentialRotationResult,
    #[context]
    mac: MacAddress,
    #[context(value)]
    target_version: i64,
    #[context]
    error: String,
}

impl BmcCredentialRotationQuarantined {
    fn new(mac: MacAddress, target_version: u32, error: String) -> Self {
        Self {
            result: BmcCredentialRotationResult::Quarantined,
            mac,
            target_version: i64::from(target_version),
            error,
        }
    }
}

#[derive(MetricFamily)]
#[metric(
    name = "carbide_dpu_bmc_service_credential_rotation_results_total",
    kind = counter,
    component = "credential-rotation",
    describe = "Number of persisted DPU BMC service credential rotation results, by result"
)]
struct DpuBmcServiceCredentialRotationResults {
    result: BmcCredentialRotationResult,
}

#[derive(Event)]
#[event(
    event_name = "dpu_bmc_service_credential_rotation_converged",
    metric_family = DpuBmcServiceCredentialRotationResults,
    log = info,
    message = "DPU BMC service credential rotated and converged"
)]
struct DpuBmcServiceCredentialRotationConverged {
    #[label]
    result: BmcCredentialRotationResult,
    #[context]
    mac: MacAddress,
    #[context(value)]
    target_version: i64,
}

impl DpuBmcServiceCredentialRotationConverged {
    fn new(mac: MacAddress, target_version: u32) -> Self {
        Self {
            result: BmcCredentialRotationResult::Converged,
            mac,
            target_version: i64::from(target_version),
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "dpu_bmc_service_credential_rotation_recovered",
    metric_family = DpuBmcServiceCredentialRotationResults,
    log = warn,
    message = "DPU BMC service account already at rotate-to credential; recovered from an interrupted prior rotation"
)]
struct DpuBmcServiceCredentialRotationRecovered {
    #[label]
    result: BmcCredentialRotationResult,
    #[context]
    mac: MacAddress,
    #[context(value)]
    target_version: i64,
    #[context]
    change_error: String,
}

impl DpuBmcServiceCredentialRotationRecovered {
    fn new(mac: MacAddress, target_version: u32, change_error: String) -> Self {
        Self {
            result: BmcCredentialRotationResult::Recovered,
            mac,
            target_version: i64::from(target_version),
            change_error,
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "dpu_bmc_service_credential_rotation_quarantined",
    metric_family = DpuBmcServiceCredentialRotationResults,
    log = warn,
    message = "DPU BMC service credential rotation attempt failed; quarantining"
)]
struct DpuBmcServiceCredentialRotationQuarantined {
    #[label]
    result: BmcCredentialRotationResult,
    #[context]
    mac: MacAddress,
    #[context(value)]
    target_version: i64,
    #[context]
    error: String,
}

impl DpuBmcServiceCredentialRotationQuarantined {
    fn new(mac: MacAddress, target_version: u32, error: String) -> Self {
        Self {
            result: BmcCredentialRotationResult::Quarantined,
            mac,
            target_version: i64::from(target_version),
            error,
        }
    }
}

/// Default freshness window for the [`RotationGate`] aggregate cache. Short
/// enough that a freshly staged rotation is picked up within roughly one sweep,
/// long enough that steady-state sweeps don't hammer the aggregate query.
const DEFAULT_AGGREGATE_TTL: Duration = Duration::from_secs(15);

/// The result of attempting to converge one device toward the staged target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotateOutcome {
    /// The device is at the target version (just rotated, or already there).
    /// The controller transitions back to its steady (`Ready`) state.
    Converged,
    /// The attempt failed *before* the hardware password changed (or the change
    /// itself failed), so the stored per-device secret still authenticates the
    /// BMC. A backoff window was recorded. The controller returns to steady
    /// state and its entry guard skips this device (it reads as `quarantined`)
    /// until `until` passes.
    Quarantined { until: DateTime<Utc> },
    /// The BMC reached the rotate-to password but persisting the new per-device
    /// secret to the credential store failed: the hardware is AHEAD of the
    /// store (the BMC is on the new password while the store still serves the
    /// old one). The device is intentionally left **not quarantined and not
    /// converged**, so it keeps reading as `needs_rotation` and the engine's
    /// change-then-verify recovery re-persists and converges on a later tick.
    ///
    /// The controller must **remain in its rotation state** (keeping the device
    /// suppressed from site-explorer and out of the allocatable pool) and
    /// re-tick until the store write succeeds -- reconciliation is retried every
    /// sweep rather than backed off, and is never abandoned.
    CredentialStoreReconcilePending,
    /// Nothing to do: no rotation row for this device (not under management, or
    /// the row was torn down). The controller transitions back without acting.
    NoWork,
}

/// How the engine obtains the dispatch vendor `set_bmc_root_password` branches
/// on for a rotation target.
///
/// The precise Redfish dispatch vendor is not persisted for any device family
/// (the stored hardware vendor is DMI-derived and too coarse). The distinction
/// is instead about *who* resolves it: a caller that already holds the exact
/// vendor hands it over (`Fixed`), while a caller that would otherwise have to
/// probe an authenticated BMC to learn it defers that probe to the engine
/// (`Probe`), so the probe runs inside the engine's quarantine-on-failure
/// envelope.
#[derive(Debug, Clone)]
pub enum DispatchVendor {
    /// The caller already holds the precise vendor: the switch controller (a
    /// compile-time constant, NVIDIA MGX) and the machine controller (which
    /// probes it in its own controller layer before calling). Nothing persists
    /// it -- `Fixed` only means "resolved by the caller, not the engine".
    Fixed(RedfishVendor),
    /// Resolve the vendor at rotation time by probing the BMC's Chassis
    /// manufacturer ([`BmcCredentialOps::probe_bmc_vendor`]) -- power-shelf
    /// PMCs (Lite-On/Delta), which do *not* expose a recognized vendor in their
    /// Redfish service root. The probe runs *inside* the engine's
    /// quarantine-on-failure envelope and reuses the same credential candidates
    /// as the rotation itself, so a failed probe records backoff (lockout-safe)
    /// rather than looping the entry guard, and a stale per-device secret does
    /// not deadlock recovery.
    Probe,
}

/// Where and how to reach a single device's BMC for rotation.
///
/// `device_mac` is the BMC MAC that keys both the `device_credential_rotation`
/// row and the per-device Vault secret. `vendor` is either the caller-known
/// dispatch vendor or a directive to probe it at rotation time (see
/// [`DispatchVendor`]).
#[derive(Debug, Clone)]
pub struct BmcRotationTarget {
    /// BMC MAC keying the rotation row and the per-device secret.
    pub device_mac: MacAddress,
    /// BMC host (IP or hostname).
    pub host: String,
    /// BMC port, when non-default.
    pub port: Option<u16>,
    /// How to obtain the dispatch vendor `set_bmc_root_password` branches on.
    pub vendor: DispatchVendor,
}

/// A reachable, keyable BMC endpoint: the MAC that keys both the
/// `device_credential_rotation` row and the per-device Vault secret, plus where
/// to reach it. Vendor-independent -- it is the precursor a controller resolves
/// a dispatch vendor for (by probing at rotation time) to form a
/// [`BmcRotationTarget`]. Each device state controller builds these from its own
/// snapshot: the machine controller from the host BMC and each DPU BMC, the
/// switch controller from the switch's single BMC.
#[derive(Debug, Clone)]
pub struct BmcEndpoint {
    /// BMC MAC keying the rotation row and the per-device secret.
    pub device_mac: MacAddress,
    /// BMC host (IP or hostname).
    pub host: String,
    /// BMC port, when non-default.
    pub port: Option<u16>,
}

impl BmcEndpoint {
    /// Build an endpoint from a device's BMC info, or `None` when the BMC lacks
    /// a MAC or IP (unkeyable / unreachable, so there is nothing to rotate).
    fn from_bmc_info(info: &BmcInfo) -> Option<Self> {
        Some(Self {
            device_mac: info.mac?,
            host: info.ip?.to_string(),
            port: info.port,
        })
    }

    /// The BMC endpoint of a machine (a managed host or one of its DPUs), or
    /// `None` when that machine's BMC is unkeyable / unreachable.
    pub fn from_machine(machine: &Machine) -> Option<Self> {
        Self::from_bmc_info(&machine.status.bmc_info)
    }

    /// The BMC endpoint of a switch, or `None` when the switch has no BMC info
    /// or it is unkeyable / unreachable.
    pub fn from_switch(switch: &Switch) -> Option<Self> {
        Self::from_bmc_info(switch.bmc_info.as_ref()?)
    }

    /// The BMC (PMC) endpoint of a power shelf, or `None` when the power shelf
    /// has no BMC info or it is unkeyable / unreachable.
    pub fn from_power_shelf(power_shelf: &PowerShelf) -> Option<Self> {
        Self::from_bmc_info(power_shelf.bmc_info.as_ref()?)
    }

    /// Pair this endpoint with a caller-known dispatch `vendor` to form the
    /// [`BmcRotationTarget`] the engine rotates (machine + switch controllers).
    pub fn into_target(self, vendor: RedfishVendor) -> BmcRotationTarget {
        self.into_target_with(DispatchVendor::Fixed(vendor))
    }

    /// Pair this endpoint with a directive to probe the dispatch vendor at
    /// rotation time (power-shelf controller: Lite-On/Delta PMCs).
    pub fn into_target_probing_vendor(self) -> BmcRotationTarget {
        self.into_target_with(DispatchVendor::Probe)
    }

    fn into_target_with(self, vendor: DispatchVendor) -> BmcRotationTarget {
        BmcRotationTarget {
            device_mac: self.device_mac,
            host: self.host,
            port: self.port,
            vendor,
        }
    }
}

#[cfg(test)]
mod bmc_endpoint_tests {
    use std::net::IpAddr;

    use super::*;

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0, 0, 0, 0, last])
    }

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    fn bmc_info(mac: Option<MacAddress>, ip: Option<IpAddr>, port: Option<u16>) -> BmcInfo {
        BmcInfo {
            machine_interface_id: None,
            ip,
            port,
            mac,
            version: None,
            firmware_version: None,
        }
    }

    #[test]
    fn endpoint_resolves_from_bmc_info() {
        let endpoint = BmcEndpoint::from_bmc_info(&bmc_info(Some(mac(1)), Some(ip(1)), Some(8443)))
            .expect("a fully addressable BMC yields an endpoint");
        assert_eq!(endpoint.device_mac, mac(1));
        assert_eq!(endpoint.host, ip(1).to_string());
        assert_eq!(endpoint.port, Some(8443));
    }

    #[test]
    fn endpoint_is_none_when_mac_missing() {
        // No MAC means the rotation row / per-device secret cannot be keyed.
        assert!(BmcEndpoint::from_bmc_info(&bmc_info(None, Some(ip(1)), None)).is_none());
    }

    #[test]
    fn endpoint_is_none_when_ip_missing() {
        // No IP means the BMC cannot be reached.
        assert!(BmcEndpoint::from_bmc_info(&bmc_info(Some(mac(1)), None, None)).is_none());
    }
}

/// Transient-failure retry budget for a single BMC-rotation entry. Device-level
/// failures are handled by the engine (quarantine + backoff), so this only
/// bounds re-entries caused by transient *bookkeeping* errors before a
/// controller gives up and returns to its steady state; the entry guard
/// re-enters on the next sweep if a device still lags.
pub const MAX_BMC_ROTATION_RETRIES: u32 = 3;

/// Outcome of one BMC-rotation tick, as seen by a device state controller.
///
/// The tick may cover one device (a switch or power-shelf BMC) or several (a
/// managed host's host BMC plus each DPU BMC); either way it collapses to
/// whether every device reached a terminal engine outcome or at least one hit a
/// transient bookkeeping failure worth retrying.
pub enum BmcRotationTick {
    /// Every device reached a terminal outcome (converged, quarantined, or no
    /// work). The controller should leave the rotation state; the entry guard
    /// re-enters later if a quarantined device becomes eligible again.
    Settled,
    /// At least one device hit a transient bookkeeping failure. The controller
    /// should retry the tick, bounded by the state's retry budget.
    Retry,
    /// At least one device changed its hardware password but the credential
    /// store persist has not yet succeeded (hardware ahead of the stored
    /// secret). The controller must **remain** in its rotation state -- keeping
    /// the device suppressed and out of the allocatable pool -- and wait; a
    /// later tick's change-then-verify recovery re-persists and converges once
    /// the store write succeeds. Dominates `Retry` and `Settled` (see
    /// [`BmcRotationTick::merge`]) so a mixed multi-device tick never leaves the
    /// rotation state while any device's hardware is ahead of its store.
    WaitForCredentialStoreReconcile,
}

impl BmcRotationTick {
    /// Fold two device outcomes into one tick outcome, strongest wins:
    /// `WaitForCredentialStoreReconcile` > `Retry` > `Settled`. Holding for a lagging store
    /// dominates a transient retry so the bounded transient budget can never
    /// `GaveUp` out of the rotation state (returning the device to an
    /// allocatable, un-suppressed steady state) while a device's hardware is
    /// ahead of its stored secret.
    pub fn merge(self, other: Self) -> Self {
        use BmcRotationTick::*;
        match (self, other) {
            (WaitForCredentialStoreReconcile, _) | (_, WaitForCredentialStoreReconcile) => {
                WaitForCredentialStoreReconcile
            }
            (Retry, _) | (_, Retry) => Retry,
            (Settled, Settled) => Settled,
        }
    }
}

/// What a state controller should do after one BMC-rotation tick, independent of
/// which controller or state drives it. Keeping this state-neutral lets the
/// machine (`ManagedHostState::RotatingBmc`), switch
/// (`SwitchControllerState::RotatingBmc`), and (later) power-shelf controllers
/// share one retry/budget policy and remain thin maps onto their own state
/// constructors.
pub enum RotationStep {
    /// The tick reached a terminal outcome for every device (converged,
    /// quarantined, or no work): leave the rotation state, and it is safe to
    /// clear any one-shot force request because the forced attempt genuinely
    /// fired. The entry guard re-enters on a later sweep if a device lags again.
    Settled,
    /// The transient-retry budget was exhausted without settling. Leave the
    /// rotation state, but do *not* treat a force request as satisfied: the
    /// forced attempt never cleanly ran (the failures were pre-hardware
    /// bookkeeping errors that record no quarantine), so leaving the flag set
    /// lets the entry guard re-attempt on a later sweep instead of silently
    /// dropping the operator's request.
    GaveUp,
    /// Re-enter the rotation state carrying this incremented retry count.
    Retry { retry_count: u32 },
    /// A device's hardware is ahead of its stored secret (the credential-store
    /// persist is still pending): **remain** in the rotation state and wait for
    /// a later tick to reconcile the store. Unlike `Retry` this does *not*
    /// consume the transient-retry budget -- it is a legitimate hold, not a
    /// bookkeeping error -- and unlike `Settled`/`GaveUp` it must not resume
    /// site-explorer or clear a force request, so the device stays suppressed
    /// and non-allocatable until the store write succeeds.
    WaitForCredentialStoreReconcile,
}

/// Fold one tick outcome and the current retry count into the next
/// [`RotationStep`]. Device-level failures are already handled by the engine
/// (quarantine + backoff); this only bounds re-entries caused by *transient
/// bookkeeping* failures before a controller falls back to its steady state.
///
/// `object_id` is used only for the give-up log line, so any controller passes
/// its own identifier (a machine, switch, or power-shelf id).
pub fn advance(
    tick: BmcRotationTick,
    retry_count: u32,
    object_id: impl std::fmt::Display,
) -> RotationStep {
    match tick {
        BmcRotationTick::Settled => RotationStep::Settled,
        // A store-reconcile hold is budget-neutral: it is a legitimate wait for
        // the credential store, not a transient bookkeeping error, so it neither
        // advances nor exhausts the retry count.
        BmcRotationTick::WaitForCredentialStoreReconcile => {
            RotationStep::WaitForCredentialStoreReconcile
        }
        BmcRotationTick::Retry => {
            let next = retry_count + 1;
            if next >= MAX_BMC_ROTATION_RETRIES {
                tracing::warn!(
                    %object_id,
                    "BMC rotation exhausted its transient-retry budget; returning to steady state (a pending force request stays set so the entry guard re-attempts on a later sweep; a passively-lagging device is re-selected by the gate)"
                );
                RotationStep::GaveUp
            } else {
                RotationStep::Retry { retry_count: next }
            }
        }
    }
}

#[cfg(test)]
mod retry_seam_tests {
    use super::{BmcRotationTick, MAX_BMC_ROTATION_RETRIES, RotationStep, advance};

    /// Any Display value stands in for a real object id; the seam uses it only
    /// for the give-up log line.
    const OBJECT_ID: &str = "test-object";

    #[test]
    fn advance_settles_regardless_of_retry_count() {
        // A settled tick always leaves the rotation state, even mid-budget: every
        // device reached a terminal outcome, so there is nothing left to retry.
        // `Settled` (not `GaveUp`) is what authorizes the caller to clear a
        // satisfied force request.
        assert!(matches!(
            advance(BmcRotationTick::Settled, 0, OBJECT_ID),
            RotationStep::Settled
        ));
        assert!(matches!(
            advance(
                BmcRotationTick::Settled,
                MAX_BMC_ROTATION_RETRIES - 1,
                OBJECT_ID
            ),
            RotationStep::Settled
        ));
    }

    #[test]
    fn advance_retries_with_incremented_count_within_budget() {
        // A transient failure below budget re-enters carrying count+1, so the
        // budget actually advances toward its bound rather than looping forever.
        let RotationStep::Retry { retry_count } = advance(BmcRotationTick::Retry, 0, OBJECT_ID)
        else {
            panic!("a transient failure below budget must retry");
        };
        assert_eq!(retry_count, 1);
    }

    #[test]
    fn advance_gives_up_at_budget() {
        // The last attempt before the bound (count+1 == MAX) stops retrying and
        // falls back to the steady state instead of exceeding the budget. It
        // reports `GaveUp` rather than `Settled` so the caller leaves a pending
        // force request in place (the forced attempt never cleanly ran) instead
        // of silently clearing it.
        assert!(matches!(
            advance(
                BmcRotationTick::Retry,
                MAX_BMC_ROTATION_RETRIES - 1,
                OBJECT_ID
            ),
            RotationStep::GaveUp
        ));
    }

    #[test]
    fn advance_holds_for_store_reconcile_without_consuming_budget() {
        // A store-reconcile hold is budget-neutral: it maps straight to
        // `WaitForCredentialStoreReconcile` regardless of retry count, so waiting for the
        // credential store never exhausts the transient-retry budget and can
        // never `GaveUp` out of the rotation state (which would return the device
        // to an allocatable, un-suppressed steady state while its hardware is
        // ahead of the store).
        for count in [0, MAX_BMC_ROTATION_RETRIES - 1, MAX_BMC_ROTATION_RETRIES] {
            assert!(matches!(
                advance(
                    BmcRotationTick::WaitForCredentialStoreReconcile,
                    count,
                    OBJECT_ID
                ),
                RotationStep::WaitForCredentialStoreReconcile
            ));
        }
    }

    #[test]
    fn merge_folds_device_outcomes_strongest_wins() {
        use BmcRotationTick::*;
        // WaitForCredentialStoreReconcile dominates everything so a mixed multi-device tick
        // holds; Retry dominates Settled; Settled only survives when both are.
        assert!(matches!(Settled.merge(Settled), Settled));
        assert!(matches!(Settled.merge(Retry), Retry));
        assert!(matches!(Retry.merge(Settled), Retry));
        assert!(matches!(Retry.merge(Retry), Retry));
        assert!(matches!(
            Settled.merge(WaitForCredentialStoreReconcile),
            WaitForCredentialStoreReconcile
        ));
        assert!(matches!(
            WaitForCredentialStoreReconcile.merge(Settled),
            WaitForCredentialStoreReconcile
        ));
        assert!(matches!(
            Retry.merge(WaitForCredentialStoreReconcile),
            WaitForCredentialStoreReconcile
        ));
        assert!(matches!(
            WaitForCredentialStoreReconcile.merge(Retry),
            WaitForCredentialStoreReconcile
        ));
        assert!(matches!(
            WaitForCredentialStoreReconcile.merge(WaitForCredentialStoreReconcile),
            WaitForCredentialStoreReconcile
        ));
    }
}

/// Errors that abort a rotation tick as a transient handler failure (so the
/// controller retries the whole tick), as opposed to a device-level failure
/// (which quarantines the device and is reported via
/// [`RotateOutcome::Quarantined`]).
#[derive(thiserror::Error, Debug)]
pub enum RotationEngineError {
    /// Rotation bookkeeping (the `device_credential_rotation` table) failed.
    #[error("rotation bookkeeping error: {0}")]
    Database(#[from] DatabaseError),
    /// A database connection could not be acquired from the pool.
    #[error("could not acquire a database connection: {0}")]
    Acquire(#[from] sqlx::Error),
    /// The site-wide target version is not representable as an unsigned version
    /// (a corrupted bookkeeping invariant rather than a device fault).
    #[error("rotation target version {0} is not representable")]
    BadTargetVersion(i32),
}

/// Whether `status` describes a device that still needs to converge to the
/// current site-wide target: behind the target and not currently quarantined.
///
/// A pure predicate over a status a caller already holds. A device with no
/// rotation row never reaches here (the caller resolves the row first); a
/// converged or quarantined device is left alone.
pub fn needs_rotation(status: &DeviceRotationStatus) -> bool {
    !status.converged && !status.quarantined
}

/// A short-TTL cache of the site-wide rotation aggregate for one credential
/// family (BMC, host UEFI, ...), shared across a controller's per-object ticks
/// (cheap to clone; `Arc`-backed). The family is fixed at construction; a
/// controller that rotates two families holds one gate per family.
///
/// The controller's per-tick entry guard calls [`Self::rotation_needed`], which
/// consults the cached aggregate first and
/// only issues the per-device query when the site-wide counts say some device
/// actually lags. This keeps the steady state (nothing staged / fully
/// converged) at one cheap aggregate query per TTL window rather than a
/// per-device query for every device on every sweep.
///
/// The cache is per-process: each controller replica maintains its own, which
/// is correct because it is only a gate -- the authoritative per-device check
/// and the `FOR UPDATE SKIP LOCKED` object claim still serialize the actual
/// rotation across replicas.
#[derive(Clone)]
pub struct RotationGate {
    inner: Arc<Mutex<CachedAggregate>>,
    ttl: Duration,
    /// The credential family this gate reports on (BMC, host UEFI, ...). A gate
    /// is single-family: the cached aggregate and the per-device query are both
    /// scoped to it, so a controller that rotates two families holds one gate
    /// per family.
    family: CredentialRotationType,
}

#[derive(Default)]
struct CachedAggregate {
    /// When the aggregate was last refreshed; `None` before the first query.
    checked_at: Option<Instant>,
    /// Whether the last refresh saw any device lagging the site-wide target.
    work_pending: bool,
}

impl RotationGate {
    /// A gate for a credential family with the default aggregate-cache TTL. The
    /// family is required: the type is family-generic and its cached aggregate
    /// and per-device query are both scoped to it, so callers always name the
    /// family they gate on rather than relying on an implicit default.
    pub fn new_for_family(family: CredentialRotationType) -> Self {
        Self::with_ttl_and_family(DEFAULT_AGGREGATE_TTL, family)
    }

    /// A gate for a credential family with an explicit aggregate-cache TTL. A
    /// zero TTL disables caching (every call re-queries the aggregate) -- useful
    /// in tests.
    pub fn with_ttl_and_family(ttl: Duration, family: CredentialRotationType) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CachedAggregate::default())),
            ttl,
            family,
        }
    }

    /// Whether *any* BMC device currently lags the site-wide target, from the
    /// cached aggregate (refreshed at most once per TTL window).
    ///
    /// "Work pending" means the target has advanced past the v0 baseline and at
    /// least one device is pending or quarantined. The steady state -- nothing
    /// staged (`target_version == 0`) or everything converged -- returns
    /// `false` from cache without a per-device query.
    pub async fn rotation_pending(&self, pool: &PgPool) -> Result<bool, RotationEngineError> {
        // Fast path: a still-fresh cached decision, without touching the DB.
        // The guard is dropped before any await so we never hold a std mutex
        // across a suspension point.
        {
            let cache = self.inner.lock().expect("rotation gate mutex poisoned");
            if let Some(checked_at) = cache.checked_at
                && checked_at.elapsed() < self.ttl
            {
                return Ok(cache.work_pending);
            }
        }

        // Refresh with one cheap aggregate query. A concurrent refresh by
        // another tick is harmless: the query is read-only and idempotent, and
        // last-writer-wins on the cache is fine for a gate.
        let mut conn = pool.acquire().await?;
        let status = rotation_status(&mut conn, self.family).await?;
        drop(conn);
        let work_pending = status.target_version > 0 && (status.pending + status.quarantined) > 0;

        let mut cache = self.inner.lock().expect("rotation gate mutex poisoned");
        cache.checked_at = Some(Instant::now());
        cache.work_pending = work_pending;
        Ok(work_pending)
    }

    /// Controller entry guard for one device: `true` when the device's BMC
    /// credential is behind the site-wide target and not quarantined, so the
    /// controller should enter its BMC-rotation state.
    ///
    /// Gated by [`Self::rotation_pending`], so the per-device query runs only
    /// when the cached aggregate says some device lags. A device with no
    /// rotation row (not under management) returns `false`.
    pub async fn rotation_needed(
        &self,
        pool: &PgPool,
        device_mac: MacAddress,
    ) -> Result<bool, RotationEngineError> {
        if !self.rotation_pending(pool).await? {
            return Ok(false);
        }
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, self.family, device_mac).await?;
        Ok(status.as_ref().is_some_and(needs_rotation))
    }
}

/// The shared entry gate every per-device rotation family runs identically:
/// read the device's rotation status, apply the `NoWork`/`Converged`/
/// `Quarantined` gates, validate the target version, and stage the crash-safety
/// marker before any hardware is touched. Family-specific convergence and result
/// bookkeeping stay in each `rotate_*` entry point.
///
/// [`ControlFlow::Break`] carries an already-resolved [`RotateOutcome`] the caller
/// returns directly; [`ControlFlow::Continue`] carries the `(status, target_version)`
/// the caller needs to converge and, on failure, to back off.
///
/// Re-marking to the live target every tick supersedes a stale in-flight marker.
/// A target that cannot be represented as an unsigned version is a corrupted
/// invariant, not a device fault, so it aborts the tick with
/// [`RotationEngineError::BadTargetVersion`] rather than quarantining. `force`
/// attempts a device still inside its backoff window; it does not bypass
/// `NoWork` or `Converged`.
async fn enter_device_rotation(
    db_pool: &PgPool,
    credential_type: CredentialRotationType,
    mac: MacAddress,
    force: bool,
) -> Result<ControlFlow<RotateOutcome, (DeviceRotationStatus, u32)>, RotationEngineError> {
    let mut conn = db_pool.acquire().await?;
    let status = device_rotation_status(&mut conn, credential_type, mac).await?;
    drop(conn);

    let Some(status) = status else {
        return Ok(ControlFlow::Break(RotateOutcome::NoWork));
    };
    if status.converged {
        return Ok(ControlFlow::Break(RotateOutcome::Converged));
    }
    if status.quarantined && !force {
        return Ok(ControlFlow::Break(RotateOutcome::Quarantined {
            until: status.quarantined_until.unwrap_or_else(Utc::now),
        }));
    }
    let target_version = u32::try_from(status.target_version)
        .map_err(|_| RotationEngineError::BadTargetVersion(status.target_version))?;

    let mut conn = db_pool.acquire().await?;
    mark_device_rotating_to_version(&mut conn, mac, credential_type, status.target_version).await?;
    drop(conn);

    Ok(ControlFlow::Continue((status, target_version)))
}

/// Converge one device's BMC root password to the staged site-wide target.
///
/// Idempotent and crash-safe: safe to call every tick while the controller is
/// in its BMC-rotation state. Returns [`RotateOutcome::Converged`] once the
/// device is at the target (so the controller can leave the state),
/// [`RotateOutcome::Quarantined`] when an attempt failed (backoff recorded), or
/// [`RotateOutcome::NoWork`] when there is no rotation row.
///
/// `credential_manager` both *reads* the per-device and site-wide secrets and
/// *writes* the per-device secret on success -- the same store
/// `RotateCredential` staged the target into. (The Redfish pool's
/// `credential_reader` is intentionally not used here; the engine owns
/// credential resolution.)
/// `force` is the operator escape hatch: when `true`, a device still inside its
/// backoff window is attempted anyway rather than short-circuiting as
/// [`RotateOutcome::Quarantined`]. It does not bypass [`RotateOutcome::NoWork`]
/// (no rotation row) or [`RotateOutcome::Converged`] (already at target) -- there
/// is nothing to force in those cases -- and a forced attempt that fails
/// re-quarantines through the normal backoff bookkeeping.
///
/// `redfish_pool` is the direct pool's credential-operations handle
/// ([`BmcCredentialOps`]): rotation authenticates to the BMC itself to
/// change and verify passwords.
pub async fn rotate_bmc(
    db_pool: &PgPool,
    credential_manager: &dyn CredentialManager,
    redfish_pool: &dyn BmcCredentialOps,
    bmc: &BmcRotationTarget,
    force: bool,
) -> Result<RotateOutcome, RotationEngineError> {
    let mac = bmc.device_mac;
    let (status, target_version) = match enter_device_rotation(db_pool, BMC, mac, force).await? {
        ControlFlow::Break(outcome) => return Ok(outcome),
        ControlFlow::Continue(proceed) => proceed,
    };

    match converge_bmc_password(credential_manager, redfish_pool, bmc, target_version).await {
        Ok(convergence) => {
            let mut conn = db_pool.acquire().await?;
            // Flush cached BMC sessions so the next login re-authenticates with
            // the freshly-written credential rather than a now-stale token.
            db::bmc_redfish_session::delete_by_mac(&mut conn, mac).await?;
            promote_rotating_to_current(&mut conn, mac, BMC).await?;
            match convergence {
                CredentialConvergence::Changed => {
                    emit(BmcCredentialRotationConverged::new(mac, target_version));
                }
                // The change failed but the rotate-TO value already
                // authenticated: a prior attempt changed the hardware and
                // crashed before recording success, and this tick reconciled it.
                // WARN because reaching here means an earlier attempt was
                // interrupted mid-rotation; the redacted change error explains
                // why the direct change failed (usually a stale-credential auth
                // rejection).
                CredentialConvergence::Recovered { change_error } => {
                    emit(BmcCredentialRotationRecovered::new(
                        mac,
                        target_version,
                        change_error,
                    ));
                }
            }
            Ok(RotateOutcome::Converged)
        }
        Err(ConvergeError::DeviceFault(redacted)) => {
            let until = backoff_until(status.rotate_attempts, Utc::now());
            let mut conn = db_pool.acquire().await?;
            increment_rotate_attempt(&mut conn, mac, BMC, &redacted, until).await?;
            emit(BmcCredentialRotationQuarantined::new(
                mac,
                target_version,
                redacted,
            ));
            Ok(RotateOutcome::Quarantined { until })
        }
        // The hardware reached the target but the store write failed: the BMC is
        // ahead of the stored secret. Deliberately record *no* backoff and leave
        // the row not-converged / not-quarantined, so it keeps reading as
        // `needs_rotation` and the change-then-verify recovery re-persists on a
        // later tick. The crash marker stays set (we never promoted). The
        // controller holds in its rotation state until the store reconciles, so
        // reconciliation is retried every sweep and never abandoned.
        Err(ConvergeError::CredentialStoreWriteFailed(redacted)) => {
            // A transient hold, not a terminal result: a plain log rather than a
            // metric series. The device keeps reading as `needs_rotation`, so the
            // retry is observable through the existing rotation-state gauges; a
            // dedicated counter can follow if operators need to trend it.
            tracing::warn!(
                %mac,
                target_version,
                error = %redacted,
                "BMC reached the rotate-to password but persisting the new per-device secret failed; holding rotation until the credential store reconciles"
            );
            Ok(RotateOutcome::CredentialStoreReconcilePending)
        }
    }
}

/// Converge one BF4 DPU BMC's `service` account password to the staged
/// site-wide target.
///
/// `redfish_pool` is the direct pool's credential-operations handle
/// ([`BmcCredentialOps`]): rotation authenticates to the BMC itself to
/// change and verify passwords.
pub async fn rotate_dpu_bmc_service(
    db_pool: &PgPool,
    credential_manager: &dyn CredentialManager,
    redfish_pool: &dyn BmcCredentialOps,
    endpoint: &BmcEndpoint,
    force: bool,
) -> Result<RotateOutcome, RotationEngineError> {
    let mac = endpoint.device_mac;
    let (status, target_version) =
        match enter_device_rotation(db_pool, DPU_BMC_SERVICE, mac, force).await? {
            ControlFlow::Break(outcome) => return Ok(outcome),
            ControlFlow::Continue(proceed) => proceed,
        };

    match converge_dpu_bmc_service_password(
        credential_manager,
        redfish_pool,
        endpoint,
        target_version,
    )
    .await
    {
        Ok(convergence) => {
            let mut conn = db_pool.acquire().await?;
            promote_rotating_to_current(&mut conn, mac, DPU_BMC_SERVICE).await?;
            match convergence {
                CredentialConvergence::Changed => {
                    emit(DpuBmcServiceCredentialRotationConverged::new(
                        mac,
                        target_version,
                    ));
                }
                // The re-applied change was rejected but the rotate-to value
                // already authenticated: a prior tick changed the hardware and
                // crashed before recording success, and this tick reconciled it.
                CredentialConvergence::Recovered { change_error } => {
                    emit(DpuBmcServiceCredentialRotationRecovered::new(
                        mac,
                        target_version,
                        change_error,
                    ));
                }
            }
            Ok(RotateOutcome::Converged)
        }
        Err(redacted) => {
            let until = backoff_until(status.rotate_attempts, Utc::now());
            let mut conn = db_pool.acquire().await?;
            increment_rotate_attempt(&mut conn, mac, DPU_BMC_SERVICE, &redacted, until).await?;
            emit(DpuBmcServiceCredentialRotationQuarantined::new(
                mac,
                target_version,
                redacted,
            ));
            Ok(RotateOutcome::Quarantined { until })
        }
    }
}

/// Read the auth (per-device BMC root) and target (site-wide `service` at
/// `rotate_to_version`) secrets and PATCH the `service` account.
///
/// Returns [`CredentialConvergence::Changed`] when the PATCH succeeded. If it
/// fails, probe whether the `service` account already authenticates with the
/// rotate-to password: a crash-resume tick re-applying the target to a BMC that
/// rejects same-value changes lands here already converged, reported as
/// [`CredentialConvergence::Recovered`] carrying the redacted change error rather
/// than quarantining. Returns `Err` with an already-redacted, secret-free reason
/// on a genuine device-level failure so the caller can quarantine. No per-device
/// secret is written: the site-wide versioned secret is the only copy.
async fn converge_dpu_bmc_service_password(
    credential_manager: &dyn CredentialManager,
    redfish_pool: &dyn BmcCredentialOps,
    endpoint: &BmcEndpoint,
    rotate_to_version: u32,
) -> Result<CredentialConvergence, String> {
    let root_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: endpoint.device_mac,
        },
    };
    let root = credential_manager
        .get_credentials(&root_key)
        .await
        .map_err(|e| format!("read per-device BMC root secret: {e}"))?
        .ok_or_else(|| "per-device BMC root secret is not set".to_string())?;

    let rotate_to_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::site_wide_dpu_bmc_service(rotate_to_version),
    };
    let rotate_to = credential_manager
        .get_credentials(&rotate_to_key)
        .await
        .map_err(|e| format!("read rotate-to DPU service secret: {e}"))?
        .ok_or_else(|| {
            format!("rotate-to DPU service secret for version {rotate_to_version} is not staged")
        })?;

    let Credentials::UsernamePassword {
        password: root_password,
        ..
    } = &root;
    let Credentials::UsernamePassword {
        password: new_password,
        ..
    } = &rotate_to;
    let root_password = root_password.clone();
    let new_password = new_password.clone();

    if let Err(patch_err) = redfish_pool
        .set_bf4_dpu_service_password(
            &endpoint.host,
            endpoint.port,
            root.clone(),
            new_password.clone(),
        )
        .await
    {
        // A failed PATCH is ambiguous. A crash after a prior tick changed the
        // hardware but before it recorded the rotation leaves the `service`
        // account already at the rotate-to password; a BMC that enforces a
        // password-reuse policy then rejects re-applying that same value, even
        // though the device is already converged. Authenticate as `service` with
        // the rotate-to password to tell that benign rejection from a real
        // failure: if it authenticates, the hardware already carries the target
        // and we converge; otherwise the failure stands and the caller
        // quarantines. A probe that cannot confirm (transport error) is treated
        // as "not converged" so the original failure is retried after backoff.
        let already_at_target = redfish_pool
            .bf4_dpu_service_credentials_valid(&endpoint.host, endpoint.port, new_password.clone())
            .await
            .unwrap_or(false);
        let change_error = redact(patch_err.to_string(), &[&root_password, &new_password]);
        if !already_at_target {
            return Err(change_error);
        }
        return Ok(CredentialConvergence::Recovered { change_error });
    }

    Ok(CredentialConvergence::Changed)
}

/// How a device reached the rotate-TO credential, so the caller can tell a
/// routine rotation from a crash-recovery reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CredentialConvergence {
    /// The password change succeeded on this attempt.
    Changed,
    /// The change failed but the rotate-TO value already authenticated: a prior
    /// attempt changed the hardware and crashed before recording success, and
    /// this tick reconciled it. Carries the (redacted) change error for context.
    Recovered { change_error: String },
}

/// Where a convergence attempt failed, so the caller can tell a *device* fault
/// (quarantine + backoff; the stored secret still authenticates the BMC) from a
/// credential-store *persist* failure that lands after the hardware already
/// changed (hardware AHEAD of the store; hold and reconcile rather than
/// quarantine). Both carry an already-redacted reason.
#[derive(Debug)]
enum ConvergeError {
    /// The attempt failed before (or at) the hardware change: the stored
    /// per-device secret still authenticates the BMC, so the device is safe to
    /// return to steady state after a backoff.
    DeviceFault(String),
    /// The hardware reached the rotate-to password but persisting the new
    /// per-device secret to the credential store failed: the BMC is on the new
    /// password while the store still serves the old one.
    CredentialStoreWriteFailed(String),
}

/// Every pre-persist failure in [`converge_bmc_password`] is a device fault: the
/// hardware has not (confirmed) moved, so the stored secret is still
/// authoritative. Only the final store write is special-cased to
/// [`ConvergeError::CredentialStoreWriteFailed`] explicitly.
impl From<String> for ConvergeError {
    fn from(reason: String) -> Self {
        Self::DeviceFault(reason)
    }
}

impl CredentialConvergence {
    /// Redact every secret from the recovery context error, so the swallowed
    /// change failure can be surfaced without leaking a password.
    fn redacted(self, secrets: &[&str]) -> Self {
        match self {
            Self::Changed => Self::Changed,
            Self::Recovered { change_error } => Self::Recovered {
                change_error: redact(change_error, secrets),
            },
        }
    }
}

/// Resolve the credentials, converge the device to the rotate-to password
/// (change-then-verify recovery via [`change_or_recover`]), and persist the
/// per-device secret. Returns `Err` with an already-redacted reason on any
/// device-level failure, so the caller can record it and quarantine. Never
/// returns a secret-bearing string.
async fn converge_bmc_password(
    credential_manager: &dyn CredentialManager,
    redfish_pool: &dyn BmcCredentialOps,
    bmc: &BmcRotationTarget,
    rotate_to_version: u32,
) -> Result<CredentialConvergence, ConvergeError> {
    let per_device_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: bmc.device_mac,
        },
    };
    let per_device = credential_manager
        .get_credentials(&per_device_key)
        .await
        .map_err(|e| format!("read per-device BMC secret: {e}"))?
        .ok_or_else(|| "per-device BMC secret is not set".to_string())?;

    let rotate_to_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::site_wide_root(rotate_to_version),
    };
    let rotate_to = credential_manager
        .get_credentials(&rotate_to_key)
        .await
        .map_err(|e| format!("read rotate-to BMC secret: {e}"))?
        .ok_or_else(|| {
            format!("rotate-to BMC secret for version {rotate_to_version} is not staged")
        })?;

    let Credentials::UsernamePassword {
        username,
        password: current_password,
    } = per_device;
    let Credentials::UsernamePassword {
        password: new_password,
        ..
    } = rotate_to;

    let rotate_from = Credentials::UsernamePassword {
        username: username.clone(),
        password: current_password.clone(),
    };
    let rotate_to = Credentials::UsernamePassword {
        username: username.clone(),
        password: new_password.clone(),
    };

    // Resolve the dispatch vendor before touching the password. For a probe
    // target this is an authenticated Chassis read, so it must go through the
    // same redaction and (via the caller) quarantine-on-failure path as the
    // rotation itself.
    let vendor = resolve_dispatch_vendor(redfish_pool, bmc, &rotate_from, &rotate_to)
        .await
        .map_err(|e| redact(e, &[&current_password, &new_password]))?;

    let convergence = change_or_recover(redfish_pool, bmc, vendor, rotate_from, rotate_to)
        .await
        .map_err(|e| redact(e, &[&current_password, &new_password]))?
        .redacted(&[&current_password, &new_password]);

    // Persist the per-device secret at the new password so future logins -- and
    // the next rotation's "current" -- use it. The username is unchanged.
    let updated = Credentials::UsernamePassword {
        username,
        password: new_password.clone(),
    };
    credential_manager
        .set_credentials(&per_device_key, &updated)
        .await
        .map_err(|e| {
            // This is the one failure that leaves the hardware ahead of the
            // store: the password change already succeeded above, so a failed
            // persist means the BMC is on the new password while the store still
            // serves the old one. Surface it distinctly so the caller holds and
            // reconciles instead of quarantining.
            ConvergeError::CredentialStoreWriteFailed(redact(
                format!("persist per-device BMC secret: {e}"),
                &[&current_password, &new_password],
            ))
        })?;

    Ok(convergence)
}

/// Converge the hardware to `rotate_to` with change-then-verify recovery.
///
/// The normal path authenticates with `rotate_from` (the current per-device
/// secret) and changes the password to `rotate_to`. When that fails, the
/// hardware may already be at `rotate_to` because a prior attempt changed it and
/// crashed before recording success; [`BmcCredentialOps::bmc_credentials_valid`]
/// confirms that without re-issuing a same-value (`new -> new`) change some BMCs
/// reject. Only when the rotate-TO value does *not* already authenticate is the
/// change treated as a genuine failure.
///
/// Returns an already-`to_string`-ed error (still to be redacted by the caller)
/// on a genuine device-level failure. Bounded to at most one failed login on the
/// recovery path, so it cannot trip BMC lockout.
async fn change_or_recover(
    redfish_pool: &dyn BmcCredentialOps,
    bmc: &BmcRotationTarget,
    vendor: RedfishVendor,
    rotate_from: Credentials,
    rotate_to: Credentials,
) -> Result<CredentialConvergence, String> {
    let Credentials::UsernamePassword {
        password: new_password,
        ..
    } = &rotate_to;

    let change_err = match redfish_pool
        .set_bmc_root_password(
            &bmc.host,
            bmc.port,
            vendor,
            rotate_from,
            new_password.clone(),
        )
        .await
    {
        Ok(()) => return Ok(CredentialConvergence::Changed),
        Err(e) => e.to_string(),
    };

    // The change failed. If the rotate-TO value already authenticates, a prior
    // attempt changed the hardware before crashing and the device is at target;
    // converge without a same-value change. Otherwise surface the change error.
    match redfish_pool
        .bmc_credentials_valid(&bmc.host, bmc.port, rotate_to)
        .await
    {
        Ok(true) => Ok(CredentialConvergence::Recovered {
            change_error: change_err,
        }),
        Ok(false) => Err(change_err),
        Err(verify_err) => Err(format!(
            "{change_err}; rotate-to credential probe also failed: {verify_err}"
        )),
    }
}

/// Resolve the dispatch vendor `set_bmc_root_password` branches on.
///
/// A [`DispatchVendor::Fixed`] target returns immediately. A
/// [`DispatchVendor::Probe`] target (power-shelf PMCs) probes the BMC's Chassis
/// manufacturer, trying the per-device secret first and the rotate-TO value
/// second -- the same two candidates the change path uses. That ordering means
/// a crash-recovered device (whose per-device secret still lags the hardware)
/// still resolves its vendor via the rotate-TO value rather than deadlocking,
/// and it is bounded to at most two logins so it stays clear of BMC lockout. A
/// probe that never authenticates returns `Err`, which the caller records as a
/// quarantine with backoff. Returns an already-`to_string`-ed error (still to be
/// redacted by the caller); never returns a secret-bearing string itself.
async fn resolve_dispatch_vendor(
    redfish_pool: &dyn BmcCredentialOps,
    bmc: &BmcRotationTarget,
    rotate_from: &Credentials,
    rotate_to: &Credentials,
) -> Result<RedfishVendor, String> {
    match &bmc.vendor {
        DispatchVendor::Fixed(vendor) => Ok(*vendor),
        DispatchVendor::Probe => {
            let mut last_err = None;
            for candidate in [rotate_from, rotate_to] {
                match redfish_pool
                    .probe_bmc_vendor(&bmc.host, bmc.port, candidate.clone())
                    .await
                {
                    Ok(vendor) => return Ok(vendor),
                    Err(e) => last_err = Some(e.to_string()),
                }
            }
            Err(format!(
                "probe BMC dispatch vendor: {}",
                last_err.unwrap_or_else(|| "no credential candidates".to_string())
            ))
        }
    }
}

/// Replace every non-empty secret in `message` with `REDACTED`. Defense in
/// depth on top of the Redfish layer's own redaction, so no password reaches a
/// log line or the `rotate_last_error_redacted` column.
fn redact(message: String, secrets: &[&str]) -> String {
    let mut message = message;
    for secret in secrets {
        if !secret.is_empty() {
            message = message.replace(secret, "REDACTED");
        }
    }
    message
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use carbide_instrument::emit;
    use carbide_instrument::testing::{CapturedFieldKind, MetricsCapture, capture_logs};
    use carbide_redfish::libredfish::BmcCredentialOps;
    use carbide_redfish::libredfish::test_support::RedfishSim;
    use carbide_secrets::credentials::{
        BmcCredentialType, CredentialKey, CredentialReader, CredentialWriter, Credentials,
    };
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use carbide_test_support::{Check, check_values};
    use chrono::{Duration, Utc};
    use db::credential_rotation::{
        DeviceRotationStatus, device_rotation_status, increment_rotate_attempt,
        mark_device_rotating_to_version, record_device_converged, set_next_target_version,
    };
    use libredfish::model::service_root::RedfishVendor;
    use mac_address::MacAddress;
    use sqlx::PgPool;

    use super::{
        BMC, BmcCredentialRotationConverged, BmcCredentialRotationQuarantined,
        BmcCredentialRotationRecovered, BmcEndpoint, BmcRotationTarget, CredentialConvergence,
        DPU_BMC_SERVICE, DispatchVendor, RotateOutcome, RotationGate, change_or_recover,
        needs_rotation, redact, resolve_dispatch_vendor, rotate_bmc, rotate_dpu_bmc_service,
    };

    const BMC_ROTATION_RESULTS_METRIC: &str = "carbide_bmc_credential_rotation_results_total";
    const TEST_MAC: &str = "02:00:00:00:00:01";
    const TEST_TARGET_VERSION: u32 = 7;

    fn test_mac() -> MacAddress {
        TEST_MAC.parse().unwrap()
    }

    fn creds(username: &str, password: &str) -> Credentials {
        Credentials::UsernamePassword {
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    fn target() -> BmcRotationTarget {
        BmcRotationTarget {
            device_mac: test_mac(),
            host: "127.0.0.1".to_string(),
            port: Some(443),
            vendor: DispatchVendor::Fixed(RedfishVendor::NvidiaGBx00),
        }
    }

    /// A probe-vendor variant of [`target`] for power-shelf-style PMCs, whose
    /// dispatch vendor the engine resolves at rotation time.
    fn probe_target() -> BmcRotationTarget {
        BmcRotationTarget {
            vendor: DispatchVendor::Probe,
            ..target()
        }
    }

    fn per_device_key() -> CredentialKey {
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: test_mac(),
            },
        }
    }

    fn rotate_to_key(version: u32) -> CredentialKey {
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::site_wide_root(version),
        }
    }

    /// Record the device as converged at the current target (0), then advance
    /// the site-wide BMC target `steps` times so the device lags by `steps`.
    async fn seed_device_behind_target(pool: &PgPool, steps: i32) {
        let mut conn = pool.acquire().await.unwrap();
        record_device_converged(&mut conn, test_mac(), BMC)
            .await
            .unwrap();
        for expected in 0..steps {
            set_next_target_version(&mut conn, BMC, expected, serde_json::json!({}))
                .await
                .unwrap()
                .expect("target must advance from the expected current version");
        }
    }

    async fn status_of(pool: &PgPool) -> DeviceRotationStatus {
        let mut conn = pool.acquire().await.unwrap();
        device_rotation_status(&mut conn, BMC, test_mac())
            .await
            .unwrap()
            .expect("device row must exist")
    }

    /// A [`RedfishSim`] modeling a BMC whose `root` account currently holds
    /// `password`, with authentication enforced and same-value password changes
    /// rejected -- the vendor behavior BMC rotation's crash recovery must
    /// accommodate. Rotation scenarios then emerge from the seeded hardware
    /// password: a change authenticated with a stale credential fails, the
    /// rotate-TO value authenticates only once the hardware already carries it,
    /// and a re-issued same-value change is refused (so the engine is held to
    /// never issuing one).
    fn bmc_on_password(password: &str) -> RedfishSim {
        let sim = RedfishSim::default();
        sim.set_enforce_auth(true);
        sim.set_reject_password_reuse(true);
        sim.seed_user("root", password);
        sim
    }

    /// Whether the BMC currently authenticates with `password`.
    async fn bmc_accepts(sim: &RedfishSim, password: &str) -> bool {
        sim.bmc_credentials_valid("127.0.0.1", Some(443), creds("root", password))
            .await
            .expect("the credential probe must not raise a transport error")
    }

    fn rotation_result_deltas(metrics: &MetricsCapture) -> [f64; 3] {
        [
            metrics.counter_delta(BMC_ROTATION_RESULTS_METRIC, &[("result", "converged")]),
            metrics.counter_delta(BMC_ROTATION_RESULTS_METRIC, &[("result", "recovered")]),
            metrics.counter_delta(BMC_ROTATION_RESULTS_METRIC, &[("result", "quarantined")]),
        ]
    }

    enum RotationEventCase {
        Converged,
        Recovered { change_error: &'static str },
        Quarantined { error: &'static str },
    }

    impl RotationEventCase {
        fn result(&self) -> &'static str {
            match self {
                Self::Converged => "converged",
                Self::Recovered { .. } => "recovered",
                Self::Quarantined { .. } => "quarantined",
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct RotationEventObservation {
        metric_deltas: [f64; 3],
        logs: Vec<RotationLogObservation>,
    }

    #[derive(Debug, PartialEq)]
    struct RotationLogObservation {
        metadata_name: String,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        event_name_kind: Option<CapturedFieldKind>,
        metric_name: Option<String>,
        metric_name_kind: Option<CapturedFieldKind>,
        result: Option<String>,
        result_kind: Option<CapturedFieldKind>,
        mac: Option<String>,
        mac_kind: Option<CapturedFieldKind>,
        target_version: Option<String>,
        target_version_kind: Option<CapturedFieldKind>,
        change_error: Option<String>,
        change_error_kind: Option<CapturedFieldKind>,
        error: Option<String>,
        error_kind: Option<CapturedFieldKind>,
    }

    fn expected_rotation_log(
        metadata_name: &str,
        level: tracing::Level,
        message: &str,
        result: &str,
        change_error: Option<&str>,
        error: Option<&str>,
    ) -> Vec<RotationLogObservation> {
        vec![RotationLogObservation {
            metadata_name: metadata_name.to_string(),
            level,
            message: message.to_string(),
            event_name: Some(metadata_name.to_string()),
            event_name_kind: Some(CapturedFieldKind::String),
            metric_name: Some(BMC_ROTATION_RESULTS_METRIC.to_string()),
            metric_name_kind: Some(CapturedFieldKind::String),
            result: Some(result.to_string()),
            result_kind: Some(CapturedFieldKind::String),
            mac: Some(TEST_MAC.to_string()),
            mac_kind: Some(CapturedFieldKind::Debug),
            target_version: Some(TEST_TARGET_VERSION.to_string()),
            target_version_kind: Some(CapturedFieldKind::I64),
            change_error: change_error.map(str::to_string),
            change_error_kind: change_error.map(|_| CapturedFieldKind::Debug),
            error: error.map(str::to_string),
            error_kind: error.map(|_| CapturedFieldKind::Debug),
        }]
    }

    fn observe_rotation_event(case: RotationEventCase) -> RotationEventObservation {
        let result = case.result();
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| match case {
            RotationEventCase::Converged => emit(BmcCredentialRotationConverged::new(
                test_mac(),
                TEST_TARGET_VERSION,
            )),
            RotationEventCase::Recovered { change_error } => {
                emit(BmcCredentialRotationRecovered::new(
                    test_mac(),
                    TEST_TARGET_VERSION,
                    change_error.to_string(),
                ))
            }
            RotationEventCase::Quarantined { error } => {
                emit(BmcCredentialRotationQuarantined::new(
                    test_mac(),
                    TEST_TARGET_VERSION,
                    error.to_string(),
                ))
            }
        })
        .into_iter()
        .map(|log| RotationLogObservation {
            event_name: log.field("event_name").map(str::to_string),
            event_name_kind: log.field_kind("event_name"),
            metric_name: log.field("metric_name").map(str::to_string),
            metric_name_kind: log.field_kind("metric_name"),
            result: log.field("result").map(str::to_string),
            result_kind: log.field_kind("result"),
            mac: log.field("mac").map(str::to_string),
            mac_kind: log.field_kind("mac"),
            target_version: log.field("target_version").map(str::to_string),
            target_version_kind: log.field_kind("target_version"),
            change_error: log.field("change_error").map(str::to_string),
            change_error_kind: log.field_kind("change_error"),
            error: log.field("error").map(str::to_string),
            error_kind: log.field_kind("error"),
            metadata_name: log.metadata_name,
            level: log.level,
            message: log.message,
        })
        .collect();

        let metric_deltas = rotation_result_deltas(&metrics);
        assert_eq!(
            metric_deltas.iter().sum::<f64>(),
            1.0,
            "{result} must increment exactly one result series"
        );

        RotationEventObservation {
            metric_deltas,
            logs,
        }
    }

    #[test]
    fn bmc_rotation_events_preserve_each_terminal_log_and_count_one_result() {
        const RECOVERY_ERROR: &str = "stale credential rejected; password=REDACTED";
        const QUARANTINE_ERROR: &str = "BMC rejected login with password=REDACTED";

        check_values(
            [
                Check {
                    scenario: "normal convergence keeps the existing info record",
                    input: RotationEventCase::Converged,
                    expect: RotationEventObservation {
                        metric_deltas: [1.0, 0.0, 0.0],
                        logs: expected_rotation_log(
                            "bmc_credential_rotation_converged",
                            tracing::Level::INFO,
                            "BMC credential rotated and converged",
                            "converged",
                            None,
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "crash recovery keeps the redacted change failure",
                    input: RotationEventCase::Recovered {
                        change_error: RECOVERY_ERROR,
                    },
                    expect: RotationEventObservation {
                        metric_deltas: [0.0, 1.0, 0.0],
                        logs: expected_rotation_log(
                            "bmc_credential_rotation_recovered",
                            tracing::Level::WARN,
                            "BMC already at rotate-to credential; recovered from an interrupted prior rotation",
                            "recovered",
                            Some(RECOVERY_ERROR),
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "quarantine keeps the redacted terminal failure",
                    input: RotationEventCase::Quarantined {
                        error: QUARANTINE_ERROR,
                    },
                    expect: RotationEventObservation {
                        metric_deltas: [0.0, 0.0, 1.0],
                        logs: expected_rotation_log(
                            "bmc_credential_rotation_quarantined",
                            tracing::Level::WARN,
                            "BMC credential rotation attempt failed; quarantining",
                            "quarantined",
                            None,
                            Some(QUARANTINE_ERROR),
                        ),
                    },
                },
            ],
            observe_rotation_event,
        );
    }

    #[tokio::test]
    async fn change_or_recover_changes_password_when_current_authenticates() {
        // The BMC is on "old"; the change to "new" authenticates and succeeds.
        let sim = bmc_on_password("old");

        let convergence = change_or_recover(
            &sim,
            &target(),
            RedfishVendor::NvidiaGBx00,
            creds("root", "old"),
            creds("root", "new"),
        )
        .await
        .expect("the change should succeed");

        assert_eq!(
            convergence,
            CredentialConvergence::Changed,
            "a successful direct change must report Changed"
        );
        assert!(
            bmc_accepts(&sim, "new").await,
            "the change must leave the BMC on the rotate-to password"
        );
    }

    #[tokio::test]
    async fn change_or_recover_converges_when_hardware_already_on_rotate_to() {
        // The BMC is already on "new" (a prior attempt changed it before
        // crashing): the change authenticated with the stale "old" fails, but the
        // probe with "new" succeeds, so recovery converges -- and because the sim
        // rejects a same-value change, this can only pass by probing rather than
        // re-issuing the change.
        let sim = bmc_on_password("new");

        let convergence = change_or_recover(
            &sim,
            &target(),
            RedfishVendor::NvidiaGBx00,
            creds("root", "old"),
            creds("root", "new"),
        )
        .await
        .expect("an already-converged BMC must be recovered");

        assert!(
            matches!(convergence, CredentialConvergence::Recovered { .. }),
            "an already-at-target BMC must report Recovered, got {convergence:?}"
        );
    }

    #[tokio::test]
    async fn change_or_recover_fails_when_neither_credential_authenticates() {
        // The BMC is on some third password, so neither the change (with "old")
        // nor the probe (with "new") authenticates: the change error is surfaced.
        let sim = bmc_on_password("mystery");

        change_or_recover(
            &sim,
            &target(),
            RedfishVendor::NvidiaGBx00,
            creds("root", "old"),
            creds("root", "new"),
        )
        .await
        .expect_err("neither credential authenticating must surface an error");
    }

    #[tokio::test]
    async fn change_or_recover_surfaces_both_errors_when_the_probe_also_fails() {
        // The change fails and the recovery probe itself errors with a transport
        // fault (not a clean rejection): both failures are surfaced so the
        // recorded reason makes clear convergence could not even be confirmed.
        let sim = bmc_on_password("mystery");
        sim.set_change_password_error("change boom");
        sim.set_get_accounts_error(true);

        let err = change_or_recover(
            &sim,
            &target(),
            RedfishVendor::NvidiaGBx00,
            creds("root", "old"),
            creds("root", "new"),
        )
        .await
        .expect_err("a failed change plus a failed probe must surface an error");

        assert!(
            err.contains("probe also failed"),
            "both the change and probe failures must be surfaced: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_dispatch_vendor_returns_the_fixed_vendor_without_probing() {
        // A Fixed target (machine / switch) returns its vendor verbatim; the sim's
        // service root would resolve to a different vendor, so a returned NvidiaGBx00
        // proves the fixed value short-circuits the probe.
        let sim = RedfishSim::default();
        let vendor = resolve_dispatch_vendor(
            &sim,
            &target(),
            &creds("root", "old"),
            &creds("root", "new"),
        )
        .await
        .expect("a fixed vendor resolves without touching the BMC");
        assert_eq!(vendor, RedfishVendor::NvidiaGBx00);
    }

    #[tokio::test]
    async fn resolve_dispatch_vendor_probes_the_chassis_manufacturer_for_power_shelves() {
        // A Probe target with an unrecognized service-root vendor falls back to the
        // Chassis manufacturer, the standard power-shelf (Lite-On / Delta)
        // determination.
        for (manufacturer, expected) in [
            ("Lite-On Technology Corp.", RedfishVendor::LiteOnPowerShelf),
            ("Delta Electronics", RedfishVendor::DeltaPowerShelf),
        ] {
            let sim = RedfishSim::default();
            sim.set_service_root_vendor(Some("Unrecognized Vendor".to_string()));
            sim.set_chassis_manufacturer(Some(manufacturer.to_string()));
            let vendor = resolve_dispatch_vendor(
                &sim,
                &probe_target(),
                &creds("root", "old"),
                &creds("root", "new"),
            )
            .await
            .expect("a Lite-On/Delta chassis must resolve a power-shelf vendor");
            assert_eq!(vendor, expected, "manufacturer {manufacturer:?}");
        }
    }

    #[test]
    fn needs_rotation_only_when_behind_and_not_quarantined() {
        let mut status = DeviceRotationStatus {
            target_version: 1,
            started_at: Utc::now(),
            device_mac: TEST_MAC.to_string(),
            current_version: Some(0),
            rotating_to_version: None,
            converged: false,
            quarantined: false,
            quarantined_until: None,
            rotate_attempts: 0,
            rotate_last_attempt_at: None,
            rotate_last_error_redacted: None,
        };
        assert!(needs_rotation(&status), "behind target and not quarantined");

        status.quarantined = true;
        assert!(!needs_rotation(&status), "quarantined is left alone");

        status.quarantined = false;
        status.converged = true;
        assert!(!needs_rotation(&status), "converged is left alone");
    }

    #[test]
    fn redact_replaces_every_nonempty_secret_and_skips_empty() {
        // Defense in depth: every non-empty secret is masked (an empty needle is
        // skipped, never matching the whole string) so no password fragment can
        // survive into a log line or the recorded error.
        let masked = redact(
            "login user=root current=swordfish new=hunter2".to_string(),
            &["swordfish", "hunter2", ""],
        );

        assert_eq!(masked, "login user=root current=REDACTED new=REDACTED");
        assert!(
            !masked.contains("swordfish") && !masked.contains("hunter2"),
            "no secret may survive redaction: {masked}"
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_converges_and_persists_new_secret(pool: PgPool) {
        seed_device_behind_target(&pool, 1).await;
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "old"))
            .await
            .unwrap();
        cm.set_credentials(&rotate_to_key(1), &creds("root", "new"))
            .await
            .unwrap();
        // The BMC is on the per-device secret ("old"), so the change succeeds.
        let redfish = bmc_on_password("old");

        let metrics = MetricsCapture::start();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .expect("rotation must not raise a transient engine error");

        assert_eq!(outcome, RotateOutcome::Converged);
        assert_eq!(rotation_result_deltas(&metrics), [1.0, 0.0, 0.0]);
        drop(metrics);
        let status = status_of(&pool).await;
        assert!(status.converged, "device must be recorded converged");
        assert_eq!(status.current_version, Some(1));
        assert_eq!(
            status.rotating_to_version, None,
            "the crash marker must be cleared on promotion"
        );
        // The per-device secret was rewritten to the rotate-TO password so
        // future logins (and the next rotation's "current") use it.
        let persisted = cm
            .get_credentials(&per_device_key())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted, creds("root", "new"));
        // The hardware now authenticates with the rotate-TO password.
        assert!(
            bmc_accepts(&redfish, "new").await,
            "the BMC must now be on the rotate-TO password"
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_holds_for_store_reconcile_when_persist_fails_then_converges(pool: PgPool) {
        // Fault-injection: the BMC accepts the new password but persisting the
        // new per-device secret to the store fails. The hardware is now AHEAD of
        // the store.  The engine must NOT quarantine: it reports `CredentialStoreReconcilePending`,
        // leaves the row not-converged / not-quarantined (so it keeps reading as
        // `needs_rotation`), and the stored secret still lags. A later tick with
        // the store writable then recovers (the rotate-to value already authenticates)
        // and converges, reconciling the store -- reconciliation is never abandoned.
        seed_device_behind_target(&pool, 1).await;
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "old"))
            .await
            .unwrap();
        cm.set_credentials(&rotate_to_key(1), &creds("root", "new"))
            .await
            .unwrap();
        // The BMC is on the per-device secret ("old"), so the hardware change
        // old -> new succeeds; only the store write is made to fail.
        let redfish = bmc_on_password("old");
        cm.set_set_credentials_failure(true);

        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .expect("a store-persist failure is not a transient engine error");

        assert_eq!(
            outcome,
            RotateOutcome::CredentialStoreReconcilePending,
            "a persist failure after the hardware changed must hold for reconcile, not quarantine"
        );

        let status = status_of(&pool).await;
        assert!(
            !status.converged,
            "hardware ahead of store is not converged"
        );
        assert!(
            !status.quarantined,
            "a persist failure must NOT record a backoff; the device stays eligible so the persist is retried every sweep"
        );
        assert_eq!(
            status.current_version,
            Some(0),
            "the convergence marker must not advance while the store lags"
        );
        assert_eq!(
            status.rotating_to_version,
            Some(1),
            "the crash marker stays set (we never promoted) so recovery re-enters"
        );
        // Hardware is on the new password, but the stored secret still lags.
        assert!(
            bmc_accepts(&redfish, "new").await,
            "the hardware change must have landed even though the persist failed"
        );
        assert_eq!(
            cm.get_credentials(&per_device_key())
                .await
                .unwrap()
                .unwrap(),
            creds("root", "old"),
            "the stored secret must still lag after the failed persist"
        );

        // The store becomes writable again: the next tick's change-then-verify
        // recovery re-persists and converges, reconciling hardware and store.
        cm.set_set_credentials_failure(false);
        let metrics = MetricsCapture::start();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .expect("the reconciling tick must not raise a transient engine error");

        assert_eq!(outcome, RotateOutcome::Converged);
        assert_eq!(
            rotation_result_deltas(&metrics),
            [0.0, 1.0, 0.0],
            "reconciling an already-changed BMC counts as a recovery"
        );
        drop(metrics);
        let status = status_of(&pool).await;
        assert!(status.converged);
        assert_eq!(status.current_version, Some(1));
        assert_eq!(status.rotating_to_version, None);
        assert_eq!(
            cm.get_credentials(&per_device_key())
                .await
                .unwrap()
                .unwrap(),
            creds("root", "new"),
            "the store is reconciled to the new password once the write succeeds"
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_recovers_when_hardware_already_changed_before_crash(pool: PgPool) {
        // Models a crash after the hardware password changed but before the
        // per-device secret was persisted: the stored "current" is stale
        // ("old"), and only the rotate-TO value ("new") authenticates.
        seed_device_behind_target(&pool, 1).await;
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "old"))
            .await
            .unwrap();
        cm.set_credentials(&rotate_to_key(1), &creds("root", "new"))
            .await
            .unwrap();
        // The hardware is already on "new" while the stored secret is stale
        // ("old"): the change authenticated with "old" fails, the probe with
        // "new" succeeds, and recovery converges. The sim's same-value rejection
        // means this can only pass by probing, not by re-issuing the change.
        let redfish = bmc_on_password("new");

        let metrics = MetricsCapture::start();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .unwrap();

        assert_eq!(outcome, RotateOutcome::Converged);
        assert_eq!(rotation_result_deltas(&metrics), [0.0, 1.0, 0.0]);
        drop(metrics);
        assert_eq!(status_of(&pool).await.current_version, Some(1));
        assert_eq!(
            cm.get_credentials(&per_device_key())
                .await
                .unwrap()
                .unwrap(),
            creds("root", "new"),
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_quarantines_with_backoff_and_redacted_error(pool: PgPool) {
        seed_device_behind_target(&pool, 1).await;
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "old"))
            .await
            .unwrap();
        cm.set_credentials(&rotate_to_key(1), &creds("root", "topsecret"))
            .await
            .unwrap();
        // The BMC is on some other password and the change fails carrying the
        // rotate-TO password (so we can assert it never lands in the recorded
        // error); the rotate-TO value does not authenticate either, so the device
        // is quarantined. Redaction is exercised end to end: the redfish layer
        // and the engine both strip the password before it is recorded.
        let redfish = bmc_on_password("mystery");
        redfish.set_change_password_error("BMC rejected login with password=topsecret");

        let metrics = MetricsCapture::start();
        let before = Utc::now();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .unwrap();

        let until = match outcome {
            RotateOutcome::Quarantined { until } => until,
            other => panic!("expected Quarantined, got {other:?}"),
        };
        assert_eq!(rotation_result_deltas(&metrics), [0.0, 0.0, 1.0]);
        assert!(
            !metrics.render().contains("topsecret"),
            "credential material must not become a metric label"
        );
        drop(metrics);
        // First failure: backoff is the base window (15 minutes) from "now".
        assert!(until >= before + Duration::minutes(15));
        assert!(until <= Utc::now() + Duration::minutes(15) + Duration::seconds(1));

        let status = status_of(&pool).await;
        assert!(status.quarantined, "device must be in a backoff window");
        assert!(!status.converged);
        assert_eq!(status.rotate_attempts, 1);
        let recorded = status
            .rotate_last_error_redacted
            .expect("a failure must record a redacted error");
        assert!(
            !recorded.contains("topsecret"),
            "the password must never reach the error column, got: {recorded}"
        );
        assert!(recorded.contains("REDACTED"));
        // A failed attempt must not advance the convergence marker.
        assert_eq!(status.current_version, Some(0));
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_reports_converged_row_without_touching_hardware(pool: PgPool) {
        // A device already at the target (converged marker, current == target) is
        // a no-op: rotate_bmc returns Converged straight from the row without
        // resolving credentials or contacting the BMC. This is the idempotency
        // guarantee the controller relies on when it re-ticks a converged device.
        seed_device_behind_target(&pool, 0).await;
        let cm = TestCredentialManager::default();
        let redfish = bmc_on_password("old");

        let metrics = MetricsCapture::start();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .unwrap();

        assert_eq!(outcome, RotateOutcome::Converged);
        assert_eq!(
            rotation_result_deltas(&metrics),
            [0.0, 0.0, 0.0],
            "an already-converged row is not a new persisted result"
        );
        drop(metrics);
        assert!(
            redfish.create_client_calls().is_empty(),
            "an already-converged device must not touch hardware"
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_quarantines_when_rotate_to_secret_is_not_staged(pool: PgPool) {
        // The site-wide target advanced but the rotate-TO secret was never written
        // to the store (a staging bug): the device quarantines with a clear,
        // secret-free reason, and the BMC is never contacted because the missing
        // secret is caught before any change is issued.
        seed_device_behind_target(&pool, 1).await;
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "old"))
            .await
            .unwrap();
        // rotate_to_key(1) is intentionally left unstaged.
        let redfish = bmc_on_password("old");

        let metrics = MetricsCapture::start();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .unwrap();

        assert!(matches!(outcome, RotateOutcome::Quarantined { .. }));
        assert_eq!(rotation_result_deltas(&metrics), [0.0, 0.0, 1.0]);
        drop(metrics);
        let recorded = status_of(&pool)
            .await
            .rotate_last_error_redacted
            .expect("a failure must record a reason");
        assert!(
            recorded.contains("not staged"),
            "the reason must name the missing rotate-to secret: {recorded}"
        );
        assert!(
            redfish.create_client_calls().is_empty(),
            "a missing rotate-to secret must be caught before touching hardware"
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_reports_no_work_without_a_rotation_row(pool: PgPool) {
        // No `device_credential_rotation` row was seeded for this MAC.
        let cm = TestCredentialManager::default();
        let redfish = bmc_on_password("old");

        let metrics = MetricsCapture::start();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .unwrap();

        assert_eq!(outcome, RotateOutcome::NoWork);
        assert_eq!(
            rotation_result_deltas(&metrics),
            [0.0, 0.0, 0.0],
            "a missing rotation row must stay silent"
        );
        drop(metrics);
        assert!(
            redfish.create_client_calls().is_empty(),
            "an orphaned device must not touch hardware"
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_skips_a_quarantined_device(pool: PgPool) {
        seed_device_behind_target(&pool, 1).await;
        // Plant a future backoff window directly.
        let mut conn = pool.acquire().await.unwrap();
        increment_rotate_attempt(
            &mut conn,
            test_mac(),
            BMC,
            "earlier failure",
            Utc::now() + Duration::seconds(3600),
        )
        .await
        .unwrap();
        drop(conn);
        let cm = TestCredentialManager::default();
        let redfish = bmc_on_password("old");

        let metrics = MetricsCapture::start();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .unwrap();

        assert!(matches!(outcome, RotateOutcome::Quarantined { .. }));
        assert_eq!(
            rotation_result_deltas(&metrics),
            [0.0, 0.0, 0.0],
            "an active quarantine is not a new persisted result"
        );
        drop(metrics);
        assert!(
            redfish.create_client_calls().is_empty(),
            "a quarantined device must not be retried before its window passes"
        );
        // The marker is untouched (still behind target, not converged).
        assert_eq!(status_of(&pool).await.current_version, Some(0));
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_force_attempts_a_quarantined_device(pool: PgPool) {
        // Same lagging + quarantined device as the skip test, but the operator
        // force flag makes the engine attempt the change during the backoff
        // window and converge it, rather than short-circuiting as Quarantined.
        seed_device_behind_target(&pool, 1).await;
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "old"))
            .await
            .unwrap();
        cm.set_credentials(&rotate_to_key(1), &creds("root", "new"))
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        increment_rotate_attempt(
            &mut conn,
            test_mac(),
            BMC,
            "earlier failure",
            Utc::now() + Duration::seconds(3600),
        )
        .await
        .unwrap();
        drop(conn);
        let redfish = bmc_on_password("old");

        // Hold the metrics serialization guard across the converging call: it
        // emits `converged` into the process-global registry, and without the
        // guard it could run concurrently with a metric-asserting test and leak
        // that count into its delta window. This test asserts outcome/state, not
        // metrics, so the guard is dropped before those checks.
        let metrics = MetricsCapture::start();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), true)
            .await
            .expect("a forced rotation must not raise a transient engine error");
        drop(metrics);

        assert_eq!(
            outcome,
            RotateOutcome::Converged,
            "force must attempt the change despite an active quarantine window"
        );
        let status = status_of(&pool).await;
        assert_eq!(status.current_version, Some(1));
        assert!(
            !redfish.create_client_calls().is_empty(),
            "a forced device must actually touch hardware"
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_bmc_converges_to_the_latest_target_when_superseded(pool: PgPool) {
        // The device lags by two versions: a rotation that began toward v1 must
        // converge to the live target (v2), not the stale intermediate.
        seed_device_behind_target(&pool, 2).await;
        // Simulate a prior crashed attempt that staged the (now stale) v1 marker.
        let mut conn = pool.acquire().await.unwrap();
        mark_device_rotating_to_version(&mut conn, test_mac(), BMC, 1)
            .await
            .unwrap();
        drop(conn);
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "old"))
            .await
            .unwrap();
        cm.set_credentials(&rotate_to_key(2), &creds("root", "newest"))
            .await
            .unwrap();
        let redfish = bmc_on_password("old");

        let metrics = MetricsCapture::start();
        let outcome = rotate_bmc(&pool, &cm, &redfish, &target(), false)
            .await
            .unwrap();

        assert_eq!(outcome, RotateOutcome::Converged);
        assert_eq!(rotation_result_deltas(&metrics), [1.0, 0.0, 0.0]);
        drop(metrics);
        let status = status_of(&pool).await;
        assert_eq!(
            status.current_version,
            Some(2),
            "must converge to the live target, superseding the stale v1 marker"
        );
        assert!(status.converged);
        assert_eq!(
            cm.get_credentials(&per_device_key())
                .await
                .unwrap()
                .unwrap(),
            creds("root", "newest"),
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotation_gate_caches_aggregate_and_gates_per_device(pool: PgPool) {
        // Nothing staged yet (bmc seeded at target 0): the gate reports no work
        // without a per-device query.
        let gate = RotationGate::new_for_family(BMC);
        assert!(
            !gate.rotation_pending(&pool).await.unwrap(),
            "target 0 baseline is not work"
        );
        assert!(
            !gate.rotation_needed(&pool, test_mac()).await.unwrap(),
            "no work means the per-device guard is false"
        );

        // Stage a rotation the device lags. A long-TTL gate still returns the
        // cached (stale) `false` -- proving it does not re-query every call.
        seed_device_behind_target(&pool, 1).await;
        assert!(
            !gate.rotation_pending(&pool).await.unwrap(),
            "a fresh cache must not observe the newly staged target yet"
        );

        // A zero-TTL gate always re-queries, so it observes the staged work and
        // the per-device guard now fires.
        let fresh = RotationGate::with_ttl_and_family(StdDuration::ZERO, BMC);
        assert!(fresh.rotation_pending(&pool).await.unwrap());
        assert!(
            fresh.rotation_needed(&pool, test_mac()).await.unwrap(),
            "a lagging device under a live target needs rotation"
        );
        // An unknown device has no row, so the guard is false even when work is
        // pending site-wide.
        let unknown: MacAddress = "02:00:00:00:00:ff".parse().unwrap();
        assert!(!fresh.rotation_needed(&pool, unknown).await.unwrap());
    }

    // --- DPU BMC `service` rotation (`rotate_dpu_bmc_service`) ---------------
    //
    // Unlike BMC root, every attempt authenticates with the per-device BMC
    // *root* secret and PATCHes the `service` account, so there is no
    // change-then-verify recovery and no per-device secret to persist. These
    // tests lock the resulting simpler contract: converge (writing nothing but
    // the hardware), resume idempotently after a crash, quarantine with a
    // redacted error, and the shared NoWork / already-converged / force gating.

    const DPU_SERVICE_ROTATION_RESULTS_METRIC: &str =
        "carbide_dpu_bmc_service_credential_rotation_results_total";

    fn dpu_service_endpoint() -> BmcEndpoint {
        BmcEndpoint {
            device_mac: test_mac(),
            host: "127.0.0.1".to_string(),
            port: Some(443),
        }
    }

    fn dpu_service_rotate_to_key(version: u32) -> CredentialKey {
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::site_wide_dpu_bmc_service(version),
        }
    }

    /// Record the DPU converged on the `dpu_bmc_service` family at the current
    /// target (0), then advance the site-wide service target `steps` times so
    /// the device lags by `steps`.
    async fn seed_dpu_service_behind_target(pool: &PgPool, steps: i32) {
        let mut conn = pool.acquire().await.unwrap();
        record_device_converged(&mut conn, test_mac(), DPU_BMC_SERVICE)
            .await
            .unwrap();
        for expected in 0..steps {
            set_next_target_version(&mut conn, DPU_BMC_SERVICE, expected, serde_json::json!({}))
                .await
                .unwrap()
                .expect("target must advance from the expected current version");
        }
    }

    async fn dpu_service_status_of(pool: &PgPool) -> DeviceRotationStatus {
        let mut conn = pool.acquire().await.unwrap();
        device_rotation_status(&mut conn, DPU_BMC_SERVICE, test_mac())
            .await
            .unwrap()
            .expect("device row must exist")
    }

    /// A [`RedfishSim`] modeling a BF4 DPU BMC whose `root` account
    /// authenticates with `root_password` and whose `service` account currently
    /// holds `service_password`. Auth is enforced (a stale root secret fails).
    /// Password reuse is *not* rejected by default, modeling a BMC on which a
    /// root-authenticated re-PATCH of the same value simply succeeds; the
    /// reuse-rejecting variant is opted into per-test via `set_reject_password_reuse`.
    fn dpu_bmc(root_password: &str, service_password: &str) -> RedfishSim {
        let sim = RedfishSim::default();
        sim.set_enforce_auth(true);
        sim.seed_user("root", root_password);
        sim.seed_user("service", service_password);
        sim
    }

    /// `[converged, recovered, quarantined]` deltas for the shared
    /// `carbide_dpu_bmc_service_credential_rotation_results_total` metric.
    fn dpu_service_result_deltas(metrics: &MetricsCapture) -> [f64; 3] {
        [
            metrics.counter_delta(
                DPU_SERVICE_ROTATION_RESULTS_METRIC,
                &[("result", "converged")],
            ),
            metrics.counter_delta(
                DPU_SERVICE_ROTATION_RESULTS_METRIC,
                &[("result", "recovered")],
            ),
            metrics.counter_delta(
                DPU_SERVICE_ROTATION_RESULTS_METRIC,
                &[("result", "quarantined")],
            ),
        ]
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_dpu_bmc_service_converges_and_writes_no_per_device_secret(pool: PgPool) {
        seed_dpu_service_behind_target(&pool, 1).await;
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "rootpw"))
            .await
            .unwrap();
        cm.set_credentials(
            &dpu_service_rotate_to_key(1),
            &creds("service", "new-service-pw"),
        )
        .await
        .unwrap();
        // The service account is on some old password; root authenticates the
        // PATCH, so the change succeeds.
        let redfish = dpu_bmc("rootpw", "old-service-pw");

        let metrics = MetricsCapture::start();
        let outcome = rotate_dpu_bmc_service(&pool, &cm, &redfish, &dpu_service_endpoint(), false)
            .await
            .expect("rotation must not raise a transient engine error");

        assert_eq!(outcome, RotateOutcome::Converged);
        assert_eq!(dpu_service_result_deltas(&metrics), [1.0, 0.0, 0.0]);
        drop(metrics);

        let status = dpu_service_status_of(&pool).await;
        assert!(status.converged, "device must be recorded converged");
        assert_eq!(status.current_version, Some(1));
        assert_eq!(
            status.rotating_to_version, None,
            "the crash marker must be cleared on promotion"
        );

        // The hardware `service` account now carries the rotate-to password,
        // authenticated with the per-device root secret that is only read.
        assert_eq!(
            redfish.user_password("service").as_deref(),
            Some("new-service-pw"),
            "the service account must be on the rotate-to password"
        );
        assert_eq!(redfish.user_password("root").as_deref(), Some("rootpw"));
        assert_eq!(
            cm.get_credentials(&per_device_key())
                .await
                .unwrap()
                .unwrap(),
            creds("root", "rootpw"),
            "the per-device root secret is read for auth, never rewritten"
        );
        // The site-wide versioned secret is the only copy of the service
        // password and convergence never rewrites it.
        assert_eq!(
            cm.get_credentials(&dpu_service_rotate_to_key(1))
                .await
                .unwrap()
                .unwrap(),
            creds("service", "new-service-pw"),
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_dpu_bmc_service_resumes_idempotently_after_a_crash_before_promotion(
        pool: PgPool,
    ) {
        // A crash after the hardware `service` password changed but before
        // promotion: the marker is set and the hardware already carries the
        // rotate-to password. Because root (not the service account) authenticates
        // the PATCH, re-applying it is idempotent and converges with no
        // change-then-verify recovery -- the whole reason this family is simpler
        // than BMC root.
        seed_dpu_service_behind_target(&pool, 1).await;
        let mut conn = pool.acquire().await.unwrap();
        mark_device_rotating_to_version(&mut conn, test_mac(), DPU_BMC_SERVICE, 1)
            .await
            .unwrap();
        drop(conn);
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "rootpw"))
            .await
            .unwrap();
        cm.set_credentials(
            &dpu_service_rotate_to_key(1),
            &creds("service", "new-service-pw"),
        )
        .await
        .unwrap();
        // The service account already holds the rotate-to password from the
        // prior (crashed) attempt.
        let redfish = dpu_bmc("rootpw", "new-service-pw");

        let metrics = MetricsCapture::start();
        let outcome = rotate_dpu_bmc_service(&pool, &cm, &redfish, &dpu_service_endpoint(), false)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            RotateOutcome::Converged,
            "an idempotent re-PATCH must converge, not quarantine"
        );
        assert_eq!(dpu_service_result_deltas(&metrics), [1.0, 0.0, 0.0]);
        drop(metrics);
        let status = dpu_service_status_of(&pool).await;
        assert!(status.converged);
        assert_eq!(status.current_version, Some(1));
        assert_eq!(status.rotating_to_version, None);
        assert_eq!(
            redfish.user_password("service").as_deref(),
            Some("new-service-pw")
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_dpu_bmc_service_recovers_when_the_bmc_rejects_the_reapplied_password(
        pool: PgPool,
    ) {
        // Same crash-before-promotion state as the idempotent-resume test, but on
        // a BMC that enforces a password-reuse policy: re-applying the rotate-to
        // value the `service` account already holds is rejected. Without the
        // failure-path probe this would falsely quarantine an already-converged
        // device; with it, authenticating as `service` with the rotate-to
        // password confirms convergence and the attempt promotes.
        seed_dpu_service_behind_target(&pool, 1).await;
        let mut conn = pool.acquire().await.unwrap();
        mark_device_rotating_to_version(&mut conn, test_mac(), DPU_BMC_SERVICE, 1)
            .await
            .unwrap();
        drop(conn);
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "rootpw"))
            .await
            .unwrap();
        cm.set_credentials(
            &dpu_service_rotate_to_key(1),
            &creds("service", "new-service-pw"),
        )
        .await
        .unwrap();
        // The service account already holds the rotate-to password, and this BMC
        // rejects a same-value change.
        let redfish = dpu_bmc("rootpw", "new-service-pw");
        redfish.set_reject_password_reuse(true);

        let metrics = MetricsCapture::start();
        let outcome = rotate_dpu_bmc_service(&pool, &cm, &redfish, &dpu_service_endpoint(), false)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            RotateOutcome::Converged,
            "a reuse-policy rejection of the already-applied target must converge, not quarantine"
        );
        assert_eq!(
            dpu_service_result_deltas(&metrics),
            [0.0, 1.0, 0.0],
            "recovery via the probe must count as recovered, not converged"
        );
        drop(metrics);
        let status = dpu_service_status_of(&pool).await;
        assert!(status.converged);
        assert_eq!(status.current_version, Some(1));
        assert_eq!(status.rotating_to_version, None);
        assert_eq!(
            redfish.user_password("service").as_deref(),
            Some("new-service-pw")
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_dpu_bmc_service_quarantines_with_backoff_and_redacted_error(pool: PgPool) {
        seed_dpu_service_behind_target(&pool, 1).await;
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "rootpw"))
            .await
            .unwrap();
        cm.set_credentials(
            &dpu_service_rotate_to_key(1),
            &creds("service", "topsecret"),
        )
        .await
        .unwrap();
        let redfish = dpu_bmc("rootpw", "old-service-pw");
        // The PATCH fails carrying the rotate-to password, exercising redaction
        // end to end (the redfish layer and the engine both strip it).
        redfish.set_change_password_error("BMC rejected service change with password=topsecret");

        let metrics = MetricsCapture::start();
        let before = Utc::now();
        let outcome = rotate_dpu_bmc_service(&pool, &cm, &redfish, &dpu_service_endpoint(), false)
            .await
            .unwrap();

        let until = match outcome {
            RotateOutcome::Quarantined { until } => until,
            other => panic!("expected Quarantined, got {other:?}"),
        };
        assert_eq!(dpu_service_result_deltas(&metrics), [0.0, 0.0, 1.0]);
        assert!(
            !metrics.render().contains("topsecret"),
            "credential material must not become a metric label"
        );
        drop(metrics);
        // First failure: backoff is the base window (15 minutes) from "now".
        assert!(until >= before + Duration::minutes(15));

        let status = dpu_service_status_of(&pool).await;
        assert!(status.quarantined, "device must be in a backoff window");
        assert!(!status.converged);
        assert_eq!(status.rotate_attempts, 1);
        assert_eq!(
            status.current_version,
            Some(0),
            "a failed attempt must not advance the convergence marker"
        );
        let recorded = status
            .rotate_last_error_redacted
            .expect("a failure must record a redacted error");
        assert!(
            !recorded.contains("topsecret"),
            "the password must never reach the error column, got: {recorded}"
        );
        assert!(recorded.contains("REDACTED"));
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_dpu_bmc_service_reports_no_work_without_a_rotation_row(pool: PgPool) {
        // A non-BF4 DPU (or an unmanaged device) is never enrolled, so there is
        // no `device_credential_rotation` row and the engine skips it silently.
        let cm = TestCredentialManager::default();
        let redfish = dpu_bmc("rootpw", "old-service-pw");

        let metrics = MetricsCapture::start();
        let outcome = rotate_dpu_bmc_service(&pool, &cm, &redfish, &dpu_service_endpoint(), false)
            .await
            .unwrap();

        assert_eq!(outcome, RotateOutcome::NoWork);
        assert_eq!(dpu_service_result_deltas(&metrics), [0.0, 0.0, 0.0]);
        drop(metrics);
        assert!(
            redfish.create_client_calls().is_empty(),
            "an un-enrolled DPU must not touch hardware"
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_dpu_bmc_service_reports_converged_row_without_touching_hardware(pool: PgPool) {
        // A device already at the target is a no-op returned straight from the
        // row, without resolving credentials or contacting the BMC.
        seed_dpu_service_behind_target(&pool, 0).await;
        let cm = TestCredentialManager::default();
        let redfish = dpu_bmc("rootpw", "old-service-pw");

        let metrics = MetricsCapture::start();
        let outcome = rotate_dpu_bmc_service(&pool, &cm, &redfish, &dpu_service_endpoint(), false)
            .await
            .unwrap();

        assert_eq!(outcome, RotateOutcome::Converged);
        assert_eq!(dpu_service_result_deltas(&metrics), [0.0, 0.0, 0.0]);
        drop(metrics);
        assert!(
            redfish.create_client_calls().is_empty(),
            "an already-converged device must not touch hardware"
        );
    }

    #[carbide_macros::sqlx_test]
    async fn rotate_dpu_bmc_service_force_attempts_a_quarantined_device(pool: PgPool) {
        seed_dpu_service_behind_target(&pool, 1).await;
        // Plant a future backoff window directly.
        let mut conn = pool.acquire().await.unwrap();
        increment_rotate_attempt(
            &mut conn,
            test_mac(),
            DPU_BMC_SERVICE,
            "earlier failure",
            Utc::now() + Duration::seconds(3600),
        )
        .await
        .unwrap();
        drop(conn);
        let cm = TestCredentialManager::default();
        cm.set_credentials(&per_device_key(), &creds("root", "rootpw"))
            .await
            .unwrap();
        cm.set_credentials(
            &dpu_service_rotate_to_key(1),
            &creds("service", "new-service-pw"),
        )
        .await
        .unwrap();
        let redfish = dpu_bmc("rootpw", "old-service-pw");

        // Without force, the device is skipped inside its backoff window.
        let skipped = rotate_dpu_bmc_service(&pool, &cm, &redfish, &dpu_service_endpoint(), false)
            .await
            .unwrap();
        assert!(matches!(skipped, RotateOutcome::Quarantined { .. }));
        assert!(
            redfish.create_client_calls().is_empty(),
            "a quarantined device is not contacted without force"
        );

        // With force, the same device is attempted anyway and converges. The
        // capture window serializes the emitted `converged` result against other
        // tests observing the same metric family.
        let metrics = MetricsCapture::start();
        let forced = rotate_dpu_bmc_service(&pool, &cm, &redfish, &dpu_service_endpoint(), true)
            .await
            .unwrap();
        assert_eq!(dpu_service_result_deltas(&metrics), [1.0, 0.0, 0.0]);
        drop(metrics);
        assert_eq!(forced, RotateOutcome::Converged);
        assert_eq!(
            redfish.user_password("service").as_deref(),
            Some("new-service-pw")
        );
        let status = dpu_service_status_of(&pool).await;
        assert!(status.converged);
        assert_eq!(status.current_version, Some(1));
    }
}
