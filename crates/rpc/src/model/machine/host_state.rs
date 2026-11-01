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

//! Projection of the compact host read, `FindHostStatesByIds`.

use model::firmware::{ComponentInventory, FirmwareComponentType};
use model::host_machine_update::{HostUpdateGateRow, UpdateBlockingReason};
use model::machine::ManagedHostStateSnapshot;
use model::site_explorer::EndpointExplorationError;

use crate as rpc;

/// The explored report of a gate row, with its installed inventory matched to
/// the catalog. `None` when the row has no report.
pub fn explored_firmware(
    row: &HostUpdateGateRow,
    inventory: Option<Vec<ComponentInventory>>,
) -> Option<rpc::forge::HostExploredFirmware> {
    let report_version = row.report_version?;
    let versions = row.report_versions.as_ref().map(|versions| &versions.0);
    Some(rpc::forge::HostExploredFirmware {
        address: row.bmc_address?.to_string(),
        vendor: row.vendor().map(|vendor| vendor.to_string()),
        model: row.report_model.clone(),
        bmc_version: versions.and_then(|versions| versions.get("bmc").cloned()),
        uefi_version: versions.and_then(|versions| versions.get("uefi").cloned()),
        report_at: Some(report_version.timestamp().into()),
        report_version: report_version.version_string(),
        last_exploration_error: row.report_error.as_ref().map(|error| error_text(&error.0)),
        waiting_for_explorer_refresh: row.waiting_for_explorer_refresh,
        firmware_inventory: inventory.map(|groups| rpc::forge::HostFirmwareInventory {
            components: groups
                .into_iter()
                .map(|group| rpc::forge::HostFirmwareComponentInventory {
                    component: match group.component {
                        // Every drive model shares the slug, so a drive keeps its
                        // stored `drive-<model>` name.
                        component @ FirmwareComponentType::Drive { .. } => component.to_string(),
                        component => component.slug().to_string(),
                    },
                    entries: group
                        .entries
                        .into_iter()
                        .map(|entry| rpc::forge::HostFirmwareInventoryEntry {
                            service_id: entry.service_id,
                            inventory_id: entry.inventory_id,
                            version: entry.version,
                        })
                        .collect(),
                })
                .collect(),
        }),
    })
}

/// The text the full explored endpoint read reports for a stored error, or the
/// raw JSON when the error no longer decodes.
fn error_text(error: &serde_json::Value) -> String {
    serde_json::from_value::<EndpointExplorationError>(error.clone())
        .ok()
        .and_then(|error| serde_json::to_string(&error).ok())
        .unwrap_or_else(|| error.to_string())
}

