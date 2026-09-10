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

//! Policy-neutral DHCPv6 wire decoding shared by the Kea hook and standalone server.
//!
//! `dhcproto 0.15` models relay option 9 as another relay message even when its
//! payload is an ordinary client message. This crate therefore performs a
//! narrow raw-TLV pass over one relay envelope and delegates client-message
//! decoding to `dhcproto`.

use std::net::Ipv6Addr;

use dhcproto::v6::{DhcpOption, Message, MessageType, OptionCode, UnknownOption};
use dhcproto::{Decodable, Decoder, Encodable, Encoder};
use mac_address::MacAddress;

/// DHCPv6 message type for a Relay-Forward envelope.
pub const RELAY_FORWARD: u8 = 12;

/// DHCPv6 message type for a Relay-Reply envelope.
pub const RELAY_REPLY: u8 = 13;

const DUID_LLT: u16 = 1;
const DUID_EN: u16 = 2;
const DUID_LL: u16 = 3;
const DUID_UUID: u16 = 4;
const HARDWARE_TYPE_ETHERNET: u16 = 1;
const ETHERNET_MAC_LENGTH: usize = 6;
const DUID_EN_MIN_LENGTH: usize = 7;
const DUID_TYPE_LENGTH: usize = 2;
const DUID_MIN_LENGTH: usize = DUID_TYPE_LENGTH + 1;
/// Maximum complete DUID length permitted by RFC 9915.
pub const DUID_MAX_LENGTH: usize = DUID_TYPE_LENGTH + 128;
const DUID_UUID_LENGTH: usize = 18;

/// Metadata retained from one supported DHCPv6 Relay-Forward envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEnvelope {
    pub hop_count: u8,
    pub link_address: Ipv6Addr,
    pub peer_address: Ipv6Addr,
    pub interface_id: Option<Vec<u8>>,
    pub remote_id: Option<Vec<u8>>,
    pub client_link_layer: Option<Vec<u8>>,
}

/// A decoded client message and its optional one-hop relay envelope.
#[derive(Debug)]
pub struct DecodedPacket {
    pub message: Message,
    pub relay: Option<RelayEnvelope>,
}

/// Structural failures encountered before serving policy is applied.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("malformed DHCPv6 packet: {0}")]
    MalformedPacket(&'static str),
    #[error("DHCPv6 decode failed: {0}")]
    ClientMessageDecode(#[source] dhcproto::error::DecodeError),
    #[error("missing DHCPv6 relay-message option")]
    MissingRelayMessage,
    #[error("nested DHCPv6 relay packet")]
    NestedRelay,
    #[error("unsupported DHCPv6 relay hop count {0}")]
    RelayHopCountExceeded(u8),
    #[error("unexpected DHCPv6 relay-reply request")]
    UnexpectedRelayReply,
}

/// Result of parsing a DUID for an Ethernet identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuidMac {
    Mac(MacAddress),
    NoLinkLayerMac,
}

/// Why generic DUID structural validation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuidError {
    /// The DUID violates its structural or length constraints.
    Malformed,
}

// These are defensive service limits rather than DHCPv6 wire-format limits.
// Legitimate messages use shallow option trees with far fewer options.
const MAX_CLIENT_OPTION_NESTING_DEPTH: usize = 8;
const MAX_CLIENT_OPTIONS: usize = 256;

#[derive(Debug)]
struct RawOption<'data> {
    code: u16,
    data: &'data [u8],
    encoded: &'data [u8],
}

/// Decode one direct client message or one supported Relay-Forward envelope.
pub fn decode(packet: &[u8]) -> Result<DecodedPacket, DecodeError> {
    match packet.first().copied() {
        Some(RELAY_FORWARD) => decode_relay_forward(packet),
        Some(RELAY_REPLY) => Err(DecodeError::UnexpectedRelayReply),
        Some(_) => Ok(DecodedPacket {
            message: decode_client_message(packet)?,
            relay: None,
        }),
        None => Err(DecodeError::MalformedPacket("empty packet")),
    }
}

/// Classify the client message type with bounded work for ingress metrics.
///
/// Direct messages use their first octet. For one Relay-Forward envelope, the
/// first relay-message option supplies the client type. Malformed, empty, or
/// excessively option-rich relay envelopes retain the outer relay type and are
/// therefore reported through the caller's bounded `other` label.
pub fn request_message_type(packet: &[u8]) -> MessageType {
    let outer_type = MessageType::from(packet.first().copied().unwrap_or_default());
    if outer_type != MessageType::RelayForw {
        return outer_type;
    }

    relay_message_type(packet).unwrap_or(outer_type)
}

