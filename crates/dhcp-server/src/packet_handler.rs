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
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::str::FromStr;
use std::sync::Arc;

use carbide_dhcp_common::VendorClass;
use carbide_instrument::emit;
use carbide_rpc_utils::dhcp::{
    DhcpV4Config, HostConfig, InterfaceInfo, ServiceLink, ServiceLinkV4,
};
use dhcproto::v4::relay::{RelayAgentInformation, RelayCode, RelayInfo};
use dhcproto::v4::{Decodable, Decoder, DhcpOption, Message, MessageType, OptionCode};
use dhcproto::{Encodable, Encoder};
use ipnetwork::IpNetwork;
use lru::LruCache;
use rpc::forge::{DhcpDiscovery, DhcpRecord};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use crate::cache::CacheEntry;
use crate::errors::DhcpError;
use crate::metrics::{DhcpReplySent, DhcpRequestReceived, MessageTypeLabel};
use crate::modes::DhcpMode;
use crate::{Config, ReplyRetirement, ReplyStatus, util};

const PKT_TYPE_OP_REQUEST: u8 = 1;
const BOOTP_CHADDR_LEN: u8 = 16;

pub struct DecodedPacket {
    packet: Message,
}

trait DecodedPacketTrait<T> {
    fn get_option_val(
        &self,
        option: OptionCode,
        relay_code: Option<RelayCode>,
    ) -> Result<T, DhcpError>;
}

impl DecodedPacketTrait<String> for DecodedPacket {
    fn get_option_val(
        &self,
        option: OptionCode,
        relay_code: Option<RelayCode>,
    ) -> Result<String, DhcpError> {
        if let Some(value) = self.packet.opts().get(option) {
            match value {
                DhcpOption::ClassIdentifier(x) => Ok(std::str::from_utf8(x)?.to_string()),
                DhcpOption::RelayAgentInformation(agent_info) => {
                    let relay_code = relay_code.unwrap(); // This can not be None
                    let Some(val) = agent_info.get(relay_code) else {
                        return Err(DhcpError::MissingRelayCode(relay_code));
                    };

                    match val {
                        RelayInfo::LinkSelection(ip) => Ok(ip.to_string()),
                        RelayInfo::AgentCircuitId(x) | RelayInfo::AgentRemoteId(x) => {
                            Ok(util::u8_to_hex_string(x)?)
                        }
                        _ => Err(DhcpError::GenericError("Unknown relay option.".to_string())),
                    }
                }
                _ => Err(DhcpError::GenericError(format!(
                    "option is not matched, got: {value:?}."
                ))),
            }
        } else {
            Err(DhcpError::MissingOption(option))
        }
    }
}

impl DecodedPacketTrait<MessageType> for DecodedPacket {
    fn get_option_val(
        &self,
        option: OptionCode,
        _relay_code: Option<RelayCode>,
    ) -> Result<MessageType, DhcpError> {
        if let Some(value) = self.packet.opts().get(option) {
            match value {
                DhcpOption::MessageType(x) => Ok(*x),
                _ => Err(DhcpError::GenericError(format!(
                    "Message type is not matched, got: {value:?}.",
                ))),
            }
        } else {
            Err(DhcpError::MissingOption(option))
        }
    }
}

