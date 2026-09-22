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

//! Render one machine's boot-interface view (the `GetMachineBootInterfaces`
//! RPC) as an ASCII table, JSON, or YAML. The view gathers managed interface
//! rows, predictions, explored-endpoint defaults, and retained post-deletion
//! pairs around the separate desired-state record. It also reports the
//! effective owned pick, current-selection disagreement, and reconciliation.

use std::fmt::Write as _;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge as forgerpc;
use ::rpc::forge::get_machine_boot_interfaces_response::Reconciliation as RpcReconciliation;
use ::rpc::forge::get_machine_boot_interfaces_response::reconciliation::State as RpcReconciliationState;
use carbide_uuid::machine::MachineId;
use prettytable::{Cell, Row, Table};
use serde::Serialize;

use super::super::SUMMARY_LABEL_WIDTH;
use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

/// The admin-cli-side projection of `GetMachineBootInterfacesResponse`, shaped
/// for clean JSON/YAML and for table rendering. Built straight from the proto
/// response, whose nullable boot-interface fields already carry absence as
/// `Option<String>` (proto3 field presence).
#[derive(Debug, Serialize)]
struct BootInterfacesReport {
    machine_id: Option<MachineId>,
    machine_interfaces: Vec<ManagedRow>,
    predicted_interfaces: Vec<PredictedRow>,
    explored_endpoints: Vec<ExploredRow>,
    retained_interfaces: Vec<RetainedRow>,
    /// Effective owned MAC (`pick_boot_interface` over the managed rows).
    /// `None` when there is no managed candidate yet.
    effective_boot_interface_mac: Option<String>,
    /// Fully populated Redfish interface id for the effective owned pick.
    effective_boot_interface_id: Option<String>,
    /// Whether the effective owned pick, explored defaults, and declared-primary
    /// predictions contain more than one MAC.
    divergent: bool,
    /// Desired generation and the machine controller's progress toward it.
    reconciliation: Option<ReconciliationReport>,
}

/// Machine-readable and ASCII-ready view of desired boot reconciliation.
#[derive(Debug, Serialize)]
struct ReconciliationReport {
    desired_boot_interface: Option<forgerpc::MachineBootInterface>,
    selection_source: String,
    selection_updated_at: Option<String>,
    desired_version: String,
    verified_version: Option<String>,
    observed_at: Option<String>,
    is_compatibility_baseline: bool,
    reconciliation_state: String,
    machine_state: String,
    reconciling_version: Option<String>,
    failure: Option<String>,
}

#[derive(Debug, Serialize)]
struct ManagedRow {
    mac_address: String,
    primary_interface: bool,
    boot_interface_id: Option<String>,
    network_segment_type: Option<String>,
}

