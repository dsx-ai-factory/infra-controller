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

use carbide_uuid::nvlink::NvLinkDomainId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use chrono::prelude::*;
use config_version::{ConfigVersion, Versioned};
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::StateSla;
use crate::bmc_info::BmcInfo;
use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::health::HealthReportSources;
use crate::metadata::Metadata;

pub mod power_shelf_id;
mod slas;

#[derive(Debug, Clone)]
pub struct NewPowerShelf {
    pub id: PowerShelfId,
    pub config: PowerShelfConfig,
    pub bmc_mac_address: Option<MacAddress>,
    pub metadata: Option<Metadata>,
    pub rack_id: Option<RackId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerShelfConfig {
    pub name: String,
    pub capacity: Option<u32>, // Power capacity in watts
    pub voltage: Option<u32>,  // Voltage in volts
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerShelfStatus {
    pub shelf_name: String,
    pub power_state: String,   // "on", "off", "standby"
    pub health_status: String, // "ok", "warning", "critical"
}

#[derive(Debug, Clone)]
pub struct PowerShelf {
    pub id: PowerShelfId,

    pub config: PowerShelfConfig,
    pub status: Option<PowerShelfStatus>,

    pub deleted: Option<DateTime<Utc>>,

    pub controller_state: Versioned<PowerShelfControllerState>,

    /// The result of the last attempt to change state
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,

    pub bmc_mac_address: Option<MacAddress>,

    /// Operator "force-converge this power shelf PMC now" request. When
    /// `true`, the power-shelf state controller enters `RotatingBmc` and
    /// force-converges the PMC on its next sweep, bypassing the passive
    /// site-wide gate and the device's backoff quarantine. A power shelf has
    /// exactly one BMC, so the flag's presence on the row names the target
    /// device.
    pub bmc_credential_rotation_requested: bool,

    /// When set by the API, the state controller transitions a Ready power shelf
    /// into the decommissioning workflow and clears the marker atomically.
    pub decommission_requested: bool,

    /// BMC/PMC endpoint (MAC/IP + machine-interface id) resolved from the `Bmc`
    /// machine_interface linked back to this power shelf. Populated by the
    /// standard power-shelf load query, so every consumer (handlers, state
    /// machines) gets it without re-resolving. `None` when no BMC interface is
    /// linked yet.
    pub bmc_info: Option<BmcInfo>,

    /// The rack that this power shelf is associated with.
    pub rack_id: Option<RackId>,

    /// The NVLink domain of the shelf's rack, as last reported by the rack's
    /// NMX-C endpoint. Written by NVLink Manager alongside the rack's switches;
    /// `None` until a valid domain has been observed.
    pub nvlink_domain_uuid: Option<NvLinkDomainId>,

    pub power_shelf_maintenance_requested: Option<PowerShelfMaintenanceRequest>,

    /// Set by rack maintenance to request power-shelf participation in a
    /// rack-level firmware upgrade. When the power shelf is Ready and
    /// rack-firmware reprovisioning is enabled on the controller, it
    /// transitions to `ReProvisioning`.
    pub power_shelf_reprovisioning_requested: Option<PowerShelfReprovisionRequest>,

    /// Per-device firmware upgrade status written by the rack state machine
    /// during rack-level firmware upgrades. Read by
    /// `ReProvisioning::WaitingForRackFirmwareUpgrade`.
    pub firmware_upgrade_status: Option<crate::rack::RackFirmwareUpgradeStatus>,

    // Columns for these exist, but are unused in rust code
    // pub created: DateTime<Utc>,
    // pub updated: DateTime<Utc>,
    pub metadata: Metadata,
    pub version: ConfigVersion,
    pub health_reports: HealthReportSources,
}

impl<'r> FromRow<'r, PgRow> for PowerShelf {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let controller_state: sqlx::types::Json<PowerShelfControllerState> =
            row.try_get("controller_state")?;
        let config: sqlx::types::Json<PowerShelfConfig> = row.try_get("config")?;
        let status: Option<sqlx::types::Json<PowerShelfStatus>> = row.try_get("status").ok();
        let controller_state_outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
            row.try_get("controller_state_outcome").ok();
        let power_shelf_maintenance_requested: Option<
            sqlx::types::Json<PowerShelfMaintenanceRequest>,
        > = row.try_get("power_shelf_maintenance_requested").ok();
        let power_shelf_reprovisioning_requested: Option<
            sqlx::types::Json<PowerShelfReprovisionRequest>,
        > = row.try_get("power_shelf_reprovisioning_requested").ok();
        let firmware_upgrade_status: Option<
            sqlx::types::Json<crate::rack::RackFirmwareUpgradeStatus>,
        > = row.try_get("firmware_upgrade_status").ok();

        let health_reports: HealthReportSources = row
            .try_get::<sqlx::types::Json<HealthReportSources>, _>("health_reports")
            .map(|j| j.0)
            .unwrap_or_default();
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;
        let metadata = Metadata {
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            labels: labels.0,
        };
        let bmc_info = row
            .try_get::<Option<sqlx::types::Json<BmcInfo>>, _>("bmc_info")
            .ok()
            .flatten()
            .map(|j| j.0);
        Ok(PowerShelf {
            id: row.try_get("id")?,
            config: config.0,
            status: status.map(|s| s.0),
            deleted: row.try_get("deleted")?,
            bmc_mac_address: row.try_get("bmc_mac_address").ok().flatten(),
            bmc_credential_rotation_requested: row
                .try_get("bmc_credential_rotation_requested")
                .unwrap_or(false),
            decommission_requested: row.try_get("decommission_requested").unwrap_or(false),
            bmc_info,
            controller_state: Versioned {
                value: controller_state.0,
                version: row.try_get("controller_state_version")?,
            },
            controller_state_outcome: controller_state_outcome.map(|o| o.0),
            metadata,
            version: row.try_get("version")?,
            rack_id: row.try_get("rack_id").ok().flatten(),
            nvlink_domain_uuid: row.try_get("nvlink_domain_uuid").ok().flatten(),
            power_shelf_maintenance_requested: power_shelf_maintenance_requested.map(|r| r.0),
            power_shelf_reprovisioning_requested: power_shelf_reprovisioning_requested.map(|r| r.0),
            firmware_upgrade_status: firmware_upgrade_status.map(|j| j.0),
            health_reports,
        })
    }
}

pub fn derive_power_shelf_aggregate_health(
    sources: &HealthReportSources,
) -> health_report::HealthReport {
    if let Some(replace) = &sources.replace {
        return replace.clone();
    }
    let mut output = health_report::HealthReport::empty("power-shelf-aggregate-health".to_string());
    for report in sources.merges.values() {
        output.merge(report);
    }
    output.observed_at = Some(chrono::Utc::now());
    output
}

impl PowerShelf {
    pub fn is_marked_as_deleted(&self) -> bool {
        self.deleted.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "lowercase")]
#[allow(clippy::enum_variant_names)]
pub enum PowerShelfMaintenanceOperation {
    /// Power on the PowerShelf.
    PowerOn,
    /// Power off the PowerShelf.
    PowerOff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerShelfMaintenanceRequest {
    pub requested_at: DateTime<Utc>,
    pub initiator: String,
    pub operation: PowerShelfMaintenanceOperation,
}

/// Set by an external entity (typically rack maintenance) to request power-shelf
/// participation in rack-level reprovisioning. When the power shelf is Ready and
/// rack-firmware reprovisioning is enabled, the controller transitions to
/// `ReProvisioning`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerShelfReprovisionRequest {
    pub requested_at: DateTime<Utc>,
    pub initiator: String,
    /// Rack maintenance activities that initiated this request. The power shelf
    /// controller uses these to decide whether to wait for firmware. Empty means
    /// all activities.
    #[serde(default)]
    pub activities: Vec<crate::rack::MaintenanceActivity>,
}

/// Sub-state for PowerShelfControllerState::ReProvisioning
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::enum_variant_names)]
pub enum ReProvisioningState {
    /// Rack-level firmware upgrade in progress; the rack state machine manages the
    /// upgrade and clears `power_shelf_reprovisioning_requested` when done.
    WaitingForRackFirmwareUpgrade,
}

