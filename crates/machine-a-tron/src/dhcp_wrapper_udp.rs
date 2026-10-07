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
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dhcproto::v4::relay::{RelayAgentInformation, RelayInfo};
use dhcproto::v4::{
    Decodable, Decoder, DhcpOption, HType, Message, MessageType, Opcode, OptionCode,
};
use dhcproto::{Encodable, Encoder};
use mac_address::MacAddress;
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::dhcp_wrapper::{DhcpRelayError, DhcpRelayResult, DhcpRequestInfo, DhcpResponseInfo};

const DHCP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const DHCP_PACKET_BUFFER_SIZE: usize = 4096;

#[derive(Clone, Debug)]
pub struct UdpDhcpClient {
    socket: Arc<UdpSocket>,
    server_address: SocketAddrV4,
    advertise_address: Ipv4Addr,
    next_xid: Arc<AtomicU32>,
    pending: PendingResponses,
}

type PendingResponses = Arc<Mutex<HashMap<u32, PendingResponse>>>;

#[derive(Debug)]
struct PendingResponse {
    sender: oneshot::Sender<DhcpRelayResult<Message>>,
    mac_address: MacAddress,
    expected_type: MessageType,
    registration: Arc<()>,
}

#[derive(Debug)]
struct PendingResponseGuard {
    pending: PendingResponses,
    xid: u32,
    registration: Arc<()>,
}

impl Drop for PendingResponseGuard {
    fn drop(&mut self) {
        let mut pending = self
            .pending
            .lock()
            .expect("pending responses lock poisoned");
        if pending
            .get(&self.xid)
            .is_some_and(|response| Arc::ptr_eq(&response.registration, &self.registration))
        {
            pending.remove(&self.xid);
        }
    }
}

#[derive(Debug)]
pub struct UdpDhcpService {
    cancellation: CancellationToken,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

impl UdpDhcpService {
    pub async fn shutdown(mut self) -> eyre::Result<()> {
        self.cancellation.cancel();
        self.task
            .take()
            .expect("UDP DHCP task is present")
            .await??;
        Ok(())
    }
}

impl Drop for UdpDhcpService {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl UdpDhcpClient {
    pub(crate) async fn start(
        server_address: SocketAddrV4,
        listen_address: SocketAddrV4,
        advertise_address: Ipv4Addr,
    ) -> std::io::Result<(Self, UdpDhcpService)> {
        let socket = Arc::new(UdpSocket::bind(listen_address).await?);
        let pending = PendingResponses::default();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(receive_responses(
            socket.clone(),
            pending.clone(),
            advertise_address,
            cancellation.clone(),
        ));

        Ok((
            Self {
                socket,
                server_address,
                advertise_address,
                next_xid: Arc::new(AtomicU32::new(rand::random())),
                pending,
            },
            UdpDhcpService {
                cancellation,
                task: Some(task),
            },
        ))
    }

    pub(crate) async fn request_ip(
        &self,
        request_info: DhcpRequestInfo,
    ) -> DhcpRelayResult<DhcpResponseInfo> {
        tracing::debug!(
            mac_address = %request_info.mac_address,
            relay_address = %request_info.relay_address,
            server_address = %self.server_address,
            "Requesting IP address through UDP relay",
        );

        let xid = self.next_xid.fetch_add(1, Ordering::Relaxed);
        let discover = self.request_message(
            xid,
            request_info.mac_address,
            request_info.relay_address,
            request_info.vendor_class,
            MessageType::Discover,
        );
        let offer = self
            .exchange(discover, request_info.mac_address, MessageType::Offer)
            .await?;
        let offered_address = offer.yiaddr();
        if offered_address.is_unspecified() {
            return Err(DhcpRelayError::InvalidDhcpRecord(
                "OFFER does not contain an IPv4 address".to_string(),
            ));
        }
        let offered_by = server_identifier(&offer)?;

        let mut request = self.request_message(
            xid,
            request_info.mac_address,
            request_info.relay_address,
            request_info.vendor_class,
            MessageType::Request,
        );
        request
            .opts_mut()
            .insert(DhcpOption::RequestedIpAddress(offered_address));
        request
            .opts_mut()
            .insert(DhcpOption::ServerIdentifier(offered_by));

        let ack = self
            .exchange(request, request_info.mac_address, MessageType::Ack)
            .await?;
        if server_identifier(&ack)? != offered_by {
            return Err(DhcpRelayError::InvalidDhcpPacket(
                "ACK came from a different DHCP server than the OFFER".to_string(),
            ));
        }
        if ack.yiaddr().is_unspecified() {
            return Err(DhcpRelayError::InvalidDhcpRecord(
                "ACK does not contain an assigned IPv4 address".to_string(),
            ));
        }
        if ack.yiaddr() != offered_address {
            return Err(DhcpRelayError::InvalidDhcpPacket(format!(
                "ACK assigned {}, but the selected OFFER assigned {offered_address}",
                ack.yiaddr(),
            )));
        }

        tracing::info!(
            mac_address = %request_info.mac_address,
            relay_address = %request_info.relay_address,
            assigned_address = %ack.yiaddr(),
            "DHCP relay request received an address",
        );
        Ok(DhcpResponseInfo {
            ip_address: ack.yiaddr(),
        })
    }

