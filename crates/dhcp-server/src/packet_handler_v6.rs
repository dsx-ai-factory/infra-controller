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

//! DHCPv6 packet decoding and response encoding for the DPU-side server.
//!
//! `dhcproto 0.15` models option 9 exclusively as another `RelayMessage`, but
//! the innermost option 9 contains a normal client `Message`. The shared
//! `carbide-dhcpv6` decoder therefore uses a deliberately small raw-TLV parser,
//! while this server uses dhcproto's typed API for response options.

use std::net::Ipv6Addr;
use std::sync::Arc;

use carbide_dhcpv6::{
    DecodeError as WireDecodeError, DecodedPacket as WirePacket, DuidMac, RELAY_REPLY,
    RelayEnvelope, decode as decode_wire_packet, extract_mac_from_duid, extract_mac_from_option79,
};
use dhcproto::v6::{
    DhcpOption, DhcpOptions, IAAddr, IANA, Message, MessageType, NtpSuboption, OptionCode, Status,
    StatusCode, UnknownOption,
};
use dhcproto::{Encodable, Encoder};
use ipnetwork::Ipv6Network;
use lru::LruCache;
use rpc::forge::{AddressFamily, DhcpDiscovery, MessageKind};
use tokio::sync::Mutex;

use crate::cache::CacheEntry;
use crate::errors::DhcpError;
use crate::metrics::V6ReplyMessageType;
use crate::modes::{DhcpMode, V6Outcome};
use crate::{Config, util};

const DUID_EN: u16 = 2;
const NVIDIA_ENTERPRISE_NUMBER: u32 = 5703;

#[derive(Debug)]
struct DecodedPacketV6 {
    message: Message,
    relay: Option<RelayEnvelope>,
    duid: Vec<u8>,
    duid_mac: DuidMac,
}

/// An encoded DHCPv6 response and the bounded response type used for metrics.
#[derive(Debug)]
pub struct PacketV6 {
    encoded_packet: Vec<u8>,
    pub message_type: V6ReplyMessageType,
}

impl PacketV6 {
    /// Return the wire bytes for the transport responsible for sending this response.
    pub fn encoded_packet(&self) -> &[u8] {
        &self.encoded_packet
    }
}

impl DecodedPacketV6 {
    /// Decode a direct client message or one supported Relay-Forward envelope.
    fn decode(packet: &[u8]) -> Result<Self, DhcpError> {
        let WirePacket { message, relay } = decode_wire_packet(packet).map_err(map_wire_error)?;

        let (duid, duid_mac) = client_identifier(&message)?;

        Ok(Self {
            message,
            relay,
            duid,
            duid_mac,
        })
    }

    /// Resolve the authoritative lookup MAC with trusted relay identity taking precedence.
    fn selected_mac_address(&self) -> Result<String, DhcpError> {
        let duid_mac = match self.duid_mac {
            DuidMac::Mac(mac) => Some(mac),
            DuidMac::NoLinkLayerMac => None,
        };
        let relay_mac = self
            .relay
            .as_ref()
            .and_then(|relay| relay.client_link_layer.as_deref())
            .and_then(extract_mac_from_option79);

        // RFC 6939 identifies the sending link and therefore takes precedence
        // over a DUID that may have been formed from another permanent NIC.
        let selected_mac = match (relay_mac, duid_mac) {
            (Some(relay_mac), Some(duid_mac)) => {
                if relay_mac != duid_mac {
                    tracing::warn!(
                        client_mac_address = %util::u8_to_mac(&relay_mac.bytes()),
                        duid_mac_address = %util::u8_to_mac(&duid_mac.bytes()),
                        "DHCPv6 option 79 MAC disagrees with DUID MAC"
                    );
                }
                relay_mac
            }
            (Some(relay_mac), None) => relay_mac,
            (None, Some(duid_mac)) => duid_mac,
            (None, None) => {
                tracing::warn!(
                    relay_link_ip_address = ?self.relay.as_ref().map(|relay| relay.link_address),
                    "DHCPv6 request has a non-MAC DUID and no RFC 6939 option 79"
                );
                return Err(DhcpError::NoMacNoOption79);
            }
        };

        Ok(util::u8_to_mac(&selected_mac.bytes()))
    }

    /// Return the single IA_NA supported by Carbide's one-address API contract.
    fn ia_na(&self) -> Result<Option<&IANA>, DhcpError> {
        let mut associations = self
            .message
            .opts()
            .get_all(OptionCode::IANA)
            .into_iter()
            .flatten()
            .filter_map(|option| match option {
                DhcpOption::IANA(association) => Some(association),
                _ => None,
            });
        let association = associations.next();
        if associations.next().is_some() {
            Err(DhcpError::InvalidInput(
                "multiple DHCPv6 IA_NA options are unsupported".to_string(),
            ))
        } else {
            Ok(association)
        }
    }

