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
//! Resolving how to target a host's boot interface for Redfish setup calls.

use carbide_redfish::boot_interface::BootInterfaceTarget;
use mac_address::MacAddress;
use model::machine::{ManagedHostStateSnapshot, pick_boot_prediction};
use model::machine_boot_interface::MachineBootInterface;
use model::predicted_machine_interface::PredictedMachineInterface;

/// Resolve how to target this host's boot interface for Redfish setup calls.
///
/// The host's own `machine_interface` row wins the moment it exists: when that
/// row has a captured `boot_interface_id`, the full pair is supplied to
/// `libredfish` in one operation; otherwise the target contains only the MAC.
/// Both identifiers come from the same row, so the pair cannot name a different
/// interface than the MAC.
///
/// Before that first DHCP lease creates a row -- the window a zero-DPU or
/// NIC-mode host sits in, since it gets no primary row at ingestion -- the host's
/// `predictions` answer instead, via `pick_boot_prediction` (the declared
/// primary, else the sole non-underlay prediction). The predicted MAC and
/// recorded id form the same pair the real row will hold once the lease promotes
/// it.
///
/// Returns `None` only when the host has no boot interface at all -- no row and
/// no usable prediction (e.g. only the BMC has been discovered, or several
/// predictions with no declared primary).
pub fn boot_interface_target(
    mh_snapshot: &ManagedHostStateSnapshot,
    predictions: &[PredictedMachineInterface],
) -> Option<BootInterfaceTarget> {
    boot_interface_target_from(
        mh_snapshot.boot_interface(),
        mh_snapshot.boot_interface_mac(),
        predictions,
    )
}

/// The boot-target decision, split out from the snapshot lookup so it can be
/// unit-tested directly without constructing a full `ManagedHostStateSnapshot`
/// -- the same split `pick_boot_interface_mac`/`_pair` use in api-model. The
/// host's own row wins (its captured pair, else its MAC alone); only when it
/// owns no boot row does the predicted boot interface answer.
fn boot_interface_target_from(
    row_pair: Option<MachineBootInterface>,
    row_mac: Option<MacAddress>,
    predictions: &[PredictedMachineInterface],
) -> Option<BootInterfaceTarget> {
    if let Some(pair) = row_pair {
        return Some(BootInterfaceTarget::Pair(pair));
    }
    if let Some(mac) = row_mac {
        return Some(BootInterfaceTarget::MacOnly(mac));
    }
    // No real row yet -- fall back to the host's predicted boot interface.
    pick_boot_prediction(predictions).map(|prediction| {
        prediction.boot_interface().map_or(
            BootInterfaceTarget::MacOnly(prediction.mac_address),
            BootInterfaceTarget::Pair,
        )
    })
}

/// What a Redfish boot step should do with a host's boot interface.
///
/// Separates "not ready yet" from "broken". A zero-DPU host (policy `Ignore` or
/// `Nic`) boots from a plain NIC that takes its first HostInband lease only
/// after the host comes up, so until then it has no boot interface to resolve --
/// the controller should wait, not fail. A host with managed DPUs always has its
/// DPU-facing primary set at promotion, so a missing boot interface there is a
/// genuine fault.
#[derive(Debug)]
pub enum BootInterfaceResolution {
    /// The boot interface resolved; target it.
    Ready(BootInterfaceTarget),
    /// A zero-DPU host with no boot interface yet -- neither a real row nor a
    /// usable prediction -- so wait for its boot NIC to appear.
    AwaitingNic,
    /// A host that should already have a boot interface is missing one.
    Missing,
}

impl BootInterfaceResolution {
    /// Convert this resolution for BIOS paths that preserve an untargeted
    /// Redfish fallback when no interface is usable.
    ///
    /// This intentionally erases the distinction between `AwaitingNic` and
    /// `Missing`. Callers that must wait or fault retain and match the full
    /// resolution instead.
    pub(crate) fn into_target(self) -> Option<BootInterfaceTarget> {
        match self {
            Self::Ready(target) => Some(target),
            Self::AwaitingNic | Self::Missing => None,
        }
    }
}

/// Resolve this host's boot interface for a Redfish boot step, classifying a
/// missing one as either "wait for the NIC" (zero-DPU) or "fault".
pub fn resolve_boot_interface(
    mh_snapshot: &ManagedHostStateSnapshot,
    predictions: &[PredictedMachineInterface],
) -> BootInterfaceResolution {
    classify_boot_interface(
        boot_interface_target(mh_snapshot, predictions),
        mh_snapshot.has_managed_dpus(),
    )
}