/// Find the encapsulated client type without allocating or fully decoding.
fn relay_message_type(packet: &[u8]) -> Option<MessageType> {
    let mut options = packet.get(34..)?;
    for _ in 0..MAX_CLIENT_OPTIONS {
        if options.is_empty() {
            return None;
        }
        let header = options.get(..4)?;
        let option_code = u16::from_be_bytes([header[0], header[1]]);
        let payload_length = u16::from_be_bytes([header[2], header[3]]) as usize;
        let encoded_length = 4usize.checked_add(payload_length)?;
        let payload = options.get(4..encoded_length)?;
        if OptionCode::from(option_code) == OptionCode::RelayMsg {
            return payload.first().copied().map(MessageType::from);
        }
        options = options.get(encoded_length..)?;
    }
    None
}

/// Decode an ordinary client message after validating all option TLV boundaries.
pub fn decode_client_message(packet: &[u8]) -> Result<Message, DecodeError> {
    let options = packet
        .get(4..)
        .ok_or(DecodeError::MalformedPacket("truncated client header"))?;
    let options = parse_raw_options(options)?;

    // dhcproto stops at the first undecodable option, so validate each option
    // independently before allowing its lossy option-list decoder to run.
    validate_client_options(&options)?;

    // dhcproto incorrectly interprets vendor-private suboption codes as global
    // DHCPv6 option codes. Decode the standard options without option 17, then
    // retain each validated vendor payload as an opaque option.
    let sanitized_packet = options
        .iter()
        .any(|option| OptionCode::from(option.code) == OptionCode::VendorOpts)
        .then(|| {
            let mut sanitized = Vec::with_capacity(packet.len());
            sanitized.extend_from_slice(&packet[..4]);
            for option in &options {
                if OptionCode::from(option.code) != OptionCode::VendorOpts {
                    sanitized.extend_from_slice(option.encoded);
                }
            }
            sanitized
        });
    let decode_packet = sanitized_packet.as_deref().unwrap_or(packet);
    let mut message = Message::decode(&mut Decoder::new(decode_packet))
        .map_err(DecodeError::ClientMessageDecode)?;

    for option in options
        .iter()
        .filter(|option| OptionCode::from(option.code) == OptionCode::VendorOpts)
    {
        message
            .opts_mut()
            .insert(DhcpOption::Unknown(UnknownOption::new(
                OptionCode::VendorOpts,
                option.data.to_vec(),
            )));
    }
    Ok(message)
}

/// Extract an Ethernet MAC from a supported DUID, or classify a valid non-MAC DUID.
pub fn extract_mac_from_duid(duid: &[u8]) -> Result<DuidMac, DuidError> {
    // RFC 9915 requires a two-octet type followed by 1 through 128 octets.
    if !(DUID_MIN_LENGTH..=DUID_MAX_LENGTH).contains(&duid.len()) {
        return Err(DuidError::Malformed);
    }

    let duid_type = u16::from_be_bytes([duid[0], duid[1]]);
    match duid_type {
        DUID_LLT => parse_link_layer_duid(&duid[2..], 4),
        DUID_LL => parse_link_layer_duid(&duid[2..], 0),
        DUID_EN if duid.len() >= DUID_EN_MIN_LENGTH => Ok(DuidMac::NoLinkLayerMac),
        DUID_UUID if duid.len() == DUID_UUID_LENGTH => Ok(DuidMac::NoLinkLayerMac),
        DUID_EN | DUID_UUID => Err(DuidError::Malformed),
        _ => Ok(DuidMac::NoLinkLayerMac),
    }
}

/// Parse RFC 6939 option 79 and return its Ethernet MAC when supported.
pub fn extract_mac_from_option79(payload: &[u8]) -> Option<MacAddress> {
    if payload.len() != 2 + ETHERNET_MAC_LENGTH {
        return None;
    }

    let hardware_type = u16::from_be_bytes([payload[0], payload[1]]);
    (hardware_type == HARDWARE_TYPE_ETHERNET).then(|| {
        MacAddress::new([
            payload[2], payload[3], payload[4], payload[5], payload[6], payload[7],
        ])
    })
}

