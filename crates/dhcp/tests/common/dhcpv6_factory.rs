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
use std::net::Ipv6Addr;

use dhcproto::v6::{
    DhcpOption, IAAddr, IANA, IAPD, IATA, Message, MessageType, OptionCode, UnknownOption,
};
use dhcproto::{Decodable, Decoder, Encodable, Encoder};

pub(super) const RELAY_ADDR: &str = "::1";

const RELAY_FORW: u8 = 12;
const RELAY_REPL: u8 = 13;
const HTYPE_ETHERNET: u16 = 1;

pub(crate) struct DHCPv6Factory {}

impl DHCPv6Factory {
    /// Relay link-address used by generated Relay-Forward packets.
    pub(crate) const RELAY_LINK_ADDR: &str = "2001:db8::1";

    /// Hex-encoded relay interface-id expected by the API.
    pub(crate) const RELAY_INTERFACE_ID_HEX: &str = "65746830";

    /// Hex-encoded relay remote-id expected by the API.
    pub(crate) const RELAY_REMOTE_ID_HEX: &str = "7261636b2d61";

    /// Return the stable locally-administered MAC used for one test client.
    fn mac(idx: u8) -> [u8; 6] {
        [0x02, 0x00, 0x00, 0x00, 0x00, idx]
    }

    /// Return the mock API address for a client index.
    pub(crate) fn mock_addr(idx: u8) -> Ipv6Addr {
        Ipv6Addr::from(0x20010db8000000000000000000000000u128 + idx as u128)
    }

    /// Return the colon-separated MAC string used by the mock API.
    pub(crate) fn mac_string(idx: u8) -> String {
        format!("02:00:00:00:00:{idx:02x}")
    }

    /// Return a raw RFC 4704 client-FQDN payload with non-zero flags.
    pub(crate) fn client_fqdn_payload() -> Vec<u8> {
        let mut payload = vec![0x01];
        payload.extend_from_slice(&[
            9, b't', b'e', b's', b't', b'-', b'h', b'o', b's', b't', 5, b'f', b'o', b'r', b'g',
            b'e', 5, b'l', b'o', b'c', b'a', b'l', 0,
        ]);
        payload
    }

    /// Build a DUID-LL identity for one test client.
    pub(crate) fn duid_ll(idx: u8) -> Vec<u8> {
        let mut duid = vec![0, 3];
        duid.extend_from_slice(&HTYPE_ETHERNET.to_be_bytes());
        duid.extend_from_slice(&Self::mac(idx));
        duid
    }

    /// Return the hex-encoded DUID-LL identity for one test client.
    pub(crate) fn duid_ll_hex(idx: u8) -> String {
        Self::duid_hex(&Self::duid_ll(idx))
    }

