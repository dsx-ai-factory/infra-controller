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

//! NIC firmware configuration and DPA binding evidence frozen during allocation.

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use carbide_uuid::dpa_interface::DpaInterfaceId;
use carbide_uuid::machine::HostMachineId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use crate::ConfigValidationError;
use crate::dpa_interface::DpaInterface;
use crate::instance::config::spx::InstanceSpxAttachment;
use crate::nic_firmware::{NicFirmwareCardSelection, NicFirmwareProfile, NicFirmwareProfileId};
use crate::tenant::{TenantNicFirmwareConfig, TenantOrganizationId};

/// `InstanceNicFirmwareSnapshot` preserves allocation inputs independently of
/// instance configuration replaced by tenants and later operator edits.
/// `update_instance_spx_config` reads its bindings before persisting attachment edits.
/// This value is stored as JSON; readers must continue decoding previously written
/// field names, selector variants and nested profile/configuration representations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstanceNicFirmwareSnapshot {
    /// Organization requested by the allocation, including when no tenant row exists.
    pub tenant_organization_id: TenantOrganizationId,
    /// Tenant selection and version at capture; `None` records a missing tenant row.
    pub tenant: Option<TenantNicFirmwareConfig>,
    /// Machine configuration version shared by the captured machine and card selections.
    pub machine_config_version: ConfigVersion,
    /// Captured machine selection, or `None` when no machine profile was selected.
    pub machine_profile_id: Option<NicFirmwareProfileId>,
    /// Captured card selectors, including unresolved selectors; order has no meaning.
    pub card_selections: Vec<NicFirmwareCardSelection>,
    /// Complete definitions and revisions referenced by these captured selections.
    /// Retaining all entries preserves requirements even when hardware is unresolved.
    pub profiles: Vec<NicFirmwareProfile>,
    /// Evidence for every selected DPA attachment, in the mapper's output order.
    /// An empty list means no selected attachments, not successful firmware preparation.
    pub targets: Vec<NicFirmwareAllocationTarget>,
}

/// `NicFirmwareAllocationTarget` records the DPA row selected for an attachment.
/// Its observation does not establish physical card identity, freshness, flash/reset
/// scope or permission to mutate hardware. Generic NIC mutation remains blocked.
/// Its JSON representation must remain readable for the lifetime of the allocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NicFirmwareAllocationTarget {
    /// Attachment at allocation, including its logical selector and original partition.
    /// Later partition edits do not replace this historical configuration.
    pub attachment: InstanceSpxAttachment,
    /// DPA row selected by the existing SPX mapper.
    pub dpa_interface_id: DpaInterfaceId,
    /// Host recorded on the selected DPA row.
    pub machine_id: HostMachineId,
    /// MAC assigned from the selected row.
    pub mac_address: MacAddress,
    /// PCI locator recorded on the selected row.
    pub pci_name: String,
    /// Device facts stored on that row, or `None` when no report was stored.
    pub observed_device: Option<MlxDeviceInfo>,
    /// DPA row's report receipt time.
    pub observed_at: Option<DateTime<Utc>>,
    /// Diagnostic explaining why generic firmware mutation is blocked.
    /// This text is not a machine-readable status code.
    pub blocked_reason: String,
}

impl NicFirmwareAllocationTarget {
    /// `from_binding` freezes evidence from the row the SPX mapper actually selected.
    pub fn from_binding(attachment: InstanceSpxAttachment, interface: &DpaInterface) -> Self {
        Self {
            attachment,
            dpa_interface_id: interface.id,
            machine_id: interface.machine_id,
            mac_address: interface.mac_address,
            pci_name: interface.pci_name.clone(),
            observed_device: interface.device_info.clone(),
            observed_at: interface.device_info_ts,
            blocked_reason: "physical NIC update target is not qualified".to_string(),
        }
    }
}

