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

//! Carbide DNS Server
//!
//! Listens directly on a DNS port (UDP/TCP) and resolves queries by forwarding
//! them to carbide-api via the `lookup_record` RPC.

use std::iter;
use std::sync::Arc;
use std::time::{Duration, Instant};

use carbide_instrument::{Event, LabelValue, emit};
use dns_record::DnsResourceRecordType;
use eyre::Report;
use hickory_resolver::proto::op::ResponseCode;
use hickory_resolver::proto::rr::rdata::PTR;
use hickory_resolver::proto::rr::{DNSClass, Name, RData, RecordType};
use hickory_server::net::runtime::Time;
use hickory_server::proto::op::Metadata;
use hickory_server::proto::rr::Record;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::zone_handler::MessageResponseBuilder;
use metrics_endpoint::{MetricsEndpointConfig, new_metrics_setup, run_metrics_endpoint};
use opentelemetry::StringValue;
use opentelemetry::metrics::{Counter, Meter, ObservableGauge};
use rpc::forge_tls_client::{ApiConfig, ForgeClientT, ForgeTlsClient};
use rpc::protos::dns::DnsResourceRecordLookupRequest;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Mutex;
use tracing::{Instrument, error, info, warn};

pub mod config;
mod negative_cache;

use crate::config::Config;
use crate::negative_cache::{CacheKey, NegativeCache};

struct DnsMetrics {
    negative_cache_eviction: Counter<u64>,
    // Observable gauge of current cache occupancy: its callback reads the
    // cache length on each scrape. Held only to keep that callback registered
    // for the lifetime of the meter; never accessed directly.
    _negative_cache_size: ObservableGauge<u64>,
}

impl DnsMetrics {
    fn new(meter: &Meter, negative_cache: Arc<NegativeCache>) -> Self {
        Self {
            negative_cache_eviction: meter
                .u64_counter("carbide_dns_negative_cache_eviction_count")
                .build(),
            _negative_cache_size: meter
                .u64_observable_gauge("carbide_dns_negative_cache_size")
                .with_description("Number of entries in the negative DNS cache")
                .with_callback(move |observer| {
                    observer.observe(negative_cache.entry_count() as u64, &[]);
                })
                .build(),
        }
    }
}

// DnsMetrics contains OpenTelemetry instrument types which don't implement Debug.
impl std::fmt::Debug for DnsMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DnsMetrics").finish()
    }
}

#[derive(Debug)]
pub struct DnsServer {
    forge_client: Mutex<ForgeClientT>,
    negative_cache: Arc<NegativeCache>,
    /// Per-request ceiling on the upstream `lookup_record` call.
    upstream_lookup_timeout: Duration,
    metrics: DnsMetrics,
}

/// How an upstream gRPC failure is surfaced to the DNS client.
struct NegativeClassification {
    /// The DNS response code returned to the client.
    code: ResponseCode,
    /// Whether the failure is transient (a momentary upstream problem, cached
    /// only briefly) rather than a stable/authoritative negative cached for the
    /// full TTL.
    transient: bool,
}

/// Maps an upstream gRPC status to the DNS response code we return and how long
/// it may be cached, following the closest RFC 1035 RCODE semantics.
fn classify_failure(code: tonic::Code) -> NegativeClassification {
    use tonic::Code;

    let (code, transient) = match code {
        // The name genuinely does not exist: a stable, authoritative negative.
        Code::NotFound => (ResponseCode::NXDomain, false),
        // The query itself was malformed (empty qname, unparseable qtype). It
        // will stay malformed on retry, so the answer is stable.
        Code::InvalidArgument => (ResponseCode::FormErr, false),
        // The upstream does not implement this qtype/operation; stable, and
        // consistent with the NotImp we already return for unsupported qtypes.
        Code::Unimplemented => (ResponseCode::NotImp, false),
        // An authorization/policy rejection — surfaced as a policy refusal.
        Code::PermissionDenied | Code::Unauthenticated => (ResponseCode::Refused, false),
        // Everything else —
        // Surface it as ServFail and cache it only briefly: RFC 9520 requires
        // caching resolution failures so a client retry storm collapses into one
        // upstream call per name per window, while the short TTL keeps the
        // failure from outliving the upstream's recovery.
        _ => (ResponseCode::ServFail, true),
    };

    NegativeClassification { code, transient }
}