    /// Return the hex-encoded representation of a raw DUID.
    pub(crate) fn duid_hex(duid: &[u8]) -> String {
        duid.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Build a DUID-LLT identity for one test client.
    pub(crate) fn duid_llt(idx: u8) -> Vec<u8> {
        let mut duid = vec![0, 1];
        duid.extend_from_slice(&HTYPE_ETHERNET.to_be_bytes());
        duid.extend_from_slice(&1u32.to_be_bytes());
        duid.extend_from_slice(&Self::mac(idx));
        duid
    }

    /// Build a DUID-EN identity with the requested total byte length.
    pub(crate) fn duid_en(len: usize) -> Vec<u8> {
        // Type 2 plus enterprise-number form the minimum DUID-EN prefix.
        let mut duid = vec![0, 2, 0, 0, 0, 1];
        duid.resize(len, 0xaa);
        duid
    }

    /// Build a valid DUID-UUID identity with no embedded MAC.
    pub(crate) fn duid_uuid(idx: u8) -> Vec<u8> {
        vec![0, 4, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, idx]
    }

    /// Build a malformed UUID DUID that should be rejected before option 79.
    pub(crate) fn truncated_duid_uuid() -> Vec<u8> {
        vec![0, 4, 0, 1, 2, 3]
    }

    /// Build a stateful SOLICIT with IA_NA.
    pub(crate) fn solicit(idx: u8) -> Vec<u8> {
        Self::relay_wrap(Self::stateful_solicit_message(idx), true)
    }

    /// Build a stateful SOLICIT with a caller-supplied relay hop count.
    pub(crate) fn solicit_with_hop_count(idx: u8, hop_count: u8) -> Vec<u8> {
        Self::relay_wrap_with_hop_count(Self::stateful_solicit_message(idx), true, hop_count)
    }

    /// Build a SOLICIT wrapped by two Relay-Forward envelopes.
    pub(crate) fn solicit_with_nested_relay(idx: u8) -> Vec<u8> {
        let inner = Self::relay_wrap(Self::stateful_solicit_message(idx), true);
        Self::relay_wrap_payload(&inner, idx, true, 0)
    }

    /// Build a stateful SOLICIT without relay option 79.
    pub(crate) fn solicit_without_relay_option79(idx: u8) -> Vec<u8> {
        Self::relay_wrap(Self::stateful_solicit_message(idx), false)
    }

    /// Build a stateful SOLICIT carrying vendor-class option 16.
    pub(crate) fn solicit_with_vendor_class(idx: u8, vendor_class: &[u8]) -> Vec<u8> {
        let mut message = Self::stateful_solicit_message(idx);
        Self::add_vendor_class(&mut message, vendor_class);
        Self::relay_wrap(message, true)
    }

    /// Build a SOLICIT using a caller-supplied DUID and relay option-79 policy.
    pub(crate) fn solicit_with_duid(idx: u8, duid: Vec<u8>, relay_option79: bool) -> Vec<u8> {
        Self::relay_wrap(
            Self::client_message(idx, MessageType::Solicit, duid, None, true, false),
            relay_option79,
        )
    }

    /// Build a SOLICIT with a spoofed client-supplied option 79 only.
    pub(crate) fn solicit_with_inner_option79(idx: u8, duid: Vec<u8>) -> Vec<u8> {
        let mut message = Self::client_message(idx, MessageType::Solicit, duid, None, true, false);
        message
            .opts_mut()
            .insert(DhcpOption::Unknown(UnknownOption::new(
                OptionCode::ClientLinklayerAddr,
                Self::option79_payload(idx),
            )));
        Self::relay_wrap(message, false)
    }

    /// Build a SOLICIT carrying the requested number of rapid-commit options.
    pub(crate) fn solicit_with_rapid_commit_options(idx: u8, option_count: usize) -> Vec<u8> {
        let mut message = Self::client_message(
            idx,
            MessageType::Solicit,
            Self::duid_ll(idx),
            None,
            true,
            false,
        );
        for _ in 0..option_count {
            message.opts_mut().insert(DhcpOption::RapidCommit);
        }
        Self::relay_wrap(message, true)
    }

    /// Build a stateless SOLICIT with no IA_NA.
    pub(crate) fn stateless_solicit(idx: u8) -> Vec<u8> {
        Self::relay_wrap(Self::stateless_solicit_message(idx), true)
    }

    /// Build a SOLICIT carrying unsupported IA_TA.
    pub(crate) fn solicit_with_ia_ta(idx: u8) -> Vec<u8> {
        // Omit IA_NA so the packet only exercises the unsupported IA_TA path.
        let mut message = Self::stateless_solicit_message(idx);
        Self::add_ia_ta(&mut message, idx);
        Self::relay_wrap(message, true)
    }

    /// Build a SOLICIT carrying supported IA_NA plus unsupported IA_TA.
    pub(crate) fn solicit_with_ia_na_and_ia_ta(idx: u8) -> Vec<u8> {
        // Keep IA_NA present to exercise mixed-container rejection.
        let mut message = Self::stateful_solicit_message(idx);
        Self::add_ia_ta(&mut message, idx);
        Self::relay_wrap(message, true)
    }

    /// Build a SOLICIT carrying unsupported IA_PD.
    pub(crate) fn solicit_with_ia_pd(idx: u8) -> Vec<u8> {
        // Omit IA_NA so the packet only exercises the unsupported IA_PD path.
        let mut message = Self::stateless_solicit_message(idx);
        Self::add_ia_pd(&mut message, idx);
        Self::relay_wrap(message, true)
    }

    /// Build a SOLICIT carrying supported IA_NA plus unsupported IA_PD.
    pub(crate) fn solicit_with_ia_na_and_ia_pd(idx: u8) -> Vec<u8> {
        // Keep IA_NA present to exercise mixed-container rejection.
        let mut message = Self::stateful_solicit_message(idx);
        Self::add_ia_pd(&mut message, idx);
        Self::relay_wrap(message, true)
    }

    /// Build a SOLICIT carrying more than one IA_NA container.
    pub(crate) fn solicit_with_multiple_ia_na(idx: u8) -> Vec<u8> {
        // The API contract has one selected address, so multiple IA_NA
        // containers must be rejected before lease override.
        let mut message = Self::stateful_solicit_message(idx);
        message.opts_mut().insert(DhcpOption::IANA(IANA {
            id: idx as u32 + 1,
            t1: 0,
            t2: 0,
            opts: Default::default(),
        }));
        Self::relay_wrap(message, true)
    }

    /// Build an INFORMATION-REQUEST.
    pub(crate) fn information_request(idx: u8) -> Vec<u8> {
        Self::relay_wrap(Self::information_request_message(idx), true)
    }

    /// Build an INFORMATION-REQUEST carrying client option 39.
    pub(crate) fn information_request_with_client_fqdn(idx: u8) -> Vec<u8> {
        let mut message = Self::information_request_message(idx);
        Self::add_client_fqdn(&mut message);
        Self::relay_wrap(message, true)
    }

    /// Build a REQUEST selecting the advertised server and address.
    pub(crate) fn request(idx: u8, server_id: Vec<u8>, address: Ipv6Addr) -> Vec<u8> {
        Self::relay_wrap(
            Self::message_with_server_and_address(idx, MessageType::Request, server_id, address),
            true,
        )
    }

    /// Build a REQUEST with no IA_NA.
    pub(crate) fn request_without_ia_na(idx: u8) -> Vec<u8> {
        // REQUEST is stateful; this intentionally omits IA_NA to verify drop behavior.
        Self::relay_wrap(
            Self::client_message(
                idx,
                MessageType::Request,
                Self::duid_ll(idx),
                None,
                false,
                false,
            ),
            true,
        )
    }

    /// Build a REQUEST without relay option 79.
    pub(crate) fn request_without_relay_option79(
        idx: u8,
        server_id: Vec<u8>,
        address: Ipv6Addr,
    ) -> Vec<u8> {
        Self::relay_wrap(
            Self::message_with_server_and_address(idx, MessageType::Request, server_id, address),
            false,
        )
    }

    /// Build a REQUEST using a caller-supplied DUID and relay option-79 policy.
    pub(crate) fn request_with_duid(
        idx: u8,
        server_id: Vec<u8>,
        address: Ipv6Addr,
        duid: Vec<u8>,
        relay_option79: bool,
    ) -> Vec<u8> {
        Self::relay_wrap(
            Self::client_message(
                idx,
                MessageType::Request,
                duid,
                Some((server_id, address)),
                true,
                false,
            ),
            relay_option79,
        )
    }

    /// Build a REQUEST carrying vendor-class option 16.
    pub(crate) fn request_with_vendor_class(
        idx: u8,
        server_id: Vec<u8>,
        address: Ipv6Addr,
        vendor_class: &[u8],
    ) -> Vec<u8> {
        let mut message =
            Self::message_with_server_and_address(idx, MessageType::Request, server_id, address);
        Self::add_vendor_class(&mut message, vendor_class);
        Self::relay_wrap(message, true)
    }

    /// Build a RENEW for an existing lease.
    pub(crate) fn renew(idx: u8, server_id: Vec<u8>, address: Ipv6Addr) -> Vec<u8> {
        Self::relay_wrap(
            Self::message_with_server_and_address(idx, MessageType::Renew, server_id, address),
            true,
        )
    }

    /// Build a RENEW using a caller-supplied DUID and relay option-79 policy.
    pub(crate) fn renew_with_duid(
        idx: u8,
        server_id: Vec<u8>,
        address: Ipv6Addr,
        duid: Vec<u8>,
        relay_option79: bool,
    ) -> Vec<u8> {
        Self::relay_wrap(
            Self::client_message(
                idx,
                MessageType::Renew,
                duid,
                Some((server_id, address)),
                true,
                false,
            ),
            relay_option79,
        )
    }

    /// Build a REBIND for an existing lease.
    pub(crate) fn rebind(idx: u8, address: Ipv6Addr) -> Vec<u8> {
        Self::relay_wrap(
            Self::client_message_with_address(idx, MessageType::Rebind, address, None),
            true,
        )
    }

    /// Build a RELEASE with a caller-supplied relay hop count.
    pub(crate) fn release_with_hop_count(
        idx: u8,
        server_id: Vec<u8>,
        address: Ipv6Addr,
        hop_count: u8,
    ) -> Vec<u8> {
        Self::relay_wrap_with_hop_count(
            Self::message_with_server_and_address(idx, MessageType::Release, server_id, address),
            true,
            hop_count,
        )
    }

    /// Build a RELEASE carrying unsupported IA_TA alongside the released IA_NA.
    pub(crate) fn release_with_ia_ta(idx: u8, server_id: Vec<u8>, address: Ipv6Addr) -> Vec<u8> {
        // Lease-end messages are Kea protocol handling; extra IA_TA must not
        // make the Carbide hook call discovery or drop before Kea responds.
        let mut message =
            Self::message_with_server_and_address(idx, MessageType::Release, server_id, address);
        Self::add_ia_ta(&mut message, idx);
        Self::relay_wrap(message, true)
    }

    /// Build a DECLINE carrying unsupported IA_PD alongside the declined IA_NA.
    pub(crate) fn decline_with_ia_pd(idx: u8, server_id: Vec<u8>, address: Ipv6Addr) -> Vec<u8> {
        // Lease-end messages are Kea protocol handling; extra IA_PD must not
        // make the Carbide hook call discovery or drop before Kea responds.
        let mut message =
            Self::message_with_server_and_address(idx, MessageType::Decline, server_id, address);
        Self::add_ia_pd(&mut message, idx);
        Self::relay_wrap(message, true)
    }

    /// Build a DECLINE with a caller-supplied relay hop count.
    pub(crate) fn decline_with_hop_count(
        idx: u8,
        server_id: Vec<u8>,
        address: Ipv6Addr,
        hop_count: u8,
    ) -> Vec<u8> {
        Self::relay_wrap_with_hop_count(
            Self::message_with_server_and_address(idx, MessageType::Decline, server_id, address),
            true,
            hop_count,
        )
    }

    // Build a CONFIRM with an independent client identity and transaction ID.
    pub(crate) fn confirm(idx: u8, transaction_id: u8, address: Ipv6Addr) -> Vec<u8> {
        let mut message =
            Self::client_message_with_address(idx, MessageType::Confirm, address, None);
        message.set_xid([0xaa, 0xcc, transaction_id]);
        let mut inner = Vec::new();
        message.encode(&mut Encoder::new(&mut inner)).unwrap();
        // relay_wrap_payload derives option 79 from idx explicitly, keeping the
        // relay identity independent of the transaction ID.
        Self::relay_wrap_payload(&inner, idx, true, 0)
    }

    /// Build the default stateful SOLICIT message used by most lease tests.
    fn stateful_solicit_message(idx: u8) -> Message {
        Self::client_message(
            idx,
            MessageType::Solicit,
            Self::duid_ll(idx),
            None,
            true,
            false,
        )
    }

    /// Build the default stateless SOLICIT message used by options-only tests.
    fn stateless_solicit_message(idx: u8) -> Message {
        Self::client_message(
            idx,
            MessageType::Solicit,
            Self::duid_ll(idx),
            None,
            false,
            false,
        )
    }

    /// Build the default INFORMATION-REQUEST message used by options-only tests.
    fn information_request_message(idx: u8) -> Message {
        Self::client_message(
            idx,
            MessageType::InformationRequest,
            Self::duid_ll(idx),
            None,
            false,
            false,
        )
    }

    /// Build a default-DUID message that selects a server-owned IA_NA address.
    fn message_with_server_and_address(
        idx: u8,
        message_type: MessageType,
        server_id: Vec<u8>,
        address: Ipv6Addr,
    ) -> Message {
        Self::client_message(
            idx,
            message_type,
            Self::duid_ll(idx),
            Some((server_id, address)),
            true,
            false,
        )
    }

    fn client_message_with_address(
        idx: u8,
        message_type: MessageType,
        address: Ipv6Addr,
        server_id: Option<Vec<u8>>,
    ) -> Message {
        let mut message = Message::new_with_id(message_type, [0xaa, 0xbb, idx]);
        message
            .opts_mut()
            .insert(DhcpOption::ClientId(Self::duid_ll(idx)));
        if let Some(server_id) = server_id {
            message.opts_mut().insert(DhcpOption::ServerId(server_id));
        }
        let mut ia_na = IANA {
            id: idx as u32,
            t1: 0,
            t2: 0,
            opts: Default::default(),
        };
        ia_na.opts.insert(DhcpOption::IAAddr(IAAddr {
            addr: address,
            preferred_life: 0,
            valid_life: 0,
            opts: Default::default(),
        }));
        message.opts_mut().insert(DhcpOption::IANA(ia_na));
        message
    }

    fn add_client_fqdn(message: &mut Message) {
        message
            .opts_mut()
            .insert(DhcpOption::Unknown(UnknownOption::new(
                OptionCode::ClientFqdn,
                Self::client_fqdn_payload(),
            )));
    }

    /// Add an unsupported IA_TA option to a DHCPv6 test message.
    fn add_ia_ta(message: &mut Message, idx: u8) {
        // IA_TA is intentionally unsupported by Carbide's DHCPv6 API path.
        message.opts_mut().insert(DhcpOption::IATA(IATA {
            id: idx as u32,
            opts: Default::default(),
        }));
    }

    /// Add an unsupported IA_PD option to a DHCPv6 test message.
    fn add_ia_pd(message: &mut Message, idx: u8) {
        // IA_PD is intentionally unsupported by Carbide's DHCPv6 API path.
        message.opts_mut().insert(DhcpOption::IAPD(IAPD {
            id: idx as u32,
            t1: 0,
            t2: 0,
            opts: Default::default(),
        }));
    }

    fn add_vendor_class(message: &mut Message, vendor_class: &[u8]) {
        let mut payload = 32473u32.to_be_bytes().to_vec();
        payload.extend_from_slice(&(vendor_class.len() as u16).to_be_bytes());
        payload.extend_from_slice(vendor_class);
        message
            .opts_mut()
            .insert(DhcpOption::Unknown(UnknownOption::new(
                OptionCode::VendorClass,
                payload,
            )));
    }

    /// Decode Kea's Relay-Reply and return the inner DHCPv6 message.
    pub(crate) fn decode_reply(packet: &[u8]) -> Result<Message, eyre::Report> {
        match packet.first().copied() {
            Some(RELAY_REPL) => {
                let relay_msg = Self::relay_msg_payload(packet)
                    .ok_or_else(|| eyre::eyre!("relay reply did not include relay-msg"))?;
                Message::decode(&mut Decoder::new(relay_msg))
                    .map_err(|err| eyre::eyre!("failed to decode inner DHCPv6 message: {err}"))
            }
            Some(_) => Message::decode(&mut Decoder::new(packet))
                .map_err(|err| eyre::eyre!("failed to decode DHCPv6 message: {err}")),
            None => Err(eyre::eyre!("empty DHCPv6 response")),
        }
    }

    /// Extract the IAADDR from the first IA_NA option.
    pub(crate) fn ia_addr(message: &Message) -> Option<Ipv6Addr> {
        match message.opts().get(OptionCode::IANA) {
            Some(DhcpOption::IANA(ia_na)) => match ia_na.opts.get(OptionCode::IAAddr) {
                Some(DhcpOption::IAAddr(addr)) => Some(addr.addr),
                _ => None,
            },
            _ => None,
        }
    }

    /// Extract the server-id option needed for REQUEST/RENEW/RELEASE.
    pub(crate) fn server_id(message: &Message) -> Vec<u8> {
        match message.opts().get(OptionCode::ServerId) {
            Some(DhcpOption::ServerId(server_id)) => server_id.clone(),
            other => panic!("DHCPv6 response did not include server-id: {other:?}"),
        }
    }

    fn client_message(
        idx: u8,
        message_type: MessageType,
        duid: Vec<u8>,
        server_and_addr: Option<(Vec<u8>, Ipv6Addr)>,
        include_ia_na: bool,
        rapid_commit: bool,
    ) -> Message {
        let mut message = Message::new_with_id(message_type, [0xaa, 0xbb, idx]);
        message.opts_mut().insert(DhcpOption::ClientId(duid));
        if let Some((server_id, _)) = &server_and_addr {
            message
                .opts_mut()
                .insert(DhcpOption::ServerId(server_id.clone()));
        }
        if include_ia_na {
            let mut ia_na = IANA {
                id: idx as u32,
                t1: 0,
                t2: 0,
                opts: Default::default(),
            };
            if let Some((_, address)) = server_and_addr {
                ia_na.opts.insert(DhcpOption::IAAddr(IAAddr {
                    addr: address,
                    preferred_life: 300,
                    valid_life: 600,
                    opts: Default::default(),
                }));
            }
            message.opts_mut().insert(DhcpOption::IANA(ia_na));
        }
        if rapid_commit {
            message.opts_mut().insert(DhcpOption::RapidCommit);
        }
        message
    }

    fn relay_wrap(message: Message, relay_option79: bool) -> Vec<u8> {
        Self::relay_wrap_with_hop_count(message, relay_option79, 0)
    }

    fn relay_wrap_with_hop_count(message: Message, relay_option79: bool, hop_count: u8) -> Vec<u8> {
        let mut inner = Vec::new();
        message.encode(&mut Encoder::new(&mut inner)).unwrap();
        Self::relay_wrap_payload(&inner, message.xid()[2], relay_option79, hop_count)
    }

    /// Wrap raw DHCPv6 payload bytes in one Relay-Forward envelope.
    fn relay_wrap_payload(payload: &[u8], idx: u8, relay_option79: bool, hop_count: u8) -> Vec<u8> {
        let mut out = vec![RELAY_FORW, hop_count];
        out.extend_from_slice(&Self::RELAY_LINK_ADDR.parse::<Ipv6Addr>().unwrap().octets());
        out.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        out.extend_from_slice(&Self::raw_option(OptionCode::InterfaceId, b"eth0"));
        out.extend_from_slice(&Self::raw_option(OptionCode::RemoteId, b"rack-a"));
        if relay_option79 {
            out.extend_from_slice(&Self::raw_option(
                OptionCode::ClientLinklayerAddr,
                &Self::option79_payload(idx),
            ));
        }
        out.extend_from_slice(&Self::raw_option(OptionCode::RelayMsg, payload));
        out
    }

    fn relay_msg_payload(packet: &[u8]) -> Option<&[u8]> {
        if packet.len() < 34 {
            return None;
        }

        let mut options = &packet[34..];
        while options.len() >= 4 {
            let code = u16::from_be_bytes([options[0], options[1]]);
            let len = u16::from_be_bytes([options[2], options[3]]) as usize;
            options = &options[4..];
            if options.len() < len {
                return None;
            }
            let (payload, rest) = options.split_at(len);
            if code == u16::from(OptionCode::RelayMsg) {
                return Some(payload);
            }
            options = rest;
        }

        None
    }

    fn option79_payload(idx: u8) -> Vec<u8> {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&HTYPE_ETHERNET.to_be_bytes());
        payload.extend_from_slice(&Self::mac(idx));
        payload
    }

    fn raw_option(code: OptionCode, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&u16::from(code).to_be_bytes());
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }
}
