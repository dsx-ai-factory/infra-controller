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

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::rack::RackId;

use super::client::{typed_value_to_f64, typed_value_to_string};
use super::proto::{self, PathElem};
use super::subscriber::GnmiStreamMetrics;
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample};

pub(crate) const NVUE_GNMI_SAMPLE_STREAM_ID: &str = "nvue_gnmi";

pub(crate) struct GnmiSampleProcessor {
    pub(crate) data_sink: Option<Arc<dyn DataSink>>,
    pub(crate) event_context: EventContext,
    pub(crate) switch_id: String,
}

impl GnmiSampleProcessor {
    #[allow(deprecated)]
    pub(crate) fn process_subscribe_response(
        &self,
        resp: &proto::SubscribeResponse,
        stream_metrics: &GnmiStreamMetrics,
    ) {
        let notification = match &resp.response {
            Some(proto::subscribe_response::Response::Update(n)) => n,
            Some(proto::subscribe_response::Response::SyncResponse(_)) => return,
            Some(proto::subscribe_response::Response::Error(e)) => {
                stream_metrics.stream_errors_total.inc();
                tracing::warn!(
                    grpc_status_code = e.code,
                    error = %e.message,
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "nvue_gnmi SAMPLE: server error in stream"
                );
                return;
            }
            None => return,
        };

        stream_metrics.notifications_received_total.inc();
        stream_metrics
            .last_notification_timestamp
            .set(now_unix_secs());

        let start = Instant::now();
        let entity_count = self.process_notification(notification);
        stream_metrics
            .notification_processing_seconds
            .observe(start.elapsed().as_secs_f64());
        stream_metrics.monitored_entities.set(entity_count as f64);
    }

    fn process_notification(&self, notification: &proto::Notification) -> usize {
        let prefix_elems: &[PathElem] = notification
            .prefix
            .as_ref()
            .map(|p| p.elem.as_slice())
            .unwrap_or_default();

        let mut entities: HashSet<(&str, &str)> = HashSet::new();

        for update in &notification.update {
            let val = match update.val.as_ref() {
                Some(v) => v,
                None => continue,
            };

            let update_elems: &[PathElem] = update
                .path
                .as_ref()
                .map(|p| p.elem.as_slice())
                .unwrap_or_default();

            let combined: Vec<&PathElem> = prefix_elems.iter().chain(update_elems.iter()).collect();

            if let Some(iface) = find_elem_key_ref(&combined, "interface", "name") {
                entities.insert(("interface", iface));
                self.process_interface_metric(&combined, iface, val);
            } else if let Some(comp) = find_elem_key_ref(&combined, "component", "name") {
                entities.insert(("component", comp));
                self.process_component_metric(&combined, comp, val);
            } else if let Some(sensor) = find_elem_key_ref(&combined, "leak-sensor", "id") {
                entities.insert(("leak-sensor", sensor));

                if leaf_matches(&combined, &["leak-sensor", "state", "state"]) {
                    let current = leakage_state_to_state(typed_value_to_string(val).as_deref());

                    self.emit_state_set("leakage_state", "sensor", sensor, current, LEAKAGE_STATES);
                }
            } else if combined.iter().any(|e| e.name == "platform-general") {
                // switch-level singleton: no name key, counted as one entity.
                entities.insert(("platform-general", ""));
                self.process_platform_general_metric(&combined, val);
            }
        }

        entities.len()
    }

    fn process_interface_metric(
        &self,
        elems: &[&PathElem],
        iface_name: &str,
        val: &proto::TypedValue,
    ) {
        if leaf_matches(elems, &["state", "oper-status"]) {
            let current = oper_status_to_state(typed_value_to_string(val).as_deref());
            self.emit_state_set(
                "interface_oper_status",
                "interface_name",
                iface_name,
                current,
                OPER_STATUS_STATES,
            );
        } else if let Some(metric_type) = numeric_interface_leaf(elems) {
            match typed_value_to_f64(val) {
                Some(v) => self.emit_iface(metric_type.name, iface_name, v, metric_type.unit),
                None => {
                    debug_unmapped_value(elems, val, metric_type.name, self.event_context.rack_id())
                }
            }
        } else if leaf_matches(elems, &["infiniband", "state", "physical-port-state"]) {
            let current = physical_port_to_state(typed_value_to_string(val).as_deref());
            self.emit_state_set(
                "interface_physical_port_state",
                "interface_name",
                iface_name,
                current,
                PHYSICAL_PORT_STATES,
            );
        } else if leaf_matches(elems, &["infiniband", "state", "logical-port-state"]) {
            let current = logical_port_to_state(typed_value_to_string(val).as_deref());
            self.emit_state_set(
                "interface_logical_port_state",
                "interface_name",
                iface_name,
                current,
                LOGICAL_PORT_STATES,
            );
        } else if leaf_matches(elems, &["infiniband", "state", "speed"]) {
            match link_speed_to_gbps(typed_value_to_string(val).as_deref()) {
                Some(v) => self.emit_iface("interface_link_speed_active", iface_name, v, "gbps"),
                None => debug_unmapped_value(
                    elems,
                    val,
                    "interface_link_speed_active",
                    self.event_context.rack_id(),
                ),
            }
        } else if leaf_matches(elems, &["infiniband", "state", "width"]) {
            match link_width_to_f64(typed_value_to_string(val).as_deref()) {
                Some(v) => self.emit_iface("interface_link_width_active", iface_name, v, "lanes"),
                None => debug_unmapped_value(
                    elems,
                    val,
                    "interface_link_width_active",
                    self.event_context.rack_id(),
                ),
            }
        } else if leaf_matches(elems, &["infiniband", "state", "supported-widths"]) {
            match link_width_to_f64(typed_value_to_string(val).as_deref()) {
                Some(v) => self.emit_iface("interface_supported_width", iface_name, v, "lanes"),
                None => debug_unmapped_value(
                    elems,
                    val,
                    "interface_supported_width",
                    self.event_context.rack_id(),
                ),
            }
        } else if leaf_matches(elems, &["phy-diag", "state", "phy-manager-state"]) {
            let current = phy_manager_to_state(typed_value_to_string(val).as_deref());
            self.emit_state_set(
                "interface_phy_manager_state",
                "interface_name",
                iface_name,
                current,
                PHY_MANAGER_STATES,
            );
        } else if leaf_matches(elems, &["infiniband", "state", "vl-capabilities"])
            && let Some(caps) = typed_value_to_string(val).none_if_empty()
        {
            self.emit_entity_info(
                "interface_vl_capabilities_info",
                iface_name,
                "interface_name",
                "vl_capabilities",
                &caps,
            );
        }
    }

    fn emit_iface(&self, metric_type: &str, iface_name: &str, value: f64, unit: &str) {
        self.emit_data_metric(
            metric_type,
            iface_name,
            value,
            unit,
            "interface_name",
            iface_name,
        );
    }

    /// The information value is excluded from the key so updates replace the prior sample.
    fn emit_entity_info(
        &self,
        metric_type: &str,
        entity_id: &str,
        entity_label_name: &'static str,
        info_label_name: &'static str,
        info_label_value: &str,
    ) {
        let Some(sink) = &self.data_sink else { return };

        let mut key = String::with_capacity(metric_type.len() + 1 + entity_id.len());
        key.push_str(metric_type);
        key.push(':');
        key.push_str(entity_id);

        let labels = vec![
            (Cow::Borrowed(entity_label_name), entity_id.to_string()),
            (Cow::Borrowed(info_label_name), info_label_value.to_string()),
        ];

        sink.handle_event(
            &self.event_context,
            &CollectorEvent::Metric(Box::new(MetricSample {
                key,
                name: NVUE_GNMI_SAMPLE_STREAM_ID.to_string(),
                metric_type: metric_type.to_string(),
                unit: "info".to_string(),
                value: 1.0,
                labels,
                context: None,
            })),
        );
    }

    fn process_component_metric(
        &self,
        elems: &[&PathElem],
        comp_name: &str,
        val: &proto::TypedValue,
    ) {
        // `/components/component` leaves: the `component_name` label
        // distinguishes rows that share a leaf (e.g. FAN-STATE and CPU-STATE both resolve
        // to `state/oper-status`)
        if leaf_matches(elems, &["healthz", "state", "status"]) {
            let current = component_health_to_state(typed_value_to_string(val).as_deref());
            self.emit_state_set(
                "component_health_status",
                "component_name",
                comp_name,
                current,
                COMPONENT_HEALTH_STATES,
            );
        } else if leaf_matches(elems, &["state", "temperature", "instant"])
            && let Some(v) = typed_value_to_f64(val)
        {
            self.emit_comp("component_temperature_celsius", comp_name, v, "celsius");
        } else if leaf_matches(elems, &["state", "last-reboot-reason"])
            && let Some(reason) = typed_value_to_string(val).none_if_empty()
        {
            self.emit_entity_info(
                "component_last_reboot_reason",
                comp_name,
                "component_name",
                "reboot_reason",
                &reason,
            );
        } else if leaf_matches(elems, &["state", "oper-status"]) {
            // FAN-STATE (row 966) and CPU-STATE (row 1174) share this leaf.
            let current = oper_status_to_state(typed_value_to_string(val).as_deref());
            self.emit_state_set(
                "component_oper_status",
                "component_name",
                comp_name,
                current,
                OPER_STATUS_STATES,
            );
        } else if leaf_matches(elems, &["fan", "state", "speed"])
            && let Some(v) = typed_value_to_f64(val)
        {
            self.emit_comp("component_fan_speed", comp_name, v, "rpm");
        } else if leaf_matches(elems, &["power-supply", "state", "output-current"])
            && let Some(v) = typed_value_to_f64(val)
        {
            self.emit_comp(
                "component_power_supply_output_current",
                comp_name,
                v,
                "amperes",
            );
        } else if leaf_matches(elems, &["power-supply", "state", "input-current"])
            && let Some(v) = typed_value_to_f64(val)
        {
            self.emit_comp(
                "component_power_supply_input_current",
                comp_name,
                v,
                "amperes",
            );
        } else if leaf_matches(elems, &["power-supply", "state", "input-voltage"])
            && let Some(v) = typed_value_to_f64(val)
        {
            self.emit_comp(
                "component_power_supply_input_voltage",
                comp_name,
                v,
                "volts",
            );
        } else if leaf_matches(elems, &["power-supply", "state", "output-power"])
            && let Some(v) = typed_value_to_f64(val)
        {
            self.emit_comp("component_power_supply_output_power", comp_name, v, "watts");
        } else if leaf_matches(elems, &["power-supply", "state", "output-voltage"])
            && let Some(v) = typed_value_to_f64(val)
        {
            self.emit_comp(
                "component_power_supply_output_voltage",
                comp_name,
                v,
                "volts",
            );
        } else if leaf_matches(elems, &["asic", "state", "asic-temp"])
            && let Some(v) = typed_value_to_f64(val)
        {
            self.emit_comp(
                "component_asic_temperature_celsius",
                comp_name,
                v,
                "celsius",
            );
        } else if leaf_matches(elems, &["cpu", "utilization", "state", "avg"])
            && let Some(v) = typed_value_to_f64(val)
        {
            self.emit_comp("component_cpu_utilization", comp_name, v, "percent");
        }
    }