/// Decode one Relay-Forward envelope and its direct client payload.
fn decode_relay_forward(packet: &[u8]) -> Result<DecodedPacket, DecodeError> {
    if packet.len() < 34 {
        return Err(DecodeError::MalformedPacket("truncated relay header"));
    }

    let hop_count = packet[1];
    // A relay forwarding a direct client message uses zero; a nonzero value
    // implies an outer relay envelope that this one-envelope decoder rejects.
    if hop_count != 0 {
        return Err(DecodeError::RelayHopCountExceeded(hop_count));
    }
    let link_address = Ipv6Addr::from(
        <[u8; 16]>::try_from(&packet[2..18])
            .map_err(|_| DecodeError::MalformedPacket("invalid relay link-address"))?,
    );
    let peer_address = Ipv6Addr::from(
        <[u8; 16]>::try_from(&packet[18..34])
            .map_err(|_| DecodeError::MalformedPacket("invalid relay peer-address"))?,
    );
    let options = parse_raw_options(&packet[34..])?;

    // Each consumer processes one client per envelope. Reject duplicates so
    // identity selection and response handling cannot observe different clients.
    let relay_message = unique_raw_option(&options, OptionCode::RelayMsg)?
        .ok_or(DecodeError::MissingRelayMessage)?;

    // Segment precedence and reply chaining are undefined for nested relays in
    // both current consumers, so reject instead of selecting an inner relay.
    if matches!(
        relay_message.first().copied(),
        Some(RELAY_FORWARD) | Some(RELAY_REPLY)
    ) {
        return Err(DecodeError::NestedRelay);
    }

    Ok(DecodedPacket {
        message: decode_client_message(relay_message)?,
        relay: Some(RelayEnvelope {
            hop_count,
            link_address,
            peer_address,
            interface_id: owned_unique_raw_option(&options, OptionCode::InterfaceId)?,
            remote_id: owned_unique_raw_option(&options, OptionCode::RemoteId)?,
            client_link_layer: owned_unique_raw_option(&options, OptionCode::ClientLinklayerAddr)?,
        }),
    })
}

/// Parse the link-layer payload shared by DUID-LL and DUID-LLT.
fn parse_link_layer_duid(bytes: &[u8], time_length: usize) -> Result<DuidMac, DuidError> {
    let link_layer_offset = 2 + time_length;
    if bytes.len() <= link_layer_offset {
        return Err(DuidError::Malformed);
    }

    let hardware_type = u16::from_be_bytes([bytes[0], bytes[1]]);
    if hardware_type != HARDWARE_TYPE_ETHERNET {
        return Ok(DuidMac::NoLinkLayerMac);
    }

    let mac = &bytes[link_layer_offset..];
    if mac.len() != ETHERNET_MAC_LENGTH {
        return Err(DuidError::Malformed);
    }

    Ok(DuidMac::Mac(MacAddress::new([
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
    ])))
}

/// Parse a sequence of DHCPv6 option TLVs without interpreting their payloads.
fn parse_raw_options(mut bytes: &[u8]) -> Result<Vec<RawOption<'_>>, DecodeError> {
    let mut options = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 4 {
            return Err(DecodeError::MalformedPacket("truncated option header"));
        }
        let code = u16::from_be_bytes([bytes[0], bytes[1]]);
        let length = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        if bytes.len() - 4 < length {
            return Err(DecodeError::MalformedPacket("truncated option payload"));
        }
        let encoded_length = 4 + length;
        let (encoded, remaining) = bytes.split_at(encoded_length);
        options.push(RawOption {
            code,
            data: &encoded[4..],
            encoded,
        });
        bytes = remaining;
    }
    Ok(options)
}

