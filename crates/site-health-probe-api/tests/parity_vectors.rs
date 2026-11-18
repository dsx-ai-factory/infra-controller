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

//! Shared metric and filter-to-metric parity vectors for the Go runner's
//! narrow v1 history contract (`dev/k8s/scale-test/internal/probeapi`). This
//! crate owns `metrics.rs`; these files pin what the Go `series_spec`,
//! `selector_problems` and `gauge_labels` must reproduce for the v1 filters:
//! the ten ingestion-stage filters, the host Ready-and-allocatable gate and
//! its capability, the full-vs-compact host read boundary, the gauge label
//! contract and the reserved/invalid selector dimensions. They live outside
//! `testdata/v1/` so the wire-vector inventory stays separate. Regenerate with
//! `UPDATE_PARITY=1 cargo test -p carbide-site-health-probe-api --test parity_vectors`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use carbide_site_health_probe_api::{Filter, metrics};
use serde::Serialize;
use serde_json::{Value, json};

fn parity_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/parity")
}

#[derive(Serialize)]
struct GaugeOut {
    name: String,
    labels: Vec<String>,
}

#[derive(Serialize)]
struct Contract {
    #[serde(rename = "contractVersion")]
    contract_version: u32,
    gauges: Vec<GaugeOut>,
}

#[derive(Serialize)]
struct NeedOut {
    gauge: String,
    label: String,
    value: String,
}

#[derive(Serialize)]
struct SeriesOut {
    metric: String,
    family: String,
    matchers: Vec<[String; 2]>,
    needs: Vec<NeedOut>,
}

#[derive(Serialize)]
struct Outcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    series: Option<SeriesOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<[String; 2]>, // [code, field]
}

#[derive(Serialize)]
struct FilterCase {
    filter: Value,
    compact: Outcome,
    full: Outcome,
}

#[derive(Serialize)]
struct SelectorCase {
    selector: BTreeMap<String, String>,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct Parity {
    filters: Vec<FilterCase>,
    selectors: Vec<SelectorCase>,
}

fn outcome(filter: &Value, full_host_read: bool) -> Outcome {
    let f: Filter = serde_json::from_value(filter.clone()).expect("a valid filter");
    match metrics::series_spec(&f, &metrics::Capabilities { full_host_read }) {
        Ok(spec) => Outcome {
            series: Some(SeriesOut {
                metric: spec.metric.to_string(),
                family: metrics::wire(spec.family),
                matchers: spec
                    .matchers
                    .iter()
                    .map(|(l, v)| [l.to_string(), v.clone()])
                    .collect(),
                needs: spec
                    .needs
                    .iter()
                    .map(|n| {
                        let c = n.capability();
                        NeedOut {
                            gauge: c.gauge.metric_name(),
                            label: c.label.to_string(),
                            value: c.value,
                        }
                    })
                    .collect(),
            }),
            error: None,
        },
        Err(e) => Outcome {
            series: None,
            error: Some([metrics::wire(e.code), e.field]),
        },
    }
}

fn filter_case(filter: Value) -> FilterCase {
    let compact = outcome(&filter, false);
    let full = outcome(&filter, true);
    FilterCase {
        filter,
        compact,
        full,
    }
}

fn selector_case(pairs: &[(&str, &str)]) -> SelectorCase {
    let selector: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    SelectorCase {
        problems: metrics::selector_problems(&selector),
        selector,
    }
}

fn parity() -> Parity {
    // The ten fixed ingestion-stage filters (spec-E 5.2), their failed
    // filters, and the host Ready-and-allocatable gate.
    let filters = vec![
        json!({"type": "expected_record", "recordType": "rack", "linked": true}),
        json!({"type": "expected_record", "recordType": "host", "linked": true}),
        json!({"type": "expected_record", "recordType": "switch", "linked": true}),
        json!({"type": "expected_record", "recordType": "power_shelf", "linked": true}),
        json!({"type": "rack", "stage": "ready"}),
        json!({"type": "rack", "stage": "error"}),
        json!({"type": "host", "stage": "ready"}),
        json!({"type": "host", "stage": "error"}),
        json!({"type": "host", "stage": "ready", "allocatable": true}),
        json!({"type": "host", "stage": "ready", "allocatable": true, "source": "nico-api"}),
        json!({"type": "host", "stage": "expected"}),
        json!({"type": "host", "stage": "expected", "allocatable": true}),
        json!({"type": "switch", "stage": "expected"}),
        json!({"type": "dpu", "stage": "expected"}),
        json!({"type": "dpu", "stage": "ready"}),
        json!({"type": "switch", "stage": "ready"}),
        json!({"type": "power_shelf", "stage": "ready"}),
    ];
    let selectors = vec![
        selector_case(&[("job", "j"), ("namespace", "ns")]),
        selector_case(&[]),
        selector_case(&[("family", "host")]),
        selector_case(&[("type", "x")]),
        selector_case(&[("__scope", "y")]),
        selector_case(&[("1bad", "x")]),
        selector_case(&[("a-b", "x")]),
        selector_case(&[("job", "  ")]),
        selector_case(&[("job", "a\tb")]),
    ];
    Parity {
        filters: filters.into_iter().map(filter_case).collect(),
        selectors,
    }
}

fn contract() -> Contract {
    Contract {
        contract_version: metrics::CONTRACT_VERSION,
        gauges: metrics::GAUGES
            .iter()
            .map(|g| GaugeOut {
                name: g.name.to_string(),
                labels: g.labels.iter().map(|l| l.to_string()).collect(),
            })
            .collect(),
    }
}

/// Writes `file` under `testdata/parity` when `UPDATE_PARITY` is set, else
/// checks it is already the one-line JSON of `value`.
fn golden<T: Serialize>(file: &str, value: &T) {
    let path = parity_dir().join(file);
    let want = serde_json::to_string(value).expect("serialize") + "\n";
    if std::env::var_os("UPDATE_PARITY").is_some() {
        fs::create_dir_all(parity_dir()).expect("mkdir");
        fs::write(&path, &want).expect("write");
        return;
    }
    let got = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with UPDATE_PARITY=1", path.display()));
    assert_eq!(got, want, "{} is stale; run with UPDATE_PARITY=1", file);
}

#[test]
fn parity_vectors_round_trip() {
    golden("metrics-contract.json", &contract());
    golden("filter-selectors.json", &parity());
}