/// The decision behind [`resolve_boot_interface`], split out from the snapshot
/// lookup so it can be unit-tested directly.
fn classify_boot_interface(
    boot_interface: Option<BootInterfaceTarget>,
    has_managed_dpus: bool,
) -> BootInterfaceResolution {
    match boot_interface {
        Some(target) => BootInterfaceResolution::Ready(target),
        None if !has_managed_dpus => BootInterfaceResolution::AwaitingNic,
        None => BootInterfaceResolution::Missing,
    }
}

#[cfg(test)]
mod tests {
    use mac_address::MacAddress;
    use model::network_segment::NetworkSegmentType;

    use super::*;

    fn prediction(mac: &str, boot_interface_id: Option<&str>) -> PredictedMachineInterface {
        PredictedMachineInterface {
            id: uuid::Uuid::nil(),
            machine_id: "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng"
                .parse()
                .unwrap(),
            mac_address: mac.parse().unwrap(),
            expected_network_segment_type: NetworkSegmentType::HostInband,
            boot_interface_id: boot_interface_id.map(String::from),
            primary_interface: false,
        }
    }

    // The host's own row wins over any prediction: a captured id gives the pair.
    #[test]
    fn boot_target_prefers_the_owned_row_over_predictions() {
        let pair = MachineBootInterface {
            mac_address: "10:00:00:00:00:01".parse().unwrap(),
            interface_id: "NIC.Slot.5-1".to_string(),
        };
        assert_eq!(
            boot_interface_target_from(
                Some(pair.clone()),
                Some("10:00:00:00:00:01".parse().unwrap()),
                &[prediction("20:00:00:00:00:01", Some("NIC.Embedded.1-1-1"))],
            ),
            Some(BootInterfaceTarget::Pair(pair)),
        );
    }

    // No owned row yet: the predicted boot interface answers (pre-first-lease),
    // as a full pair when the prediction carries the Redfish id.
    #[test]
    fn boot_target_falls_back_to_the_prediction_before_the_first_lease() {
        assert_eq!(
            boot_interface_target_from(
                None,
                None,
                &[prediction("20:00:00:00:00:01", Some("NIC.Embedded.1-1-1"))],
            ),
            Some(BootInterfaceTarget::Pair(MachineBootInterface {
                mac_address: "20:00:00:00:00:01".parse().unwrap(),
                interface_id: "NIC.Embedded.1-1-1".to_string(),
            })),
        );
    }

    // A prediction with no captured id targets the MAC alone.
    #[test]
    fn boot_target_prediction_without_id_is_mac_only() {
        assert_eq!(
            boot_interface_target_from(None, None, &[prediction("20:00:00:00:00:01", None)]),
            Some(BootInterfaceTarget::MacOnly(
                "20:00:00:00:00:01".parse().unwrap()
            )),
        );
    }

    // No row and no usable prediction: nothing to target, so the caller waits.
    #[test]
    fn boot_target_is_none_without_row_or_prediction() {
        assert_eq!(boot_interface_target_from(None, None, &[]), None);
    }

    #[test]
    fn classify_waits_for_a_zero_dpu_host_without_a_boot_interface() {
        // The zero-DPU host's boot NIC has not taken its first lease yet: wait
        // for it instead of faulting.
        assert!(matches!(
            classify_boot_interface(None, false),
            BootInterfaceResolution::AwaitingNic
        ));
    }

    #[test]
    fn classify_faults_when_a_dpu_host_has_no_boot_interface() {
        // A host with managed DPUs always has its DPU-facing primary set at
        // promotion, so a missing boot interface is a real fault.
        assert!(matches!(
            classify_boot_interface(None, true),
            BootInterfaceResolution::Missing
        ));
    }

    #[test]
    fn classify_uses_the_resolved_interface_when_present() {
        let target = BootInterfaceTarget::MacOnly(MacAddress::new([0, 0, 0, 0, 0, 1]));
        assert!(matches!(
            classify_boot_interface(Some(target), false),
            BootInterfaceResolution::Ready(_)
        ));
    }

    #[test]
    fn only_ready_resolution_exposes_a_target() {
        let target = BootInterfaceTarget::MacOnly(MacAddress::new([0, 0, 0, 0, 0, 1]));
        for (resolution, expected) in [
            (BootInterfaceResolution::Ready(target.clone()), Some(target)),
            (BootInterfaceResolution::AwaitingNic, None),
            (BootInterfaceResolution::Missing, None),
        ] {
            assert_eq!(resolution.into_target(), expected);
        }
    }
}