/// Builds the hickory `RData` for a supported record type from the API's string
/// `content`, logging and dropping the record when the content does not parse.
/// `handle_request` only dispatches the supported types here, so any other qtype
/// yields `None`.
fn content_to_rdata(qtype: DnsResourceRecordType, content: &str) -> Option<RData> {
    match qtype {
        DnsResourceRecordType::A => match content.parse::<std::net::Ipv4Addr>() {
            Ok(ip) => Some(RData::A(ip.into())),
            Err(e) => {
                warn!(%content, error = %e, "Failed to parse IPv4 address");
                None
            }
        },
        DnsResourceRecordType::AAAA => match content.parse::<std::net::Ipv6Addr>() {
            Ok(ip) => Some(RData::AAAA(ip.into())),
            Err(e) => {
                warn!(%content, error = %e, "Failed to parse IPv6 address");
                None
            }
        },
        // The content is the target FQDN; PTR is a name, unlike the address-valued
        // A/AAAA records.
        DnsResourceRecordType::PTR => match content.parse::<Name>() {
            Ok(name) => Some(RData::PTR(PTR(name))),
            Err(e) => {
                warn!(%content, error = %e, "Failed to parse PTR target name");
                None
            }
        },
        _ => None,
    }
}

/// The query-type metric label, bounded by construction: the types the server
/// resolves each get their own value, and everything else — the types answered
/// with NotImp, and requests whose question section the handler cannot read
/// (zero or multiple questions) — collapses into `Other`. Wire-level garbage
/// never reaches the handler, so it is out of scope for these counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum Qtype {
    A,
    Aaaa,
    Ptr,
    Other,
}

impl From<RecordType> for Qtype {
    fn from(qtype: RecordType) -> Self {
        match qtype {
            RecordType::A => Qtype::A,
            RecordType::AAAA => Qtype::Aaaa,
            RecordType::PTR => Qtype::Ptr,
            _ => Qtype::Other,
        }
    }
}

/// The response-code metric label, bounded by construction: the codes the
/// server returns — NoError plus `classify_failure`'s negative set — each get
/// their own value, and any other RFC code collapses into `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum Rcode {
    NoError,
    FormErr,
    NxDomain,
    ServFail,
    Refused,
    NotImp,
    Other,
}

impl From<ResponseCode> for Rcode {
    fn from(code: ResponseCode) -> Self {
        match code {
            ResponseCode::NoError => Rcode::NoError,
            ResponseCode::FormErr => Rcode::FormErr,
            ResponseCode::NXDomain => Rcode::NxDomain,
            ResponseCode::ServFail => Rcode::ServFail,
            ResponseCode::Refused => Rcode::Refused,
            ResponseCode::NotImp => Rcode::NotImp,
            _ => Rcode::Other,
        }
    }
}

/// `NegativeCacheResponseCode` preserves Hickory's `Debug` spelling for the
/// public `response_code` label on negative-cache hit and miss counters.
///
/// A derived `LabelValue` would turn values such as `NXDomain` and `ServFail`
/// into snake_case. The manual implementation remains bounded because these
/// Events only receive `classify_failure` results or cache entries populated
/// from those same results.
#[derive(Debug, Clone, Copy)]
struct NegativeCacheResponseCode(ResponseCode);

impl LabelValue for NegativeCacheResponseCode {
    fn label_value(&self) -> StringValue {
        StringValue::from(match self.0 {
            ResponseCode::FormErr => "FormErr",
            ResponseCode::NXDomain => "NXDomain",
            ResponseCode::ServFail => "ServFail",
            ResponseCode::NotImp => "NotImp",
            ResponseCode::Refused => "Refused",
            code => return StringValue::from(format!("{code:?}")),
        })
    }
}

/// `DnsNegativeCacheHit` records a cached response returned without an
/// upstream lookup, keeping its DEBUG diagnostic and hit counter together.
#[derive(Event)]
#[event(
    event_name = "dns_negative_cache_hit",
    metric_name = "carbide_dns_negative_cache_hit_count_total",
    component = "carbide-dns",
    log = debug,
    metric = counter,
    message = "Negative cache hit",
    describe = "Number of negative DNS cache hits, by response code"
)]
struct DnsNegativeCacheHit {
    #[label]
    response_code: NegativeCacheResponseCode,
}

/// `DnsNegativeCacheMiss` records an upstream failure before it enters the
/// negative cache. Its error remains log-only context while the bounded
/// response code labels both the WARN diagnostic and counter.
#[derive(Event)]
#[event(
    event_name = "dns_negative_cache_miss",
    metric_name = "carbide_dns_negative_cache_miss_count_total",
    component = "carbide-dns",
    log = warn,
    metric = counter,
    message = "DNS lookup failed",
    describe = "Number of negative DNS cache misses, by response code"
)]
struct DnsNegativeCacheMiss {
    #[label]
    response_code: NegativeCacheResponseCode,
    #[context]
    error: String,
}

/// A query arrived, counted by query type: the request rate is the signal, so
/// no per-query log line is built.
#[derive(Event)]
#[event(
    event_name = "dns_query_received",
    metric_name = "carbide_dns_queries_total",
    component = "carbide-dns",
    log = off,
    metric = counter,
    describe = "Number of DNS queries received, by query type"
)]
struct DnsQueryReceived {
    #[label]
    qtype: Qtype,
}