impl InstanceNicFirmwareSnapshot {
    /// `validate_bindings` prevents an SPX edit from redirecting a frozen target.
    /// Callers first run the existing mapper, which validates logical selectors.
    /// Retained selectors must keep their DPA row, host, MAC and PCI locator.
    /// Removals and partition changes are allowed without changing this snapshot.
    /// New selectors are rejected when the frozen tenant selected a profile;
    /// otherwise, they retain the approved baseline without new firmware work.
    pub fn validate_bindings(
        &self,
        proposed: &[NicFirmwareAllocationTarget],
    ) -> Result<(), ConfigValidationError> {
        for target in proposed {
            let original = self.targets.iter().find(|original| {
                original.attachment.device == target.attachment.device
                    && original.attachment.device_instance == target.attachment.device_instance
            });
            let Some(original) = original else {
                if self
                    .tenant
                    .as_ref()
                    .is_some_and(|tenant| tenant.profile_id.is_some())
                {
                    return Err(ConfigValidationError::InvalidValue(format!(
                        "SPX attachment for device {} instance {} would introduce a frozen NIC firmware requirement",
                        target.attachment.device, target.attachment.device_instance
                    )));
                }
                continue;
            };
            if original.dpa_interface_id != target.dpa_interface_id
                || original.machine_id != target.machine_id
                || original.mac_address != target.mac_address
                || original.pci_name != target.pci_name
            {
                return Err(ConfigValidationError::InvalidValue(format!(
                    "SPX attachment for device {} instance {} would change its frozen NIC firmware target binding",
                    target.attachment.device, target.attachment.device_instance
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios};
    use carbide_uuid::spx::SpxPartitionId;

    use super::*;
    use crate::instance::config::spx::SpxAttachmentType;

    /// `target` provides binding evidence without observed hardware, so guard
    /// tests cannot accidentally depend on physical mutation qualification.
    fn target(device_instance: u32) -> NicFirmwareAllocationTarget {
        NicFirmwareAllocationTarget {
            attachment: InstanceSpxAttachment {
                device: "SuperNIC".to_string(),
                device_instance,
                mac_address: Some("02:00:00:00:00:01".to_string()),
                spx_partition_id: SpxPartitionId::new(),
                attachment_type: SpxAttachmentType::Physical,
                attachment_vf: None,
                attachment_ovs: None,
            },
            dpa_interface_id: DpaInterfaceId::new(),
            machine_id: crate::test_support::machine_snapshot::host_machine_id().into(),
            mac_address: MacAddress::new([2, 0, 0, 0, 0, 1]),
            pci_name: "01:00.0".to_string(),
            observed_device: None,
            observed_at: None,
            blocked_reason: "physical NIC update target is not qualified".to_string(),
        }
    }

    /// `snapshot` freezes one target with an absent tenant; each test supplies
    /// only the requirement or binding change relevant to its contract.
    fn snapshot(target: NicFirmwareAllocationTarget) -> InstanceNicFirmwareSnapshot {
        InstanceNicFirmwareSnapshot {
            tenant_organization_id: "tenant-a".parse().expect("valid tenant"),
            tenant: None,
            machine_config_version: ConfigVersion::initial(),
            machine_profile_id: None,
            card_selections: vec![],
            profiles: vec![],
            targets: vec![target],
        }
    }

    /// Each saved identity field independently prevents an unchanged logical
    /// attachment from accepting different binding evidence.
    #[test]
    fn retained_bindings_reject_changed_identity() {
        let original = target(0);
        let frozen = snapshot(original.clone());
        type BindingMutation = fn(&mut NicFirmwareAllocationTarget);
        let cases: [(&str, BindingMutation); 4] = [
            // Recreating a DPA row cannot reuse the old allocation's binding.
            ("different DPA row", |target| {
                target.dpa_interface_id = DpaInterfaceId::new()
            }),
            // A reused row identity cannot redirect the attachment to another host.
            ("different host", |target| {
                target.machine_id = "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30"
                    .parse()
                    .expect("valid other host");
            }),
            // The row ID alone cannot hide a change to its assigned address.
            ("different MAC", |target| {
                target.mac_address = MacAddress::new([2, 0, 0, 0, 0, 2])
            }),
            // A changed locator cannot silently retarget the retained attachment.
            ("different PCI locator", |target| {
                target.pci_name = "02:00.0".to_string()
            }),
        ];
        check_cases(
            cases.into_iter().map(|(scenario, mutate)| Case {
                scenario,
                input: mutate,
                expect: FailsWith(
                    "invalid value: SPX attachment for device SuperNIC instance 0 would change its frozen NIC firmware target binding".to_string(),
                ),
            }),
            |mutate| {
                let mut proposed = original.clone();
                mutate(&mut proposed);
                frozen.validate_bindings(&[proposed]).map_err(|error| error.to_string())
            },
        );
    }

    /// A tenant edit cannot erase a captured requirement, while partition edits
    /// and removals need no new firmware preparation.
    #[test]
    fn binding_edits_use_frozen_tenant_presence() {
        let original = target(0);
        let absent = snapshot(original.clone());
        let mut unselected = absent.clone();
        unselected.tenant = Some(TenantNicFirmwareConfig {
            organization_id: unselected.tenant_organization_id.clone(),
            profile_id: None,
            version: ConfigVersion::initial(),
        });
        let mut required = unselected.clone();
        required
            .tenant
            .as_mut()
            .expect("tenant is present")
            .profile_id = Some("required".parse().expect("valid profile"));
        required.targets.clear();
        let mut changed_partition = original;
        changed_partition.attachment.spx_partition_id = SpxPartitionId::new();
        let new_target = target(1);

        scenarios!(
            run = |(frozen, proposed): (&InstanceNicFirmwareSnapshot, Vec<NicFirmwareAllocationTarget>)| {
                frozen.validate_bindings(&proposed).map_err(|error| error.to_string())
            };
            "no new firmware work" {
                // A partition edit retains the allocation's selected hardware.
                (&unselected, vec![changed_partition]) => Yields(()),
                // Removing all attachments does not prepare another target.
                (&unselected, vec![]) => Yields(()),
                // Captured absence lets added attachments keep the approved baseline.
                (&absent, vec![new_target.clone()]) => Yields(()),
                // A captured tenant row without a profile also permits the baseline.
                (&unselected, vec![new_target.clone()]) => Yields(()),
            }
            "new logical target cannot bypass a frozen tenant requirement" {
                // The saved requirement still applies after the live tenant is edited.
                (&required, vec![new_target]) => Fails,
            }
        );
    }

    /// Fixed stored JSON catches decoding changes that serializing the current
    /// types first could hide, including nested profile and observation formats.
    #[test]
    fn stored_snapshot_json_remains_readable() {
        let stored = r#"{
            "tenant_organization_id": "tenant-a",
            "tenant": {
                "organization_id": "tenant-a",
                "profile_id": "frozen-profile",
                "version": "V3-T1000000"
            },
            "machine_config_version": "V7-T2000000",
            "machine_profile_id": "frozen-profile",
            "card_selections": [{
                "selector": {"BaseGuid": "opaque-Base_Guid"},
                "profile_id": "frozen-profile"
            }],
            "profiles": [{
                "id": "frozen-profile",
                "version": "V5-T3000000",
                "config": {"entries": [{
                    "firmware": {"part_number": "PN", "psid": "PSID", "version": "1.0"},
                    "image": {
                        "url": "https://firmware.example/image.bin",
                        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "device_config": {
                        "url": "https://firmware.example/config.bin",
                        "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    },
                    "approach": "Scout"
                }]}
            }],
            "targets": [{
                "attachment": {
                    "device": "SuperNIC",
                    "device_instance": 0,
                    "mac_address": "02:00:00:00:00:01",
                    "spx_partition_id": "a63eb85f-c196-4f91-bc16-cdf9ce0e03e9",
                    "attachment_type": "Physical",
                    "attachment_vf": null,
                    "attachment_ovs": null
                },
                "dpa_interface_id": "1724c9e4-3285-45e0-a38c-ff24989129a5",
                "machine_id": "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
                "mac_address": "02:00:00:00:00:01",
                "pci_name": "01:00.0",
                "observed_device": {
                    "pci_name": "01:00.0",
                    "device_type": "ConnectX-8",
                    "psid": "PSID",
                    "device_description": "SuperNIC",
                    "part_number": "PN",
                    "fw_version_current": "2.0",
                    "pxe_version_current": null,
                    "uefi_version_current": null,
                    "uefi_version_virtio_blk_current": null,
                    "uefi_version_virtio_net_current": null,
                    "base_mac": "02:00:00:00:00:01",
                    "base_guid": "opaque-Base_Guid",
                    "status": null
                },
                "observed_at": "2026-10-19T00:00:00Z",
                "blocked_reason": "physical NIC update target is not qualified"
            }]
        }"#;
        let decoded: InstanceNicFirmwareSnapshot =
            serde_json::from_str(stored).expect("decode stored allocation snapshot");
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize decoded snapshot"),
            serde_json::from_str::<serde_json::Value>(stored).expect("valid stored JSON"),
            "every captured input and binding fact must retain its stored representation"
        );
    }
}
