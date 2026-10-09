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

//! Packet-level counters for the Kea hook, plus the metrics endpoint, health
//! plumbing, and the certificate-expiry gauge.
//!
//! The counters are `carbide-instrument` events declared with `log = off`:
//! the Kea process installs no tracing subscriber, so the C++ `LOG_ERROR`
//! lines in `callouts.cc` remain the log side and the events move only the
//! metric. The DHCPv4 counters retain their existing exposed Prometheus names
//! and `reason` label values, while DHCPv6 traffic uses explicit v6 metric
//! names. The certificate-expiry gauge is point-in-time state, not an
//! occurrence, and stays on the observable-gauge pattern. `metrics_server`
//! installs the metrics endpoint's Prometheus-only meter provider, so there is
//! no separate OTLP instrument-name contract for these counters.

use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use carbide_instrument::{Event, LabelValue, emit};
use metrics_endpoint::{
    HealthController, MetricsEndpointConfig, MetricsSetup, new_metrics_setup, run_metrics_endpoint,
};
use opentelemetry::StringValue;
use rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig};
use tokio::runtime::Runtime;
use tokio::time::{interval, timeout};

use crate::{CONFIG, CarbideDhcpContext, CarbideDhcpMetrics, tls};

const METRICS_CAPTURE_FREQUENCY: Duration = Duration::from_secs(30);
const READINESS_CHECK_FREQUENCY: Duration = Duration::from_secs(30);

/// The DHCP message type of an outgoing reply, as a bounded metric label.
///
/// Built from the raw RFC 2131 message-type code Kea reports for the response
/// (`Pkt4::getType()`): DHCPOFFER = 2, DHCPACK = 5, DHCPNAK = 6. Those three
/// are the only types a DHCPv4 server sends, so any other code counts as
/// `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum ReplyMessageType {
    Offer,
    Ack,
    Nak,
    Other,
}

impl From<u8> for ReplyMessageType {
    fn from(message_type_code: u8) -> Self {
        match message_type_code {
            2 => Self::Offer, // DHCPOFFER
            5 => Self::Ack,   // DHCPACK
            6 => Self::Nak,   // DHCPNAK
            _ => Self::Other,
        }
    }
}

/// The DHCPv6 message type of an incoming client request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum V6RequestMessageType {
    Solicit,
    Request,
    Confirm,
    Renew,
    Rebind,
    Release,
    Decline,
    InformationRequest,
    Other,
}

impl From<u8> for V6RequestMessageType {
    fn from(message_type_code: u8) -> Self {
        match message_type_code {
            1 => Self::Solicit,
            3 => Self::Request,
            4 => Self::Confirm,
            5 => Self::Renew,
            6 => Self::Rebind,
            8 => Self::Release,
            9 => Self::Decline,
            11 => Self::InformationRequest,
            _ => Self::Other,
        }
    }
}

/// The DHCPv6 message type of an outgoing response, as a bounded metric label.
///
/// Built from the raw DHCPv6 message-type code Kea reports for the response
/// (`Pkt6::getType()`). Kea represents relay envelopes separately, so this
/// label intentionally describes the inner response type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum V6ReplyMessageType {
    Advertise,
    Reply,
    Reconfigure,
    Other,
}

impl From<u8> for V6ReplyMessageType {
    fn from(message_type_code: u8) -> Self {
        match message_type_code {
            2 => Self::Advertise,    // DHCPV6_ADVERTISE
            7 => Self::Reply,        // DHCPV6_REPLY
            10 => Self::Reconfigure, // DHCPV6_RECONFIGURE
            _ => Self::Other,
        }
    }
}

/// Why the hook dropped or refused a DHCP request, as the bounded `reason`
/// label on the grandfathered `carbide_dhcp_dropped_requests_total` counter.
///
/// The rendered strings are part of the metric's contract: the long-counted
/// reasons render byte-identically to the strings their sites have always
/// reported (including every `DiscoveryBuilderResult` name, which
/// `pkt4_receive` passes through verbatim on a discovery failure), and the
/// newer refusal reasons follow the same PascalCase style.
/// [`DropReason::Unknown`] closes the domain: a reason string outside this
/// taxonomy buckets there instead of minting a new time series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DropReason {
    /// `pkt4_receive` refuses packets that did not arrive through a relay.
    NonRelayedPacket,
    /// `pkt4_receive` drops packets whose machine carries no usable IPv4
    /// address.
    NoUsableIPv4Address,
    // One variant per `DiscoveryBuilderResult` name: on a machine-discovery
    // failure, `pkt4_receive` reports the result's name verbatim as the
    // reason. `Success` covers the defensive edge where discovery reported
    // success yet returned no machine object.
    Success,
    InvalidDiscoveryBuilderPointer,
    InvalidMacAddress,
    InvalidVendorClass,
    InvalidMachinePointer,
    BuilderError,
    FetchMachineError,
    InvalidCircuitId,
    TooManyFailuresError,
    InvalidDesiredAddress,
    /// `pkt4_send` hit an exception while encoding a response option.
    OptionEncodingFailed,
    /// `pkt4_send` found no machine object on the callout context.
    MissingMachineContext,
    /// `lease4_select` refused to let Kea assign its selected lease (missing
    /// hook state, or no usable address from Carbide).
    AllocationRefused,
    /// `lease4_renew` refused to let Kea extend an existing lease (missing
    /// hook state, or no usable address from Carbide).
    RenewalRefused,
    /// The FFI reason string was not part of this taxonomy; bucketing keeps
    /// the label domain closed.
    Unknown,
}

