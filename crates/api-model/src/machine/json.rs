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
use std::collections::HashMap;

use carbide_uuid::instance_type::InstanceTypeId;
use carbide_uuid::machine::MachineId;
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::rack::RackId;
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use itertools::Itertools;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use crate::bmc_info::BmcInfo;
use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::hardware_info::{MachineInventory, MachineNvLinkInfo};
use crate::instance::status::extension_service::InstanceExtensionServiceStatusObservationByType;
use crate::machine::health_override::HealthReportSources;
use crate::machine::infiniband::MachineInfinibandStatusObservation;
use crate::machine::network::{MachineNetworkStatusObservation, ManagedHostNetworkConfig};
use crate::machine::nvlink::MachineNvLinkStatusObservation;
use crate::machine::spx::MachineSpxStatusObservation;
use crate::machine::topology::MachineTopology;
use crate::machine::{
    Dpf, FailureDetails, HostProfile, HostReprovisionRequest, Machine, MachineConfig,
    MachineInterfaceSnapshot, MachineLastRebootRequested, MachineMaintenanceRequest, MachineStatus,
    ManagedHostState, ReprovisionRequest, UpgradeDecision,
};
use crate::machine_boot_interface::{
    BootInterfaceSelection, BootInterfaceSelectionSource, BootInterfaceStatusObservation,
    MachineBootInterfaceTarget, canonical_redfish_boot_interface_id,
};
use crate::metadata::Metadata;
use crate::power_manager::PowerOptions;
use crate::rack::RackFirmwareUpgradeStatus;
use crate::sku::SkuStatus;
use crate::state_history::StateHistoryRecord;

