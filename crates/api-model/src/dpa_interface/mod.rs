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

use std::convert::TryFrom;
use std::fmt::Display;
use std::net::IpAddr;

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use carbide_libmlx_model::firmware::result::FirmwareFlashReport;
use carbide_uuid::dpa_interface::DpaInterfaceId;
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::spx::NULL_SPX_PARTITION_ID;
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::StateSla;
use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::instance::snapshot::InstanceSnapshot;
use crate::machine::spx::MachineSpxStatusObservation;
use crate::state_history::StateHistoryRecord;

mod slas;

/// Interface type for the DPA interface
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "dpa_interface_type")]
pub enum DpaInterfaceType {
    Svpc,
    Astra,
}

/// State of a dpa interface as tracked by the controller
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DpaInterfaceControllerState {
    /// Initial state
    Provisioning,
    /// The dpa interface is ready. It has been configured with a zero VNI
    Ready,
    /// Unlock the card as part of the tenant allocation-flow (applying f/w update if necessary)
    Unlocking,
    /// Apply firmware to the SuperNIC, in which we will send down
    /// a FirmwareFlasherProfile matching the device P/N + PSID,
    /// with the target version. We may also send down a "None"
    /// profile, which is effectively a noop; scout will report
    /// back saying it successfully applied nothing.
    ///
    /// The API can also choose to just skip to the ApplyProfile
    /// state in the case of there being None to send to scout,
    /// but scout is expected to successfully reply "Ok" if it
    /// gets a None.
    ApplyFirmware,
    /// Apply mlx profile
    ApplyProfile,
    /// Lock the card as part of the tenant allocation-flow
    Locking,
    /// The Dpa Interface has been configured with a non-zero VNI
    Assigned,
    /// Tenant-free NIC lockdown input-key-material (IKM) rotation, phase one:
    /// unlock the card. Like the assignment `Unlocking`, it unlocks at the IKM
    /// version the card is currently locked under -- it reuses the same
    /// `build_unlock_command`, because a card must always be unlocked with the
    /// key it was locked with. That shared unlock is not what makes this a
    /// separate state; its gating and continuation are. It is entered only from
    /// the idle-only `ManagedHostState::RotatingNicLockdown` host state (never
    /// under active tenancy), and on observed unlock it clears the card's
    /// convergence bookkeeping (`record_device_unlocked`) and advances to
    /// `RotateKeyLocking` (relock at the site-wide target) rather than into the
    /// firmware/profile assignment pipeline.
    RotateKeyUnlocking,
    /// Tenant-free NIC lockdown input-key-material (IKM) rotation flow, phase 2: lock the NIC with the most recent
    /// site-wide target IKM version, completing the rekey and returning the card to `Ready`.
    RotateKeyLocking,
}

#[derive(Default)]
pub struct DpaSearchConfig {
    pub only_svpc: bool,
    pub only_astra: bool,
}

impl Display for DpaInterfaceControllerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpaInterfaceNetworkConfig {
    pub use_admin_network: Option<bool>,
    pub quarantine_state: Option<DpaInterfaceQuarantineState>,
}

impl Display for DpaInterfaceNetworkConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Default for DpaInterfaceNetworkConfig {
    fn default() -> Self {
        DpaInterfaceNetworkConfig {
            use_admin_network: Some(true),
            quarantine_state: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpaInterfaceQuarantineState {
    pub reason: Option<String>,
    pub mode: DpaInterfaceQuarantineMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DpaInterfaceQuarantineMode {
    BlockAllTraffic,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum DpaLockMode {
    Unlocked,
    Locked,
}

impl TryFrom<i32> for DpaLockMode {
    type Error = &'static str;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(DpaLockMode::Locked),
            2 => Ok(DpaLockMode::Unlocked),
            _ => Err("Invalid value for DpaLockMode"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CardState {
    pub lockmode: Option<DpaLockMode>,
    pub profile: Option<String>,
    pub profile_synced: Option<bool>,

    #[serde(default)]
    // firmware_report contains the latest FirmwareFlashReport as
    // fed back from scout after receiving a FirmwareFlashProfile
    // to apply as part of the ApplyFirmware state + OpCode
    // workflow. This report will let us know if the firmware
    // flash occurred, as well as a number of optional bits
    // of feedback (e.g. if a reset was configured, did it happen,
    // if a version verification was configured, did it happen,
    // etc). This is useful for metrics, verification, and general
    // transparency via logging or other mechanisms.
    pub firmware_report: Option<FirmwareFlashReport>,
}

impl Display for CardState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

/// Returns the SLA for the current state
/// We can be in the Provisioning, Ready and Assigned states
/// for a long time.
pub fn state_sla(state: &DpaInterfaceControllerState, state_version: &ConfigVersion) -> StateSla {
    let time_in_state = chrono::Utc::now()
        .signed_duration_since(state_version.timestamp())
        .to_std()
        .unwrap_or(std::time::Duration::from_secs(60 * 60 * 24));
    match state {
        DpaInterfaceControllerState::Provisioning => StateSla::no_sla(),
        DpaInterfaceControllerState::Ready => StateSla::no_sla(),
        DpaInterfaceControllerState::Locking => StateSla::with_sla(slas::LOCKING, time_in_state),
        DpaInterfaceControllerState::ApplyFirmware => {
            StateSla::with_sla(slas::APPLY_FIRMWARE, time_in_state)
        }
        DpaInterfaceControllerState::ApplyProfile => {
            StateSla::with_sla(slas::APPLY_PROFILE, time_in_state)
        }
        DpaInterfaceControllerState::Unlocking => {
            StateSla::with_sla(slas::UNLOCKING, time_in_state)
        }
        DpaInterfaceControllerState::Assigned => StateSla::no_sla(),
        DpaInterfaceControllerState::RotateKeyUnlocking => {
            StateSla::with_sla(slas::ROTATE_KEY_UNLOCKING, time_in_state)
        }
        DpaInterfaceControllerState::RotateKeyLocking => {
            StateSla::with_sla(slas::ROTATE_KEY_LOCKING, time_in_state)
        }
    }
}

#[derive(Clone, Debug)]
pub struct DpaInterface {
    pub id: DpaInterfaceId,
    pub machine_id: HostMachineId,

    pub mac_address: MacAddress,
    pub pci_name: String,

    pub underlay_ip: Option<IpAddr>,
    pub overlay_ip: Option<IpAddr>,

    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,

    pub controller_state: Versioned<DpaInterfaceControllerState>,

    // Last time we issued a heartbeat command to the DPA
    pub last_hb_time: DateTime<Utc>,

    /// The result of the last attempt to change state
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,

    pub network_config: Versioned<DpaInterfaceNetworkConfig>,

    pub card_state: Option<CardState>,

    // device_info and its corresponding timestamp are used to
    // keep track of the latest MlxDeviceInfo received by scout
    // for the target Mellanox device. This contains information
    // like the part number, PSID, firmware version(s), MAC address,
    // etc. We store the received timestamp alongside it to detect
    // if we're acting on potentially stale MlxDeviceInfo data.
    pub device_info: Option<MlxDeviceInfo>,
    pub device_info_ts: Option<DateTime<Utc>>,

    // mlxconfig_profile is the name of an MlxConfigProfile from
    // the mlx-config-profiles config map. When set, this profile
    // will be applied to the device during the ApplyProfile state.
    // When None, ApplyProfile will simply perform an mlxconfig
    // reset and not apply any subsequent defaults, ensuring the
    // card is back to stock before the next tenancy.
    pub mlxconfig_profile: Option<String>,

    pub history: Vec<StateHistoryRecord>,

    pub device_description: Option<String>,

    pub interface_type: DpaInterfaceType,
}

#[derive(Clone, Debug)]
pub struct NewDpaInterface {
    pub machine_id: HostMachineId,
    pub mac_address: MacAddress,
    pub device_type: String,
    pub pci_name: String,
    pub device_description: Option<String>,
    pub interface_type: DpaInterfaceType,
}

impl NewDpaInterface {
    /// from_device_info builds a NewDpaInterface instance for a given
    /// MachineId from a given MlxDeviceInfo, since it contains everything
    /// we use as input for an interface.
    ///
    /// Right now the only reason this would fail is if base_mac was unset,
    /// at which point we'll just return None, meaning the caller knows that
    /// the base_mac was unset. Since the mac_address is the latter half of
    /// what is effectively a (machine_id, mac_address) compound primary key,
    /// it's kind of important to have.
    pub fn from_device_info(
        machine_id: HostMachineId,
        base_mac: Option<MacAddress>,
        device_type: String,
        pci_name: String,
        device_description: Option<String>,
        interface_type: DpaInterfaceType,
    ) -> Option<Self> {
        Some(Self {
            machine_id,
            mac_address: base_mac?,
            device_type,
            pci_name,
            device_description,
            interface_type,
        })
    }
}

impl DpaInterface {
    pub fn use_admin_network(&self) -> bool {
        self.network_config.use_admin_network.unwrap_or(true)
    }

    pub fn get_machine_id(&self) -> HostMachineId {
        self.machine_id
    }

    // If the DPA machine is an instance, the config version sent to the card will
    // the spx_config_version of the instance.
    // If the DPA machine is a managed host, the config version sent to the card will
    // be the network_config.version of the DPA interface.
    pub fn managed_host_network_config_version_synced(
        &self,
        instance: &Option<InstanceSnapshot>,
        spx_status_observation: &Option<MachineSpxStatusObservation>,
    ) -> bool {
        // If the instance has no SPX attachments and the interface has reached
        // the Assigned state, there is nothing to sync.
        // Astra DPAs move to Assigned right away, as they don't have to go through
        // the Unlock/Lock sequence. SVPC DPAs go through that sequence and then
        // reach Assigned state.
        if let Some(instance) = instance
            && instance.config.spxconfig.spx_attachments.is_empty()
            && self.controller_state.value == DpaInterfaceControllerState::Assigned
        {
            return true;
        }

        let mut dpa_expected_version = self.network_config.version;

        // If we haven't yet seen any observations, we are not synced
        let Some(spx_status_observation) = spx_status_observation else {
            tracing::info!(
                dpa_interface_id = %self.id,
                "DPA interface is not synced because no SPX status observation is available",
            );
            return false;
        };

        // If there is an instance, and the instance has SPX attachments,
        // and one of the attachments matches our mac address and has a non-zero partition ID,
        // then the DPA expected version should be the instance's SPX config version.
        if let Some(instance) = instance
            && instance
                .config
                .spxconfig
                .spx_attachments
                .iter()
                .any(|attachment| {
                    attachment.mac_address.as_deref().unwrap_or_default()
                        == self.mac_address.to_string()
                        && attachment.spx_partition_id != NULL_SPX_PARTITION_ID
                })
        {
            dpa_expected_version = instance.spx_config_version;
        }

        for obs in spx_status_observation.spx_attachments.iter() {
            if obs.mac_address == self.mac_address
                && let Some(config_version) = obs.config_version
            {
                if config_version != dpa_expected_version {
                    tracing::info!(
                        dpa_interface_id = %self.id,
                        config_version = %config_version,
                        dpa_expected_version = %dpa_expected_version,
                        "DPA interface is not synced version mismatch!",
                    );
                    return false;
                }
                return true;
            }
        }

        tracing::info!(
            dpa_interface_id = %self.id,
            dpa_expected_version = %dpa_expected_version,
            "DPA interface is not synced because no matching SPX status attachment was found",
        );

        false
    }

    pub fn is_ready(&self) -> bool {
        self.controller_state.value == DpaInterfaceControllerState::Ready
    }
}

impl<'r> FromRow<'r, PgRow> for DpaInterface {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        // Json<T> deserializes the row bytes straight into the snapshot
        // struct, skipping the intermediate serde_json::Value DOM.
        let json: sqlx::types::Json<DpaInterfaceSnapshotPgJson> = row.try_get(0)?;
        json.0.try_into()
    }
}

#[derive(Serialize, Deserialize)]
pub struct DpaInterfaceSnapshotPgJson {
    pub id: DpaInterfaceId,
    pub machine_id: HostMachineId,
    pub mac_address: MacAddress,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
    pub last_hb_time: DateTime<Utc>,
    pub controller_state: DpaInterfaceControllerState,
    pub controller_state_version: String,
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,
    pub network_config: DpaInterfaceNetworkConfig,
    pub network_config_version: String,
    pub card_state: Option<CardState>,
    pub pci_name: String,
    pub underlay_ip: Option<IpAddr>,
    pub overlay_ip: Option<IpAddr>,
    #[serde(default, alias = "device_info_report")]
    pub device_info: Option<MlxDeviceInfo>,
    #[serde(default, alias = "device_info_report_ts")]
    pub device_info_ts: Option<DateTime<Utc>>,
    #[serde(default)]
    pub mlxconfig_profile: Option<String>,
    #[serde(default)]
    pub history: Vec<StateHistoryRecord>,
    #[serde(default)]
    pub device_description: Option<String>,
    pub interface_type: DpaInterfaceType,
}

impl TryFrom<DpaInterfaceSnapshotPgJson> for DpaInterface {
    type Error = sqlx::Error;

    fn try_from(value: DpaInterfaceSnapshotPgJson) -> sqlx::Result<Self> {
        Ok(Self {
            id: value.id,
            machine_id: value.machine_id,
            mac_address: value.mac_address,
            created: value.created,
            updated: value.updated,
            deleted: value.deleted,
            last_hb_time: value.last_hb_time,
            controller_state: Versioned {
                value: value.controller_state,
                version: value.controller_state_version.parse().map_err(|e| {
                    sqlx::error::Error::ColumnDecode {
                        index: "controller_state_version".to_string(),
                        source: Box::new(e),
                    }
                })?,
            },
            controller_state_outcome: value.controller_state_outcome,
            network_config: Versioned {
                value: value.network_config,
                version: value.network_config_version.parse().map_err(|e| {
                    sqlx::error::Error::ColumnDecode {
                        index: "network_config_version".to_string(),
                        source: Box::new(e),
                    }
                })?,
            },
            card_state: value.card_state,
            device_info: value.device_info,
            device_info_ts: value.device_info_ts,
            mlxconfig_profile: value.mlxconfig_profile,
            history: value.history,
            pci_name: value.pci_name,
            underlay_ip: value.underlay_ip,
            overlay_ip: value.overlay_ip,
            device_description: value.device_description,
            interface_type: value.interface_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};
    use carbide_uuid::instance::InstanceId;
    use carbide_uuid::machine::HostMachineId as MachineId;
    use carbide_uuid::spx::SpxPartitionId;

    use super::*;
    use crate::instance::config::InstanceConfig;
    use crate::instance::config::extension_services::InstanceExtensionServicesConfig;
    use crate::instance::config::infiniband::InstanceInfinibandConfig;
    use crate::instance::config::network::InstanceNetworkConfig;
    use crate::instance::config::nvlink::InstanceNvLinkConfig;
    use crate::instance::config::spx::{
        InstanceSpxAttachment, InstanceSpxConfig, SpxAttachmentType,
    };
    use crate::instance::config::tenant_config::TenantConfig;
    use crate::instance::status::InstanceStatusObservations;
    use crate::machine::spx::MachineSpxAttachmentStatusObservation;
    use crate::metadata::Metadata;
    use crate::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
    use crate::tenant::TenantOrganizationId;

    fn test_machine_id() -> MachineId {
        MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30").unwrap()
    }

    /// Inputs to `NewDpaInterface::from_device_info`, one per row.
    struct DeviceInfoInput {
        base_mac: Option<MacAddress>,
        device_type: String,
        pci_name: String,
        device_description: Option<String>,
        interface_type: DpaInterfaceType,
    }

    #[test]
    fn from_device_info() {
        // `from_device_info` only fails (returns None) when the base MAC is unset;
        // with a MAC present it projects the input fields straight through. We map
        // the Option to a Result and, for the success row, yield the asserted
        // (machine_id, mac_address, device_type, pci_name) fields.
        let machine_id = test_machine_id();
        scenarios!(
            run = |i| {
                NewDpaInterface::from_device_info(
                    machine_id,
                    i.base_mac,
                    i.device_type,
                    i.pci_name,
                    i.device_description,
                    i.interface_type,
                )
                .map(|n| (n.machine_id, n.mac_address, n.device_type, n.pci_name))
                .ok_or(())
            };
            "extracts fields when base mac present" {
                DeviceInfoInput {
                    base_mac: Some(MacAddress::from_str("00:11:22:33:44:55").unwrap()),
                    device_type: "BlueField3".to_string(),
                    pci_name: "01:00.0".to_string(),
                    device_description: Some("SuperNIC".to_string()),
                    interface_type: DpaInterfaceType::Svpc,
                } => Yields((
                    machine_id,
                    MacAddress::from_str("00:11:22:33:44:55").unwrap(),
                    "BlueField3".to_string(),
                    "01:00.0".to_string(),
                )),
            }

            "extracts fields for astra interface type" {
                DeviceInfoInput {
                    base_mac: Some(MacAddress::from_str("aa:bb:cc:dd:ee:ff").unwrap()),
                    device_type: "ConnectX7".to_string(),
                    pci_name: "02:00.1".to_string(),
                    device_description: None,
                    interface_type: DpaInterfaceType::Astra,
                } => Yields((
                    machine_id,
                    MacAddress::from_str("aa:bb:cc:dd:ee:ff").unwrap(),
                    "ConnectX7".to_string(),
                    "02:00.1".to_string(),
                )),
            }

            "extracts fields with empty device type and pci name" {
                DeviceInfoInput {
                    base_mac: Some(MacAddress::from_str("00:00:00:00:00:00").unwrap()),
                    device_type: String::new(),
                    pci_name: String::new(),
                    device_description: Some(String::new()),
                    interface_type: DpaInterfaceType::Svpc,
                } => Yields((
                    machine_id,
                    MacAddress::from_str("00:00:00:00:00:00").unwrap(),
                    String::new(),
                    String::new(),
                )),
            }

            "returns none without base mac" {
                DeviceInfoInput {
                    base_mac: None,
                    device_type: "BlueField3".to_string(),
                    pci_name: "01:00.0".to_string(),
                    device_description: None,
                    interface_type: DpaInterfaceType::Svpc,
                } => Fails,
            }

            "returns none without base mac for astra" {
                DeviceInfoInput {
                    base_mac: None,
                    device_type: "ConnectX7".to_string(),
                    pci_name: "02:00.1".to_string(),
                    device_description: Some("desc".to_string()),
                    interface_type: DpaInterfaceType::Astra,
                } => Fails,
            }
        );
    }

    #[test]
    fn serialize_controller_state() {
        // Each state must serialize to its exact tagged JSON and deserialize back to
        // the original value. The closure serializes, asserts the round-trip equals
        // the input state, and yields the serialized string (which is the contract).
        check_cases(
            [
                Case {
                    scenario: "provisioning",
                    input: DpaInterfaceControllerState::Provisioning {},
                    expect: Yields("{\"state\":\"provisioning\"}".to_string()),
                },
                Case {
                    scenario: "ready",
                    input: DpaInterfaceControllerState::Ready {},
                    expect: Yields("{\"state\":\"ready\"}".to_string()),
                },
                Case {
                    scenario: "unlocking",
                    input: DpaInterfaceControllerState::Unlocking,
                    expect: Yields("{\"state\":\"unlocking\"}".to_string()),
                },
                Case {
                    scenario: "applyfirmware",
                    input: DpaInterfaceControllerState::ApplyFirmware,
                    expect: Yields("{\"state\":\"applyfirmware\"}".to_string()),
                },
                Case {
                    scenario: "applyprofile",
                    input: DpaInterfaceControllerState::ApplyProfile,
                    expect: Yields("{\"state\":\"applyprofile\"}".to_string()),
                },
                Case {
                    scenario: "locking",
                    input: DpaInterfaceControllerState::Locking,
                    expect: Yields("{\"state\":\"locking\"}".to_string()),
                },
                Case {
                    scenario: "assigned",
                    input: DpaInterfaceControllerState::Assigned,
                    expect: Yields("{\"state\":\"assigned\"}".to_string()),
                },
                Case {
                    scenario: "rotatekeyunlocking",
                    input: DpaInterfaceControllerState::RotateKeyUnlocking,
                    expect: Yields("{\"state\":\"rotatekeyunlocking\"}".to_string()),
                },
                Case {
                    scenario: "rotatekeylocking",
                    input: DpaInterfaceControllerState::RotateKeyLocking,
                    expect: Yields("{\"state\":\"rotatekeylocking\"}".to_string()),
                },
            ],
            |state| -> Result<String, ()> {
                let serialized = serde_json::to_string(&state).map_err(|_| ())?;
                let round_tripped =
                    serde_json::from_str::<DpaInterfaceControllerState>(&serialized)
                        .map_err(|_| ())?;
                assert_eq!(round_tripped, state);
                Ok(serialized)
            },
        );
    }

    #[test]
    fn lock_mode_try_from_i32() {
        // `DpaLockMode::try_from(i32)` accepts only 1 (Locked) and 2 (Unlocked);
        // every other value — including the zero and the values just outside the
        // accepted pair — is rejected with the same static error.
        scenarios!(
            run = DpaLockMode::try_from;
            "one is locked" {
                1 => Yields(DpaLockMode::Locked),
            }

            "two is unlocked" {
                2 => Yields(DpaLockMode::Unlocked),
            }

            "zero is invalid" {
                0 => FailsWith("Invalid value for DpaLockMode"),
            }

            "three is invalid" {
                3 => FailsWith("Invalid value for DpaLockMode"),
            }

            "negative is invalid" {
                -1 => FailsWith("Invalid value for DpaLockMode"),
            }

            "max is invalid" {
                i32::MAX => FailsWith("Invalid value for DpaLockMode"),
            }

            "min is invalid" {
                i32::MIN => FailsWith("Invalid value for DpaLockMode"),
            }
        );
    }

    #[test]
    fn controller_state_display() {
        // The `Display` impl defers to `Debug`, so each variant renders as its
        // bare Rust identifier — distinct from the lowercase serde tag.
        value_scenarios!(
            run = |state| state.to_string();
            "provisioning" {
                DpaInterfaceControllerState::Provisioning => "Provisioning".to_string(),
            }

            "ready" {
                DpaInterfaceControllerState::Ready => "Ready".to_string(),
            }

            "unlocking" {
                DpaInterfaceControllerState::Unlocking => "Unlocking".to_string(),
            }

            "applyfirmware" {
                DpaInterfaceControllerState::ApplyFirmware => "ApplyFirmware".to_string(),
            }

            "applyprofile" {
                DpaInterfaceControllerState::ApplyProfile => "ApplyProfile".to_string(),
            }

            "locking" {
                DpaInterfaceControllerState::Locking => "Locking".to_string(),
            }

            "assigned" {
                DpaInterfaceControllerState::Assigned => "Assigned".to_string(),
            }

            "rotatekeyunlocking" {
                DpaInterfaceControllerState::RotateKeyUnlocking => "RotateKeyUnlocking".to_string(),
            }

            "rotatekeylocking" {
                DpaInterfaceControllerState::RotateKeyLocking => "RotateKeyLocking".to_string(),
            }
        );
    }

    #[test]
    fn controller_state_deserialize_rejects_unknown_tag() {
        // The accepted tags are exactly the lowercase variant names; an unknown
        // tag and a missing tag are both rejected.
        scenarios!(
            run = |json| serde_json::from_str::<DpaInterfaceControllerState>(json).map_err(|_| ());
            "known tag deserializes" {
                "{\"state\":\"ready\"}" => Yields(DpaInterfaceControllerState::Ready),
            }

            "capitalized tag is rejected" {
                "{\"state\":\"Ready\"}" => Fails,
            }

            "unknown tag is rejected" {
                "{\"state\":\"bogus\"}" => Fails,
            }

            "missing tag is rejected" {
                "{}" => Fails,
            }
        );
    }

    #[test]
    fn network_config_default_uses_admin_network() {
        // The default network config opts into the admin network and carries no
        // quarantine state.
        value_scenarios!(
            run = |()| {
                let cfg = DpaInterfaceNetworkConfig::default();
                (cfg.use_admin_network, cfg.quarantine_state)
            };
            "defaults to admin network" {
                () => (Some(true), None),
            }
        );
    }

    #[test]
    fn lock_mode_serde_round_trips() {
        // Both lock modes survive a JSON round-trip, serializing to their bare
        // variant names.
        check_cases(
            [
                Case {
                    scenario: "locked",
                    input: DpaLockMode::Locked,
                    expect: Yields("\"Locked\"".to_string()),
                },
                Case {
                    scenario: "unlocked",
                    input: DpaLockMode::Unlocked,
                    expect: Yields("\"Unlocked\"".to_string()),
                },
            ],
            |mode| -> Result<String, ()> {
                let serialized = serde_json::to_string(&mode).map_err(|_| ())?;
                let round_tripped =
                    serde_json::from_str::<DpaLockMode>(&serialized).map_err(|_| ())?;
                assert_eq!(round_tripped, mode);
                Ok(serialized)
            },
        );
    }

    #[test]
    fn interface_type_serde_round_trips() {
        // Each `DpaInterfaceType` variant serializes to its bare name and parses
        // back to itself.
        check_cases(
            [
                Case {
                    scenario: "svpc",
                    input: DpaInterfaceType::Svpc,
                    expect: Yields("\"Svpc\"".to_string()),
                },
                Case {
                    scenario: "astra",
                    input: DpaInterfaceType::Astra,
                    expect: Yields("\"Astra\"".to_string()),
                },
            ],
            |ty| -> Result<String, ()> {
                let serialized = serde_json::to_string(&ty).map_err(|_| ())?;
                let round_tripped =
                    serde_json::from_str::<DpaInterfaceType>(&serialized).map_err(|_| ())?;
                assert_eq!(round_tripped, ty);
                Ok(serialized)
            },
        );
    }

    const DPA_MAC: &str = "00:11:22:33:44:55";

    /// A `DpaInterface` on `DPA_MAC` in controller state `state` whose
    /// managed-host (fallback) network config version is `mh_version`.
    /// `managed_host_network_config_version_synced` reads only the MAC, the
    /// controller state, and this version off the interface; the remaining
    /// fields are inert.
    fn dpa_interface(
        state: DpaInterfaceControllerState,
        mh_version: ConfigVersion,
    ) -> DpaInterface {
        DpaInterface {
            id: DpaInterfaceId::nil(),
            machine_id: test_machine_id(),
            mac_address: MacAddress::from_str(DPA_MAC).unwrap(),
            pci_name: String::new(),
            underlay_ip: None,
            overlay_ip: None,
            created: Utc::now(),
            updated: Utc::now(),
            deleted: None,
            controller_state: Versioned::new(state, ConfigVersion::initial()),
            last_hb_time: Utc::now(),
            controller_state_outcome: None,
            network_config: Versioned::new(DpaInterfaceNetworkConfig::default(), mh_version),
            card_state: None,
            device_info: None,
            device_info_ts: None,
            mlxconfig_profile: None,
            history: Vec::new(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        }
    }

    /// An `InstanceSnapshot` carrying `spx_attachments` and `spx_config_version`.
    /// The sync check reads only `config.spxconfig.spx_attachments` and
    /// `spx_config_version`; everything else is filled with inert values.
    fn instance_snapshot(
        spx_attachments: Vec<InstanceSpxAttachment>,
        spx_config_version: ConfigVersion,
    ) -> InstanceSnapshot {
        let config = InstanceConfig {
            tenant: TenantConfig {
                tenant_organization_id: TenantOrganizationId::try_from("TenantA".to_string())
                    .unwrap(),
                tenant_keyset_ids: Vec::new(),
                hostname: None,
            },
            os: OperatingSystem {
                user_data: None,
                variant: OperatingSystemVariant::Ipxe(InlineIpxe {
                    ipxe_script: "boot".to_string(),
                }),
                phone_home_enabled: false,
                run_provisioning_instructions_on_every_boot: false,
            },
            network: InstanceNetworkConfig::default(),
            infiniband: InstanceInfinibandConfig::default(),
            network_security_group_id: None,
            extension_services: InstanceExtensionServicesConfig::default(),
            nvlink: InstanceNvLinkConfig::default(),
            spxconfig: InstanceSpxConfig { spx_attachments },
            power_profile: None,
        };
        InstanceSnapshot {
            id: InstanceId::nil(),
            machine_id: HostMachineId::from_str(
                "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
            )
            .unwrap(),
            instance_type_id: None,
            metadata: Metadata::default(),
            config,
            config_version: ConfigVersion::initial(),
            network_config_version: ConfigVersion::initial(),
            ib_config_version: ConfigVersion::initial(),
            nvlink_config_version: ConfigVersion::initial(),
            spx_config_version,
            storage_config_version: ConfigVersion::initial(),
            extension_services_config_version: ConfigVersion::initial(),
            observations: InstanceStatusObservations {
                network: HashMap::default(),
                extension_services: HashMap::default(),
                phone_home_last_contact: None,
            },
            use_custom_pxe_on_boot: false,
            custom_pxe_reboot_requested: false,
            deleted: None,
            update_network_config_request: None,
        }
    }

    /// An instance SPX attachment bound to `DPA_MAC` with a non-null partition
    /// id, so the sync check adopts the instance's SPX config version as the
    /// expected version.
    fn matching_instance_attachment() -> InstanceSpxAttachment {
        InstanceSpxAttachment {
            device: "eth0".to_string(),
            device_instance: 0,
            mac_address: Some(DPA_MAC.to_string()),
            spx_partition_id: SpxPartitionId::new(),
            attachment_type: SpxAttachmentType::Physical,
            virtual_function_id: None,
        }
    }

    /// An SPX status observation for `DPA_MAC` reporting `config_version`.
    fn observation(config_version: ConfigVersion) -> MachineSpxStatusObservation {
        MachineSpxStatusObservation {
            spx_attachments: vec![MachineSpxAttachmentStatusObservation {
                mac_address: MacAddress::from_str(DPA_MAC).unwrap(),
                config_version: Some(config_version),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn managed_host_network_config_version_synced_scenarios() {
        // Sync-ness depends only on: the interface's controller state and its
        // fallback (managed-host) network config version `mh_version`; the
        // instance's SPX attachments and its `instance_version`, which becomes
        // the expected version when an attachment matches DPA_MAC with a non-null
        // partition id; and the observed version for DPA_MAC. With no observation
        // we are never synced, except when the instance has no SPX attachments to
        // sync at all and the interface has reached the Assigned state.
        let mh_version = ConfigVersion::new(1);
        let instance_version = ConfigVersion::new(7);

        value_scenarios!(
            run = |(state, instance, observation): (
                DpaInterfaceControllerState,
                Option<InstanceSnapshot>,
                Option<MachineSpxStatusObservation>,
            )| {
                let dpa = dpa_interface(state, mh_version);
                dpa.managed_host_network_config_version_synced(&instance, &observation)
            };

            "no instance and no observation is not synced" {
                (DpaInterfaceControllerState::Assigned, None, None) => false,
            }

            "instance without attachments in assigned state is synced despite no observation" {
                (
                    DpaInterfaceControllerState::Assigned,
                    Some(instance_snapshot(Vec::new(), instance_version)),
                    None,
                ) => true,
            }

            "instance without attachments before assigned state is not synced" {
                (
                    DpaInterfaceControllerState::Ready,
                    Some(instance_snapshot(Vec::new(), instance_version)),
                    None,
                ) => false,
            }

            "configured attachments without observation is not synced" {
                (
                    DpaInterfaceControllerState::Assigned,
                    Some(instance_snapshot(vec![matching_instance_attachment()], instance_version)),
                    None,
                ) => false,
            }

            "configured attachments with matching observed version is synced" {
                (
                    DpaInterfaceControllerState::Assigned,
                    Some(instance_snapshot(vec![matching_instance_attachment()], instance_version)),
                    Some(observation(instance_version)),
                ) => true,
            }

            "configured attachments with mismatched observed version is not synced" {
                (
                    DpaInterfaceControllerState::Assigned,
                    Some(instance_snapshot(vec![matching_instance_attachment()], instance_version)),
                    Some(observation(mh_version)),
                ) => false,
            }
        );
    }
}
