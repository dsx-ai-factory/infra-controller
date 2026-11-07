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

//! Machine-controller BMC credential rotation.
//!
//! The shared [`carbide_credential_rotation`] engine owns the actual password
//! dance, backoff, and crash-safety; this module is the thin machine-controller
//! adapter. It answers two questions for the state machine:
//!
//! - *Should we enter rotation?* [`bmc_rotation_needed`] asks the cached
//!   [`RotationGate`] whether the host BMC or any of its DPU BMCs lags the
//!   staged site-wide target. The gate keeps the steady state at one cheap
//!   aggregate query per TTL window (see the engine crate docs).
//! - *Do one rotation tick.* [`rotate_managed_host_bmcs`] converges the host BMC
//!   and each DPU BMC toward the BMC-root target via [`rotate_bmc`], and each DPU
//!   BMC toward the `service`-account target via [`rotate_dpu_bmc_service`], then
//!   reports whether the tick settled (every device reached a terminal outcome)
//!   or hit a transient bookkeeping failure worth retrying.
//!
//! The DPU BMC `service` account (BF4 only) rides this same `RotatingBmc` state
//! rather than a state of its own: converging it is a single root-authenticated
//! Redfish call with no host-power impact, exactly like BMC root, and only BF4
//! DPUs are ever enrolled (non-BF4 devices return
//! [`RotateOutcome::NoWork`]). The two families keep independent rotation rows,
//! secrets, and backoff, so a lagging or quarantined `service` account never
//! blocks BMC-root convergence or vice versa.
//!
//! A BMC password change never touches host power or the running OS, so this is
//! safe both for pool hosts (top-level [`ManagedHostState::RotatingBmc`]) and
//! under live tenancy (`Assigned/RotatingBmc`).

use carbide_credential_rotation::{
    BmcEndpoint, BmcRotationTick, RotateOutcome, rotate_bmc, rotate_dpu_bmc_service,
};
use carbide_uuid::machine::{AsMachineId, MachineId, MachineIdSubtypeTrait};
use model::machine::ManagedHostStateSnapshot;
use sqlx::PgConnection;
use state_controller::state_handler::StateHandlerError;

use crate::context::MachineStateHandlerServices;

/// The host BMC followed by each DPU BMC that exposes both a MAC and an IP. A
/// device missing either cannot be keyed or reached, so it is skipped (the
/// entry guard likewise never selects it).
///
/// This is also the set site-explorer is paused on for the duration of a
/// rotation and resumed on when it ends -- a superset of the devices any single
/// tick actually rotates, so a force flag that appears mid-rotation is already
/// covered and the resume set always matches what was suppressed.
pub(crate) fn managed_host_bmc_endpoints(
    mh: &ManagedHostStateSnapshot,
) -> impl Iterator<Item = BmcEndpoint> + '_ {
    BmcEndpoint::from_machine(&mh.host_snapshot)
        .into_iter()
        .chain(
            mh.dpu_snapshots
                .iter()
                .filter_map(BmcEndpoint::from_machine),
        )
}