/// This represents the structure of a machine we get from postgres via the row_to_json or
/// JSONB_AGG functions. Its fields need to match the column names of the machine_snapshots query
/// exactly. It's expected that we read this directly from the JSON returned by the query, and then
/// convert it into a Machine.
#[derive(Serialize, Deserialize)]
pub struct MachineSnapshotPgJson {
    pub id: MachineId,
    pub rack_id: Option<RackId>,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deployed: Option<DateTime<Utc>>,
    pub agent_reported_inventory: Option<MachineInventory>,
    pub network_config_version: String,
    pub network_config: ManagedHostNetworkConfig,
    pub network_status_observation: Option<MachineNetworkStatusObservation>,
    pub infiniband_status_observation: Option<MachineInfinibandStatusObservation>,
    pub nvlink_status_observation: Option<MachineNvLinkStatusObservation>,
    pub spx_status_observation: Option<MachineSpxStatusObservation>,
    #[serde(default)]
    pub extension_service_status_observations: InstanceExtensionServiceStatusObservationByType,
    pub controller_state_version: String,
    pub controller_state: ManagedHostState,
    pub last_discovery_time: Option<DateTime<Utc>>,
    pub last_scout_contact_time: Option<DateTime<Utc>>,
    pub last_scout_observed_version: Option<String>,
    pub last_reboot_time: Option<DateTime<Utc>>,
    pub last_reboot_requested: Option<MachineLastRebootRequested>,
    pub last_cleanup_time: Option<DateTime<Utc>>,
    pub failure_details: FailureDetails,
    pub reprovisioning_requested: Option<ReprovisionRequest>,
    pub host_reprovisioning_requested: Option<HostReprovisionRequest>,
    pub machine_maintenance_requested: Option<MachineMaintenanceRequest>,
    #[serde(default)]
    pub decommission_requested: bool,
    #[serde(default)]
    pub bmc_credential_rotation_requested: bool,
    #[serde(default)]
    pub uefi_credential_rotation_requested: bool,
    /// is there a forced NIC lockdown rotation requested for this host
    #[serde(default)]
    pub lockdown_ikm_credential_rotation_requested: bool,
    pub manual_firmware_upgrade_completed: Option<DateTime<Utc>>,
    pub bios_password_set_time: Option<DateTime<Utc>>,
    pub last_machine_validation_time: Option<DateTime<Utc>>,
    pub discovery_machine_validation_id: Option<MachineValidationId>,
    pub cleanup_machine_validation_id: Option<MachineValidationId>,
    pub dpu_agent_upgrade_requested: Option<UpgradeDecision>,
    pub firmware_autoupdate: Option<bool>,
    pub health_reports: Option<HealthReportSources>,
    pub on_demand_machine_validation_id: Option<MachineValidationId>,
    pub on_demand_machine_validation_request: Option<bool>,
    pub asn: Option<u32>,
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,
    pub current_machine_validation_id: Option<MachineValidationId>,
    pub machine_state_model_version: i32,
    pub instance_type_id: Option<InstanceTypeId>,
    pub interfaces: Vec<MachineInterfaceSnapshot>,
    pub topology: Vec<MachineTopology>,
    pub bmc_info: BmcInfo,
    pub labels: HashMap<String, String>,
    pub name: String,
    pub description: String,
    #[serde(default)] // History is only brought in if the search config requested it
    pub history: Vec<StateHistoryRecord>,
    pub version: String,
    pub hw_sku: Option<String>,
    pub desired_boot_interface_mac: Option<MacAddress>,
    pub desired_boot_interface_id: Option<String>,
    pub desired_boot_interface_version: Option<String>,
    /// Selection source paired with the desired boot interface columns.
    pub boot_interface_selection_source: Option<BootInterfaceSelectionSource>,
    /// Decision time for `boot_interface_selection_source`; legacy selections
    /// may not have one.
    pub boot_interface_selection_updated_at: Option<DateTime<Utc>>,
    pub boot_interface_verified_version: Option<String>,
    pub boot_interface_observed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub boot_interface_observation_assumed: bool,
    pub hw_sku_status: Option<SkuStatus>,
    #[serde(default)] // Power options are valid only for host, not for DPUs.
    pub power_options: Option<PowerOptions>,
    pub hw_sku_device_type: Option<String>,
    pub update_complete: bool,
    pub nvlink_info: Option<MachineNvLinkInfo>,
    pub dpf: Dpf,
    #[serde(default)]
    pub host_profile: HostProfile,
    #[serde(default)]
    pub rack_fw_details: Option<RackFirmwareUpgradeStatus>,
    #[serde(default)]
    pub slot_number: Option<i32>,
    #[serde(default)]
    pub tray_index: Option<i32>,
}

fn desired_boot_interface_decode_error(message: impl Into<String>) -> sqlx::Error {
    sqlx::Error::ColumnDecode {
        index: "desired_boot_interface_(mac,id,version)".to_string(),
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.into(),
        )),
    }
}

fn decode_desired_boot_interface(
    mac_address: Option<MacAddress>,
    interface_id: Option<String>,
    version: Option<String>,
) -> sqlx::Result<Option<Versioned<MachineBootInterfaceTarget>>> {
    match (mac_address, interface_id, version) {
        (None, None, None) => Ok(None),
        (Some(mac_address), interface_id, Some(version)) => {
            if let Some(interface_id) = interface_id.as_deref()
                && canonical_redfish_boot_interface_id(interface_id) != Some(interface_id)
            {
                return Err(desired_boot_interface_decode_error(
                    "desired boot interface id is empty or noncanonical",
                ));
            }

            let version = version.parse().map_err(|error| sqlx::Error::ColumnDecode {
                index: "desired_boot_interface_version".to_string(),
                source: Box::new(error),
            })?;
            let value = MachineBootInterfaceTarget::from_parts(Some(mac_address), interface_id)
                .ok_or_else(|| {
                    desired_boot_interface_decode_error(
                        "desired boot interface MAC did not produce a target",
                    )
                })?;

            Ok(Some(Versioned { value, version }))
        }
        _ => Err(desired_boot_interface_decode_error(
            "desired boot interface MAC and version must both be set or both be null, and an id requires a MAC",
        )),
    }
}

