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

//! This module collects metrics from NMX-T telemetry endpoints on NVLink switches if the service is enabled.
//! Scrapes HTTP on 9352 by default. When an mTLS profile is configured, scrapes
//! HTTPS on the same port.
//!
//! Mapping is an EXPLICIT, catalog-row allowlist over the live NMX-T Prometheus scrape (see
//! `NMXT_METRIC_MAP` and `NMXT_LABEL_MAP`). Each NMX-T source name is either:
//!   * a numeric **family** -> emitted as one canonical `switch_nmxt` series (`NMXT_METRIC_MAP`), or
//!   * an identity/inventory **label dimension** carried on every series -> re-exported as a
//!     canonical label, never as a standalone metric (`NMXT_LABEL_MAP`).
//!
//! Source names not on either allowlist are skipped and counted only (never sanitized into telemetry).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nv_redfish::core::Bmc;
use url::Url;

use crate::HealthError;
use crate::collectors::{IterationResult, PeriodicCollector};
use crate::config::NmxtCollectorConfig as NmxtCollectorOptions;
use crate::endpoint::BmcEndpoint;
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample};
use crate::tls::MtlsHttpClientProvider;

/// default NMX-T port
pub(crate) const NMXT_PORT: u16 = 9352;

/// NMX-T endpoint
const NMXT_ENDPOINT: &str = "/xcset/nvlink_domain_telemetry";

/// MetricSample name for NMX-T metrics
const NMXT_METRIC_NAME: &str = "switch_nmxt";

#[derive(Debug, PartialEq)]
struct NmxtMetric {
    source: &'static str,
    metric_type: &'static str,
    unit: &'static str,
}

#[derive(Debug, PartialEq)]
struct NmxtLabel {
    source: &'static str,
    canonical: &'static str,
}

const NMXT_METRIC_MAP: &[NmxtMetric] = &[
    NmxtMetric {
        source: "Effective_BER",
        metric_type: "effective_ber",
        unit: "ratio",
    },
    NmxtMetric {
        source: "Symbol_Errors",
        metric_type: "symbol_errors",
        unit: "count",
    }, // PHY-SYMBOL-ERRORS
    NmxtMetric {
        source: "Link_Down",
        metric_type: "link_down",
        unit: "count",
    },
    NmxtMetric {
        source: "lid",
        metric_type: "lid",
        unit: "id",
    }, // LID
    NmxtMetric {
        source: "device_hw_rev",
        metric_type: "device_hw_rev",
        unit: "id",
    }, // DEVICE-HARDWARE-REVISION
    NmxtMetric {
        source: "Advanced_Status_Opcode",
        metric_type: "status_opcode",
        unit: "code",
    }, // STATUS-OPCODE
    NmxtMetric {
        source: "remote_reason_opcode",
        metric_type: "remote_reason_opcode",
        unit: "code",
    }, // REMOTE-REASON-OPCODE
    NmxtMetric {
        source: "time_to_link_up_ext_msec",
        metric_type: "time_to_link_up",
        unit: "milliseconds",
    }, // TIME-TO-LINKS-UP
    NmxtMetric {
        source: "cable_technology",
        metric_type: "cable_transmitter_technology",
        unit: "code",
    }, // CABLE-TRANSMITTER-TECHNOLOGY
    NmxtMetric {
        source: "rx_power_lane_0",
        metric_type: "cable_rx_power_lane0",
        unit: "milliwatts",
    }, // CABLE-RX-POWER-LANE0
    NmxtMetric {
        source: "rx_power_lane_1",
        metric_type: "cable_rx_power_lane1",
        unit: "milliwatts",
    }, // CABLE-RX-POWER-LANE1
    NmxtMetric {
        source: "Module_Voltage",
        metric_type: "cable_diag_supply_voltage",
        unit: "volts",
    }, // CABLE-DIAG-SUPPLY-VOLTAGE
    NmxtMetric {
        source: "link_partner_lid",
        metric_type: "link_partner_lid",
        unit: "id",
    }, // LINK-PARTNER-LID
    NmxtMetric {
        source: "successful_recovery_events",
        metric_type: "link_recovery_success_cnt",
        unit: "count",
    }, // LINK-RECOVERY-SUCCESS-CNT
    NmxtMetric {
        source: "total_successful_recovery_events",
        metric_type: "total_link_recovery_success_cnt",
        unit: "count",
    }, // TOTAL-LINK-RECOVERY-SUCCESS-CNT
    NmxtMetric {
        source: "time_since_last_recovery",
        metric_type: "time_since_last_recovery",
        unit: "seconds",
    }, // TIME-SINCE-LAST-RECOVERY
    NmxtMetric {
        source: "time_between_last_2_recoveries",
        metric_type: "time_btwn_two_recoveries",
        unit: "seconds",
    }, // TIME-BTWN-TWO-RECOVERIES
    NmxtMetric {
        source: "last_host_logical_recovery_attempts_count",
        metric_type: "recovery_attempts_l1_cnt",
        unit: "count",
    }, // RECOVERY-ATTEMPTS-L1-CNT
    NmxtMetric {
        source: "last_host_serdes_feq_attempts_count",
        metric_type: "recovery_attempts_l2_cnt",
        unit: "count",
    }, // RECOVERY-ATTEMPTS-L2-CNT
    NmxtMetric {
        source: "time_in_last_host_logical_recovery",
        metric_type: "recovery_cycle_duration",
        unit: "seconds",
    }, // RECOVERY-CYCLE-DURATION
    NmxtMetric {
        source: "time_in_last_host_serdes_feq_recovery",
        metric_type: "serdes_recovery_cycle_duration",
        unit: "seconds",
    }, // SERDES-RECOVERY-CYCLE-DURATION
    NmxtMetric {
        source: "contain_n_drain_xmit_discards",
        metric_type: "contain_drain_xmit_discard",
        unit: "count",
    }, // CONTAIN-DRAIN-XMIT-DISCARD
    NmxtMetric {
        source: "contain_n_drain_rcv_discards",
        metric_type: "contain_drain_rcv_discard",
        unit: "count",
    }, // CONTAIN-DRAIN-RCV-DISCARD
    NmxtMetric {
        source: "Raw_Errors_Lane_2",
        metric_type: "raw_err_lane_2",
        unit: "count",
    }, // RAW-ERR-LANE-2
    NmxtMetric {
        source: "Raw_Errors_Lane_3",
        metric_type: "raw_err_lane_3",
        unit: "count",
    }, // RAW-ERR-LANE-3
    NmxtMetric {
        source: "tx_cdr_lol",
        metric_type: "cable_tx_cdr_lol",
        unit: "state",
    }, // CABLE-TX-CDR-LOL
    NmxtMetric {
        source: "rx_cdr_lol",
        metric_type: "cable_rx_cdr_lol",
        unit: "state",
    }, // CABLE-RX-CDR-LOL
    NmxtMetric {
        source: "tx_los",
        metric_type: "cable_tx_los",
        unit: "state",
    }, // CABLE-TX-LOS
    NmxtMetric {
        source: "rx_los",
        metric_type: "cable_rx_los",
        unit: "state",
    }, // CABLE-RX-LOS
];