/// A response the server actually sent (counted after `send_response`
/// succeeds), by response code: the split between NoError and the negatives
/// (NXDomain, ServFail, ...) is what a dashboard watches, and comparing it
/// against the query counter shows requests that never produced a response.
#[derive(Event)]
#[event(
    event_name = "dns_response_sent",
    metric_name = "carbide_dns_responses_total",
    component = "carbide-dns",
    log = off,
    metric = counter,
    describe = "Number of DNS responses sent, by response code"
)]
struct DnsResponseSent {
    #[label]
    rcode: Rcode,
}

/// `DnsRequestCompleted` records the duration and the matching INFO diagnostic
/// from one emission. The response details stay off the histogram labels, but
/// remain native JSON values on the log record.
#[derive(Event)]
#[event(
    event_name = "dns_request_completed",
    metric_name = "carbide_dns_request_duration_milliseconds",
    component = "carbide-dns",
    log = info,
    metric = histogram,
    message = "Request completed",
    describe = "Time to process a DNS query, by query type and response code"
)]
struct DnsRequestCompleted {
    #[label]
    qtype: Qtype,
    #[label]
    rcode: Rcode,
    #[context(value)]
    response_code: String,
    #[context(value)]
    record_count: i64,
    #[context(value)]
    duration_milliseconds: f64,
    #[observation]
    took: Duration,
}

#[async_trait::async_trait]
impl RequestHandler for DnsServer {
    async fn handle_request<R: ResponseHandler, T: Time>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        // `request_info()` is fallible in hickory: a request we can't even
        // interpret gets a FormErr and no further processing.
        let request_info = match request.request_info() {
            Ok(request_info) => request_info,
            Err(_) => {
                // The query never parsed, so its type is unknowable -- it
                // counts under `other` so the query and response counters
                // stay comparable even for malformed traffic.
                emit(DnsQueryReceived {
                    qtype: Qtype::Other,
                });
                let response_info = response_handle
                    .send_response(
                        MessageResponseBuilder::new(&request.queries, None)
                            .error_msg(&request.metadata, ResponseCode::FormErr),
                    )
                    .await
                    .unwrap();
                emit(DnsResponseSent {
                    rcode: Rcode::FormErr,
                });
                return response_info;
            }
        };
        let qtype = request_info.query.query_type();
        let qname = request_info.query.name().to_string();

        let qtype_label = Qtype::from(qtype);
        emit(DnsQueryReceived { qtype: qtype_label });

        // Attach the span to the request future with `Instrument` rather than an
        // `Entered` guard. A guard held across an `.await` is not dropped when the
        // task yields.
        let span = tracing::info_span!("dns_request", %qname, %qtype);

        async move {
            let start = Instant::now();

            // Only handle types that DnsResourceRecordType supports and that we can build
            // RData for; return NotImp for everything else. Currently A, AAAA, and PTR
            // are supported; add arms here as the API and RData parsing are extended.
            let dns_qtype = match DnsResourceRecordType::try_from(qtype.to_string().as_str()) {
                Ok(
                    t @ (DnsResourceRecordType::A
                    | DnsResourceRecordType::AAAA
                    | DnsResourceRecordType::PTR),
                ) => t,
                _ => {
                    warn!(%qname, %qtype, "Unsupported query type");
                    let response = MessageResponseBuilder::from_message_request(request);
                    let response_info = response_handle
                        .send_response(
                            response.error_msg(request_info.metadata, ResponseCode::NotImp),
                        )
                        .await
                        .unwrap();
                    emit(DnsResponseSent {
                        rcode: Rcode::NotImp,
                    });
                    return response_info;
                }
            };

            let cache_key = CacheKey {
                qname: qname.clone(),
                qtype,
            };

            let cached = self.negative_cache.get(&cache_key);

            let record_name = Name::from(request_info.query.name());
            let message = MessageResponseBuilder::from_message_request(request);
            let mut response_header = Metadata::response_from_request(request_info.metadata);

            let (response_code, records) = if let Some(code) = cached {
                emit(DnsNegativeCacheHit {
                    response_code: NegativeCacheResponseCode(code),
                });
                (code, vec![])
            } else {
                // Clone the client out under the lock, then release it so the
                // upstream RPC runs without serializing other in-flight queries.
                let client = {
                    let guard = self.forge_client.lock().await;
                    guard.clone()
                };
                // a slow or overloaded carbide-api would otherwise
                // hold this handler open, piling up in-flight work and
                // stalling new queries. On timeout we Err on DeadlineExceeded,
                // which `classify_failure` maps to a briefly-cached ServFail, so we
                // fail fast
                //
                // TODO: this limits each call's *duration* but not the *number* of calls
                // Add a `tokio::sync::Semaphore`
                let result = match tokio::time::timeout(
                    self.upstream_lookup_timeout,
                    Self::retrieve_records(client, &qname, dns_qtype, &record_name),
                )
                .await
                {
                    Ok(inner) => inner,
                    Err(_elapsed) => Err(tonic::Status::deadline_exceeded(format!(
                        "upstream lookup_record exceeded {}s",
                        self.upstream_lookup_timeout.as_secs()
                    ))),
                };
                match result {
                    Ok(records) => {
                        tracing::info!(record_count = records.len(), "DNS lookup succeeded");
                        (ResponseCode::NoError, records)
                    }
                    Err(e) => {
                        let NegativeClassification { code, transient } = classify_failure(e.code());

                        // Emit before `record`: every upstream negative is a
                        // cache miss, even when admission evicts another entry.
                        emit(DnsNegativeCacheMiss {
                            response_code: NegativeCacheResponseCode(code),
                            error: e.to_string(),
                        });

                        // The LRU cache always admits the entry; a `true` return means
                        // a least-recently-used entry was evicted to make room.  Both are
                        // entries leaving the cache — so capacity pressure surfaces as
                        // a rising eviction rate.
                        if self.negative_cache.record(cache_key, code, transient) {
                            self.metrics.negative_cache_eviction.add(1, &[]);
                        }
                        tracing::debug!(response_code = %code, "Caching negative response");

                        (code, vec![])
                    }
                }
            };

            let duration = start.elapsed();
            emit(DnsRequestCompleted {
                qtype: qtype_label,
                rcode: Rcode::from(response_code),
                response_code: format!("{response_code:?}"),
                record_count: i64::try_from(records.len()).unwrap_or(i64::MAX),
                duration_milliseconds: duration.as_millis() as f64,
                took: duration,
            });

            response_header.response_code = response_code;
            let message = message.build(
                response_header,
                records.iter(),
                iter::empty(),
                iter::empty(),
                iter::empty(),
            );

            let response_info = response_handle.send_response(message).await.unwrap();
            emit(DnsResponseSent {
                rcode: Rcode::from(response_code),
            });
            response_info
        }
        .instrument(span)
        .await
    }
}

