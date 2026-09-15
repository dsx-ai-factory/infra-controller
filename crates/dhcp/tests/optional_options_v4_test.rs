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
use std::net::UdpSocket;
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