impl DropReason {
    /// Every variant, as the single source for the string mapping and tests.
    // Every variant must appear here: `From<&str>` resolves FFI strings by
    // scanning this table, so a variant missing from it buckets to `Unknown`.
    const ALL: [Self; 17] = [
        Self::NonRelayedPacket,
        Self::NoUsableIPv4Address,
        Self::Success,
        Self::InvalidDiscoveryBuilderPointer,
        Self::InvalidMacAddress,
        Self::InvalidVendorClass,
        Self::InvalidMachinePointer,
        Self::BuilderError,
        Self::FetchMachineError,
        Self::InvalidCircuitId,
        Self::TooManyFailuresError,
        Self::InvalidDesiredAddress,
        Self::OptionEncodingFailed,
        Self::MissingMachineContext,
        Self::AllocationRefused,
        Self::RenewalRefused,
        Self::Unknown,
    ];

    /// The exact string exposed as the metric's `reason` label value, and
    /// accepted back across the FFI. PascalCase by contract: dashboards
    /// select on these bytes.
    const fn as_label(self) -> &'static str {
        match self {
            Self::NonRelayedPacket => "NonRelayedPacket",
            Self::NoUsableIPv4Address => "NoUsableIPv4Address",
            Self::Success => "Success",
            Self::InvalidDiscoveryBuilderPointer => "InvalidDiscoveryBuilderPointer",
            Self::InvalidMacAddress => "InvalidMacAddress",
            Self::InvalidVendorClass => "InvalidVendorClass",
            Self::InvalidMachinePointer => "InvalidMachinePointer",
            Self::BuilderError => "BuilderError",
            Self::FetchMachineError => "FetchMachineError",
            Self::InvalidCircuitId => "InvalidCircuitId",
            Self::TooManyFailuresError => "TooManyFailuresError",
            Self::InvalidDesiredAddress => "InvalidDesiredAddress",
            Self::OptionEncodingFailed => "OptionEncodingFailed",
            Self::MissingMachineContext => "MissingMachineContext",
            Self::AllocationRefused => "AllocationRefused",
            Self::RenewalRefused => "RenewalRefused",
            Self::Unknown => "Unknown",
        }
    }
}

/// Manual rather than derived: the derive renders variant names in
/// snake_case, and this label's values are the grandfathered PascalCase
/// strings existing dashboards already select on. The fieldless enum itself
/// keeps the domain closed.
impl LabelValue for DropReason {
    fn label_value(&self) -> StringValue {
        StringValue::from(self.as_label())
    }
}

impl From<&str> for DropReason {
    /// Maps an FFI reason string onto the taxonomy; anything unrecognized
    /// buckets to [`DropReason::Unknown`].
    fn from(reason: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_label() == reason)
            .unwrap_or(Self::Unknown)
    }
}

/// Why the DHCPv6 hook dropped a request, as the bounded `reason` label on
/// `carbide_dhcp_v6_requests_dropped_total`.
///
/// These labels intentionally follow the existing DHCPv6 snake_case contract,
/// except `NonRelayedPacket`, which is shared with the v4 drop path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum V6DropReason {
    NonRelayedPacket,
    Success,
    Ignore,
    ConfirmNotOnLink,
    InvalidPacket,
    NestedRelay,
    NoMacNoOption79,
    UnsupportedDuid,
    UnsupportedMessage,
    InvalidMachinePointer,
    FetchMachineError,
    TooManyFailuresError,
    Unknown,
}

impl V6DropReason {
    /// Every variant, as the single source for FFI string mapping and tests.
    const ALL: [Self; 13] = [
        Self::NonRelayedPacket,
        Self::Success,
        Self::Ignore,
        Self::ConfirmNotOnLink,
        Self::InvalidPacket,
        Self::NestedRelay,
        Self::NoMacNoOption79,
        Self::UnsupportedDuid,
        Self::UnsupportedMessage,
        Self::InvalidMachinePointer,
        Self::FetchMachineError,
        Self::TooManyFailuresError,
        Self::Unknown,
    ];