    fn emit_comp(&self, metric_type: &str, comp_name: &str, value: f64, unit: &str) {
        self.emit_data_metric(
            metric_type,
            comp_name,
            value,
            unit,
            "component_name",
            comp_name,
        );
    }

    fn process_platform_general_metric(&self, elems: &[&PathElem], val: &proto::TypedValue) {
        let info: Option<(&str, &'static str)> = if leaf_matches(elems, &["state", "contact"]) {
            Some(("platform_contact_info", "contact"))
        } else if leaf_matches(elems, &["state", "location"]) {
            Some(("platform_location_info", "location"))
        } else if leaf_matches(elems, &["state", "platform-name"]) {
            Some(("platform_node_description_info", "node_description"))
        } else if leaf_matches(elems, &["versions", "state", "nos-version"]) {
            Some(("platform_os_version_info", "os_version"))
        } else if leaf_matches(elems, &["versions", "state", "fw-version-bmc"]) {
            Some(("platform_bmc_version_info", "bmc_version"))
        } else if leaf_matches(elems, &["versions", "state", "fw-version-erot"]) {
            Some(("platform_erot_version_info", "erot_version"))
        } else {
            None
        };
        if let Some((metric_type, info_label_name)) = info {
            if let Some(s) = typed_value_to_string(val).none_if_empty() {
                self.emit_switch_info(metric_type, info_label_name, &s);
            }
            return;
        }

        let (metric_type, unit) = if leaf_matches(elems, &["state", "memory-used"]) {
            ("platform_memory_used", "bytes")
        } else if leaf_matches(elems, &["state", "memory-total-size"]) {
            ("platform_memory_total", "bytes")
        } else if leaf_matches(elems, &["state", "disk-total-size"]) {
            ("platform_disk_total", "bytes")
        } else if leaf_matches(elems, &["state", "disk-used"]) {
            ("platform_disk_used", "bytes")
        } else if leaf_matches(elems, &["state", "ambient-temperature"]) {
            ("platform_ambient_temperature", "celsius")
        } else {
            return;
        };

        match typed_value_to_f64(val) {
            Some(v) => self.emit_switch(metric_type, v, unit),
            None => debug_unmapped_value(elems, val, metric_type, self.event_context.rack_id()),
        }
    }

    /// switch-level singleton series: no per-entity name, endpoint identity added by PrometheusSink.
    fn emit_switch(&self, metric_type: &str, value: f64, unit: &str) {
        let Some(sink) = &self.data_sink else { return };

        sink.handle_event(
            &self.event_context,
            &CollectorEvent::Metric(Box::new(MetricSample {
                key: metric_type.to_string(),
                name: NVUE_GNMI_SAMPLE_STREAM_ID.to_string(),
                metric_type: metric_type.to_string(),
                unit: unit.to_string(),
                value,
                labels: Vec::new(),
                context: None,
            })),
        );
    }

    /// switch-level info-metric: constant `1.0` sample carrying a single string label.
    fn emit_switch_info(
        &self,
        metric_type: &str,
        info_label_name: &'static str,
        info_label_value: &str,
    ) {
        let Some(sink) = &self.data_sink else { return };

        let labels = vec![(Cow::Borrowed(info_label_name), info_label_value.to_string())];

        sink.handle_event(
            &self.event_context,
            &CollectorEvent::Metric(Box::new(MetricSample {
                key: metric_type.to_string(),
                name: NVUE_GNMI_SAMPLE_STREAM_ID.to_string(),
                metric_type: metric_type.to_string(),
                unit: "info".to_string(),
                value: 1.0,
                labels,
                context: None,
            })),
        );
    }

    fn emit_data_metric(
        &self,
        metric_type: &str,
        entity_id: &str,
        value: f64,
        unit: &str,
        entity_label_name: &'static str,
        entity_label_value: &str,
    ) {
        let Some(sink) = &self.data_sink else { return };

        let mut key = String::with_capacity(metric_type.len() + 1 + entity_id.len());
        key.push_str(metric_type);
        key.push(':');
        key.push_str(entity_id);

        let labels = vec![(
            Cow::Borrowed(entity_label_name),
            entity_label_value.to_string(),
        )];

        sink.handle_event(
            &self.event_context,
            &CollectorEvent::Metric(Box::new(MetricSample {
                key,
                name: NVUE_GNMI_SAMPLE_STREAM_ID.to_string(),
                metric_type: metric_type.to_string(),
                unit: unit.to_string(),
                value,
                labels,
                context: None,
            })),
        );
    }

    /// OpenMetrics StateSet: one `0.0`/`1.0` series per state (current == 1.0), with a `state`
    /// label.
    fn emit_state_set(
        &self,
        metric_type: &str,
        entity_label_name: &'static str,
        entity_id: &str,
        current_state: &str,
        all_states: &[&'static str],
    ) {
        let Some(sink) = &self.data_sink else { return };

        for state in all_states {
            let mut key =
                String::with_capacity(metric_type.len() + 1 + entity_id.len() + 1 + state.len());
            key.push_str(metric_type);
            key.push(':');
            key.push_str(entity_id);
            key.push(':');
            key.push_str(state);

            let labels = vec![
                (Cow::Borrowed(entity_label_name), entity_id.to_string()),
                (Cow::Borrowed("state"), state.to_string()),
            ];

            sink.handle_event(
                &self.event_context,
                &CollectorEvent::Metric(Box::new(MetricSample {
                    key,
                    name: NVUE_GNMI_SAMPLE_STREAM_ID.to_string(),
                    metric_type: metric_type.to_string(),
                    unit: "state".to_string(),
                    value: if *state == current_state { 1.0 } else { 0.0 },
                    labels,
                    context: None,
                })),
            );
        }
    }
}

fn find_elem_key_ref<'a>(
    elems: &[&'a PathElem],
    elem_name: &str,
    key_name: &str,
) -> Option<&'a str> {
    elems
        .iter()
        .find(|e| e.name == elem_name)
        .and_then(|e| e.key.get(key_name).map(String::as_str))
}

fn leaf_matches(elems: &[&PathElem], expected: &[&str]) -> bool {
    if elems.len() < expected.len() {
        return false;
    }
    let start = elems.len() - expected.len();
    elems[start..]
        .iter()
        .zip(expected)
        .all(|(elem, name)| elem.name == *name)
}

struct NumericLeafMapping {
    tail: &'static [&'static str],
    name: &'static str,
    unit: &'static str,
}

struct NumericLeaf {
    name: &'static str,
    unit: &'static str,
}

