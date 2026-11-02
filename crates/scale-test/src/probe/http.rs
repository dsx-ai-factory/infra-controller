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

//! [`Probe`] over HTTP.

use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use url::Url;

use super::wire::{ConfigResponse, CountResponse, Filter, UnitsQuery, UnitsResponse};
use super::{ConfigFilter, MAX_RETRY_AFTER, Page, Probe, ProbeError, to_query};

/// Most bytes read from a 2xx reply. A full page of units is far below it.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Most bytes read from an error reply. A probe error is one small object.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// The probe's `/v1` API at `base`, e.g. `http://nico-site-health-probe-api:9010`.
///
/// Makes one HTTP attempt per call. Callers own polling and retries. A
/// `Retry-After` of a 429 or 503 is kept on the error, capped at
/// [`MAX_RETRY_AFTER`]. A reply body is read up to a cap, and no error
/// carries the URL.
pub struct HttpProbe {
    client: reqwest::Client,
    base: Url,
    read_timeout: Duration,
}

impl HttpProbe {
    /// `read_timeout` bounds every read. A timeout cancels the request.
    pub fn new(base: Url, read_timeout: Duration) -> Result<Self, ProbeError> {
        // The probe is a fixed in-cluster Service: no redirect, and no proxy
        // from the environment.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| ProbeError::local_invalid("build HTTP client failed".to_string()))?;
        Ok(Self {
            client,
            base,
            read_timeout,
        })
    }

    fn url(&self, path: &str, query: Vec<(String, String)>) -> Result<Url, ProbeError> {
        let mut url = self
            .base
            .join(path)
            .map_err(|_| ProbeError::local_invalid(format!("probe URL cannot take path {path}")))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: Vec<(String, String)>,
        timeout: Duration,
    ) -> Result<T, ProbeError> {
        let url = self.url(path, query)?;
        self.send(self.client.get(url), timeout).await
    }

    async fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        timeout: Duration,
    ) -> Result<T, ProbeError> {
        let mut reply = request.timeout(timeout).send().await.map_err(transport)?;
        let status = reply.status();
        let retry_after = reply
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        let ok = status.is_success();
        let cap = if ok {
            MAX_BODY_BYTES
        } else {
            MAX_ERROR_BODY_BYTES
        };
        let too_big = || ProbeError::Decode(format!("reply body is over {cap} bytes"));
        if ok && reply.content_length().is_some_and(|n| n > cap as u64) {
            return Err(too_big());
        }
        let mut body = Vec::new();
        loop {
            let chunk = match reply.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(e) if ok => return Err(transport(e)),
                // The headers already said what a refusal is. A body that
                // stalls or resets is not read, not even its start: a
                // detail is trusted only from a body that ended within the
                // cap.
                Err(_) => {
                    body.clear();
                    break;
                }
            };
            if chunk.len() > cap - body.len() {
                if ok {
                    return Err(too_big());
                }
                body.clear();
                break;
            }
            body.extend_from_slice(&chunk);
        }
        if !ok {
            // Strict UTF-8: a repaired byte must not turn a body into a
            // detail that is trusted. Bytes that are not text are no body.
            let body = std::str::from_utf8(&body).unwrap_or_default();
            return Err(ProbeError::from_reply(status.as_u16(), retry_after, body));
        }
        serde_json::from_slice(&body).map_err(|e| {
            // The kind and place only: serde's text can quote the body.
            ProbeError::Decode(format!(
                "{:?} error at line {} column {}",
                e.classify(),
                e.line(),
                e.column()
            ))
        })
    }
}

/// `Retry-After` in whole seconds, at least 1 s so a poller never spins, and
/// capped. The date form is not used by the probe.
fn parse_retry_after(value: &str) -> Option<Duration> {
    let seconds: u64 = value.trim().parse().ok()?;
    Some(Duration::from_secs(seconds.max(1)).min(MAX_RETRY_AFTER))
}

