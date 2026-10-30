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

use reqwest::StatusCode;
use url::Url;

use crate::framework::ObservationRecorder;
use crate::gate::{CallError, CallGate, classify_http};
use crate::rest::client::read_capped;

/// A store reply bigger than this is not a few hours of probe series.
const REPLY_CAP: usize = 64 << 20;
/// The last time, in Unix seconds, a probe sample can have or show: the end
/// of year 9999.
const LAST_TIME_SECS: f64 = 253_402_300_799.0;

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
    /// A store call ran past its deadline, or the store pushed back on
    /// every retry.
    Timeout,
}

#[async_trait::async_trait]
pub(crate) trait MetricsStore: Send + Sync {
    /// The raw samples of `query` in `(at - range, at]`.
    async fn raw(&self, query: &RawQuery, at: SystemTime) -> Result<Vec<RawSeries>, StoreError>;
}

/// `value` as a PromQL string literal. A JSON string is one: PromQL takes
/// Go escapes, and JSON escapes quotes, backslashes and control characters
/// with a subset of them.
fn quoted(value: &str) -> String {
    serde_json::Value::from(value).to_string()
}

/// Whether `secs` is a Unix time a probe sample can have or show: from the
/// epoch to the end of year 9999.
pub(crate) fn is_time(secs: f64) -> bool {
    (0.0..=LAST_TIME_SECS).contains(&secs)
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
        matchers.push(format!("{label}{op}{}", quoted(value)));
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

/// The matrix of an instant query's JSON reply. Errors are fixed text, safe
/// to serve; what the reply said is logged at debug only.
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
    let unavailable = |message: &str| StoreError::Unavailable(format!("metrics store: {message}"));
    let reply: Reply = serde_json::from_slice(body).map_err(|e| {
        tracing::debug!(error = %e, "metrics store reply does not decode");
        unavailable("undecodable reply")
    })?;
    if reply.status != "success" {
        tracing::debug!(status = %reply.status, error = ?reply.error, "metrics store query failed");
        return Err(unavailable("query failed"));
    }
    let data = reply.data.ok_or_else(|| unavailable("reply has no data"))?;
    if data.result_type != "matrix" {
        tracing::debug!(result_type = %data.result_type, "metrics store reply is not a matrix");
        return Err(unavailable("reply is not a matrix"));
    }
    data.result
        .into_iter()
        .map(|series| {
            // A `_timestamp_seconds` series has Unix times for values
            // (sweep start and end, process start).
            let holds_times = series
                .metric
                .get("__name__")
                .is_some_and(|name| name.ends_with("_timestamp_seconds"));
            let samples = series
                .values
                .into_iter()
                .map(|(at, value)| {
                    if !is_time(at) {
                        tracing::debug!(at, "metrics store sample time is out of range");
                        return Err(unavailable("a sample time is out of range"));
                    }
                    // The probe publishes finite numbers only; NaN or an
                    // infinity is not its sample.
                    let value: f64 = value
                        .parse()
                        .ok()
                        .filter(|v: &f64| v.is_finite())
                        .ok_or_else(|| {
                            tracing::debug!(
                                value = %value,
                                "metrics store sample is not a finite number"
                            );
                            unavailable("a sample is not a finite number")
                        })?;
                    if holds_times && !is_time(value) {
                        tracing::debug!(value, "metrics store time sample is out of range");
                        return Err(unavailable("a time sample is out of range"));
                    }
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
        // Transport errors drop the URL: it carries the query and can carry
        // credentials.
        let body = self
            .gate
            .call_with_retries(&rec, "query", || async {
                let response = self.http.get(url.clone()).send().await.map_err(|e| {
                    CallError::Transport(format!("store query: {}", e.without_url()))
                })?;
                let status = response.status();
                match classify_http(status, response.headers()) {
                    None => {}
                    Some(pushback @ CallError::Pushback { .. }) => return Err(pushback),
                    // A gateway or the store gave up on the query in time.
                    Some(_)
                        if status == StatusCode::GATEWAY_TIMEOUT
                            || status == StatusCode::REQUEST_TIMEOUT =>
                    {
                        return Err(CallError::Timeout);
                    }
                    // Any other refusal is its status code, final, whatever
                    // its body says; the body is never read.
                    Some(refused) => return Err(refused),
                }
                let body = read_capped(response, REPLY_CAP + 1).await.map_err(|e| {
                    CallError::Transport(format!("store reply: {}", e.without_url()))
                })?;
                if body.len() > REPLY_CAP {
                    return Err(CallError::Decode(format!(
                        "store reply is over {REPLY_CAP} bytes"
                    )));
                }
                Ok(body)
            })
            .await
            .map_err(|e| match e {
                // Pushback that outlasts the retries is a store that did not
                // answer in time.
                CallError::Timeout | CallError::Pushback { .. } => StoreError::Timeout,
                other => {
                    tracing::debug!(error = %other, "metrics store query failed");
                    StoreError::Unavailable(format!("metrics store: {}", other.category()))
                }
            })?;
        parse_matrix(&body)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::atomic::{AtomicU32, Ordering};

    use axum::Router;
    use axum::extract::Query;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;

    use super::*;
    use crate::gate::MAX_RETRIES;

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

    #[test]
    fn parse_matrix_rejects_out_of_range_time_value() {
        let reply = |name: &str, value: &str| {
            serde_json::json!({
                "status": "success",
                "data": {"resultType": "matrix", "result": [{
                    "metric": {"__name__": name},
                    "values": [[1761213600.0, value]],
                }]},
            })
            .to_string()
        };
        for name in [
            "carbide_site_health_probe_sweep_complete_timestamp_seconds",
            "carbide_site_health_probe_sweep_start_timestamp_seconds",
            "carbide_site_health_probe_process_start_timestamp_seconds",
        ] {
            assert!(parse_matrix(reply(name, "1761213590.5").as_bytes()).is_ok());
            for bad in ["1e308", "-1", "1e13"] {
                assert_eq!(
                    parse_matrix(reply(name, bad).as_bytes()),
                    Err(StoreError::Unavailable(
                        "metrics store: a time sample is out of range".to_string()
                    )),
                    "{name} {bad}"
                );
            }
        }
        // A count is not a time.
        let count = reply("carbide_site_health_probe_sweeps_total", "1e13");
        assert!(parse_matrix(count.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn prom_store_maps_error_status() {
        let bad = serde_json::json!({
            "status": "error", "errorType": "bad_data", "error": "parse error SENTINEL",
        });
        let url = serve(StatusCode::BAD_REQUEST, bad).await;
        let err = store(&url)
            .raw(&query(), SystemTime::now())
            .await
            .expect_err("a query error");
        assert_eq!(
            err,
            StoreError::Unavailable("metrics store: HTTP 400".to_string()),
            "the status code, never the store's error text"
        );

        let url = serve(StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({})).await;
        let err = store(&url)
            .raw(&query(), SystemTime::now())
            .await
            .expect_err("a server error");
        assert_eq!(
            err,
            StoreError::Unavailable("metrics store: HTTP 500".to_string())
        );

        let ok = serde_json::json!({"status": "success", "data": {"resultType": "matrix", "result": []}});
        let url = serve(StatusCode::OK, ok).await;
        assert_eq!(
            store(&url).raw(&query(), SystemTime::now()).await,
            Ok(vec![])
        );
    }

    #[test]
    fn parse_matrix_errors_never_carry_reply_text() {
        let replies = [
            serde_json::json!({"status": "error", "error": "SENTINEL"}).to_string(),
            serde_json::json!({
                "status": "success", "data": {"resultType": "SENTINEL", "result": []},
            })
            .to_string(),
            serde_json::json!({"status": "success", "data": {"resultType": "matrix", "result": [{
                "metric": {}, "values": [[1761213600.0, "SENTINEL"]],
            }]}})
            .to_string(),
            "SENTINEL".to_string(),
        ];
        for reply in replies {
            let err = parse_matrix(reply.as_bytes()).expect_err("rejected");
            assert!(
                matches!(&err, StoreError::Unavailable(m) if !m.contains("SENTINEL")),
                "{reply} leaks into {err:?}"
            );
        }
    }

    #[test]
    fn promql_escapes_control_characters() {
        let query = RawQuery {
            names: vec!["carbide_site_health_probe_hosts".to_string()],
            matchers: vec![("model".to_string(), MatchOp::Eq, "a\nb\rc\td".to_string())],
            range: Duration::from_secs(60),
        };
        assert_eq!(
            render_promql(&query),
            r#"carbide_site_health_probe_hosts{model="a\nb\rc\td"}[60s]"#
        );
    }

    #[test]
    fn parse_matrix_rejects_out_of_range_sample_time() {
        for at in [-1.0, 1e13, 1e308] {
            let body = serde_json::json!({
                "status": "success",
                "data": {"resultType": "matrix", "result": [{
                    "metric": {"__name__": "carbide_site_health_probe_units"},
                    "values": [[at, "1"]],
                }]},
            });
            assert_eq!(
                parse_matrix(body.to_string().as_bytes()),
                Err(StoreError::Unavailable(
                    "metrics store: a sample time is out of range".to_string()
                )),
                "{at}"
            );
        }
    }

    /// A store that answers request `i` (from 0) with `reply(i)`, and the
    /// count of requests it got.
    async fn serve_with<F, Fut>(reply: F) -> (String, Arc<AtomicU32>)
    where
        F: Fn(u32) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let requests = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&requests);
        let app = Router::new().route(
            "/prom/api/v1/query",
            get(move || reply(seen.fetch_add(1, Ordering::SeqCst))),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}/prom"), requests)
    }

    /// A store client with a short back-off.
    fn quick_store(url: &str, call_timeout: Duration) -> PromStore {
        PromStore::new(
            reqwest::Client::new(),
            url,
            Arc::new(CallGate::new(
                "metrics-store",
                100.0,
                call_timeout,
                Duration::from_millis(10),
            )),
        )
        .expect("store")
    }

    fn empty_matrix() -> Response {
        let reply = serde_json::json!({
            "status": "success", "data": {"resultType": "matrix", "result": []},
        });
        axum::Json(reply).into_response()
    }

    #[tokio::test]
    async fn prom_store_slow_reply_is_timeout() {
        let (url, requests) = serve_with(|_| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            empty_matrix()
        })
        .await;
        let err = quick_store(&url, Duration::from_millis(50))
            .raw(&query(), SystemTime::now())
            .await
            .expect_err("too slow");
        assert_eq!(err, StoreError::Timeout);
        assert_eq!(requests.load(Ordering::SeqCst), 1 + MAX_RETRIES as u32);
    }

    #[tokio::test]
    async fn prom_store_retries_pushback() {
        let (url, requests) = serve_with(|i| async move {
            if i == 0 {
                StatusCode::TOO_MANY_REQUESTS.into_response()
            } else {
                empty_matrix()
            }
        })
        .await;
        let store = quick_store(&url, Duration::from_secs(5));
        assert_eq!(store.raw(&query(), SystemTime::now()).await, Ok(vec![]));
        assert_eq!(requests.load(Ordering::SeqCst), 2, "one retry");

        // A store that still pushes back after every retry did not answer
        // in time.
        let (url, requests) =
            serve_with(|_| async { StatusCode::SERVICE_UNAVAILABLE.into_response() }).await;
        let err = quick_store(&url, Duration::from_secs(5))
            .raw(&query(), SystemTime::now())
            .await
            .expect_err("never answers");
        assert_eq!(err, StoreError::Timeout);
        assert_eq!(requests.load(Ordering::SeqCst), 1 + MAX_RETRIES as u32);
    }

    #[tokio::test]
    async fn prom_store_gateway_timeout_is_timeout() {
        for status in [StatusCode::GATEWAY_TIMEOUT, StatusCode::REQUEST_TIMEOUT] {
            let (url, requests) = serve_with(move |_| async move { status.into_response() }).await;
            let err = quick_store(&url, Duration::from_secs(5))
                .raw(&query(), SystemTime::now())
                .await
                .expect_err("timed out");
            assert_eq!(err, StoreError::Timeout, "{status}");
            assert_eq!(requests.load(Ordering::SeqCst), 1 + MAX_RETRIES as u32);
        }
    }

    #[tokio::test]
    async fn prom_store_refusal_with_a_matrix_body_is_an_error() {
        let (url, requests) =
            serve_with(|_| async { (StatusCode::BAD_REQUEST, empty_matrix()).into_response() })
                .await;
        let err = quick_store(&url, Duration::from_secs(5))
            .raw(&query(), SystemTime::now())
            .await
            .expect_err("refused");
        assert_eq!(
            err,
            StoreError::Unavailable("metrics store: HTTP 400".to_string())
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn prom_store_refusal_with_a_slow_body_is_final() {
        // The status arrives, the promised body never does.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&requests);
        tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((mut socket, _)) = listener.accept().await {
                seen.fetch_add(1, Ordering::SeqCst);
                let head = "HTTP/1.1 403 Forbidden\r\ncontent-length: 100\r\n\r\n";
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, head.as_bytes()).await;
                open.push(socket);
            }
        });
        let err = quick_store(&format!("http://{addr}/prom"), Duration::from_millis(200))
            .raw(&query(), SystemTime::now())
            .await
            .expect_err("refused");
        assert_eq!(
            err,
            StoreError::Unavailable("metrics store: HTTP 403".to_string())
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1, "never retried");
    }

    #[tokio::test]
    async fn prom_store_keeps_status_of_non_json_refusal() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::NOT_FOUND,
            StatusCode::FOUND,
        ] {
            let (url, requests) =
                serve_with(
                    move |_| async move { (status, "<html>SENTINEL</html>").into_response() },
                )
                .await;
            let err = quick_store(&url, Duration::from_secs(5))
                .raw(&query(), SystemTime::now())
                .await
                .expect_err("refused");
            assert_eq!(
                err,
                StoreError::Unavailable(format!("metrics store: HTTP {}", status.as_u16()))
            );
            assert_eq!(requests.load(Ordering::SeqCst), 1, "a refusal is final");
        }
    }

    #[tokio::test]
    async fn prom_store_rejects_oversize_reply() {
        // A valid empty matrix, padded with whitespace to one byte over.
        let mut body =
            br#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#.to_vec();
        body.resize(REPLY_CAP + 1, b' ');
        let (url, requests) = serve_with(move |_| {
            let body = body.clone();
            async move { body.into_response() }
        })
        .await;
        let err = quick_store(&url, Duration::from_secs(5))
            .raw(&query(), SystemTime::now())
            .await
            .expect_err("too big");
        assert_eq!(
            err,
            StoreError::Unavailable("metrics store: undecodable reply".to_string())
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1, "never retried");
    }

    #[tokio::test]
    async fn prom_store_transport_error_hides_url() {
        // Nothing listens on port 1.
        let err = quick_store(
            "http://probe:SECRET@127.0.0.1:1/prom",
            Duration::from_secs(5),
        )
        .raw(&query(), SystemTime::now())
        .await
        .expect_err("refused");
        assert_eq!(
            err,
            StoreError::Unavailable("metrics store: transport error".to_string())
        );
    }
}
