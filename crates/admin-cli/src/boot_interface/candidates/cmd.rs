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

//! Render one machine's boot-interface candidates: every NIC the machine
//! offers (managed `machine_interfaces` rows and pre-first-lease predictions),
//! annotated with the selection/default picks the system computes among them.
//! Current, default (primary flag masked), predicted, and explored-endpoint
//! picks all arrive server-computed on `GetMachineBootInterfaces`; this module
//! only matches rows against them, it never re-derives selection logic. The
//! persisted desired target and controller progress live in `boot-interface
//! show`, not in this narrower candidate view.

use std::fmt::Write as _;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge as forgerpc;
use carbide_uuid::machine::MachineId;
use prettytable::{Cell, Row, Table};
use serde::Serialize;

use super::super::SUMMARY_LABEL_WIDTH;
use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

/// Where a candidate row lives: a managed `machine_interfaces` row, or a
/// `predicted_machine_interfaces` row from the pre-first-lease window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CandidateSource {
    Managed,
    Predicted,
}

/// One candidate NIC with the marks the picks put on it. The pick messages
/// identify targets by MAC (plus a Redfish id), not by managed row id, so
/// duplicate MACs across segments both receive the same MAC-level mark.
#[derive(Debug, Serialize)]
struct CandidateRow {
    mac_address: String,
    /// The `machine_interfaces` row id -- the exact form `boot-interface set`
    /// accepts. Absent for predictions (they have no managed row yet) and when
    /// the API server predates row-id reporting.
    interface_id: Option<String>,
    /// Vendor-named Redfish `EthernetInterface.Id`, when captured.
    boot_interface_id: Option<String>,
    network_segment_type: Option<String>,
    source: CandidateSource,
    primary_interface: bool,
    /// Whether selection considers this row: a declared primary is eligible
    /// regardless of segment, while non-primary underlay rows are excluded
    /// from the automatic fallback. Display only; the actual picks are
    /// server-computed.
    eligible: bool,
    /// This NIC is the effective managed/predicted selection. Persisted
    /// desired state may differ and is intentionally outside this view.
    current: bool,
    /// This NIC is the automatic pick with the primary flag masked -- what
    /// the system would choose if nothing were declared.
    default: bool,
    /// This NIC matches site-explorer's stored default for one of the
    /// machine's BMC endpoints.
    explored_default: bool,
}

/// The candidates view: the rows plus the picks they were matched against.
#[derive(Debug, Serialize)]
struct CandidatesReport {
    machine_id: Option<MachineId>,
    candidates: Vec<CandidateRow>,
    /// The effective managed pick, else the predicted pick when managed rows
    /// offer no candidate. Persisted desired state may differ.
    current_boot_interface_mac: Option<String>,
    current_boot_interface_id: Option<String>,
    current_source: Option<CandidateSource>,
    /// The automatic pick with the primary flag masked (managed rows).
    default_boot_interface_mac: Option<String>,
    default_boot_interface_id: Option<String>,
    /// The `pick_boot_prediction` result for the pre-first-lease window.
    predicted_boot_interface_mac: Option<String>,
    predicted_boot_interface_id: Option<String>,
    /// Every boot pair recorded on the machine's explored BMC endpoints --
    /// the MAC plus the Redfish interface id when captured.
    explored_boot_interfaces: Vec<ExploredDefault>,
    /// Why the current desired target was selected, when it is initialized.
    desired_selection_source: Option<String>,
    /// Time the current desired target and source decision was established.
    /// Adding a Redfish interface ID for the same MAC preserves it.
    desired_selection_updated_at: Option<String>,
    divergent: bool,
}

/// An explored-endpoint default: the pair site-explorer recorded for one of
/// the machine's BMC endpoints.
#[derive(Debug, Serialize)]
struct ExploredDefault {
    mac_address: String,
    boot_interface_id: Option<String>,
}