/// State of a PowerShelf as tracked by the controller
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum PowerShelfControllerState {
    /// The PowerShelf is created in Carbide, waiting for initialization.
    Initializing,
    /// The PowerShelf is fetching data.
    FetchingData,
    /// The PowerShelf is configuring.
    Configuring,
    /// The PowerShelf is ready for use.
    Ready,

    /// The PowerShelf's BMC (PMC) credential is being converged to the staged
    /// site-wide rotation target, entered from `Ready` at lowest
    /// precedence. The shared engine owns crash-safety and per-device backoff,
    /// so this state carries only a retry budget for transient handler failures.
    RotatingBmc {
        #[serde(default)]
        retry_count: u32,
    },

    Maintenance {
        operation: PowerShelfMaintenanceOperation,
    },

    /// Rack-driven firmware wait in progress.
    ReProvisioning {
        reprovisioning_state: ReProvisioningState,
    },
    /// Managed decommissioning workflow in progress.
    Decommissioning {
        decommissioning_state: PowerShelfDecommissioningState,
    },
    /// There is error in PowerShelf; PowerShelf can not be used if it's in error.
    Error { cause: String },
    /// The PowerShelf is in the process of deleting.
    Deleting,
}

/// Progress through managed power-shelf decommissioning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum PowerShelfDecommissioningState {
    /// Site Explorer is being suppressed before the destructive reset.
    SuppressingSiteExplorer,
    /// BMC DHCP is suppressed before the BMC factory reset so post-reset discovers are ignored.
    SuppressingBmcDhcp,
    /// Issues the BMC factory reset.
    FactoryResetBmc,
    /// Waiting for the pre-reset BMC DHCP suppression to be acknowledged.
    WaitingForBmcDhcpAcknowledgement,
    /// Managed per-device credentials are being removed after factory reset.
    DeletingManagedCredentials,
    /// Terminal substate: the power shelf has been removed from managed service.
    Decommissioned,
}