    /// Return the optional single IAADDR hint carried inside the client's IA_NA.
    fn desired_address(&self) -> Result<Option<Ipv6Addr>, DhcpError> {
        let Some(association) = self.ia_na()? else {
            return Ok(None);
        };
        let mut addresses = association
            .opts
            .get_all(OptionCode::IAAddr)
            .into_iter()
            .flatten()
            .filter_map(|option| match option {
                DhcpOption::IAAddr(address) => Some(address.addr),
                _ => None,
            });
        let address = addresses.next();
        if addresses.next().is_some() {
            Err(DhcpError::InvalidInput(
                "multiple DHCPv6 IAADDR options are unsupported".to_string(),
            ))
        } else {
            Ok(address)
        }
    }

    /// Select the circuit identifier used by DPU lookup and API cache routing.
    fn circuit_id(&self, local_interface: Option<&str>) -> Result<Option<String>, DhcpError> {
        // A DPU listener is bound to one host interface, so that ingress name is
        // authoritative even when a client supplies a Relay-Forward envelope.
        if let Some(local_interface) = local_interface {
            return Ok(Some(local_interface.to_string()));
        }

        // Controller mode has no local host.yaml authority and preserves the
        // relay's opaque identifier for API routing.
        Ok(self
            .relay
            .as_ref()
            .and_then(|relay| relay.interface_id.as_deref())
            .map(bytes_to_hex))
    }

    /// Build the family-aware API discovery request for this wire message.
    fn discovery_request(
        &self,
        source_address: Ipv6Addr,
        local_interface: &str,
        message_kind: MessageKind,
        use_local_circuit_id: bool,
    ) -> Result<DhcpDiscovery, DhcpError> {
        // Local DPU lookup is keyed by ingress interface. Only the authoritative
        // controller path needs a MAC for API and cache identity.
        let mac_address = if use_local_circuit_id {
            String::new()
        } else {
            self.selected_mac_address()?
        };
        let relay_address = self
            .relay
            .as_ref()
            .map_or(source_address, |relay| relay.link_address);
        let vendor_string = match self.message.opts().get(OptionCode::VendorClass) {
            Some(DhcpOption::VendorClass(vendor)) => vendor
                .data
                .iter()
                .find_map(|value| std::str::from_utf8(value).ok().map(str::to_owned)),
            _ => None,
        };

        Ok(DhcpDiscovery {
            mac_address,
            relay_address: relay_address.to_string(),
            vendor_string,
            link_address: self
                .relay
                .as_ref()
                .map(|relay| relay.link_address.to_string()),
            circuit_id: self.circuit_id(use_local_circuit_id.then_some(local_interface))?,
            remote_id: self
                .relay
                .as_ref()
                .and_then(|relay| relay.remote_id.as_deref())
                .map(bytes_to_hex),
            desired_address: self.desired_address()?.map(|address| address.to_string()),
            address_family: Some(AddressFamily::V6 as i32),
            message_kind: Some(message_kind as i32),
            duid: Some(self.duid.clone()),
        })
    }
}