impl From<forgerpc::GetMachineBootInterfacesResponse> for CandidatesReport {
    fn from(r: forgerpc::GetMachineBootInterfacesResponse) -> Self {
        let desired_selection_source = r
            .reconciliation
            .as_ref()
            .map(|status| status.selection_source().as_str().to_string());
        let desired_selection_updated_at = r.reconciliation.as_ref().and_then(|status| {
            status
                .selection_updated_at
                .map(|timestamp| timestamp.to_string())
        });

        // The default and predicted picks arrive as `MachineBootInterface`
        // messages -- flatten to the strings the report carries.
        let default_mac = r
            .default_boot_interface
            .as_ref()
            .map(|b| b.mac_address.clone());
        let default_id = r
            .default_boot_interface
            .as_ref()
            .and_then(|b| b.interface_id.clone());
        let predicted_mac = r
            .predicted_boot_interface
            .as_ref()
            .map(|b| b.mac_address.clone());
        let predicted_id = r
            .predicted_boot_interface
            .as_ref()
            .and_then(|b| b.interface_id.clone());

        // The current pick follows the resolvers' order: managed rows first,
        // predictions only when the managed rows offer nothing.
        let (current_mac, current_id, current_source) = if r.effective_boot_interface_mac.is_some()
        {
            (
                r.effective_boot_interface_mac.clone(),
                r.effective_boot_interface_id.clone(),
                Some(CandidateSource::Managed),
            )
        } else if predicted_mac.is_some() {
            (
                predicted_mac.clone(),
                predicted_id.clone(),
                Some(CandidateSource::Predicted),
            )
        } else {
            (None, None, None)
        };

        let explored_defaults: Vec<ExploredDefault> = r
            .explored_endpoints
            .iter()
            .filter_map(|e| {
                e.boot_interface_mac.clone().map(|mac| ExploredDefault {
                    mac_address: mac,
                    boot_interface_id: e.boot_interface_id.clone(),
                })
            })
            .collect();
        let explored_macs: Vec<String> = explored_defaults
            .iter()
            .map(|d| d.mac_address.clone())
            .collect();

        let mark = |mac: &str, pick: &Option<String>| pick.as_deref() == Some(mac);
        // Primary wins regardless of segment; non-primary underlay rows are
        // excluded from the automatic fallback. Match against the segment
        // type's display form (`NetworkSegmentType::Underlay` renders as
        // "tor"). Display only -- the picks themselves arrive server-computed.
        let underlay = model::network_segment::NetworkSegmentType::Underlay.to_string();
        let is_eligible = |primary, segment| primary || segment != Some(underlay.as_str());

        let mut candidates: Vec<CandidateRow> = r
            .machine_interfaces
            .iter()
            .map(|i| CandidateRow {
                mac_address: i.mac_address.clone(),
                interface_id: i.interface_id.map(|id| id.to_string()),
                boot_interface_id: i.boot_interface_id.clone(),
                network_segment_type: i.network_segment_type.clone(),
                source: CandidateSource::Managed,
                primary_interface: i.primary_interface,
                eligible: is_eligible(i.primary_interface, i.network_segment_type.as_deref()),
                current: mark(&i.mac_address, &current_mac),
                default: mark(&i.mac_address, &default_mac),
                explored_default: explored_macs.contains(&i.mac_address),
            })
            .collect();
        candidates.extend(r.predicted_interfaces.iter().map(|p| CandidateRow {
            mac_address: p.mac_address.clone(),
            interface_id: None,
            boot_interface_id: p.boot_interface_id.clone(),
            network_segment_type: p.network_segment_type.clone(),
            source: CandidateSource::Predicted,
            primary_interface: p.primary_interface,
            eligible: is_eligible(p.primary_interface, p.network_segment_type.as_deref()),
            current: mark(&p.mac_address, &current_mac),
            default: mark(&p.mac_address, &default_mac),
            explored_default: explored_macs.contains(&p.mac_address),
        }));

        CandidatesReport {
            machine_id: r.machine_id,
            candidates,
            current_boot_interface_mac: current_mac,
            current_boot_interface_id: current_id,
            current_source,
            default_boot_interface_mac: default_mac,
            default_boot_interface_id: default_id,
            predicted_boot_interface_mac: predicted_mac,
            predicted_boot_interface_id: predicted_id,
            explored_boot_interfaces: explored_defaults,
            desired_selection_source,
            desired_selection_updated_at,
            divergent: r.divergent,
        }
    }
}