#[derive(Debug, Serialize)]
struct PredictedRow {
    mac_address: String,
    primary_interface: bool,
    boot_interface_id: Option<String>,
    network_segment_type: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExploredRow {
    address: String,
    boot_interface_mac: Option<String>,
    boot_interface_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct RetainedRow {
    mac_address: String,
    boot_interface_id: String,
    recorded_at: Option<String>,
}

impl From<forgerpc::GetMachineBootInterfacesResponse> for BootInterfacesReport {
    fn from(r: forgerpc::GetMachineBootInterfacesResponse) -> Self {
        BootInterfacesReport {
            machine_id: r.machine_id,
            machine_interfaces: r
                .machine_interfaces
                .into_iter()
                .map(|i| ManagedRow {
                    mac_address: i.mac_address,
                    primary_interface: i.primary_interface,
                    boot_interface_id: i.boot_interface_id,
                    network_segment_type: i.network_segment_type,
                })
                .collect(),
            predicted_interfaces: r
                .predicted_interfaces
                .into_iter()
                .map(|p| PredictedRow {
                    mac_address: p.mac_address,
                    primary_interface: p.primary_interface,
                    boot_interface_id: p.boot_interface_id,
                    network_segment_type: p.network_segment_type,
                })
                .collect(),
            explored_endpoints: r
                .explored_endpoints
                .into_iter()
                .map(|e| ExploredRow {
                    address: e.address,
                    boot_interface_mac: e.boot_interface_mac,
                    boot_interface_id: e.boot_interface_id,
                })
                .collect(),
            retained_interfaces: r
                .retained_interfaces
                .into_iter()
                .map(|t| RetainedRow {
                    mac_address: t.mac_address,
                    boot_interface_id: t.boot_interface_id,
                    recorded_at: t.recorded_at.map(|ts| ts.to_string()),
                })
                .collect(),
            effective_boot_interface_mac: r.effective_boot_interface_mac,
            effective_boot_interface_id: r.effective_boot_interface_id,
            divergent: r.divergent,
            reconciliation: r.reconciliation.map(Into::into),
        }
    }
}

impl From<RpcReconciliation> for ReconciliationReport {
    fn from(status: RpcReconciliation) -> Self {
        let reconciliation_state = RpcReconciliationState::try_from(status.reconciliation_state)
            .map_or_else(
                |_| format!("Unknown({})", status.reconciliation_state),
                |state| state.as_str_name().to_string(),
            );
        let selection_source = status.selection_source().as_str().to_string();

        Self {
            desired_boot_interface: status.desired_boot_interface,
            selection_source,
            selection_updated_at: status
                .selection_updated_at
                .map(|timestamp| timestamp.to_string()),
            desired_version: status.desired_version,
            verified_version: status.verified_version,
            observed_at: status.observed_at.map(|timestamp| timestamp.to_string()),
            is_compatibility_baseline: status.is_compatibility_baseline,
            reconciliation_state,
            machine_state: status.machine_state,
            reconciling_version: status.reconciling_version,
            failure: status.failure,
        }
    }
}

pub(super) async fn handle_boot_interfaces(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let response = api_client.get_machine_boot_interfaces(args.machine).await?;
    let report = BootInterfacesReport::from(response);

    match output_format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        OutputFormat::Yaml => {
            println!("{}", serde_yaml::to_string(&report)?);
        }
        // CSV is a poor fit for a multi-section report; AsciiTable is the
        // human-readable form. Both render the same tables.
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            print!("{}", render_tables(&report));
        }
    }
    Ok(())
}