const NMXT_LABEL_MAP: &[NmxtLabel] = &[
    NmxtLabel {
        source: "FW_Version",
        canonical: "net_fw_ver",
    }, // NET-FW-VER
    NmxtLabel {
        source: "sw_serial_number",
        canonical: "serial",
    }, // SERIAL
    NmxtLabel {
        source: "Node_GUID",
        canonical: "node_guid",
    }, // NODE-GUID
    NmxtLabel {
        source: "port_guid",
        canonical: "port_guid",
    }, // PORT-GUID
    NmxtLabel {
        source: "Port_Number",
        canonical: "port_num",
    }, // PORT-NUMBER
    NmxtLabel {
        source: "port_label",
        canonical: "port_label",
    }, // PORT-LABEL
    NmxtLabel {
        source: "sw_revision",
        canonical: "revision",
    }, // REVISION
    NmxtLabel {
        source: "Active_FEC",
        canonical: "fec_mode_active",
    }, // FEC-MODE-ACTIVE
    NmxtLabel {
        source: "Device_ID",
        canonical: "device_id",
    }, // DEVICE-ID
    NmxtLabel {
        source: "local_reason_opcode",
        canonical: "local_reason_opcode",
    }, // LOCAL-REASON-OPCODE
    NmxtLabel {
        source: "Cable_PN",
        canonical: "cable_part_number",
    }, // CABLE-PART-NUMBER
    NmxtLabel {
        source: "Cable_SN",
        canonical: "cable_serial_number",
    }, // CABLE-SERIAL-NUMBER
    NmxtLabel {
        source: "cable_type",
        canonical: "cable_type",
    }, // CABLE-TYPE
    NmxtLabel {
        source: "cable_vendor",
        canonical: "cable_vendor",
    }, // CABLE-VENDOR
    NmxtLabel {
        source: "cable_length",
        canonical: "cable_length",
    }, // CABLE-LENGTH
    NmxtLabel {
        source: "cable_identifier",
        canonical: "cable_identifier",
    }, // CABLE-IDENTIFIER
    NmxtLabel {
        source: "vendor_rev",
        canonical: "cable_rev",
    }, // CABLE-REV
    NmxtLabel {
        source: "cable_fw_version",
        canonical: "cable_fw_version",
    }, // CABLE-FW-VERSION
    NmxtLabel {
        source: "link_partner_description",
        canonical: "link_partner_description",
    }, // LINK-PARTNER-DESCRIPTION
    NmxtLabel {
        source: "link_partner_node_guid",
        canonical: "link_partner_node_guid",
    }, // LINK-PARTNER-NODE-GUID
    NmxtLabel {
        source: "link_partner_port_num",
        canonical: "link_partner_port_num",
    }, // LINK-PARTNER-PORT-NUM
    NmxtLabel {
        source: "device_num_on_tray",
        canonical: "device_num",
    }, // DEVICE-NUM
    NmxtLabel {
        source: "board_type",
        canonical: "board_type",
    }, // BOARD-TYPE
    NmxtLabel {
        source: "chassis_slot_index",
        canonical: "chassis_slot_idx",
    }, // CHASSIS-SLOT-IDX
    NmxtLabel {
        source: "tray_index",
        canonical: "tray_idx",
    }, // TRAY-IDX
    NmxtLabel {
        source: "topology_id",
        canonical: "topology_id",
    }, // TOPOLOGY-ID
    NmxtLabel {
        source: "chassis_id",
        canonical: "chassis_id",
    }, // CHASSIS-ID
];

fn lookup_nmxt_metric(name: &str) -> Option<&'static NmxtMetric> {
    NMXT_METRIC_MAP.iter().find(|m| m.source == name)
}

/// Parse `Module_Temperature` as a label value (e.g. `"0C"`), never its own numeric
/// line and emit as a gauge with either numeric or `None
fn cable_temp_to_celsius(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    let digits = trimmed.strip_suffix(['C', 'c']).unwrap_or(trimmed).trim();
    digits.parse::<f64>().ok()
}

/// Enum for `down_blame`, emitted as a StateSet (one 0/1 series per state).
const DOWN_BLAME_STATES: &[&str] = &["unknown", "local_phy", "remote_phy"];

/// Maps a raw `down_blame` value to its canonical state, case-insensitively; unknown/empty -> "unknown".
fn down_blame_to_state(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "local_phy" => "local_phy",
        "remote_phy" => "remote_phy",
        _ => "unknown",
    }
}

fn required_port_num<'a>(sample_labels: &HashMap<&str, &'a str>) -> Option<&'a str> {
    sample_labels
        .get("Port_Number")
        .copied()
        .filter(|port_num| !port_num.is_empty())
}

#[cfg(test)]
fn lookup_nmxt_label(key: &str) -> Option<&'static NmxtLabel> {
    NMXT_LABEL_MAP.iter().find(|l| l.source == key)
}