pub(super) async fn handle_candidates(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let response = api_client.get_machine_boot_interfaces(args.machine).await?;
    let report = CandidatesReport::from(response);

    match output_format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        OutputFormat::Yaml => {
            println!("{}", serde_yaml::to_string(&report)?);
        }
        // CSV is a poor fit for a table-plus-summary report; AsciiTable is
        // the human-readable form. Both render the same output.
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            print!("{}", render_candidates(&report));
        }
    }
    Ok(())
}

/// One row per candidate NIC, then a summary block naming each pick.
fn render_candidates(report: &CandidatesReport) -> String {
    let mut out = String::new();
    let dash = |s: &Option<String>| s.as_deref().unwrap_or("-").to_string();
    let summary_width = SUMMARY_LABEL_WIDTH;

    let machine_id = report
        .machine_id
        .map(|id| id.to_string())
        .unwrap_or_default();
    let _ = writeln!(out, "Boot-interface candidates for machine {machine_id}");
    let _ = writeln!(out);

    let mut table = Table::new();
    table.set_titles(Row::new(
        [
            "MAC Address",
            "Interface UUID",
            "Boot Interface Id",
            "Segment",
            "Source",
            "Primary",
            "Eligible",
            "Markers",
        ]
        .into_iter()
        .map(Cell::new)
        .collect(),
    ));
    if report.candidates.is_empty() {
        table.add_row(Row::new(vec![Cell::new("(none)")]));
    } else {
        for c in &report.candidates {
            let mut markers = Vec::new();
            if c.current {
                markers.push("current");
            }
            if c.default {
                markers.push("default");
            }
            if c.explored_default {
                markers.push("explored");
            }
            let markers = if markers.is_empty() {
                "-".to_string()
            } else {
                markers.join(",")
            };
            let source = match c.source {
                CandidateSource::Managed => "managed",
                CandidateSource::Predicted => "predicted",
            };
            let eligible = if c.eligible {
                "yes"
            } else {
                "no (non-primary underlay)"
            };
            table.add_row(Row::new(vec![
                Cell::new(&c.mac_address),
                Cell::new(&dash(&c.interface_id)),
                Cell::new(&dash(&c.boot_interface_id)),
                Cell::new(&dash(&c.network_segment_type)),
                Cell::new(source),
                Cell::new(&c.primary_interface.to_string()),
                Cell::new(eligible),
                Cell::new(&markers),
            ]));
        }
    }
    let _ = write!(out, "{table}");

    // Summary: each pick on its own line, then divergence. Every pick renders
    // whatever halves of its boot pair are recorded -- the MAC plus the
    // Redfish interface id when captured.
    let pair = |mac: Option<&str>, id: Option<&str>| match (mac, id) {
        (Some(mac), Some(id)) => format!("{mac} ({id})"),
        (Some(mac), None) => mac.to_string(),
        // An id without a MAC is not a state the stores produce; render it
        // anyway rather than hide data.
        (None, Some(id)) => format!("({id})"),
        (None, None) => "-".to_string(),
    };
    let current_source = match report.current_source {
        Some(CandidateSource::Managed) => "",
        Some(CandidateSource::Predicted) => " (from a prediction -- not yet leased)",
        None => "",
    };
    let _ = writeln!(
        out,
        "\n{:<summary_width$} {}{}",
        "Current boot interface:",
        pair(
            report.current_boot_interface_mac.as_deref(),
            report.current_boot_interface_id.as_deref(),
        ),
        current_source,
    );
    let _ = writeln!(
        out,
        "{:<summary_width$} {}",
        "Default (auto) pick:",
        pair(
            report.default_boot_interface_mac.as_deref(),
            report.default_boot_interface_id.as_deref(),
        ),
    );
    // The pre-first-lease story only matters while predictions exist.
    let predictions: Vec<_> = report
        .candidates
        .iter()
        .filter(|c| c.source == CandidateSource::Predicted)
        .collect();
    if !predictions.is_empty() {
        // The refusal explanation is only printed when the rows themselves
        // show its precondition -- several eligible predictions, none declared
        // primary. An absent pick without that precondition means the API
        // server predates pick reporting, and claiming a refusal would be
        // wrong; a bare dash is honest in both worlds.
        let refusal_visible = predictions.iter().filter(|p| p.eligible).count() >= 2
            && !predictions.iter().any(|p| p.primary_interface);
        match (&report.predicted_boot_interface_mac, refusal_visible) {
            (Some(_), _) => {
                let _ = writeln!(
                    out,
                    "{:<summary_width$} {}",
                    "Predicted pick:",
                    pair(
                        report.predicted_boot_interface_mac.as_deref(),
                        report.predicted_boot_interface_id.as_deref(),
                    )
                );
            }
            (None, false) => {
                let _ = writeln!(out, "{:<summary_width$} -", "Predicted pick:");
            }
            (None, true) => {
                let _ = writeln!(
                    out,
                    "{:<summary_width$} none -- multiple predictions and none declared \
                     primary, so the system refuses to guess (declare one via the expected \
                     machine's `interfaces[].primary`)",
                    "Predicted pick:",
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "{:<summary_width$} {}",
        "Explored default(s):",
        if report.explored_boot_interfaces.is_empty() {
            "-".to_string()
        } else {
            report
                .explored_boot_interfaces
                .iter()
                .map(|d| pair(Some(d.mac_address.as_str()), d.boot_interface_id.as_deref()))
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
    let _ = writeln!(
        out,
        "{:<summary_width$} {}",
        "Stores diverge on boot MAC:", report.divergent,
    );
    let _ = writeln!(
        out,
        "{:<summary_width$} {}",
        "Desired selection source:",
        dash(&report.desired_selection_source),
    );
    let _ = writeln!(
        out,
        "{:<summary_width$} {}",
        "Selection updated at:",
        dash(&report.desired_selection_updated_at),
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response with a managed primary (current), a lower-MAC managed row the
    /// masked pick prefers (default), an underlay row (ineligible), and a
    /// prediction; the explored default names the primary's MAC.
    fn sample_response() -> forgerpc::GetMachineBootInterfacesResponse {
        forgerpc::GetMachineBootInterfacesResponse {
            machine_id: None,
            machine_interfaces: vec![
                forgerpc::MachineInterfaceBootInterface {
                    mac_address: "aa:bb:cc:00:00:02".to_string(),
                    primary_interface: true,
                    boot_interface_id: Some("NIC.Slot.2-1-1".to_string()),
                    network_segment_type: Some("admin".to_string()),
                    interface_id: None,
                },
                forgerpc::MachineInterfaceBootInterface {
                    mac_address: "aa:bb:cc:00:00:01".to_string(),
                    primary_interface: false,
                    boot_interface_id: Some("NIC.Slot.1-1-1".to_string()),
                    network_segment_type: Some("host_inband".to_string()),
                    interface_id: None,
                },
                forgerpc::MachineInterfaceBootInterface {
                    mac_address: "aa:bb:cc:00:00:00".to_string(),
                    primary_interface: false,
                    boot_interface_id: None,
                    network_segment_type: Some("tor".to_string()),
                    interface_id: None,
                },
            ],
            predicted_interfaces: vec![forgerpc::PredictedBootInterface {
                mac_address: "aa:bb:cc:00:00:09".to_string(),
                primary_interface: false,
                boot_interface_id: None,
                network_segment_type: Some("host_inband".to_string()),
            }],
            explored_endpoints: vec![forgerpc::ExploredBootInterface {
                address: "192.0.2.10".to_string(),
                boot_interface_mac: Some("aa:bb:cc:00:00:02".to_string()),
                boot_interface_id: Some("NIC.Slot.2-1-1".to_string()),
            }],
            retained_interfaces: vec![],
            effective_boot_interface_mac: Some("aa:bb:cc:00:00:02".to_string()),
            effective_boot_interface_id: Some("NIC.Slot.2-1-1".to_string()),
            divergent: false,
            default_boot_interface: Some(forgerpc::MachineBootInterface {
                mac_address: "aa:bb:cc:00:00:01".to_string(),
                interface_id: Some("NIC.Slot.1-1-1".to_string()),
            }),
            predicted_boot_interface: Some(forgerpc::MachineBootInterface {
                mac_address: "aa:bb:cc:00:00:09".to_string(),
                interface_id: None,
            }),
            reconciliation: Some(
                forgerpc::get_machine_boot_interfaces_response::Reconciliation {
                    selection_source: forgerpc::BootInterfaceSelectionSource::RedfishSerialNumber
                        as i32,
                    selection_updated_at: Some(Default::default()),
                    ..Default::default()
                },
            ),
        }
    }

    #[test]
    fn markers_land_on_the_right_rows() {
        let report = CandidatesReport::from(sample_response());

        // Owned rows first, predictions after.
        assert_eq!(report.candidates.len(), 4);

        let primary = &report.candidates[0];
        assert!(primary.current, "the managed primary is the current pick");
        assert!(!primary.default, "the masked pick prefers the lower MAC");
        assert!(primary.explored_default);
        assert_eq!(primary.source, CandidateSource::Managed);

        let lower = &report.candidates[1];
        assert!(!lower.current);
        assert!(lower.default, "the lower non-underlay MAC is the default");

        let underlay = &report.candidates[2];
        assert!(
            !underlay.eligible,
            "non-primary underlay rows are excluded from the fallback"
        );
        assert!(!underlay.current && !underlay.default);

        let prediction = &report.candidates[3];
        assert_eq!(prediction.source, CandidateSource::Predicted);
        assert!(
            !prediction.current,
            "predictions are not current while managed rows offer a pick"
        );

        // The current pick is the effective managed one.
        assert_eq!(report.current_source, Some(CandidateSource::Managed));
        assert_eq!(
            report.current_boot_interface_mac.as_deref(),
            Some("aa:bb:cc:00:00:02")
        );
    }

    #[test]
    fn current_falls_back_to_the_predicted_pick_without_managed_rows() {
        let response = forgerpc::GetMachineBootInterfacesResponse {
            machine_interfaces: vec![],
            effective_boot_interface_mac: None,
            effective_boot_interface_id: None,
            ..sample_response()
        };

        let report = CandidatesReport::from(response);

        assert_eq!(report.current_source, Some(CandidateSource::Predicted));
        assert_eq!(
            report.current_boot_interface_mac.as_deref(),
            Some("aa:bb:cc:00:00:09")
        );
        let prediction = report
            .candidates
            .iter()
            .find(|c| c.source == CandidateSource::Predicted)
            .expect("the prediction survives");
        assert!(
            prediction.current,
            "the predicted pick is current in the pre-first-lease window"
        );

        let rendered = render_candidates(&report);
        assert!(
            rendered.contains("(from a prediction -- not yet leased)"),
            "a prediction-sourced current pick says so in the summary"
        );
    }

    #[test]
    fn a_declared_primary_underlay_row_is_eligible() {
        let mut response = sample_response();
        response.machine_interfaces[0].primary_interface = false;
        response.machine_interfaces[2].primary_interface = true;
        response.effective_boot_interface_mac = Some("aa:bb:cc:00:00:00".to_string());
        response.effective_boot_interface_id = None;

        let report = CandidatesReport::from(response);
        let underlay = &report.candidates[2];

        assert!(underlay.eligible);
        assert!(underlay.current);
    }

    #[test]
    fn ascii_render_shows_markers_and_summary() {
        let rendered = render_candidates(&CandidatesReport::from(sample_response()));

        assert!(rendered.contains("current,explored"));
        assert!(rendered.contains("no (non-primary underlay)"));
        assert!(
            rendered.contains("Current boot interface:       aa:bb:cc:00:00:02 (NIC.Slot.2-1-1)\n")
        );
        assert!(
            rendered.contains("Default (auto) pick:          aa:bb:cc:00:00:01 (NIC.Slot.1-1-1)\n")
        );
        assert!(rendered.contains("Predicted pick:               aa:bb:cc:00:00:09"));
        assert!(
            rendered.contains("Explored default(s):          aa:bb:cc:00:00:02 (NIC.Slot.2-1-1)")
        );
        assert!(rendered.contains("Stores diverge on boot MAC:   false"));
        assert!(rendered.contains("Desired selection source:     RedfishSerialNumber"));
        assert!(rendered.contains("Selection updated at:         1970-01-01T00:00:00Z"));
    }

    #[test]
    fn ascii_render_explains_the_refusal_to_guess() {
        // Two undeclared predictions, no managed rows: the server reports no
        // predicted pick, and the render says why instead of showing a bare
        // dash.
        let response = forgerpc::GetMachineBootInterfacesResponse {
            machine_interfaces: vec![],
            explored_endpoints: vec![],
            effective_boot_interface_mac: None,
            effective_boot_interface_id: None,
            default_boot_interface: None,
            predicted_boot_interface: None,
            predicted_interfaces: vec![
                forgerpc::PredictedBootInterface {
                    mac_address: "aa:bb:cc:00:00:08".to_string(),
                    primary_interface: false,
                    boot_interface_id: None,
                    network_segment_type: Some("host_inband".to_string()),
                },
                forgerpc::PredictedBootInterface {
                    mac_address: "aa:bb:cc:00:00:09".to_string(),
                    primary_interface: false,
                    boot_interface_id: None,
                    network_segment_type: Some("host_inband".to_string()),
                },
            ],
            ..sample_response()
        };

        let rendered = render_candidates(&CandidatesReport::from(response));

        assert!(rendered.contains("Current boot interface:       -"));
        assert!(rendered.contains("refuses to guess"));
    }

    #[test]
    fn an_absent_predicted_pick_without_the_refusal_precondition_stays_a_dash() {
        // A declared-primary prediction with the pick fields absent -- an API
        // server that predates pick reporting. A new server would have picked
        // this prediction, so the render must NOT claim the system refused to
        // guess; it shows a plain dash instead.
        let response = forgerpc::GetMachineBootInterfacesResponse {
            machine_interfaces: vec![],
            explored_endpoints: vec![],
            effective_boot_interface_mac: None,
            effective_boot_interface_id: None,
            default_boot_interface: None,
            predicted_boot_interface: None,
            predicted_interfaces: vec![forgerpc::PredictedBootInterface {
                mac_address: "aa:bb:cc:00:00:08".to_string(),
                primary_interface: true,
                boot_interface_id: None,
                network_segment_type: Some("host_inband".to_string()),
            }],
            ..sample_response()
        };

        let rendered = render_candidates(&CandidatesReport::from(response));

        assert!(rendered.contains("Predicted pick:               -"));
        assert!(!rendered.contains("refuses to guess"));
    }

    #[test]
    fn json_round_trips_with_marker_fields() {
        let json = serde_json::to_string_pretty(&CandidatesReport::from(sample_response()))
            .expect("serialize json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse json");

        assert_eq!(value["candidates"][0]["current"], true);
        assert_eq!(value["candidates"][0]["source"], "managed");
        assert_eq!(value["candidates"][1]["default"], true);
        assert_eq!(value["candidates"][2]["eligible"], false);
        assert_eq!(value["candidates"][3]["source"], "predicted");
        assert_eq!(value["current_source"], "managed");
        assert_eq!(
            value["explored_boot_interfaces"][0]["mac_address"],
            "aa:bb:cc:00:00:02"
        );
        assert_eq!(
            value["explored_boot_interfaces"][0]["boot_interface_id"],
            "NIC.Slot.2-1-1"
        );
        assert_eq!(value["divergent"], false);
        assert_eq!(value["desired_selection_source"], "RedfishSerialNumber");
        assert_eq!(
            value["desired_selection_updated_at"],
            "1970-01-01T00:00:00Z"
        );
    }
}