/// Reconstructs selection metadata while rejecting column states that disagree
/// with the presence of a desired boot interface.
fn decode_boot_interface_selection(
    desired_boot_interface_is_set: bool,
    source: Option<BootInterfaceSelectionSource>,
    updated_at: Option<DateTime<Utc>>,
) -> sqlx::Result<Option<BootInterfaceSelection>> {
    match (desired_boot_interface_is_set, source, updated_at) {
        (false, None, None) => Ok(None),
        (true, Some(BootInterfaceSelectionSource::LegacyUnknown), updated_at) => {
            Ok(Some(BootInterfaceSelection {
                source: BootInterfaceSelectionSource::LegacyUnknown,
                updated_at,
            }))
        }
        (true, Some(source), Some(updated_at)) => Ok(Some(BootInterfaceSelection {
            source,
            updated_at: Some(updated_at),
        })),
        _ => Err(sqlx::Error::ColumnDecode {
            index: "boot_interface_selection_(source,updated_at)".to_string(),
            source: Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "boot interface selection source must be set exactly when a desired boot interface exists, and attributed selections require a selection time",
            )),
        }),
    }
}

fn decode_boot_interface_status_observation(
    config_version: Option<String>,
    observed_at: Option<DateTime<Utc>>,
    assumed: bool,
) -> sqlx::Result<Option<BootInterfaceStatusObservation>> {
    match (config_version, observed_at, assumed) {
        (None, None, false) => Ok(None),
        (Some(config_version), Some(observed_at), assumed) => {
            let config_version =
                config_version
                    .parse()
                    .map_err(|error| sqlx::Error::ColumnDecode {
                        index: "boot_interface_verified_version".to_string(),
                        source: Box::new(error),
                    })?;
            Ok(Some(BootInterfaceStatusObservation {
                config_version,
                observed_at,
                assumed,
            }))
        }
        _ => Err(sqlx::Error::ColumnDecode {
            index: "boot_interface_(verified_version,observed_at,assumed)".to_string(),
            source: Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "boot interface verified version and observation time must both be set or both be null, and assumed requires an observation",
            )),
        }),
    }
}

impl TryFrom<MachineSnapshotPgJson> for Machine {
    type Error = sqlx::Error;