/// Parses one line into a reusable lookup borrowing only fields used by emission.
/// Clearing the lookup also prevents malformed lines from reusing previous labels.
fn parse_prometheus_line<'a>(
    line: &'a str,
    labels: &mut HashMap<&'a str, &'a str>,
) -> Option<(&'a str, f64)> {
    labels.clear();

    let (name_part, rest) = if let Some(brace_pos) = line.find('{') {
        let name = &line[..brace_pos];
        let rest = &line[brace_pos..];
        (name, rest)
    } else {
        let mut parts = line.split_whitespace();
        let name = parts.next()?;
        let value = parts.next()?.parse::<f64>().ok()?;

        return Some((name, value));
    };

    let close_brace = rest.find('}')?;
    let labels_str = &rest[1..close_brace];
    let value_part = rest[close_brace + 1..].trim();
    let value_str = value_part.split_whitespace().next()?;
    let value = value_str.parse::<f64>().ok()?;

    for label_pair in labels_str.split(',') {
        let label_pair = label_pair.trim();
        if let Some(eq_pos) = label_pair.find('=') {
            let key = label_pair[..eq_pos].trim();
            let val = label_pair[eq_pos + 1..].trim().trim_matches('"');
            if matches!(key, "Module_Temperature" | "down_blame")
                || NMXT_LABEL_MAP.iter().any(|label| label.source == key)
            {
                labels.insert(key, val);
            }
        }
    }

    Some((name_part, value))
}

async fn scrape_switch_nmxt_metrics(
    http_client: &reqwest::Client,
    url: &str,
) -> Result<bytes::Bytes, HealthError> {
    let response =
        http_client.get(url).send().await.map_err(|e| {
            HealthError::GenericError(format!("HTTP request failed for {url}: {e}"))
        })?;

    if !response.status().is_success() {
        return Err(HealthError::GenericError(format!(
            "HTTP request to {} returned status {}",
            url,
            response.status()
        )));
    }

    response.bytes().await.map_err(|e| {
        HealthError::GenericError(format!("Failed to read response body from {url}: {e}"))
    })
}

async fn scrape_switch_nmxt_metrics_tls(
    http_client: &crate::tls::MtlsHttpClient,
    url: &str,
    request_timeout: std::time::Duration,
) -> Result<bytes::Bytes, HealthError> {
    let url = Url::parse(url)
        .map_err(|e| HealthError::GenericError(format!("{url}: invalid NMX-T URL: {e}")))?;

    let response = http_client
        .get(&url, [], request_timeout)
        .await
        .map_err(|e| HealthError::GenericError(format!("HTTP request failed for {url}: {e}")))?;

    if !response.status.is_success() {
        return Err(HealthError::GenericError(format!(
            "HTTP request to {} returned status {}",
            url, response.status
        )));
    }

    Ok(response.body)
}

fn nmxt_endpoint_url(switch_ip: &str, tls_enabled: bool) -> String {
    let scheme = if tls_enabled { "https" } else { "http" };
    format!("{scheme}://{}:{}{}", switch_ip, NMXT_PORT, NMXT_ENDPOINT)
}

pub struct NmxtCollectorConfig {
    /// User-facing NMX-T collector settings from the health service configuration.
    pub nmxt_config: NmxtCollectorOptions,

    /// Optional sink that receives NMX-T metric events.
    pub data_sink: Option<Arc<dyn DataSink>>,

    /// Shared mTLS HTTP client provider used for HTTPS scrapes when configured.
    pub(crate) tls_http_client_provider: Option<MtlsHttpClientProvider>,
}

pub struct NmxtCollector {
    endpoint: Arc<BmcEndpoint>,
    http_client: NmxtHttpClient,
    request_timeout: std::time::Duration,
    event_context: EventContext,
    data_sink: Option<Arc<dyn DataSink>>,

    // Local HTTP tests use an assigned port without changing production URLs.
    #[cfg(test)]
    http_url_override: Option<String>,
}

enum NmxtHttpClient {
    Legacy(reqwest::Client),

    // The shared provider owns mTLS reload and client reuse. Target identity
    // stays on the request URL, so switch inventory changes only clone or drop
    // this provider.
    Tls { provider: MtlsHttpClientProvider },
}

impl<B: Bmc + 'static> PeriodicCollector<B> for NmxtCollector {
    type Config = NmxtCollectorConfig;

    fn new_runner(
        _bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let event_context = EventContext::from_endpoint(endpoint.as_ref(), "nmxt");
        let request_timeout = config.nmxt_config.request_timeout;

        let http_client = match config.tls_http_client_provider {
            Some(provider) => NmxtHttpClient::Tls { provider },
            None => {
                let mut http_client_builder = reqwest::Client::builder().timeout(request_timeout);

                if config.nmxt_config.dangerously_skip_tls_verification {
                    http_client_builder = http_client_builder.danger_accept_invalid_certs(true);
                }

                let http_client = http_client_builder.build().map_err(|e| {
                    HealthError::GenericError(format!("Failed to create HTTP client: {}", e))
                })?;

                NmxtHttpClient::Legacy(http_client)
            }
        };

        Ok(Self {
            endpoint,
            http_client,
            request_timeout,
            event_context,
            data_sink: config.data_sink,
            #[cfg(test)]
            http_url_override: None,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        self.scrape_iteration().await?;
        Ok(IterationResult {
            refresh_triggered: true,
            entity_count: None,
            fetch_failures: 0,
        })
    }

    fn collector_type(&self) -> &'static str {
        "nmxt"
    }

    async fn stop(&mut self) {
        self.emit_event(CollectorEvent::CollectorRemoved);
    }
}

impl NmxtCollector {
    fn emit_event(&self, event: CollectorEvent) {
        if let Some(data_sink) = &self.data_sink {
            data_sink.handle_event(&self.event_context, &event);
        }
    }