/// `true` when the host BMC or any DPU BMC is behind the staged site-wide target
/// and not currently quarantined -- i.e. the machine should enter its
/// BMC-rotation state. Covers both the BMC-root family (host + every DPU) and
/// the DPU BMC `service` family (each DPU only); either lagging enters the state.
pub(crate) async fn bmc_rotation_needed(
    services: &MachineStateHandlerServices,
    mh: &ManagedHostStateSnapshot,
) -> Result<bool, StateHandlerError> {
    for endpoint in managed_host_bmc_endpoints(mh) {
        let needed = services
            .bmc_rotation_gate
            .rotation_needed(&services.db_pool, endpoint.device_mac)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre::eyre!("bmc rotation gate query: {e}"))
            })?;
        if needed {
            return Ok(true);
        }
    }

    // NICo only configures the service BMC account on BF4s and only BF4s have
    // rows in the device_credential_rotation table.
    for endpoint in mh
        .dpu_snapshots
        .iter()
        .filter_map(BmcEndpoint::from_machine)
    {
        let needed = services
            .dpu_bmc_service_rotation_gate
            .rotation_needed(&services.db_pool, endpoint.device_mac)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "dpu bmc service rotation gate query for {}: {e}",
                    endpoint.device_mac
                ))
            })?;
        if needed {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a Ready host should enter `ManagedHostState::RotatingBmc` now.
///
/// An operator force-converge request always wins -- the ops escape hatch is
/// honored even when the site-wide flag is off. Otherwise the passive gate
/// fires only when BMC rotation is enabled site-wide *and* a device lags the
/// staged target; the flag is checked first so a disabled site never runs the
/// per-device gate query.
pub(crate) async fn should_enter_bmc_rotation(
    services: &MachineStateHandlerServices,
    mh: &ManagedHostStateSnapshot,
) -> Result<bool, StateHandlerError> {
    if bmc_rotation_force_requested(mh) {
        return Ok(true);
    }
    Ok(services.site_config.bmc_rotation_enabled && bmc_rotation_needed(services, mh).await?)
}

/// Run one rotation tick over the host BMC and each DPU BMC. A device is rotated
/// when it is force-requested (operator escape hatch, which also bypasses that
/// device's backoff quarantine) or when passive site-wide rotation is enabled;
/// a device that is neither is left alone this tick. Because `force` is decided
/// per device, a forced DPU and a passively-lagging host converge together in
/// the same tick. Each device is independent (its own rotation row, secret, and
/// backoff), so one lagging or quarantined device never blocks the others.
/// Device-level failures are recorded and quarantined inside [`rotate_bmc`];
/// only transient bookkeeping errors surface here as [`BmcRotationTick::Retry`].
pub(crate) async fn rotate_managed_host_bmcs(
    services: &MachineStateHandlerServices,
    mh: &ManagedHostStateSnapshot,
) -> BmcRotationTick {
    let is_sitewide_bmc_rotation_enabled = services.site_config.bmc_rotation_enabled;
    let mut tick = BmcRotationTick::Settled;
    // Host first (BMC root only), then each DPU (BMC root *and* the BF4-only
    // `service` account).
    let host = std::iter::once((
        mh.host_snapshot.id.to_machine_id(),
        mh.host_snapshot.bmc_credential_rotation_requested,
        BmcEndpoint::from_machine(&mh.host_snapshot),
    ));
    let dpus = mh.dpu_snapshots.iter().map(|machine| {
        (
            machine.id.to_machine_id(),
            machine.bmc_credential_rotation_requested,
            BmcEndpoint::from_machine(machine),
        )
    });
    for (machine_id, force, endpoint) in host.chain(dpus) {
        let is_dpu = machine_id.is_dpu();
        if !force && !is_sitewide_bmc_rotation_enabled {
            continue;
        }
        match endpoint {
            Some(endpoint) => {
                // Fold each device outcome in, strongest wins: a store-reconcile
                // hold or a transient retry from any one BMC carries the whole
                // tick (the other endpoints still ran, and their per-device rows
                // persist). `merge` keeps a lagging store dominant so the tick
                // never settles out of the rotation state while any BMC's
                // hardware is ahead of its stored secret.
                tick = tick.merge(rotate_endpoint(services, endpoint.clone(), force).await);
                // A DPU additionally converges its `service` account under the
                // same force flag. Non-BF4 DPUs are never enrolled, so the engine
                // returns `NoWork` (a cheap, settled skip).
                if is_dpu {
                    tick = tick.merge(rotate_dpu_service_endpoint(services, endpoint, force).await);
                }
            }
            // A forced request announces a missing BMC so its one-shot flag
            // still clears; a passive sweep silently skips an unaddressable
            // device, exactly as `managed_host_bmc_endpoints` does.
            None if force => tracing::warn!(
                machine_id = %machine_id,
                "force-converge request on a machine with no addressable BMC; clearing the request without action"
            ),
            None => {}
        }
    }
    tick
}

/// The machines under this managed host (the host machine and each DPU machine)
/// that carry a pending operator force-converge request. Each such machine owns
/// exactly one BMC, so the flag's location names the target device -- no MAC is
/// needed in the request payload.
/// `true` when an operator has recorded a force-converge request against the host
/// machine or any of its DPU machines. Presence alone drives entry into
/// `RotatingBmc`.
pub(crate) fn bmc_rotation_force_requested(mh: &ManagedHostStateSnapshot) -> bool {
    mh.host_snapshot.bmc_credential_rotation_requested
        || mh
            .dpu_snapshots
            .iter()
            .any(|machine| machine.bmc_credential_rotation_requested)
}

/// The machine ids carrying a pending force-converge request, so the controller
/// can clear exactly those rows once the forced tick settles.
fn forced_bmc_machine_ids(mh: &ManagedHostStateSnapshot) -> impl Iterator<Item = MachineId> + '_ {
    mh.host_snapshot
        .bmc_credential_rotation_requested
        .then(|| mh.host_snapshot.id.into())
        .into_iter()
        .chain(
            mh.dpu_snapshots
                .iter()
                .filter(|machine| machine.bmc_credential_rotation_requested)
                .map(|machine| machine.id.into()),
        )
}