    /// The exact metric `reason` label value accepted from the Kea FFI.
    const fn as_label(self) -> &'static str {
        match self {
            Self::NonRelayedPacket => "NonRelayedPacket",
            Self::Success => "success",
            Self::Ignore => "ignore",
            Self::ConfirmNotOnLink => "confirm_not_on_link",
            Self::InvalidPacket => "invalid_packet",
            Self::NestedRelay => "nested_relay",
            Self::NoMacNoOption79 => "no_mac_no_option79",
            Self::UnsupportedDuid => "unsupported_duid",
            Self::UnsupportedMessage => "unsupported_message",
            Self::InvalidMachinePointer => "invalid_machine_pointer",
            Self::FetchMachineError => "fetch_machine_error",
            Self::TooManyFailuresError => "too_many_failures_error",
            Self::Unknown => "unknown",
        }
    }
}

impl LabelValue for V6DropReason {
    fn label_value(&self) -> StringValue {
        StringValue::from(self.as_label())
    }
}

impl From<&str> for V6DropReason {
    /// Maps an FFI reason string onto the v6 taxonomy; anything unrecognized
    /// buckets to [`V6DropReason::Unknown`].
    fn from(reason: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_label() == reason)
            .unwrap_or(Self::Unknown)
    }
}

/// A DHCPv4 request reached the hook's `pkt4_receive` callout, whatever becomes
/// of it next.
#[derive(Event)]
#[event(
    event_name = "kea_dhcp_request_received",
    metric_name = "carbide_dhcp_requests_total",
    component = "carbide-dhcp",
    log = off,
    metric = counter,
    describe = "Number of DHCPv4 requests received."
)]
struct DhcpRequestReceived;

/// The hook dropped or refused a DHCPv4 request. This counts refusal events,
/// not packets: an exchange that fails at more than one callout (a refused
/// lease selection and then a missing machine at send time) counts each site.
#[derive(Event)]
#[event(
    event_name = "kea_dhcp_request_dropped",
    metric_name = "carbide_dhcp_dropped_requests_total",
    component = "carbide-dhcp",
    log = off,
    metric = counter,
    describe = "Number of DHCPv4 requests dropped or refused, by reason."
)]
struct DhcpRequestDropped {
    #[label]
    reason: DropReason,
}

/// A DHCPv6 request reached the hook's `pkt6_receive` callout.
#[derive(Event)]
#[event(
    event_name = "kea_dhcp_v6_request_received",
    metric_name = "carbide_dhcp_v6_requests_total",
    component = "carbide-dhcp",
    log = off,
    metric = counter,
    describe = "Number of DHCPv6 requests received, by DHCP message type."
)]
struct DhcpV6RequestReceived {
    #[label]
    message_type: V6RequestMessageType,
}

/// The DHCPv6 hook dropped a packet before Kea could safely answer it.
#[derive(Event)]
#[event(
    event_name = "kea_dhcp_v6_request_dropped",
    metric_name = "carbide_dhcp_v6_requests_dropped_total",
    component = "carbide-dhcp",
    log = off,
    metric = counter,
    describe = "Number of DHCPv6 requests dropped or refused, by reason."
)]
struct DhcpV6RequestDropped {
    #[label]
    reason: V6DropReason,
}

/// A fully assembled DHCPv4 reply left `pkt4_send` for transmission: an `offer`
/// proposes a lease, an `ack` commits one, a `nak` refuses one.
///
/// Unlike the two request counters this metric is new, so it uses the
/// standard name rather than a grandfathered one.
#[derive(Event)]
#[event(
    event_name = "kea_dhcp_reply_sent",
    metric_name = "carbide_dhcp_replies_sent_total",
    component = "carbide-dhcp",
    log = off,
    metric = counter,
    describe = "Number of DHCPv4 replies sent, by reply message type."
)]
struct DhcpReplySent {
    #[label]
    message_type: ReplyMessageType,
}

/// A fully assembled DHCPv6 response left `pkt6_send` for transmission.
#[derive(Event)]
#[event(
    event_name = "kea_dhcp_v6_reply_sent",
    metric_name = "carbide_dhcp_v6_replies_sent_total",
    component = "carbide-dhcp",
    log = off,
    metric = counter,
    describe = "Number of DHCPv6 replies sent, by response message type."
)]
struct DhcpV6ReplySent {
    #[label]
    message_type: V6ReplyMessageType,
}

async fn certificate_loop() {
    let mut interval = tokio::time::interval(METRICS_CAPTURE_FREQUENCY);
    loop {
        interval.tick().await;
        let metrics = CONFIG
            .read()
            .expect("config lock poisoned?")
            .metrics
            .clone();
        if let Some(metrics) = metrics
            && let Some(client_expiry) = metrics.forge_client_config.client_cert_expiry()
        {
            metrics
                .certificate_expiration_value
                .store(client_expiry, Ordering::SeqCst);
        }
    }
}

