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
use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

use dhcp::mock_api_server;
use dhcproto::{Decodable, Decoder, v4};

#[allow(dead_code)]
mod common;

use common::{DHCPFactory, Kea};

const READ_TIMEOUT: Duration = Duration::from_secs(2);

fn send_and_recv(socket: &UdpSocket, message: v4::Message) -> Result<v4::Message, eyre::Report> {
    socket.send(&DHCPFactory::encode(message)?)?;
    let mut buffer = [0u8; 1500];
    let length = socket.recv(&mut buffer)?;
    v4::Message::decode(&mut Decoder::new(&buffer[..length]))
        .map_err(|error| eyre::eyre!("failed to decode DHCP response: {error}"))
}

fn server_identifier(message: &v4::Message) -> Ipv4Addr {
    match message.opts().get(v4::OptionCode::ServerIdentifier) {
        Some(v4::DhcpOption::ServerIdentifier(address)) => *address,
        other => panic!("DHCPOFFER did not include a server identifier: {other:?}"),
    }
}

#[test]
fn missing_optional_vendor_class_is_not_logged_as_error() -> Result<(), eyre::Report> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let api_server = runtime.block_on(mock_api_server::MockAPIServer::start());
    let (mut kea, socket) = Kea::start(api_server.local_http_addr(), None)?;
    socket.set_read_timeout(Some(READ_TIMEOUT))?;

    let mut request = DHCPFactory::discover(1);
    request.opts_mut().remove(v4::OptionCode::ClassIdentifier);

    let response = send_and_recv(&socket, request)?;
    assert_eq!(response.opts().msg_type(), Some(v4::MessageType::Offer));
    kea.stop_process();
    assert!(
        !kea.has_log("Missing option [60] in packet"),
        "optional vendor-class absence must not be logged as an error"
    );

    Ok(())
}

#[test]
fn renewal_without_client_architecture_is_not_logged_as_error() -> Result<(), eyre::Report> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let api_server = runtime.block_on(mock_api_server::MockAPIServer::start());
    let (mut kea, socket) = Kea::start(api_server.local_http_addr(), None)?;
    socket.set_read_timeout(Some(READ_TIMEOUT))?;

    let idx = 2;
    let offer = send_and_recv(&socket, DHCPFactory::discover(idx))?;
    assert_eq!(offer.opts().msg_type(), Some(v4::MessageType::Offer));
    let lease_address = offer.yiaddr();

    let mut request = DHCPFactory::base_relayed_message(idx, v4::MessageType::Request);
    request
        .opts_mut()
        .insert(v4::DhcpOption::RequestedIpAddress(lease_address));
    request
        .opts_mut()
        .insert(v4::DhcpOption::ServerIdentifier(server_identifier(&offer)));
    let response = send_and_recv(&socket, request)?;
    assert_eq!(response.opts().msg_type(), Some(v4::MessageType::Ack));

    let mut renewal = DHCPFactory::base_relayed_message(idx, v4::MessageType::Request);
    renewal.set_ciaddr(lease_address);
    assert!(
        renewal
            .opts_mut()
            .remove(v4::OptionCode::ClientSystemArchitecture)
            .is_some(),
        "test fixture must include client-system-architecture before removal"
    );
    let response = send_and_recv(&socket, renewal)?;
    assert_eq!(response.opts().msg_type(), Some(v4::MessageType::Ack));

    kea.stop_process();
    assert!(
        !kea.has_log("Missing option [93] in packet"),
        "client-system-architecture absence during renewal must not be logged as an error"
    );

    Ok(())
}

fn assert_pxe_service_reply(message: &v4::Message, url: Option<&str>, boot_url: &str) {
    let mut vendor = vec![6, 4, 0, 0, 0, 8];
    if let Some(url) = url {
        vendor.extend_from_slice(&[72, u8::try_from(url.len()).unwrap()]);
        vendor.extend_from_slice(url.as_bytes());
    }
    assert_eq!(
        message.opts().get(v4::OptionCode::VendorExtensions),
        Some(&v4::DhcpOption::VendorExtensions(vendor)),
    );
    assert_eq!(
        message.opts().get(v4::OptionCode::BootfileName),
        Some(&v4::DhcpOption::BootfileName(boot_url.as_bytes().to_vec())),
    );
}

/// Replacing or removing the URL after restart preserves unrelated DHCP data.
#[test]
fn nico_pxe_service_url_preserves_boot_options_across_restarts() -> Result<(), eyre::Report> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let api_server = runtime.block_on(mock_api_server::MockAPIServer::start());
    let boot_url = "http://images.example/ipxe.efi";
    api_server.set_booturl_override("02:00:00:00:00:03", boot_url);
    let (mut kea, socket) = Kea::start(api_server.local_http_addr(), None)?;
    socket.set_read_timeout(Some(READ_TIMEOUT))?;
    let request = DHCPFactory::discover(3);
    assert!(request.opts().get(v4::OptionCode::UserClass).is_none());
    let baseline = send_and_recv(&socket, request.clone())?;
    assert_pxe_service_reply(&baseline, None, boot_url);

    // Kea's 255-byte limit includes the outer header, suboption 6 and suboption 72's header.
    let url = format!("http://{}/", "a".repeat(237));
    let cases = [
        ("preserve configured bytes", Some(url.as_str())),
        (
            "replace configured URL",
            Some("http://replacement.example:8080/"),
        ),
        ("remove configured URL", None),
        ("configure after removal", Some("http://192.0.2.10:8080")),
        ("empty clears configured URL", Some("")),
    ];
    for (scenario, configured) in cases {
        kea.restart_with_nico_pxe_service_url(&socket, configured)
            .map_err(|error| eyre::eyre!("{scenario}: {error}"))?;
        let mut response = send_and_recv(&socket, request.clone())?;
        assert_eq!(
            response.opts().msg_type(),
            Some(v4::MessageType::Offer),
            "{scenario}"
        );
        assert_pxe_service_reply(
            &response,
            configured.filter(|url| !url.is_empty()),
            boot_url,
        );
        assert_eq!(response.yiaddr(), baseline.yiaddr(), "{scenario}");
        assert_eq!(response.siaddr(), baseline.siaddr(), "{scenario}");
        assert_eq!(response.fname(), baseline.fname(), "{scenario}");
        response.opts_mut().insert(
            baseline
                .opts()
                .get(v4::OptionCode::VendorExtensions)
                .unwrap()
                .clone(),
        );
        assert_eq!(
            response.opts(),
            baseline.opts(),
            "{scenario}: unrelated DHCP options changed"
        );
    }

    let oversized_url = format!("{url}a");
    assert!(
        kea.restart_with_nico_pxe_service_url(&socket, Some(&oversized_url))
            .is_err(),
        "Kea accepted a URL that requires splitting option 43"
    );
    kea.stop_process();
    assert!(kea.has_log("nico-pxe-service-url exceeds DHCPv4 limit of 245 bytes"));
    Ok(())
}