/// Table-driven dispatch for numeric `/interfaces/interface` leaves. The
/// expected leaf path tail is matched against the live gNMI tree.
fn numeric_interface_leaf(elems: &[&PathElem]) -> Option<NumericLeaf> {
    const TABLE: &[NumericLeafMapping] = &[
        // OpenConfig interface counters (`/state/counters/*`)
        NumericLeafMapping {
            tail: &["state", "counters", "in-errors"],
            name: "interface_in_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["state", "counters", "out-errors"],
            name: "interface_out_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["state", "counters", "out-discards"],
            name: "interface_out_discards",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["state", "counters", "in-octets"],
            name: "interface_in_octets",
            unit: "bytes",
        },
        NumericLeafMapping {
            tail: &["state", "counters", "out-octets"],
            name: "interface_out_octets",
            unit: "bytes",
        },
        NumericLeafMapping {
            tail: &["state", "counters", "in-pkts"],
            name: "interface_in_packets",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["state", "counters", "out-pkts"],
            name: "interface_out_packets",
            unit: "count",
        },
        // InfiniBand port counters (`/infiniband/state/counters/port/*`)
        NumericLeafMapping {
            tail: &["infiniband", "state", "counters", "port", "link-downed"],
            name: "interface_link_downed",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &[
                "infiniband",
                "state",
                "counters",
                "port",
                "link-error-recovery",
            ],
            name: "interface_link_error_recovery",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &[
                "infiniband",
                "state",
                "counters",
                "port",
                "rcv-remote-phy-errors",
            ],
            name: "interface_rcv_remote_physical_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &[
                "infiniband",
                "state",
                "counters",
                "port",
                "rcv-switch-relay-errors",
            ],
            name: "interface_rcv_switch_relay_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &[
                "infiniband",
                "state",
                "counters",
                "port",
                "rcv-constraints-errors",
            ],
            name: "interface_rcv_constraint_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &[
                "infiniband",
                "state",
                "counters",
                "port",
                "local-link-integrity-errors",
            ],
            name: "interface_local_link_integrity_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &[
                "infiniband",
                "state",
                "counters",
                "port",
                "excessive-buffer-overrun",
            ],
            name: "interface_port_buffer_overrun_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["infiniband", "state", "counters", "port", "qp1-dropped"],
            name: "interface_qp1_dropped",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["infiniband", "state", "counters", "port", "vl15-dropped"],
            name: "interface_vl15_dropped",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["infiniband", "state", "counters", "port", "xmit-wait"],
            name: "interface_port_xmit_wait",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["infiniband", "state", "mtu"],
            name: "interface_mtu",
            unit: "bytes",
        },
        NumericLeafMapping {
            tail: &["infiniband", "state", "max-supported-mtus"],
            name: "interface_max_supported_mtu",
            unit: "bytes",
        },
        // phy-diag counters and ratios (`/phy-diag/state/*`)
        NumericLeafMapping {
            tail: &["phy-diag", "state", "raw-ber"],
            name: "interface_raw_ber",
            unit: "ratio",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "effective-ber"],
            name: "interface_effective_ber",
            unit: "ratio",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "symbol-ber"],
            name: "interface_symbol_ber",
            unit: "ratio",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "raw-ber-ch-1"],
            name: "interface_raw_ber_lane0",
            unit: "ratio",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "raw-ber-ch-2"],
            name: "interface_raw_ber_lane1",
            unit: "ratio",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "raw-errors-ch-1"],
            name: "interface_phy_raw_errors_lane0",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "raw-errors-ch-2"],
            name: "interface_phy_raw_errors_lane1",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "effective-errors"],
            name: "interface_phy_effective_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "zero-hist"],
            name: "interface_zero_hist",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "phy-received-bits"],
            name: "interface_phy_received_bits",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-malformed-packet-errors"],
            name: "interface_port_malformed_packet_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-neighbor-mtu-discards"],
            name: "interface_port_neighbor_mtu_discards",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-multi-cast-rcv-pkts"],
            name: "interface_port_multicast_rcv_packets",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-multi-cast-xmit-pkts"],
            name: "interface_port_multicast_xmit_packets",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-uni-cast-rcv-pkts"],
            name: "interface_port_unicast_rcv_packets",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-uni-cast-xmit-pkts"],
            name: "interface_port_unicast_xmit_packets",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-local-physical-errors"],
            name: "interface_port_local_physical_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "sync-header-error-counter"],
            name: "interface_sync_header_error_counter",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-dlid-mapping-errors"],
            name: "interface_port_dlid_mapping_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-vl-mapping-errors"],
            name: "interface_port_vl_mapping_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-looping-errors"],
            name: "interface_port_looping_errors",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "port-inactive-discards"],
            name: "interface_port_inactive_discards",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "rq-general-error"],
            name: "interface_rq_general_error",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "plr-rcv-codes"],
            name: "interface_plr_rcv_codes",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "plr-rcv-code-err"],
            name: "interface_plr_rcv_codes_err",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "plr-rcv-uncorrectable-code"],
            name: "interface_plr_rcv_uncorrectables_code",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "plr-xmit-codes"],
            name: "interface_plr_xmit_codes",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "plr-xmit-retry-codes"],
            name: "interface_plr_xmit_retrys_codes",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "plr-xmit-retry-events"],
            name: "interface_plr_xmit_retrys_events",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "plr-sync-events"],
            name: "interface_plr_sync_events",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &[
                "phy-diag",
                "state",
                "plr-xmit-retry-events-within-t-sec-max",
            ],
            name: "interface_plr_xmit_retry_events_within_minute",
            unit: "count",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "plr-bw-loss-percent"],
            name: "interface_plr_bw_loss_percent",
            unit: "percent",
        },
        NumericLeafMapping {
            tail: &["phy-diag", "state", "unintentional-link-down-events"],
            name: "interface_link_down_events",
            unit: "count",
        },
    ];

    // FEC histogram bins 0..=15 -> interface_fec_hist_{n}
    if let Some(leaf) = elems.last().map(|e| e.name.as_str())
        && let Some(bin) = leaf.strip_prefix("rs-num-corr-err-bin")
        && let Ok(n) = bin.parse::<usize>()
        && n <= 15
        && leaf_matches(elems, &["phy-diag", "state", leaf])
    {
        return Some(NumericLeaf {
            name: FEC_HIST_NAMES[n],
            unit: "count",
        });
    }

    TABLE.iter().find_map(|m| {
        leaf_matches(elems, m.tail).then_some(NumericLeaf {
            name: m.name,
            unit: m.unit,
        })
    })
}

/// FEC histogram bins 0..=15
const FEC_HIST_NAMES: [&str; 16] = [
    "interface_fec_hist_0",
    "interface_fec_hist_1",
    "interface_fec_hist_2",
    "interface_fec_hist_3",
    "interface_fec_hist_4",
    "interface_fec_hist_5",
    "interface_fec_hist_6",
    "interface_fec_hist_7",
    "interface_fec_hist_8",
    "interface_fec_hist_9",
    "interface_fec_hist_10",
    "interface_fec_hist_11",
    "interface_fec_hist_12",
    "interface_fec_hist_13",
    "interface_fec_hist_14",
    "interface_fec_hist_15",
];

const LEAKAGE_STATES: &[&str] = &["ok", "leak", "unknown"];

/// Maps NVUE leakage sensor strings to the emitted StateSet domain.
/// Missing or unrecognized values are emitted as `unknown`.
fn leakage_state_to_state(state: Option<&str>) -> &'static str {
    match state.map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("ok") => "ok",
        Some(s) if s.eq_ignore_ascii_case("leak") => "leak",
        _ => "unknown",
    }
}

const OPER_STATUS_STATES: &[&str] = &["up", "down"];

/// oper-status string -> current StateSet state. "up" when the source reads
/// "up" or "active" else "down". Applies to
/// `interface_oper_status` and `component_oper_status`.
fn oper_status_to_state(status: Option<&str>) -> &'static str {
    match status {
        Some(s) if s.eq_ignore_ascii_case("up") || s.eq_ignore_ascii_case("active") => "up",
        _ => "down",
    }
}

const PHYSICAL_PORT_STATES: &[&str] = &["up", "down"];

/// InfiniBand physical port state enum -> current StateSet state. Values
/// observed live on GB200: `LINK_UP`, `POLLING`, `PORT_CONFIGURATION_TRAINING`.
/// Binary: "up" only when the link is up; polling/training/everything-else is
/// "down".
fn physical_port_to_state(state: Option<&str>) -> &'static str {
    match state {
        Some(s) if s.eq_ignore_ascii_case("link_up") => "up",
        _ => "down",
    }
}

const PHY_MANAGER_STATES: &[&str] = &["up", "down"];

/// PHY manager FSM state string -> current StateSet state. The PHY manager
/// reports a dynamic FSM label (e.g. "Active_or_Linkup", "Disabled"), so we
/// match the `active`/`linkup` tokens
fn phy_manager_to_state(state: Option<&str>) -> &'static str {
    match state {
        Some(s)
            if s.split(|c: char| !c.is_ascii_alphanumeric()).any(|tok| {
                tok.eq_ignore_ascii_case("active") || tok.eq_ignore_ascii_case("linkup")
            }) =>
        {
            "up"
        }
        _ => "down",
    }
}

const LOGICAL_PORT_STATES: &[&str] = &["active", "down"];

/// InfiniBand logical port state enum -> current StateSet state.
/// (e.g. `ACTIVE`, `DOWN`). "active" when the source reads
/// "active", else "down".
fn logical_port_to_state(state: Option<&str>) -> &'static str {
    match state {
        Some(s) if s.eq_ignore_ascii_case("active") => "active",
        _ => "down",
    }
}