fn initialize_metrics(mconf: &MetricsSetup) -> CarbideDhcpMetrics {
    let certificate_expiration_value = Arc::new(AtomicI64::new(0));
    let metrics = CarbideDhcpMetrics {
        forge_client_config: tls::build_forge_client_config(),
        certificate_expiration_value: certificate_expiration_value.clone(),
    };

    // Observable gauges don't need to be stored anywhere, they're
    // stored internally within the meter and the callback is run when metrics are
    // collected.
    mconf
        .meter
        .i64_observable_gauge("carbide-dhcp.certificate_expiration_time")
        .with_description("The certificate expiration time (epoch seconds)")
        .with_callback(move |observer| {
            let measurement = certificate_expiration_value.deref().load(Ordering::SeqCst);
            observer.observe(measurement, &[]);
        })
        .build();

    metrics
}

pub(super) fn metrics_server() {
    let metrics_endpoint = CONFIG
        .read()
        .expect("config lock poisoned?")
        .metrics_endpoint;

    if let Some(metrics_endpoint) = metrics_endpoint {
        let mconf = new_metrics_setup("carbide-dhcp", "forge-system", true);
        match mconf {
            Ok(mconf) => {
                // initialize metrics.
                let metrics = initialize_metrics(&mconf);
                let health_controller = HealthController::new();

                {
                    let mut config = CONFIG.write().expect("config lock poisoned");
                    config.metrics = Some(metrics);
                    config.health_controller = Some(health_controller.clone());
                }

                let runtime: &Runtime = CarbideDhcpContext::get_tokio_runtime();
                // start certificate loop
                runtime.spawn(async move {
                    certificate_loop().await;
                });
                // start readiness loop
                runtime.spawn(async move {
                    start_readiness_monitoring().await;
                });

                // start metrics server
                runtime.block_on(async move {
                    if let Err(e) = run_metrics_endpoint(&MetricsEndpointConfig {
                        address: metrics_endpoint,
                        registry: mconf.registry,
                        health_controller: Some(health_controller),
                        additional_prefix: None,
                    })
                    .await
                    {
                        log::error!("Metrics endpoint failed with error: {e}");
                    }
                });
            }
            Err(err) => {
                log::error!("failed to set-up metrics config: {err}");
            }
        }
    } else {
        log::warn!("no metrics endpoint configured, no metrics will be recorded");
    }
}

async fn check_api_connectivity(carbide_api_url: &str, client_config: &ForgeClientConfig) -> bool {
    let api_config: ApiConfig<'_> = ApiConfig::new(carbide_api_url, client_config);
    match forge_tls_client::ForgeTlsClient::retry_build(&api_config).await {
        Ok(mut client) => {
            let request = tonic::Request::new(rpc::forge::EchoRequest {
                message: "dhcp_echo".into(),
            });

            match client.echo(request).await {
                Ok(_) => true,
                Err(e) => {
                    log::error!("error communication with carbide API: {e:?}");
                    false
                }
            }
        }
        Err(e) => {
            log::error!("api connectivity check timed out: {e:?}");
            false
        }
    }
}

async fn start_readiness_monitoring() {
    let mut readiness_interval = interval(READINESS_CHECK_FREQUENCY);
    let forge_client_config = tls::build_forge_client_config();

    let url = &CONFIG.read().expect("config poisoned").api_endpoint.clone();

    loop {
        readiness_interval.tick().await;
        match timeout(
            Duration::from_secs(10),
            check_api_connectivity(url, &forge_client_config),
        )
        .await
        {
            Ok(result) => set_service_ready(result),
            Err(e) => {
                log::warn!("Readiness check timed out: {e:?}");
                set_service_ready(false)
            }
        }
    }
}

/// True once [`metrics_server`] has installed the global meter provider and
/// stored the initialized metrics state.
///
/// Event instruments resolve from the global meter once per event type on
/// first emit, so emitting before the provider install would bind that event
/// type to the no-op meter for the process lifetime. The gate also keeps an
/// endpointless configuration recording nothing, as before.
fn metrics_initialized() -> bool {
    CONFIG
        .read()
        .expect("config lock poisoned")
        .metrics
        .is_some()
}

pub(super) fn increment_total_requests() {
    if metrics_initialized() {
        emit(DhcpRequestReceived);
    }
}

/// Increment the DHCPv6 request counter for an incoming message type.
pub(super) fn increment_v6_requests(message_type: V6RequestMessageType) {
    if metrics_initialized() {
        emit(DhcpV6RequestReceived { message_type });
    }
}

pub(super) fn increment_dropped_requests(reason: DropReason) {
    if metrics_initialized() {
        emit(DhcpRequestDropped { reason });
    }
}