    /// Builds label set for one `switch_nmxt` series
    fn build_labels(
        &self,
        sample_labels: &HashMap<&str, &str>,
    ) -> Vec<(Cow<'static, str>, String)> {
        let mut labels: Vec<(Cow<'static, str>, String)> = Vec::with_capacity(NMXT_LABEL_MAP.len());

        for label in NMXT_LABEL_MAP {
            if let Some(value) = sample_labels.get(label.source) {
                labels.push((Cow::Borrowed(label.canonical), (*value).to_string()));
            }
        }

        labels
    }

    /// Emits a completed scrape without retaining an owned copy of every row.
    fn emit_metric_collection(&self, body: &str) {
        self.emit_event(CollectorEvent::MetricCollectionStart);

        // Ports already emitted a cable temperature this iteration (one series per port).
        let mut cable_temp_ports: HashSet<&str> = HashSet::new();
        // Ports already emitted a down_blame StateSet this iteration (one set per port).
        let mut down_blame_ports: HashSet<&str> = HashSet::new();
        let mut sample_labels = HashMap::with_capacity(NMXT_LABEL_MAP.len() + 2);

        for line in body.lines() {
            let line = line.trim();

            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((name, value)) = parse_prometheus_line(line, &mut sample_labels) else {
                continue;
            };

            // `Module_Temperature` rides as a label on lines whose map entry may not be
            // collected. Emit before the map check, once per port.
            if let Some(celsius) = sample_labels
                .get("Module_Temperature")
                .and_then(|raw| cable_temp_to_celsius(raw))
            {
                let Some(port_num) = required_port_num(&sample_labels) else {
                    continue;
                };

                if cable_temp_ports.insert(port_num) {
                    let labels = self.build_labels(&sample_labels);
                    self.emit_event(CollectorEvent::Metric(
                        MetricSample {
                            key: format!("cable_temperature_celsius:{}", port_num),
                            name: NMXT_METRIC_NAME.to_string(),
                            metric_type: "cable_temperature_celsius".to_string(),
                            unit: "celsius".to_string(),
                            value: celsius,
                            labels,
                            context: None,
                        }
                        .into(),
                    ));
                }
            }

            // `down_blame` is an enum riding as a label; emit per port as a StateSet
            if let Some(raw) = sample_labels.get("down_blame") {
                let Some(port_num) = required_port_num(&sample_labels) else {
                    continue;
                };

                if down_blame_ports.insert(port_num) {
                    let current = down_blame_to_state(raw);
                    let base_labels = self.build_labels(&sample_labels);
                    for state in DOWN_BLAME_STATES {
                        let mut labels = base_labels.clone();
                        labels.push((Cow::Borrowed("state"), (*state).to_string()));
                        self.emit_event(CollectorEvent::Metric(
                            MetricSample {
                                key: format!("down_blame:{}:{}", port_num, state),
                                name: NMXT_METRIC_NAME.to_string(),
                                metric_type: "down_blame".to_string(),
                                unit: "state".to_string(),
                                value: if *state == current { 1.0 } else { 0.0 },
                                labels,
                                context: None,
                            }
                            .into(),
                        ));
                    }
                }
            }

            let Some(metric) = lookup_nmxt_metric(name) else {
                continue;
            };
            let (metric_type, unit) = (metric.metric_type, metric.unit);

            // Port number anchors the per-series key.
            let Some(port_num) = required_port_num(&sample_labels) else {
                continue;
            };

            let mut metric_key = String::with_capacity(metric_type.len() + 1 + port_num.len());
            metric_key.push_str(metric_type);
            metric_key.push(':');
            metric_key.push_str(port_num);

            let labels = self.build_labels(&sample_labels);

            self.emit_event(CollectorEvent::Metric(
                MetricSample {
                    key: metric_key,
                    name: NMXT_METRIC_NAME.to_string(),
                    metric_type: metric_type.to_string(),
                    unit: unit.to_string(),
                    value,
                    labels,
                    context: None,
                }
                .into(),
            ));
        }

        self.emit_event(CollectorEvent::MetricCollectionEnd);
    }

    async fn scrape_iteration(&mut self) -> Result<(), HealthError> {
        let switch_connect_host = self.endpoint.switch_connect_host_for_uri().to_string();

        match &mut self.http_client {
            NmxtHttpClient::Legacy(client) => {
                let url = nmxt_endpoint_url(&switch_connect_host, false);
                #[cfg(test)]
                let url = self.http_url_override.clone().unwrap_or(url);

                let body = scrape_switch_nmxt_metrics(client, &url).await?;

                // Borrow valid UTF-8 to avoid copying the full response, while
                // preserving replacement characters for invalid UTF-8.
                let body = String::from_utf8_lossy(&body);

                self.emit_metric_collection(&body);
            }
            NmxtHttpClient::Tls { provider } => {
                let client = provider.client().await?;

                let url = nmxt_endpoint_url(&switch_connect_host, true);

                let body =
                    scrape_switch_nmxt_metrics_tls(&client, &url, self.request_timeout).await?;

                let body = std::str::from_utf8(&body).map_err(|e| {
                    HealthError::GenericError(format!(
                        "Failed to read response body from {url}: {e}"
                    ))
                })?;

                self.emit_metric_collection(body);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};

    #[derive(Clone, Debug, PartialEq)]
    struct ObservedMetric {
        key: String,
        name: String,
        metric_type: String,
        unit: String,
        value: f64,
        labels: Vec<(String, String)>,
        has_context: bool,
    }

    #[derive(Clone, Debug, PartialEq)]
    enum ObservedEvent {
        Start,
        Metric(ObservedMetric),
        End,
    }

    #[derive(Default)]
    struct CapturingSink {
        events: StdMutex<Vec<ObservedEvent>>,
    }

    impl DataSink for CapturingSink {
        fn sink_type(&self) -> &'static str {
            "capturing_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), crate::HealthError> {
            let observed = match event {
                CollectorEvent::MetricCollectionStart => Some(ObservedEvent::Start),
                CollectorEvent::Metric(sample) => Some(ObservedEvent::Metric(ObservedMetric {
                    key: sample.key.clone(),
                    name: sample.name.clone(),
                    metric_type: sample.metric_type.clone(),
                    unit: sample.unit.clone(),
                    value: sample.value,
                    labels: sample
                        .labels
                        .iter()
                        .map(|(key, value)| (key.to_string(), value.clone()))
                        .collect(),
                    has_context: sample.context.is_some(),
                })),
                CollectorEvent::MetricCollectionEnd => Some(ObservedEvent::End),
                CollectorEvent::CollectorRemoved
                | CollectorEvent::Log(_)
                | CollectorEvent::Firmware(_)
                | CollectorEvent::HealthReport(_) => None,
            };

            if let Some(observed) = observed {
                self.events.lock().unwrap().push(observed);
            }
            Ok(())
        }
    }

    fn observed_metric(
        key: &str,
        metric_type: &str,
        unit: &str,
        value: f64,
        labels: &[(&str, &str)],
    ) -> ObservedEvent {
        ObservedEvent::Metric(ObservedMetric {
            key: key.to_string(),
            name: "switch_nmxt".to_string(),
            metric_type: metric_type.to_string(),
            unit: unit.to_string(),
            value,
            labels: labels
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            has_context: false,
        })
    }

    fn observed_collection(metrics: Vec<ObservedEvent>) -> Vec<ObservedEvent> {
        std::iter::once(ObservedEvent::Start)
            .chain(metrics)
            .chain(std::iter::once(ObservedEvent::End))
            .collect()
    }

    fn collect_metric_events(body: &str) -> Vec<ObservedEvent> {
        let endpoint = Arc::new(test_endpoint(mac("00:11:22:33:44:55")));
        let sink = Arc::new(CapturingSink::default());
        let collector = NmxtCollector {
            endpoint: endpoint.clone(),
            http_client: NmxtHttpClient::Legacy(reqwest::Client::new()),
            request_timeout: std::time::Duration::from_secs(30),
            event_context: EventContext::from_endpoint(endpoint.as_ref(), "nmxt"),
            data_sink: Some(sink.clone()),
            http_url_override: None,
        };

        collector.emit_metric_collection(body);

        sink.events.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn failed_http_scrapes_preserve_snapshot_and_recover()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        use axum::body::{Body, Bytes};
        use axum::routing::get;
        use futures::{StreamExt, stream};

        use crate::metrics::MetricsManager;
        use crate::sink::{CompositeDataSink, PrometheusSink};

        let mode = Arc::new(AtomicUsize::new(0));
        let route_mode = mode.clone();

        let router = axum::Router::new().route(
            NMXT_ENDPOINT,
            get(move || {
                let mode = route_mode.load(Ordering::Relaxed);

                async move {
                    let prefix = Bytes::from_static(b"Effective_BER{Port_Number=\"2\"} 0\n");

                    let body = match mode {
                        1 => Body::from_stream(stream::iter([
                            Ok(prefix),
                            Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "interrupted scrape",
                            )),
                        ])),
                        2 => Body::from_stream(
                            stream::once(async move { Ok::<_, std::io::Error>(prefix) })
                                .chain(stream::pending()),
                        ),
                        3 => Body::from("Effective_BER{Port_Number=\"2\"} 2\n"),
                        4 => Body::from(Bytes::from_static(
                            b"Effective_BER{Port_Number=\"2\",Node_GUID=\"node-\xff\"} 3\n",
                        )),
                        _ => Body::from(prefix),
                    };

                    axum::response::Response::new(body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let listen_addr = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let manager = Arc::new(MetricsManager::new("test")?);
        let prometheus = Arc::new(PrometheusSink::new(manager.clone(), "test")?);
        let capture = Arc::new(CapturingSink::default());

        let sink = Arc::new(CompositeDataSink::new(
            vec![prometheus, capture.clone()],
            manager.clone(),
        ));

        let mut endpoint = test_endpoint(mac("00:11:22:33:44:55"));
        endpoint.addr.ip = "127.0.0.1".parse()?;
        let endpoint = Arc::new(endpoint);

        let mut collector = NmxtCollector {
            endpoint: endpoint.clone(),
            http_client: NmxtHttpClient::Legacy(
                reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_millis(100))
                    .build()?,
            ),
            request_timeout: Duration::from_millis(100),
            event_context: EventContext::from_endpoint(endpoint.as_ref(), "nmxt"),
            data_sink: Some(sink),
            http_url_override: Some(format!("http://{listen_addr}{NMXT_ENDPOINT}")),
        };

        collector.scrape_iteration().await?;
        let initial = manager.export_telemetry()?;

        assert!(initial.contains("test_switch_nmxt_effective_ber_ratio"));
        assert_eq!(capture.events.lock().unwrap().len(), 3);

        for failure_mode in [1, 2] {
            mode.store(failure_mode, Ordering::Relaxed);

            let result = collector.scrape_iteration().await;

            assert!(result.is_err(), "failure mode {failure_mode}");

            assert_eq!(capture.events.lock().unwrap().len(), 3);
            assert_eq!(manager.export_telemetry()?, initial);
        }

        mode.store(3, Ordering::Relaxed);
        collector.scrape_iteration().await?;

        assert_eq!(capture.events.lock().unwrap().len(), 6);

        assert!(
            capture
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| matches!(event, ObservedEvent::Metric(metric) if metric.value == 2.0))
        );

        assert_ne!(manager.export_telemetry()?, initial);

        capture.events.lock().unwrap().clear();
        mode.store(4, Ordering::Relaxed);
        collector.scrape_iteration().await?;

        assert_eq!(
            *capture.events.lock().unwrap(),
            observed_collection(vec![observed_metric(
                "effective_ber:2",
                "effective_ber",
                "ratio",
                3.0,
                &[("node_guid", "node-\u{fffd}"), ("port_num", "2")],
            )])
        );

        server.abort();
        let _ = server.await;

        Ok(())
    }

    #[test]
    fn borrowed_label_lookup_does_not_reuse_previous_line() {
        let mut labels = HashMap::new();

        assert!(
            parse_prometheus_line(
                r#"Effective_BER{Port_Number="2",unknown="ignored"} 1"#,
                &mut labels
            )
            .is_some()
        );

        assert_eq!(labels, HashMap::from([("Port_Number", "2")]));
        assert!(parse_prometheus_line("malformed", &mut labels).is_none());
        assert!(labels.is_empty());

        assert_eq!(
            parse_prometheus_line("Effective_BER 0", &mut labels),
            Some(("Effective_BER", 0.0))
        );

        assert!(labels.is_empty());
    }

    #[test]
    fn test_nmxt_endpoint_url_switches_scheme_when_tls_enabled() {
        struct TestCase {
            name: &'static str,
            tls_enabled: bool,
            expected: &'static str,
        }

        let cases = [
            TestCase {
                name: "legacy HTTP",
                tls_enabled: false,
                expected: "http://10.0.0.9:9352/xcset/nvlink_domain_telemetry",
            },
            TestCase {
                name: "mTLS HTTPS",
                tls_enabled: true,
                expected: "https://10.0.0.9:9352/xcset/nvlink_domain_telemetry",
            },
        ];

        for case in cases {
            let actual = nmxt_endpoint_url("10.0.0.9", case.tls_enabled);

            assert_eq!(actual, case.expected, "{}", case.name);
        }
    }

    #[test]
    fn test_parse_prometheus_line_with_labels() {
        let line = r#"Effective_BER{Port_Number="2", Node_GUID="0x8e2161c8803caf64"} 1.5e-254"#;
        let mut labels = HashMap::new();
        let (name, value) = parse_prometheus_line(line, &mut labels).unwrap();

        assert_eq!(name, "Effective_BER");
        assert_eq!(labels.get("Port_Number"), Some(&"2"));
        assert_eq!(labels.get("Node_GUID"), Some(&"0x8e2161c8803caf64"));
        assert_eq!(value, 1.5e-254);
    }

    #[test]
    fn test_parse_prometheus_line_no_labels() {
        let line = "simple_metric 42.5 1234567890";
        let mut labels = HashMap::new();
        let (name, value) = parse_prometheus_line(line, &mut labels).unwrap();

        assert_eq!(name, "simple_metric");
        assert!(labels.is_empty());
        assert_eq!(value, 42.5);
    }

    #[test]
    fn test_parse_prometheus_metrics() {
        let body = r#"
# HELP Effective_BER Effective bit error rate
# TYPE Effective_BER gauge
Effective_BER{Port_Number="1"} 0
Effective_BER{Port_Number="2"} 1e-10
Symbol_Errors{Port_Number="1"} 0
Link_Down{Port_Number="1"} 5
"#;

        let events = collect_metric_events(body);

        assert_eq!(events.len(), 6);
        assert!(matches!(events.first(), Some(ObservedEvent::Start)));
        assert!(matches!(events.last(), Some(ObservedEvent::End)));
    }

    #[test]
    fn test_required_port_num_requires_present_non_empty_label() {
        let missing = HashMap::new();
        assert_eq!(required_port_num(&missing), None);

        let mut empty = HashMap::new();
        empty.insert("Port_Number", "");

        assert_eq!(required_port_num(&empty), None);

        let mut present = HashMap::new();
        present.insert("Port_Number", "11");

        assert_eq!(required_port_num(&present), Some("11"));
    }

    /// Live NMX-T `lid` series from the Stage-0 GB200 scrape (`nmxt-prometheus.txt`).
    const SAMPLE_LID_LINE: &str = r#"lid{Device_ID="GB100", port_label="GPUP10", logical_state="ACT", device_num_on_tray="2", board_type="3", chassis_slot_index="27", tray_index="17", topology_id="128", chassis_id="1820325172739", Active_FEC="Int_KP4_FEC_PLR", link_partner_description="MF0;sw06:N5400_LD/U1", link_partner_node_guid="0x2c5eab0300b6a900", link_partner_port_num="71", cable_vendor="Other", down_blame="Unknown", local_reason_opcode="No_link_down_indication", Node_GUID="0xe1d04a69816f16bc", node_description="GB100 Nvidia Technologies", Port_Number="11", FW_Version="36.2014.1866", Cable_PN="NA", Cable_SN="NA", cable_type="850 nm VCSEL", cable_length="NA", cable_identifier="Backplane", vendor_rev="NA", cable_fw_version="N/A", Module_Temperature="0C", Status_Message="No issue was observed", port_guid="0xe1d04a69816f16c6", sw_serial_number="MT123", sw_revision="A1", remote_reason_opcode="4"}  3093 1781993954087"#;

    #[test]
    fn test_nmxt_metric_map_locks_type_and_unit() {
        let expected: &[(&str, &str, &str)] = &[
            ("Effective_BER", "effective_ber", "ratio"),
            ("Symbol_Errors", "symbol_errors", "count"),
            ("Link_Down", "link_down", "count"),
            ("lid", "lid", "id"),
            ("device_hw_rev", "device_hw_rev", "id"),
            ("Advanced_Status_Opcode", "status_opcode", "code"),
            ("remote_reason_opcode", "remote_reason_opcode", "code"),
            (
                "time_to_link_up_ext_msec",
                "time_to_link_up",
                "milliseconds",
            ),
            ("cable_technology", "cable_transmitter_technology", "code"),
            ("rx_power_lane_0", "cable_rx_power_lane0", "milliwatts"),
            ("rx_power_lane_1", "cable_rx_power_lane1", "milliwatts"),
            ("Module_Voltage", "cable_diag_supply_voltage", "volts"),
            ("link_partner_lid", "link_partner_lid", "id"),
            (
                "successful_recovery_events",
                "link_recovery_success_cnt",
                "count",
            ),
            (
                "total_successful_recovery_events",
                "total_link_recovery_success_cnt",
                "count",
            ),
            (
                "time_since_last_recovery",
                "time_since_last_recovery",
                "seconds",
            ),
            (
                "time_between_last_2_recoveries",
                "time_btwn_two_recoveries",
                "seconds",
            ),
            (
                "last_host_logical_recovery_attempts_count",
                "recovery_attempts_l1_cnt",
                "count",
            ),
            (
                "last_host_serdes_feq_attempts_count",
                "recovery_attempts_l2_cnt",
                "count",
            ),
            (
                "time_in_last_host_logical_recovery",
                "recovery_cycle_duration",
                "seconds",
            ),
            (
                "time_in_last_host_serdes_feq_recovery",
                "serdes_recovery_cycle_duration",
                "seconds",
            ),
            (
                "contain_n_drain_xmit_discards",
                "contain_drain_xmit_discard",
                "count",
            ),
            (
                "contain_n_drain_rcv_discards",
                "contain_drain_rcv_discard",
                "count",
            ),
            ("Raw_Errors_Lane_2", "raw_err_lane_2", "count"),
            ("Raw_Errors_Lane_3", "raw_err_lane_3", "count"),
            ("tx_cdr_lol", "cable_tx_cdr_lol", "state"),
            ("rx_cdr_lol", "cable_rx_cdr_lol", "state"),
            ("tx_los", "cable_tx_los", "state"),
            ("rx_los", "cable_rx_los", "state"),
        ];

        for (source, metric_type, unit) in expected {
            let m = lookup_nmxt_metric(source)
                .unwrap_or_else(|| panic!("family `{source}` must be allowlisted"));
            assert_eq!(
                (m.metric_type, m.unit),
                (*metric_type, *unit),
                "family `{source}` must map to ({metric_type}, {unit})"
            );
        }
        // The allowlist must contain exactly these explicit families (no extras, no generic).
        assert_eq!(NMXT_METRIC_MAP.len(), expected.len());
    }

    #[test]
    fn test_nmxt_label_map_locks_canonical_names() {
        let expected: &[(&str, &str)] = &[
            ("FW_Version", "net_fw_ver"),
            ("sw_serial_number", "serial"),
            ("Node_GUID", "node_guid"),
            ("port_guid", "port_guid"),
            ("Port_Number", "port_num"),
            ("port_label", "port_label"),
            ("sw_revision", "revision"),
            ("Active_FEC", "fec_mode_active"),
            ("Device_ID", "device_id"),
            ("local_reason_opcode", "local_reason_opcode"),
            ("Cable_PN", "cable_part_number"),
            ("Cable_SN", "cable_serial_number"),
            ("cable_type", "cable_type"),
            ("cable_vendor", "cable_vendor"),
            ("cable_length", "cable_length"),
            ("cable_identifier", "cable_identifier"),
            ("vendor_rev", "cable_rev"),
            ("cable_fw_version", "cable_fw_version"),
            ("link_partner_description", "link_partner_description"),
            ("link_partner_node_guid", "link_partner_node_guid"),
            ("link_partner_port_num", "link_partner_port_num"),
            ("device_num_on_tray", "device_num"),
            ("board_type", "board_type"),
            ("chassis_slot_index", "chassis_slot_idx"),
            ("tray_index", "tray_idx"),
            ("topology_id", "topology_id"),
            ("chassis_id", "chassis_id"),
        ];

        for (key, canonical) in expected {
            assert_eq!(
                lookup_nmxt_label(key).map(|l| l.canonical),
                Some(*canonical),
                "label `{key}` must map to canonical `{canonical}`"
            );
        }
        assert_eq!(NMXT_LABEL_MAP.len(), expected.len());
    }

    // Unknown NMX-T source names are not on either allowlist (never sanitized into telemetry).
    #[test]
    fn test_unknown_nmxt_sources_not_allowlisted() {
        // Live-but-blocked families and arbitrary unknowns: all must be rejected.
        for unknown in [
            "HiRetransmissionRate", // row 931, not live
            "rq_num_wrfe",          // row 1706, not live
            "rq_num_lle",           // row 1707, not live
            "sq_num_wrfe",          // row 1708, not live
            "Chip_Temp",            // threshold blocker, not an NMX-T explicit mapping
            "totally_made_up_metric",
        ] {
            assert!(
                lookup_nmxt_metric(unknown).is_none(),
                "`{unknown}` must not be an allowlisted family"
            );
            assert!(
                lookup_nmxt_label(unknown).is_none(),
                "`{unknown}` must not be an allowlisted label"
            );
        }
    }

    // End-to-end: a live family line yields one canonical key and re-exported allowlisted labels.
    #[test]
    fn test_label_map_reexports_identity_dims_from_live_series() {
        let mut labels = HashMap::new();

        let (name, _) =
            parse_prometheus_line(SAMPLE_LID_LINE, &mut labels).expect("parse lid line");

        assert_eq!(name, "lid");

        // Resolve canonical labels exactly as build_labels would (allowlist-gated).
        let mut canonical = HashMap::new();

        for label in NMXT_LABEL_MAP {
            if let Some(value) = labels.get(label.source) {
                canonical.insert(label.canonical, (*value).to_string());
            }
        }

        assert_eq!(
            canonical.get("node_guid"),
            Some(&"0xe1d04a69816f16bc".to_string())
        );
        assert_eq!(
            canonical.get("port_guid"),
            Some(&"0xe1d04a69816f16c6".to_string())
        );
        assert_eq!(canonical.get("port_num"), Some(&"11".to_string()));
        assert_eq!(canonical.get("port_label"), Some(&"GPUP10".to_string()));
        assert_eq!(
            canonical.get("net_fw_ver"),
            Some(&"36.2014.1866".to_string())
        );
        assert_eq!(canonical.get("serial"), Some(&"MT123".to_string()));
        assert_eq!(canonical.get("revision"), Some(&"A1".to_string()));
        assert_eq!(canonical.get("device_id"), Some(&"GB100".to_string()));
        assert_eq!(
            canonical.get("fec_mode_active"),
            Some(&"Int_KP4_FEC_PLR".to_string())
        );
        assert_eq!(canonical.get("cable_part_number"), Some(&"NA".to_string()));
        // Module_Temperature is no longer a re-exported label; it becomes a numeric metric.
        assert!(!canonical.contains_key("cable_temp"));

        assert_eq!(
            labels
                .get("Module_Temperature")
                .and_then(|raw| cable_temp_to_celsius(raw)),
            Some(0.0)
        );
        assert_eq!(
            canonical.get("chassis_id"),
            Some(&"1820325172739".to_string())
        );
        assert_eq!(
            canonical.get("link_partner_node_guid"),
            Some(&"0x2c5eab0300b6a900".to_string())
        );

        // node_description is present on the series but NOT allowlisted -> not re-exported.
        assert!(!canonical.contains_key("node_description"));
    }

    #[test]
    fn test_down_blame_to_state() {
        assert_eq!(down_blame_to_state("Unknown"), "unknown");
        assert_eq!(down_blame_to_state("Local_phy"), "local_phy");
        assert_eq!(down_blame_to_state("Remote_phy"), "remote_phy");
        // Case-insensitive.
        assert_eq!(down_blame_to_state("LOCAL_PHY"), "local_phy");
        assert_eq!(down_blame_to_state("remote_PHY"), "remote_phy");
        // Unrecognized / empty -> "unknown".
        assert_eq!(down_blame_to_state("garbage"), "unknown");
        assert_eq!(down_blame_to_state(""), "unknown");
    }

    #[test]
    fn test_cable_temp_to_celsius() {
        assert_eq!(cable_temp_to_celsius("0C"), Some(0.0));
        assert_eq!(cable_temp_to_celsius("37C"), Some(37.0));
        assert_eq!(cable_temp_to_celsius("37.5C"), Some(37.5));
        assert_eq!(cable_temp_to_celsius("N/A"), None);
        assert_eq!(cable_temp_to_celsius(""), None);
        assert_eq!(cable_temp_to_celsius("NA"), None);
    }

    #[test]
    fn test_emit_metric_collection() {
        check_values(
            [
                Check {
                    scenario: "empty scrape preserves collection boundaries",
                    input: "",
                    expect: observed_collection(vec![]),
                },
                Check {
                    scenario: "down blame state set is emitted once per port",
                    input: r#"
                        lid{Port_Number="11", down_blame="Remote_phy"} 3093
                        Effective_BER{Port_Number="11", down_blame="Remote_phy"} 0
                    "#,
                    expect: observed_collection(vec![
                        observed_metric(
                            "down_blame:11:unknown",
                            "down_blame",
                            "state",
                            0.0,
                            &[("port_num", "11"), ("state", "unknown")],
                        ),
                        observed_metric(
                            "down_blame:11:local_phy",
                            "down_blame",
                            "state",
                            0.0,
                            &[("port_num", "11"), ("state", "local_phy")],
                        ),
                        observed_metric(
                            "down_blame:11:remote_phy",
                            "down_blame",
                            "state",
                            1.0,
                            &[("port_num", "11"), ("state", "remote_phy")],
                        ),
                        observed_metric("lid:11", "lid", "id", 3093.0, &[("port_num", "11")]),
                        observed_metric(
                            "effective_ber:11",
                            "effective_ber",
                            "ratio",
                            0.0,
                            &[("port_num", "11")],
                        ),
                    ]),
                },
                Check {
                    scenario: "cable temperature is emitted once per port",
                    input: r#"
                        lid{Port_Number="11", Module_Temperature="37.5C"} 3093
                        Effective_BER{Port_Number="11", Module_Temperature="37.5C"} 0
                    "#,
                    expect: observed_collection(vec![
                        observed_metric(
                            "cable_temperature_celsius:11",
                            "cable_temperature_celsius",
                            "celsius",
                            37.5,
                            &[("port_num", "11")],
                        ),
                        observed_metric("lid:11", "lid", "id", 3093.0, &[("port_num", "11")]),
                        observed_metric(
                            "effective_ber:11",
                            "effective_ber",
                            "ratio",
                            0.0,
                            &[("port_num", "11")],
                        ),
                    ]),
                },
                Check {
                    scenario: "label metrics precede filtering and deduplicate independently per port",
                    input: r#"
                        blocked_a{FW_Version="1.0", Port_Number="12", Module_Temperature="40C", down_blame="Local_phy"} 1
                        blocked_b{FW_Version="2.0", Port_Number="12", Module_Temperature="99C", down_blame="Remote_phy"} 2
                        blocked_c{Port_Number="13", Module_Temperature="41C", down_blame="Remote_phy"} 3
                    "#,
                    expect: observed_collection(vec![
                        observed_metric(
                            "cable_temperature_celsius:12",
                            "cable_temperature_celsius",
                            "celsius",
                            40.0,
                            &[("net_fw_ver", "1.0"), ("port_num", "12")],
                        ),
                        observed_metric(
                            "down_blame:12:unknown",
                            "down_blame",
                            "state",
                            0.0,
                            &[
                                ("net_fw_ver", "1.0"),
                                ("port_num", "12"),
                                ("state", "unknown"),
                            ],
                        ),
                        observed_metric(
                            "down_blame:12:local_phy",
                            "down_blame",
                            "state",
                            1.0,
                            &[
                                ("net_fw_ver", "1.0"),
                                ("port_num", "12"),
                                ("state", "local_phy"),
                            ],
                        ),
                        observed_metric(
                            "down_blame:12:remote_phy",
                            "down_blame",
                            "state",
                            0.0,
                            &[
                                ("net_fw_ver", "1.0"),
                                ("port_num", "12"),
                                ("state", "remote_phy"),
                            ],
                        ),
                        observed_metric(
                            "cable_temperature_celsius:13",
                            "cable_temperature_celsius",
                            "celsius",
                            41.0,
                            &[("port_num", "13")],
                        ),
                        observed_metric(
                            "down_blame:13:unknown",
                            "down_blame",
                            "state",
                            0.0,
                            &[("port_num", "13"), ("state", "unknown")],
                        ),
                        observed_metric(
                            "down_blame:13:local_phy",
                            "down_blame",
                            "state",
                            0.0,
                            &[("port_num", "13"), ("state", "local_phy")],
                        ),
                        observed_metric(
                            "down_blame:13:remote_phy",
                            "down_blame",
                            "state",
                            1.0,
                            &[("port_num", "13"), ("state", "remote_phy")],
                        ),
                    ]),
                },
                Check {
                    scenario: "missing port suppresses every metric path",
                    input: r#"
                        blocked_temp{Module_Temperature="37C"} 1
                        blocked_blame{down_blame="Remote_phy"} 1
                        lid 1
                    "#,
                    expect: observed_collection(vec![]),
                },
                Check {
                    scenario: "invalid label metric on blocked family is ignored",
                    input: r#"blocked{Port_Number="14", Module_Temperature="N/A"} 1"#,
                    expect: observed_collection(vec![]),
                },
                Check {
                    scenario: "mapped family reexports canonical identity labels in catalog order",
                    input: r#"Effective_BER{Port_Number="2", Node_GUID="0x123", FW_Version="36.1"} 1.5e-10"#,
                    expect: observed_collection(vec![observed_metric(
                        "effective_ber:2",
                        "effective_ber",
                        "ratio",
                        1.5e-10,
                        &[
                            ("net_fw_ver", "36.1"),
                            ("node_guid", "0x123"),
                            ("port_num", "2"),
                        ],
                    )]),
                },
            ],
            collect_metric_events,
        );
    }
}