/// Clear the one-shot force-converge flag on exactly the machines that carried a
/// pending request *in this snapshot*, writing into the caller's transaction so
/// the clear commits atomically with the state transition.
///
/// Only the observed-forced machines are cleared -- never the whole managed host
/// -- so a request that lands mid-tick (after this snapshot was read, hence not
/// acted on this tick) survives for the next sweep instead of being silently
/// dropped. When nothing was forced this is a no-op. Call this only on a settled
/// tick, where the forced attempt genuinely fired.
pub(crate) async fn clear_forced_bmc_requests(
    txn: &mut PgConnection,
    mh: &ManagedHostStateSnapshot,
) -> Result<(), StateHandlerError> {
    for machine_id in forced_bmc_machine_ids(mh) {
        db::machine::clear_bmc_credential_rotation_requested(&mut *txn, &machine_id).await?;
    }
    Ok(())
}

/// Rotate a single BMC endpoint toward the staged target. `force` bypasses the
/// device's backoff quarantine (operator escape hatch). Returns
/// [`BmcRotationTick::Retry`] only on a transient bookkeeping error; device
/// faults are quarantined inside [`rotate_bmc`] and reported as `Settled`.
async fn rotate_endpoint(
    services: &MachineStateHandlerServices,
    endpoint: BmcEndpoint,
    force: bool,
) -> BmcRotationTick {
    // The precise `RedfishVendor` is not persisted for a machine BMC (the stored
    // hardware vendor is DMI-derived and too coarse), so defer resolution to the
    // engine's probe, which runs inside the same quarantine-on-failure envelope
    // as the rotation and reuses its credential candidates.
    let target = endpoint.into_target_probing_vendor();
    match rotate_bmc(
        &services.db_pool,
        services.credential_manager.as_ref(),
        services.bmc_credential_ops.as_ref(),
        &target,
        force,
    )
    .await
    {
        Ok(RotateOutcome::Converged) => {
            tracing::info!(mac = %target.device_mac, force, "BMC converged to site-wide rotation target");
            BmcRotationTick::Settled
        }
        Ok(RotateOutcome::Quarantined { until }) => {
            tracing::warn!(
                mac = %target.device_mac,
                %until,
                "BMC rotation attempt failed; quarantined until backoff elapses"
            );
            BmcRotationTick::Settled
        }
        Ok(RotateOutcome::CredentialStoreReconcilePending) => {
            tracing::warn!(
                mac = %target.device_mac,
                "BMC hardware changed but the per-device secret persist failed; holding rotation until the credential store reconciles"
            );
            BmcRotationTick::WaitForCredentialStoreReconcile
        }
        Ok(RotateOutcome::NoWork) => BmcRotationTick::Settled,
        Err(e) => {
            tracing::warn!(
                mac = %target.device_mac,
                error = %e,
                "transient BMC rotation bookkeeping failure; will retry the tick"
            );
            BmcRotationTick::Retry
        }
    }
}