/// The compact state of one host. Fields shared with `Machine` are mapped as
/// the full read maps them.
pub fn host_state_from_snapshot(
    snapshot: ManagedHostStateSnapshot,
    explored: Option<rpc::forge::HostExploredFirmware>,
    update: Result<(), UpdateBlockingReason>,
    policy: Option<Result<(), UpdateBlockingReason>>,
) -> rpc::forge::HostState {
    let allocation = snapshot.is_usable_as_instance(false);
    let ManagedHostStateSnapshot {
        host_snapshot: mut host,
        aggregate_health,
        ..
    } = snapshot;
    rpc::forge::HostState {
        id: Some(host.id.into()),
        state: host.state.value.to_string(),
        state_version: host.state.version.version_string(),
        state_reason: host.controller_state_outcome.take().map(Into::into),
        rack_id: host.rack_id.take(),
        nvlink_domain_id: host
            .status
            .nvlink_info
            .as_ref()
            .map(|info| info.domain_uuid),
        instance_type_id: host
            .config
            .instance_type_id
            .as_ref()
            .map(ToString::to_string),
        bmc_ip: host.status.bmc_info.ip.map(|ip| ip.to_string()),
        bmc_mac: host.status.bmc_info.mac.map(|mac| mac.to_string()),
        dpu_ids: host.associated_dpu_machine_ids(),
        update_complete: host.status.update_complete,
        firmware_autoupdate: host.config.firmware_autoupdate,
        health_alerts: aggregate_health
            .alerts
            .into_iter()
            .map(Into::into)
            .collect(),
        quarantine: host.network_config.quarantine_state.take().map(Into::into),
        allocation: Some(rpc::forge::HostAllocation {
            allocatable: allocation.is_ok(),
            blocking_reason: allocation.as_ref().err().map(|reason| reason.code().into()),
            blocking_detail: allocation.err().map(|reason| reason.to_string()),
        }),
        explored,
        update: Some(rpc::forge::HostUpdateEligibility {
            eligible: update.is_ok(),
            blocking_reason: update.err().map(|reason| reason.code().into()),
            firmware_policy_eligible: policy.map(|policy| policy.is_ok()),
            firmware_policy_blocking_reason: policy
                .and_then(Result::err)
                .map(|reason| reason.code().into()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use carbide_test_support::value_scenarios;
    use config_version::ConfigVersion;
    use model::machine::slas::MachineSlaConfig;
    use model::site_explorer::EndpointExplorationReport;
    use model::test_support::machine_snapshot::managed_host_state_snapshot;

    use super::*;
    use crate::model::machine::ManagedHostStateSnapshotRpc;

    fn report_row() -> HostUpdateGateRow {
        HostUpdateGateRow {
            host_id: managed_host_state_snapshot().host_snapshot.id,
            bmc_address: Some([10, 0, 0, 1].into()),
            report_version: Some(ConfigVersion::initial()),
            waiting_for_explorer_refresh: Some(false),
            report_vendor: Some("Dell".to_string()),
            report_model: Some("PowerEdge R750".to_string()),
            report_versions: Some(
                BTreeMap::from([
                    ("bmc".to_string(), "7.10".to_string()),
                    ("uefi".to_string(), "1.13.2".to_string()),
                ])
                .into(),
            ),
            report_error: None,
            report_service: None,
            has_target: true,
            stable_host: true,
            versions_differ: Some(true),
            no_open_request: true,
            state_ok: true,
            autoupdate_on: true,
            window_ok: true,
        }
    }

    #[test]
    fn host_state_projection_maps_snapshot_fields() {
        let snapshot = managed_host_state_snapshot();
        let machine = snapshot
            .clone()
            .into_rpc_machine_state(None, &MachineSlaConfig::default())
            .unwrap();
        let status = machine.status.unwrap();
        let config = machine.config.unwrap();

        let state = host_state_from_snapshot(
            snapshot,
            None,
            Err(UpdateBlockingReason::NotReady),
            Some(Ok(())),
        );

        let bmc = machine.bmc_info.unwrap_or_default();
        let expected = rpc::forge::HostState {
            id: machine.id,
            state: machine.state,
            state_version: machine.state_version,
            state_reason: machine.state_reason,
            rack_id: machine.rack_id,
            nvlink_domain_id: status.nvlink_info.and_then(|info| info.domain_uuid),
            instance_type_id: config.instance_type_id,
            bmc_ip: bmc.ip,
            bmc_mac: bmc.mac,
            dpu_ids: status.associated_dpu_machine_ids,
            update_complete: status.update_complete,
            firmware_autoupdate: config.firmware_autoupdate,
            health_alerts: status.health.unwrap().alerts,
            quarantine: status.quarantine,
            allocation: Some(rpc::forge::HostAllocation {
                allocatable: true,
                blocking_reason: None,
                blocking_detail: None,
            }),
            explored: None,
            update: Some(rpc::forge::HostUpdateEligibility {
                eligible: false,
                blocking_reason: Some("not_ready".to_string()),
                firmware_policy_eligible: Some(true),
                firmware_policy_blocking_reason: None,
            }),
        };
        assert_eq!(state, expected);
    }

    #[test]
    fn explored_firmware_maps_json_null_and_missing_key_to_none() {
        value_scenarios!(
            run = |row: HostUpdateGateRow| explored_firmware(&row, None).map(|explored| (
                explored.bmc_version,
                explored.uefi_version,
                explored.last_exploration_error,
            ));
            "a report maps its versions" {
                report_row() => Some((Some("7.10".to_string()), Some("1.13.2".to_string()), None)),
            }
            "absent or null Versions and a missing key map to none" {
                HostUpdateGateRow { report_versions: None, ..report_row() } => Some((None, None, None)),
                HostUpdateGateRow {
                    report_versions: Some(BTreeMap::from([("bmc".to_string(), "7.10".to_string())]).into()),
                    ..report_row()
                } => Some((Some("7.10".to_string()), None, None)),
            }
            "no report, no explored firmware" {
                HostUpdateGateRow { report_version: None, ..report_row() } => None,
            }
        );
    }

    #[test]
    fn report_error_text_matches_full_read() {
        let error = EndpointExplorationError::ConnectionRefused {
            details: "port 443".to_string(),
        };
        let full_read =
            rpc::site_explorer::EndpointExplorationReport::from(EndpointExplorationReport {
                last_exploration_error: Some(error.clone()),
                ..Default::default()
            });
        value_scenarios!(
            run = |stored: serde_json::Value| explored_firmware(
                &HostUpdateGateRow {
                    report_error: Some(stored.into()),
                    ..report_row()
                },
                None,
            )
            .and_then(|explored| explored.last_exploration_error);
            "a decodable error matches the full read" {
                serde_json::to_value(&error).unwrap() => full_read.last_exploration_error,
            }
            "an undecodable error keeps its raw JSON" {
                serde_json::json!({"Retired": "x"}) => Some(r#"{"Retired":"x"}"#.to_string()),
            }
        );
    }

    #[test]
    fn vendor_is_display_and_unknown_is_none() {
        value_scenarios!(
            run = |report_vendor: Option<&str>| explored_firmware(
                &HostUpdateGateRow {
                    report_vendor: report_vendor.map(str::to_string),
                    ..report_row()
                },
                None,
            )
            .and_then(|explored| explored.vendor);
            "known vendors use the full read's Display form" {
                Some("Dell") => Some("dell".to_string()),
                Some("LenovoAMI") => Some("lenovoami".to_string()),
            }
            "unknown or absent vendors are none" {
                Some("Unknown") => None,
                Some("NotAVendor") => None,
                None => None,
            }
        );
    }
}
