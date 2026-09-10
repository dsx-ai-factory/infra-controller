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

//! Packet-level counters and logs for the DHCP server. Request and reply Events
//! write INFO records with bounded protocol and socket details. DHCPv4 keeps
//! option-rich packets at DEBUG, while DHCPv6 keeps transport direction there.
//! Drop Events count every rejected packet; routine DHCPv6 policy outcomes
//! remain at DEBUG while processing failures write ERROR records.
//! Timestamp-file failures share a counter by operation while their paths,
//! host interface, and errors remain log-only diagnostics.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use carbide_instrument::{DynamicLog, Event, LabelValue, LogAt, MetricFamily, emit};
use dhcproto::v4::MessageType;
use dhcproto::v6::MessageType as MessageTypeV6;

use crate::errors::DhcpError;

/// The DHCP message type of a packet, as a bounded metric label. Named variants
/// cover the v4 and v6 exchanges this server handles; extensions and unknown
/// codes count as `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum MessageTypeLabel {
    Discover,
    Solicit,
    Request,
    Renew,
    Rebind,
    Confirm,
    InformationRequest,
    Offer,
    Advertise,
    Ack,
    Nak,
    Reply,
    Release,
    Decline,
    Inform,
    Other,
}

impl From<MessageTypeV6> for MessageTypeLabel {
    fn from(message_type: MessageTypeV6) -> Self {
        match message_type {
            MessageTypeV6::Solicit => Self::Solicit,
            MessageTypeV6::Request => Self::Request,
            MessageTypeV6::Renew => Self::Renew,
            MessageTypeV6::Rebind => Self::Rebind,
            MessageTypeV6::Confirm => Self::Confirm,
            MessageTypeV6::InformationRequest => Self::InformationRequest,
            MessageTypeV6::Advertise => Self::Advertise,
            MessageTypeV6::Reply => Self::Reply,
            MessageTypeV6::Release => Self::Release,
            MessageTypeV6::Decline => Self::Decline,
            _ => Self::Other,
        }
    }
}

impl From<MessageType> for MessageTypeLabel {
    fn from(message_type: MessageType) -> Self {
        match message_type {
            MessageType::Discover => Self::Discover,
            MessageType::Request => Self::Request,
            MessageType::Offer => Self::Offer,
            MessageType::Ack => Self::Ack,
            MessageType::Nak => Self::Nak,
            MessageType::Release => Self::Release,
            MessageType::Decline => Self::Decline,
            MessageType::Inform => Self::Inform,
            _ => Self::Other,
        }
    }
}

/// Why a DHCPv4 packet was dropped, including receive-loop failures that do not
/// construct a [`DhcpError`]. DHCPv6-only errors map to the closest legacy
/// category only to keep the conversion total; the v6 path uses [`V6DropReason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub enum DropReason {
    RateLimited,
    TooShort,
    NotIpv4,
    SendFailed,
    IoError,
    ConfigParseFailure,
    MissingArgument,
    MissingOption,
    UnhandledMessageType,
    DhcpDeclineMessage,
    MissingRelayCode,
    InvalidInput,
    GenericError,
    UpstreamApiError,
    Utf8Error,
    PacketDecodeFailure,
    PacketEncodeFailure,
    AddressParseError,
    NonRelayedPacket,
    UnknownPacket,
    NotMyPacket,
    VendorClassParseError,
    MultipleInterfaces,
    MissingOptionV6,
    UnhandledMessageTypeV6,
    MalformedDuid,
    NoMacNoOption79,
    NestedRelayV6,
    RelayHopCountExceededV6,
}

impl From<&DhcpError> for DropReason {
    fn from(error: &DhcpError) -> Self {
        match error {
            DhcpError::IoError(_) => Self::IoError,
            DhcpError::SerdeYaml(_) => Self::ConfigParseFailure,
            DhcpError::MissingArgument(_) => Self::MissingArgument,
            DhcpError::MissingOption(_) => Self::MissingOption,
            DhcpError::UnhandledMessageType(_) => Self::UnhandledMessageType,
            DhcpError::DhcpDeclineMessage(_, _) => Self::DhcpDeclineMessage,
            DhcpError::MissingRelayCode(_) => Self::MissingRelayCode,
            DhcpError::InvalidInput(_) => Self::InvalidInput,
            DhcpError::GenericError(_) => Self::GenericError,
            DhcpError::TonicStatusError(_) => Self::UpstreamApiError,
            DhcpError::Utf8Error(_) => Self::Utf8Error,
            DhcpError::PacketDecodeFailure(_) => Self::PacketDecodeFailure,
            DhcpError::PacketEncodeFailure(_) => Self::PacketEncodeFailure,
            DhcpError::AddressParseError(_) => Self::AddressParseError,
            DhcpError::NonRelayedPacket(_) => Self::NonRelayedPacket,
            DhcpError::UnknownPacket(_) => Self::UnknownPacket,
            DhcpError::NotMyPacket(_) => Self::NotMyPacket,
            DhcpError::VendorClassParseError(_) => Self::VendorClassParseError,
            DhcpError::MultipleInterfacesProvidedOneSupported(_) => Self::MultipleInterfaces,
            DhcpError::MissingOptionV6(_) => Self::MissingOptionV6,
            DhcpError::UnhandledMessageTypeV6(_) => Self::UnhandledMessageTypeV6,
            DhcpError::MalformedDuid => Self::MalformedDuid,
            DhcpError::NoIpv6Configuration(_) => Self::MissingArgument,
            DhcpError::InvalidDhcpV6Lifetimes { .. }
            | DhcpError::UnexpectedDhcpV6SourcePort { .. } => Self::InvalidInput,
            DhcpError::UnsupportedAddressAssociationV6(_) => Self::UnhandledMessageTypeV6,
            DhcpError::NoMacNoOption79 => Self::NoMacNoOption79,
            DhcpError::NestedRelayV6 => Self::NestedRelayV6,
            DhcpError::RelayHopCountExceededV6(_) => Self::RelayHopCountExceededV6,
            DhcpError::DhcpV6ApiTimeout(_) => Self::UpstreamApiError,
        }
    }
}

/// Why a DHCPv6 request was dropped or refused before a safe response was possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub enum V6DropReason {
    InvalidPacket,
    /// The request violates a DHCPv6 input or admission constraint.
    InvalidInput,
    /// Local or upstream configuration cannot produce a valid response.
    InvalidConfiguration,
    /// The selected interface intentionally has no IPv6 configuration.
    NoIpv6Configuration,
    /// The request explicitly selected a different DHCPv6 server.
    NotMyPacket,
    /// The client or relay used the other DHCPv6 role's source port.
    UnexpectedSourcePort,
    /// A required DHCPv6 option is absent.
    MissingOption,
    /// The DHCPv6 packet could not be decoded.
    PacketDecodeFailure,
    /// The DHCPv6 response could not be encoded.
    PacketEncodeFailure,
    /// A configured or discovered IPv6 address is malformed.
    AddressParseError,
    /// The request contains an address-association type the server cannot represent.
    UnsupportedAssociation,
    NestedRelay,
    RelayHopCountExceeded,
    NoMacNoOption79,
    MalformedDuid,
    UnsupportedMessage,
    FetchMachineError,
    RateLimited,
    SendFailed,
}

/// Why a DHCPv6 listener became unavailable after bounded socket retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum V6ListenerUnavailableReason {
    InitialSocketSetup,
    SocketRecreation,
}