pub(super) fn increment_reply_sent(message_type: ReplyMessageType) {
    if metrics_initialized() {
        emit(DhcpReplySent { message_type });
    }
}

/// Increment the DHCPv6 reply-sent counter for an outgoing response type.
pub(super) fn increment_v6_reply_sent(message_type: V6ReplyMessageType) {
    if metrics_initialized() {
        emit(DhcpV6ReplySent { message_type });
    }
}

/// Increment the DHCPv6-specific dropped-request counter for a reason label.
pub(super) fn increment_dropped_v6_requests(reason: V6DropReason) {
    if metrics_initialized() {
        emit(DhcpV6RequestDropped { reason });
    }
}

fn set_service_ready(ready: bool) {
    if let Some(health_controller) = &CONFIG
        .read()
        .expect("config lock poisoned")
        .health_controller
    {
        health_controller.set_ready(ready);
        log::debug!("DHCP readiness set to: {ready}");
    }
}

pub(super) fn set_service_healthy(healthy: bool) {
    if let Some(health_controller) = &CONFIG
        .read()
        .expect("config lock poisoned")
        .health_controller
    {
        health_controller.set_healthy(healthy);
        log::debug!("DHCP health set to: {healthy}");
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CStr;

    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};
    use prometheus::{Encoder, TextEncoder};

    use super::*;
    use crate::discovery::{DiscoveryBuilderResult, discovery_builder_result_as_str};

    /// The certificate-expiry gauge stays on the per-process meter; its
    /// exposition is pinned verbatim.
    #[test]
    fn certificate_gauge_exposition_is_stable() {
        let mconf = new_metrics_setup("carbide-dhcp", "forge-system", false).unwrap();
        let metrics = initialize_metrics(&mconf);
        metrics
            .certificate_expiration_value
            .store(1740173562, Ordering::SeqCst);

        let mut buffer = vec![];
        let encoder = TextEncoder::new();
        let metric_families = mconf.registry.gather();
        encoder.encode(&metric_families, &mut buffer).unwrap();

        let prom_metrics = String::from_utf8(buffer).unwrap();
        assert_eq!(prom_metrics, include_str!("../tests/fixtures/metrics.txt"));
    }

    #[test]
    fn reply_message_type_maps_the_rfc2131_reply_codes_and_buckets_the_rest() {
        check_values(
            [
                Check {
                    scenario: "DHCPOFFER (2)",
                    input: 2u8,
                    expect: ReplyMessageType::Offer,
                },
                Check {
                    scenario: "DHCPACK (5)",
                    input: 5,
                    expect: ReplyMessageType::Ack,
                },
                Check {
                    scenario: "DHCPNAK (6)",
                    input: 6,
                    expect: ReplyMessageType::Nak,
                },
                Check {
                    scenario: "DHCPDISCOVER (1) is not a reply type",
                    input: 1,
                    expect: ReplyMessageType::Other,
                },
                Check {
                    scenario: "DHCPREQUEST (3) is not a reply type",
                    input: 3,
                    expect: ReplyMessageType::Other,
                },
                Check {
                    scenario: "unknown code buckets as other",
                    input: 250,
                    expect: ReplyMessageType::Other,
                },
            ],
            ReplyMessageType::from,
        );
    }

    /// Verifies Kea's raw DHCPv6 request codes map onto the bounded request
    /// labels exposed by the v6 request metric.
    #[test]
    fn v6_request_message_type_maps_dhcpv6_request_codes_and_buckets_the_rest() {
        check_values(
            [
                // SOLICIT starts DHCPv6 allocation and keeps its own metric label.
                Check {
                    scenario: "SOLICIT (1)",
                    input: 1u8,
                    expect: V6RequestMessageType::Solicit,
                },
                // REQUEST commits an advertised binding and remains independently visible.
                Check {
                    scenario: "REQUEST (3)",
                    input: 3,
                    expect: V6RequestMessageType::Request,
                },
                // CONFIRM checks link validity without being folded into allocation traffic.
                Check {
                    scenario: "CONFIRM (4)",
                    input: 4,
                    expect: V6RequestMessageType::Confirm,
                },
                // RENEW targets the current server and keeps its refresh identity.
                Check {
                    scenario: "RENEW (5)",
                    input: 5,
                    expect: V6RequestMessageType::Renew,
                },
                // REBIND is a distinct refresh after the original server is unavailable.
                Check {
                    scenario: "REBIND (6)",
                    input: 6,
                    expect: V6RequestMessageType::Rebind,
                },
                // RELEASE ends a binding and must remain observable as lease-end traffic.
                Check {
                    scenario: "RELEASE (8)",
                    input: 8,
                    expect: V6RequestMessageType::Release,
                },
                // DECLINE reports an unusable address separately from RELEASE.
                Check {
                    scenario: "DECLINE (9)",
                    input: 9,
                    expect: V6RequestMessageType::Decline,
                },
                // INFORMATION-REQUEST carries only configuration options.
                Check {
                    scenario: "INFORMATION-REQUEST (11)",
                    input: 11,
                    expect: V6RequestMessageType::InformationRequest,
                },
                // RELAY-FORWARD is an envelope and must not impersonate a client request.
                Check {
                    scenario: "RELAY-FORWARD (12) is an outer wire envelope",
                    input: 12,
                    expect: V6RequestMessageType::Other,
                },
                // Unrecognized codes share the bounded fallback instead of adding cardinality.
                Check {
                    scenario: "unknown code buckets as other",
                    input: 250,
                    expect: V6RequestMessageType::Other,
                },
            ],
            V6RequestMessageType::from,
        );
    }

    #[test]
    fn v6_reply_message_type_maps_dhcpv6_response_codes_and_buckets_the_rest() {
        check_values(
            [
                Check {
                    scenario: "ADVERTISE (2)",
                    input: 2u8,
                    expect: V6ReplyMessageType::Advertise,
                },
                Check {
                    scenario: "REPLY (7)",
                    input: 7,
                    expect: V6ReplyMessageType::Reply,
                },
                Check {
                    scenario: "RECONFIGURE (10)",
                    input: 10,
                    expect: V6ReplyMessageType::Reconfigure,
                },
                Check {
                    scenario: "RELAY-REPLY (13) is an outer wire envelope",
                    input: 13,
                    expect: V6ReplyMessageType::Other,
                },
                Check {
                    scenario: "SOLICIT (1) is not a response type",
                    input: 1,
                    expect: V6ReplyMessageType::Other,
                },
                Check {
                    scenario: "unknown code buckets as other",
                    input: 250,
                    expect: V6ReplyMessageType::Other,
                },
            ],
            V6ReplyMessageType::from,
        );
    }

    /// Pins every `reason` label rendering. The strings are the metric's
    /// contract: the long-counted reasons must keep their exact legacy bytes,
    /// and the newer ones must hold still for the same reason.
    #[test]
    fn drop_reason_renders_the_contract_strings_byte_identically() {
        check_values(
            [
                Check {
                    scenario: "non-relayed packet (legacy)",
                    input: DropReason::NonRelayedPacket,
                    expect: "NonRelayedPacket",
                },
                Check {
                    scenario: "no usable IPv4 address (legacy)",
                    input: DropReason::NoUsableIPv4Address,
                    expect: "NoUsableIPv4Address",
                },
                Check {
                    scenario: "discovery success without machine (legacy)",
                    input: DropReason::Success,
                    expect: "Success",
                },
                Check {
                    scenario: "invalid discovery builder pointer (legacy)",
                    input: DropReason::InvalidDiscoveryBuilderPointer,
                    expect: "InvalidDiscoveryBuilderPointer",
                },
                Check {
                    scenario: "invalid mac address (legacy)",
                    input: DropReason::InvalidMacAddress,
                    expect: "InvalidMacAddress",
                },
                Check {
                    scenario: "invalid vendor class (legacy)",
                    input: DropReason::InvalidVendorClass,
                    expect: "InvalidVendorClass",
                },
                Check {
                    scenario: "invalid machine pointer (legacy)",
                    input: DropReason::InvalidMachinePointer,
                    expect: "InvalidMachinePointer",
                },
                Check {
                    scenario: "builder error (legacy)",
                    input: DropReason::BuilderError,
                    expect: "BuilderError",
                },
                Check {
                    scenario: "fetch machine error (legacy)",
                    input: DropReason::FetchMachineError,
                    expect: "FetchMachineError",
                },
                Check {
                    scenario: "invalid circuit id (legacy)",
                    input: DropReason::InvalidCircuitId,
                    expect: "InvalidCircuitId",
                },
                Check {
                    scenario: "too many failures (legacy)",
                    input: DropReason::TooManyFailuresError,
                    expect: "TooManyFailuresError",
                },
                Check {
                    scenario: "invalid desired address (legacy)",
                    input: DropReason::InvalidDesiredAddress,
                    expect: "InvalidDesiredAddress",
                },
                Check {
                    scenario: "option encoding failed (new)",
                    input: DropReason::OptionEncodingFailed,
                    expect: "OptionEncodingFailed",
                },
                Check {
                    scenario: "missing machine context (new)",
                    input: DropReason::MissingMachineContext,
                    expect: "MissingMachineContext",
                },
                Check {
                    scenario: "allocation refused (new)",
                    input: DropReason::AllocationRefused,
                    expect: "AllocationRefused",
                },
                Check {
                    scenario: "renewal refused (new)",
                    input: DropReason::RenewalRefused,
                    expect: "RenewalRefused",
                },
                Check {
                    scenario: "unknown bucket",
                    input: DropReason::Unknown,
                    expect: "Unknown",
                },
            ],
            DropReason::as_label,
        );
    }

    /// The metric label is `as_label` verbatim, for every variant -- the
    /// string table above therefore pins what lands on the exposition.
    #[test]
    fn label_value_renders_as_label_for_every_variant() {
        for reason in DropReason::ALL {
            assert_eq!(
                reason.label_value().as_str(),
                reason.as_label(),
                "{reason:?}"
            );
        }
    }

    /// The pkt4_receive discovery-failure site passes
    /// `discovery_builder_result_as_str(...)` straight through the FFI, so
    /// every result name must map onto the taxonomy and render back to the
    /// same bytes -- a new `DiscoveryBuilderResult` variant that fell into
    /// the `Unknown` bucket would fail here.
    #[test]
    fn drop_reason_round_trips_every_discovery_builder_result_string() {
        for result in [
            DiscoveryBuilderResult::Success,
            DiscoveryBuilderResult::InvalidDiscoveryBuilderPointer,
            DiscoveryBuilderResult::InvalidMacAddress,
            DiscoveryBuilderResult::InvalidVendorClass,
            DiscoveryBuilderResult::InvalidMachinePointer,
            DiscoveryBuilderResult::BuilderError,
            DiscoveryBuilderResult::FetchMachineError,
            DiscoveryBuilderResult::InvalidCircuitId,
            DiscoveryBuilderResult::TooManyFailuresError,
            DiscoveryBuilderResult::InvalidDesiredAddress,
        ] {
            // SAFETY: The result function returns a pointer into a
            // process-lifetime, explicitly NUL-terminated Rust string literal.
            let reason_str = unsafe { CStr::from_ptr(discovery_builder_result_as_str(result)) }
                .to_str()
                .unwrap();
            let reason = DropReason::from(reason_str);
            assert_ne!(reason, DropReason::Unknown, "{result:?} is unmapped");
            assert_eq!(reason.label_value().as_str(), reason_str, "{result:?}");
        }
    }

    #[test]
    fn unmapped_reason_strings_bucket_to_unknown() {
        assert_eq!(DropReason::from("SomeFutureReason"), DropReason::Unknown);
    }

    /// Pins every DHCPv6 `reason` label rendering. These strings come from
    /// `V6HookResult` and must remain the exact bytes tests and dashboards use.
    #[test]
    fn v6_drop_reason_renders_the_contract_strings_byte_identically() {
        check_values(
            [
                Check {
                    scenario: "shared non-relayed packet reason",
                    input: V6DropReason::NonRelayedPacket,
                    expect: "NonRelayedPacket",
                },
                Check {
                    scenario: "success",
                    input: V6DropReason::Success,
                    expect: "success",
                },
                Check {
                    scenario: "ignore",
                    input: V6DropReason::Ignore,
                    expect: "ignore",
                },
                Check {
                    scenario: "confirm not on link",
                    input: V6DropReason::ConfirmNotOnLink,
                    expect: "confirm_not_on_link",
                },
                Check {
                    scenario: "invalid packet",
                    input: V6DropReason::InvalidPacket,
                    expect: "invalid_packet",
                },
                Check {
                    scenario: "nested relay",
                    input: V6DropReason::NestedRelay,
                    expect: "nested_relay",
                },
                Check {
                    scenario: "no MAC and no option 79",
                    input: V6DropReason::NoMacNoOption79,
                    expect: "no_mac_no_option79",
                },
                Check {
                    scenario: "unsupported DUID",
                    input: V6DropReason::UnsupportedDuid,
                    expect: "unsupported_duid",
                },
                Check {
                    scenario: "unsupported message",
                    input: V6DropReason::UnsupportedMessage,
                    expect: "unsupported_message",
                },
                Check {
                    scenario: "invalid machine pointer",
                    input: V6DropReason::InvalidMachinePointer,
                    expect: "invalid_machine_pointer",
                },
                Check {
                    scenario: "fetch machine error",
                    input: V6DropReason::FetchMachineError,
                    expect: "fetch_machine_error",
                },
                Check {
                    scenario: "too many failures",
                    input: V6DropReason::TooManyFailuresError,
                    expect: "too_many_failures_error",
                },
                Check {
                    scenario: "unknown bucket",
                    input: V6DropReason::Unknown,
                    expect: "unknown",
                },
            ],
            V6DropReason::as_label,
        );
    }

    /// Every known v6 FFI reason maps onto a bounded label and renders back
    /// unchanged, so adding a future v6 result cannot silently mint labels.
    #[test]
    fn v6_drop_reason_label_value_renders_as_label_for_every_variant() {
        for reason in V6DropReason::ALL {
            assert_eq!(
                reason.label_value().as_str(),
                reason.as_label(),
                "{reason:?}"
            );
        }
    }

    /// Packet counters are metric-only: one emit moves exactly its
    /// counter -- under the exposed name dashboards use -- and builds no log
    /// line at all (the Kea process has no tracing subscriber; the C++ log
    /// lines remain the log side).
    #[test]
    fn packet_events_count_under_the_exposed_names_and_never_log() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(DhcpRequestReceived);
            emit(DhcpRequestDropped {
                reason: DropReason::TooManyFailuresError,
            });
            emit(DhcpV6RequestReceived {
                message_type: V6RequestMessageType::Confirm,
            });
            emit(DhcpV6RequestDropped {
                reason: V6DropReason::NestedRelay,
            });
            emit(DhcpReplySent {
                message_type: ReplyMessageType::Offer,
            });
            emit(DhcpV6ReplySent {
                message_type: V6ReplyMessageType::Reply,
            });
        });

        assert!(logs.is_empty(), "log = off events must not log: {logs:?}");
        assert_eq!(
            metrics.counter_delta("carbide_dhcp_requests_total", &[]),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_dropped_requests_total",
                &[("reason", "TooManyFailuresError")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_total",
                &[("message_type", "confirm")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_dropped_total",
                &[("reason", "nested_relay")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_replies_sent_total",
                &[("message_type", "offer")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_replies_sent_total",
                &[("message_type", "reply")]
            ),
            1.0
        );
    }

    /// The C++ callouts reach the counters through the FFI layer: reason
    /// strings map onto the bounded taxonomy (unknown ones bucket), the raw
    /// Kea message-type codes map onto their family-specific reply labels,
    /// and nothing records until `metrics_server` has initialized metrics --
    /// the gate that also guarantees the global meter provider is installed
    /// before the first emit.
    #[test]
    fn ffi_increments_gate_on_initialization_and_map_their_arguments() {
        let metrics = MetricsCapture::start();

        // Before initialization the gate keeps every counter still. (Nothing
        // else in this test binary sets `CONFIG.metrics`, so this phase is
        // deterministic.)
        // SAFETY: Every reason argument is a static, NUL-terminated C string;
        // the total-request function has no pointer preconditions.
        unsafe {
            crate::carbide_increment_total_requests();
            crate::carbide_increment_dropped_requests(c"NonRelayedPacket".as_ptr());
            crate::carbide_increment_dropped_v6_requests(c"nested_relay".as_ptr());
        }
        crate::carbide_increment_v6_requests(4);
        crate::carbide_increment_reply_sent(5);
        crate::carbide_increment_v6_reply_sent(7);
        assert_eq!(
            metrics.counter_delta("carbide_dhcp_requests_total", &[]),
            0.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_replies_sent_total",
                &[("message_type", "ack")]
            ),
            0.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_total",
                &[("message_type", "confirm")]
            ),
            0.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_dropped_total",
                &[("reason", "nested_relay")]
            ),
            0.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_replies_sent_total",
                &[("message_type", "reply")]
            ),
            0.0
        );

        // Initialize the way `metrics_server` does (a local registry carries
        // the gauge; the counters resolve from the global test meter).
        let mconf = new_metrics_setup("carbide-dhcp", "forge-system", false).unwrap();
        let initialized = initialize_metrics(&mconf);
        CONFIG.write().expect("config lock poisoned").metrics = Some(initialized);

        // SAFETY: Every reason argument is a static, NUL-terminated C string;
        // the total-request function has no pointer preconditions.
        unsafe {
            crate::carbide_increment_total_requests();
            crate::carbide_increment_dropped_requests(c"NonRelayedPacket".as_ptr());
            crate::carbide_increment_dropped_requests(c"NotAReasonWeKnow".as_ptr());
            crate::carbide_increment_dropped_v6_requests(c"nested_relay".as_ptr());
            crate::carbide_increment_dropped_v6_requests(c"not_a_v6_reason".as_ptr());
        }
        crate::carbide_increment_v6_requests(4); // DHCPV6_CONFIRM
        crate::carbide_increment_reply_sent(5); // DHCPACK
        crate::carbide_increment_v6_reply_sent(7); // DHCPV6_REPLY

        assert_eq!(
            metrics.counter_delta("carbide_dhcp_requests_total", &[]),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_dropped_requests_total",
                &[("reason", "NonRelayedPacket")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_dropped_requests_total",
                &[("reason", "Unknown")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_total",
                &[("message_type", "confirm")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_dropped_total",
                &[("reason", "nested_relay")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_dropped_total",
                &[("reason", "unknown")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_replies_sent_total",
                &[("message_type", "ack")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_replies_sent_total",
                &[("message_type", "reply")]
            ),
            1.0
        );
    }
}