/// Only the kind of failure, never reqwest's text: it holds the URL with its
/// query and any userinfo. A request that cannot be built is this client's
/// own fault and is never retried.
fn transport(e: reqwest::Error) -> ProbeError {
    if e.is_timeout() {
        return ProbeError::Timeout;
    }
    if e.is_builder() {
        return ProbeError::local_invalid("request could not be built".to_string());
    }
    let kind = if e.is_connect() {
        "connection failed"
    } else if e.is_redirect() {
        "unexpected redirect"
    } else if e.is_body() || e.is_decode() {
        "reply body failed"
    } else {
        "request failed"
    };
    ProbeError::Transport(kind.to_string())
}

#[async_trait]
impl Probe for HttpProbe {
    async fn count(&self, filter: &Filter) -> Result<CountResponse, ProbeError> {
        self.get("/v1/count", to_query(filter)?, self.read_timeout)
            .await
    }

    async fn units(
        &self,
        filter: &Filter,
        name: Option<&str>,
        page: &Page,
    ) -> Result<UnitsResponse, ProbeError> {
        let mut query = to_query(filter)?;
        if let Some(name) = name {
            query.push(("name".to_string(), name.to_string()));
        }
        query.extend(to_query(page)?);
        self.get("/v1/units", query, self.read_timeout).await
    }

    async fn units_query(&self, query: &UnitsQuery) -> Result<UnitsResponse, ProbeError> {
        let url = self.url("/v1/units/query", Vec::new())?;
        self.send(self.client.post(url).json(query), self.read_timeout)
            .await
    }