/// Returns the SLA for the current state
pub fn state_sla(state: &PowerShelfControllerState, state_version: &ConfigVersion) -> StateSla {
    let time_in_state = chrono::Utc::now()
        .signed_duration_since(state_version.timestamp())
        .to_std()
        .unwrap_or(std::time::Duration::from_secs(60 * 60 * 24));

    match state {
        PowerShelfControllerState::Initializing => StateSla::with_sla(
            std::time::Duration::from_secs(slas::INITIALIZING),
            time_in_state,
        ),
        PowerShelfControllerState::FetchingData => StateSla::with_sla(
            std::time::Duration::from_secs(slas::FETCHING_DATA),
            time_in_state,
        ),
        PowerShelfControllerState::Configuring => StateSla::with_sla(
            std::time::Duration::from_secs(slas::CONFIGURING),
            time_in_state,
        ),
        PowerShelfControllerState::Ready => StateSla::no_sla(),
        PowerShelfControllerState::RotatingBmc { .. } => StateSla::with_sla(
            std::time::Duration::from_secs(slas::ROTATING_BMC),
            time_in_state,
        ),
        PowerShelfControllerState::Maintenance { .. } => StateSla::with_sla(
            std::time::Duration::from_secs(slas::MAINTENANCE),
            time_in_state,
        ),
        PowerShelfControllerState::ReProvisioning { .. } => StateSla::with_sla(
            std::time::Duration::from_secs(slas::REPROVISIONING),
            time_in_state,
        ),
        PowerShelfControllerState::Decommissioning {
            decommissioning_state,
        } => match decommissioning_state {
            PowerShelfDecommissioningState::SuppressingSiteExplorer => StateSla::with_sla(
                std::time::Duration::from_secs(slas::DECOMMISSIONING_SUPPRESSING_SITE_EXPLORER),
                time_in_state,
            ),
            PowerShelfDecommissioningState::SuppressingBmcDhcp => StateSla::with_sla(
                std::time::Duration::from_secs(slas::DECOMMISSIONING_SUPPRESSING_BMC_DHCP),
                time_in_state,
            ),
            PowerShelfDecommissioningState::FactoryResetBmc => StateSla::with_sla(
                std::time::Duration::from_secs(slas::DECOMMISSIONING_FACTORY_RESET_BMC),
                time_in_state,
            ),
            PowerShelfDecommissioningState::WaitingForBmcDhcpAcknowledgement => StateSla::with_sla(
                std::time::Duration::from_secs(slas::DECOMMISSIONING_WAITING_FOR_BMC_DHCP_ACK),
                time_in_state,
            ),
            PowerShelfDecommissioningState::DeletingManagedCredentials => StateSla::with_sla(
                std::time::Duration::from_secs(slas::DECOMMISSIONING_DELETING_MANAGED_CREDENTIALS),
                time_in_state,
            ),
            PowerShelfDecommissioningState::Decommissioned => StateSla::no_sla(),
        },
        PowerShelfControllerState::Error { .. } => StateSla::no_sla(),
        PowerShelfControllerState::Deleting => StateSla::with_sla(
            std::time::Duration::from_secs(slas::DELETING),
            time_in_state,
        ),
    }
}