impl DecodedPacketTrait<Option<Ipv4Addr>> for DecodedPacket {
    fn get_option_val(
        &self,
        option: OptionCode,
        _relay_code: Option<RelayCode>,
    ) -> Result<Option<Ipv4Addr>, DhcpError> {
        match self.packet.opts().get(option) {
            Some(value) => {
                if let DhcpOption::ServerIdentifier(x) = value {
                    Ok(Some(*x))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }
}

impl DecodedPacket {
    fn is_relayed(&self) -> Result<(), DhcpError> {
        // get gi address
        let giaddress = self.packet.giaddr();
        if giaddress.is_broadcast() || giaddress == Ipv4Addr::new(0, 0, 0, 0) {
            return Err(DhcpError::NonRelayedPacket(giaddress));
        }
        Ok(())
    }

    fn is_this_for_us(&self, config: &Config) -> Result<(), DhcpError> {
        if let Some(val) = self.get_option_val(OptionCode::ServerIdentifier, None)? {
            if Some(val) == config.dhcp_config.carbide_dhcp_server {
                return Ok(());
            }
            return Err(DhcpError::NotMyPacket(val.to_string()));
        }

        // No identifier sent by client. It can be for us
        Ok(())
    }

    fn get_vendor_string(&self) -> Option<String> {
        self.get_option_val(OptionCode::ClassIdentifier, None).ok()
    }

    fn get_link_select(&self) -> Option<String> {
        self.get_option_val(
            OptionCode::RelayAgentInformation,
            Some(RelayCode::LinkSelection),
        )
        .ok()
    }

    pub(super) fn get_circuit_id(&self) -> Option<String> {
        self.get_option_val(
            OptionCode::RelayAgentInformation,
            Some(RelayCode::AgentCircuitId),
        )
        .ok()
    }

    fn get_remote_id(&self) -> Option<String> {
        self.get_option_val(
            OptionCode::RelayAgentInformation,
            Some(RelayCode::AgentRemoteId),
        )
        .ok()
    }

    /// Returns option 50 only when it has the expected address form so service REQUEST validation
    /// cannot accept unrelated option data.
    fn requested_ip_address(&self) -> Option<Ipv4Addr> {
        match self.packet.opts().get(OptionCode::RequestedIpAddress) {
            Some(DhcpOption::RequestedIpAddress(address)) => Some(*address),
            _ => None,
        }
    }

    /// Copies the caller's option 61 so service responses preserve client-selected identity without
    /// using it for reservation lookup.
    fn client_identifier(&self) -> Option<Vec<u8>> {
        match self.packet.opts().get(OptionCode::ClientIdentifier) {
            Some(DhcpOption::ClientIdentifier(identifier)) => Some(identifier.clone()),
            _ => None,
        }
    }

    fn get_discovery_request(&self, handler: &dyn DhcpMode, circuit_id: &str) -> DhcpDiscovery {
        DhcpDiscovery {
            mac_address: util::u8_to_mac(self.packet.chaddr()),
            relay_address: self.packet.giaddr().to_string(),
            vendor_string: self.get_vendor_string(),
            link_address: self.get_link_select(),
            circuit_id: handler.get_circuit_id(self, circuit_id),
            remote_id: self.get_remote_id(),
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        }
    }

    /// Relay/Gateway IP is used as destination ip.
    /// Only exception is if ciaddr is not empty. If it is not empty means client already has a IP
    /// and listening on it.
    fn decide_dst_ip(
        &self,
        _message_type: MessageType,
        relay_response_port: u16,
    ) -> (Ipv4Addr, u16) {
        // Relayed packet.
        if self.packet.giaddr() != Ipv4Addr::from([0, 0, 0, 0]) {
            return (self.packet.giaddr(), relay_response_port);
        }

        // Client unicast packet. Lease renewal case.
        if self.packet.ciaddr() != Ipv4Addr::from([0, 0, 0, 0]) {
            return (self.packet.ciaddr(), 68); // Client is listening on port 68
        }

        // We don't know who sent this packet. Broadcast it back.
        (Ipv4Addr::from([255, 255, 255, 255]), 68)
    }
}

pub struct Packet {
    encoded_packet: Vec<u8>,
    sent_packet: Option<Message>,
    dst_address: Ipv4Addr,
    dst_port: u16,
    /// The reply's DHCP message type (an Offer, an Ack, a Nak), as the
    /// bounded label the send path counts lease grants under.
    message_type: Option<MessageTypeLabel>,
    service_response: bool,
    reply_retirement: ReplyRetirement,
}

impl Packet {
    /// Return the encoded DHCPv4 response bytes.
    pub fn encoded_packet(&self) -> &[u8] {
        &self.encoded_packet
    }

    pub(super) fn dst_address(&self) -> SocketAddrV4 {
        SocketAddrV4::new(self.dst_address, self.dst_port)
    }

    /// Returns whether a sent reply belongs to host DHCP and should update the host activity
    /// timestamp.
    pub fn records_host_activity(&self) -> bool {
        self.sent_packet.is_some() && !self.service_response
    }
}

impl Packet {
    /// Sends a prepared response through its listener generation, returning `NoReply` or `Retired`
    /// without socket or metric effects.
    pub async fn send(
        &self,
        dst_address: SocketAddrV4,
        socket: Arc<UdpSocket>,
    ) -> Result<ReplyStatus, String> {
        let Some(sent_packet) = self.sent_packet.as_ref() else {
            return Ok(ReplyStatus::NoReply);
        };
        let status = match self
            .reply_retirement
            .send_to(&socket, &self.encoded_packet, dst_address.into())
            .await
        {
            Ok(status) => status,
            Err(error) => {
                tracing::debug!(
                    packet.send = %sent_packet,
                    destination_address = %dst_address,
                    error = %error,
                    "Failed to send DHCP packet"
                );
                return Err(error.to_string());
            }
        };
        // A retired generation must not log or count a response that never reached the socket.
        if status == ReplyStatus::Retired {
            return Ok(status);
        }

        emit(DhcpReplySent {
            message_type: self.message_type.unwrap_or(MessageTypeLabel::Other),
            destination_address: dst_address,
            xid: i64::from(sent_packet.xid()),
            broadcast_flag: sent_packet.flags().broadcast(),
            ciaddr: sent_packet.ciaddr(),
            yiaddr: sent_packet.yiaddr(),
            siaddr: sent_packet.siaddr(),
            giaddr: sent_packet.giaddr(),
            chaddr: util::u8_to_mac(sent_packet.chaddr()),
        });
        tracing::debug!(packet.send = %sent_packet, "Sent DHCP packet");

        Ok(status)
    }
}

pub async fn process_packet(
    buf: &[u8],
    source_address: SocketAddr,
    config: &Config,
    circuit_id: &str,
    handler: &dyn DhcpMode,
    machine_cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
) -> Result<Packet, DhcpError> {
    let ipv4 = config
        .ipv4()?
        .ok_or_else(|| DhcpError::InvalidInput("DHCPv4 is disabled".to_string()))?;
    let (&bootp_op, _) = buf.split_first().ok_or(DhcpError::PacketDecodeFailure(
        dhcproto::error::DecodeError::NotEnoughBytes,
    ))?;

    if bootp_op != PKT_TYPE_OP_REQUEST {
        // Not valid packet. Drop it.
        return Err(DhcpError::UnknownPacket(bootp_op));
    }

    let packet = Message::decode(&mut Decoder::new(buf))?;
    let hardware_address_len = packet.hlen();
    if hardware_address_len > BOOTP_CHADDR_LEN {
        return Err(DhcpError::InvalidInput(format!(
            "DHCP hardware address length {hardware_address_len} exceeds the {BOOTP_CHADDR_LEN}-byte BOOTP field"
        )));
    }
    let decoded_packet = DecodedPacket { packet };

    // Every packet with a usable decoded BOOTP header counts as a received
    // request, labelled by its message type (`other` when the option is
    // missing or unrecognised). The Result is consumed after the relay and
    // ownership checks so that error precedence stays unchanged.
    let msg_type: Result<MessageType, DhcpError> =
        decoded_packet.get_option_val(OptionCode::MessageType, None);
    emit(DhcpRequestReceived {
        message_type: msg_type
            .as_ref()
            .map_or(MessageTypeLabel::Other, |msg_type| {
                MessageTypeLabel::from(*msg_type)
            }),
        bootp_op: i64::from(bootp_op),
        source_address,
        xid: i64::from(decoded_packet.packet.xid()),
        broadcast_flag: decoded_packet.packet.flags().broadcast(),
        ciaddr: decoded_packet.packet.ciaddr(),
        yiaddr: decoded_packet.packet.yiaddr(),
        siaddr: decoded_packet.packet.siaddr(),
        giaddr: decoded_packet.packet.giaddr(),
        chaddr: util::u8_to_mac(decoded_packet.packet.chaddr()),
    });
    tracing::debug!(packet.received = %decoded_packet.packet, "Received Packet");

    if handler.should_be_relayed() {
        decoded_packet.is_relayed()?;
    }
    decoded_packet.is_this_for_us(config)?;

    let msg_type = msg_type?;
    let client_mac = util::u8_to_mac(decoded_packet.packet.chaddr());
    // Resolve static service reservations before host discovery; DPU lookup fails closed on
    // configured service interfaces.
    if let Some(reservation) = handler.service_reservation(config, circuit_id, &client_mac)? {
        let ServiceLink::Ipv4(link) = &reservation.link else {
            return Err(DhcpError::InvalidInput(format!(
                "IPv6 service reservation {} cannot answer DHCPv4",
                reservation.internal_uuid
            )));
        };
        let Some(packet) =
            create_service_reply_packet(&decoded_packet, link, config, ipv4, msg_type)?
        else {
            let (dst_address, dst_port) =
                decoded_packet.decide_dst_ip(msg_type, config.relay_response_port);
            return Ok(Packet {
                encoded_packet: Vec::new(),
                sent_packet: None,
                dst_address,
                dst_port,
                message_type: None,
                service_response: true,
                reply_retirement: config.reply_retirement.clone(),
            });
        };
        // A direct NAK rejects ciaddr, so sending the rejection to that address could make it unreachable.
        let (dst_address, dst_port) = if packet.opts().msg_type() == Some(MessageType::Nak)
            && decoded_packet.packet.giaddr().is_unspecified()
        {
            (Ipv4Addr::BROADCAST, 68)
        } else {
            decoded_packet.decide_dst_ip(msg_type, config.relay_response_port)
        };
        return encode_packet(packet, dst_address, dst_port, true, config);
    }

    let dhcp_response = handler
        .discover_dhcp(
            decoded_packet.get_discovery_request(handler, circuit_id),
            config,
            machine_cache,
        )
        .await?;

    let (dst_address, dst_port) =
        decoded_packet.decide_dst_ip(msg_type, config.relay_response_port);

    let packet = create_dhcp_reply_packet(
        &decoded_packet,
        circuit_id,
        dhcp_response,
        config,
        ipv4,
        msg_type,
    )?;

    encode_packet(packet, dst_address, dst_port, false, config)
}

/// Encodes one DHCPv4 response with its immutable generation guard so every caller shares the same
/// retirement check.
fn encode_packet(
    packet: Message,
    dst_address: Ipv4Addr,
    dst_port: u16,
    service_response: bool,
    config: &Config,
) -> Result<Packet, DhcpError> {
    let reply_message_type = packet
        .opts()
        .msg_type()
        .map_or(MessageTypeLabel::Other, MessageTypeLabel::from);
    let mut encoded_packet = Vec::new();
    let mut encoder = Encoder::new(&mut encoded_packet);
    if let Err(error) = packet.encode(&mut encoder) {
        tracing::debug!(packet.encode = %packet, error = %error, "Failed to encode DHCP packet");
        return Err(error.into());
    }
    Ok(Packet {
        encoded_packet,
        sent_packet: Some(packet),
        dst_address,
        dst_port,
        message_type: Some(reply_message_type),
        service_response,
        reply_retirement: config.reply_retirement.clone(),
    })
}

/// Builds a minimal fixed-address DHCPv4 service response so host identity, DNS, PXE, and MTU data
/// never cross into a service VRF.
fn create_service_reply_packet(
    src: &DecodedPacket,
    link: &ServiceLinkV4,
    config: &Config,
    ipv4: DhcpV4Config,
    dhcp_msg_type: MessageType,
) -> Result<Option<Message>, DhcpError> {
    let reply_message_type = match dhcp_msg_type {
        MessageType::Discover => MessageType::Offer,
        MessageType::Request => {
            // A bound ciaddr identifies renewals; otherwise option 50 identifies selecting and INIT-REBOOT requests.
            let requested_address = if src.packet.ciaddr().is_unspecified() {
                src.requested_ip_address()
            } else {
                Some(src.packet.ciaddr())
            };
            if requested_address != Some(link.address) {
                return service_nak_packet(src, ipv4.server).map(Some);
            }
            MessageType::Ack
        }
        // A service address is statically owned, so RELEASE has no allocation state to mutate or acknowledge.
        MessageType::Release => return Ok(None),
        MessageType::Decline => {
            return Err(DhcpError::DhcpDeclineMessage(
                src.packet.ciaddr().to_string(),
                util::u8_to_mac(src.packet.chaddr()),
            ));
        }
        _ => return Err(DhcpError::UnhandledMessageType(dhcp_msg_type)),
    };

    let mut message = Message::default();
    message
        .set_opcode(dhcproto::v4::Opcode::BootReply)
        .set_htype(dhcproto::v4::HType::Eth)
        .set_hops(0)
        .set_xid(src.packet.xid())
        .set_secs(0)
        .set_flags(src.packet.flags())
        .set_ciaddr(src.packet.ciaddr())
        .set_yiaddr(link.address)
        .set_siaddr(Ipv4Addr::UNSPECIFIED)
        .set_giaddr(src.packet.giaddr())
        .set_chaddr(src.packet.chaddr());

    message
        .opts_mut()
        .insert(DhcpOption::MessageType(reply_message_type));
    message
        .opts_mut()
        .insert(DhcpOption::SubnetMask(link.prefix.mask()));
    message
        .opts_mut()
        .insert(DhcpOption::Router(vec![link.gateway]));
    message.opts_mut().insert(DhcpOption::AddressLeaseTime(
        config.dhcp_config.lease_time_secs,
    ));
    message
        .opts_mut()
        .insert(DhcpOption::ServerIdentifier(ipv4.server));
    message
        .opts_mut()
        .insert(DhcpOption::Renewal(config.dhcp_config.renewal_time_secs));
    message.opts_mut().insert(DhcpOption::Rebinding(
        config.dhcp_config.rebinding_time_secs,
    ));

    if let Some(client_identifier) = src.client_identifier() {
        message
            .opts_mut()
            .insert(DhcpOption::ClientIdentifier(client_identifier));
    }
    Ok(Some(message))
}

/// Builds a service-specific NAK without provisioning-server data or rejected ciaddr so invalid
/// client state and host PXE connectivity are not echoed.
fn service_nak_packet(
    src: &DecodedPacket,
    carbide_dhcp_server: Ipv4Addr,
) -> Result<Message, DhcpError> {
    let mut message = nak_packet(
        src,
        Ipv4Addr::UNSPECIFIED,
        carbide_dhcp_server,
        src.client_identifier(),
    )?;
    // The rejected address is not valid client state and must not be echoed in a service NAK.
    message.set_ciaddr(Ipv4Addr::UNSPECIFIED);
    Ok(message)
}

fn create_dhcp_reply_packet(
    src: &DecodedPacket,
    circuit_id: &str,
    forge_response: DhcpRecord,
    config: &Config,
    ipv4: DhcpV4Config,
    dhcp_msg_type: MessageType,
) -> Result<Message, DhcpError> {
    let relay_address = forge_response
        .gateway
        .clone()
        .map(|x| {
            x.parse::<Ipv4Addr>()
                .unwrap_or_else(|_| Ipv4Addr::from([0, 0, 0, 0]))
        })
        .unwrap_or(ipv4.server);
    let allocated_address = Ipv4Addr::from_str(&forge_response.address)?;
    let reply_message_type = match dhcp_msg_type {
        MessageType::Discover => MessageType::Offer,
        // This can be 0 as per the rfc2131. If 0, send the allocated address.
        MessageType::Request if src.packet.ciaddr() == Ipv4Addr::from([0, 0, 0, 0]) => {
            MessageType::Ack
        }
        // This is the case of IP renew.
        // We are able to allocate the same IP to client as requested by it.
        MessageType::Request if src.packet.ciaddr() == allocated_address => MessageType::Ack,
        // This means allocated IP address is not same as requested by the client. Send NAK.
        MessageType::Request => {
            return nak_packet(
                src,
                ipv4.provisioning_server,
                ipv4.server,
                Some(ethernet_client_identifier(src)),
            );
        }
        MessageType::Decline => {
            return Err(DhcpError::DhcpDeclineMessage(
                src.packet.ciaddr().to_string(),
                src.packet
                    .chaddr()
                    .iter()
                    .map(|x| format!("{x:x}"))
                    .collect::<Vec<String>>()
                    .join(":"),
            ));
        }
        _ => {
            return Err(DhcpError::UnhandledMessageType(dhcp_msg_type));
        }
    };

    let parse = forge_response.prefix.parse::<IpNetwork>();
    let (prefix, broadcast) = match parse {
        Ok(prefix) => match prefix {
            IpNetwork::V4(prefix) => (prefix.mask(), prefix.broadcast()),
            // Kept for safety until v6 path is at parity.
            IpNetwork::V6(prefix) => {
                return Err(DhcpError::GenericError(format!(
                    "Prefix ({prefix}) is an IPv6 network, which is not supported."
                )));
            }
        },
        Err(error) => {
            return Err(DhcpError::GenericError(format!(
                "prefix value in deserialized protobuf is not an IP Network: {error}"
            )));
        }
    };

    let vendor_string = src.get_vendor_string();

    let vendor_class = if let Some(vendor_string) = vendor_string {
        Some(VendorClass::from_str(vendor_string.as_str()).map_err(|e| {
            DhcpError::VendorClassParseError(format!("Vendor string parse failed: {e:?}"))
        })?)
    } else {
        None
    };

    // https://www.ietf.org/rfc/rfc2131.txt
    let mut msg = Message::default();
    msg.set_opcode(dhcproto::v4::Opcode::BootReply)
        .set_htype(dhcproto::v4::HType::Eth)
        .set_hops(0x0)
        .set_xid(src.packet.xid())
        .set_secs(0)
        .set_flags(src.packet.flags())
        .set_ciaddr(src.packet.ciaddr())
        .set_yiaddr(allocated_address)
        .set_siaddr(ipv4.provisioning_server)
        .set_giaddr(src.packet.giaddr())
        .set_chaddr(src.packet.chaddr());

    msg.opts_mut()
        .insert(DhcpOption::MessageType(reply_message_type));
    msg.opts_mut().insert(DhcpOption::SubnetMask(prefix));
    msg.opts_mut()
        .insert(DhcpOption::Router(vec![relay_address]));
    msg.opts_mut().insert(DhcpOption::NameServer(
        config.dhcp_config.carbide_nameservers.clone(),
    ));
    msg.opts_mut().insert(DhcpOption::DomainNameServer(
        config.dhcp_config.carbide_nameservers.clone(),
    ));
    msg.opts_mut()
        .insert(DhcpOption::DomainName(forge_response.fqdn.clone()));
    msg.opts_mut()
        .insert(DhcpOption::Hostname(forge_response.fqdn.clone()));

    // // I guess we don't need Client_FQDN. Option12, Hostname seems sufficient.
    // let mut client_fqdn = ClientFQDN::new(
    //     FqdnFlags::new(0x0e),
    //     Name::from_str(&forge_response.fqdn.clone())
    //         .map_err(|x| DhcpError::GenericError(x.to_string()))?,
    // );
    // client_fqdn.set_r1(0);
    // client_fqdn.set_r2(0);
    // msg.opts_mut().insert(DhcpOption::ClientFQDN(client_fqdn));

    msg.opts_mut().insert(DhcpOption::BroadcastAddr(broadcast));
    msg.opts_mut().insert(DhcpOption::AddressLeaseTime(
        config.dhcp_config.lease_time_secs,
    ));
    msg.opts_mut()
        .insert(DhcpOption::ServerIdentifier(ipv4.server));
    msg.opts_mut()
        .insert(DhcpOption::Renewal(config.dhcp_config.renewal_time_secs));
    msg.opts_mut().insert(DhcpOption::Rebinding(
        config.dhcp_config.rebinding_time_secs,
    ));

    msg.opts_mut().insert(DhcpOption::InterfaceMtu(get_mtu(
        circuit_id,
        config.host_config.as_ref(),
    )));

    let client_identifier = ethernet_client_identifier(src);
    msg.opts_mut()
        .insert(DhcpOption::ClientIdentifier(client_identifier));

    msg.opts_mut().insert(DhcpOption::NtpServers(
        config.dhcp_config.carbide_ntpservers.clone(),
    ));

    if let Some(vendor_class) = vendor_class {
        msg.opts_mut().insert(DhcpOption::ClassIdentifier(
            vendor_class.id.as_bytes().to_vec(),
        ));

        if vendor_class.is_netboot() {
            msg.opts_mut()
                .insert(DhcpOption::BootfileName(util::machine_get_filename(
                    &forge_response,
                    &vendor_class,
                    ipv4.provisioning_server,
                )));
        }
    }

    let mut relay_agent = RelayAgentInformation::default();
    let circuit_id = src.get_circuit_id();
    if let Some(circuit_id) = circuit_id {
        relay_agent.insert(RelayInfo::AgentCircuitId(circuit_id.as_bytes().to_vec()));
    }

    let remote_id = src.get_remote_id();
    if let Some(remote_id) = remote_id {
        relay_agent.insert(RelayInfo::AgentRemoteId(remote_id.as_bytes().to_vec()));
    }

    let link_select = src.get_link_select();

    if let Some(link_select) = link_select {
        relay_agent.insert(RelayInfo::LinkSelection(Ipv4Addr::from_str(
            link_select.as_str(),
        )?));
    }

    if !relay_agent.is_empty() {
        let agent_options = DhcpOption::RelayAgentInformation(relay_agent);
        msg.opts_mut().insert(agent_options);
    }

    let mut vendor_option: Vec<u8> = vec![6, 4, 0, 0, 0, 8, 70];
    let mut machine_id = forge_response
        .machine_interface_id
        .map(|x| x.to_string())
        .unwrap_or_default()
        .as_bytes()
        .to_vec();

    vendor_option.push(machine_id.len() as u8);
    vendor_option.append(&mut machine_id);

    msg.opts_mut()
        .insert(DhcpOption::VendorExtensions(vendor_option));

    Ok(msg)
}

fn nak_packet(
    src: &DecodedPacket,
    carbide_provisioning_server_ipv4: Ipv4Addr,
    carbide_dhcp_server: Ipv4Addr,
    client_identifier: Option<Vec<u8>>,
) -> Result<Message, DhcpError> {
    // https://www.ietf.org/rfc/rfc2131.txt
    let mut msg = Message::default();
    msg.set_opcode(dhcproto::v4::Opcode::BootReply)
        .set_htype(dhcproto::v4::HType::Eth)
        .set_hops(0x0)
        .set_xid(src.packet.xid())
        .set_secs(0)
        .set_flags(src.packet.flags())
        .set_ciaddr(src.packet.ciaddr())
        .set_yiaddr(Ipv4Addr::from([0, 0, 0, 0]))
        .set_siaddr(carbide_provisioning_server_ipv4)
        .set_giaddr(src.packet.giaddr())
        .set_chaddr(src.packet.chaddr());

    msg.opts_mut()
        .insert(DhcpOption::MessageType(MessageType::Nak));

    if let Some(client_identifier) = client_identifier {
        msg.opts_mut()
            .insert(DhcpOption::ClientIdentifier(client_identifier));
    }
    msg.opts_mut()
        .insert(DhcpOption::ServerIdentifier(carbide_dhcp_server));

    Ok(msg)
}

/// Builds the legacy Ethernet option 61 used by host DHCP replies so service echo behavior does not
/// alter host compatibility.
fn ethernet_client_identifier(src: &DecodedPacket) -> Vec<u8> {
    let mut identifier = Vec::with_capacity(src.packet.chaddr().len() + 1);
    identifier.push(1);
    identifier.extend_from_slice(src.packet.chaddr());
    identifier
}

fn get_mtu(circuit_id: &str, host_config: Option<&HostConfig>) -> u16 {
    host_config
        .map(|x| x.host_ip_addresses.clone())
        .unwrap_or_default()
        .get(circuit_id)
        .get_or_insert(&InterfaceInfo::default())
        .mtu
        .unwrap_or(1500)
        .try_into()
        .unwrap_or(1500)
}
mod test {
    #[test]
    fn test_get_mtu() {
        let interface_mtu_none = crate::packet_handler::InterfaceInfo {
            address: Some(
                <std::net::Ipv4Addr as std::str::FromStr>::from_str("10.12.1.2")
                    .ok()
                    .unwrap(),
            ),
            gateway: Some(
                <std::net::Ipv4Addr as std::str::FromStr>::from_str("10.12.1.2")
                    .ok()
                    .unwrap(),
            ),
            prefix: Some("24".to_string()),
            fqdn: "fqdn1".to_string(),
            booturl: None,
            mtu: None,
            ipv6: None,
        };
        let interface_mtu_9000 = crate::packet_handler::InterfaceInfo {
            address: Some(
                <std::net::Ipv4Addr as std::str::FromStr>::from_str("20.22.2.2")
                    .ok()
                    .unwrap(),
            ),
            gateway: Some(
                <std::net::Ipv4Addr as std::str::FromStr>::from_str("20.22.2.2")
                    .ok()
                    .unwrap(),
            ),
            prefix: Some("16".to_string()),
            fqdn: "fqdn2".to_string(),
            booturl: None,
            mtu: Some(9000),
            ipv6: None,
        };
        let mut interface_mtu_65537 = interface_mtu_none.clone();
        interface_mtu_65537.mtu = Some(65537);

        let mut interface_mtu_12000 = interface_mtu_none.clone();
        interface_mtu_12000.mtu = Some(12000);

        let mut tree =
            std::collections::BTreeMap::<String, crate::packet_handler::InterfaceInfo>::new();
        let mut expected = std::collections::BTreeMap::<String, u16>::new();

        tree.insert("interface_mtu_none".to_string(), interface_mtu_none);
        expected.insert("interface_mtu_none".to_string(), 1500);
        tree.insert("interface_mtu_9000".to_string(), interface_mtu_9000);
        expected.insert("interface_mtu_9000".to_string(), 9000);
        tree.insert("interface_mtu_65537".to_string(), interface_mtu_65537);
        expected.insert("interface_mtu_65537".to_string(), 1500);
        tree.insert("interface_mtu_12000".to_string(), interface_mtu_12000);
        expected.insert("interface_mtu_12000".to_string(), 12000);

        let host_config: carbide_rpc_utils::dhcp::HostConfig =
            carbide_rpc_utils::dhcp::HostConfig {
                host_interface_id:
                    <carbide_uuid::machine::MachineInterfaceId as std::str::FromStr>::from_str(
                        "959888da-cdc8-4079-8d23-8a09832447ce",
                    )
                    .ok()
                    .unwrap(),
                host_ip_addresses: tree.clone(),
            };

        for (circuit_id, expected_mtu) in expected.iter() {
            println!("Checking circuit_id: {}", circuit_id);
            assert_eq!(
                *expected_mtu,
                crate::packet_handler::get_mtu(circuit_id, Some(&host_config))
            );
        }
        println!("Checking circuit_id: host_config_none");
        assert_eq!(
            1500,
            crate::packet_handler::get_mtu("host_config_none", None)
        );
    }
}