impl From<&DhcpError> for V6DropReason {
    fn from(error: &DhcpError) -> Self {
        match error {
            DhcpError::InvalidInput(_) => Self::InvalidInput,
            DhcpError::SerdeYaml(_) | DhcpError::MultipleInterfacesProvidedOneSupported(_) => {
                Self::InvalidConfiguration
            }
            DhcpError::InvalidDhcpV6Lifetimes { .. } => Self::InvalidConfiguration,
            DhcpError::NoIpv6Configuration(_) => Self::NoIpv6Configuration,
            DhcpError::NotMyPacket(_) => Self::NotMyPacket,
            DhcpError::UnexpectedDhcpV6SourcePort { .. } => Self::UnexpectedSourcePort,
            DhcpError::MissingOptionV6(_) => Self::MissingOption,
            DhcpError::PacketDecodeFailure(_) => Self::PacketDecodeFailure,
            DhcpError::PacketEncodeFailure(_) => Self::PacketEncodeFailure,
            DhcpError::AddressParseError(_) => Self::AddressParseError,
            DhcpError::UnsupportedAddressAssociationV6(_) => Self::UnsupportedAssociation,
            DhcpError::NestedRelayV6 => Self::NestedRelay,
            DhcpError::RelayHopCountExceededV6(_) => Self::RelayHopCountExceeded,
            DhcpError::NoMacNoOption79 => Self::NoMacNoOption79,
            DhcpError::MalformedDuid => Self::MalformedDuid,
            DhcpError::UnhandledMessageTypeV6(_) => Self::UnsupportedMessage,
            DhcpError::TonicStatusError(_)
            | DhcpError::GenericError(_)
            | DhcpError::DhcpV6ApiTimeout(_) => Self::FetchMachineError,
            DhcpError::IoError(_)
            | DhcpError::MissingArgument(_)
            | DhcpError::MissingOption(_)
            | DhcpError::UnhandledMessageType(_)
            | DhcpError::DhcpDeclineMessage(_, _)
            | DhcpError::MissingRelayCode(_)
            | DhcpError::Utf8Error(_)
            | DhcpError::NonRelayedPacket(_)
            | DhcpError::UnknownPacket(_)
            | DhcpError::VendorClassParseError(_) => Self::InvalidPacket,
        }
    }
}

/// The inner DHCPv6 response type sent directly or inside a relay envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub enum V6ReplyMessageType {
    Advertise,
    Reply,
}

/// The timestamp-file operation that failed. These are the only three file
/// operations performed by the DHCP server, so the metric remains bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum TimestampFileOperation {
    Initialize,
    Write,
    Read,
}

/// `SocketSetupOperation` identifies the bounded syscall or socket option that
/// failed. Interface names, addresses, and error text stay on the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum SocketSetupOperation {
    Create,
    ReuseAddress,
    SetNonblocking,
    BindAddress,
    SetBroadcast,
    BindDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum SocketSetupNextAction {
    Retry,
    Panic,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_dhcp_socket_setup_failures_total",
    kind = counter,
    component = "nico-dhcp",
    describe = "Number of DHCP socket setup failures, by operation and next action."
)]
pub(crate) struct DhcpSocketSetupFailures {
    operation: SocketSetupOperation,
    next_action: SocketSetupNextAction,
}

/// One socket-setup syscall failed. Each variant is the call that failed, and
/// keeps the diagnostic that call already logged.
#[derive(Event)]
#[event(
    event_name = "dhcp_server_socket_setup_failed",
    metric_family = DhcpSocketSetupFailures
)]
pub(crate) enum DhcpSocketSetupFailed {
    #[event(
        labels(operation = SocketSetupOperation::Create),
        log = info,
        message = "Socket creation failed"
    )]
    Create {
        #[label]
        next_action: SocketSetupNextAction,
        #[context(value)]
        retry: i64,
        #[context]
        error: String,
    },

    #[event(
        labels(operation = SocketSetupOperation::ReuseAddress),
        log = info,
        message = "Socket set option failed"
    )]
    ReuseAddress {
        #[label]
        next_action: SocketSetupNextAction,
        #[context(value)]
        retry: i64,
        #[context]
        error: String,
    },

    #[event(
        labels(operation = SocketSetupOperation::SetNonblocking),
        log = info,
        message = "Socket set option failed"
    )]
    SetNonblocking {
        #[label]
        next_action: SocketSetupNextAction,
        #[context(value)]
        retry: i64,
        #[context]
        error: String,
    },

    #[event(
        labels(operation = SocketSetupOperation::BindAddress),
        log = info,
        message = "Socket set option failed"
    )]
    BindAddress {
        #[label]
        next_action: SocketSetupNextAction,
        #[context(value)]
        retry: i64,
        #[context]
        error: String,
    },

    #[event(
        labels(operation = SocketSetupOperation::SetBroadcast),
        log = info,
        message = "Socket set option failed"
    )]
    SetBroadcast {
        #[label]
        next_action: SocketSetupNextAction,
        #[context(value)]
        retry: i64,
        #[context]
        error: String,
    },

    #[event(
        labels(operation = SocketSetupOperation::BindDevice),
        log = info,
        message = "Socket set option failed"
    )]
    BindDevice {
        #[label]
        next_action: SocketSetupNextAction,
        #[context(value)]
        retry: i64,
        #[context]
        error: String,
    },
}

impl DhcpSocketSetupFailed {
    /// Which call failed, with its retry count widened for the log field.
    pub(crate) fn new(
        operation: SocketSetupOperation,
        next_action: SocketSetupNextAction,
        retry: i32,
        error: String,
    ) -> Self {
        let retry = i64::from(retry);
        match operation {
            SocketSetupOperation::Create => Self::Create {
                next_action,
                retry,
                error,
            },
            SocketSetupOperation::ReuseAddress => Self::ReuseAddress {
                next_action,
                retry,
                error,
            },
            SocketSetupOperation::SetNonblocking => Self::SetNonblocking {
                next_action,
                retry,
                error,
            },
            SocketSetupOperation::BindAddress => Self::BindAddress {
                next_action,
                retry,
                error,
            },
            SocketSetupOperation::SetBroadcast => Self::SetBroadcast {
                next_action,
                retry,
                error,
            },
            SocketSetupOperation::BindDevice => Self::BindDevice {
                next_action,
                retry,
                error,
            },
        }
    }
}
#[derive(Event)]
#[event(
    event_name = "dhcp_server_interface_bind_failed",
    metric_family = DhcpSocketSetupFailures,
    log = info,
    message = "Interface not ready, retrying"
)]
pub(crate) struct DhcpInterfaceBindFailed {
    #[label]
    operation: SocketSetupOperation,
    #[label]
    next_action: SocketSetupNextAction,
    #[context(value)]
    interface_name: String,
    #[context(value)]
    retries_left: i64,
    #[context]
    error: String,
}

impl DhcpInterfaceBindFailed {
    pub(crate) fn new(
        next_action: SocketSetupNextAction,
        interface_name: String,
        retries_left: i32,
        error: String,
    ) -> Self {
        Self {
            operation: SocketSetupOperation::BindDevice,
            next_action,
            interface_name,
            retries_left: i64::from(retries_left),
            error,
        }
    }
}

/// A DHCPv4 packet with a usable BOOTP header was decoded from the wire. Its
/// selected header fields and source socket stay on the log line and never
/// become metric labels; the option-rich full packet stays at DEBUG.
#[derive(Event)]
#[event(
    event_name = "dhcp_server_request_received",
    metric_name = "carbide_dhcp_requests_total",
    component = "nico-dhcp",
    log = info,
    metric = counter,
    message = "Decoded DHCP packet",
    describe = "Number of DHCPv4 packets received and decoded, by DHCP message type."
)]
pub(super) struct DhcpRequestReceived {
    #[label]
    pub(super) message_type: MessageTypeLabel,
    #[context(value)]
    pub(super) bootp_op: i64,
    #[context]
    pub(super) source_address: SocketAddr,
    #[context(value)]
    pub(super) xid: i64,
    #[context(value)]
    pub(super) broadcast_flag: bool,
    #[context]
    pub(super) ciaddr: Ipv4Addr,
    #[context]
    pub(super) yiaddr: Ipv4Addr,
    #[context]
    pub(super) siaddr: Ipv4Addr,
    #[context]
    pub(super) giaddr: Ipv4Addr,
    #[context(value)]
    pub(super) chaddr: String,
}

/// A DHCPv4 packet was dropped without a reply reaching the client -- anywhere from
/// the receive loop (rate limiting, undersized or non-IPv4 packets) through
/// packet processing to the final send.
#[derive(Event)]
#[event(
    event_name = "dhcp_server_packet_dropped",
    metric_name = "carbide_dhcp_dropped_requests_total",
    component = "nico-dhcp",
    message = "Dropped a DHCP packet",
    log = error,
    metric = counter,
    describe = "Number of DHCPv4 packets dropped without a reply, by drop reason."
)]
pub struct DhcpPacketDropped {
    #[label]
    pub reason: DropReason,
    /// The detail behind the drop (an error's Display text where one exists).
    /// Log-line-only by construction; never a metric label.
    #[context]
    pub error: String,
}