/// Process one DHCPv6 request and return no packet when the protocol requires silence.
pub async fn process_packet(
    packet: &[u8],
    source_address: Ipv6Addr,
    config: &Config,
    local_interface: &str,
    handler: &dyn DhcpMode,
    machine_cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
) -> Result<Option<PacketV6>, DhcpError> {
    let decoded = DecodedPacketV6::decode(packet)?;

    let requires_relay = handler.should_be_relayed();
    // Controller mode trusts its validated relay topology; DPU mode trusts
    // only the receiving interface and rejects relay-controlled metadata.
    match (requires_relay, decoded.relay.is_some()) {
        (true, false) => {
            return Err(DhcpError::InvalidInput(
                "controller mode requires a relayed DHCPv6 packet".to_string(),
            ));
        }
        (false, true) => {
            return Err(DhcpError::InvalidInput(
                "DPU mode requires a direct DHCPv6 packet".to_string(),
            ));
        }
        _ => {}
    }
    ensure_server_identifier(&decoded.message, config)?;

    // TODO(dhcpv6-rapid-commit): Milestone 05 may turn SOLICIT directly into
    // REPLY when option 14 is present. This milestone always uses ADVERTISE.
    let wire_type = decoded.message.msg_type();
    let ia_na = decoded.ia_na()?;

    // IA_TA and IA_PD cannot be represented by the single-address API model.
    // Reject them before protocol-local replies can acknowledge unsupported state.
    if let Some(option_code) = [OptionCode::IATA, OptionCode::IAPD]
        .into_iter()
        .find(|option_code| decoded.message.opts().get(*option_code).is_some())
    {
        return Err(DhcpError::UnsupportedAddressAssociationV6(option_code));
    }

    let message_kind = match wire_type {
        MessageType::Solicit if ia_na.is_some() => MessageKind::V6Solicit,
        MessageType::Solicit | MessageType::InformationRequest if ia_na.is_none() => {
            MessageKind::V6InfoRequest
        }
        MessageType::Request | MessageType::Renew | MessageType::Rebind if ia_na.is_some() => {
            MessageKind::V6Request
        }
        MessageType::Confirm => {
            return encode_local_reply(&decoded, config, local_interface);
        }
        MessageType::Release | MessageType::Decline => {
            if ia_na.is_none() {
                return Err(DhcpError::MissingOptionV6(OptionCode::IANA));
            }
            if decoded.desired_address()?.is_none() {
                return Err(DhcpError::MissingOptionV6(OptionCode::IAAddr));
            }
            // Controller mode uses a relay-selected MAC as its authoritative
            // client identity even when the protocol response remains local.
            if requires_relay {
                decoded.selected_mac_address()?;
            }
            // DHCPv6 acknowledges RELEASE/DECLINE with a Reply, unlike the
            // standalone DHCPv4 path. This server has no Kea-style binding
            // database, so Success acknowledges a validly addressed lease-end
            // notification; it does not prove an active (DUID, IAID, address)
            // binding, release the API-owned allocation, or quarantine a
            // declined address.
            return encode_local_reply(&decoded, config, local_interface);
        }
        MessageType::Solicit | MessageType::Request | MessageType::Renew | MessageType::Rebind => {
            return Err(DhcpError::MissingOptionV6(OptionCode::IANA));
        }
        other => return Err(DhcpError::UnhandledMessageTypeV6(other)),
    };

    // Stateful controller discovery can allocate, so reject unusable local
    // lifetime configuration before crossing the API boundary. DPU discovery
    // is read-only and keeps serving address-less bindings without lifetimes.
    if requires_relay && message_kind != MessageKind::V6InfoRequest {
        stateful_lifetimes(config)?;
    }

    // DPU mode keys direct requests by the receiving interface; controller mode
    // forwards only relay-provided circuit identity and never invents one.
    let discovery = decoded.discovery_request(
        source_address,
        local_interface,
        message_kind,
        !requires_relay,
    )?;
    let outcome = handler
        .discover_dhcp_v6(discovery, config, machine_cache)
        .await?;
    let outcome = match (wire_type, outcome) {
        // A refresh is valid only when the client names the authoritative binding;
        // no address or a different address has no binding to renew.
        (MessageType::Renew | MessageType::Rebind, V6Outcome::Stateful(record)) => {
            match decoded.desired_address()? {
                Some(desired) if record.address.parse::<Ipv6Addr>()? == desired => {
                    V6Outcome::Stateful(record)
                }
                _ => V6Outcome::NoAddress,
            }
        }
        (_, outcome) => outcome,
    };
    encode_mode_reply(&decoded, outcome, config).map(Some)
}

/// Return and classify the request's single structurally valid ClientId.
fn client_identifier(message: &Message) -> Result<(Vec<u8>, DuidMac), DhcpError> {
    let mut identifiers = message
        .opts()
        .get_all(OptionCode::ClientId)
        .into_iter()
        .flatten();
    let identifier = identifiers.next();
    if identifiers.next().is_some() {
        return Err(DhcpError::InvalidInput(
            "multiple DHCPv6 ClientId options are unsupported".to_string(),
        ));
    }

    let Some(DhcpOption::ClientId(duid)) = identifier else {
        return Err(DhcpError::MissingOptionV6(OptionCode::ClientId));
    };
    let duid_mac = extract_mac_from_duid(duid).map_err(|_| DhcpError::MalformedDuid)?;
    Ok((duid.clone(), duid_mac))
}

