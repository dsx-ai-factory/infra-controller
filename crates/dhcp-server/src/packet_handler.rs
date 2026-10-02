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
use carbide_rpc_utils::dhcp::{HostConfig, InterfaceInfo};
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
use crate::{Config, util};

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
            if val == config.dhcp_config.carbide_dhcp_server {
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
    sent_packet: Message,
    dst_address: Ipv4Addr,
    dst_port: u16,
    /// The reply's DHCP message type (an Offer, an Ack, a Nak), as the
    /// bounded label the send path counts lease grants under.
    message_type: MessageTypeLabel,
}

impl Packet {
    /// Return the encoded DHCPv4 response bytes.
    pub fn encoded_packet(&self) -> &[u8] {
        &self.encoded_packet
    }

    pub(super) fn dst_address(&self) -> SocketAddrV4 {
        SocketAddrV4::new(self.dst_address, self.dst_port)
    }
}

impl Packet {
    pub async fn send(
        &self,
        dst_address: SocketAddrV4,
        socket: Arc<UdpSocket>,
    ) -> Result<(), String> {
        if let Err(error) = socket.send_to(&self.encoded_packet, dst_address).await {
            tracing::debug!(
                packet.send = %self.sent_packet,
                destination_address = %dst_address,
                error = %error,
                "Failed to send DHCP packet"
            );
            return Err(error.to_string());
        }

        emit(DhcpReplySent {
            message_type: self.message_type,
            destination_address: dst_address,
            xid: i64::from(self.sent_packet.xid()),
            broadcast_flag: self.sent_packet.flags().broadcast(),
            ciaddr: self.sent_packet.ciaddr(),
            yiaddr: self.sent_packet.yiaddr(),
            siaddr: self.sent_packet.siaddr(),
            giaddr: self.sent_packet.giaddr(),
            chaddr: util::u8_to_mac(self.sent_packet.chaddr()),
        });
        tracing::debug!(packet.send = %self.sent_packet, "Sent DHCP packet");

        Ok(())
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
    let dhcp_response = handler
        .discover_dhcp(
            decoded_packet.get_discovery_request(handler, circuit_id),
            config,
            machine_cache,
        )
        .await?;

    let (dst_address, dst_port) =
        decoded_packet.decide_dst_ip(msg_type, config.relay_response_port);

    let packet =
        create_dhcp_reply_packet(&decoded_packet, circuit_id, dhcp_response, config, msg_type)?;

    // Read the type off the reply itself: create_dhcp_reply_packet answers a
    // Request with either an Ack or a Nak.
    let reply_message_type = packet
        .opts()
        .msg_type()
        .map_or(MessageTypeLabel::Other, MessageTypeLabel::from);

    let mut encoded_packet = Vec::new();
    let mut e = Encoder::new(&mut encoded_packet);
    if let Err(error) = packet.encode(&mut e) {
        tracing::debug!(
            packet.encode = %packet,
            error = %error,
            "Failed to encode DHCP packet"
        );
        return Err(error.into());
    }

    Ok(Packet {
        encoded_packet,
        sent_packet: packet,
        dst_address,
        dst_port,
        message_type: reply_message_type,
    })
}

fn create_dhcp_reply_packet(
    src: &DecodedPacket,
    circuit_id: &str,
    forge_response: DhcpRecord,
    config: &Config,
    dhcp_msg_type: MessageType,
) -> Result<Message, DhcpError> {
    let relay_address = forge_response
        .gateway
        .clone()
        .map(|x| {
            x.parse::<Ipv4Addr>()
                .unwrap_or_else(|_| Ipv4Addr::from([0, 0, 0, 0]))
        })
        .unwrap_or(config.dhcp_config.carbide_dhcp_server);
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
                config.dhcp_config.carbide_provisioning_server_ipv4,
                config.dhcp_config.carbide_dhcp_server,
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
        .set_siaddr(config.dhcp_config.carbide_provisioning_server_ipv4)
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
    msg.opts_mut().insert(DhcpOption::ServerIdentifier(
        config.dhcp_config.carbide_dhcp_server,
    ));
    msg.opts_mut()
        .insert(DhcpOption::Renewal(config.dhcp_config.renewal_time_secs));
    msg.opts_mut().insert(DhcpOption::Rebinding(
        config.dhcp_config.rebinding_time_secs,
    ));

    msg.opts_mut().insert(DhcpOption::InterfaceMtu(get_mtu(
        circuit_id,
        config.host_config.as_ref(),
    )));

    let mut client_identifier: Vec<u8> = Vec::with_capacity(src.packet.chaddr().len() + 1);
    client_identifier.push(1); // ethernet
    src.packet
        .chaddr()
        .iter()
        .for_each(|x| client_identifier.push(*x));
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
                    config,
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

    let mut client_identifier: Vec<u8> = Vec::with_capacity(src.packet.chaddr().len() + 1);
    client_identifier.push(1); // ethernet
    src.packet
        .chaddr()
        .iter()
        .for_each(|x| client_identifier.push(*x));
    msg.opts_mut()
        .insert(DhcpOption::ClientIdentifier(client_identifier));
    msg.opts_mut()
        .insert(DhcpOption::ServerIdentifier(carbide_dhcp_server));

    Ok(msg)
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