/// One labeled table per store, then the effective owned pick, selection
/// disagreement, and desired-state reconciliation.
fn render_tables(report: &BootInterfacesReport) -> String {
    let mut out = String::new();
    let dash = |s: &Option<String>| s.as_deref().unwrap_or("-").to_string();
    let summary_width = SUMMARY_LABEL_WIDTH;

    let machine_id = report
        .machine_id
        .map(|id| id.to_string())
        .unwrap_or_default();
    let _ = writeln!(out, "Boot interfaces for machine {machine_id}");

    // Store 1: managed interface candidates and primary selection.
    let _ = writeln!(out, "\nmachine_interfaces (managed rows):");
    let mut managed = Table::new();
    managed.set_titles(Row::new(
        [
            "MAC Address",
            "Primary",
            "Boot Interface Id",
            "Segment Type",
        ]
        .into_iter()
        .map(Cell::new)
        .collect(),
    ));
    if report.machine_interfaces.is_empty() {
        managed.add_row(Row::new(vec![Cell::new("(none)")]));
    } else {
        for i in &report.machine_interfaces {
            managed.add_row(Row::new(vec![
                Cell::new(&i.mac_address),
                Cell::new(&i.primary_interface.to_string()),
                Cell::new(&dash(&i.boot_interface_id)),
                Cell::new(&dash(&i.network_segment_type)),
            ]));
        }
    }
    let _ = write!(out, "{managed}");

    // Store 2: predictions (pre-first-lease candidates).
    let _ = writeln!(out, "\npredicted_machine_interfaces:");
    let mut predicted = Table::new();
    predicted.set_titles(Row::new(
        [
            "MAC Address",
            "Primary",
            "Boot Interface Id",
            "Segment Type",
        ]
        .into_iter()
        .map(Cell::new)
        .collect(),
    ));
    if report.predicted_interfaces.is_empty() {
        predicted.add_row(Row::new(vec![Cell::new("(none)")]));
    } else {
        for p in &report.predicted_interfaces {
            predicted.add_row(Row::new(vec![
                Cell::new(&p.mac_address),
                Cell::new(&p.primary_interface.to_string()),
                Cell::new(&dash(&p.boot_interface_id)),
                Cell::new(&dash(&p.network_segment_type)),
            ]));
        }
    }
    let _ = write!(out, "{predicted}");

    // Store 3: explored endpoint default (machine-less default; shown for the
    // machine's BMC endpoints).
    let _ = writeln!(
        out,
        "\nexplored_endpoints (default for endpoints without a machine):"
    );
    let mut explored = Table::new();
    explored.set_titles(Row::new(
        [
            "Endpoint Address",
            "Boot Interface MAC",
            "Boot Interface Id",
        ]
        .into_iter()
        .map(Cell::new)
        .collect(),
    ));
    if report.explored_endpoints.is_empty() {
        explored.add_row(Row::new(vec![Cell::new("(none)")]));
    } else {
        for e in &report.explored_endpoints {
            explored.add_row(Row::new(vec![
                Cell::new(&e.address),
                Cell::new(&dash(&e.boot_interface_mac)),
                Cell::new(&dash(&e.boot_interface_id)),
            ]));
        }
    }
    let _ = write!(out, "{explored}");

    // Store 4: retained post-deletion pairs (raw, including stale records).
    let _ = writeln!(
        out,
        "\nretained_boot_interfaces (post-deletion, incl. stale):"
    );
    let mut retained = Table::new();
    retained.set_titles(Row::new(
        ["MAC Address", "Boot Interface Id", "Recorded At"]
            .into_iter()
            .map(Cell::new)
            .collect(),
    ));
    if report.retained_interfaces.is_empty() {
        retained.add_row(Row::new(vec![Cell::new("(none)")]));
    } else {
        for t in &report.retained_interfaces {
            retained.add_row(Row::new(vec![
                Cell::new(&t.mac_address),
                Cell::new(&t.boot_interface_id),
                Cell::new(&dash(&t.recorded_at)),
            ]));
        }
    }
    let _ = write!(out, "{retained}");

    // Summary: the effective pick, store agreement, and controller progress
    // toward the persisted desired target.
    let _ = writeln!(
        out,
        "\n{:<summary_width$} {}",
        "Effective boot interface MAC:",
        dash(&report.effective_boot_interface_mac)
    );
    let _ = writeln!(
        out,
        "{:<summary_width$} {}",
        "Effective boot interface id:",
        dash(&report.effective_boot_interface_id)
    );
    let _ = writeln!(
        out,
        "{:<summary_width$} {}",
        "Stores diverge on boot MAC:", report.divergent,
    );
    if let Some(reconciliation) = &report.reconciliation {
        let desired_boot_interface = reconciliation
            .desired_boot_interface
            .as_ref()
            .map(|target| match &target.interface_id {
                Some(interface_id) => format!("{} ({interface_id})", target.mac_address),
                None => target.mac_address.clone(),
            })
            .unwrap_or_else(|| "-".to_string());
        let observation = reconciliation.observed_at.as_ref().map_or_else(
            || "-".to_string(),
            |observed_at| {
                let kind = if reconciliation.is_compatibility_baseline {
                    "compatibility baseline"
                } else {
                    "Redfish verified"
                };
                format!("{observed_at} ({kind})")
            },
        );
        writeln!(
            out,
            "{:<summary_width$} {desired_boot_interface}",
            "Desired boot interface:",
        )
        .ok();
        writeln!(
            out,
            "{:<summary_width$} {}",
            "Selection source:", reconciliation.selection_source,
        )
        .ok();
        writeln!(
            out,
            "{:<summary_width$} {}",
            "Selection updated at:",
            dash(&reconciliation.selection_updated_at),
        )
        .ok();
        writeln!(
            out,
            "{:<summary_width$} {} (desired {}, verified {})",
            "Reconciliation:",
            reconciliation.reconciliation_state,
            reconciliation.desired_version,
            dash(&reconciliation.verified_version),
        )
        .ok();
        writeln!(
            out,
            "{:<summary_width$} {} (active {})",
            "Machine controller:",
            reconciliation.machine_state,
            dash(&reconciliation.reconciling_version),
        )
        .ok();
        writeln!(out, "{:<summary_width$} {observation}", "Last observation:",).ok();
        writeln!(
            out,
            "{:<summary_width$} {}",
            "Reconciliation failure:",
            dash(&reconciliation.failure),
        )
        .ok();
    } else {
        writeln!(out, "{:<summary_width$} -", "Reconciliation:").ok();
    }

    out
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// A fixed report exercising every store, a captured pair, a stale retained
    /// record, a declared primary, and divergence.
    fn sample_report() -> BootInterfacesReport {
        BootInterfacesReport {
            machine_id: None,
            machine_interfaces: vec![ManagedRow {
                mac_address: "aa:bb:cc:00:00:01".to_string(),
                primary_interface: true,
                boot_interface_id: Some("NIC.Slot.1-1-1".to_string()),
                network_segment_type: Some("HostInband".to_string()),
            }],
            predicted_interfaces: vec![PredictedRow {
                mac_address: "aa:bb:cc:00:00:02".to_string(),
                primary_interface: false,
                boot_interface_id: None,
                network_segment_type: Some("Admin".to_string()),
            }],
            explored_endpoints: vec![ExploredRow {
                address: "10.0.0.5".to_string(),
                // A different NIC than the effective managed pick -> divergence.
                boot_interface_mac: Some("aa:bb:cc:00:00:09".to_string()),
                boot_interface_id: Some("NIC.Slot.9-1-1".to_string()),
            }],
            retained_interfaces: vec![RetainedRow {
                mac_address: "aa:bb:cc:00:00:03".to_string(),
                boot_interface_id: "NIC.Old.1-1-1".to_string(),
                recorded_at: Some("2026-06-01T00:00:00Z".to_string()),
            }],
            effective_boot_interface_mac: Some("aa:bb:cc:00:00:01".to_string()),
            effective_boot_interface_id: Some("NIC.Slot.1-1-1".to_string()),
            divergent: true,
            reconciliation: Some(ReconciliationReport {
                desired_boot_interface: Some(forgerpc::MachineBootInterface {
                    mac_address: "aa:bb:cc:00:00:01".to_string(),
                    interface_id: Some("NIC.Slot.1-1-1".to_string()),
                }),
                selection_source: "RedfishUefiPci".to_string(),
                selection_updated_at: Some("2026-06-01T12:00:00Z".to_string()),
                desired_version: "V7-T700".to_string(),
                verified_version: Some("V6-T600".to_string()),
                observed_at: Some("2026-06-02T00:00:00Z".to_string()),
                is_compatibility_baseline: false,
                reconciliation_state: "Failed".to_string(),
                machine_state: "BootConfiguring/Failed".to_string(),
                reconciling_version: Some("V7-T700".to_string()),
                failure: Some("BIOS job retries exhausted".to_string()),
            }),
        }
    }

    #[test]
    fn reconciliation_report_names_known_and_unknown_states() {
        value_scenarios!(
            run = |reconciliation_state| {
                ReconciliationReport::from(RpcReconciliation {
                    reconciliation_state,
                    ..Default::default()
                })
                .reconciliation_state
            };
            "known state uses its protobuf name" {
                RpcReconciliationState::Pending as i32 => "Pending".to_string(),
            }

            "unknown state preserves its numeric value" {
                i32::MAX => format!("Unknown({})", i32::MAX),
            }
        );
    }

    #[test]
    fn ascii_table_shows_each_store_and_summary() {
        let table = render_tables(&sample_report());

        // Section labels.
        assert!(table.contains("machine_interfaces (managed rows):"));
        assert!(table.contains("predicted_machine_interfaces:"));
        assert!(table.contains("explored_endpoints"));
        assert!(table.contains("retained_boot_interfaces"));

        // The boot_interface_id of the managed row.
        assert!(table.contains("NIC.Slot.1-1-1"));
        // The primary flag.
        assert!(table.contains("true"));
        // The retained record's recorded_at.
        assert!(table.contains("2026-06-01T00:00:00Z"));
        // The effective pick and divergence flag.
        assert!(table.contains("Effective boot interface MAC: aa:bb:cc:00:00:01"));
        assert!(table.contains("Stores diverge on boot MAC:   true"));
        assert!(table.contains("Desired boot interface:       aa:bb:cc:00:00:01 (NIC.Slot.1-1-1)"));
        assert!(table.contains("Selection source:             RedfishUefiPci"));
        assert!(table.contains("Selection updated at:         2026-06-01T12:00:00Z"));
        assert!(
            table.contains(
                "Reconciliation:               Failed (desired V7-T700, verified V6-T600)"
            )
        );
        assert!(table.contains("Machine controller:           BootConfiguring/Failed"));
        assert!(table.contains("Reconciliation failure:       BIOS job retries exhausted"));
    }

    #[test]
    fn json_round_trips_with_every_field() {
        let json = serde_json::to_string_pretty(&sample_report()).expect("serialize json");

        // Field presence.
        assert!(json.contains("\"boot_interface_id\""));
        assert!(json.contains("NIC.Slot.1-1-1"));
        assert!(json.contains("\"recorded_at\""));
        assert!(json.contains("2026-06-01T00:00:00Z"));
        assert!(json.contains("\"primary_interface\": true"));
        assert!(json.contains("\"divergent\": true"));
        assert!(json.contains("\"reconciliation\""));
        assert!(json.contains("\"reconciliation_state\": \"Failed\""));
        assert!(json.contains("\"selection_source\": \"RedfishUefiPci\""));

        // Round-trips into a generic JSON value with the expected structure.
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(value["divergent"], serde_json::Value::Bool(true));
        assert_eq!(value["machine_interfaces"][0]["primary_interface"], true);
        assert_eq!(
            value["machine_interfaces"][0]["boot_interface_id"],
            "NIC.Slot.1-1-1"
        );
        assert_eq!(
            value["retained_interfaces"][0]["recorded_at"],
            "2026-06-01T00:00:00Z"
        );
        assert_eq!(value["effective_boot_interface_mac"], "aa:bb:cc:00:00:01");
        assert_eq!(value["reconciliation"]["desired_version"], "V7-T700");
        assert_eq!(
            value["reconciliation"]["selection_updated_at"],
            "2026-06-01T12:00:00Z"
        );
        assert_eq!(
            value["reconciliation"]["failure"],
            "BIOS job retries exhausted"
        );
    }

    /// The proto -> report conversion: absent fields (proto3 field presence,
    /// `None`) stay `None`, present ones pass through, and a `Timestamp`
    /// renders as RFC 3339.
    #[test]
    fn from_proto_response_maps_fields() {
        let response = forgerpc::GetMachineBootInterfacesResponse {
            machine_id: None,
            machine_interfaces: vec![forgerpc::MachineInterfaceBootInterface {
                mac_address: "aa:bb:cc:00:00:01".to_string(),
                primary_interface: true,
                boot_interface_id: Some("NIC.Slot.1-1-1".to_string()),
                network_segment_type: Some("HostInband".to_string()),
                interface_id: None,
            }],
            predicted_interfaces: vec![],
            explored_endpoints: vec![forgerpc::ExploredBootInterface {
                address: "10.0.0.5".to_string(),
                // An absent boot MAC -> `None` in the report.
                boot_interface_mac: None,
                boot_interface_id: Some("NIC.Slot.9-1-1".to_string()),
            }],
            retained_interfaces: vec![forgerpc::RetainedBootInterface {
                mac_address: "aa:bb:cc:00:00:03".to_string(),
                boot_interface_id: "NIC.Old.1-1-1".to_string(),
                // The default Timestamp is the unix epoch; Display renders RFC 3339.
                recorded_at: Some(Default::default()),
            }],
            effective_boot_interface_mac: Some("aa:bb:cc:00:00:01".to_string()),
            // Absent -> `None`.
            effective_boot_interface_id: None,
            divergent: false,
            default_boot_interface: None,
            predicted_boot_interface: None,
            reconciliation: Some(RpcReconciliation {
                desired_boot_interface: Some(forgerpc::MachineBootInterface {
                    mac_address: "aa:bb:cc:00:00:01".to_string(),
                    interface_id: Some("NIC.Slot.1-1-1".to_string()),
                }),
                desired_version: "V7-T700".to_string(),
                verified_version: Some("V6-T600".to_string()),
                observed_at: Some(Default::default()),
                is_compatibility_baseline: true,
                reconciliation_state: RpcReconciliationState::Pending as i32,
                machine_state: "Assigned/Ready".to_string(),
                reconciling_version: None,
                failure: None,
                selection_source: forgerpc::BootInterfaceSelectionSource::RedfishSerialNumber
                    as i32,
                selection_updated_at: Some(Default::default()),
            }),
        };

        let report = BootInterfacesReport::from(response);

        // Present values pass through; absent ones stay `None`.
        assert_eq!(
            report.machine_interfaces[0].boot_interface_id.as_deref(),
            Some("NIC.Slot.1-1-1")
        );
        assert!(report.machine_interfaces[0].primary_interface);
        assert_eq!(report.explored_endpoints[0].boot_interface_mac, None);
        assert_eq!(report.effective_boot_interface_id, None);
        assert_eq!(
            report.effective_boot_interface_mac.as_deref(),
            Some("aa:bb:cc:00:00:01")
        );
        // The Timestamp renders as an RFC 3339 string (epoch default here).
        assert_eq!(
            report.retained_interfaces[0].recorded_at.as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        let reconciliation = report
            .reconciliation
            .expect("desired reconciliation should be mapped");
        assert_eq!(reconciliation.reconciliation_state, "Pending");
        assert_eq!(reconciliation.selection_source, "RedfishSerialNumber");
        assert_eq!(
            reconciliation.selection_updated_at.as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert_eq!(reconciliation.desired_version, "V7-T700");
        assert_eq!(reconciliation.verified_version.as_deref(), Some("V6-T600"));
        assert_eq!(
            reconciliation.observed_at.as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert!(reconciliation.is_compatibility_baseline);
        assert_eq!(reconciliation.machine_state, "Assigned/Ready");
    }

    #[test]
    fn reconciliation_projection_preserves_selection_source_and_time() {
        value_scenarios!(run = |(selection_source, selection_updated_at)| {
            let report = ReconciliationReport::from(RpcReconciliation {
                selection_source,
                selection_updated_at,
                ..Default::default()
            });
            (report.selection_source, report.selection_updated_at)
        };
            "wire sentinel with absent time" {
                (
                    forgerpc::BootInterfaceSelectionSource::Unspecified as i32,
                    None,
                ) => ("Unspecified".to_string(), None),
            }
            "persisted unknown with absent time" {
                (
                    forgerpc::BootInterfaceSelectionSource::LegacyUnknown as i32,
                    None,
                ) => ("LegacyUnknown".to_string(), None),
            }
            "operator with present time" {
                (
                    forgerpc::BootInterfaceSelectionSource::Operator as i32,
                    Some(Default::default()),
                ) => (
                    "Operator".to_string(),
                    Some("1970-01-01T00:00:00Z".to_string()),
                ),
            }
        );
    }
}