impl DnsServer {
    pub fn new(forge_client: Mutex<ForgeClientT>, meter: &Meter, config: &Config) -> Self {
        let servfail_ttl = config.servfail_cache_ttl();
        if servfail_ttl.as_secs() != config.negative_cache_servfail_ttl_secs {
            warn!(
                configured_servfail_ttl_seconds = config.negative_cache_servfail_ttl_secs,
                clamped_servfail_ttl_seconds = servfail_ttl.as_secs(),
                minimum_servfail_ttl_seconds = config::NEGATIVE_CACHE_SERVFAIL_TTL_MIN_SECS,
                maximum_servfail_ttl_seconds = config::NEGATIVE_CACHE_SERVFAIL_TTL_MAX_SECS,
                "negative_cache_servfail_ttl_secs out of range; clamped"
            );
        }

        let negative_cache = Arc::new(NegativeCache::new(
            Duration::from_secs(config.negative_cache_ttl_secs),
            servfail_ttl,
            config.negative_cache_entries_max_count as usize,
        ));

        Self {
            forge_client,
            upstream_lookup_timeout: Duration::from_secs(config.upstream_lookup_timeout_secs),
            // The metrics gauge callback holds a clone of the cache to read its
            // occupancy on scrape.
            metrics: DnsMetrics::new(meter, negative_cache.clone()),
            negative_cache,
        }
    }

    /// Queries carbide-api for DNS records matching `qname` and `qtype`, then
    /// converts the results into hickory `Record` objects ready for the response.
    // `#[instrument]` attaches the span to the returned future. skip_all fields by default,
    // then include only qname and qtype1
    #[tracing::instrument(level = "debug", skip_all, fields(qname = %qname, qtype = %qtype))]
    async fn retrieve_records(
        mut forge_client: ForgeClientT,
        qname: &str,
        qtype: DnsResourceRecordType,
        record_name: &Name,
    ) -> Result<Vec<Record>, tonic::Status> {
        let request = tonic::Request::new(DnsResourceRecordLookupRequest {
            qtype: qtype.to_string(),
            qname: qname.to_string(),
            zone_id: "-1".to_string(),
            local: None,
            remote: None,
            real_remote: None,
        });

        let api_start = Instant::now();
        let response = forge_client.lookup_record(request).await?.into_inner();
        let api_duration = api_start.elapsed();

        tracing::debug!(
            record_count = response.records.len(),
            duration_milliseconds = api_duration.as_millis(),
            "API lookup completed"
        );

        let records = response
            .records
            .into_iter()
            // The API returns all record types for the qname; keep only the requested type.
            .filter(|r| DnsResourceRecordType::try_from(r.qtype.as_str()).ok() == Some(qtype))
            .filter_map(|r| {
                let rdata = content_to_rdata(qtype, &r.content)?;
                // hickory infers the record type from the rdata; set the class
                // explicitly since `from_rdata` defaults it.
                let mut record = Record::from_rdata(record_name.clone(), r.ttl, rdata);
                record.dns_class = DNSClass::IN;
                Some(record)
            })
            .collect::<Vec<_>>();

        tracing::debug!(
            filtered_record_count = records.len(),
            "Records after filtering by qtype"
        );

        if records.is_empty() {
            return Err(tonic::Status::not_found(format!(
                "no {} records found for {}",
                qtype, qname
            )));
        }

        Ok(records)
    }