/// Reject semantically invalid client options before dhcproto can panic or
/// silently truncate its decoded option list.
fn validate_client_options(options: &[RawOption<'_>]) -> Result<(), DecodeError> {
    // Complete the resource-budget pass before invoking dhcproto anywhere in
    // the attacker-controlled option tree.
    let mut option_count = 0;
    validate_client_option_budget(options, 0, &mut option_count)?;
    validate_client_option_semantics(options)
}

/// Bound attacker-controlled option-tree depth and size before typed decoding.
fn validate_client_option_budget(
    options: &[RawOption<'_>],
    depth: usize,
    option_count: &mut usize,
) -> Result<(), DecodeError> {
    if depth > MAX_CLIENT_OPTION_NESTING_DEPTH {
        return Err(DecodeError::MalformedPacket(
            "client option nesting limit exceeded",
        ));
    }

    for option in options {
        *option_count += 1;
        if *option_count > MAX_CLIENT_OPTIONS {
            return Err(DecodeError::MalformedPacket(
                "client option count limit exceeded",
            ));
        }

        if OptionCode::from(option.code) == OptionCode::VendorOpts {
            if depth != 0 {
                return Err(DecodeError::MalformedPacket(
                    "vendor-specific option in nested client option",
                ));
            }
            let vendor_payload = option.data.get(4..).ok_or(DecodeError::MalformedPacket(
                "invalid vendor-specific option",
            ))?;
            for _ in parse_raw_options(vendor_payload)? {
                *option_count += 1;
                if *option_count > MAX_CLIENT_OPTIONS {
                    return Err(DecodeError::MalformedPacket(
                        "client option count limit exceeded",
                    ));
                }
            }
            continue;
        }

        if let Some(nested) = nested_option_payload(option)? {
            let nested_options = parse_raw_options(nested)?;
            if !nested_options.is_empty() {
                validate_client_option_budget(&nested_options, depth + 1, option_count)?;
            }
        }
    }
    Ok(())
}

/// Validate every bounded option in isolation before whole-message decoding.
fn validate_client_option_semantics(options: &[RawOption<'_>]) -> Result<(), DecodeError> {
    for option in options {
        // Vendor suboption codes are enterprise-local and have no global
        // OptionCode semantics. Their framing was validated by the budget pass.
        if OptionCode::from(option.code) == OptionCode::VendorOpts {
            continue;
        }

        let nested = nested_option_payload(option)?;

        // Validate nested DHCP option containers before their parent decoder
        // can consume bytes from a following malformed child option.
        if let Some(nested) = nested {
            validate_client_option_semantics(&parse_raw_options(nested)?)?;
        }

        // Isolating one TLV prevents a short option from borrowing bytes from
        // its successor. Full consumption rejects fixed-size length mismatches.
        let mut decoder = Decoder::new(option.encoded);
        let decoded = DhcpOption::decode(&mut decoder).map_err(DecodeError::ClientMessageDecode)?;
        if !decoder.buffer().is_empty() {
            return Err(DecodeError::MalformedPacket("invalid client option length"));
        }

        // Several dhcproto container decoders stop successfully at a malformed
        // child. A shorter round trip proves that bytes were silently omitted.
        let mut encoded = Vec::with_capacity(option.encoded.len());
        decoded
            .encode(&mut Encoder::new(&mut encoded))
            .map_err(|_| DecodeError::MalformedPacket("unencodable client option"))?;
        if encoded.len() != option.encoded.len() {
            return Err(DecodeError::MalformedPacket(
                "partially decoded client option",
            ));
        }
    }
    Ok(())
}

/// Return the nested option area for a DHCPv6 container option.
fn nested_option_payload<'data>(
    option: &RawOption<'data>,
) -> Result<Option<&'data [u8]>, DecodeError> {
    let offset = match OptionCode::from(option.code) {
        OptionCode::IANA | OptionCode::IAPD => 12,
        OptionCode::IATA => 4,
        OptionCode::IAAddr => 24,
        OptionCode::IAPrefix => 25,
        // Relay Message is valid in a relay envelope, not inside the
        // ordinary client message validated by this function.
        OptionCode::RelayMsg => {
            return Err(DecodeError::MalformedPacket(
                "relay-message option in client message",
            ));
        }
        _ => return Ok(None),
    };

    option
        .data
        .get(offset..)
        .map(Some)
        .ok_or(DecodeError::MalformedPacket("invalid client option"))
}

/// Return one raw option payload while rejecting ambiguous duplicates.
fn unique_raw_option<'data>(
    options: &[RawOption<'data>],
    option_code: OptionCode,
) -> Result<Option<&'data [u8]>, DecodeError> {
    let mut matches = options
        .iter()
        .filter(|option| option.code == u16::from(option_code))
        .map(|option| option.data);
    let value = matches.next();
    if matches.next().is_some() {
        Err(DecodeError::MalformedPacket("duplicate relay option"))
    } else {
        Ok(value)
    }
}