/// Translate policy-neutral wire failures into server packet errors.
fn map_wire_error(error: WireDecodeError) -> DhcpError {
    match error {
        WireDecodeError::MalformedPacket(reason) => {
            DhcpError::InvalidInput(format!("malformed DHCPv6 packet: {reason}"))
        }
        WireDecodeError::ClientMessageDecode(error) => DhcpError::PacketDecodeFailure(error),
        WireDecodeError::MissingRelayMessage => DhcpError::MissingOptionV6(OptionCode::RelayMsg),
        WireDecodeError::NestedRelay => DhcpError::NestedRelayV6,
        WireDecodeError::RelayHopCountExceeded(hop_count) => {
            DhcpError::RelayHopCountExceededV6(hop_count)
        }
        WireDecodeError::UnexpectedRelayReply => {
            DhcpError::UnhandledMessageTypeV6(MessageType::RelayRepl)
        }
    }
}

/// Render opaque relay metadata without introducing lossy UTF-8 collisions.
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// Return the stable DUID-EN used as this server's DHCPv6 identifier.
fn server_identifier(config: &Config) -> Vec<u8> {
    let mut identifier = Vec::with_capacity(10);
    identifier.extend_from_slice(&DUID_EN.to_be_bytes());
    identifier.extend_from_slice(&NVIDIA_ENTERPRISE_NUMBER.to_be_bytes());
    identifier.extend_from_slice(&config.dhcp_config.carbide_dhcp_server.octets());
    identifier
}

/// Return validated lifetimes for a stateful DHCPv6 response.
fn stateful_lifetimes(config: &Config) -> Result<(u32, u32), DhcpError> {
    let preferred_lifetime = config.dhcp_config.dhcpv6_preferred_lifetime_secs;
    let valid_lifetime = config.dhcp_config.dhcpv6_valid_lifetime_secs;
    if preferred_lifetime == 0 || valid_lifetime == 0 || preferred_lifetime > valid_lifetime {
        return Err(DhcpError::InvalidDhcpV6Lifetimes {
            preferred_lifetime_secs: preferred_lifetime,
            valid_lifetime_secs: valid_lifetime,
        });
    }
    Ok((preferred_lifetime, valid_lifetime))
}

/// Reject messages explicitly selecting another server and require selection where mandated.
fn ensure_server_identifier(message: &Message, config: &Config) -> Result<(), DhcpError> {
    let mut identifiers = message
        .opts()
        .get_all(OptionCode::ServerId)
        .into_iter()
        .flatten();
    let received = match identifiers.next() {
        Some(DhcpOption::ServerId(identifier)) => Some(identifier),
        _ => None,
    };
    if identifiers.next().is_some() {
        return Err(DhcpError::InvalidInput(
            "multiple DHCPv6 ServerId options are unsupported".to_string(),
        ));
    }

    if matches!(
        message.msg_type(),
        MessageType::Solicit | MessageType::Confirm | MessageType::Rebind
    ) && received.is_some()
    {
        return Err(DhcpError::InvalidInput(format!(
            "{:?} must not include a DHCPv6 ServerId option",
            message.msg_type()
        )));
    }

    if let Some(received) = received
        && received != &server_identifier(config)
    {
        return Err(DhcpError::NotMyPacket(bytes_to_hex(received)));
    }

    // These exchanges continue a binding selected from an earlier Advertise.
    if matches!(
        message.msg_type(),
        MessageType::Request | MessageType::Renew | MessageType::Release | MessageType::Decline
    ) && received.is_none()
    {
        return Err(DhcpError::MissingOptionV6(OptionCode::ServerId));
    }
    Ok(())
}

/// Encode a mode-backed address or options response.
fn encode_mode_reply(
    request: &DecodedPacketV6,
    outcome: V6Outcome,
    config: &Config,
) -> Result<PacketV6, DhcpError> {
    let reply_type = reply_type_for(request.message.msg_type());
    let mut reply = base_reply(request, reply_type, config);

    // The client owns option-39 negotiation flags; its requested name is never trusted.
    let requested_fqdn_flags = match request.message.opts().get(OptionCode::ClientFqdn) {
        Some(DhcpOption::Unknown(option)) => option.data().first().copied(),
        _ => None,
    };

    match outcome {
        V6Outcome::Stateful(record) => {
            add_config_options(&mut reply, Some(&record.fqdn), requested_fqdn_flags, config)?;
            let association = request
                .ia_na()?
                .ok_or(DhcpError::MissingOptionV6(OptionCode::IANA))?;
            let (preferred_lifetime, valid_lifetime) = stateful_lifetimes(config)?;
            let address = record.address.parse::<Ipv6Addr>()?;
            let mut address_options = DhcpOptions::new();
            address_options.insert(DhcpOption::IAAddr(IAAddr {
                addr: address,
                preferred_life: preferred_lifetime,
                valid_life: valid_lifetime,
                opts: DhcpOptions::new(),
            }));
            reply.opts_mut().insert(DhcpOption::IANA(IANA {
                id: association.id,
                t1: 0,
                t2: 0,
                opts: address_options,
            }));
        }
        V6Outcome::OptionsOnly(record) => {
            add_config_options(&mut reply, Some(&record.fqdn), requested_fqdn_flags, config)?;
        }
        V6Outcome::NoAddress => {
            add_config_options(&mut reply, None, requested_fqdn_flags, config)?;
            let association = request
                .ia_na()?
                .ok_or(DhcpError::MissingOptionV6(OptionCode::IANA))?;
            let status = match request.message.msg_type() {
                MessageType::Solicit | MessageType::Request => Status::NoAddrsAvail,
                MessageType::Renew | MessageType::Rebind => Status::NoBinding,
                other => return Err(DhcpError::UnhandledMessageTypeV6(other)),
            };
            let mut association_options = DhcpOptions::new();
            association_options.insert(status_option(status));
            reply.opts_mut().insert(DhcpOption::IANA(IANA {
                id: association.id,
                t1: 0,
                t2: 0,
                opts: association_options,
            }));
        }
    }

    encode_packet(reply, request.relay.as_ref())
}