/// Rotate a single DPU BMC's `service` account toward the staged
/// `dpu_bmc_service` target. `force` bypasses the device's backoff quarantine
/// (operator escape hatch). A non-BF4 DPU (or any unenrolled device) returns
/// [`RotateOutcome::NoWork`], which is `Settled`. Returns
/// [`BmcRotationTick::Retry`] only on a transient bookkeeping error; device
/// faults are quarantined inside [`rotate_dpu_bmc_service`] and reported as
/// `Settled`. The engine writes no per-device secret, so it never returns
/// [`RotateOutcome::CredentialStoreReconcilePending`].
async fn rotate_dpu_service_endpoint(
    services: &MachineStateHandlerServices,
    endpoint: BmcEndpoint,
    force: bool,
) -> BmcRotationTick {
    let mac = endpoint.device_mac;
    match rotate_dpu_bmc_service(
        &services.db_pool,
        services.credential_manager.as_ref(),
        services.bmc_credential_ops.as_ref(),
        &endpoint,
        force,
    )
    .await
    {
        Ok(RotateOutcome::Converged) => {
            tracing::info!(%mac, force, "DPU BMC service account converged to site-wide rotation target");
            BmcRotationTick::Settled
        }
        Ok(RotateOutcome::Quarantined { until }) => {
            tracing::warn!(
                %mac,
                %until,
                "DPU BMC service rotation attempt failed; quarantined until backoff elapses"
            );
            BmcRotationTick::Settled
        }
        // No per-device secret write means the hardware can never be ahead of the
        // store, so this variant is unreachable here; treat it as settled rather
        // than holding the state.
        Ok(RotateOutcome::CredentialStoreReconcilePending) => BmcRotationTick::Settled,
        Ok(RotateOutcome::NoWork) => BmcRotationTick::Settled,
        Err(e) => {
            tracing::warn!(
                %mac,
                error = %e,
                "transient DPU BMC service rotation bookkeeping failure; will retry the tick"
            );
            BmcRotationTick::Retry
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use mac_address::MacAddress;
    use model::test_support::machine_snapshot::managed_host_state_snapshot;

    use super::*;

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0, 0, 0, 0, last])
    }

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    /// The fixture bundles a host with two DPUs sharing one BMC address; give
    /// each a distinct `(mac, ip)` so endpoint ordering and per-device selection
    /// are observable.
    fn snapshot_with_distinct_bmcs() -> ManagedHostStateSnapshot {
        let mut mh = managed_host_state_snapshot();
        mh.host_snapshot.status.bmc_info.mac = Some(mac(1));
        mh.host_snapshot.status.bmc_info.ip = Some(ip(1));
        mh.dpu_snapshots[0].status.bmc_info.mac = Some(mac(2));
        mh.dpu_snapshots[0].status.bmc_info.ip = Some(ip(2));
        mh.dpu_snapshots[1].status.bmc_info.mac = Some(mac(3));
        mh.dpu_snapshots[1].status.bmc_info.ip = Some(ip(3));
        mh
    }

    fn selected_macs(mh: &ManagedHostStateSnapshot) -> Vec<MacAddress> {
        managed_host_bmc_endpoints(mh)
            .map(|e| e.device_mac)
            .collect()
    }

    #[test]
    fn endpoints_are_host_then_dpus_in_order() {
        assert_eq!(
            selected_macs(&snapshot_with_distinct_bmcs()),
            vec![mac(1), mac(2), mac(3)],
            "the host BMC leads, followed by each DPU BMC in order"
        );
    }

    #[test]
    fn a_device_missing_its_mac_or_ip_is_skipped() {
        // A BMC with no MAC cannot key its rotation row/secret; one with no IP
        // cannot be reached. Either omission drops just that device, never the
        // rest.
        let mut mh = snapshot_with_distinct_bmcs();
        mh.dpu_snapshots[0].status.bmc_info.mac = None;
        mh.dpu_snapshots[1].status.bmc_info.ip = None;

        assert_eq!(
            selected_macs(&mh),
            vec![mac(1)],
            "only the fully-addressable host BMC remains"
        );
    }

    #[test]
    fn a_host_missing_its_bmc_mac_yields_only_dpus() {
        let mut mh = snapshot_with_distinct_bmcs();
        mh.host_snapshot.status.bmc_info.mac = None;

        assert_eq!(selected_macs(&mh), vec![mac(2), mac(3)]);
    }

    #[test]
    fn bmc_endpoint_carries_resolved_host_and_port() {
        let mh = snapshot_with_distinct_bmcs();
        let endpoint =
            BmcEndpoint::from_machine(&mh.host_snapshot).expect("host BMC is fully addressable");
        assert_eq!(endpoint.device_mac, mac(1));
        assert_eq!(endpoint.host, ip(1).to_string());
        // The fixture seeds the standard Redfish port.
        assert_eq!(endpoint.port, Some(443));
    }
}