#[derive(Clone, Debug, Default)]
pub struct PowerShelfSearchFilter {
    pub rack_id: Option<RackId>,
    pub deleted: crate::DeletedFilter,
    pub controller_state: Option<String>,
    pub bmc_mac: Option<MacAddress>,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    #[test]
    fn serialize_controller_state() {
        // Each controller-state variant serializes to its tagged JSON and round-trips
        // back to the same value. The op yields (serialized, deserialized) so both the
        // exact JSON string and the round-trip equality are asserted per row.
        scenarios!(
            // Serialize the state, then deserialize the produced JSON back.
            run = |state: PowerShelfControllerState| {
                let serialized = serde_json::to_string(&state).map_err(drop)?;
                let parsed =
                    serde_json::from_str::<PowerShelfControllerState>(&serialized).map_err(drop)?;
                Ok::<_, ()>((serialized, parsed))
            };
            "initializing" {
                PowerShelfControllerState::Initializing {} => Yields((
                    "{\"state\":\"initializing\"}".to_string(),
                    PowerShelfControllerState::Initializing {},
                )),
            }

            "fetching data" {
                PowerShelfControllerState::FetchingData {} => Yields((
                    "{\"state\":\"fetchingdata\"}".to_string(),
                    PowerShelfControllerState::FetchingData {},
                )),
            }

            "configuring" {
                PowerShelfControllerState::Configuring {} => Yields((
                    "{\"state\":\"configuring\"}".to_string(),
                    PowerShelfControllerState::Configuring {},
                )),
            }

            "ready" {
                PowerShelfControllerState::Ready {} => Yields((
                    "{\"state\":\"ready\"}".to_string(),
                    PowerShelfControllerState::Ready {},
                )),
            }

            "rotatingbmc carries its retry count" {
                PowerShelfControllerState::RotatingBmc { retry_count: 4 } => Yields((
                    r#"{"state":"rotatingbmc","retry_count":4}"#.to_string(),
                    PowerShelfControllerState::RotatingBmc { retry_count: 4 },
                )),
            }

            "error with cause" {
                PowerShelfControllerState::Error {
                    cause: "cause goes here".to_string(),
                } => Yields((
                    r#"{"state":"error","cause":"cause goes here"}"#.to_string(),
                    PowerShelfControllerState::Error {
                        cause: "cause goes here".to_string(),
                    },
                )),
            }

            "deleting" {
                PowerShelfControllerState::Deleting {} => Yields((
                    "{\"state\":\"deleting\"}".to_string(),
                    PowerShelfControllerState::Deleting {},
                )),
            }

            "maintenance power-on" {
                PowerShelfControllerState::Maintenance {
                    operation: PowerShelfMaintenanceOperation::PowerOn,
                } => Yields((
                    r#"{"state":"maintenance","operation":{"operation":"poweron"}}"#
                        .to_string(),
                    PowerShelfControllerState::Maintenance {
                        operation: PowerShelfMaintenanceOperation::PowerOn,
                    },
                )),
            }

            "maintenance power-off" {
                PowerShelfControllerState::Maintenance {
                    operation: PowerShelfMaintenanceOperation::PowerOff,
                } => Yields((
                    r#"{"state":"maintenance","operation":{"operation":"poweroff"}}"#
                        .to_string(),
                    PowerShelfControllerState::Maintenance {
                        operation: PowerShelfMaintenanceOperation::PowerOff,
                    },
                )),
            }

            "reprovisioning waiting for rack firmware" {
                PowerShelfControllerState::ReProvisioning {
                    reprovisioning_state: ReProvisioningState::WaitingForRackFirmwareUpgrade,
                } => Yields((
                    r#"{"state":"reprovisioning","reprovisioning_state":"WaitingForRackFirmwareUpgrade"}"#
                        .to_string(),
                    PowerShelfControllerState::ReProvisioning {
                        reprovisioning_state: ReProvisioningState::WaitingForRackFirmwareUpgrade,
                    },
                )),
            }

            "decommissioning: suppressing Site Explorer" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state:
                        PowerShelfDecommissioningState::SuppressingSiteExplorer,
                } => Yields((
                    r#"{"state":"decommissioning","decommissioning_state":{"state":"suppressingsiteexplorer"}}"#
                        .to_string(),
                    PowerShelfControllerState::Decommissioning {
                        decommissioning_state:
                            PowerShelfDecommissioningState::SuppressingSiteExplorer,
                    },
                )),
            }