    fn request_message(
        &self,
        xid: u32,
        mac_address: MacAddress,
        link_selection: Ipv4Addr,
        vendor_class: Option<&str>,
        message_type: MessageType,
    ) -> Message {
        let mut relay_information = RelayAgentInformation::default();
        relay_information.insert(RelayInfo::LinkSelection(link_selection));

        let mut message = Message::default();
        message
            .set_opcode(Opcode::BootRequest)
            .set_htype(HType::Eth)
            .set_hops(1)
            .set_xid(xid)
            .set_chaddr(&mac_address.bytes())
            .set_giaddr(self.advertise_address);
        message
            .opts_mut()
            .insert(DhcpOption::MessageType(message_type));
        message
            .opts_mut()
            .insert(DhcpOption::RelayAgentInformation(relay_information));
        if let Some(vendor_class) = vendor_class {
            message.opts_mut().insert(DhcpOption::ClassIdentifier(
                vendor_class.as_bytes().to_vec(),
            ));
        }
        message
    }

    async fn exchange(
        &self,
        request: Message,
        mac_address: MacAddress,
        expected_type: MessageType,
    ) -> DhcpRelayResult<Message> {
        let xid = request.xid();
        let mut bytes = Vec::with_capacity(300);
        request
            .encode(&mut Encoder::new(&mut bytes))
            .map_err(|error| DhcpRelayError::InvalidDhcpPacket(error.to_string()))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let registration = Arc::new(());
        {
            let mut pending = self
                .pending
                .lock()
                .expect("pending responses lock poisoned");
            match pending.entry(xid) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(PendingResponse {
                        sender: reply_tx,
                        mac_address,
                        expected_type,
                        registration: registration.clone(),
                    });
                }
                std::collections::hash_map::Entry::Occupied(_) => {
                    return Err(DhcpRelayError::TransactionCollision(xid));
                }
            }
        }
        let _pending_response_guard = PendingResponseGuard {
            pending: self.pending.clone(),
            xid,
            registration,
        };

        self.socket.send_to(&bytes, self.server_address).await?;

        match tokio::time::timeout(DHCP_RESPONSE_TIMEOUT, reply_rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => Err(DhcpRelayError::ResponseReceiverStopped),
            Err(_) => Err(DhcpRelayError::ResponseTimeout { xid, expected_type }),
        }
    }
}

async fn receive_responses(
    socket: Arc<UdpSocket>,
    pending: PendingResponses,
    advertise_address: Ipv4Addr,
    cancellation: CancellationToken,
) -> std::io::Result<()> {
    let mut buffer = vec![0; DHCP_PACKET_BUFFER_SIZE];
    loop {
        let (length, source) = tokio::select! {
            () = cancellation.cancelled() => return Ok(()),
            result = socket.recv_from(&mut buffer) => result?,
        };
        let response = match Message::decode(&mut Decoder::new(&buffer[..length])) {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(%source, length, %error, "Ignoring malformed DHCP response");
                continue;
            }
        };
        let xid = response.xid();
        let mut pending = pending.lock().expect("pending responses lock poisoned");
        let Some(transaction) = pending.get(&xid) else {
            tracing::debug!(%source, xid, "Ignoring DHCP response with unknown transaction ID");
            continue;
        };
        let result = validate_response(
            &response,
            xid,
            transaction.mac_address,
            advertise_address,
            transaction.expected_type,
            source,
        );
        let terminal_result = match result {
            Ok(()) => Ok(response),
            Err(error @ DhcpRelayError::NegativeAcknowledgement(_)) => Err(error),
            Err(error) => {
                tracing::debug!(%source, xid, %error, "Ignoring invalid DHCP response");
                continue;
            }
        };
        let transaction = pending
            .remove(&xid)
            .expect("validated DHCP transaction is still pending");
        transaction.sender.send(terminal_result).ok();
    }
}

