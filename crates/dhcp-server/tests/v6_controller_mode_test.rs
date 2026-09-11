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

use std::convert::Infallible;
use std::net::Ipv6Addr;
use std::sync::{Arc, Mutex};

use carbide_dhcp_server::errors::DhcpError;
use carbide_dhcp_server::modes::controller::Controller;
use carbide_dhcp_server::packet_handler_v6::process_packet;
use carbide_dhcpv6::RELAY_REPLY;
use dhcproto::v6::{DhcpOption, Message, MessageType, OptionCode};
use dhcproto::{Decodable, Decoder};
use futures::stream;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rpc::forge::{AddressFamily, BuildInfo, DhcpDiscovery, DhcpRecord};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

mod common;

use common::{
    DUID_UUID, client_message, controller_config, controller_config_with_lifetimes, encode,
    machine_cache, relay_forward, relay_option, response_ia_na,
};

const OPTION79: &[u8] = &[0, 1, 2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

/// Minimal Forge mock supporting only client initialization and DiscoverDhcp.
struct MockDiscoverDhcpApi {
    url: String,
    discoveries: Arc<Mutex<Vec<DhcpDiscovery>>>,
    shutdown: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<()>>,
}

impl MockDiscoverDhcpApi {
    /// Start an isolated HTTP/2 gRPC endpoint without linking the Kea hook crate.
    async fn start() -> Self {
        // Bind before spawning so the returned URL is immediately usable.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock Forge listener binds");
        let address = listener.local_addr().expect("mock listener has an address");
        let discoveries = Arc::new(Mutex::new(Vec::new()));
        let server_discoveries = discoveries.clone();
        let (shutdown, shutdown_rx) = oneshot::channel();

        // One connection serves the Version probe and test DiscoverDhcp call.
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("mock accepts a client");
            let connection = http2::Builder::new(TokioExecutor::new()).serve_connection(
                TokioIo::new(stream),
                service_fn(move |request| mock_forge_request(request, server_discoveries.clone())),
            );
            tokio::pin!(connection);
            tokio::select! {
                result = connection.as_mut() => {
                    result.expect("mock serves the gRPC connection");
                }
                _ = shutdown_rx => {
                    connection.as_mut().graceful_shutdown();
                    connection
                        .await
                        .expect("mock gracefully closes the gRPC connection");
                }
            }
        });

        Self {
            url: format!("http://{address}"),
            discoveries,
            shutdown: Some(shutdown),
            server: Some(server),
        }
    }

    /// Return the loopback URL consumed by the production Forge client.
    fn url(&self) -> &str {
        &self.url
    }

    /// Stop the mock, surface task failures, and return its observed requests.
    async fn shutdown(mut self) -> Vec<DhcpDiscovery> {
        let shutdown_sent = match self.shutdown.take() {
            Some(shutdown) => shutdown.send(()).is_ok(),
            None => true,
        };
        self.server
            .take()
            .expect("mock server task is present")
            .await
            .expect("mock server task did not panic");
        assert!(
            shutdown_sent,
            "mock shutdown receiver closed before the explicit signal"
        );
        self.discoveries
            .lock()
            .expect("mock discoveries lock is available")
            .clone()
    }
}