            "decommissioning: suppressing BMC DHCP" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state: PowerShelfDecommissioningState::SuppressingBmcDhcp,
                } => Yields((
                    r#"{"state":"decommissioning","decommissioning_state":{"state":"suppressingbmcdhcp"}}"#
                        .to_string(),
                    PowerShelfControllerState::Decommissioning {
                        decommissioning_state: PowerShelfDecommissioningState::SuppressingBmcDhcp,
                    },
                )),
            }

            "decommissioning: factory resetting BMC" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state: PowerShelfDecommissioningState::FactoryResetBmc,
                } => Yields((
                    r#"{"state":"decommissioning","decommissioning_state":{"state":"factoryresetbmc"}}"#
                        .to_string(),
                    PowerShelfControllerState::Decommissioning {
                        decommissioning_state: PowerShelfDecommissioningState::FactoryResetBmc,
                    },
                )),
            }

            "decommissioning: waiting for BMC DHCP acknowledgement" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state:
                        PowerShelfDecommissioningState::WaitingForBmcDhcpAcknowledgement,
                } => Yields((
                    r#"{"state":"decommissioning","decommissioning_state":{"state":"waitingforbmcdhcpacknowledgement"}}"#
                        .to_string(),
                    PowerShelfControllerState::Decommissioning {
                        decommissioning_state:
                            PowerShelfDecommissioningState::WaitingForBmcDhcpAcknowledgement,
                    },
                )),
            }

            "decommissioning: deleting managed credentials" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state: PowerShelfDecommissioningState::DeletingManagedCredentials,
                } => Yields((
                    r#"{"state":"decommissioning","decommissioning_state":{"state":"deletingmanagedcredentials"}}"#
                        .to_string(),
                    PowerShelfControllerState::Decommissioning {
                        decommissioning_state:
                            PowerShelfDecommissioningState::DeletingManagedCredentials,
                    },
                )),
            }

            "decommissioning: decommissioned" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state: PowerShelfDecommissioningState::Decommissioned,
                } => Yields((
                    r#"{"state":"decommissioning","decommissioning_state":{"state":"decommissioned"}}"#
                        .to_string(),
                    PowerShelfControllerState::Decommissioning {
                        decommissioning_state: PowerShelfDecommissioningState::Decommissioned,
                    },
                )),
            }
        );
    }

    #[test]
    fn serialize_maintenance_operation() {
        // Each maintenance operation serializes to its lowercase-tagged JSON and
        // round-trips back. The op yields (serialized, deserialized), folding the
        // exact-tag and round-trip assertions into one table.
        scenarios!(
            run = |operation: PowerShelfMaintenanceOperation| {
                let serialized = serde_json::to_string(&operation).map_err(drop)?;
                let parsed = serde_json::from_str::<PowerShelfMaintenanceOperation>(&serialized)
                    .map_err(drop)?;
                Ok::<_, ()>((serialized, parsed))
            };
            "power on" {
                PowerShelfMaintenanceOperation::PowerOn => Yields((
                    r#"{"operation":"poweron"}"#.to_string(),
                    PowerShelfMaintenanceOperation::PowerOn,
                )),
            }

            "power off" {
                PowerShelfMaintenanceOperation::PowerOff => Yields((
                    r#"{"operation":"poweroff"}"#.to_string(),
                    PowerShelfMaintenanceOperation::PowerOff,
                )),
            }
        );
    }

    #[test]
    fn serialize_maintenance_request_round_trip() {
        // A maintenance request round-trips through JSON for each operation. Only the
        // operation varies; the timestamp and initiator are fixed across rows.
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-13T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let request = |operation| PowerShelfMaintenanceRequest {
            requested_at: now,
            initiator: "operator (TICKET-1)".to_string(),
            operation,
        };
        scenarios!(
            run = |operation| {
                let serialized = serde_json::to_string(&request(operation)).map_err(drop)?;
                serde_json::from_str::<PowerShelfMaintenanceRequest>(&serialized).map_err(drop)
            };
            "power on" {
                PowerShelfMaintenanceOperation::PowerOn => Yields(request(PowerShelfMaintenanceOperation::PowerOn)),
            }

            "power off" {
                PowerShelfMaintenanceOperation::PowerOff => Yields(request(PowerShelfMaintenanceOperation::PowerOff)),
            }
        );
    }

    #[test]
    fn maintenance_state_distinguishes_on_and_off() {
        let on = PowerShelfControllerState::Maintenance {
            operation: PowerShelfMaintenanceOperation::PowerOn,
        };
        let off = PowerShelfControllerState::Maintenance {
            operation: PowerShelfMaintenanceOperation::PowerOff,
        };
        assert_ne!(on, off);
    }

    #[test]
    fn controller_state_deserializes_from_tagged_json() {
        // Each tagged-JSON form parses back to its variant; malformed or unknown
        // tags are rejected. The op yields the parsed variant so both the accepted
        // shapes and the rejected ones are pinned in one table.
        scenarios!(
            run = |json: &str| serde_json::from_str::<PowerShelfControllerState>(json).map_err(drop);
            "initializing tag" {
                r#"{"state":"initializing"}"# => Yields(PowerShelfControllerState::Initializing),
            }

            "fetchingdata tag" {
                r#"{"state":"fetchingdata"}"# => Yields(PowerShelfControllerState::FetchingData),
            }

            "configuring tag" {
                r#"{"state":"configuring"}"# => Yields(PowerShelfControllerState::Configuring),
            }

            "ready tag" {
                r#"{"state":"ready"}"# => Yields(PowerShelfControllerState::Ready),
            }

            "rotatingbmc round-trips its retry count" {
                r#"{"state":"rotatingbmc","retry_count":4}"# => Yields(PowerShelfControllerState::RotatingBmc {
                    retry_count: 4,
                }),
            }

            "rotatingbmc absent retry_count defaults to 0" {
                r#"{"state":"rotatingbmc"}"# => Yields(PowerShelfControllerState::RotatingBmc {
                    retry_count: 0,
                }),
            }

            "deleting tag" {
                r#"{"state":"deleting"}"# => Yields(PowerShelfControllerState::Deleting),
            }

            "error with cause" {
                r#"{"state":"error","cause":"boom"}"# => Yields(PowerShelfControllerState::Error {
                    cause: "boom".to_string(),
                }),
            }

            "error with empty cause" {
                r#"{"state":"error","cause":""}"# => Yields(PowerShelfControllerState::Error {
                    cause: String::new(),
                }),
            }

            "maintenance power-on" {
                r#"{"state":"maintenance","operation":{"operation":"poweron"}}"# => Yields(PowerShelfControllerState::Maintenance {
                    operation: PowerShelfMaintenanceOperation::PowerOn,
                }),
            }

            "maintenance power-off" {
                r#"{"state":"maintenance","operation":{"operation":"poweroff"}}"# => Yields(PowerShelfControllerState::Maintenance {
                    operation: PowerShelfMaintenanceOperation::PowerOff,
                }),
            }

            "reprovisioning waiting for rack firmware" {
                r#"{"state":"reprovisioning","reprovisioning_state":"WaitingForRackFirmwareUpgrade"}"# => Yields(
                    PowerShelfControllerState::ReProvisioning {
                        reprovisioning_state: ReProvisioningState::WaitingForRackFirmwareUpgrade,
                    },
                ),
            }

            "unknown tag is rejected" {
                r#"{"state":"running"}"# => Fails,
            }

            "wrong-case tag is rejected" {
                r#"{"state":"Initializing"}"# => Fails,
            }

            "error without cause is rejected" {
                r#"{"state":"error"}"# => Fails,
            }

            "maintenance without operation is rejected" {
                r#"{"state":"maintenance"}"# => Fails,
            }

            "missing state tag is rejected" {
                r#"{}"# => Fails,
            }

            "not an object is rejected" {
                r#""initializing""# => Fails,
            }

            "malformed json is rejected" {
                r#"{"state":"# => Fails,
            }
        );
    }

    #[test]
    fn maintenance_operation_deserializes_from_tagged_json() {
        // The lowercase operation tags parse back to their variants; unknown or
        // wrong-case tags are rejected.
        scenarios!(
            run = |json: &str| serde_json::from_str::<PowerShelfMaintenanceOperation>(json).map_err(drop);
            "poweron tag" {
                r#"{"operation":"poweron"}"# => Yields(PowerShelfMaintenanceOperation::PowerOn),
            }

            "poweroff tag" {
                r#"{"operation":"poweroff"}"# => Yields(PowerShelfMaintenanceOperation::PowerOff),
            }

            "unknown operation is rejected" {
                r#"{"operation":"reset"}"# => Fails,
            }

            "wrong-case operation is rejected" {
                r#"{"operation":"PowerOn"}"# => Fails,
            }

            "missing operation tag is rejected" {
                r#"{}"# => Fails,
            }
        );
    }

    #[test]
    fn config_round_trips_through_json() {
        // PowerShelfConfig round-trips for present/absent optionals, empty names, and
        // numeric boundaries. The op yields the value parsed back from its own JSON.
        scenarios!(
            run = |config: PowerShelfConfig| {
                let serialized = serde_json::to_string(&config).map_err(drop)?;
                serde_json::from_str::<PowerShelfConfig>(&serialized).map_err(drop)
            };
            "all fields present" {
                PowerShelfConfig {
                    name: "shelf-1".to_string(),
                    capacity: Some(5000),
                    voltage: Some(48),
                } => Yields(PowerShelfConfig {
                    name: "shelf-1".to_string(),
                    capacity: Some(5000),
                    voltage: Some(48),
                }),
            }

            "optionals absent" {
                PowerShelfConfig {
                    name: "shelf-2".to_string(),
                    capacity: None,
                    voltage: None,
                } => Yields(PowerShelfConfig {
                    name: "shelf-2".to_string(),
                    capacity: None,
                    voltage: None,
                }),
            }

            "empty name and zero values" {
                PowerShelfConfig {
                    name: String::new(),
                    capacity: Some(0),
                    voltage: Some(0),
                } => Yields(PowerShelfConfig {
                    name: String::new(),
                    capacity: Some(0),
                    voltage: Some(0),
                }),
            }

            "u32 maxima" {
                PowerShelfConfig {
                    name: "max".to_string(),
                    capacity: Some(u32::MAX),
                    voltage: Some(u32::MAX),
                } => Yields(PowerShelfConfig {
                    name: "max".to_string(),
                    capacity: Some(u32::MAX),
                    voltage: Some(u32::MAX),
                }),
            }
        );
    }

    #[test]
    fn status_round_trips_through_json() {
        // PowerShelfStatus round-trips across each power/health string the field is
        // documented to hold, plus empty strings.
        scenarios!(
            run = |status: PowerShelfStatus| {
                let serialized = serde_json::to_string(&status).map_err(drop)?;
                serde_json::from_str::<PowerShelfStatus>(&serialized).map_err(drop)
            };
            "on / ok" {
                PowerShelfStatus {
                    shelf_name: "psu-a".to_string(),
                    power_state: "on".to_string(),
                    health_status: "ok".to_string(),
                } => Yields(PowerShelfStatus {
                    shelf_name: "psu-a".to_string(),
                    power_state: "on".to_string(),
                    health_status: "ok".to_string(),
                }),
            }

            "off / warning" {
                PowerShelfStatus {
                    shelf_name: "psu-b".to_string(),
                    power_state: "off".to_string(),
                    health_status: "warning".to_string(),
                } => Yields(PowerShelfStatus {
                    shelf_name: "psu-b".to_string(),
                    power_state: "off".to_string(),
                    health_status: "warning".to_string(),
                }),
            }

            "standby / critical" {
                PowerShelfStatus {
                    shelf_name: "psu-c".to_string(),
                    power_state: "standby".to_string(),
                    health_status: "critical".to_string(),
                } => Yields(PowerShelfStatus {
                    shelf_name: "psu-c".to_string(),
                    power_state: "standby".to_string(),
                    health_status: "critical".to_string(),
                }),
            }

            "empty strings" {
                PowerShelfStatus {
                    shelf_name: String::new(),
                    power_state: String::new(),
                    health_status: String::new(),
                } => Yields(PowerShelfStatus {
                    shelf_name: String::new(),
                    power_state: String::new(),
                    health_status: String::new(),
                }),
            }
        );
    }

    #[test]
    fn state_sla_reports_whether_an_sla_applies() {
        // `state_sla` selects an SLA bucket per controller state. Driven from an
        // epoch-old `ConfigVersion` (via `invalid()`), the time-in-state is far past
        // any finite SLA, so each SLA-bearing state reports its exact SLA duration and
        // an `above_sla` of true, while the no-SLA states report `None`/false. The op
        // yields `(sla, time_in_state_above_sla)`.
        let stale = ConfigVersion::invalid();
        let secs = |s: u64| Some(std::time::Duration::from_secs(s));
        value_scenarios!(
            run = |state: PowerShelfControllerState| {
                let result = state_sla(&state, &stale);
                (result.sla, result.time_in_state_above_sla)
            };
            "initializing has an SLA and is overdue" {
                PowerShelfControllerState::Initializing => (secs(slas::INITIALIZING), true),
            }

            "fetching-data has an SLA and is overdue" {
                PowerShelfControllerState::FetchingData => (secs(slas::FETCHING_DATA), true),
            }

            "configuring has an SLA and is overdue" {
                PowerShelfControllerState::Configuring => (secs(slas::CONFIGURING), true),
            }

            "deleting has an SLA and is overdue" {
                PowerShelfControllerState::Deleting => (secs(slas::DELETING), true),
            }

            "maintenance power-on has the maintenance SLA" {
                PowerShelfControllerState::Maintenance {
                    operation: PowerShelfMaintenanceOperation::PowerOn,
                } => (secs(slas::MAINTENANCE), true),
            }

            "maintenance power-off has the maintenance SLA" {
                PowerShelfControllerState::Maintenance {
                    operation: PowerShelfMaintenanceOperation::PowerOff,
                } => (secs(slas::MAINTENANCE), true),
            }

            "reprovisioning has the reprovisioning SLA" {
                PowerShelfControllerState::ReProvisioning {
                    reprovisioning_state: ReProvisioningState::WaitingForRackFirmwareUpgrade,
                } => (secs(slas::REPROVISIONING), true),
            }

            "rotatingbmc has the rotating-bmc SLA" {
                PowerShelfControllerState::RotatingBmc { retry_count: 0 } => (secs(slas::ROTATING_BMC), true),
            }

            "decommissioning suppressing-site-explorer has an SLA" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state:
                        PowerShelfDecommissioningState::SuppressingSiteExplorer,
                } => (secs(slas::DECOMMISSIONING_SUPPRESSING_SITE_EXPLORER), true),
            }

            "decommissioning suppressing-bmc-dhcp has an SLA" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state: PowerShelfDecommissioningState::SuppressingBmcDhcp,
                } => (secs(slas::DECOMMISSIONING_SUPPRESSING_BMC_DHCP), true),
            }

            "decommissioning factory-reset-bmc has an SLA" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state: PowerShelfDecommissioningState::FactoryResetBmc,
                } => (secs(slas::DECOMMISSIONING_FACTORY_RESET_BMC), true),
            }

            "decommissioning waiting-for-bmc-dhcp-ack has an SLA" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state:
                        PowerShelfDecommissioningState::WaitingForBmcDhcpAcknowledgement,
                } => (secs(slas::DECOMMISSIONING_WAITING_FOR_BMC_DHCP_ACK), true),
            }

            "decommissioning deleting-managed-credentials has an SLA" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state: PowerShelfDecommissioningState::DeletingManagedCredentials,
                } => (secs(slas::DECOMMISSIONING_DELETING_MANAGED_CREDENTIALS), true),
            }

            "decommissioning decommissioned carries no SLA" {
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state: PowerShelfDecommissioningState::Decommissioned,
                } => (None, false),
            }

            "ready carries no SLA" {
                PowerShelfControllerState::Ready => (None, false),
            }

            "error carries no SLA" {
                PowerShelfControllerState::Error {
                    cause: "boom".to_string(),
                } => (None, false),
            }
        );
    }

    #[test]
    fn aggregate_health_prefers_the_replace_source() {
        // When a `replace` source is set, `derive_power_shelf_aggregate_health` returns
        // it verbatim (its `observed_at` untouched), short-circuiting the merge path.
        // The op yields the derived report's source name.
        let with_replace = |source: &str| HealthReportSources {
            replace: Some(health_report::HealthReport::empty(source.to_string())),
            ..Default::default()
        };
        scenarios!(
            run = |sources: HealthReportSources| {
                Ok::<_, ()>(derive_power_shelf_aggregate_health(&sources).source)
            };
            "replace source wins" {
                with_replace("override.sre") => Yields("override.sre".to_string()),
            }

            "no replace falls back to the aggregate name" {
                HealthReportSources::default() => Yields("power-shelf-aggregate-health".to_string()),
            }
        );
    }
}