fn validate_response(
    response: &Message,
    xid: u32,
    mac_address: MacAddress,
    advertise_address: Ipv4Addr,
    expected_type: MessageType,
    source: SocketAddr,
) -> DhcpRelayResult<()> {
    // A DHCP server sends replies to giaddr as a new UDP flow, so neither the
    // source IP nor source port has to match the configured request destination.
    // Correlate replies using the DHCP transaction fields instead.
    if response.opcode() != Opcode::BootReply
        || response.xid() != xid
        || response.chaddr() != mac_address.bytes()
        || response.giaddr() != advertise_address
    {
        return Err(DhcpRelayError::InvalidDhcpPacket(format!(
            "response from {source} does not match DHCP transaction {xid}"
        )));
    }
    let actual_type = response.opts().msg_type().ok_or_else(|| {
        DhcpRelayError::InvalidDhcpPacket("response is missing DHCP message type".to_string())
    })?;
    server_identifier(response)?;
    if actual_type == MessageType::Nak {
        return Err(DhcpRelayError::NegativeAcknowledgement(xid));
    }
    if actual_type != expected_type {
        return Err(DhcpRelayError::InvalidDhcpPacket(format!(
            "expected {expected_type:?} for transaction {xid}, received {actual_type:?}"
        )));
    }
    Ok(())
}