/// A DHCPv4 reply was sent, labelled by the reply's message type: an `offer` is
/// a proposed lease, an `ack` a committed one, and a `nak` a refusal. Its
/// selected BOOTP header fields and destination stay on the log line only; the
/// option-rich full packet stays at DEBUG.
#[derive(Event)]
#[event(
    event_name = "dhcp_server_reply_sent",
    metric_name = "carbide_dhcp_replies_sent_total",
    component = "nico-dhcp",
    log = info,
    metric = counter,
    message = "Sent DHCP reply",
    describe = "Number of DHCPv4 replies sent, by reply message type."
)]
pub(super) struct DhcpReplySent {
    #[label]
    pub(super) message_type: MessageTypeLabel,
    #[context]
    pub(super) destination_address: SocketAddrV4,
    #[context(value)]
    pub(super) xid: i64,
    #[context(value)]
    pub(super) broadcast_flag: bool,
    #[context]
    pub(super) ciaddr: Ipv4Addr,
    #[context]
    pub(super) yiaddr: Ipv4Addr,
    #[context]
    pub(super) siaddr: Ipv4Addr,
    #[context]
    pub(super) giaddr: Ipv4Addr,
    #[context(value)]
    pub(super) chaddr: String,
}

/// A DHCPv6 datagram reached the standalone listener.
#[derive(Event)]
#[event(
    event_name = "dhcp_server_v6_request_received",
    metric_name = "carbide_dhcp_v6_requests_total",
    component = "nico-dhcp",
    log = info,
    metric = counter,
    message = "Received DHCPv6 request",
    describe = "Number of DHCPv6 requests received, by DHCP message type."
)]
pub(super) struct DhcpV6RequestReceived {
    #[label]
    pub(super) message_type: MessageTypeLabel,
    #[context]
    pub(super) source_address: SocketAddr,
}

/// Count one received DHCPv6 datagram using bounded ingress classification.
pub fn record_v6_request_received(packet: &[u8], source_address: SocketAddr) {
    emit(DhcpV6RequestReceived {
        message_type: MessageTypeLabel::from(carbide_dhcpv6::request_message_type(packet)),
        source_address,
    });
}

/// A DHCPv6 request was rejected by the DPU-side transport.
#[derive(Event)]
#[event(
    event_name = "dhcp_server_v6_request_dropped",
    metric_name = "carbide_dhcp_v6_requests_dropped_total",
    component = "nico-dhcp",
    message = "Dropped a DHCPv6 packet",
    log = dynamic,
    metric = counter,
    describe = "Number of DHCPv6 requests dropped or refused, by reason."
)]
pub struct DhcpV6RequestDropped {
    #[label]
    pub reason: V6DropReason,
    /// The detail behind the drop. This remains log-only and never becomes a
    /// metric label.
    #[context]
    pub error: String,
}

impl DynamicLog for DhcpV6RequestDropped {
    fn log_at(&self) -> LogAt {
        match self.reason {
            // These are expected selection or enablement outcomes, so retain
            // diagnostics without reporting a packet-processing failure.
            V6DropReason::NoIpv6Configuration | V6DropReason::NotMyPacket => {
                LogAt::Level(tracing::Level::DEBUG)
            }
            _ => LogAt::Level(tracing::Level::ERROR),
        }
    }
}

/// A DHCPv6 listener exhausted its bounded socket setup retries.
#[derive(Event)]
#[event(
    event_name = "dhcp_server_v6_listener_unavailable",
    metric_name = "carbide_dhcp_v6_listener_unavailable_total",
    component = "nico-dhcp",
    metric = counter,
    describe = "Number of DHCPv6 listeners made unavailable by socket setup failure, by reason.",
    labels(reason: V6ListenerUnavailableReason),
)]
pub enum DhcpV6ListenerUnavailable {
    /// Initial socket setup failed, so this interface starts without DHCPv6.
    #[event(
        labels(reason = InitialSocketSetup),
        log = warn,
        message = "DHCPv6 listener unavailable"
    )]
    InitialSocketSetup {
        #[context(value)]
        interface_name: String,
        #[context]
        error: String,
    },

    /// A receive failure was followed by unsuccessful socket recreation.
    #[event(
        labels(reason = SocketRecreation),
        log = warn,
        message = "DHCPv6 listener could not be recreated"
    )]
    SocketRecreation {
        #[context(value)]
        interface_name: String,
        #[context]
        error: String,
    },
}

/// A DHCPv6 response was sent, labelled by its inner message type.
#[derive(Event)]
#[event(
    event_name = "dhcp_server_v6_reply_sent",
    metric_name = "carbide_dhcp_v6_replies_sent_total",
    component = "nico-dhcp",
    log = info,
    metric = counter,
    message = "Sent DHCPv6 reply",
    describe = "Number of DHCPv6 replies sent, by response message type."
)]
pub struct DhcpV6ReplySent {
    /// The bounded inner response kind.
    #[label]
    pub message_type: V6ReplyMessageType,
    /// The client or relay socket that received the response.
    #[context]
    pub destination_address: SocketAddrV6,
}

