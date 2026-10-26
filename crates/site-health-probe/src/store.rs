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

//! The metrics store `/v1/summary` reads the probe's own series back from:
//! any Prometheus-compatible store with the instant query API. A range
//! selector in an instant query returns the raw samples, so the summary sees
//! every scrape, not a resampled curve.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use url::Url;

use crate::framework::ObservationRecorder;
use crate::gate::{CallError, CallGate, classify_http};
use crate::rest::client::read_capped;

/// A store reply bigger than this is not a few hours of probe series.
const REPLY_CAP: usize = 64 << 20;

/// How a label matcher compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchOp {
    Eq,
    Re,
}

/// A range selector over one or more probe series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawQuery {
    /// Full metric names. More than one is a `__name__` regex.
    pub names: Vec<String>,
    pub matchers: Vec<(String, MatchOp, String)>,
    pub range: Duration,
}

/// One series and its raw samples, in Unix milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawSeries {
    pub labels: BTreeMap<String, String>,
    pub samples: Vec<(i64, f64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoreError {
    /// The store answered with an error, or not at all.
    Unavailable(String),
    /// A store call ran past its deadline.
    Timeout,
}

#[async_trait::async_trait]
pub(crate) trait MetricsStore: Send + Sync {
    /// The raw samples of `query` in `(at - range, at]`.
    async fn raw(&self, query: &RawQuery, at: SystemTime) -> Result<Vec<RawSeries>, StoreError>;
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The PromQL text of a range selector.
pub(crate) fn render_promql(query: &RawQuery) -> String {
    let mut matchers = Vec::new();
    let mut text = String::new();
    match query.names.as_slice() {
        [name] => text.push_str(name),
        names => {
            let common = "carbide_site_health_probe_";
            let all_probe = names.iter().all(|n| n.starts_with(common));
            let alternatives: Vec<&str> = names
                .iter()
                .map(|n| {
                    if all_probe {
                        &n[common.len()..]
                    } else {
                        n.as_str()
                    }
                })
                .collect();
            let prefix = if all_probe { common } else { "" };
            matchers.push(format!(
                "__name__=~\"{prefix}({})\"",
                alternatives.join("|")
            ));
        }
    }
    for (label, op, value) in &query.matchers {
        let op = match op {
            MatchOp::Eq => "=",
            MatchOp::Re => "=~",
        };
        matchers.push(format!("{label}{op}\"{}\"", escape(value)));
    }
    let _ = write!(
        text,
        "{{{}}}[{}]",
        matchers.join(","),
        range_text(query.range)
    );
    text
}

/// A range selector duration that covers `range` exactly, or outward: whole
/// seconds as `Ns`, anything else in milliseconds, rounded up.
fn range_text(range: Duration) -> String {
    let ms = range.as_nanos().div_ceil(1_000_000).max(1);
    if ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}

/// The instant query time, in Unix seconds to the millisecond (sample
/// timestamps are milliseconds).
fn unix_seconds_text(at: SystemTime) -> String {
    let at = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:03}", at.as_secs(), at.subsec_millis())
}

/// The matrix of an instant query's JSON reply.
fn parse_matrix(body: &[u8]) -> Result<Vec<RawSeries>, StoreError> {
    #[derive(serde::Deserialize)]
    struct Reply {
        status: String,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        data: Option<Data>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        result_type: String,
        result: Vec<Series>,
    }
    #[derive(serde::Deserialize)]
    struct Series {
        metric: BTreeMap<String, String>,
        #[serde(default)]
        values: Vec<(f64, String)>,
    }
    let reply: Reply = serde_json::from_slice(body)
        .map_err(|e| StoreError::Unavailable(format!("store reply: {e}")))?;
    if reply.status != "success" {
        return Err(StoreError::Unavailable(format!(
            "store query failed: {}",
            reply.error.unwrap_or(reply.status)
        )));
    }
    let data = reply
        .data
        .ok_or_else(|| StoreError::Unavailable("store reply has no data".to_string()))?;
    if data.result_type != "matrix" {
        return Err(StoreError::Unavailable(format!(
            "store reply is a {}, not a matrix",
            data.result_type
        )));
    }
    data.result
        .into_iter()
        .map(|series| {
            let samples = series
                .values
                .into_iter()
                .map(|(at, value)| {
                    // The probe publishes finite numbers only; NaN or an
                    // infinity is not its sample.
                    let value: f64 = value
                        .parse()
                        .ok()
                        .filter(|v: &f64| v.is_finite())
                        .ok_or_else(|| {
                            StoreError::Unavailable(format!(
                                "sample value {value:?} is not a finite number"
                            ))
                        })?;
                    Ok(((at * 1000.0).round() as i64, value))
                })
                .collect::<Result<_, _>>()?;
            Ok(RawSeries {
                labels: series.metric,
                samples,
            })
        })
        .collect()
}

/// A Prometheus-compatible store over HTTP, through the metrics-store gate.
pub(crate) struct PromStore {
    http: reqwest::Client,
    query_url: Url,
    gate: Arc<CallGate>,
}

impl PromStore {
    pub(crate) fn new(http: reqwest::Client, url: &str, gate: Arc<CallGate>) -> eyre::Result<Self> {
        let mut query_url = Url::parse(url).map_err(|e| eyre::eyre!("metrics store url: {e}"))?;
        query_url
            .path_segments_mut()
            .map_err(|()| eyre::eyre!("metrics store url cannot be a base"))?
            .pop_if_empty()
            .extend(["api", "v1", "query"]);
        Ok(Self {
            http,
            query_url,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl MetricsStore for PromStore {
    async fn raw(&self, query: &RawQuery, at: SystemTime) -> Result<Vec<RawSeries>, StoreError> {
        let mut url = self.query_url.clone();
        url.query_pairs_mut()
            .append_pair("query", &render_promql(query))
            .append_pair("time", &unix_seconds_text(at));
        let rec = ObservationRecorder::default();
        let body = self
            .gate
            .call_with_retries(&rec, "query", || async {
                let response = self
                    .http
                    .get(url.clone())
                    .send()
                    .await
                    .map_err(|e| CallError::Transport(format!("store query: {e}")))?;
                let status = response.status();
                if let Some(pushback @ CallError::Pushback { .. }) =
                    classify_http(status, response.headers())
                {
                    return Err(pushback);
                }
                let body = read_capped(response, REPLY_CAP + 1)
                    .await
                    .map_err(|e| CallError::Transport(format!("store reply: {e}")))?;
                if body.len() > REPLY_CAP {
                    return Err(CallError::Decode(format!(
                        "store reply is over {REPLY_CAP} bytes"
                    )));
                }
                // Prometheus answers query errors as 4xx with a JSON error
                // body, which the parser reports.
                if status.is_server_error() {
                    return Err(CallError::Status(format!(
                        "store answered HTTP {}",
                        status.as_u16()
                    )));
                }
                Ok(body)
            })
            .await
            .map_err(|e| match e {
                CallError::Timeout => StoreError::Timeout,
                other => StoreError::Unavailable(other.to_string()),
            })?;
        parse_matrix(&body)
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::extract::Query;
    use axum::http::StatusCode;
    use axum::routing::get;

    use super::*;

    #[test]
    fn promql_text_exact() {
        let selector = [
            (
                "job".to_string(),
                MatchOp::Eq,
                "nico-site-health-probe-metrics".to_string(),
            ),
            (
                "namespace".to_string(),
                MatchOp::Eq,
                "nico-system".to_string(),
            ),
        ];
        let count = RawQuery {
            names: vec!["carbide_site_health_probe_units".to_string()],
            matchers: [
                ("type".to_string(), MatchOp::Eq, "host".to_string()),
                ("stage".to_string(), MatchOp::Eq, "ready".to_string()),
            ]
            .into_iter()
            .chain(selector.clone())
            .collect(),
            range: Duration::from_secs(3600),
        };
        assert_eq!(
            render_promql(&count),
            r#"carbide_site_health_probe_units{type="host",stage="ready",job="nico-site-health-probe-metrics",namespace="nico-system"}[3600s]"#
        );
        let times = RawQuery {
            names: [
                "carbide_site_health_probe_sweep_start_timestamp_seconds",
                "carbide_site_health_probe_sweep_complete_timestamp_seconds",
                "carbide_site_health_probe_sweeps_total",
                "carbide_site_health_probe_process_start_timestamp_seconds",
            ]
            .map(str::to_string)
            .to_vec(),
            matchers: [("family".to_string(), MatchOp::Re, "host|".to_string())]
                .into_iter()
                .chain(selector)
                .collect(),
            range: Duration::from_secs(900),
        };
        assert_eq!(
            render_promql(&times),
            r#"{__name__=~"carbide_site_health_probe_(sweep_start_timestamp_seconds|sweep_complete_timestamp_seconds|sweeps_total|process_start_timestamp_seconds)",family=~"host|",job="nico-site-health-probe-metrics",namespace="nico-system"}[900s]"#
        );
        let fractional = RawQuery {
            names: vec!["carbide_site_health_probe_units".to_string()],
            matchers: vec![],
            range: Duration::from_millis(2_500),
        };
        assert_eq!(
            render_promql(&fractional),
            "carbide_site_health_probe_units{}[2500ms]",
            "never rounded down to [2s]"
        );
        let sub_ms = RawQuery {
            range: Duration::from_micros(2_000_001),
            ..fractional
        };
        assert_eq!(
            render_promql(&sub_ms),
            "carbide_site_health_probe_units{}[2001ms]",
            "rounded outward"
        );
        let quoted = RawQuery {
            names: vec!["carbide_site_health_probe_hosts".to_string()],
            matchers: vec![("model".to_string(), MatchOp::Eq, r#"a"b\c"#.to_string())],
            range: Duration::from_secs(60),
        };
        assert_eq!(
            render_promql(&quoted),
            r#"carbide_site_health_probe_hosts{model="a\"b\\c"}[60s]"#
        );
    }

    #[test]
    fn prom_store_parses_matrix_json() {
        let body = serde_json::json!({
            "status": "success",
            "data": {
                "resultType": "matrix",
                "result": [{
                    "metric": {"__name__": "carbide_site_health_probe_units", "type": "host"},
                    "values": [[1761213600.0, "12"], [1761213605.5, "13"]],
                }],
            },
        });
        let series = parse_matrix(body.to_string().as_bytes()).expect("parses");
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].labels["type"], "host");
        assert_eq!(
            series[0].samples,
            vec![(1_761_213_600_000, 12.0), (1_761_213_605_500, 13.0)]
        );
        for bad in ["NaN", "+Inf", "-Inf", "twelve"] {
            let body = serde_json::json!({
                "status": "success",
                "data": {"resultType": "matrix", "result": [{
                    "metric": {"__name__": "carbide_site_health_probe_units"},
                    "values": [[1761213600.0, bad]],
                }]},
            });
            assert!(
                matches!(
                    parse_matrix(body.to_string().as_bytes()),
                    Err(StoreError::Unavailable(_))
                ),
                "{bad} is rejected"
            );
        }
        let vector = serde_json::json!({"status": "success", "data": {"resultType": "vector", "result": []}});
        assert!(parse_matrix(vector.to_string().as_bytes()).is_err());
    }

    async fn serve(status: StatusCode, body: serde_json::Value) -> String {
        let app = Router::new().route(
            "/prom/api/v1/query",
            get(move |Query(params): Query<BTreeMap<String, String>>| {
                let body = body.clone();
                async move {
                    assert!(params.contains_key("query") && params.contains_key("time"));
                    (status, axum::Json(body))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}/prom")
    }

    fn store(url: &str) -> PromStore {
        PromStore::new(
            reqwest::Client::new(),
            url,
            Arc::new(CallGate::new(
                "metrics-store",
                100.0,
                Duration::from_secs(5),
                Duration::from_secs(1),
            )),
        )
        .expect("store")
    }

    fn query() -> RawQuery {
        RawQuery {
            names: vec!["carbide_site_health_probe_units".to_string()],
            matchers: vec![],
            range: Duration::from_secs(60),
        }
    }

    #[tokio::test]
    async fn prom_store_maps_error_status() {
        let bad =
            serde_json::json!({"status": "error", "errorType": "bad_data", "error": "parse error"});
        let url = serve(StatusCode::BAD_REQUEST, bad).await;
        let err = store(&url)
            .raw(&query(), SystemTime::now())
            .await
            .expect_err("a query error");
        assert!(
            matches!(&err, StoreError::Unavailable(m) if m.contains("parse error")),
            "{err:?}"
        );

        let url = serve(StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({})).await;
        let err = store(&url)
            .raw(&query(), SystemTime::now())
            .await
            .expect_err("a server error");
        assert!(
            matches!(&err, StoreError::Unavailable(m) if m.contains("500")),
            "{err:?}"
        );

        let ok = serde_json::json!({"status": "success", "data": {"resultType": "matrix", "result": []}});
        let url = serve(StatusCode::OK, ok).await;
        assert_eq!(
            store(&url).raw(&query(), SystemTime::now()).await,
            Ok(vec![])
        );
    }
}