/// Return an owned raw option payload while retaining duplicate validation.
fn owned_unique_raw_option(
    options: &[RawOption<'_>],
    option_code: OptionCode,
) -> Result<Option<Vec<u8>>, DecodeError> {
    unique_raw_option(options, option_code).map(|value| value.map(|value| value.to_vec()))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    const DIRECT_SOLICIT: &[u8] = &[1, 0, 0, 1];

    /// Encode one raw option so malformed-packet tests control its declared
    /// payload independently of dhcproto's typed encoder.
    fn raw_option(option_code: OptionCode, payload: &[u8]) -> Vec<u8> {
        let mut option = Vec::new();
        option.extend_from_slice(&u16::from(option_code).to_be_bytes());
        option.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        option.extend_from_slice(payload);
        option
    }

    /// Build an ambiguous relay envelope so duplicate-option rejection is
    /// exercised before either consumer can select different metadata.
    fn relay_forward_with_duplicate_option(option_code: OptionCode) -> Vec<u8> {
        let mut packet = vec![RELAY_FORWARD, 0];
        packet.extend_from_slice(&[0; 32]);
        packet.extend(raw_option(option_code, b"first"));
        packet.extend(raw_option(option_code, b"second"));
        packet.extend(raw_option(OptionCode::RelayMsg, DIRECT_SOLICIT));
        packet
    }

    /// Build a structurally valid one-hop Relay-Forward around an arbitrary
    /// payload so relay-boundary tests vary only the encapsulated message.
    fn relay_forward(inner: &[u8]) -> Vec<u8> {
        let mut packet = vec![RELAY_FORWARD, 0];
        packet.extend_from_slice(&[0; 32]);
        packet.extend(raw_option(OptionCode::RelayMsg, inner));
        packet
    }

    /// Build an unregistered opaque DUID of an exact length so boundary tests
    /// remain independent of registered type-specific layouts.
    fn opaque_duid(length: usize) -> Vec<u8> {
        let mut duid = vec![0, 99];
        duid.resize(length, 0xaa);
        duid
    }

    /// Build a client message with an exact nested IA_TA depth so the
    /// resource-boundary test does not depend on dhcproto's typed encoder.
    fn client_message_with_option_nesting(nesting_depth: usize) -> Vec<u8> {
        let mut option = raw_option(OptionCode::Unknown(65_000), &[]);
        for _ in 0..nesting_depth {
            let mut payload = vec![0; 4];
            payload.extend(option);
            option = raw_option(OptionCode::IATA, &payload);
        }

        let mut packet = DIRECT_SOLICIT.to_vec();
        packet.extend(option);
        packet
    }

    /// Build one opaque vendor container with an exact number of framed
    /// enterprise-local suboptions.
    fn client_message_with_vendor_option_count(option_count: usize) -> Vec<u8> {
        let mut vendor_payload = 5703u32.to_be_bytes().to_vec();
        for _ in 0..option_count {
            vendor_payload.extend(raw_option(OptionCode::Unknown(65_000), &[]));
        }

        let mut packet = DIRECT_SOLICIT.to_vec();
        packet.extend(raw_option(OptionCode::VendorOpts, &vendor_payload));
        packet
    }

    /// Build a client message with an exact number of independent options so
    /// the count limit is exercised without adding nesting.
    fn client_message_with_option_count(option_count: usize) -> Vec<u8> {
        let mut packet = DIRECT_SOLICIT.to_vec();
        for _ in 0..option_count {
            packet.extend(raw_option(OptionCode::Unknown(65_000), &[]));
        }
        packet
    }

    /// Verifies generic DUID validation accepts exactly the complete-length
    /// range defined by RFC 9915, independent of registered DUID type.
    #[test]
    fn validates_duid_length_boundaries() {
        check_values(
            [
                // A type code without identifier data is incomplete.
                Check {
                    scenario: "type-only DUID",
                    input: opaque_duid(DUID_MIN_LENGTH - 1),
                    expect: Err(DuidError::Malformed),
                },
                // The shortest opaque DUID has one identifier octet.
                Check {
                    scenario: "minimum-length opaque DUID",
                    input: opaque_duid(DUID_MIN_LENGTH),
                    expect: Ok(DuidMac::NoLinkLayerMac),
                },
                // The longest opaque DUID has 128 identifier octets.
                Check {
                    scenario: "maximum-length opaque DUID",
                    input: opaque_duid(DUID_MAX_LENGTH),
                    expect: Ok(DuidMac::NoLinkLayerMac),
                },
                // One octet beyond the RFC limit is malformed.
                Check {
                    scenario: "oversized opaque DUID",
                    input: opaque_duid(DUID_MAX_LENGTH + 1),
                    expect: Err(DuidError::Malformed),
                },
            ],
            |duid| extract_mac_from_duid(&duid),
        );
    }

    /// Structurally complete link-layer DUIDs remain valid identities when
    /// their hardware address cannot be interpreted as an Ethernet MAC.
    #[test]
    fn classifies_non_ethernet_link_layer_duids_as_non_mac_identities() {
        check_values(
            [
                // DUID-LL accepts a variable-length InfiniBand address.
                Check {
                    scenario: "non-Ethernet DUID-LL",
                    input: {
                        let mut duid = DUID_LL.to_be_bytes().to_vec();
                        duid.extend_from_slice(&32u16.to_be_bytes());
                        duid.extend_from_slice(&[0xaa; 20]);
                        duid
                    },
                    expect: Ok(DuidMac::NoLinkLayerMac),
                },
                // DUID-LLT applies the same identity policy after its time field.
                Check {
                    scenario: "non-Ethernet DUID-LLT",
                    input: {
                        let mut duid = DUID_LLT.to_be_bytes().to_vec();
                        duid.extend_from_slice(&32u16.to_be_bytes());
                        duid.extend_from_slice(&1234u32.to_be_bytes());
                        duid.extend_from_slice(&[0xbb; 20]);
                        duid
                    },
                    expect: Ok(DuidMac::NoLinkLayerMac),
                },
                // A DUID-LL still needs at least one link-layer-address octet.
                Check {
                    scenario: "DUID-LL without link-layer address",
                    input: {
                        let mut duid = DUID_LL.to_be_bytes().to_vec();
                        duid.extend_from_slice(&32u16.to_be_bytes());
                        duid
                    },
                    expect: Err(DuidError::Malformed),
                },
                // A DUID-LLT must not end at its fixed hardware and time fields.
                Check {
                    scenario: "DUID-LLT without link-layer address",
                    input: {
                        let mut duid = DUID_LLT.to_be_bytes().to_vec();
                        duid.extend_from_slice(&32u16.to_be_bytes());
                        duid.extend_from_slice(&1234u32.to_be_bytes());
                        duid
                    },
                    expect: Err(DuidError::Malformed),
                },
            ],
            |duid| extract_mac_from_duid(&duid),
        );
    }

    /// Ingress classification extracts direct and one-hop client message types
    /// without applying the full DHCPv6 decoder or allocating option storage.
    #[test]
    fn classifies_request_message_type_with_bounded_relay_inspection() {
        check_values(
            [
                // Direct traffic exposes its client message type in octet zero.
                Check {
                    scenario: "direct Solicit",
                    input: DIRECT_SOLICIT.to_vec(),
                    expect: MessageType::Solicit,
                },
                // Relay traffic uses the first octet of its relay-message payload.
                Check {
                    scenario: "relayed Request",
                    input: relay_forward(&[u8::from(MessageType::Request), 0, 0, 1]),
                    expect: MessageType::Request,
                },
                // A malformed relay stays in the bounded catch-all metric label.
                Check {
                    scenario: "truncated Relay-Forward",
                    input: vec![RELAY_FORWARD],
                    expect: MessageType::RelayForw,
                },
            ],
            |packet| request_message_type(&packet),
        );
    }

    /// Verifies incomplete client-option TLVs cannot be silently ignored.
    ///
    /// This prevents a partially decoded client message from reaching either
    /// the Kea hook or standalone DHCPv6 policy.
    #[test]
    fn rejects_incomplete_client_option_tlvs() {
        check_values(
            [
                // An incomplete header cannot identify an option or its payload.
                Check {
                    scenario: "truncated option header",
                    input: {
                        let mut packet = DIRECT_SOLICIT.to_vec();
                        packet.extend_from_slice(&[0, 1, 0]);
                        packet
                    },
                    expect: true,
                },
                // A complete header must not claim bytes beyond the datagram.
                Check {
                    scenario: "truncated option payload",
                    input: {
                        let mut packet = DIRECT_SOLICIT.to_vec();
                        packet.extend_from_slice(&[0, 1, 0, 2, 0xaa]);
                        packet
                    },
                    expect: true,
                },
            ],
            |packet| {
                matches!(
                    decode_client_message(&packet),
                    Err(DecodeError::MalformedPacket(_))
                )
            },
        );
    }

    /// Verifies semantically malformed options fail closed instead of panicking
    /// or disappearing from dhcproto's successfully decoded message.
    ///
    /// This protects both the standalone task boundary and Kea's C ABI from
    /// attacker-controlled option payloads.
    #[test]
    fn rejects_semantically_invalid_client_options() {
        check_values(
            [
                // A short Vendor Class must not borrow the following TLV header
                // and underflow its declared length.
                Check {
                    scenario: "zero-length Vendor Class",
                    input: {
                        let mut packet = DIRECT_SOLICIT.to_vec();
                        packet.extend(raw_option(OptionCode::VendorClass, &[]));
                        packet.extend(raw_option(OptionCode::RapidCommit, &[]));
                        packet
                    },
                    expect: true,
                },
                // A fixed-size IA_NA payload must not disappear with every
                // otherwise valid option that follows it.
                Check {
                    scenario: "short IA_NA",
                    input: {
                        let mut packet = DIRECT_SOLICIT.to_vec();
                        packet.extend(raw_option(OptionCode::IANA, &[]));
                        packet.extend(raw_option(OptionCode::RapidCommit, &[]));
                        packet
                    },
                    expect: true,
                },
                // A truncated length-prefixed class value is otherwise decoded
                // as an empty option with its malformed bytes discarded.
                Check {
                    scenario: "truncated User Class value",
                    input: {
                        let mut packet = DIRECT_SOLICIT.to_vec();
                        packet.extend(raw_option(OptionCode::UserClass, &[0, 2, 0xaa]));
                        packet
                    },
                    expect: true,
                },
                // Nested IA_NA options need the same isolation as top-level
                // options because dhcproto recursively uses its lossy decoder.
                Check {
                    scenario: "zero-length nested Vendor Class",
                    input: {
                        let mut association = vec![0; 12];
                        association.extend(raw_option(OptionCode::VendorClass, &[]));
                        association.extend(raw_option(OptionCode::RapidCommit, &[]));
                        let mut packet = DIRECT_SOLICIT.to_vec();
                        packet.extend(raw_option(OptionCode::IANA, &association));
                        packet
                    },
                    expect: true,
                },
            ],
            |packet| {
                matches!(
                    decode_client_message(&packet),
                    Err(DecodeError::MalformedPacket(_)) | Err(DecodeError::ClientMessageDecode(_))
                )
            },
        );
    }

    /// Verifies enterprise-local vendor suboption codes remain opaque even
    /// when they collide with globally registered DHCPv6 option codes.
    #[test]
    fn preserves_opaque_vendor_suboptions() {
        // Private code 3 collides with IA_NA, but its one-byte payload has only
        // the vendor-defined meaning and must not be decoded as an IA_NA.
        let private_suboption = raw_option(OptionCode::IANA, &[0xaa]);
        let mut vendor_payload = 5703u32.to_be_bytes().to_vec();
        vendor_payload.extend(private_suboption);
        let mut packet = DIRECT_SOLICIT.to_vec();
        packet.extend(raw_option(OptionCode::VendorOpts, &vendor_payload));

        let message = decode_client_message(&packet).expect("opaque vendor suboption is accepted");
        match message.opts().get(OptionCode::VendorOpts) {
            Some(DhcpOption::Unknown(option)) => {
                assert_eq!(option.data(), vendor_payload);
            }
            other => panic!("expected opaque vendor-specific option, got {other:?}"),
        }
    }

    /// Verifies vendor-private payloads still obey their own TLV framing.
    #[test]
    fn rejects_truncated_vendor_suboptions() {
        let mut vendor_payload = 5703u32.to_be_bytes().to_vec();
        // The private suboption declares two bytes but supplies only one.
        vendor_payload.extend_from_slice(&[0, 3, 0, 2, 0xaa]);
        let mut packet = DIRECT_SOLICIT.to_vec();
        packet.extend(raw_option(OptionCode::VendorOpts, &vendor_payload));

        assert!(matches!(
            decode_client_message(&packet),
            Err(DecodeError::MalformedPacket("truncated option payload"))
        ));
    }

    /// Verifies attacker-controlled option trees are accepted through the
    /// explicit service budget and rejected immediately beyond either bound.
    #[test]
    fn enforces_client_option_resource_budget() {
        check_values(
            [
                // The deepest supported option tree remains available to clients.
                Check {
                    scenario: "maximum nesting depth",
                    input: client_message_with_option_nesting(MAX_CLIENT_OPTION_NESTING_DEPTH),
                    expect: true,
                },
                // One additional container is rejected before typed decoding recurses.
                Check {
                    scenario: "nesting depth above limit",
                    input: client_message_with_option_nesting(MAX_CLIENT_OPTION_NESTING_DEPTH + 1),
                    expect: false,
                },
                // The maximum bounded number of shallow options remains accepted.
                Check {
                    scenario: "maximum option count",
                    input: client_message_with_option_count(MAX_CLIENT_OPTIONS),
                    expect: true,
                },
                // One additional option is rejected before any typed option decode.
                Check {
                    scenario: "option count above limit",
                    input: client_message_with_option_count(MAX_CLIENT_OPTIONS + 1),
                    expect: false,
                },
                // One vendor container plus 255 private suboptions meets the limit.
                Check {
                    scenario: "maximum vendor suboption count",
                    input: client_message_with_vendor_option_count(MAX_CLIENT_OPTIONS - 1),
                    expect: true,
                },
                // A vendor container and 256 private suboptions exceed the limit.
                Check {
                    scenario: "vendor suboption count above limit",
                    input: client_message_with_vendor_option_count(MAX_CLIENT_OPTIONS),
                    expect: false,
                },
            ],
            |packet| decode_client_message(&packet).is_ok(),
        );
    }

    /// Verifies single-valued relay identity and routing options reject
    /// ambiguous duplicates.
    ///
    /// This ensures the Kea hook and standalone server cannot select different
    /// metadata from the same Relay-Forward envelope.
    #[test]
    fn rejects_duplicate_single_valued_relay_options() {
        check_values(
            [
                // Repeated Interface-ID values make segment selection ambiguous.
                Check {
                    scenario: "duplicate Interface-ID",
                    input: OptionCode::InterfaceId,
                    expect: true,
                },
                // Repeated Remote-ID values make relay identity ambiguous.
                Check {
                    scenario: "duplicate Remote-ID",
                    input: OptionCode::RemoteId,
                    expect: true,
                },
                // Repeated option 79 values make MAC selection ambiguous.
                Check {
                    scenario: "duplicate Client Link-Layer Address",
                    input: OptionCode::ClientLinklayerAddr,
                    expect: true,
                },
            ],
            |option_code| {
                matches!(
                    decode(&relay_forward_with_duplicate_option(option_code)),
                    Err(DecodeError::MalformedPacket("duplicate relay option"))
                )
            },
        );
    }

    /// Verifies the shared decoder owns relay-header, hop-count, and nesting
    /// boundaries independently of its Kea and standalone-server adapters.
    #[test]
    fn rejects_unsupported_relay_boundaries() {
        check_values(
            [
                // A Relay-Forward must contain its complete 34-byte fixed header.
                Check {
                    scenario: "truncated relay header",
                    input: {
                        let mut packet = relay_forward(DIRECT_SOLICIT);
                        packet.truncate(33);
                        packet
                    },
                    expect: "malformed DHCPv6 packet: truncated relay header".to_string(),
                },
                // A direct client payload is valid only in the first relay envelope.
                Check {
                    scenario: "nonzero hop count",
                    input: {
                        let mut packet = relay_forward(DIRECT_SOLICIT);
                        packet[1] = 1;
                        packet
                    },
                    expect: "unsupported DHCPv6 relay hop count 1".to_string(),
                },
                // A nested Relay-Forward has undefined routing precedence here.
                Check {
                    scenario: "nested Relay-Forward",
                    input: relay_forward(&[RELAY_FORWARD]),
                    expect: "nested DHCPv6 relay packet".to_string(),
                },
                // A nested Relay-Reply is also invalid as a client payload.
                Check {
                    scenario: "nested Relay-Reply",
                    input: relay_forward(&[RELAY_REPLY]),
                    expect: "nested DHCPv6 relay packet".to_string(),
                },
            ],
            |packet| {
                decode(&packet)
                    .expect_err("unsupported relay boundary must fail")
                    .to_string()
            },
        );
    }
}