/// A DHCP timestamp-file operation failed. Each variant is one operation, and
/// holds only what that path has -- only a write is per-interface, so only it
/// has a `host_interface_id`.
#[derive(Event)]
#[event(
    event_name = "dhcp_timestamp_file_failed",
    metric_name = "carbide_dhcp_timestamp_file_failures_total",
    component = "nico-dhcp",
    metric = counter,
    describe = "Number of DHCP timestamp file failures, by operation",
    labels(operation: TimestampFileOperation),
)]
pub enum DhcpTimestampFileFailed {
    /// The startup write could not initialize the file, so this server
    /// generation does not start.
    #[event(
        labels(operation = Initialize),
        log = error,
        message = "Failed to init DHCP timestamps file"
    )]
    Initialization {
        #[context]
        dhcp_timestamps_path: String,
        #[context]
        error: String,
    },

    /// The in-memory timestamp advanced but the file did not. Packet
    /// processing continues because the write is best effort.
    #[event(
        labels(operation = Write),
        log = error,
        message = "Failed to write DHCP timestamps file"
    )]
    Write {
        #[context]
        dhcp_timestamps_path: String,
        #[context]
        host_interface_id: String,
        #[context]
        error: String,
    },

    /// The control RPC could not read the file. It still returns an empty list
    /// so callers keep treating an unreadable file as no requests yet.
    #[event(
        labels(operation = Read),
        log = warn,
        message = "Failed to read DHCP timestamps file"
    )]
    Read {
        #[context]
        dhcp_timestamps_path: String,
        #[context]
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    use carbide_instrument::emit;
    use carbide_instrument::testing::{CapturedFieldKind, MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};
    use dhcproto::v4::OptionCode;
    use dhcproto::v4::relay::RelayCode;

    use super::*;

    const SOCKET_SETUP_FAILURE_METRIC: &str = "carbide_dhcp_socket_setup_failures_total";
    const TIMESTAMP_FILE_FAILURE_METRIC: &str = "carbide_dhcp_timestamp_file_failures_total";
    const V6_LISTENER_UNAVAILABLE_METRIC: &str = "carbide_dhcp_v6_listener_unavailable_total";

    struct SocketSetupFailureCase {
        emit: fn(),
        operation: &'static str,
        next_action: &'static str,
    }

    #[derive(Debug, PartialEq)]
    struct SocketSetupFailureObservation {
        counter_delta: f64,
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        operation: Option<String>,
        next_action: Option<String>,
        retry: Option<String>,
        retry_kind: Option<CapturedFieldKind>,
        error: Option<String>,
        error_kind: Option<CapturedFieldKind>,
        interface_name: Option<String>,
        interface_name_kind: Option<CapturedFieldKind>,
        retries_left: Option<String>,
        retries_left_kind: Option<CapturedFieldKind>,
    }

    struct TimestampFileFailureInput {
        emit: fn(),
    }

    #[derive(Debug, PartialEq)]
    struct TimestampFileFailureObservation {
        initialize_delta: f64,
        write_delta: f64,
        read_delta: f64,
        log: TimestampFileFailureLog,
    }

    #[derive(Debug, PartialEq)]
    struct TimestampFileFailureLog {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        operation: Option<String>,
        dhcp_timestamps_path: Option<String>,
        host_interface_id: Option<String>,
        error: Option<String>,
    }

    fn emit_timestamp_initialization_failure() {
        emit(DhcpTimestampFileFailed::Initialization {
            dhcp_timestamps_path: "/var/support/forge-dhcp/logs/dhcp_timestamps.json.tmp"
                .to_string(),
            error: "permission denied".to_string(),
        });
    }

    fn emit_timestamp_write_failure() {
        emit(DhcpTimestampFileFailed::Write {
            dhcp_timestamps_path: "/var/support/forge-dhcp/logs/dhcp_timestamps.json.tmp"
                .to_string(),
            host_interface_id: "60cef902-9779-4666-8362-c9bb4b37185f".to_string(),
            error: "read-only file system".to_string(),
        });
    }

    fn emit_timestamp_read_failure() {
        emit(DhcpTimestampFileFailed::Read {
            dhcp_timestamps_path: "/var/support/forge-dhcp/logs/dhcp_timestamps.json".to_string(),
            error: "file not found".to_string(),
        });
    }

    fn observe_timestamp_file_failure(
        input: TimestampFileFailureInput,
    ) -> TimestampFileFailureObservation {
        let metrics = MetricsCapture::start();
        let mut logs = capture_logs(input.emit);
        assert_eq!(logs.len(), 1, "a timestamp-file failure logs once");
        let log = logs.pop().expect("the timestamp-file failure log");
        let field = |name: &str| log.field(name).map(str::to_owned);

        TimestampFileFailureObservation {
            initialize_delta: metrics.counter_delta(
                TIMESTAMP_FILE_FAILURE_METRIC,
                &[("operation", "initialize")],
            ),
            write_delta: metrics
                .counter_delta(TIMESTAMP_FILE_FAILURE_METRIC, &[("operation", "write")]),
            read_delta: metrics
                .counter_delta(TIMESTAMP_FILE_FAILURE_METRIC, &[("operation", "read")]),
            log: TimestampFileFailureLog {
                level: log.level,
                metadata_name: log.metadata_name.clone(),
                message: log.message.clone(),
                event_name: field("event_name"),
                metric_name: field("metric_name"),
                operation: field("operation"),
                dhcp_timestamps_path: field("dhcp_timestamps_path"),
                host_interface_id: field("host_interface_id"),
                error: field("error"),
            },
        }
    }

    fn expected_timestamp_file_failure(
        operation: &str,
        level: tracing::Level,
        event_name: &str,
        message: &str,
        dhcp_timestamps_path: &str,
        host_interface_id: Option<&str>,
        error: &str,
    ) -> TimestampFileFailureObservation {
        TimestampFileFailureObservation {
            initialize_delta: if operation == "initialize" { 1.0 } else { 0.0 },
            write_delta: if operation == "write" { 1.0 } else { 0.0 },
            read_delta: if operation == "read" { 1.0 } else { 0.0 },
            log: TimestampFileFailureLog {
                level,
                metadata_name: event_name.to_string(),
                message: message.to_string(),
                event_name: Some(event_name.to_string()),
                metric_name: Some(TIMESTAMP_FILE_FAILURE_METRIC.to_string()),
                operation: Some(operation.to_string()),
                dhcp_timestamps_path: Some(dhcp_timestamps_path.to_string()),
                host_interface_id: host_interface_id.map(str::to_owned),
                error: Some(error.to_string()),
            },
        }
    }

    fn observe_socket_setup_failure(
        metrics: &MetricsCapture,
        case: SocketSetupFailureCase,
    ) -> SocketSetupFailureObservation {
        let mut logs = capture_logs(case.emit);
        assert_eq!(logs.len(), 1, "one socket failure logs once");
        let log = logs.pop().expect("the socket setup failure log");
        let field = |name: &str| log.field(name).map(str::to_owned);

        SocketSetupFailureObservation {
            counter_delta: metrics.counter_delta(
                SOCKET_SETUP_FAILURE_METRIC,
                &[
                    ("operation", case.operation),
                    ("next_action", case.next_action),
                ],
            ),
            level: log.level,
            metadata_name: log.metadata_name.clone(),
            message: log.message.clone(),
            event_name: field("event_name"),
            metric_name: field("metric_name"),
            operation: field("operation"),
            next_action: field("next_action"),
            retry: field("retry"),
            retry_kind: log.field_kind("retry"),
            error: field("error"),
            error_kind: log.field_kind("error"),
            interface_name: field("interface_name"),
            interface_name_kind: log.field_kind("interface_name"),
            retries_left: field("retries_left"),
            retries_left_kind: log.field_kind("retries_left"),
        }
    }

    fn expected_socket_setup_failure(
        diagnostic: (&str, &str),
        operation: &str,
        next_action: &str,
        retry: Option<i32>,
        error: Option<&str>,
        interface_name: Option<&str>,
        retries_left: Option<i32>,
    ) -> SocketSetupFailureObservation {
        let (metadata_name, message) = diagnostic;
        SocketSetupFailureObservation {
            counter_delta: 1.0,
            level: tracing::Level::INFO,
            metadata_name: metadata_name.to_string(),
            message: message.to_string(),
            event_name: Some(metadata_name.to_string()),
            metric_name: Some(SOCKET_SETUP_FAILURE_METRIC.to_string()),
            operation: Some(operation.to_string()),
            next_action: Some(next_action.to_string()),
            retry: retry.map(|value| value.to_string()),
            retry_kind: retry.map(|_| CapturedFieldKind::I64),
            error: error.map(str::to_owned),
            error_kind: error.map(|_| CapturedFieldKind::Debug),
            interface_name: interface_name.map(str::to_owned),
            interface_name_kind: interface_name.map(|_| CapturedFieldKind::String),
            retries_left: retries_left.map(|value| value.to_string()),
            retries_left_kind: retries_left.map(|_| CapturedFieldKind::I64),
        }
    }

    #[test]
    fn socket_setup_failures_keep_the_existing_diagnostics() {
        let metrics = MetricsCapture::start();

        check_values(
            [
                Check {
                    scenario: "socket creation will retry",
                    input: SocketSetupFailureCase {
                        emit: || {
                            emit(DhcpSocketSetupFailed::new(
                                SocketSetupOperation::Create,
                                SocketSetupNextAction::Retry,
                                0,
                                "too many open files".to_string(),
                            ))
                        },
                        operation: "create",
                        next_action: "retry",
                    },
                    expect: expected_socket_setup_failure(
                        ("dhcp_server_socket_setup_failed", "Socket creation failed"),
                        "create",
                        "retry",
                        Some(0),
                        Some("too many open files"),
                        None,
                        None,
                    ),
                },
                Check {
                    scenario: "reuse-address setup will retry",
                    input: SocketSetupFailureCase {
                        emit: || {
                            emit(DhcpSocketSetupFailed::new(
                                SocketSetupOperation::ReuseAddress,
                                SocketSetupNextAction::Retry,
                                1,
                                "operation not supported".to_string(),
                            ))
                        },
                        operation: "reuse_address",
                        next_action: "retry",
                    },
                    expect: expected_socket_setup_failure(
                        (
                            "dhcp_server_socket_setup_failed",
                            "Socket set option failed",
                        ),
                        "reuse_address",
                        "retry",
                        Some(1),
                        Some("operation not supported"),
                        None,
                        None,
                    ),
                },
                Check {
                    scenario: "nonblocking setup will retry",
                    input: SocketSetupFailureCase {
                        emit: || {
                            emit(DhcpSocketSetupFailed::new(
                                SocketSetupOperation::SetNonblocking,
                                SocketSetupNextAction::Retry,
                                2,
                                "bad file descriptor".to_string(),
                            ))
                        },
                        operation: "set_nonblocking",
                        next_action: "retry",
                    },
                    expect: expected_socket_setup_failure(
                        (
                            "dhcp_server_socket_setup_failed",
                            "Socket set option failed",
                        ),
                        "set_nonblocking",
                        "retry",
                        Some(2),
                        Some("bad file descriptor"),
                        None,
                        None,
                    ),
                },
                Check {
                    scenario: "address bind will retry",
                    input: SocketSetupFailureCase {
                        emit: || {
                            emit(DhcpSocketSetupFailed::new(
                                SocketSetupOperation::BindAddress,
                                SocketSetupNextAction::Retry,
                                3,
                                "address in use".to_string(),
                            ))
                        },
                        operation: "bind_address",
                        next_action: "retry",
                    },
                    expect: expected_socket_setup_failure(
                        (
                            "dhcp_server_socket_setup_failed",
                            "Socket set option failed",
                        ),
                        "bind_address",
                        "retry",
                        Some(3),
                        Some("address in use"),
                        None,
                        None,
                    ),
                },
                Check {
                    scenario: "broadcast setup exhausts the outer loop",
                    input: SocketSetupFailureCase {
                        emit: || {
                            emit(DhcpSocketSetupFailed::new(
                                SocketSetupOperation::SetBroadcast,
                                SocketSetupNextAction::Panic,
                                9,
                                "permission denied".to_string(),
                            ))
                        },
                        operation: "set_broadcast",
                        next_action: "panic",
                    },
                    expect: expected_socket_setup_failure(
                        (
                            "dhcp_server_socket_setup_failed",
                            "Socket set option failed",
                        ),
                        "set_broadcast",
                        "panic",
                        Some(9),
                        Some("permission denied"),
                        None,
                        None,
                    ),
                },
                Check {
                    scenario: "device bind will retry",
                    input: SocketSetupFailureCase {
                        emit: || {
                            emit(DhcpInterfaceBindFailed::new(
                                SocketSetupNextAction::Retry,
                                "pf0hpf".to_string(),
                                1,
                                "device not found".to_string(),
                            ))
                        },
                        operation: "bind_device",
                        next_action: "retry",
                    },
                    expect: expected_socket_setup_failure(
                        (
                            "dhcp_server_interface_bind_failed",
                            "Interface not ready, retrying",
                        ),
                        "bind_device",
                        "retry",
                        None,
                        Some("device not found"),
                        Some("pf0hpf"),
                        Some(1),
                    ),
                },
                Check {
                    scenario: "device bind exhausts its retry loop",
                    input: SocketSetupFailureCase {
                        emit: || {
                            emit(DhcpInterfaceBindFailed::new(
                                SocketSetupNextAction::Panic,
                                "pf0hpf".to_string(),
                                0,
                                "permission denied".to_string(),
                            ))
                        },
                        operation: "bind_device",
                        next_action: "panic",
                    },
                    expect: expected_socket_setup_failure(
                        (
                            "dhcp_server_interface_bind_failed",
                            "Interface not ready, retrying",
                        ),
                        "bind_device",
                        "panic",
                        None,
                        Some("permission denied"),
                        Some("pf0hpf"),
                        Some(0),
                    ),
                },
            ],
            |case| observe_socket_setup_failure(&metrics, case),
        );
    }

    /// Every timestamp-file failure keeps its historical diagnostic while the
    /// operation label selects exactly one series in the shared counter.
    #[test]
    fn timestamp_file_failures_log_and_count_by_operation() {
        // No other test in this binary triggers a timestamp-file failure. The
        // exact process-global counter deltas below rely on that isolation;
        // keep any future call-site failure test under the same log capture.
        check_values(
            [
                Check {
                    scenario: "initialization failure",
                    input: TimestampFileFailureInput {
                        emit: emit_timestamp_initialization_failure,
                    },
                    expect: expected_timestamp_file_failure(
                        "initialize",
                        tracing::Level::ERROR,
                        "dhcp_timestamp_file_failed",
                        "Failed to init DHCP timestamps file",
                        "/var/support/forge-dhcp/logs/dhcp_timestamps.json.tmp",
                        None,
                        "permission denied",
                    ),
                },
                Check {
                    scenario: "post-reply write failure",
                    input: TimestampFileFailureInput {
                        emit: emit_timestamp_write_failure,
                    },
                    expect: expected_timestamp_file_failure(
                        "write",
                        tracing::Level::ERROR,
                        "dhcp_timestamp_file_failed",
                        "Failed to write DHCP timestamps file",
                        "/var/support/forge-dhcp/logs/dhcp_timestamps.json.tmp",
                        Some("60cef902-9779-4666-8362-c9bb4b37185f"),
                        "read-only file system",
                    ),
                },
                Check {
                    scenario: "read failure",
                    input: TimestampFileFailureInput {
                        emit: emit_timestamp_read_failure,
                    },
                    expect: expected_timestamp_file_failure(
                        "read",
                        tracing::Level::WARN,
                        "dhcp_timestamp_file_failed",
                        "Failed to read DHCP timestamps file",
                        "/var/support/forge-dhcp/logs/dhcp_timestamps.json",
                        None,
                        "file not found",
                    ),
                },
            ],
            observe_timestamp_file_failure,
        );
    }

    #[test]
    fn message_type_label_maps_the_rfc2131_set_and_buckets_the_rest() {
        check_values(
            [
                Check {
                    scenario: "discover",
                    input: MessageType::Discover,
                    expect: MessageTypeLabel::Discover,
                },
                Check {
                    scenario: "request",
                    input: MessageType::Request,
                    expect: MessageTypeLabel::Request,
                },
                Check {
                    scenario: "offer",
                    input: MessageType::Offer,
                    expect: MessageTypeLabel::Offer,
                },
                Check {
                    scenario: "ack",
                    input: MessageType::Ack,
                    expect: MessageTypeLabel::Ack,
                },
                Check {
                    scenario: "nak",
                    input: MessageType::Nak,
                    expect: MessageTypeLabel::Nak,
                },
                Check {
                    scenario: "release",
                    input: MessageType::Release,
                    expect: MessageTypeLabel::Release,
                },
                Check {
                    scenario: "decline",
                    input: MessageType::Decline,
                    expect: MessageTypeLabel::Decline,
                },
                Check {
                    scenario: "inform",
                    input: MessageType::Inform,
                    expect: MessageTypeLabel::Inform,
                },
                Check {
                    scenario: "lease-query extension buckets as other",
                    input: MessageType::LeaseQuery,
                    expect: MessageTypeLabel::Other,
                },
                Check {
                    scenario: "unknown code buckets as other",
                    input: MessageType::Unknown(250),
                    expect: MessageTypeLabel::Other,
                },
            ],
            MessageTypeLabel::from,
        );
    }

    /// Verifies DHCPv6 wire types use bounded labels shared with transport metrics.
    #[test]
    fn message_type_label_maps_the_supported_dhcpv6_set() {
        check_values(
            [
                // SOLICIT retains its allocation-start label.
                Check {
                    scenario: "solicit",
                    input: MessageTypeV6::Solicit,
                    expect: MessageTypeLabel::Solicit,
                },
                // REQUEST retains its allocation-commit label.
                Check {
                    scenario: "request",
                    input: MessageTypeV6::Request,
                    expect: MessageTypeLabel::Request,
                },
                // RENEW remains distinguishable from initial allocation traffic.
                Check {
                    scenario: "renew",
                    input: MessageTypeV6::Renew,
                    expect: MessageTypeLabel::Renew,
                },
                // REBIND remains distinguishable from server-targeted renewal.
                Check {
                    scenario: "rebind",
                    input: MessageTypeV6::Rebind,
                    expect: MessageTypeLabel::Rebind,
                },
                // CONFIRM retains its link-validation label.
                Check {
                    scenario: "confirm",
                    input: MessageTypeV6::Confirm,
                    expect: MessageTypeLabel::Confirm,
                },
                // Information-only traffic keeps its dedicated bounded label.
                Check {
                    scenario: "information request",
                    input: MessageTypeV6::InformationRequest,
                    expect: MessageTypeLabel::InformationRequest,
                },
                // ADVERTISE remains distinguishable from final replies.
                Check {
                    scenario: "advertise",
                    input: MessageTypeV6::Advertise,
                    expect: MessageTypeLabel::Advertise,
                },
                // REPLY retains the common final-response label.
                Check {
                    scenario: "reply",
                    input: MessageTypeV6::Reply,
                    expect: MessageTypeLabel::Reply,
                },
                // RELEASE retains its lease-end label.
                Check {
                    scenario: "release",
                    input: MessageTypeV6::Release,
                    expect: MessageTypeLabel::Release,
                },
                // DECLINE retains its unusable-address label.
                Check {
                    scenario: "decline",
                    input: MessageTypeV6::Decline,
                    expect: MessageTypeLabel::Decline,
                },
                // Relay envelopes are transport wrappers, not client request kinds.
                Check {
                    scenario: "relay forward buckets as other",
                    input: MessageTypeV6::RelayForw,
                    expect: MessageTypeLabel::Other,
                },
            ],
            MessageTypeLabel::from,
        );
    }

    #[test]
    fn drop_reason_covers_every_dhcp_error_variant() {
        // 0x80 is a lone UTF-8 continuation byte -- the decode failure is the
        // point here.
        #[allow(invalid_from_utf8)]
        let utf8_error = std::str::from_utf8(&[0x80]).unwrap_err();

        check_values(
            [
                Check {
                    scenario: "io error",
                    input: DhcpError::IoError(std::io::Error::other("read failed")),
                    expect: DropReason::IoError,
                },
                Check {
                    scenario: "config parse failure",
                    input: DhcpError::SerdeYaml(serde_yaml::from_str::<usize>("[").unwrap_err()),
                    expect: DropReason::ConfigParseFailure,
                },
                Check {
                    scenario: "missing argument",
                    input: DhcpError::MissingArgument("interface".to_string()),
                    expect: DropReason::MissingArgument,
                },
                Check {
                    scenario: "missing option",
                    input: DhcpError::MissingOption(OptionCode::MessageType),
                    expect: DropReason::MissingOption,
                },
                Check {
                    scenario: "unhandled message type",
                    input: DhcpError::UnhandledMessageType(MessageType::Offer),
                    expect: DropReason::UnhandledMessageType,
                },
                Check {
                    scenario: "decline message",
                    input: DhcpError::DhcpDeclineMessage(
                        "10.0.0.1".to_string(),
                        "aa:bb:cc:dd:ee:ff".to_string(),
                    ),
                    expect: DropReason::DhcpDeclineMessage,
                },
                Check {
                    scenario: "missing relay code",
                    input: DhcpError::MissingRelayCode(RelayCode::LinkSelection),
                    expect: DropReason::MissingRelayCode,
                },
                Check {
                    scenario: "invalid input",
                    input: DhcpError::InvalidInput("bad".to_string()),
                    expect: DropReason::InvalidInput,
                },
                Check {
                    scenario: "generic error",
                    input: DhcpError::GenericError("oops".to_string()),
                    expect: DropReason::GenericError,
                },
                Check {
                    scenario: "gRPC failure names the upstream API",
                    input: DhcpError::TonicStatusError(tonic::Status::unavailable("api down")),
                    expect: DropReason::UpstreamApiError,
                },
                Check {
                    scenario: "utf8 error",
                    input: DhcpError::Utf8Error(utf8_error),
                    expect: DropReason::Utf8Error,
                },
                Check {
                    scenario: "packet decode failure",
                    input: DhcpError::PacketDecodeFailure(
                        dhcproto::error::DecodeError::NotEnoughBytes,
                    ),
                    expect: DropReason::PacketDecodeFailure,
                },
                Check {
                    scenario: "packet encode failure",
                    input: DhcpError::PacketEncodeFailure(
                        dhcproto::error::EncodeError::AddOverflow,
                    ),
                    expect: DropReason::PacketEncodeFailure,
                },
                Check {
                    scenario: "address parse error",
                    input: DhcpError::AddressParseError(
                        "not-an-ip".parse::<Ipv4Addr>().unwrap_err(),
                    ),
                    expect: DropReason::AddressParseError,
                },
                Check {
                    scenario: "non-relayed packet",
                    input: DhcpError::NonRelayedPacket(Ipv4Addr::new(0, 0, 0, 0)),
                    expect: DropReason::NonRelayedPacket,
                },
                Check {
                    scenario: "unknown packet",
                    input: DhcpError::UnknownPacket(2),
                    expect: DropReason::UnknownPacket,
                },
                Check {
                    scenario: "not my packet",
                    input: DhcpError::NotMyPacket("10.0.0.2".to_string()),
                    expect: DropReason::NotMyPacket,
                },
                Check {
                    scenario: "vendor class parse error",
                    input: DhcpError::VendorClassParseError("garbled".to_string()),
                    expect: DropReason::VendorClassParseError,
                },
                Check {
                    scenario: "multiple interfaces",
                    input: DhcpError::MultipleInterfacesProvidedOneSupported(2),
                    expect: DropReason::MultipleInterfaces,
                },
                Check {
                    scenario: "missing DHCPv6 option",
                    input: DhcpError::MissingOptionV6(dhcproto::v6::OptionCode::ClientId),
                    expect: DropReason::MissingOptionV6,
                },
                Check {
                    scenario: "unhandled DHCPv6 message",
                    input: DhcpError::UnhandledMessageTypeV6(MessageTypeV6::Advertise),
                    expect: DropReason::UnhandledMessageTypeV6,
                },
                // A structurally invalid DUID remains distinct from an unsupported type.
                Check {
                    scenario: "malformed DUID",
                    input: DhcpError::MalformedDuid,
                    expect: DropReason::MalformedDuid,
                },
                // A v6-disabled interface remains a missing-configuration input to v4 metrics.
                Check {
                    scenario: "missing DHCPv6 configuration",
                    input: DhcpError::NoIpv6Configuration("eth0".to_string()),
                    expect: DropReason::MissingArgument,
                },
                // Invalid v6 lifetimes remain an invalid-input category outside v6 metrics.
                Check {
                    scenario: "invalid DHCPv6 lifetimes",
                    input: DhcpError::InvalidDhcpV6Lifetimes {
                        preferred_lifetime_secs: 0,
                        valid_lifetime_secs: 7200,
                    },
                    expect: DropReason::InvalidInput,
                },
                // The shared v4 taxonomy has no source-port-specific category.
                Check {
                    scenario: "unexpected DHCPv6 source port",
                    input: DhcpError::UnexpectedDhcpV6SourcePort {
                        sender: "client",
                        actual: 547,
                        expected: 546,
                    },
                    expect: DropReason::InvalidInput,
                },
                // Unsupported v6 associations remain within the existing v6 message category.
                Check {
                    scenario: "unsupported DHCPv6 address association",
                    input: DhcpError::UnsupportedAddressAssociationV6(
                        dhcproto::v6::OptionCode::IAPD,
                    ),
                    expect: DropReason::UnhandledMessageTypeV6,
                },
                Check {
                    scenario: "non-MAC DUID without relay identity",
                    input: DhcpError::NoMacNoOption79,
                    expect: DropReason::NoMacNoOption79,
                },
                Check {
                    scenario: "nested DHCPv6 relay",
                    input: DhcpError::NestedRelayV6,
                    expect: DropReason::NestedRelayV6,
                },
                Check {
                    scenario: "DHCPv6 relay hop count exceeded",
                    input: DhcpError::RelayHopCountExceededV6(2),
                    expect: DropReason::RelayHopCountExceededV6,
                },
            ],
            |error| DropReason::from(&error),
        );
    }

    /// Verifies materially different DHCPv6 failures retain bounded operational labels.
    #[test]
    fn v6_drop_reason_preserves_distinct_failure_categories() {
        check_values(
            [
                // Nested relay envelopes remain distinct from malformed client traffic.
                Check {
                    scenario: "nested relay",
                    input: DhcpError::NestedRelayV6,
                    expect: V6DropReason::NestedRelay,
                },
                // Unsupported relay topology retains its dedicated bounded label.
                Check {
                    scenario: "relay hop count exceeded",
                    input: DhcpError::RelayHopCountExceededV6(2),
                    expect: V6DropReason::RelayHopCountExceeded,
                },
                // Missing authoritative identity remains actionable in controller mode.
                Check {
                    scenario: "missing relay identity",
                    input: DhcpError::NoMacNoOption79,
                    expect: V6DropReason::NoMacNoOption79,
                },
                // A malformed DUID is rejected before mode-specific identity selection.
                Check {
                    scenario: "malformed DUID",
                    input: DhcpError::MalformedDuid,
                    expect: V6DropReason::MalformedDuid,
                },
                // A selected interface without IPv6 is an expected enablement outcome.
                Check {
                    scenario: "missing IPv6 configuration",
                    input: DhcpError::NoIpv6Configuration("eth0".to_string()),
                    expect: V6DropReason::NoIpv6Configuration,
                },
                // Invalid stateful lifetimes identify local configuration rather than a bad client.
                Check {
                    scenario: "invalid DHCPv6 lifetimes",
                    input: DhcpError::InvalidDhcpV6Lifetimes {
                        preferred_lifetime_secs: 0,
                        valid_lifetime_secs: 7200,
                    },
                    expect: V6DropReason::InvalidConfiguration,
                },
                // A crossed DHCPv6 role port retains its transport-specific label.
                Check {
                    scenario: "unexpected source port",
                    input: DhcpError::UnexpectedDhcpV6SourcePort {
                        sender: "client",
                        actual: 547,
                        expected: 546,
                    },
                    expect: V6DropReason::UnexpectedSourcePort,
                },
                // Unsupported address-association types are distinct from message policy.
                Check {
                    scenario: "unsupported address association",
                    input: DhcpError::UnsupportedAddressAssociationV6(
                        dhcproto::v6::OptionCode::IAPD,
                    ),
                    expect: V6DropReason::UnsupportedAssociation,
                },
                // Server selection is a normal protocol-policy discard.
                Check {
                    scenario: "packet for another server",
                    input: DhcpError::NotMyPacket("0002".to_string()),
                    expect: V6DropReason::NotMyPacket,
                },
                // Required DHCPv6 options retain a specific bounded reason.
                Check {
                    scenario: "missing option",
                    input: DhcpError::MissingOptionV6(dhcproto::v6::OptionCode::ClientId),
                    expect: V6DropReason::MissingOption,
                },
                // Wire decoding failures remain distinct from policy rejection.
                Check {
                    scenario: "packet decode failure",
                    input: DhcpError::PacketDecodeFailure(
                        dhcproto::error::DecodeError::NotEnoughBytes,
                    ),
                    expect: V6DropReason::PacketDecodeFailure,
                },
                // Response encoding failures identify a server-side packet failure.
                Check {
                    scenario: "packet encode failure",
                    input: DhcpError::PacketEncodeFailure(
                        dhcproto::error::EncodeError::AddOverflow,
                    ),
                    expect: V6DropReason::PacketEncodeFailure,
                },
                // API or configuration address text failures retain their own category.
                Check {
                    scenario: "address parse failure",
                    input: DhcpError::AddressParseError(
                        "not-an-ip".parse::<Ipv4Addr>().unwrap_err(),
                    ),
                    expect: V6DropReason::AddressParseError,
                },
                // Unsupported DHCPv6 exchanges remain distinct from malformed packets.
                Check {
                    scenario: "unsupported message",
                    input: DhcpError::UnhandledMessageTypeV6(MessageTypeV6::RelayForw),
                    expect: V6DropReason::UnsupportedMessage,
                },
                // A typed gRPC status identifies controller lookup failure.
                Check {
                    scenario: "API status failure",
                    input: DhcpError::TonicStatusError(tonic::Status::unavailable("api down")),
                    expect: V6DropReason::FetchMachineError,
                },
                // A non-status API error belongs to the same bounded lookup-failure family.
                Check {
                    scenario: "generic API failure",
                    input: DhcpError::GenericError("api transport failed".to_string()),
                    expect: V6DropReason::FetchMachineError,
                },
                // A bounded Controller deadline remains an upstream lookup failure.
                Check {
                    scenario: "API deadline exceeded",
                    input: DhcpError::DhcpV6ApiTimeout(Duration::from_secs(5)),
                    expect: V6DropReason::FetchMachineError,
                },
                // Unclassified input errors retain the generic malformed-packet label.
                Check {
                    scenario: "ordinary malformed packet",
                    input: DhcpError::InvalidInput("bad packet".to_string()),
                    expect: V6DropReason::InvalidInput,
                },
            ],
            |error| V6DropReason::from(&error),
        );
    }

    /// Verifies final DHCPv6 socket failures retain their distinct bounded
    /// reasons and diagnostic warning context.
    #[test]
    fn v6_listener_unavailability_counts_each_reason_and_logs_diagnostics() {
        let metrics = MetricsCapture::start();

        // Emit one final failure from each listener lifecycle boundary.
        let logs = capture_logs(|| {
            emit(DhcpV6ListenerUnavailable::InitialSocketSetup {
                interface_name: "eth0".to_string(),
                error: "address unavailable".to_string(),
            });
            emit(DhcpV6ListenerUnavailable::SocketRecreation {
                interface_name: "eth1".to_string(),
                error: "device disappeared".to_string(),
            });
        });

        // Both reasons have independent counters and preserve their warnings.
        assert_eq!(
            metrics.counter_delta(
                V6_LISTENER_UNAVAILABLE_METRIC,
                &[("reason", "initial_socket_setup")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                V6_LISTENER_UNAVAILABLE_METRIC,
                &[("reason", "socket_recreation")]
            ),
            1.0
        );
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].level, tracing::Level::WARN);
        assert_eq!(logs[0].message, "DHCPv6 listener unavailable");
        assert_eq!(logs[0].field("interface_name"), Some("eth0"));
        assert_eq!(logs[0].field("error"), Some("address unavailable"));
        assert_eq!(logs[1].level, tracing::Level::WARN);
        assert_eq!(logs[1].message, "DHCPv6 listener could not be recreated");
        assert_eq!(logs[1].field("interface_name"), Some("eth1"));
        assert_eq!(logs[1].field("error"), Some("device disappeared"));
    }

    /// Expected DHCPv6 selection and enablement drops remain diagnosable
    /// without appearing as packet-processing errors at the default log level.
    #[test]
    fn expected_v6_drops_log_at_debug() {
        // Exercise the two routine outcomes that are counted but not operational failures.
        let logs = capture_logs(|| {
            emit(DhcpV6RequestDropped {
                reason: V6DropReason::NoIpv6Configuration,
                error: "no DHCPv6 configuration for interface eth0".to_string(),
            });
            emit(DhcpV6RequestDropped {
                reason: V6DropReason::NotMyPacket,
                error: "packet received for other server: 0002".to_string(),
            });
        });

        // Both outcomes retain bounded context at DEBUG rather than ERROR.
        assert_eq!(logs.len(), 2);
        assert!(
            logs.iter()
                .all(|entry| entry.level == tracing::Level::DEBUG)
        );
        assert_eq!(logs[0].field("reason"), Some("no_ipv6_configuration"));
        assert_eq!(logs[1].field("reason"), Some("not_my_packet"));
    }

    /// Every packet Event moves exactly its counter. Both families log bounded
    /// request and reply activity at INFO, while processing drops log at ERROR.
    #[test]
    fn packet_events_count_per_label_and_log_at_their_operational_level() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            // Labels deliberately unused by any other test in this binary:
            // the capture mutex serializes only capture-holding tests, so
            // shared labels (a Discover request, an Offer grant) would race
            // with the end-to-end packet tests.
            emit(DhcpRequestReceived {
                message_type: MessageTypeLabel::Release,
                bootp_op: 1,
                source_address: "192.0.2.10:68".parse().unwrap(),
                xid: 0x0102_0304,
                broadcast_flag: true,
                ciaddr: Ipv4Addr::new(192, 0, 2, 20),
                yiaddr: Ipv4Addr::new(192, 0, 2, 21),
                siaddr: Ipv4Addr::new(192, 0, 2, 1),
                giaddr: Ipv4Addr::new(192, 0, 2, 254),
                chaddr: "00:11:22:33:44:55".to_string(),
            });
            emit(DhcpRequestReceived {
                message_type: MessageTypeLabel::Release,
                bootp_op: 1,
                source_address: "192.0.2.10:68".parse().unwrap(),
                xid: 0x0102_0304,
                broadcast_flag: true,
                ciaddr: Ipv4Addr::new(192, 0, 2, 20),
                yiaddr: Ipv4Addr::new(192, 0, 2, 21),
                siaddr: Ipv4Addr::new(192, 0, 2, 1),
                giaddr: Ipv4Addr::new(192, 0, 2, 254),
                chaddr: "00:11:22:33:44:55".to_string(),
            });
            emit(DhcpReplySent {
                message_type: MessageTypeLabel::Nak,
                destination_address: "192.0.2.10:68".parse().unwrap(),
                xid: 0x0102_0304,
                broadcast_flag: true,
                ciaddr: Ipv4Addr::new(192, 0, 2, 20),
                yiaddr: Ipv4Addr::new(192, 0, 2, 21),
                siaddr: Ipv4Addr::new(192, 0, 2, 1),
                giaddr: Ipv4Addr::new(192, 0, 2, 254),
                chaddr: "00:11:22:33:44:55".to_string(),
            });
            emit(DhcpV6RequestReceived {
                message_type: MessageTypeLabel::Confirm,
                source_address: "[fe80::10]:546".parse().unwrap(),
            });
            emit(DhcpV6ReplySent {
                message_type: V6ReplyMessageType::Reply,
                destination_address: "[fe80::10]:546".parse().unwrap(),
            });
            emit(DhcpPacketDropped {
                reason: DropReason::RateLimited,
                error: "parallel packet handling limit reached".to_string(),
            });
            let error = DhcpError::TonicStatusError(tonic::Status::unavailable("api down"));
            emit(DhcpPacketDropped {
                reason: DropReason::from(&error),
                error: error.to_string(),
            });
            emit(DhcpV6RequestDropped {
                reason: V6DropReason::RateLimited,
                error: "v6 parallel packet handling limit reached".to_string(),
            });
        });

        assert_eq!(logs.len(), 8, "each logging packet Event writes one record");
        let request_logs: Vec<_> = logs
            .iter()
            .filter(|entry| entry.metadata_name == "dhcp_server_request_received")
            .collect();
        let reply_logs: Vec<_> = logs
            .iter()
            .filter(|entry| entry.metadata_name == "dhcp_server_reply_sent")
            .collect();
        let v6_request_logs: Vec<_> = logs
            .iter()
            .filter(|entry| entry.metadata_name == "dhcp_server_v6_request_received")
            .collect();
        let v6_reply_logs: Vec<_> = logs
            .iter()
            .filter(|entry| entry.metadata_name == "dhcp_server_v6_reply_sent")
            .collect();
        let drop_logs: Vec<_> = logs
            .iter()
            .filter(|entry| {
                entry.metadata_name == "dhcp_server_packet_dropped"
                    || entry.metadata_name == "dhcp_server_v6_request_dropped"
            })
            .collect();

        assert_eq!(request_logs.len(), 2, "each request writes one INFO line");
        for entry in request_logs {
            assert_eq!(entry.level, tracing::Level::INFO);
            assert_eq!(entry.message, "Decoded DHCP packet");
            assert_eq!(
                entry.field("event_name"),
                Some("dhcp_server_request_received")
            );
            assert_eq!(
                entry.field("metric_name"),
                Some("carbide_dhcp_requests_total")
            );
            assert_eq!(entry.field("message_type"), Some("release"));
            assert_eq!(entry.field("bootp_op"), Some("1"));
            assert_eq!(entry.field_kind("bootp_op"), Some(CapturedFieldKind::I64));
            assert_eq!(entry.field("source_address"), Some("192.0.2.10:68"));
            assert_eq!(entry.field("xid"), Some("16909060"));
            assert_eq!(entry.field_kind("xid"), Some(CapturedFieldKind::I64));
            assert_eq!(entry.field("broadcast_flag"), Some("true"));
            assert_eq!(
                entry.field_kind("broadcast_flag"),
                Some(CapturedFieldKind::Bool)
            );
            assert_eq!(entry.field("ciaddr"), Some("192.0.2.20"));
            assert_eq!(entry.field("yiaddr"), Some("192.0.2.21"));
            assert_eq!(entry.field("siaddr"), Some("192.0.2.1"));
            assert_eq!(entry.field("giaddr"), Some("192.0.2.254"));
            assert_eq!(entry.field("chaddr"), Some("00:11:22:33:44:55"));
            assert_eq!(entry.field_kind("chaddr"), Some(CapturedFieldKind::String));
            assert_eq!(entry.field("received_packet"), None);
        }

        let [reply_log] = reply_logs.as_slice() else {
            panic!("the reply Event should write one INFO line, got {reply_logs:?}");
        };
        assert_eq!(reply_log.level, tracing::Level::INFO);
        assert_eq!(reply_log.message, "Sent DHCP reply");
        assert_eq!(
            reply_log.field("event_name"),
            Some("dhcp_server_reply_sent")
        );
        assert_eq!(
            reply_log.field("metric_name"),
            Some("carbide_dhcp_replies_sent_total")
        );
        assert_eq!(reply_log.field("message_type"), Some("nak"));
        assert_eq!(
            reply_log.field("destination_address"),
            Some("192.0.2.10:68")
        );
        assert_eq!(reply_log.field("xid"), Some("16909060"));
        assert_eq!(reply_log.field_kind("xid"), Some(CapturedFieldKind::I64));
        assert_eq!(reply_log.field("broadcast_flag"), Some("true"));
        assert_eq!(
            reply_log.field_kind("broadcast_flag"),
            Some(CapturedFieldKind::Bool)
        );
        assert_eq!(reply_log.field("ciaddr"), Some("192.0.2.20"));
        assert_eq!(reply_log.field("yiaddr"), Some("192.0.2.21"));
        assert_eq!(reply_log.field("siaddr"), Some("192.0.2.1"));
        assert_eq!(reply_log.field("giaddr"), Some("192.0.2.254"));
        assert_eq!(reply_log.field("chaddr"), Some("00:11:22:33:44:55"));
        assert_eq!(
            reply_log.field_kind("chaddr"),
            Some(CapturedFieldKind::String)
        );
        assert_eq!(reply_log.field("sent_packet"), None);

        let [v6_request_log] = v6_request_logs.as_slice() else {
            panic!("the DHCPv6 request Event should write one INFO line, got {v6_request_logs:?}");
        };
        assert_eq!(v6_request_log.level, tracing::Level::INFO);
        assert_eq!(v6_request_log.message, "Received DHCPv6 request");
        assert_eq!(v6_request_log.field("message_type"), Some("confirm"));
        assert_eq!(
            v6_request_log.field("source_address"),
            Some("[fe80::10]:546")
        );

        let [v6_reply_log] = v6_reply_logs.as_slice() else {
            panic!("the DHCPv6 reply Event should write one INFO line, got {v6_reply_logs:?}");
        };
        assert_eq!(v6_reply_log.level, tracing::Level::INFO);
        assert_eq!(v6_reply_log.message, "Sent DHCPv6 reply");
        assert_eq!(v6_reply_log.field("message_type"), Some("reply"));
        assert_eq!(
            v6_reply_log.field("destination_address"),
            Some("[fe80::10]:546")
        );

        assert_eq!(drop_logs.len(), 3, "each drop writes one error line");
        assert!(
            drop_logs
                .iter()
                .all(|entry| entry.level == tracing::Level::ERROR)
        );
        assert_eq!(drop_logs[0].field("reason"), Some("rate_limited"));
        assert!(
            drop_logs[1]
                .field("error")
                .is_some_and(|error| error.contains("api down")),
            "the drop line carries the upstream error detail"
        );
        assert!(
            drop_logs[2]
                .field("error")
                .is_some_and(|error| error.contains("v6 parallel")),
            "the v6 drop line carries its diagnostic detail"
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_requests_total",
                &[("message_type", "release")]
            ),
            2.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_replies_sent_total",
                &[("message_type", "nak")]
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
                "carbide_dhcp_v6_replies_sent_total",
                &[("message_type", "reply")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_dropped_requests_total",
                &[("reason", "rate_limited")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_dropped_total",
                &[("reason", "rate_limited")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_dropped_requests_total",
                &[("reason", "upstream_api_error")]
            ),
            1.0
        );
    }
}