/// Encode protocol-local CONFIRM, RELEASE, and DECLINE responses without API mutation.
fn encode_local_reply(
    request: &DecodedPacketV6,
    config: &Config,
    local_interface: &str,
) -> Result<Option<PacketV6>, DhcpError> {
    let status = match request.message.msg_type() {
        MessageType::Confirm => {
            let Some(status) = confirm_status(request, config, local_interface)? else {
                return Ok(None);
            };
            status
        }
        MessageType::Release | MessageType::Decline => Status::Success,
        other => return Err(DhcpError::UnhandledMessageTypeV6(other)),
    };
    let mut reply = base_reply(request, MessageType::Reply, config);
    reply.opts_mut().insert(status_option(status));
    encode_packet(reply, request.relay.as_ref()).map(Some)
}

/// Determine CONFIRM status when this server has authoritative link knowledge.
fn confirm_status(
    request: &DecodedPacketV6,
    config: &Config,
    local_interface: &str,
) -> Result<Option<Status>, DhcpError> {
    let Some(association) = request.ia_na()? else {
        return Ok(None);
    };
    let addresses = association
        .opts
        .iter()
        .filter_map(|option| match option {
            DhcpOption::IAAddr(address) => Some(address.addr),
            _ => None,
        })
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Ok(None);
    }

    // Controller mode has no host.yaml link authority, so CONFIRM remains
    // silent before applying the DPU binding-selection rules below.
    let Some(host_config) = config.host_config.as_ref() else {
        return Ok(None);
    };

    let circuit_id = request
        .circuit_id(Some(local_interface))?
        .unwrap_or_default();
    let prefix = host_config
        .host_ip_addresses
        .get(&circuit_id)
        .and_then(|interface| interface.ipv6.as_ref())
        .and_then(|ipv6| ipv6.prefix.parse::<Ipv6Network>().ok());

    // RFC 8415 requires silence when the server cannot determine whether
    // the addresses belong to the receiving link.
    let Some(prefix) = prefix else {
        return Ok(None);
    };

    Ok(Some(
        if addresses.iter().all(|address| prefix.contains(*address)) {
            Status::Success
        } else {
            Status::NotOnLink
        },
    ))
}

/// Create the common transaction and identity options for one response.
fn base_reply(request: &DecodedPacketV6, message_type: MessageType, config: &Config) -> Message {
    let mut reply = Message::new_with_id(message_type, request.message.xid());
    reply
        .opts_mut()
        .insert(DhcpOption::ClientId(request.duid.clone()));
    reply
        .opts_mut()
        .insert(DhcpOption::ServerId(server_identifier(config)));
    reply
}