    async fn config(&self, filter: &ConfigFilter) -> Result<ConfigResponse, ProbeError> {
        self.get("/v1/config", to_query(filter)?, self.read_timeout)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use chrono::{DateTime, TimeDelta, Utc};

    use super::super::fake::{Endpoint, FakeProbe, FakeSweep, serve, unit};
    use super::super::wire::{
        Component, ConfigNicoApi, ErrorCode, ErrorDetail, Family, FirmwareTarget, ReadInfo,
        RuntimeSettings, Stage, UnitType,
    };
    use super::super::{MAX_PAGE, filter_key, filter_matches};
    use super::*;
    use crate::clock::FixedClock;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-23T10:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn ready() -> Filter {
        Filter {
            stage: Some(Stage::Ready),
            ..Filter::new(UnitType::Host)
        }
    }

    fn page(limit: u32, cursor: Option<String>) -> Page {
        Page {
            limit: Some(limit),
            cursor,
        }
    }

    /// A client of the probe at `addr`.
    fn client(addr: SocketAddr, read_timeout: Duration) -> HttpProbe {
        let base = Url::parse(&format!("http://{addr}")).unwrap();
        HttpProbe::new(base, read_timeout).unwrap()
    }

    /// No timeout is under test with this one: it never cuts a reply short,
    /// however loaded the machine is.
    const PATIENT: Duration = Duration::from_secs(600);

    /// A fake with one host sweep and config, served over HTTP, and a patient
    /// client of it.
    async fn served() -> (Arc<FakeProbe>, HttpProbe, SocketAddr) {
        let fake = Arc::new(FakeProbe::new(Arc::new(FixedClock::new(now()))));
        let units: Vec<_> = (0..4)
            .map(|i| unit(UnitType::Host, &format!("h{i}"), i < 3))
            .collect();
        let ready_count = units.iter().filter(|u| filter_matches(&ready(), u)).count() as u64;
        fake.push_sweep(FakeSweep {
            family: Family::Host,
            sweep_start: now() - TimeDelta::seconds(25),
            read_at: now() - TimeDelta::seconds(5),
            interval: Duration::from_secs(20),
            complete: true,
            counts: BTreeMap::from([(filter_key(&ready()), ready_count)]),
            units,
        });
        let read = ReadInfo {
            read_start: now() - TimeDelta::seconds(2),
            read_at: now() - TimeDelta::seconds(1),
            read_age_seconds: 1.0,
            read_interval_seconds: 60.0,
            last_attempt_error: None,
        };
        let target = |model: &str| FirmwareTarget {
            vendor: "Dell".to_string(),
            model: model.to_string(),
            component: Component::Bmc,
            version: "7.10".to_string(),
            explicit_start: false,
        };
        fake.set_config(ConfigResponse {
            nico_api: Some(ConfigNicoApi {
                read,
                firmware_targets: vec![target("R750"), target("R760")],
                runtime: RuntimeSettings {
                    max_concurrent_machine_updates_absolute: Some(10),
                    max_concurrent_machine_updates_percent: None,
                    auto_host_firmware_update: Some(false),
                },
                site_explorer: None,
            }),
            rest: None,
            rest_site: None,
        });
        let (addr, _stop) = serve(fake.clone()).await;
        (fake, client(addr, PATIENT), addr)
    }

    #[tokio::test]
    async fn http_probe_round_trips_each_endpoint() {
        let (fake, http, _) = served().await;

        let count = http.count(&ready()).await.unwrap();
        assert_eq!(count, fake.count(&ready()).await.unwrap());
        assert_eq!(count.count, 3);

        let not_done = Filter {
            done: Some(false),
            ..Filter::new(UnitType::Host)
        };
        let one = page(1, None);
        let units = http.units(&not_done, None, &one).await.unwrap();
        assert_eq!(units, fake.units(&not_done, None, &one).await.unwrap());
        assert_eq!(units.units[0].id, "h3");

        let all = Filter::new(UnitType::Host);
        let first = http.units(&all, None, &page(3, None)).await.unwrap();
        let cursor = first.next_cursor.clone();
        let rest = http.units(&all, None, &page(3, cursor)).await.unwrap();
        assert_eq!((first.units.len(), rest.units.len()), (3, 1));
        assert_eq!(rest.next_cursor, None);

        let query = UnitsQuery {
            filter: Filter {
                done: Some(true),
                ..Filter::new(UnitType::Host)
            },
            name: None,
            ids: vec!["h0".to_string(), "h3".to_string(), "gone".to_string()],
            limit: None,
            cursor: None,
        };
        let queried = http.units_query(&query).await.unwrap();
        assert_eq!(queried, fake.units_query(&query).await.unwrap());
        assert_eq!(queried.units.len(), 1);
        assert_eq!(queried.missing_ids, ["gone"]);

        let filter = ConfigFilter {
            model: Some("R760".to_string()),
            component: Some(Component::Bmc),
            ..ConfigFilter::default()
        };
        let config = http.config(&filter).await.unwrap();
        assert_eq!(config, fake.config(&filter).await.unwrap());
        assert_eq!(config.nico_api.unwrap().firmware_targets.len(), 1);

        // The server decoded the same filters the client sent.
        let keys: Vec<(Endpoint, String)> = fake.calls();
        assert!(keys.contains(&(Endpoint::Count, filter_key(&ready()))));
        assert!(keys.contains(&(Endpoint::Units, filter_key(&not_done))));
        assert!(keys.contains(&(Endpoint::UnitsQuery, filter_key(&query.filter))));
    }

    fn detail(code: ErrorCode, field: Option<&str>) -> ErrorDetail {
        ErrorDetail {
            code,
            message: super::super::local_message(code).to_string(),
            field: field.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn http_probe_maps_every_error_code() {
        let (fake, http, addr) = served().await;
        let cases = [
            ProbeError::InvalidParam(detail(ErrorCode::InvalidParam, Some("stage"))),
            ProbeError::SnapshotExpired,
            ProbeError::Unsupported(detail(ErrorCode::FieldUnavailable, Some("allocatable"))),
            ProbeError::Unsupported(detail(ErrorCode::UnsupportedFilter, None)),
            ProbeError::Busy { retry_after: None },
            ProbeError::Busy {
                retry_after: Some(Duration::from_secs(5)),
            },
            ProbeError::NoCompleteSweep { retry_after: None },
            ProbeError::NoCompleteSweep {
                retry_after: Some(Duration::from_secs(10)),
            },
            ProbeError::Status {
                status: 500,
                code: None,
            },
            ProbeError::Status {
                status: 502,
                code: Some(ErrorCode::Internal),
            },
            // The fake sends the body "not json".
            ProbeError::Decode("Syntax error at line 1 column 2".to_string()),
        ];
        for injected in cases {
            fake.fail_next(Endpoint::Count, injected.clone(), 1);
            let got = http.count(&ready()).await.unwrap_err();
            assert_eq!(format!("{got:?}"), format!("{injected:?}"));
        }
        assert!(http.count(&ready()).await.is_ok(), "failures were used up");

        // A held read times out at the read timeout and cancels.
        let timed = client(addr, Duration::from_millis(200));
        fake.hold(Endpoint::Config);
        assert!(matches!(
            timed.config(&ConfigFilter::default()).await,
            Err(ProbeError::Timeout)
        ));
        fake.release(Endpoint::Config);

        // Port 0 never accepts a connection: no race with another listener.
        let gone = client(SocketAddr::from(([127, 0, 0, 1], 0)), PATIENT);
        assert!(matches!(
            gone.count(&ready()).await,
            Err(ProbeError::Transport(_))
        ));
    }

    #[tokio::test]
    async fn bytes_that_are_not_utf8_are_never_repaired() {
        // A whole 422 field_unavailable whose message holds the byte 0xFF.
        let error = serve_raw(|| {
            let mut body = br#"{"error":{"code":"field_unavailable","message":"b"#.to_vec();
            body.push(0xFF);
            body.extend_from_slice(br#"d","field":"f"}}"#);
            (StatusCode::UNPROCESSABLE_ENTITY, body).into_response()
        })
        .await;
        match client(error, PATIENT).count(&ready()).await {
            Err(ProbeError::Unsupported(d)) => {
                assert_eq!(d.code, ErrorCode::UnsupportedFilter);
                assert_eq!(d.field, None);
                assert!(!d.message.contains('\u{FFFD}'), "{}", d.message);
            }
            other => panic!("{other:?}"),
        }

        // A 200 whose opaque ID would read as another ID once repaired.
        let reply = serve_raw(|| {
            let mut body = COUNT_JSON.replace("\"s1\"", "\"sXX\"").into_bytes();
            let at = body.windows(3).position(|w| w == b"sXX").unwrap();
            body[at + 1] = 0xFF;
            (StatusCode::OK, body).into_response()
        })
        .await;
        match client(reply, PATIENT).count(&ready()).await {
            Err(ProbeError::Decode(message)) => {
                assert!(!message.contains('\u{FFFD}'), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_field_that_is_unavailable_recovers() {
        let (fake, http, _) = served().await;
        let gap = detail(ErrorCode::FieldUnavailable, Some("allocatable"));
        fake.fail_next(Endpoint::Count, ProbeError::Unsupported(gap), 1);
        let err = http.count(&ready()).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert_eq!(http.count(&ready()).await.unwrap().count, 3, "next sweep");

        let never = detail(ErrorCode::UnsupportedFilter, None);
        fake.fail_next(Endpoint::Count, ProbeError::Unsupported(never), 1);
        let err = http.count(&ready()).await.unwrap_err();
        assert!(!err.is_transient(), "{err:?}");
    }

    #[tokio::test]
    async fn retry_after_is_capped() {
        let (fake, http, _) = served().await;
        let long = Duration::from_secs(3600);
        fake.fail_next(
            Endpoint::Count,
            ProbeError::NoCompleteSweep {
                retry_after: Some(long),
            },
            1,
        );
        let got = http.count(&ready()).await.unwrap_err();
        assert_eq!(got.retry_after(), Some(MAX_RETRY_AFTER));
        assert_eq!(parse_retry_after("12"), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(1)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
    }

    /// Serves `reply` for every request on `127.0.0.1:0`.
    async fn serve_raw(reply: fn() -> axum::response::Response) -> SocketAddr {
        let router = axum::Router::new().fallback(move || async move { reply() });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        addr
    }

    type Chunk = Result<axum::body::Bytes, std::io::Error>;

    fn chunk(bytes: Vec<u8>) -> Chunk {
        Ok(axum::body::Bytes::from(bytes))
    }

    fn streamed(
        status: StatusCode,
        chunks: impl futures::Stream<Item = Chunk> + Send + 'static,
    ) -> axum::response::Response {
        (status, axum::body::Body::from_stream(chunks)).into_response()
    }

    /// A reply of `status` and valid JSON `head`, padded with spaces to
    /// `total` bytes, sent in 1 MiB chunks so no Content-Length announces it.
    fn padded_chunked(status: StatusCode, head: &str, total: usize) -> axum::response::Response {
        let mut first = head.as_bytes().to_vec();
        first.resize(1 << 20, b' ');
        let rest = (total / (1 << 20)).saturating_sub(1);
        let chunks =
            std::iter::once(chunk(first)).chain((0..rest).map(|_| chunk(vec![b' '; 1 << 20])));
        streamed(status, futures::stream::iter(chunks))
    }

    /// The count of `ready()`, as the probe sends it.
    const COUNT_JSON: &str = r#"{"count":3,"snapshotId":"s1","family":"host","sweepStart":"2026-10-23T09:59:35Z","readAt":"2026-10-23T09:59:55Z","sweepAgeSeconds":5.0,"sweepIntervalSeconds":20.0}"#;

    #[tokio::test]
    async fn reply_bodies_are_capped() {
        let over = MAX_BODY_BYTES + (1 << 20);
        // Valid JSON within the cap is read, so the padded replies below
        // fail on the cap alone.
        let within = serve_raw(|| (StatusCode::OK, COUNT_JSON).into_response()).await;
        let got = client(within, PATIENT).count(&ready()).await;
        assert_eq!(got.unwrap().count, 3);

        // Announced by Content-Length, then the body stalls after its first
        // bytes. Only a refusal on the header returns: reading the body would
        // wait on the stall.
        let announced = serve_raw(|| {
            let start = futures::stream::iter([chunk(COUNT_JSON.as_bytes().to_vec())]);
            let stall = futures::stream::pending::<Chunk>();
            let mut reply = streamed(StatusCode::OK, futures::StreamExt::chain(start, stall));
            reply.headers_mut().insert(
                axum::http::header::CONTENT_LENGTH,
                axum::http::HeaderValue::from(MAX_BODY_BYTES + (1 << 20)),
            );
            reply
        })
        .await;
        // Not announced.
        let unannounced =
            serve_raw(|| padded_chunked(StatusCode::OK, COUNT_JSON, MAX_BODY_BYTES + (1 << 20)))
                .await;
        for (scenario, addr) in [("content-length", announced), ("chunked", unannounced)] {
            let (http, filter) = (client(addr, PATIENT), ready());
            let got = tokio::time::timeout(Duration::from_secs(20), http.count(&filter)).await;
            match got.unwrap_or_else(|_| panic!("{scenario}: no refusal")) {
                Err(ProbeError::Decode(message)) => {
                    assert!(message.contains("over"), "{scenario}: {message}");
                    assert!(!message.contains(&over.to_string()), "{scenario}");
                }
                other => panic!("{scenario}: {other:?}"),
            }
        }

        // An error body that never ends: read up to its cap, then mapped
        // by its code. Without the cap the call hangs.
        let endless = serve_raw(|| {
            let head = r#"{"error":{"code":"field_unavailable","message":"m","field":"f"}}"#;
            let mut first = head.as_bytes().to_vec();
            first.resize(1 << 16, b' ');
            let rest = futures::stream::repeat_with(|| chunk(vec![b' '; 1 << 16]));
            streamed(
                StatusCode::UNPROCESSABLE_ENTITY,
                futures::StreamExt::chain(futures::stream::iter([chunk(first)]), rest),
            )
        })
        .await;
        let (http, filter) = (client(endless, PATIENT), ready());
        let call = http.count(&filter);
        match tokio::time::timeout(Duration::from_secs(20), call).await {
            // Cut at the cap: its detail is not trusted, so the filter is
            // unsupported, not a gap that may recover.
            Ok(Err(ProbeError::Unsupported(d))) => assert_eq!(d.code, ErrorCode::UnsupportedFilter),
            other => panic!("{other:?}"),
        }

        // One byte over the cap is cut. Exactly the cap is read whole.
        let exact = serve_raw(|| sized_gap(MAX_ERROR_BODY_BYTES)).await;
        let over_by_one = serve_raw(|| sized_gap(MAX_ERROR_BODY_BYTES + 1)).await;
        for (scenario, addr, want) in [
            ("the cap", exact, ErrorCode::FieldUnavailable),
            (
                "the cap plus one",
                over_by_one,
                ErrorCode::UnsupportedFilter,
            ),
        ] {
            match client(addr, PATIENT).count(&ready()).await {
                Err(ProbeError::Unsupported(d)) => assert_eq!(d.code, want, "{scenario}"),
                other => panic!("{scenario}: {other:?}"),
            }
        }
    }

    /// A 422 `field_unavailable` body of exactly `len` bytes.
    fn sized_gap(len: usize) -> axum::response::Response {
        let head = r#"{"error":{"code":"field_unavailable","message":"m","field":"f"}}"#;
        let mut body = head.as_bytes().to_vec();
        body.resize(len, b' ');
        (StatusCode::UNPROCESSABLE_ENTITY, body).into_response()
    }

    #[tokio::test]
    async fn a_refusal_survives_its_body_stalling_or_resetting() {
        // Real headers and the start of a body, then nothing, or a reset.
        fn broken(status: StatusCode, reset: bool) -> axum::response::Response {
            broken_after(status, br#"{"err"#, reset)
        }
        fn broken_after(
            status: StatusCode,
            sent: &'static [u8],
            reset: bool,
        ) -> axum::response::Response {
            let start = futures::stream::iter([chunk(sent.to_vec())]);
            if reset {
                // Late, so the headers and the first bytes are on the wire.
                let end = futures::stream::once(async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Err(std::io::Error::other("reset"))
                });
                streamed(status, futures::StreamExt::chain(start, end))
            } else {
                let end = futures::stream::pending::<Chunk>();
                streamed(status, futures::StreamExt::chain(start, end))
            }
        }
        let short = Duration::from_millis(300);
        let mut rows: Vec<(&str, SocketAddr, &str)> = vec![
            (
                "403 stalled",
                serve_raw(|| broken(StatusCode::FORBIDDEN, false)).await,
                "Status { status: 403, code: None }",
            ),
            (
                "403 reset",
                serve_raw(|| broken(StatusCode::FORBIDDEN, true)).await,
                "Status { status: 403, code: None }",
            ),
            (
                "409 reset",
                serve_raw(|| broken(StatusCode::CONFLICT, true)).await,
                "SnapshotExpired",
            ),
            (
                "503 stalled",
                serve_raw(|| broken(StatusCode::SERVICE_UNAVAILABLE, false)).await,
                "NoCompleteSweep { retry_after: None }",
            ),
        ];
        for (scenario, addr, want) in rows.drain(..) {
            let got = client(addr, short).count(&ready()).await.unwrap_err();
            assert_eq!(format!("{got:?}"), want, "{scenario}");
        }
        // A 422 whose body did not end is never field_unavailable, even when
        // what came before the stall or reset is a whole valid error.
        const GAP: &[u8] = br#"{"error":{"code":"field_unavailable","message":"m","field":"f"}}"#;
        let incomplete = [
            (
                "partial, stalled",
                serve_raw(|| broken(StatusCode::UNPROCESSABLE_ENTITY, false)).await,
            ),
            (
                "partial, reset",
                serve_raw(|| broken(StatusCode::UNPROCESSABLE_ENTITY, true)).await,
            ),
            (
                "valid error, stalled",
                serve_raw(|| broken_after(StatusCode::UNPROCESSABLE_ENTITY, GAP, false)).await,
            ),
            (
                "valid error, reset",
                serve_raw(|| broken_after(StatusCode::UNPROCESSABLE_ENTITY, GAP, true)).await,
            ),
        ];
        for (scenario, addr) in incomplete {
            match client(addr, short).count(&ready()).await {
                Err(ProbeError::Unsupported(d)) => {
                    assert_eq!(d.code, ErrorCode::UnsupportedFilter, "{scenario}");
                    assert_eq!(d.field, None, "{scenario}");
                }
                other => panic!("{scenario}: {other:?}"),
            }
        }
        let got = client(
            serve_raw(|| broken(StatusCode::FORBIDDEN, false)).await,
            short,
        )
        .count(&ready())
        .await
        .unwrap_err();
        assert!(got.is_unknown() && !got.is_transient(), "403 is final");

        // A 2xx whose body stalls or resets is still a failed read.
        let stalled = serve_raw(|| broken(StatusCode::OK, false)).await;
        assert!(matches!(
            client(stalled, short).count(&ready()).await,
            Err(ProbeError::Timeout)
        ));
        let reset = serve_raw(|| broken(StatusCode::OK, true)).await;
        assert!(matches!(
            client(reset, short).count(&ready()).await,
            Err(ProbeError::Transport(_))
        ));
    }

    #[tokio::test]
    async fn raw_error_bodies_leak_nothing() {
        let hostile = serve_raw(|| {
            (
                StatusCode::BAD_GATEWAY,
                "<html>http://user:hunter2@sentinel-host/v1/count</html>",
            )
                .into_response()
        })
        .await;
        let got = client(hostile, PATIENT).count(&ready()).await.unwrap_err();
        let text = format!("{got} {got:?}");
        assert!(
            !text.contains("hunter2") && !text.contains("sentinel-host"),
            "{text}"
        );
        assert!(matches!(
            got,
            ProbeError::Status {
                status: 502,
                code: None
            }
        ));

        // A 200 that is not JSON: the decode text says where, not what.
        let garbage = serve_raw(|| {
            (StatusCode::OK, "http://user:hunter2@sentinel-host nope").into_response()
        })
        .await;
        let got = client(garbage, PATIENT).count(&ready()).await.unwrap_err();
        let text = format!("{got} {got:?}");
        assert!(
            !text.contains("hunter2") && !text.contains("sentinel-host"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn local_failures_are_final() {
        // A base that takes no path, and a scheme reqwest will not send.
        for (scenario, base) in [
            ("URL join", "mailto:probe@example.com"),
            ("request build", "ftp://127.0.0.1:0/"),
        ] {
            let http = HttpProbe::new(Url::parse(base).unwrap(), PATIENT).unwrap();
            let err = http.count(&ready()).await.unwrap_err();
            assert!(
                matches!(err, ProbeError::InvalidParam(_)),
                "{scenario}: {err:?}"
            );
            assert!(!err.is_transient(), "{scenario}");
        }
    }

    #[tokio::test]
    async fn units_over_http_match_as_the_probe_lists() {
        let fake = Arc::new(FakeProbe::new(Arc::new(FixedClock::new(now()))));
        let (mismatches, hosts): (Vec<_>, Vec<_>) = super::super::tests::predicate_units()
            .into_iter()
            .partition(|u| u.unit_type == UnitType::IdentityMismatch);
        for (family, units) in [(Family::Host, hosts), (Family::Expected, mismatches)] {
            fake.push_sweep(FakeSweep {
                family,
                sweep_start: now() - TimeDelta::seconds(25),
                read_at: now() - TimeDelta::seconds(5),
                interval: Duration::from_secs(20),
                complete: true,
                counts: BTreeMap::new(),
                units,
            });
        }
        let (addr, _stop) = serve(fake).await;
        let http = client(addr, PATIENT);
        let page = page(MAX_PAGE, None);
        for (scenario, filter, want) in super::super::tests::predicate_rows() {
            let reply = http.units(&filter, None, &page).await.unwrap();
            let mut got: Vec<String> = reply.units.into_iter().map(|u| u.id).collect();
            got.sort();
            assert_eq!(got, want, "{scenario}");
        }
    }

    #[tokio::test]
    async fn transport_errors_leak_no_url() {
        // Port 0 never accepts. The URL holds a secret, a host and a query.
        let base = Url::parse("http://user:hunter2@127.0.0.1:0").unwrap();
        let http = HttpProbe::new(base, PATIENT).unwrap();
        let filter = Filter {
            state: Some("needle-state".to_string()),
            ..ready()
        };
        let calls = [
            http.count(&filter).await.unwrap_err(),
            http.units(&filter, Some("needle-name"), &Page::default())
                .await
                .unwrap_err(),
        ];
        for err in calls {
            assert!(matches!(err, ProbeError::Transport(_)), "{err:?}");
            let text = format!("{err} {err:?}");
            for secret in ["hunter2", "user", "127.0.0.1", "needle", "/v1/"] {
                assert!(!text.contains(secret), "{secret} leaked in {text}");
            }
        }
    }
}