    fn try_from(value: MachineSnapshotPgJson) -> sqlx::Result<Self> {
        let hardware_info = value
            .topology
            .into_iter()
            .map(|t| {
                let topology = t.into_topology();
                Some(topology.discovery_data.info)
            })
            .next()
            .unwrap_or(None);

        let metadata = Metadata {
            name: value.name,
            description: value.description,
            labels: value.labels,
        };

        let desired_boot_interface = decode_desired_boot_interface(
            value.desired_boot_interface_mac,
            value.desired_boot_interface_id,
            value.desired_boot_interface_version,
        )?;
        let boot_interface_selection = decode_boot_interface_selection(
            desired_boot_interface.is_some(),
            value.boot_interface_selection_source,
            value.boot_interface_selection_updated_at,
        )?;
        let boot_interface_status_observation = decode_boot_interface_status_observation(
            value.boot_interface_verified_version,
            value.boot_interface_observed_at,
            value.boot_interface_observation_assumed,
        )?;

        let version: ConfigVersion =
            value
                .version
                .parse()
                .map_err(|e| sqlx::error::Error::ColumnDecode {
                    index: "version".to_string(),
                    source: Box::new(e),
                })?;

        let history = value
            .history
            .into_iter()
            .sorted_by(|s1: &StateHistoryRecord, s2: &StateHistoryRecord| {
                Ord::cmp(&s1.state_version.timestamp(), &s2.state_version.timestamp())
            })
            .collect();

        let health_reports = value.health_reports.unwrap_or_default();
        let (maintenance_reference, maintenance_start_time) = health_reports
            .maintenance_override()
            .map(|o| {
                (
                    Some(o.maintenance_reference.clone()),
                    o.maintenance_start_time,
                )
            })
            .unwrap_or_default();

        Ok(Self {
            id: value.id,
            state: Versioned {
                value: value.controller_state,
                version: value.controller_state_version.parse().map_err(|e| {
                    sqlx::error::Error::ColumnDecode {
                        index: "controller_state_version".to_string(),
                        source: Box::new(e),
                    }
                })?,
            },
            network_config: Versioned {
                value: value.network_config,
                version: value.network_config_version.parse().map_err(|e| {
                    sqlx::error::Error::ColumnDecode {
                        index: "network_config_version".to_string(),
                        source: Box::new(e),
                    }
                })?,
            },
            network_status_observation: value.network_status_observation,
            history,
            metadata,
            version,
            rack_id: value.rack_id,
            config: MachineConfig {
                firmware_autoupdate: value.firmware_autoupdate,
                instance_type_id: value.instance_type_id,
                dpf: value.dpf,
                hw_sku: value.hw_sku,
                desired_boot_interface,
                boot_interface_selection,
                maintenance_reference,
                maintenance_start_time,
            },
            status: MachineStatus {
                interfaces: value.interfaces,
                boot_interface_status_observation,
                hardware_info,
                bmc_info: value.bmc_info,
                last_reboot_time: value.last_reboot_time,
                last_cleanup_time: value.last_cleanup_time,
                last_discovery_time: value.last_discovery_time,
                last_scout_contact_time: value.last_scout_contact_time,
                last_scout_observed_version: value.last_scout_observed_version,
                failure_details: value.failure_details,
                inventory: value.agent_reported_inventory,
                last_reboot_requested: value.last_reboot_requested,
                hw_sku: value.hw_sku_status,
                hw_sku_device_type: value.hw_sku_device_type,
                update_complete: value.update_complete,
                nvlink_info: value.nvlink_info,
                infiniband_status_observation: value.infiniband_status_observation,
                nvlink_status_observation: value.nvlink_status_observation,
                spx_status_observation: value.spx_status_observation,
                extension_service_status_observations: value.extension_service_status_observations,
                slot_number: value.slot_number,
                tray_index: value.tray_index,
                power_options: value.power_options,
            },
            health_reports,
            reprovision_requested: value.reprovisioning_requested,
            host_reprovision_requested: value.host_reprovisioning_requested,
            dpu_agent_upgrade_requested: value.dpu_agent_upgrade_requested,
            controller_state_outcome: value.controller_state_outcome,
            bios_password_set_time: value.bios_password_set_time,
            last_machine_validation_time: value.last_machine_validation_time,
            discovery_machine_validation_id: value.discovery_machine_validation_id,
            cleanup_machine_validation_id: value.cleanup_machine_validation_id,
            on_demand_machine_validation_id: value.on_demand_machine_validation_id,
            on_demand_machine_validation_request: value.on_demand_machine_validation_request,
            asn: value.asn,
            host_profile: value.host_profile,
            rack_fw_details: value.rack_fw_details,
            machine_maintenance_requested: value.machine_maintenance_requested,
            decommission_requested: value.decommission_requested,
            bmc_credential_rotation_requested: value.bmc_credential_rotation_requested,
            uefi_credential_rotation_requested: value.uefi_credential_rotation_requested,
            lockdown_ikm_credential_rotation_requested: value
                .lockdown_ikm_credential_rotation_requested,
            manual_firmware_upgrade_completed: value.manual_firmware_upgrade_completed,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, check_cases};

    use super::*;
    use crate::machine_boot_interface::MachineBootInterface;

    #[derive(Debug)]
    struct Input {
        mac_address: Option<MacAddress>,
        interface_id: Option<String>,
        version: Option<String>,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Decoded {
        Unset,
        MacOnly {
            mac_address: MacAddress,
            version_nr: u64,
        },
        Pair {
            mac_address: MacAddress,
            interface_id: String,
            version_nr: u64,
        },
    }

    #[derive(Debug)]
    struct ObservationInput {
        config_version: Option<String>,
        observed_at: Option<DateTime<Utc>>,
        assumed: bool,
    }

    #[derive(Debug)]
    struct SelectionInput {
        desired_boot_interface_is_set: bool,
        source: Option<BootInterfaceSelectionSource>,
        updated_at: Option<DateTime<Utc>>,
    }

    fn summarize(value: Option<Versioned<MachineBootInterfaceTarget>>) -> Decoded {
        match value {
            None => Decoded::Unset,
            Some(Versioned {
                value: MachineBootInterfaceTarget::MacOnly(mac_address),
                version,
            }) => Decoded::MacOnly {
                mac_address,
                version_nr: version.version_nr(),
            },
            Some(Versioned {
                value:
                    MachineBootInterfaceTarget::Pair(MachineBootInterface {
                        mac_address,
                        interface_id,
                    }),
                version,
            }) => Decoded::Pair {
                mac_address,
                interface_id,
                version_nr: version.version_nr(),
            },
        }
    }

    #[test]
    fn desired_boot_interface_columns_decode_atomically() {
        let mac_address = MacAddress::new([1, 2, 3, 4, 5, 6]);
        let version = ConfigVersion::new(7).version_string();

        check_cases(
            [
                Case {
                    scenario: "all columns null",
                    input: Input {
                        mac_address: None,
                        interface_id: None,
                        version: None,
                    },
                    expect: Yields(Decoded::Unset),
                },
                Case {
                    scenario: "MAC and version",
                    input: Input {
                        mac_address: Some(mac_address),
                        interface_id: None,
                        version: Some(version.clone()),
                    },
                    expect: Yields(Decoded::MacOnly {
                        mac_address,
                        version_nr: 7,
                    }),
                },
                Case {
                    scenario: "complete pair and version",
                    input: Input {
                        mac_address: Some(mac_address),
                        interface_id: Some("NIC.Slot.7-1-1".to_string()),
                        version: Some(version.clone()),
                    },
                    expect: Yields(Decoded::Pair {
                        mac_address,
                        interface_id: "NIC.Slot.7-1-1".to_string(),
                        version_nr: 7,
                    }),
                },
                Case {
                    scenario: "MAC without version",
                    input: Input {
                        mac_address: Some(mac_address),
                        interface_id: None,
                        version: None,
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "version without MAC",
                    input: Input {
                        mac_address: None,
                        interface_id: None,
                        version: Some(version.clone()),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "id without MAC",
                    input: Input {
                        mac_address: None,
                        interface_id: Some("NIC.Slot.7-1-1".to_string()),
                        version: Some(version),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "blank id",
                    input: Input {
                        mac_address: Some(mac_address),
                        interface_id: Some("\t\n".to_string()),
                        version: Some(ConfigVersion::new(7).version_string()),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "padded valid id",
                    input: Input {
                        mac_address: Some(mac_address),
                        interface_id: Some(" \tNIC.Slot.7-1-1\n ".to_string()),
                        version: Some(ConfigVersion::new(7).version_string()),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "malformed version",
                    input: Input {
                        mac_address: Some(mac_address),
                        interface_id: None,
                        version: Some("not-a-version".to_string()),
                    },
                    expect: Fails,
                },
            ],
            |Input {
                 mac_address,
                 interface_id,
                 version,
             }| {
                decode_desired_boot_interface(mac_address, interface_id, version)
                    .map(summarize)
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn boot_interface_selection_columns_decode_atomically() {
        let updated_at = DateTime::from_timestamp(1_722_000_000, 123_000_000)
            .expect("fixture timestamp is valid");

        check_cases(
            [
                Case {
                    scenario: "no desired target and no selection",
                    input: SelectionInput {
                        desired_boot_interface_is_set: false,
                        source: None,
                        updated_at: None,
                    },
                    expect: Yields(None),
                },
                Case {
                    scenario: "pre-tracking selection",
                    input: SelectionInput {
                        desired_boot_interface_is_set: true,
                        source: Some(BootInterfaceSelectionSource::LegacyUnknown),
                        updated_at: None,
                    },
                    expect: Yields(Some(BootInterfaceSelection {
                        source: BootInterfaceSelectionSource::LegacyUnknown,
                        updated_at: None,
                    })),
                },
                Case {
                    scenario: "actively recorded unknown selection",
                    input: SelectionInput {
                        desired_boot_interface_is_set: true,
                        source: Some(BootInterfaceSelectionSource::LegacyUnknown),
                        updated_at: Some(updated_at),
                    },
                    expect: Yields(Some(BootInterfaceSelection {
                        source: BootInterfaceSelectionSource::LegacyUnknown,
                        updated_at: Some(updated_at),
                    })),
                },
                Case {
                    scenario: "attributed selection",
                    input: SelectionInput {
                        desired_boot_interface_is_set: true,
                        source: Some(BootInterfaceSelectionSource::ExpectedMachine),
                        updated_at: Some(updated_at),
                    },
                    expect: Yields(Some(BootInterfaceSelection {
                        source: BootInterfaceSelectionSource::ExpectedMachine,
                        updated_at: Some(updated_at),
                    })),
                },
                Case {
                    scenario: "source without desired target",
                    input: SelectionInput {
                        desired_boot_interface_is_set: false,
                        source: Some(BootInterfaceSelectionSource::ExpectedMachine),
                        updated_at: Some(updated_at),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "desired target without source",
                    input: SelectionInput {
                        desired_boot_interface_is_set: true,
                        source: None,
                        updated_at: None,
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "attributed source without decision time",
                    input: SelectionInput {
                        desired_boot_interface_is_set: true,
                        source: Some(BootInterfaceSelectionSource::ExpectedMachine),
                        updated_at: None,
                    },
                    expect: Fails,
                },
            ],
            |SelectionInput {
                 desired_boot_interface_is_set,
                 source,
                 updated_at,
             }| {
                decode_boot_interface_selection(desired_boot_interface_is_set, source, updated_at)
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn boot_interface_status_columns_decode_atomically() {
        let observed_at = DateTime::from_timestamp(1_722_000_000, 123_000_000)
            .expect("fixture timestamp is valid");
        let version = ConfigVersion::new(7);
        let config_version = version.version_string();

        check_cases(
            [
                Case {
                    scenario: "no observation",
                    input: ObservationInput {
                        config_version: None,
                        observed_at: None,
                        assumed: false,
                    },
                    expect: Yields(None),
                },
                Case {
                    scenario: "Redfish observation",
                    input: ObservationInput {
                        config_version: Some(config_version.clone()),
                        observed_at: Some(observed_at),
                        assumed: false,
                    },
                    expect: Yields(Some(BootInterfaceStatusObservation {
                        config_version: version,
                        observed_at,
                        assumed: false,
                    })),
                },
                Case {
                    scenario: "rollout baseline",
                    input: ObservationInput {
                        config_version: Some(config_version.clone()),
                        observed_at: Some(observed_at),
                        assumed: true,
                    },
                    expect: Yields(Some(BootInterfaceStatusObservation {
                        config_version: version,
                        observed_at,
                        assumed: true,
                    })),
                },
                Case {
                    scenario: "version without time",
                    input: ObservationInput {
                        config_version: Some(config_version),
                        observed_at: None,
                        assumed: false,
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "time without version",
                    input: ObservationInput {
                        config_version: None,
                        observed_at: Some(observed_at),
                        assumed: false,
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "assumed without observation",
                    input: ObservationInput {
                        config_version: None,
                        observed_at: None,
                        assumed: true,
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "malformed version",
                    input: ObservationInput {
                        config_version: Some("not-a-version".to_string()),
                        observed_at: Some(observed_at),
                        assumed: false,
                    },
                    expect: Fails,
                },
            ],
            |ObservationInput {
                 config_version,
                 observed_at,
                 assumed,
             }| {
                decode_boot_interface_status_observation(config_version, observed_at, assumed)
                    .map_err(drop)
            },
        );
    }
}