    pub async fn run(config: Config) -> Result<(), Report> {
        let listen = config.listen_address;

        info!(listen_address = %listen, "Starting DNS server");

        let forge_client_config = config.forge_client_config();
        let api_uri = config.api_uri.to_string();
        let api_config = ApiConfig::new(api_uri.as_str(), &forge_client_config);

        info!(%api_uri, "Connecting to carbide-api");

        let client = Mutex::new(ForgeTlsClient::retry_build(&api_config).await?);

        // Sweep at the shorter of the two negative-cache lifetimes. ServFail
        // entries expire far sooner than NXDomain/Refused, and although an
        // expired entry is never *served* (get() filters on expiry), it still
        // occupies a slot against the entry cap until swept. Reclaiming on the
        // short cadence keeps a ServFail flood from filling the cap with
        // already-expired entries and starving genuinely-new negatives.
        let sweep_interval = Duration::from_secs(config.negative_cache_ttl_secs)
            .min(config.servfail_cache_ttl())
            .max(Duration::from_secs(1));

        let metrics_setup = new_metrics_setup("carbide-dns", "carbide", true)?;
        carbide_instrument::log_events::register(&metrics_setup.meter);

        // Must keep meter_provider alive for the lifetime of the server;
        // dropping it shuts down the Prometheus exporter.
        let _metrics_guard = metrics_setup.meter_provider;

        let metrics_config = MetricsEndpointConfig {
            address: config.metrics_listen_address,
            registry: metrics_setup.registry,
            health_controller: Some(metrics_setup.health_controller),
            additional_prefix: None,
        };

        tokio::spawn(async move {
            tracing::info!(metrics_address = %metrics_config.address, "Spawning metrics endpoint");
            if let Err(e) = run_metrics_endpoint(&metrics_config).await {
                tracing::error!(error = %e, "Metrics endpoint error");
            }
        });

        let server = DnsServer::new(client, &metrics_setup.meter, &config);

        let cache = server.negative_cache.clone();
        let cache_eviction_counter = server.metrics.negative_cache_eviction.clone();

        // Periodically remove expired negative cache entries.
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(sweep_interval);
            loop {
                interval.tick().await;
                let evicted = cache.evict_expired();
                if evicted > 0 {
                    cache_eviction_counter.add(evicted as u64, &[]);
                }
            }
        });

        let mut srv = hickory_server::Server::new(server);
        let udp_socket = UdpSocket::bind(&listen).await?;
        srv.register_socket(udp_socket);

        let tcp_socket = TcpListener::bind(&listen).await?;
        // 32 is hickory_server's default response buffer size when run as a
        // binary; match it here.
        srv.register_listener(tcp_socket, Duration::new(5, 0), 32);

        info!(
            listen_address = %listen,
            version = carbide_version::version!(),
            "Started DNS server",
        );

        match srv.block_until_done().await {
            Ok(()) => {
                info!("Carbide-dns server is stopping");
            }
            Err(e) => {
                let error_msg = format!("Carbide-dns has encountered an error: {e}");
                error!(error = %e, "Carbide-dns has encountered an error");
                return Err(eyre::eyre!("{}", error_msg));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_failure_maps_grpc_codes_to_dns_response_codes() {
        use carbide_test_support::value_scenarios;
        use tonic::Code;

        /// The classification a gRPC code is expected to produce: the DNS
        /// response code returned to the client and whether it is transient.
        #[derive(Debug, PartialEq)]
        struct Expected {
            code: ResponseCode,
            transient: bool,
        }

        value_scenarios!(
            run = |code| {
                let NegativeClassification { code, transient } = classify_failure(code);
                Expected { code, transient }
            };
            "stable, authoritative negatives" {
                // The name genuinely does not exist.
                Code::NotFound => Expected { code: ResponseCode::NXDomain, transient: false },
                // A malformed query stays malformed on retry.
                Code::InvalidArgument => Expected { code: ResponseCode::FormErr, transient: false },
                // The upstream does not implement this qtype/operation.
                Code::Unimplemented => Expected { code: ResponseCode::NotImp, transient: false },
            }
            "policy refusals" {
                Code::PermissionDenied => Expected { code: ResponseCode::Refused, transient: false },
                Code::Unauthenticated => Expected { code: ResponseCode::Refused, transient: false },
            }
            "transient failures cached briefly as ServFail" {
                Code::Internal => Expected { code: ResponseCode::ServFail, transient: true },
                Code::Unavailable => Expected { code: ResponseCode::ServFail, transient: true },
                Code::DeadlineExceeded => Expected { code: ResponseCode::ServFail, transient: true },
                Code::ResourceExhausted => Expected { code: ResponseCode::ServFail, transient: true },
                Code::Aborted => Expected { code: ResponseCode::ServFail, transient: true },
                Code::Cancelled => Expected { code: ResponseCode::ServFail, transient: true },
                Code::AlreadyExists => Expected { code: ResponseCode::ServFail, transient: true },
                Code::FailedPrecondition => Expected { code: ResponseCode::ServFail, transient: true },
                Code::OutOfRange => Expected { code: ResponseCode::ServFail, transient: true },
                Code::DataLoss => Expected { code: ResponseCode::ServFail, transient: true },
                Code::Ok => Expected { code: ResponseCode::ServFail, transient: true },
                Code::Unknown => Expected { code: ResponseCode::ServFail, transient: true },
            }
        );
    }

    #[test]
    fn content_to_rdata_builds_supported_types_and_drops_unparseable() {
        use std::net::{Ipv4Addr, Ipv6Addr};

        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |(qtype, content): (DnsResourceRecordType, &str)| content_to_rdata(qtype, content);
            "supported types build the matching RData" {
                (DnsResourceRecordType::A, "192.0.2.1")
                    => Some(RData::A(Ipv4Addr::new(192, 0, 2, 1).into())),
                (DnsResourceRecordType::AAAA, "fd00::1")
                    => Some(RData::AAAA("fd00::1".parse::<Ipv6Addr>().unwrap().into())),
                // A PTR's content is the target FQDN, which round-trips into the RData.
                (DnsResourceRecordType::PTR, "host.example.com.")
                    => Some(RData::PTR(PTR("host.example.com.".parse::<Name>().unwrap()))),
            }
            "unparseable content is dropped rather than panicked on" {
                (DnsResourceRecordType::A, "not-an-ip") => None,
                (DnsResourceRecordType::AAAA, "192.0.2.1") => None,
            }
            "a type the gate never dispatches here yields nothing" {
                (DnsResourceRecordType::SOA, "unused") => None,
            }
        );
    }

    #[test]
    fn qtype_label_gives_resolvable_types_their_own_value() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |qtype: RecordType| Qtype::from(qtype);
            "the types the server resolves" {
                RecordType::A => Qtype::A,
                RecordType::AAAA => Qtype::Aaaa,
                RecordType::PTR => Qtype::Ptr,
            }
            "everything else collapses into Other" {
                RecordType::MX => Qtype::Other,
                RecordType::SOA => Qtype::Other,
                RecordType::TXT => Qtype::Other,
                RecordType::ANY => Qtype::Other,
            }
        );
    }

    #[test]
    fn rcode_label_gives_returned_codes_their_own_value() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |code: ResponseCode| Rcode::from(code);
            "the codes the server returns" {
                ResponseCode::NoError => Rcode::NoError,
                ResponseCode::FormErr => Rcode::FormErr,
                ResponseCode::NXDomain => Rcode::NxDomain,
                ResponseCode::ServFail => Rcode::ServFail,
                ResponseCode::Refused => Rcode::Refused,
                ResponseCode::NotImp => Rcode::NotImp,
            }
            "codes the server never returns collapse into Other" {
                ResponseCode::NotAuth => Rcode::Other,
                ResponseCode::NXRRSet => Rcode::Other,
                ResponseCode::Unknown(999) => Rcode::Other,
            }
        );
    }

    #[test]
    fn negative_cache_response_code_preserves_legacy_debug_rendering() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |code: ResponseCode| NegativeCacheResponseCode(code).label_value().to_string();
            "the classified negative response codes" {
                ResponseCode::FormErr => "FormErr".to_string(),
                ResponseCode::NXDomain => "NXDomain".to_string(),
                ResponseCode::ServFail => "ServFail".to_string(),
                ResponseCode::Refused => "Refused".to_string(),
                ResponseCode::NotImp => "NotImp".to_string(),
            }
        );
    }

    /// `dns_negative_cache_events_pair_metrics_and_logs` pins one counter
    /// increment and one diagnostic record to the same `emit`, including the
    /// shared `response_code` label spelling.
    #[test]
    fn dns_negative_cache_events_pair_metrics_and_logs() {
        use carbide_instrument::testing::{MetricsCapture, capture_logs};
        use carbide_test_support::{Check, check_values};

        enum NegativeCacheEvent {
            Hit(ResponseCode),
            Miss {
                response_code: ResponseCode,
                error: &'static str,
            },
        }

        #[derive(Debug, PartialEq)]
        struct LogObservation {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            response_code: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            log_count: usize,
            log: Option<LogObservation>,
            counters: [f64; 2],
        }

        fn expected_log(
            level: tracing::Level,
            metadata_name: &str,
            message: &str,
            metric_name: &str,
            response_code: &str,
            error: Option<&str>,
        ) -> Option<LogObservation> {
            Some(LogObservation {
                level,
                metadata_name: metadata_name.to_string(),
                message: message.to_string(),
                event_name: Some(metadata_name.to_string()),
                metric_name: Some(metric_name.to_string()),
                response_code: Some(response_code.to_string()),
                error: error.map(str::to_string),
            })
        }

        check_values(
            [
                Check {
                    scenario: "negative-cache hit",
                    input: NegativeCacheEvent::Hit(ResponseCode::NXDomain),
                    expect: Observation {
                        log_count: 1,
                        log: expected_log(
                            tracing::Level::DEBUG,
                            "dns_negative_cache_hit",
                            "Negative cache hit",
                            "carbide_dns_negative_cache_hit_count_total",
                            "NXDomain",
                            None,
                        ),
                        counters: [1.0, 0.0],
                    },
                },
                Check {
                    scenario: "negative-cache miss",
                    input: NegativeCacheEvent::Miss {
                        response_code: ResponseCode::ServFail,
                        error: "upstream unavailable",
                    },
                    expect: Observation {
                        log_count: 1,
                        log: expected_log(
                            tracing::Level::WARN,
                            "dns_negative_cache_miss",
                            "DNS lookup failed",
                            "carbide_dns_negative_cache_miss_count_total",
                            "ServFail",
                            Some("upstream unavailable"),
                        ),
                        counters: [0.0, 1.0],
                    },
                },
            ],
            |event| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| match event {
                    NegativeCacheEvent::Hit(response_code) => emit(DnsNegativeCacheHit {
                        response_code: NegativeCacheResponseCode(response_code),
                    }),
                    NegativeCacheEvent::Miss {
                        response_code,
                        error,
                    } => emit(DnsNegativeCacheMiss {
                        response_code: NegativeCacheResponseCode(response_code),
                        error: error.to_string(),
                    }),
                });
                let log = logs.first().map(|log| LogObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    response_code: log.field("response_code").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                });

                Observation {
                    log_count: logs.len(),
                    log,
                    counters: [
                        metrics.counter_delta(
                            "carbide_dns_negative_cache_hit_count_total",
                            &[("response_code", "NXDomain")],
                        ),
                        metrics.counter_delta(
                            "carbide_dns_negative_cache_miss_count_total",
                            &[("response_code", "ServFail")],
                        ),
                    ],
                }
            },
        );
    }

    /// The query and response events only move their counters; neither builds
    /// a per-request log record.
    #[test]
    fn dns_query_and_response_events_move_their_metrics_without_logging() {
        use carbide_instrument::testing::{MetricsCapture, capture_logs};

        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(DnsQueryReceived { qtype: Qtype::A });
            emit(DnsQueryReceived {
                qtype: Qtype::Other,
            });
            emit(DnsResponseSent {
                rcode: Rcode::NoError,
            });
            emit(DnsResponseSent {
                rcode: Rcode::NotImp,
            });
        });

        assert!(
            logs.is_empty(),
            "request-rate events are metric-only: {logs:?}"
        );

        assert_eq!(
            metrics.counter_delta("carbide_dns_queries_total", &[("qtype", "a")]),
            1.0
        );
        assert_eq!(
            metrics.counter_delta("carbide_dns_queries_total", &[("qtype", "other")]),
            1.0
        );
        assert_eq!(
            metrics.counter_delta("carbide_dns_responses_total", &[("rcode", "no_error")]),
            1.0
        );
        assert_eq!(
            metrics.counter_delta("carbide_dns_responses_total", &[("rcode", "not_imp")]),
            1.0
        );
    }

    /// `DnsRequestCompleted` replaces the separate histogram and INFO calls:
    /// one `emit()` records one duration and one diagnostic with the response
    /// details kept as native structured values.
    #[test]
    fn dns_request_completed_pairs_its_histogram_and_log() {
        use carbide_instrument::testing::{
            ApproxHistogramSum, CapturedFieldKind, MetricsCapture, capture_logs,
        };
        use carbide_test_support::{Check, check_values};

        struct RequestCompletedCase {
            qtype: Qtype,
            qtype_label: &'static str,
            rcode: Rcode,
            rcode_label: &'static str,
            response_code: &'static str,
            record_count: i64,
            duration: Duration,
            duration_milliseconds: f64,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            log_count: usize,
            metadata_name: Option<String>,
            level: Option<tracing::Level>,
            message: Option<String>,
            event_name: Option<String>,
            metric_name: Option<String>,
            qtype: Option<String>,
            rcode: Option<String>,
            response_code: Option<String>,
            record_count: Option<String>,
            duration_milliseconds: Option<String>,
            response_code_kind: Option<CapturedFieldKind>,
            record_count_kind: Option<CapturedFieldKind>,
            duration_milliseconds_kind: Option<CapturedFieldKind>,
            histogram_count: u64,
            histogram_sum: ApproxHistogramSum,
        }

        check_values(
            [
                Check {
                    scenario: "successful A request",
                    input: RequestCompletedCase {
                        qtype: Qtype::A,
                        qtype_label: "a",
                        rcode: Rcode::NoError,
                        rcode_label: "no_error",
                        response_code: "NoError",
                        record_count: 3,
                        duration: Duration::from_millis(250),
                        duration_milliseconds: 250.0,
                    },
                    expect: Observation {
                        log_count: 1,
                        metadata_name: Some("dns_request_completed".to_string()),
                        level: Some(tracing::Level::INFO),
                        message: Some("Request completed".to_string()),
                        event_name: Some("dns_request_completed".to_string()),
                        metric_name: Some("carbide_dns_request_duration_milliseconds".to_string()),
                        qtype: Some("a".to_string()),
                        rcode: Some("no_error".to_string()),
                        response_code: Some("NoError".to_string()),
                        record_count: Some("3".to_string()),
                        duration_milliseconds: Some("250".to_string()),
                        response_code_kind: Some(CapturedFieldKind::String),
                        record_count_kind: Some(CapturedFieldKind::I64),
                        duration_milliseconds_kind: Some(CapturedFieldKind::F64),
                        histogram_count: 1,
                        histogram_sum: ApproxHistogramSum(250.0),
                    },
                },
                Check {
                    scenario: "event-level NotImp completion",
                    input: RequestCompletedCase {
                        qtype: Qtype::Other,
                        qtype_label: "other",
                        rcode: Rcode::NotImp,
                        rcode_label: "not_imp",
                        response_code: "NotImp",
                        record_count: 0,
                        duration: Duration::from_millis(1_500),
                        duration_milliseconds: 1_500.0,
                    },
                    expect: Observation {
                        log_count: 1,
                        metadata_name: Some("dns_request_completed".to_string()),
                        level: Some(tracing::Level::INFO),
                        message: Some("Request completed".to_string()),
                        event_name: Some("dns_request_completed".to_string()),
                        metric_name: Some("carbide_dns_request_duration_milliseconds".to_string()),
                        qtype: Some("other".to_string()),
                        rcode: Some("not_imp".to_string()),
                        response_code: Some("NotImp".to_string()),
                        record_count: Some("0".to_string()),
                        duration_milliseconds: Some("1500".to_string()),
                        response_code_kind: Some(CapturedFieldKind::String),
                        record_count_kind: Some(CapturedFieldKind::I64),
                        duration_milliseconds_kind: Some(CapturedFieldKind::F64),
                        histogram_count: 1,
                        histogram_sum: ApproxHistogramSum(1_500.0),
                    },
                },
            ],
            |case| {
                let RequestCompletedCase {
                    qtype,
                    qtype_label,
                    rcode,
                    rcode_label,
                    response_code,
                    record_count,
                    duration,
                    duration_milliseconds,
                } = case;
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    emit(DnsRequestCompleted {
                        qtype,
                        rcode,
                        response_code: response_code.to_string(),
                        record_count,
                        duration_milliseconds,
                        took: duration,
                    });
                });
                let log = logs.first();

                Observation {
                    log_count: logs.len(),
                    metadata_name: log.map(|log| log.metadata_name.clone()),
                    level: log.map(|log| log.level),
                    message: log.map(|log| log.message.clone()),
                    event_name: log
                        .and_then(|log| log.field("event_name"))
                        .map(str::to_string),
                    metric_name: log
                        .and_then(|log| log.field("metric_name"))
                        .map(str::to_string),
                    qtype: log.and_then(|log| log.field("qtype")).map(str::to_string),
                    rcode: log.and_then(|log| log.field("rcode")).map(str::to_string),
                    response_code: log
                        .and_then(|log| log.field("response_code"))
                        .map(str::to_string),
                    record_count: log
                        .and_then(|log| log.field("record_count"))
                        .map(str::to_string),
                    duration_milliseconds: log
                        .and_then(|log| log.field("duration_milliseconds"))
                        .map(str::to_string),
                    response_code_kind: log.and_then(|log| log.field_kind("response_code")),
                    record_count_kind: log.and_then(|log| log.field_kind("record_count")),
                    duration_milliseconds_kind: log
                        .and_then(|log| log.field_kind("duration_milliseconds")),
                    histogram_count: metrics.histogram_count_delta(
                        "carbide_dns_request_duration_milliseconds",
                        &[("qtype", qtype_label), ("rcode", rcode_label)],
                    ),
                    histogram_sum: metrics.histogram_sum_delta(
                        "carbide_dns_request_duration_milliseconds",
                        &[("qtype", qtype_label), ("rcode", rcode_label)],
                    ),
                }
            },
        );
    }
}