impl Drop for MockDiscoverDhcpApi {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

/// Handle the two Forge methods used by controller-mode discovery.
async fn mock_forge_request(
    request: Request<Incoming>,
    discoveries: Arc<Mutex<Vec<DhcpDiscovery>>>,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
    let response = match request.uri().path() {
        "/forge.Forge/Version" => grpc_response(BuildInfo::default()),
        "/forge.Forge/DiscoverDhcp" => {
            // Strip the gRPC frame and retain the request for contract assertions.
            let body = request
                .into_body()
                .collect()
                .await
                .expect("DiscoverDhcp request body is readable")
                .to_bytes();
            let payload = body.get(5..).expect("DiscoverDhcp has a gRPC frame");
            let discovery = DhcpDiscovery::decode(payload).expect("DiscoverDhcp request decodes");
            discoveries
                .lock()
                .expect("mock discoveries lock is available")
                .push(discovery.clone());

            // Return only the fields consumed by the DHCPv6 response encoder.
            grpc_response(DhcpRecord {
                fqdn: "host.example.com".to_string(),
                mac_address: discovery.mac_address,
                address: "2001:db8::ee".to_string(),
                prefix: "2001:db8::/64".to_string(),
                ..Default::default()
            })
        }
        path => panic!("unexpected mock Forge method: {path}"),
    };
    Ok(response)
}

/// Encode one protobuf response with the gRPC data and status frames tonic expects.
fn grpc_response(message: impl prost::Message) -> Response<UnsyncBoxBody<Bytes, Infallible>> {
    // Prefix the protobuf with the standard uncompressed gRPC frame header.
    let mut data = Vec::with_capacity(5 + message.encoded_len());
    data.push(0);
    data.extend_from_slice(
        &u32::try_from(message.encoded_len())
            .expect("test response fits in a gRPC frame")
            .to_be_bytes(),
    );
    message
        .encode(&mut data)
        .expect("mock gRPC response encodes");

    // Complete the response with an OK status trailer.
    let mut trailers = hyper::HeaderMap::new();
    trailers.insert(
        header::HeaderName::from_static("grpc-status"),
        header::HeaderValue::from_static("0"),
    );
    let body = StreamBody::new(stream::iter([
        Ok::<_, Infallible>(Frame::data(Bytes::from(data))),
        Ok(Frame::trailers(trailers)),
    ]))
    .boxed_unsync();

    Response::builder()
        .header(header::CONTENT_TYPE, "application/grpc+tonic")
        .body(body)
        .expect("mock gRPC response is valid")
}

/// Verifies controller mode forwards selected v6 identity and restores the relay envelope.
#[tokio::test]
async fn relayed_solicit_round_trips_through_controller_api() {
    let api = MockDiscoverDhcpApi::start().await;
    let config = controller_config(api.url());
    let inner = encode(&client_message(MessageType::Solicit, DUID_UUID, None, None));
    let request = relay_forward(&inner, b"swp1", OPTION79);
    let mut cache = machine_cache();

    // A non-MAC DUID is valid here because the trusted relay supplies option 79.
    let packet = process_packet(
        &request,
        "fe80::100".parse().unwrap(),
        &config,
        "eth0",
        &Controller {},
        &mut cache,
    )
    .await;
    let discoveries = api.shutdown().await;
    let packet = packet
        .expect("relayed controller SOLICIT is valid")
        .expect("relayed controller SOLICIT is served");

    // The response preserves relay routing metadata and wraps an ADVERTISE.
    assert_eq!(packet.encoded_packet()[0], RELAY_REPLY);
    assert_eq!(
        relay_option(packet.encoded_packet(), OptionCode::InterfaceId),
        b"swp1"
    );
    let response = Message::decode(&mut Decoder::new(relay_option(
        packet.encoded_packet(),
        OptionCode::RelayMsg,
    )))
    .expect("inner ADVERTISE decodes");
    assert_eq!(response.msg_type(), MessageType::Advertise);
    match response_ia_na(&response).opts.get(OptionCode::IAAddr) {
        Some(DhcpOption::IAAddr(address)) => {
            assert_eq!(address.addr, "2001:db8::ee".parse::<Ipv6Addr>().unwrap());
        }
        other => panic!("expected controller IAADDR, got {other:?}"),
    }

    // The API observes the transport-selected MAC and complete family-aware contract.
    assert_eq!(discoveries.len(), 1);
    let discovery = &discoveries[0];
    assert_eq!(discovery.address_family, Some(AddressFamily::V6 as i32));
    assert_eq!(discovery.duid.as_deref(), Some(DUID_UUID));
    assert_eq!(discovery.mac_address, "02:aa:bb:cc:dd:ee");
    assert_eq!(discovery.circuit_id.as_deref(), Some("73777031"));
    assert_eq!(discovery.link_address.as_deref(), Some("2001:db8::1"));
}

/// Verifies controller CONFIRM without optional Interface-ID remains silent.
#[tokio::test]
async fn relayed_confirm_without_link_knowledge_is_ignored() {
    let config = controller_config("http://[::1]:1");
    let inner = encode(&client_message(
        MessageType::Confirm,
        DUID_UUID,
        Some("2001:db8::20".parse().unwrap()),
        None,
    ));
    let request = relay_forward(&inner, &[], OPTION79);
    let mut cache = machine_cache();

    // CONFIRM bypasses the API, so omitting relay Interface-ID must preserve silence.
    let response = process_packet(
        &request,
        "fe80::100".parse().unwrap(),
        &config,
        "eth0",
        &Controller {},
        &mut cache,
    )
    .await
    .expect("relayed CONFIRM is valid");
    assert!(response.is_none());
}

/// Invalid stateful lifetimes fail locally before controller discovery can allocate.
#[tokio::test]
async fn invalid_stateful_lifetimes_fail_before_controller_discovery() {
    // An unreachable API makes any accidental discovery call observable as a transport error.
    let config = controller_config_with_lifetimes("http://[::1]:1", 0, 7200);
    let inner = encode(&client_message(MessageType::Solicit, DUID_UUID, None, None));
    let request = relay_forward(&inner, b"swp1", OPTION79);
    let mut cache = machine_cache();

    let result = process_packet(
        &request,
        "fe80::100".parse().unwrap(),
        &config,
        "eth0",
        &Controller {},
        &mut cache,
    )
    .await;

    assert!(matches!(
        result,
        Err(DhcpError::InvalidDhcpV6Lifetimes {
            preferred_lifetime_secs: 0,
            valid_lifetime_secs: 7200,
        })
    ));
}