/// Append configured service options and request-negotiated API-owned naming options.
fn add_config_options(
    reply: &mut Message,
    fqdn: Option<&str>,
    requested_fqdn_flags: Option<u8>,
    config: &Config,
) -> Result<(), DhcpError> {
    if !config.dhcp_config.carbide_nameservers_v6.is_empty() {
        reply.opts_mut().insert(DhcpOption::DomainNameServers(
            config.dhcp_config.carbide_nameservers_v6.clone(),
        ));
    }
    if !config.dhcp_config.carbide_ntpservers_v6.is_empty() {
        reply.opts_mut().insert(DhcpOption::NtpServer(
            config
                .dhcp_config
                .carbide_ntpservers_v6
                .iter()
                .copied()
                .map(NtpSuboption::ServerAddress)
                .collect(),
        ));
    }
    if let Some(fqdn) = fqdn.filter(|fqdn| !fqdn.is_empty()) {
        let encoded_fqdn = encode_domain_name(fqdn)?;

        // Domain search is API-owned and independent of client-FQDN negotiation.
        if let Some((_, domain)) = fqdn.trim_end_matches('.').split_once('.') {
            reply
                .opts_mut()
                .insert(DhcpOption::Unknown(UnknownOption::new(
                    OptionCode::DomainSearchList,
                    encode_domain_name(domain)?,
                )));
        }

        // Preserve client negotiation flags while replacing its name with the trusted FQDN.
        if let Some(flags) = requested_fqdn_flags {
            let mut client_fqdn = vec![flags];
            client_fqdn.extend(encoded_fqdn);
            reply
                .opts_mut()
                .insert(DhcpOption::Unknown(UnknownOption::new(
                    OptionCode::ClientFqdn,
                    client_fqdn,
                )));
        }
    }
    Ok(())
}

/// Encode one DNS name in uncompressed RFC 1035 wire format for DHCPv6 name options.
fn encode_domain_name(name: &str) -> Result<Vec<u8>, DhcpError> {
    let mut encoded = Vec::new();
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(DhcpError::InvalidInput(format!(
                "invalid DHCPv6 domain-name label in {name}"
            )));
        }
        encoded.push(label.len() as u8);
        encoded.extend_from_slice(label.as_bytes());
    }
    encoded.push(0);
    // The terminating root label counts toward the RFC 1035 255-byte name limit.
    if encoded.len() > 255 {
        return Err(DhcpError::InvalidInput(format!(
            "DHCPv6 domain name exceeds the 255-byte wire limit: {name}"
        )));
    }
    Ok(encoded)
}

/// Build a DHCPv6 Status Code option with no diagnostic text.
fn status_option(status: Status) -> DhcpOption {
    DhcpOption::StatusCode(StatusCode {
        status,
        msg: String::new(),
    })
}

/// Select ADVERTISE for SOLICIT and REPLY for every subsequent exchange.
fn reply_type_for(request_type: MessageType) -> MessageType {
    if request_type == MessageType::Solicit {
        MessageType::Advertise
    } else {
        MessageType::Reply
    }
}

/// Encode the inner response and optionally restore its Relay-Reply envelope.
fn encode_packet(response: Message, relay: Option<&RelayEnvelope>) -> Result<PacketV6, DhcpError> {
    let message_type = if response.msg_type() == MessageType::Advertise {
        V6ReplyMessageType::Advertise
    } else {
        V6ReplyMessageType::Reply
    };
    let mut encoded = Vec::new();
    response.encode(&mut Encoder::new(&mut encoded))?;
    if let Some(relay) = relay {
        encoded = wrap_relay_reply(&encoded, relay)?;
    }
    Ok(PacketV6 {
        encoded_packet: encoded,
        message_type,
    })
}

/// Wrap an encoded client response in the matching one-hop Relay-Reply envelope.
fn wrap_relay_reply(inner_response: &[u8], relay: &RelayEnvelope) -> Result<Vec<u8>, DhcpError> {
    let mut response = Vec::new();
    response.push(RELAY_REPLY);
    response.push(relay.hop_count);
    response.extend_from_slice(&relay.link_address.octets());
    response.extend_from_slice(&relay.peer_address.octets());
    if let Some(interface_id) = &relay.interface_id {
        encode_raw_option(&mut response, OptionCode::InterfaceId, interface_id)?;
    }
    if let Some(remote_id) = &relay.remote_id {
        encode_raw_option(&mut response, OptionCode::RemoteId, remote_id)?;
    }
    encode_raw_option(&mut response, OptionCode::RelayMsg, inner_response)?;
    Ok(response)
}