fn server_identifier(message: &Message) -> DhcpRelayResult<Ipv4Addr> {
    match message.opts().get(OptionCode::ServerIdentifier) {
        Some(DhcpOption::ServerIdentifier(address)) => Ok(*address),
        _ => Err(DhcpRelayError::InvalidDhcpPacket(
            "DHCP response is missing server identifier".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};
    use dhcproto::v4::relay::{RelayCode, RelayInfo};

    use super::*;

    const ASSIGNED_ADDRESS: Ipv4Addr = Ipv4Addr::new(172, 21, 0, 42);
    const SERVER_IDENTIFIER: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);

    #[derive(Clone, Copy, Debug)]
    enum ResponseVariation {
        Valid,
        DifferentSourceIp,
        DifferentSourcePort,
        MissingServerIdentifier,
        DifferentServerIdentifier,
        WrongOpcode,
        WrongTransactionId,
        WrongMacAddress,
        WrongRelayAddress,
        MissingMessageType,
        WrongMessageType,
        NegativeAcknowledgement,
    }

    #[test]
    fn validates_response_against_pending_transaction() {
        let transaction_id = 42;
        let mac_address = MacAddress::new([2, 0, 0, 0, 0, 1]);
        let advertise_address = Ipv4Addr::LOCALHOST;
        let server_address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 6767);

        check_cases(
            [
                Case {
                    scenario: "matching response",
                    input: ResponseVariation::Valid,
                    expect: Yields(()),
                },
                Case {
                    scenario: "DHCP pod source behind a Kubernetes Service",
                    input: ResponseVariation::DifferentSourceIp,
                    expect: Yields(()),
                },
                Case {
                    scenario: "different source port",
                    input: ResponseVariation::DifferentSourcePort,
                    expect: Yields(()),
                },
                Case {
                    scenario: "missing server identifier",
                    input: ResponseVariation::MissingServerIdentifier,
                    expect: Fails,
                },
                Case {
                    scenario: "server identifier differs from request destination",
                    input: ResponseVariation::DifferentServerIdentifier,
                    expect: Yields(()),
                },
                Case {
                    scenario: "wrong opcode",
                    input: ResponseVariation::WrongOpcode,
                    expect: Fails,
                },
                Case {
                    scenario: "wrong transaction ID",
                    input: ResponseVariation::WrongTransactionId,
                    expect: Fails,
                },
                Case {
                    scenario: "wrong MAC address",
                    input: ResponseVariation::WrongMacAddress,
                    expect: Fails,
                },
                Case {
                    scenario: "wrong relay address",
                    input: ResponseVariation::WrongRelayAddress,
                    expect: Fails,
                },
                Case {
                    scenario: "missing message type",
                    input: ResponseVariation::MissingMessageType,
                    expect: Fails,
                },
                Case {
                    scenario: "wrong message type",
                    input: ResponseVariation::WrongMessageType,
                    expect: Fails,
                },
                Case {
                    scenario: "negative acknowledgement",
                    input: ResponseVariation::NegativeAcknowledgement,
                    expect: Fails,
                },
            ],
            |variation| {
                let mut response = Message::default();
                response
                    .set_opcode(Opcode::BootReply)
                    .set_xid(transaction_id)
                    .set_chaddr(&mac_address.bytes())
                    .set_giaddr(advertise_address);
                response
                    .opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Offer));
                response
                    .opts_mut()
                    .insert(DhcpOption::ServerIdentifier(*server_address.ip()));
                let mut source = SocketAddr::V4(server_address);

                match variation {
                    ResponseVariation::Valid => {}
                    ResponseVariation::DifferentSourceIp => {
                        source = "127.0.0.2:6767".parse().unwrap();
                    }
                    ResponseVariation::DifferentSourcePort => {
                        source = "127.0.0.1:6768".parse().unwrap();
                    }
                    ResponseVariation::MissingServerIdentifier => {
                        response.opts_mut().remove(OptionCode::ServerIdentifier);
                    }
                    ResponseVariation::DifferentServerIdentifier => {
                        response
                            .opts_mut()
                            .insert(DhcpOption::ServerIdentifier(Ipv4Addr::new(127, 0, 0, 2)));
                    }
                    ResponseVariation::WrongOpcode => {
                        response.set_opcode(Opcode::BootRequest);
                    }
                    ResponseVariation::WrongTransactionId => {
                        response.set_xid(transaction_id + 1);
                    }
                    ResponseVariation::WrongMacAddress => {
                        response.set_chaddr(&[2, 0, 0, 0, 0, 2]);
                    }
                    ResponseVariation::WrongRelayAddress => {
                        response.set_giaddr(Ipv4Addr::new(127, 0, 0, 2));
                    }
                    ResponseVariation::MissingMessageType => {
                        response.opts_mut().remove(OptionCode::MessageType);
                    }
                    ResponseVariation::WrongMessageType => {
                        response
                            .opts_mut()
                            .insert(DhcpOption::MessageType(MessageType::Ack));
                    }
                    ResponseVariation::NegativeAcknowledgement => {
                        response
                            .opts_mut()
                            .insert(DhcpOption::MessageType(MessageType::Nak));
                    }
                }

                validate_response(
                    &response,
                    transaction_id,
                    mac_address,
                    advertise_address,
                    MessageType::Offer,
                    source,
                )
                .map_err(drop)
            },
        );
    }

    #[tokio::test]
    async fn cancelling_udp_exchange_removes_pending_response() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_address = match server.local_addr().unwrap() {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => unreachable!(),
        };
        let (client, service) = UdpDhcpClient::start(
            server_address,
            "127.0.0.1:0".parse().unwrap(),
            Ipv4Addr::LOCALHOST,
        )
        .await
        .unwrap();
        let xid = 42;
        let mac_address = MacAddress::new([2, 0, 0, 0, 0, 1]);
        let request = client.request_message(
            xid,
            mac_address,
            Ipv4Addr::new(172, 21, 0, 1),
            None,
            MessageType::Discover,
        );
        let exchange_client = client.clone();
        let exchange = tokio::spawn(async move {
            exchange_client
                .exchange(request, mac_address, MessageType::Offer)
                .await
        });

        let mut buffer = [0; DHCP_PACKET_BUFFER_SIZE];
        server.recv_from(&mut buffer).await.unwrap();
        assert!(client.pending.lock().unwrap().contains_key(&xid));

        exchange.abort();
        assert!(exchange.await.unwrap_err().is_cancelled());
        assert!(client.pending.lock().unwrap().is_empty());
        service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn udp_relay_completes_concurrent_dhcp_exchanges() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_address = match server.local_addr().unwrap() {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => unreachable!(),
        };
        let (client, service) = UdpDhcpClient::start(
            server_address,
            "127.0.0.1:0".parse().unwrap(),
            Ipv4Addr::LOCALHOST,
        )
        .await
        .unwrap();
        let client_address = client.socket.local_addr().unwrap();
        let server_task =
            tokio::spawn(run_fake_server(server, client_address, 2, ASSIGNED_ADDRESS));

        let request = |last_mac_byte| DhcpRequestInfo {
            mac_address: MacAddress::new([2, 0, 0, 0, 0, last_mac_byte]),
            relay_address: Ipv4Addr::new(172, 21, 0, 1),
            vendor_class: Some("NVIDIA/BF/BMC"),
        };
        let (first, second) =
            tokio::join!(client.request_ip(request(1)), client.request_ip(request(2)));

        for response in [first.unwrap(), second.unwrap()] {
            assert_eq!(response.ip_address, ASSIGNED_ADDRESS);
        }
        server_task.await.unwrap();
        service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn udp_relay_rejects_ack_for_a_different_address() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_address = match server.local_addr().unwrap() {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => unreachable!(),
        };
        let (client, service) = UdpDhcpClient::start(
            server_address,
            "127.0.0.1:0".parse().unwrap(),
            Ipv4Addr::LOCALHOST,
        )
        .await
        .unwrap();
        let client_address = client.socket.local_addr().unwrap();
        let server_task = tokio::spawn(run_fake_server(
            server,
            client_address,
            1,
            Ipv4Addr::new(172, 21, 0, 43),
        ));

        let result = client
            .request_ip(DhcpRequestInfo {
                mac_address: MacAddress::new([2, 0, 0, 0, 0, 1]),
                relay_address: Ipv4Addr::new(172, 21, 0, 1),
                vendor_class: Some("NVIDIA/BF/BMC"),
            })
            .await;

        assert!(matches!(result, Err(DhcpRelayError::InvalidDhcpPacket(_))));
        server_task.await.unwrap();
        service.shutdown().await.unwrap();
    }

    async fn run_fake_server(
        server: UdpSocket,
        client_address: SocketAddr,
        exchange_count: usize,
        ack_address: Ipv4Addr,
    ) {
        let mut buffer = [0; DHCP_PACKET_BUFFER_SIZE];
        for _ in 0..exchange_count * 2 {
            let (length, _) = server.recv_from(&mut buffer).await.unwrap();
            let request = Message::decode(&mut Decoder::new(&buffer[..length])).unwrap();
            assert_eq!(request.giaddr(), Ipv4Addr::LOCALHOST);
            let relay_information = match request
                .opts()
                .get(OptionCode::RelayAgentInformation)
                .unwrap()
            {
                DhcpOption::RelayAgentInformation(information) => information,
                option => panic!("unexpected relay option: {option:?}"),
            };
            assert_eq!(
                relay_information.get(RelayCode::LinkSelection),
                Some(&RelayInfo::LinkSelection(Ipv4Addr::new(172, 21, 0, 1)))
            );

            let request_type = request.opts().msg_type().unwrap();
            let response_type = match request_type {
                MessageType::Discover => MessageType::Offer,
                MessageType::Request => {
                    assert_eq!(
                        request.opts().get(OptionCode::RequestedIpAddress),
                        Some(&DhcpOption::RequestedIpAddress(ASSIGNED_ADDRESS))
                    );
                    MessageType::Ack
                }
                message_type => panic!("unexpected DHCP request: {message_type:?}"),
            };
            if request_type == MessageType::Request {
                let delayed_offer = fake_response(&request, MessageType::Offer, ASSIGNED_ADDRESS);
                let mut encoded = Vec::new();
                delayed_offer
                    .encode(&mut Encoder::new(&mut encoded))
                    .unwrap();
                server.send_to(&encoded, client_address).await.unwrap();
            }
            let assigned_address = if response_type == MessageType::Ack {
                ack_address
            } else {
                ASSIGNED_ADDRESS
            };
            let response = fake_response(&request, response_type, assigned_address);
            let mut encoded = Vec::new();
            response.encode(&mut Encoder::new(&mut encoded)).unwrap();
            server.send_to(&encoded, client_address).await.unwrap();
        }
    }

    fn fake_response(
        request: &Message,
        message_type: MessageType,
        assigned_address: Ipv4Addr,
    ) -> Message {
        let mut response = Message::default();
        response
            .set_opcode(Opcode::BootReply)
            .set_htype(HType::Eth)
            .set_xid(request.xid())
            .set_chaddr(request.chaddr())
            .set_giaddr(request.giaddr())
            .set_yiaddr(assigned_address);
        response
            .opts_mut()
            .insert(DhcpOption::MessageType(message_type));
        response
            .opts_mut()
            .insert(DhcpOption::ServerIdentifier(SERVER_IDENTIFIER));
        response
    }
}