/// IB link width -> active lane count. Handles both the single live form
/// ("2X") and the comma-composite the NVOS schema allows for supported-widths
/// ("1X,2X,4X"); each token is parsed as `<n>X` and the maximum lane count is
/// returned. Returns None when no token matches the `<n>X` shape so unknown
/// widths are not exported.
fn parse_finite_non_negative(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn link_width_to_f64(width: Option<&str>) -> Option<f64> {
    let w = width?;
    w.split(',')
        .filter_map(|tok| {
            tok.trim()
                .strip_suffix(['X', 'x'])
                .and_then(parse_finite_non_negative)
        })
        .reduce(f64::max)
}

/// IB link speed -> Gbps. GB200 emits bare numeric Gbps; we also accept the
/// suffix forms the schema permits.
fn link_speed_to_gbps(speed: Option<&str>) -> Option<f64> {
    let s = speed?.trim();
    if s.is_empty() {
        return None;
    }
    // handle Mbit suffix
    if let Some(mbps) = s
        .strip_suffix("Mb/s")
        .or_else(|| s.strip_suffix("Mbps"))
        .or_else(|| s.strip_suffix('M'))
    {
        return parse_finite_non_negative(mbps.trim()).map(|v| v / 1000.0);
    }
    // "<n>G" Gbps suffix
    if let Some(gbps) = s.strip_suffix(['G', 'g']) {
        return parse_finite_non_negative(gbps.trim());
    }
    // base case numeric implicit Gbps
    parse_finite_non_negative(s)
}

/// Log when an interface leaf that matched a known mapping but value wasn't caught.
fn debug_unmapped_value(
    elems: &[&PathElem],
    val: &proto::TypedValue,
    metric_type: &str,
    rack_id: Option<&RackId>,
) {
    tracing::debug!(
        leaf = %leaf_path(elems),
        raw = ?typed_value_to_string(val),
        metric_type,
        rack_id = rack_id.map(tracing::field::display),
        "nvue_gnmi SAMPLE: matched leaf but value coercion returned None; dropping"
    );
}

/// Render the gNMI element tail as a slash path for diagnostics, e.g.
/// "infiniband/state/speed".
fn leaf_path(elems: &[&PathElem]) -> String {
    elems
        .iter()
        .map(|e| e.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

const COMPONENT_HEALTH_STATES: &[&str] = &["healthy", "unhealthy", "unknown"];

/// component healthz status -> current StateSet state. "healthy"/"unhealthy"
/// else "unknown".
fn component_health_to_state(status: Option<&str>) -> &'static str {
    match status {
        Some(s) if s.eq_ignore_ascii_case("healthy") => "healthy",
        Some(s) if s.eq_ignore_ascii_case("unhealthy") => "unhealthy",
        _ => "unknown",
    }
}

pub(crate) fn now_unix_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use carbide_uuid::rack::RackId;
    use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};

    use super::*;
    use crate::endpoint::{EndpointMetadata, SwitchData, SwitchEndpointRole};

    #[derive(Default)]
    struct CapturingSink {
        events: Mutex<Vec<(EventContext, CollectorEvent)>>,
    }

    impl DataSink for CapturingSink {
        fn sink_type(&self) -> &'static str {
            "capturing_sink"
        }

        fn try_handle_event(
            &self,
            context: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), crate::HealthError> {
            self.events
                .lock()
                .expect("lock poisoned")
                .push((context.clone(), event.clone()));
            Ok(())
        }
    }

    #[test]
    fn test_leaf_matches() {
        let elems: Vec<PathElem> = ["interfaces", "interface", "state", "oper-status"]
            .iter()
            .map(|n| PathElem {
                name: n.to_string(),
                key: Default::default(),
            })
            .collect();
        let refs: Vec<&PathElem> = elems.iter().collect();

        assert!(leaf_matches(&refs, &["state", "oper-status"]));
        assert!(leaf_matches(&refs, &["oper-status"]));
        assert!(!leaf_matches(&refs, &["counters", "oper-status"]));
        assert!(!leaf_matches(&refs, &["a", "b", "c", "d", "e"]));
    }

    #[test]
    fn test_find_elem_key_ref() {
        let mut key_map = HashMap::new();
        key_map.insert("name".to_string(), "nvl0".to_string());
        let elems = [
            PathElem {
                name: "interfaces".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "interface".to_string(),
                key: key_map,
            },
        ];
        let refs: Vec<&PathElem> = elems.iter().collect();

        assert_eq!(find_elem_key_ref(&refs, "interface", "name"), Some("nvl0"));
        assert_eq!(find_elem_key_ref(&refs, "interface", "id"), None);
        assert_eq!(find_elem_key_ref(&refs, "component", "name"), None);
    }

    #[test]
    fn test_oper_status_mapping() {
        assert_eq!(oper_status_to_state(Some("UP")), "up");
        assert_eq!(oper_status_to_state(Some("up")), "up");
        assert_eq!(oper_status_to_state(Some("DOWN")), "down");
        assert_eq!(oper_status_to_state(None), "down");
    }

    #[test]
    fn test_component_health_mapping() {
        assert_eq!(component_health_to_state(Some("healthy")), "healthy");
        assert_eq!(component_health_to_state(Some("HEALTHY")), "healthy");
        assert_eq!(component_health_to_state(Some("unhealthy")), "unhealthy");
        assert_eq!(component_health_to_state(Some("UNHEALTHY")), "unhealthy");
        // unrecognized / absent => "unknown"
        assert_eq!(component_health_to_state(Some("weird")), "unknown");
        assert_eq!(component_health_to_state(None), "unknown");
    }

    fn make_path_elem(name: &str, keys: &[(&str, &str)]) -> PathElem {
        PathElem {
            name: name.to_string(),
            key: keys
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn make_typed_value_string(s: &str) -> proto::TypedValue {
        proto::TypedValue {
            value: Some(proto::typed_value::Value::StringVal(s.to_string())),
        }
    }

    fn make_typed_value_uint(v: u64) -> proto::TypedValue {
        proto::TypedValue {
            value: Some(proto::typed_value::Value::UintVal(v)),
        }
    }

    fn test_processor() -> GnmiSampleProcessor {
        use std::str::FromStr;

        use mac_address::MacAddress;

        use crate::endpoint::BmcAddr;

        let addr = BmcAddr {
            ip: "10.0.0.1".parse().unwrap(),
            port: None,
            mac: MacAddress::from_str("AA:BB:CC:DD:EE:FF").unwrap(),
        };
        let event_context = EventContext {
            endpoint_key: "aa:bb:cc:dd:ee:ff".to_string(),
            addr,
            collector_type: NVUE_GNMI_SAMPLE_STREAM_ID,
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        };
        GnmiSampleProcessor {
            data_sink: None,
            event_context,
            switch_id: "serial-abc".to_string(),
        }
    }

    fn test_switch_id(label: &str) -> SwitchId {
        let mut hash = [0u8; 32];
        let bytes = label.as_bytes();
        hash[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
        SwitchId::new(SwitchIdSource::Tpm, hash, SwitchType::NvLink)
    }

    #[test]
    fn test_process_notification_interface_oper_status() {
        let proc = test_processor();
        let notification = proto::Notification {
            timestamp: 0,
            prefix: Some(proto::Path {
                elem: vec![
                    make_path_elem("interfaces", &[]),
                    make_path_elem("interface", &[("name", "nvl4")]),
                ],
                ..Default::default()
            }),
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        make_path_elem("state", &[]),
                        make_path_elem("oper-status", &[]),
                    ],
                    ..Default::default()
                }),
                val: Some(make_typed_value_string("UP")),
                ..Default::default()
            }],
            ..Default::default()
        };

        let count = proc.process_notification(&notification);
        assert_eq!(count, 1);
    }

    #[test]
    fn emitted_metrics_preserve_switch_position_context() {
        use std::str::FromStr;

        use mac_address::MacAddress;

        use crate::endpoint::BmcAddr;

        let sink = Arc::new(CapturingSink::default());
        let switch_id = test_switch_id("switch-a");
        let proc = GnmiSampleProcessor {
            data_sink: Some(sink.clone()),
            event_context: EventContext {
                endpoint_key: "aa:bb:cc:dd:ee:ff".to_string(),
                addr: BmcAddr {
                    ip: "10.0.0.1".parse().unwrap(),
                    port: None,
                    mac: MacAddress::from_str("AA:BB:CC:DD:EE:FF").unwrap(),
                },
                collector_type: NVUE_GNMI_SAMPLE_STREAM_ID,
                labels: Default::default(),
                metadata: Some(EndpointMetadata::Switch(SwitchData {
                    id: Some(switch_id),
                    serial: "SN-SWITCH-001".to_string(),
                    slot_number: Some(7),
                    tray_index: Some(3),
                    nvlink_domain_uuid: None,
                    endpoint_role: SwitchEndpointRole::Host,
                    is_primary: false,
                    nmxc_enabled: false,
                    nmxt_enabled: false,
                })),
                rack_id: Some(RackId::new("RACK_2")),
            },
            switch_id: "SN-SWITCH-001".to_string(),
        };
        let notification = proto::Notification {
            timestamp: 0,
            prefix: Some(proto::Path {
                elem: vec![
                    make_path_elem("interfaces", &[]),
                    make_path_elem("interface", &[("name", "nvl4")]),
                ],
                ..Default::default()
            }),
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        make_path_elem("state", &[]),
                        make_path_elem("oper-status", &[]),
                    ],
                    ..Default::default()
                }),
                val: Some(make_typed_value_string("UP")),
                ..Default::default()
            }],
            ..Default::default()
        };

        let count = proc.process_notification(&notification);
        assert_eq!(count, 1);

        let events = sink.events.lock().expect("lock poisoned");
        // oper-status is a StateSet: one 0/1 series per state ("up"/"down").
        assert_eq!(events.len(), OPER_STATUS_STATES.len());
        // every emitted series preserves the switch-position context.
        for (context, event) in events.iter() {
            assert_eq!(context.switch_id(), Some(switch_id));
            assert_eq!(context.switch_slot_number(), Some(7));
            assert_eq!(context.switch_tray_index(), Some(3));
            assert_eq!(context.rack_id().map(RackId::as_str), Some("RACK_2"));
            assert!(matches!(event, CollectorEvent::Metric(_)));
        }
    }

    #[test]
    fn test_process_notification_component_temperature() {
        let proc = test_processor();
        let notification = proto::Notification {
            timestamp: 0,
            prefix: Some(proto::Path {
                elem: vec![
                    make_path_elem("components", &[]),
                    make_path_elem("component", &[("name", "PSU-1")]),
                ],
                ..Default::default()
            }),
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        make_path_elem("state", &[]),
                        make_path_elem("temperature", &[]),
                        make_path_elem("instant", &[]),
                    ],
                    ..Default::default()
                }),
                val: Some(proto::TypedValue {
                    value: Some(proto::typed_value::Value::DoubleVal(42.5)),
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        let count = proc.process_notification(&notification);
        assert_eq!(count, 1);
    }

    #[test]
    fn leak_sensor_states_emit_distinct_state_sets() {
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());

        let cases = [("LEAK0", "ok"), ("LEAK1", "leak"), ("LEAK2", "unknown")];

        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: cases
                .iter()
                .map(|(sensor, state)| proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("platform-general", &[]),
                            make_path_elem("leak-sensors", &[]),
                            make_path_elem("leak-sensor", &[("id", sensor)]),
                            make_path_elem("state", &[]),
                            make_path_elem("state", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string(state)),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };

        assert_eq!(proc.process_notification(&notification), cases.len());

        let events = sink.events.lock().expect("lock poisoned");

        let samples = events
            .iter()
            .map(|(_, event)| {
                let CollectorEvent::Metric(sample) = event else {
                    panic!("expected a Metric event");
                };

                sample.as_ref()
            })
            .collect::<Vec<_>>();

        for (sensor, state) in cases {
            let sensor_samples = samples
                .iter()
                .copied()
                .filter(|sample| {
                    sample
                        .labels
                        .iter()
                        .any(|(key, value)| key == "sensor" && value == sensor)
                })
                .cloned()
                .collect::<Vec<_>>();

            assert_state_set(
                &sensor_samples,
                "leakage_state",
                "sensor",
                sensor,
                LEAKAGE_STATES,
                state,
            );
        }
    }

    #[test]
    fn test_process_notification_multiple_updates() {
        let proc = test_processor();
        let notification = proto::Notification {
            timestamp: 0,
            prefix: Some(proto::Path {
                elem: vec![
                    make_path_elem("interfaces", &[]),
                    make_path_elem("interface", &[("name", "nvl0")]),
                ],
                ..Default::default()
            }),
            update: vec![
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("state", &[]),
                            make_path_elem("oper-status", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string("UP")),
                    ..Default::default()
                },
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("state", &[]),
                            make_path_elem("counters", &[]),
                            make_path_elem("in-errors", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_uint(42)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        // same interface, so entity count is 1 even with multiple updates
        let count = proc.process_notification(&notification);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_process_notification_mixed_entities() {
        let proc = test_processor();

        let iface_update = proto::Update {
            path: Some(proto::Path {
                elem: vec![
                    make_path_elem("interfaces", &[]),
                    make_path_elem("interface", &[("name", "nvl0")]),
                    make_path_elem("state", &[]),
                    make_path_elem("oper-status", &[]),
                ],
                ..Default::default()
            }),
            val: Some(make_typed_value_string("DOWN")),
            ..Default::default()
        };

        let comp_update = proto::Update {
            path: Some(proto::Path {
                elem: vec![
                    make_path_elem("components", &[]),
                    make_path_elem("component", &[("name", "FAN-1")]),
                    make_path_elem("healthz", &[]),
                    make_path_elem("state", &[]),
                    make_path_elem("status", &[]),
                ],
                ..Default::default()
            }),
            val: Some(make_typed_value_string("healthy")),
            ..Default::default()
        };

        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: vec![iface_update, comp_update],
            ..Default::default()
        };

        let count = proc.process_notification(&notification);
        assert_eq!(count, 2);
    }

    #[test]
    fn test_process_notification_update_without_val_is_skipped() {
        let proc = test_processor();
        let notification = proto::Notification {
            timestamp: 0,
            prefix: Some(proto::Path {
                elem: vec![
                    make_path_elem("interfaces", &[]),
                    make_path_elem("interface", &[("name", "nvl0")]),
                ],
                ..Default::default()
            }),
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        make_path_elem("state", &[]),
                        make_path_elem("oper-status", &[]),
                    ],
                    ..Default::default()
                }),
                val: None,
                ..Default::default()
            }],
            ..Default::default()
        };

        let count = proc.process_notification(&notification);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_process_notification_symbol_ber_and_link_down_events() {
        let proc = test_processor();
        let notification = proto::Notification {
            timestamp: 0,
            prefix: Some(proto::Path {
                elem: vec![
                    make_path_elem("interfaces", &[]),
                    make_path_elem("interface", &[("name", "nvl2")]),
                ],
                ..Default::default()
            }),
            update: vec![
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("phy-diag", &[]),
                            make_path_elem("state", &[]),
                            make_path_elem("symbol-ber", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(proto::TypedValue {
                        value: Some(proto::typed_value::Value::DoubleVal(3.2e-10)),
                    }),
                    ..Default::default()
                },
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("phy-diag", &[]),
                            make_path_elem("state", &[]),
                            make_path_elem("unintentional-link-down-events", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_uint(7)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let count = proc.process_notification(&notification);
        assert_eq!(count, 1);
    }

    fn test_stream_metrics() -> super::super::subscriber::GnmiStreamMetrics {
        use prometheus::{Counter, Gauge, Histogram, HistogramOpts, IntGauge};
        super::super::subscriber::GnmiStreamMetrics {
            connection_state: IntGauge::new("test_conn_state", "test").unwrap(),
            connected: IntGauge::new("test_connected", "test").unwrap(),
            synchronized: IntGauge::new("test_synchronized", "test").unwrap(),
            reconnections_total: Counter::new("test_reconn", "test").unwrap(),
            server_initiated_closures_total: Counter::new("test_closures", "test").unwrap(),
            connection_established_timestamp: Gauge::new("test_conn_ts", "test").unwrap(),
            notifications_received_total: Counter::new("test_notif_total", "test").unwrap(),
            last_notification_timestamp: Gauge::new("test_last_notif_ts", "test").unwrap(),
            notification_processing_seconds: Histogram::with_opts(HistogramOpts::new(
                "test_proc_secs",
                "test",
            ))
            .unwrap(),
            stream_errors_total: Counter::new("test_errors", "test").unwrap(),
            monitored_entities: Gauge::new("test_entities", "test").unwrap(),
        }
    }

    #[test]
    fn test_process_subscribe_response_sync_response_is_noop() {
        let proc = test_processor();
        let metrics = test_stream_metrics();
        let resp = proto::SubscribeResponse {
            response: Some(proto::subscribe_response::Response::SyncResponse(true)),
            ..Default::default()
        };

        proc.process_subscribe_response(&resp, &metrics);

        assert_eq!(metrics.notifications_received_total.get(), 0.0);
        assert_eq!(metrics.stream_errors_total.get(), 0.0);
    }

    #[test]
    #[allow(deprecated)]
    fn test_process_subscribe_response_error_increments_counter() {
        let proc = test_processor();
        let metrics = test_stream_metrics();
        let resp = proto::SubscribeResponse {
            response: Some(proto::subscribe_response::Response::Error(proto::Error {
                code: 13,
                message: "internal server error".into(),
                ..Default::default()
            })),
            ..Default::default()
        };

        proc.process_subscribe_response(&resp, &metrics);

        assert_eq!(metrics.stream_errors_total.get(), 1.0);
        assert_eq!(metrics.notifications_received_total.get(), 0.0);
    }

    #[test]
    fn test_process_subscribe_response_none_is_noop() {
        let proc = test_processor();
        let metrics = test_stream_metrics();
        let resp = proto::SubscribeResponse {
            response: None,
            ..Default::default()
        };

        proc.process_subscribe_response(&resp, &metrics);

        assert_eq!(metrics.notifications_received_total.get(), 0.0);
        assert_eq!(metrics.stream_errors_total.get(), 0.0);
    }

    // ---- explicit GB200 mapping coverage ------------------------------------

    /// Drive a single `/interfaces/interface[name=acp0]/<tail...>` update and
    /// return the one captured `MetricSample`, asserting the producer-level
    /// invariants (stream `name`, `collector_type`, `interface_name` label).
    fn run_interface_leaf(tail: &[&str], val: proto::TypedValue) -> (MetricSample, EventContext) {
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());

        let mut elems = vec![
            make_path_elem("interfaces", &[]),
            make_path_elem("interface", &[("name", "acp0")]),
        ];
        elems.extend(tail.iter().map(|n| make_path_elem(n, &[])));

        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: elems,
                    ..Default::default()
                }),
                val: Some(val),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);

        let events = sink.events.lock().expect("lock poisoned");
        assert_eq!(events.len(), 1, "expected exactly one emitted metric");
        let (ctx, event) = events[0].clone();
        let CollectorEvent::Metric(sample) = event else {
            panic!("expected a Metric event");
        };
        // shared producer invariants for every interface mapping. The
        // `interface_name` label is always present as the first (entity) label;
        // info-metrics may carry additional info labels after it, so assert the
        // first label rather than the exact set.
        assert_eq!(sample.name, NVUE_GNMI_SAMPLE_STREAM_ID);
        assert_eq!(ctx.collector_type, NVUE_GNMI_SAMPLE_STREAM_ID);
        assert_eq!(
            sample.labels.first(),
            Some(&(Cow::Borrowed("interface_name"), "acp0".to_string()))
        );
        (*sample, ctx)
    }

    /// Same as `run_interface_leaf` but for `/components/component[name=...]`.
    fn run_component_leaf(comp_name: &str, tail: &[&str], val: proto::TypedValue) -> MetricSample {
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());

        let mut elems = vec![
            make_path_elem("components", &[]),
            make_path_elem("component", &[("name", comp_name)]),
        ];
        elems.extend(tail.iter().map(|n| make_path_elem(n, &[])));

        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: elems,
                    ..Default::default()
                }),
                val: Some(val),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);

        let events = sink.events.lock().expect("lock poisoned");
        assert_eq!(events.len(), 1, "expected exactly one emitted metric");
        let (ctx, event) = events[0].clone();
        let CollectorEvent::Metric(sample) = event else {
            panic!("expected a Metric event");
        };
        assert_eq!(sample.name, NVUE_GNMI_SAMPLE_STREAM_ID);
        assert_eq!(ctx.collector_type, NVUE_GNMI_SAMPLE_STREAM_ID);
        assert_eq!(
            sample.labels,
            vec![(Cow::Borrowed("component_name"), comp_name.to_string())]
        );
        *sample
    }

    /// Drive a single `/interfaces/interface[name=acp0]/<tail...>` update and
    /// return ALL captured `MetricSample`s. Used for StateSet leaves, which
    /// fan a single source value out into one 0/1 series per possible state.
    fn run_interface_leaf_all(tail: &[&str], val: proto::TypedValue) -> Vec<MetricSample> {
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());

        let mut elems = vec![
            make_path_elem("interfaces", &[]),
            make_path_elem("interface", &[("name", "acp0")]),
        ];
        elems.extend(tail.iter().map(|n| make_path_elem(n, &[])));

        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: elems,
                    ..Default::default()
                }),
                val: Some(val),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);

        sink.events
            .lock()
            .expect("lock poisoned")
            .iter()
            .map(|(_, event)| {
                let CollectorEvent::Metric(sample) = event else {
                    panic!("expected a Metric event");
                };
                (**sample).clone()
            })
            .collect()
    }

    /// Same as `run_interface_leaf_all` but for `/components/component[name=...]`.
    fn run_component_leaf_all(
        comp_name: &str,
        tail: &[&str],
        val: proto::TypedValue,
    ) -> Vec<MetricSample> {
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());

        let mut elems = vec![
            make_path_elem("components", &[]),
            make_path_elem("component", &[("name", comp_name)]),
        ];
        elems.extend(tail.iter().map(|n| make_path_elem(n, &[])));

        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: elems,
                    ..Default::default()
                }),
                val: Some(val),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);

        sink.events
            .lock()
            .expect("lock poisoned")
            .iter()
            .map(|(_, event)| {
                let CollectorEvent::Metric(sample) = event else {
                    panic!("expected a Metric event");
                };
                (**sample).clone()
            })
            .collect()
    }

    /// Assert OpenMetrics StateSet semantics over a captured fan-out: exactly
    /// one 0/1 series per `all_states` entry, each with unit "state", the named
    /// entity label present, and a `state` label; the series whose `state`
    /// label equals `current` has value 1.0 and every other series is 0.0.
    fn assert_state_set(
        samples: &[MetricSample],
        metric_type: &str,
        entity_label: &str,
        entity_id: &str,
        all_states: &[&str],
        current: &str,
    ) {
        assert_eq!(
            samples.len(),
            all_states.len(),
            "{metric_type}: expected one series per state"
        );
        for state in all_states {
            let sample = samples
                .iter()
                .find(|s| s.labels.iter().any(|(k, v)| k == "state" && v == state))
                .unwrap_or_else(|| panic!("{metric_type}: missing series for state {state}"));
            assert_eq!(sample.metric_type, metric_type, "state {state}");
            assert_eq!(sample.unit, "state", "state {state}");
            assert_eq!(
                sample.value,
                if *state == current { 1.0 } else { 0.0 },
                "{metric_type} state {state}: value (current={current})"
            );
            assert!(
                sample
                    .labels
                    .iter()
                    .any(|(k, v)| k == entity_label && v == entity_id),
                "{metric_type} state {state}: missing entity label {entity_label}={entity_id}"
            );
        }
    }

    #[test]
    fn test_interface_fec_histogram_bins() {
        for n in 0u8..=15 {
            let leaf = format!("rs-num-corr-err-bin{n}");
            let (sample, _) =
                run_interface_leaf(&["phy-diag", "state", &leaf], make_typed_value_uint(11));
            assert_eq!(sample.metric_type, format!("interface_fec_hist_{n}"));
            assert_eq!(sample.unit, "count");
            assert_eq!(sample.value, 11.0);
        }
    }

    #[test]
    fn test_interface_ber_parses_scientific_notation() {
        // live BER values arrive as scientific-notation strings, e.g. "15E-255"
        let (sample, _) = run_interface_leaf(
            &["phy-diag", "state", "raw-ber"],
            make_typed_value_string("1E-12"),
        );
        assert_eq!(sample.metric_type, "interface_raw_ber");
        assert_eq!(sample.unit, "ratio");
        assert!((sample.value - 1e-12).abs() < f64::EPSILON);
    }

    #[test]
    fn test_interface_physical_port_state_enum() {
        // Binary StateSet: only LINK_UP is "up"; polling/training/anything-else
        // is "down" (regression: ordinal codes 2/3 collapsed to "down").
        for (raw, current) in [
            ("LINK_UP", "up"),
            ("POLLING", "down"),
            ("PORT_CONFIGURATION_TRAINING", "down"),
            ("SOMETHING_ELSE", "down"),
        ] {
            let samples = run_interface_leaf_all(
                &["infiniband", "state", "physical-port-state"],
                make_typed_value_string(raw),
            );
            assert_state_set(
                &samples,
                "interface_physical_port_state",
                "interface_name",
                "acp0",
                PHYSICAL_PORT_STATES,
                current,
            );
        }
    }

    #[test]
    fn test_interface_logical_port_state_enum() {
        for (raw, current) in [("ACTIVE", "active"), ("DOWN", "down")] {
            let samples = run_interface_leaf_all(
                &["infiniband", "state", "logical-port-state"],
                make_typed_value_string(raw),
            );
            assert_state_set(
                &samples,
                "interface_logical_port_state",
                "interface_name",
                "acp0",
                LOGICAL_PORT_STATES,
                current,
            );
        }
    }

    #[test]
    fn test_phy_manager_to_state_helper() {
        // token match, case-insensitive: active/linkup => "up"
        assert_eq!(phy_manager_to_state(Some("Active_or_Linkup")), "up");
        assert_eq!(phy_manager_to_state(Some("LINKUP")), "up");
        assert_eq!(phy_manager_to_state(Some("active")), "up");
        // anything else => "down"
        assert_eq!(phy_manager_to_state(Some("Disabled")), "down");
        assert_eq!(phy_manager_to_state(Some("")), "down");
        assert_eq!(phy_manager_to_state(None), "down");
        // regression: "active" is a substring of these down-states but must NOT
        // match as up -- word-boundary token match, not substring.
        assert_eq!(phy_manager_to_state(Some("Inactive")), "down");
        assert_eq!(phy_manager_to_state(Some("Deactivated")), "down");
    }

    #[test]
    fn test_interface_phy_manager_state_enum() {
        // PHY-MANAGER-STATE (row 961): dynamic FSM string emitted as a StateSet.
        for (raw, current) in [
            ("Active_or_Linkup", "up"),
            ("LINKUP", "up"),
            ("Disabled", "down"),
            ("", "down"),
            // regression for the substring bug: these contain "active" as a
            // substring but are down-states.
            ("Inactive", "down"),
            ("Deactivated", "down"),
        ] {
            let samples = run_interface_leaf_all(
                &["phy-diag", "state", "phy-manager-state"],
                make_typed_value_string(raw),
            );
            assert_state_set(
                &samples,
                "interface_phy_manager_state",
                "interface_name",
                "acp0",
                PHY_MANAGER_STATES,
                current,
            );
        }
    }

    #[test]
    fn test_interface_oper_status_state_set() {
        for (raw, current) in [("UP", "up"), ("active", "up"), ("DOWN", "down")] {
            let samples =
                run_interface_leaf_all(&["state", "oper-status"], make_typed_value_string(raw));
            assert_state_set(
                &samples,
                "interface_oper_status",
                "interface_name",
                "acp0",
                OPER_STATUS_STATES,
                current,
            );
        }
    }

    #[test]
    fn test_interface_vl_capabilities_info() {
        // VL-CAPABILITIES (row 965): non-empty string -> one info sample whose
        // information is carried by the `vl_capabilities` label alongside
        // `interface_name`. The shared invariant assert in `run_interface_leaf`
        // only checks the first (interface_name) label, so assert the full set
        // explicitly here.
        let (sample, _) = run_interface_leaf(
            &["infiniband", "state", "vl-capabilities"],
            make_typed_value_string("VL0-VL7"),
        );
        assert_eq!(sample.metric_type, "interface_vl_capabilities_info");
        assert_eq!(sample.unit, "info");
        assert_eq!(sample.value, 1.0);
        assert_eq!(
            sample.labels,
            vec![
                (Cow::Borrowed("interface_name"), "acp0".to_string()),
                (Cow::Borrowed("vl_capabilities"), "VL0-VL7".to_string()),
            ]
        );
    }

    #[test]
    fn test_interface_vl_capabilities_empty_is_not_exported() {
        // An empty vl-capabilities string carries no information and emits nothing.
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());
        let notification = proto::Notification {
            timestamp: 0,
            prefix: Some(proto::Path {
                elem: vec![
                    make_path_elem("interfaces", &[]),
                    make_path_elem("interface", &[("name", "acp0")]),
                ],
                ..Default::default()
            }),
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        make_path_elem("infiniband", &[]),
                        make_path_elem("state", &[]),
                        make_path_elem("vl-capabilities", &[]),
                    ],
                    ..Default::default()
                }),
                val: Some(make_typed_value_string("")),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);
        assert_eq!(
            sink.events.lock().expect("lock poisoned").len(),
            0,
            "empty vl-capabilities must not emit a metric"
        );
    }

    #[test]
    fn test_interface_link_width_enum() {
        let (active, _) = run_interface_leaf(
            &["infiniband", "state", "width"],
            make_typed_value_string("2X"),
        );
        assert_eq!(active.metric_type, "interface_link_width_active");
        assert_eq!(active.unit, "lanes");
        assert_eq!(active.value, 2.0);

        let (supported, _) = run_interface_leaf(
            &["infiniband", "state", "supported-widths"],
            make_typed_value_string("4X"),
        );
        assert_eq!(supported.metric_type, "interface_supported_width");
        assert_eq!(supported.unit, "lanes");
        assert_eq!(supported.value, 4.0);
    }

    #[test]
    fn component_numeric_leaves_emit_expected_metrics() {
        struct Case {
            component_name: &'static str,
            tail: &'static [&'static str],
            raw: u64,
            metric_type: &'static str,
            unit: &'static str,
        }

        let cases = [
            Case {
                component_name: "ASIC1",
                tail: &["asic", "state", "asic-temp"],
                raw: 46,
                metric_type: "component_asic_temperature_celsius",
                unit: "celsius",
            },
            Case {
                component_name: "cpu",
                tail: &["cpu", "utilization", "state", "avg"],
                raw: 24,
                metric_type: "component_cpu_utilization",
                unit: "percent",
            },
            Case {
                component_name: "FAN1/1",
                tail: &["fan", "state", "speed"],
                raw: 12_000,
                metric_type: "component_fan_speed",
                unit: "rpm",
            },
            Case {
                component_name: "PSU1",
                tail: &["power-supply", "state", "output-current"],
                raw: 8,
                metric_type: "component_power_supply_output_current",
                unit: "amperes",
            },
            Case {
                component_name: "PSU1",
                tail: &["power-supply", "state", "input-current"],
                raw: 6,
                metric_type: "component_power_supply_input_current",
                unit: "amperes",
            },
            Case {
                component_name: "PSU1",
                tail: &["power-supply", "state", "input-voltage"],
                raw: 230,
                metric_type: "component_power_supply_input_voltage",
                unit: "volts",
            },
            Case {
                component_name: "PSU1",
                tail: &["power-supply", "state", "output-power"],
                raw: 1_200,
                metric_type: "component_power_supply_output_power",
                unit: "watts",
            },
            Case {
                component_name: "PSU1",
                tail: &["power-supply", "state", "output-voltage"],
                raw: 48,
                metric_type: "component_power_supply_output_voltage",
                unit: "volts",
            },
        ];

        for case in cases {
            let sample = run_component_leaf(
                case.component_name,
                case.tail,
                make_typed_value_uint(case.raw),
            );

            assert_eq!(
                sample.key,
                format!("{}:{}", case.metric_type, case.component_name)
            );

            assert_eq!(sample.metric_type, case.metric_type);
            assert_eq!(sample.unit, case.unit);
            assert_eq!(sample.value, case.raw as f64);
        }
    }

    #[test]
    fn test_component_last_reboot_reason_info() {
        let samples = run_component_leaf_all(
            "SYSTEM",
            &["state", "last-reboot-reason"],
            make_typed_value_string("POWER_CYCLE"),
        );

        assert_eq!(samples.len(), 1);

        let sample = &samples[0];

        assert_eq!(sample.name, NVUE_GNMI_SAMPLE_STREAM_ID);
        assert_eq!(sample.key, "component_last_reboot_reason:SYSTEM");
        assert_eq!(sample.metric_type, "component_last_reboot_reason");
        assert_eq!(sample.unit, "info");
        assert_eq!(sample.value, 1.0);
        assert!(sample.context.is_none());

        assert_eq!(
            sample.labels,
            vec![
                (Cow::Borrowed("component_name"), "SYSTEM".to_string()),
                (Cow::Borrowed("reboot_reason"), "POWER_CYCLE".to_string()),
            ]
        );

        assert!(
            run_component_leaf_all(
                "SYSTEM",
                &["state", "last-reboot-reason"],
                make_typed_value_string(""),
            )
            .is_empty()
        );
    }

    #[test]
    fn test_component_oper_status_shared_leaf_fan_and_cpu() {
        // FAN-STATE (row 966) and CPU-STATE (row 1174) share state/oper-status;
        // the component_name label is the only discriminator. Emitted as a
        // StateSet (one 0/1 series per state).
        let fan = run_component_leaf_all(
            "FAN1/1",
            &["state", "oper-status"],
            make_typed_value_string("ACTIVE"),
        );
        assert_state_set(
            &fan,
            "component_oper_status",
            "component_name",
            "FAN1/1",
            OPER_STATUS_STATES,
            "up",
        );

        let cpu = run_component_leaf_all(
            "cpu",
            &["state", "oper-status"],
            make_typed_value_string("DOWN"),
        );
        assert_state_set(
            &cpu,
            "component_oper_status",
            "component_name",
            "cpu",
            OPER_STATUS_STATES,
            "down",
        );
    }

    #[test]
    fn test_component_health_status_state_set() {
        // healthz status emitted as a 3-state StateSet; unrecognized => unknown.
        for (raw, current) in [
            ("healthy", "healthy"),
            ("unhealthy", "unhealthy"),
            ("something_weird", "unknown"),
        ] {
            let samples = run_component_leaf_all(
                "ASIC1",
                &["healthz", "state", "status"],
                make_typed_value_string(raw),
            );
            assert_state_set(
                &samples,
                "component_health_status",
                "component_name",
                "ASIC1",
                COMPONENT_HEALTH_STATES,
                current,
            );
        }
    }

    #[test]
    fn test_unknown_interface_leaf_is_not_exported() {
        // a live but unmapped leaf (e.g. ip-address, which is not in any
        // canonical mapping arm or the numeric table) must never produce a
        // MetricSample.
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());
        let notification = proto::Notification {
            timestamp: 0,
            prefix: Some(proto::Path {
                elem: vec![
                    make_path_elem("interfaces", &[]),
                    make_path_elem("interface", &[("name", "acp0")]),
                ],
                ..Default::default()
            }),
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        make_path_elem("state", &[]),
                        make_path_elem("ip-address", &[]),
                    ],
                    ..Default::default()
                }),
                val: Some(make_typed_value_string("10.0.0.1")),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);
        assert_eq!(
            sink.events.lock().expect("lock poisoned").len(),
            0,
            "unmapped leaf must not emit a metric"
        );
    }

    #[test]
    fn test_link_width_to_f64_helper() {
        assert_eq!(link_width_to_f64(Some("1X")), Some(1.0));
        assert_eq!(link_width_to_f64(Some("2X")), Some(2.0));
        assert_eq!(link_width_to_f64(Some("4x")), Some(4.0));
        // comma-composite supported-widths -> max lane count
        assert_eq!(link_width_to_f64(Some("1X,2X,4X")), Some(4.0));
        assert_eq!(link_width_to_f64(Some("1X, 2X")), Some(2.0));
        // partially-unrecognized composites still yield the max of the valid lanes
        assert_eq!(link_width_to_f64(Some("2X,foo")), Some(2.0));
        assert_eq!(link_width_to_f64(Some("-1X")), None);
        assert_eq!(link_width_to_f64(Some("NaNX")), None);
        assert_eq!(link_width_to_f64(Some("infX")), None);
        assert_eq!(link_width_to_f64(Some("VL0-VL7")), None);
        assert_eq!(link_width_to_f64(Some("")), None);
        assert_eq!(link_width_to_f64(None), None);
    }

    #[test]
    fn test_link_speed_to_gbps_helper() {
        // live GB200: bare numerics are already Gbps
        assert_eq!(link_speed_to_gbps(Some("400")), Some(400.0));
        assert_eq!(link_speed_to_gbps(Some("100")), Some(100.0));
        assert_eq!(link_speed_to_gbps(Some("0")), Some(0.0));
        assert_eq!(link_speed_to_gbps(Some("2.5")), Some(2.5));
        // defensive: trailing "G"/"g" suffix (NVOS schema enum form)
        assert_eq!(link_speed_to_gbps(Some("400G")), Some(400.0));
        assert_eq!(link_speed_to_gbps(Some("2.5g")), Some(2.5));
        // defensive: Mb/s and M suffix -> divide by 1000
        assert_eq!(link_speed_to_gbps(Some("1000Mb/s")), Some(1.0));
        assert_eq!(link_speed_to_gbps(Some("1000M")), Some(1.0));
        assert_eq!(link_speed_to_gbps(Some("-1000M")), None);
        assert_eq!(link_speed_to_gbps(Some("NaN")), None);
        assert_eq!(link_speed_to_gbps(Some("inf")), None);
        assert_eq!(link_speed_to_gbps(Some("-1")), None);
        // unrecognized -> None
        assert_eq!(link_speed_to_gbps(Some("hdr")), None);
        assert_eq!(link_speed_to_gbps(Some("")), None);
        assert_eq!(link_speed_to_gbps(None), None);
    }

    #[test]
    fn test_interface_link_speed_active_gbps() {
        // bare numerics (live GB200 form) pass through as Gbps
        for (raw, expected) in [("400", 400.0), ("100", 100.0), ("0", 0.0)] {
            let (sample, _) = run_interface_leaf(
                &["infiniband", "state", "speed"],
                make_typed_value_string(raw),
            );
            assert_eq!(sample.metric_type, "interface_link_speed_active");
            assert_eq!(sample.unit, "gbps", "speed unit must be gbps for {raw}");
            assert_eq!(sample.value, expected, "speed {raw}");
        }

        // defensive suffix forms
        let (g_suffix, _) = run_interface_leaf(
            &["infiniband", "state", "speed"],
            make_typed_value_string("400G"),
        );
        assert_eq!(g_suffix.unit, "gbps");
        assert_eq!(g_suffix.value, 400.0);

        let (g_frac, _) = run_interface_leaf(
            &["infiniband", "state", "speed"],
            make_typed_value_string("2.5G"),
        );
        assert_eq!(g_frac.value, 2.5);

        let (mb, _) = run_interface_leaf(
            &["infiniband", "state", "speed"],
            make_typed_value_string("1000Mb/s"),
        );
        assert_eq!(mb.unit, "gbps");
        assert_eq!(mb.value, 1.0);
    }

    #[test]
    fn test_interface_link_speed_unparseable_is_not_exported() {
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());
        let notification = proto::Notification {
            timestamp: 0,
            prefix: Some(proto::Path {
                elem: vec![
                    make_path_elem("interfaces", &[]),
                    make_path_elem("interface", &[("name", "acp0")]),
                ],
                ..Default::default()
            }),
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        make_path_elem("infiniband", &[]),
                        make_path_elem("state", &[]),
                        make_path_elem("speed", &[]),
                    ],
                    ..Default::default()
                }),
                val: Some(make_typed_value_string("hdr")),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);
        assert_eq!(
            sink.events.lock().expect("lock poisoned").len(),
            0,
            "unparseable speed must not emit a metric"
        );
    }

    #[test]
    fn test_oper_status_active_is_up() {
        assert_eq!(oper_status_to_state(Some("ACTIVE")), "up");
        assert_eq!(oper_status_to_state(Some("active")), "up");
        assert_eq!(oper_status_to_state(Some("DOWN")), "down");
    }

    #[test]
    fn test_process_subscribe_response_update_increments_notification_counter() {
        let proc = test_processor();
        let metrics = test_stream_metrics();
        let resp = proto::SubscribeResponse {
            response: Some(proto::subscribe_response::Response::Update(
                proto::Notification {
                    timestamp: 0,
                    prefix: Some(proto::Path {
                        elem: vec![
                            make_path_elem("interfaces", &[]),
                            make_path_elem("interface", &[("name", "nvl0")]),
                        ],
                        ..Default::default()
                    }),
                    update: vec![proto::Update {
                        path: Some(proto::Path {
                            elem: vec![
                                make_path_elem("state", &[]),
                                make_path_elem("oper-status", &[]),
                            ],
                            ..Default::default()
                        }),
                        val: Some(make_typed_value_string("UP")),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )),
            ..Default::default()
        };

        proc.process_subscribe_response(&resp, &metrics);

        assert_eq!(metrics.notifications_received_total.get(), 1.0);
        assert_eq!(metrics.monitored_entities.get(), 1.0);
        assert_eq!(metrics.stream_errors_total.get(), 0.0);
    }

    // ---- /platform-general switch-level singleton coverage -----------------

    /// Drive a single `/platform-general/<tail...>` update and return the one
    /// captured `MetricSample`, asserting the producer-level invariants (stream
    /// `name`, `collector_type`, and that the switch-level singleton carries no
    /// per-entity name label).
    fn run_platform_general_leaf(tail: &[&str], val: proto::TypedValue) -> MetricSample {
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());

        let mut elems = vec![make_path_elem("platform-general", &[])];
        elems.extend(tail.iter().map(|n| make_path_elem(n, &[])));

        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: elems,
                    ..Default::default()
                }),
                val: Some(val),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);

        let events = sink.events.lock().expect("lock poisoned");
        assert_eq!(events.len(), 1, "expected exactly one emitted metric");
        let (ctx, event) = events[0].clone();
        let CollectorEvent::Metric(sample) = event else {
            panic!("expected a Metric event");
        };
        assert_eq!(sample.name, NVUE_GNMI_SAMPLE_STREAM_ID);
        assert_eq!(ctx.collector_type, NVUE_GNMI_SAMPLE_STREAM_ID);
        assert!(
            sample.labels.is_empty(),
            "switch-level singleton must not carry a per-entity name label"
        );
        *sample
    }

    #[test]
    fn platform_general_numeric_leaves_emit_expected_metrics() {
        let cases: &[(&[&str], u64, &str, &str)] = &[
            (
                &["state", "memory-used"],
                3_856_510_976,
                "platform_memory_used",
                "bytes",
            ),
            (
                &["state", "memory-total-size"],
                16_151_990_272,
                "platform_memory_total",
                "bytes",
            ),
            (
                &["state", "disk-total-size"],
                77_780_082_688,
                "platform_disk_total",
                "bytes",
            ),
            (
                &["state", "disk-used"],
                22_848_192_512,
                "platform_disk_used",
                "bytes",
            ),
            (
                &["state", "ambient-temperature"],
                27,
                "platform_ambient_temperature",
                "celsius",
            ),
        ];

        for (tail, raw, metric_type, unit) in cases {
            let sample = run_platform_general_leaf(tail, make_typed_value_uint(*raw));

            assert_eq!(sample.key, *metric_type, "leaf {tail:?}");
            assert_eq!(sample.metric_type, *metric_type, "leaf {tail:?}");
            assert_eq!(sample.unit, *unit, "leaf {tail:?}");
            assert_eq!(sample.value, *raw as f64, "leaf {tail:?} value");
        }
    }

    #[test]
    fn test_platform_general_non_numeric_value_is_not_exported() {
        // A numeric leaf whose value cannot be coerced to f64 emits nothing.
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());
        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        make_path_elem("platform-general", &[]),
                        make_path_elem("state", &[]),
                        make_path_elem("memory-used", &[]),
                    ],
                    ..Default::default()
                }),
                val: Some(make_typed_value_string("not-a-number")),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);
        assert_eq!(
            sink.events.lock().expect("lock poisoned").len(),
            0,
            "non-numeric platform-general value must not emit a metric"
        );
    }

    #[test]
    fn test_platform_general_unmapped_string_leaf_is_not_exported() {
        // A platform-general string leaf that is not one of the mapped info
        // leaves (contact/location/platform-name) must fall through and emit
        // nothing, while still being counted as the platform-general entity.
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());
        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        make_path_elem("platform-general", &[]),
                        make_path_elem("state", &[]),
                        make_path_elem("product-name", &[]),
                    ],
                    ..Default::default()
                }),
                val: Some(make_typed_value_string("MQM9700")),
                ..Default::default()
            }],
            ..Default::default()
        };
        let count = proc.process_notification(&notification);
        // the platform-general entity is still counted, but nothing is emitted
        assert_eq!(count, 1);
        assert_eq!(
            sink.events.lock().expect("lock poisoned").len(),
            0,
            "unmapped platform-general string leaf must not emit a metric"
        );
    }

    #[test]
    fn test_platform_general_empty_info_string_is_not_exported() {
        // CONTACT/LOCATION are empty on the GB200 rig; an empty info string
        // carries no information and must emit nothing.
        for leaf in ["contact", "location", "platform-name"] {
            let sink = Arc::new(CapturingSink::default());
            let mut proc = test_processor();
            proc.data_sink = Some(sink.clone());
            let notification = proto::Notification {
                timestamp: 0,
                prefix: None,
                update: vec![proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("platform-general", &[]),
                            make_path_elem("state", &[]),
                            make_path_elem(leaf, &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string("")),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let count = proc.process_notification(&notification);
            assert_eq!(
                count, 1,
                "platform-general entity is still counted for {leaf}"
            );
            assert_eq!(
                sink.events.lock().expect("lock poisoned").len(),
                0,
                "empty info string must not emit a metric for {leaf}"
            );
        }
    }

    #[test]
    fn test_platform_general_node_description_info() {
        // NODE-DESCRIPTION (row 864): a non-empty platform-name emits a single
        // switch-level info-metric carrying the raw string as `node_description`.
        let sample = run_platform_general_leaf_info(
            &["state", "platform-name"],
            "x86_64-nvidia_n5400_ld-r0",
        );
        assert_eq!(sample.metric_type, "platform_node_description_info");
        assert_eq!(sample.unit, "info");
        assert_eq!(sample.value, 1.0);
        assert_eq!(
            sample.labels,
            vec![(
                Cow::Borrowed("node_description"),
                "x86_64-nvidia_n5400_ld-r0".to_string()
            )]
        );
    }

    #[test]
    fn test_platform_general_contact_and_location_info() {
        // CONTACT (862) / LOCATION (863): non-empty strings emit their info
        // series with the matching single label.
        for (leaf, metric_type, label, raw) in [
            (
                "contact",
                "platform_contact_info",
                "contact",
                "noc@example.com",
            ),
            ("location", "platform_location_info", "location", "rack-7"),
        ] {
            let sample = run_platform_general_leaf_info(&["state", leaf], raw);
            assert_eq!(sample.metric_type, metric_type, "leaf {leaf}");
            assert_eq!(sample.unit, "info", "leaf {leaf}");
            assert_eq!(sample.value, 1.0, "leaf {leaf}");
            assert_eq!(
                sample.labels,
                vec![(Cow::Borrowed(label), raw.to_string())],
                "leaf {leaf}"
            );
        }
    }

    #[test]
    fn test_platform_general_version_info_metrics() {
        // OS-VERSION (868) / BMC-VERSION (869) / EROT-FW-VERSION (870): non-empty
        // version strings under `/platform-general/versions/state` each emit one
        // switch-level info-metric carrying the raw version in a single label.
        // Values are the authoritative live GB200 Stage-0 capture.
        for (tail, metric_type, label, raw) in [
            (
                ["versions", "state", "nos-version"],
                "platform_os_version_info",
                "os_version",
                "nvos-25.02.2553",
            ),
            (
                ["versions", "state", "fw-version-bmc"],
                "platform_bmc_version_info",
                "bmc_version",
                "88.0002.1336",
            ),
            (
                ["versions", "state", "fw-version-erot"],
                "platform_erot_version_info",
                "erot_version",
                "01.04.0026.0000_n04",
            ),
        ] {
            let sample = run_platform_general_leaf_info(&tail, raw);
            assert_eq!(sample.metric_type, metric_type, "leaf {tail:?}");
            assert_eq!(sample.unit, "info", "leaf {tail:?}");
            assert_eq!(sample.value, 1.0, "leaf {tail:?}");
            assert_eq!(
                sample.labels,
                vec![(Cow::Borrowed(label), raw.to_string())],
                "leaf {tail:?}"
            );
        }
    }

    #[test]
    fn test_platform_general_empty_version_string_is_not_exported() {
        // An empty version string carries no information and must emit nothing,
        // while still being counted as the platform-general entity.
        for tail in [
            ["versions", "state", "nos-version"],
            ["versions", "state", "fw-version-bmc"],
            ["versions", "state", "fw-version-erot"],
        ] {
            let sink = Arc::new(CapturingSink::default());
            let mut proc = test_processor();
            proc.data_sink = Some(sink.clone());
            let mut elems = vec![make_path_elem("platform-general", &[])];
            elems.extend(tail.iter().map(|n| make_path_elem(n, &[])));
            let notification = proto::Notification {
                timestamp: 0,
                prefix: None,
                update: vec![proto::Update {
                    path: Some(proto::Path {
                        elem: elems,
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string("")),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let count = proc.process_notification(&notification);
            assert_eq!(
                count, 1,
                "platform-general entity is still counted for {tail:?}"
            );
            assert_eq!(
                sink.events.lock().expect("lock poisoned").len(),
                0,
                "empty version string must not emit a metric for {tail:?}"
            );
        }
    }

    /// Drive a single `/platform-general/<tail...>` string update and return the
    /// one captured info `MetricSample`. Unlike `run_platform_general_leaf`, the
    /// switch-level info series carries a single string label (no per-entity
    /// name), so the empty-labels invariant does not apply.
    fn run_platform_general_leaf_info(tail: &[&str], raw: &str) -> MetricSample {
        let sink = Arc::new(CapturingSink::default());
        let mut proc = test_processor();
        proc.data_sink = Some(sink.clone());

        let mut elems = vec![make_path_elem("platform-general", &[])];
        elems.extend(tail.iter().map(|n| make_path_elem(n, &[])));

        let notification = proto::Notification {
            timestamp: 0,
            prefix: None,
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: elems,
                    ..Default::default()
                }),
                val: Some(make_typed_value_string(raw)),
                ..Default::default()
            }],
            ..Default::default()
        };
        proc.process_notification(&notification);

        let events = sink.events.lock().expect("lock poisoned");
        assert_eq!(events.len(), 1, "expected exactly one emitted metric");
        let (ctx, event) = events[0].clone();
        let CollectorEvent::Metric(sample) = event else {
            panic!("expected a Metric event");
        };
        assert_eq!(sample.name, NVUE_GNMI_SAMPLE_STREAM_ID);
        assert_eq!(ctx.collector_type, NVUE_GNMI_SAMPLE_STREAM_ID);
        *sample
    }
}