/// Append one raw DHCPv6 TLV after checking its 16-bit wire length.
fn encode_raw_option(
    packet: &mut Vec<u8>,
    option_code: OptionCode,
    payload: &[u8],
) -> Result<(), DhcpError> {
    let length = u16::try_from(payload.len()).map_err(|_| {
        DhcpError::InvalidInput(format!("DHCPv6 option {option_code:?} exceeds 65535 bytes"))
    })?;
    packet.extend_from_slice(&u16::from(option_code).to_be_bytes());
    packet.extend_from_slice(&length.to_be_bytes());
    packet.extend_from_slice(payload);
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_rpc_utils::dhcp::DhcpConfig;
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};
    use rpc::forge_tls_client::ForgeClientConfig;

    use super::*;

    /// Verifies ClientId extraction accepts one valid option and rejects
    /// missing, malformed, or duplicate options.
    ///
    /// This prevents ambiguous or unusable client identity from reaching
    /// request processing.
    #[test]
    fn client_identifier_requires_one_nonempty_option() {
        let client_id = vec![0, 3, 0, 1, 2, 3, 4, 5, 6, 7];
        let other_client_id = vec![0, 3, 10, 11, 12, 13, 14, 15, 16, 17];

        scenarios!(run = |identifiers| {
                let mut message = Message::new_with_id(MessageType::Solicit, [0, 0, 0]);
                for identifier in identifiers {
                    message.opts_mut().insert(DhcpOption::ClientId(identifier));
                }
                client_identifier(&message)
                    .map(|(duid, _)| duid)
                    .map_err(drop)
            };
            "valid ClientId" {
                // One nonempty ClientId provides an unambiguous client identity.
                vec![client_id.clone()] => Yields(client_id.clone()),
            }
            "missing, malformed, or duplicate ClientId" {
                // An absent ClientId cannot identify a client.
                vec![] => Fails,
                // An empty ClientId is structurally malformed.
                vec![Vec::new()] => Fails,
                // A type-only ClientId lacks the required identifier octet.
                vec![vec![0, 3]] => Fails,
                // A one-byte ClientId cannot contain a DUID type.
                vec![vec![0]] => Fails,
                // Repeating the same ClientId is still ambiguous on the wire.
                vec![client_id.clone(), client_id.clone()] => Fails,
                // Conflicting ClientIds must not depend on option ordering.
                vec![client_id, other_client_id] => Fails,
            }
        );
    }

    /// Verifies each DHCPv6 client message enforces its ServerId presence and
    /// matching policy.
    ///
    /// This prevents forbidden, missing, mismatched, or duplicate server
    /// selection from being accepted.
    #[test]
    fn server_identifier_enforces_message_presence_matrix_and_cardinality() {
        #[derive(Clone, Copy)]
        enum ServerIds {
            Absent,
            ThisServer,
            OtherServer,
            DuplicateSame,
            DuplicateMixed,
        }

        struct Row {
            message_type: MessageType,
            server_ids: ServerIds,
        }

        let config = Config::new(
            DhcpConfig::default(),
            None,
            67,
            ForgeClientConfig::new(String::new(), None),
        );
        let server_id = server_identifier(&config);
        let other_server_id = vec![0xff];

        value_scenarios!(run = |Row { message_type, server_ids }| {
                let mut message = Message::new_with_id(message_type, [0, 0, 0]);
                let identifiers = match server_ids {
                    ServerIds::Absent => vec![],
                    ServerIds::ThisServer => vec![server_id.clone()],
                    ServerIds::OtherServer => vec![other_server_id.clone()],
                    ServerIds::DuplicateSame => vec![server_id.clone(), server_id.clone()],
                    ServerIds::DuplicateMixed => vec![server_id.clone(), other_server_id.clone()],
                };
                for identifier in identifiers {
                    message.opts_mut().insert(DhcpOption::ServerId(identifier));
                }
                ensure_server_identifier(&message, &config).is_ok()
            };
            "SOLICIT" {
                // SOLICIT starts server selection and therefore omits ServerId.
                Row { message_type: MessageType::Solicit, server_ids: ServerIds::Absent } => true,
                // A SOLICIT must not preselect this server.
                Row { message_type: MessageType::Solicit, server_ids: ServerIds::ThisServer } => false,
            }
            "REQUEST" {
                // REQUEST commits the server selected by its matching identifier.
                Row { message_type: MessageType::Request, server_ids: ServerIds::ThisServer } => true,
                // REQUEST without ServerId has not selected a server.
                Row { message_type: MessageType::Request, server_ids: ServerIds::Absent } => false,
                // Another server's identifier must not reach local allocation.
                Row { message_type: MessageType::Request, server_ids: ServerIds::OtherServer } => false,
            }
            "CONFIRM" {
                // CONFIRM asks any server about the current link and omits ServerId.
                Row { message_type: MessageType::Confirm, server_ids: ServerIds::Absent } => true,
                // CONFIRM must remain independent of a previously selected server.
                Row { message_type: MessageType::Confirm, server_ids: ServerIds::ThisServer } => false,
            }
            "RENEW" {
                // RENEW targets the server that owns the existing binding.
                Row { message_type: MessageType::Renew, server_ids: ServerIds::ThisServer } => true,
                // RENEW without ServerId cannot identify its binding owner.
                Row { message_type: MessageType::Renew, server_ids: ServerIds::Absent } => false,
                // A RENEW for another server must not mutate local state.
                Row { message_type: MessageType::Renew, server_ids: ServerIds::OtherServer } => false,
            }
            "REBIND" {
                // REBIND is broadcast after the original server stops responding.
                Row { message_type: MessageType::Rebind, server_ids: ServerIds::Absent } => true,
                // REBIND must not remain pinned to the unavailable server.
                Row { message_type: MessageType::Rebind, server_ids: ServerIds::ThisServer } => false,
            }
            "RELEASE" {
                // RELEASE addresses the server that owns the binding being ended.
                Row { message_type: MessageType::Release, server_ids: ServerIds::ThisServer } => true,
                // RELEASE without ServerId cannot identify the binding owner.
                Row { message_type: MessageType::Release, server_ids: ServerIds::Absent } => false,
                // Another server's RELEASE must not be acknowledged locally.
                Row { message_type: MessageType::Release, server_ids: ServerIds::OtherServer } => false,
            }
            "DECLINE" {
                // DECLINE reports an unusable address to the server that supplied it.
                Row { message_type: MessageType::Decline, server_ids: ServerIds::ThisServer } => true,
                // DECLINE without ServerId cannot identify the address supplier.
                Row { message_type: MessageType::Decline, server_ids: ServerIds::Absent } => false,
                // Another server's declined address is outside local authority.
                Row { message_type: MessageType::Decline, server_ids: ServerIds::OtherServer } => false,
            }
            "INFORMATION-REQUEST" {
                // A normal multicast INFORMATION-REQUEST omits ServerId.
                Row { message_type: MessageType::InformationRequest, server_ids: ServerIds::Absent } => true,
                // A reconfiguration-triggered INFORMATION-REQUEST targets this server.
                Row { message_type: MessageType::InformationRequest, server_ids: ServerIds::ThisServer } => true,
                // A targeted request for another server must be discarded locally.
                Row { message_type: MessageType::InformationRequest, server_ids: ServerIds::OtherServer } => false,
            }
            "duplicate ServerId" {
                // Repeated matching identifiers remain ambiguous and invalid.
                Row { message_type: MessageType::Request, server_ids: ServerIds::DuplicateSame } => false,
                // Conflicting duplicate identifiers must not depend on option ordering.
                Row { message_type: MessageType::Request, server_ids: ServerIds::DuplicateMixed } => false,
            }
        );
    }

    /// Verifies the DNS name's 255-byte limit includes its root but excludes option-39 flags.
    ///
    /// This prevents emitting an overlong name or rejecting a legal 256-byte option payload.
    #[test]
    fn enforces_dns_name_wire_limit_separately_from_client_fqdn_flags() {
        // Three maximum labels consume 192 wire bytes, leaving the final label
        // length, its contents, and the terminating root to establish the boundary.
        let three_maximum_labels = ["a".repeat(63), "b".repeat(63), "c".repeat(63)].join(".");
        let maximum_name = format!("{three_maximum_labels}.{}", "d".repeat(61));
        let overlong_name = format!("{three_maximum_labels}.{}", "d".repeat(62));

        // Keep unrelated options empty so only DNS-name encoding controls the outcome.
        let config = Config::new(
            DhcpConfig::default(),
            None,
            67,
            ForgeClientConfig::new(String::new(), None),
        );

        // Render through the option-39 production path so the flags/name boundary is covered.
        check_cases(
            [
                // The maximum legal DNS name still leaves room for option-39 flags.
                Case {
                    scenario: "255-byte DNS wire name",
                    input: maximum_name,
                    expect: Yields((256, Some(1), Some(0))),
                },
                // One additional DNS octet exceeds the protocol name boundary.
                Case {
                    scenario: "256-byte DNS wire name",
                    input: overlong_name,
                    expect: Fails,
                },
            ],
            |fqdn| {
                let mut reply = Message::new_with_id(MessageType::Reply, [0, 0, 0]);
                add_config_options(&mut reply, Some(&fqdn), Some(1), &config).map_err(drop)?;

                // Return the payload boundary, preserved flags, and terminating root.
                let payload = match reply.opts().get(OptionCode::ClientFqdn) {
                    Some(DhcpOption::Unknown(option)) => option.data(),
                    _ => return Err(()),
                };
                Ok((
                    payload.len(),
                    payload.first().copied(),
                    payload.last().copied(),
                ))
            },
        );
    }
}
