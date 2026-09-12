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
use std::net::{Ipv6Addr, SocketAddr};
use std::time::Duration;

use carbide_instrument::testing::MetricsCapture;
use futures::stream;
use hickory_resolver::proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_resolver::proto::rr::rdata::PTR;
use hickory_resolver::proto::rr::{Name, RData, Record, RecordType};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rpc::forge_tls_client::{ForgeClientConfig, ForgeTlsClient};
use rpc::protos::dns::{
    DnsResourceRecord, DnsResourceRecordLookupRequest, DnsResourceRecordLookupResponse,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::{Config, DnsServer};

// Each test gets ten seconds for setup, one packet exchange, and teardown.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn aaaa_query_over_ipv6_udp_returns_only_ipv6_answers() {
    // The real handler emits metrics, so share the exact-counter tests' lock.
    let _metrics_window = MetricsCapture::start();
    timeout(TEST_TIMEOUT, async {
        let name: Name = "host.example.com.".parse().unwrap();
        let records = [
            ("A", "192.0.2.42", 60),
            ("AAAA", "2001:db8::42", 120),
            ("AAAA", "2001:db8::43", 180),
        ]
        .into_iter()
        .map(|(qtype, content, ttl)| DnsResourceRecord {
            qname: name.to_string(),
            qtype: qtype.to_string(),
            content: content.to_string(),
            ttl,
            ..Default::default()
        })
        .collect();
        let server = TestDnsServer::start(records).await;
        let request = query(name.clone(), RecordType::AAAA);
        let packet = request.to_vec().expect("AAAA query encodes");
        let client = UdpSocket::bind("[::1]:0").await.unwrap();
        client.connect(server.udp_address).await.unwrap();
        assert_eq!(client.send(&packet).await.unwrap(), packet.len());

        let mut response = [0; 512];
        let length = client
            .recv(&mut response)
            .await
            .expect("UDP response arrives");
        let response = Message::from_vec(&response[..length]).expect("UDP DNS response decodes");
        let lookup = server.shutdown().await;

        let expected_answers = [
            Record::from_rdata(
                name.clone(),
                120,
                RData::AAAA("2001:db8::42".parse::<Ipv6Addr>().unwrap().into()),
            ),
            Record::from_rdata(
                name.clone(),
                180,
                RData::AAAA("2001:db8::43".parse::<Ipv6Addr>().unwrap().into()),
            ),
        ];
        assert_response(&response, &request, &expected_answers);
        assert_eq!(lookup.qname, name.to_string());
        assert_eq!(lookup.qtype, "AAAA");
    })
    .await
    .expect("IPv6 UDP query and teardown complete before the test deadline");
}

#[tokio::test]
async fn ipv6_ptr_query_over_tcp_returns_the_target_name() {
    let _metrics_window = MetricsCapture::start();
    timeout(TEST_TIMEOUT, async {
        let name: Name =
            "2.4.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.8.b.d.0.1.0.0.2.ip6.arpa."
                .parse()
                .unwrap();
        let target: Name = "host.example.com.".parse().unwrap();
        let server = TestDnsServer::start(vec![DnsResourceRecord {
            qname: name.to_string(),
            qtype: "PTR".to_string(),
            content: target.to_string(),
            ttl: 300,
            ..Default::default()
        }])
        .await;
        let request = query(name.clone(), RecordType::PTR);
        let packet = request.to_vec().expect("PTR query encodes");
        let mut client = TcpStream::connect(server.tcp_address).await.unwrap();

        // TCP DNS messages have a two-byte length prefix, unlike UDP packets.
        client
            .write_u16(u16::try_from(packet.len()).unwrap())
            .await
            .unwrap();
        client.write_all(&packet).await.unwrap();
        let length = client
            .read_u16()
            .await
            .expect("TCP response length arrives");
        let mut response = vec![0; usize::from(length)];
        client
            .read_exact(&mut response)
            .await
            .expect("complete TCP response arrives");
        let response = Message::from_vec(&response).expect("TCP DNS response decodes");
        drop(client);
        let lookup = server.shutdown().await;

        assert_response(
            &response,
            &request,
            &[Record::from_rdata(
                name.clone(),
                300,
                RData::PTR(PTR(target)),
            )],
        );
        assert_eq!(lookup.qname, name.to_string());
        assert_eq!(lookup.qtype, "PTR");
    })
    .await
    .expect("IPv6 TCP query and teardown complete before the test deadline");
}

fn query(name: Name, record_type: RecordType) -> Message {
    let mut request = Message::new(0x5405, MessageType::Query, OpCode::Query);
    request.queries.push(Query::query(name, record_type));
    request
}

fn assert_response(response: &Message, request: &Message, answers: &[Record]) {
    assert_eq!(response.metadata.id, request.metadata.id);
    assert_eq!(response.metadata.message_type, MessageType::Response);
    assert_eq!(response.metadata.op_code, OpCode::Query);
    assert_eq!(response.metadata.response_code, ResponseCode::NoError);
    assert_eq!(response.queries, request.queries);
    assert_eq!(response.answers, answers);
    // Hickory's record equality deliberately excludes TTL.
    for (actual, expected) in response.answers.iter().zip(answers) {
        assert_eq!(actual.ttl, expected.ttl);
    }
}

struct TestDnsServer {
    server: hickory_server::Server<DnsServer>,
    api: MockLookupApi,
    udp_address: SocketAddr,
    tcp_address: SocketAddr,
}

impl TestDnsServer {
    async fn start(records: Vec<DnsResourceRecord>) -> Self {
        let api = MockLookupApi::start(records).await;
        let client = ForgeTlsClient::new(&ForgeClientConfig::default())
            .build(format!("http://{}", api.address))
            .await
            .expect("production Forge client builds for the mock API");
        let meter = opentelemetry::global::meter("dns-socket-tests");
        let server = DnsServer::new(Mutex::new(client), &meter, &Config::default());

        // Retain both sockets until registration; releasing a reserved port
        // would let another concurrent test bind it first.
        let udp_socket = UdpSocket::bind("[::1]:0").await.unwrap();
        let tcp_socket = TcpListener::bind("[::1]:0").await.unwrap();
        Self {
            udp_address: udp_socket.local_addr().unwrap(),
            tcp_address: tcp_socket.local_addr().unwrap(),
            server: server.register_sockets(udp_socket, tcp_socket),
            api,
        }
    }

    async fn shutdown(mut self) -> DnsResourceRecordLookupRequest {
        self.server
            .shutdown_gracefully()
            .await
            .expect("DNS server shuts down");
        self.api.shutdown().await
    }
}

struct MockLookupApi {
    address: SocketAddr,
    requests: mpsc::Receiver<DnsResourceRecordLookupRequest>,
    shutdown: CancellationToken,
    server: JoinHandle<()>,
}

impl MockLookupApi {
    async fn start(records: Vec<DnsResourceRecord>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        // Each packet test makes exactly one upstream lookup.
        let (requests_tx, requests) = mpsc::channel(1);
        let shutdown = CancellationToken::new();
        let cancelled = shutdown.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = tokio::select! {
                result = listener.accept() => result.expect("mock accepts the Forge client"),
                _ = cancelled.cancelled() => return,
            };
            let connection = http2::Builder::new(TokioExecutor::new()).serve_connection(
                TokioIo::new(stream),
                service_fn(move |request| {
                    mock_lookup(request, requests_tx.clone(), records.clone())
                }),
            );
            tokio::pin!(connection);
            tokio::select! {
                result = connection.as_mut() => result.expect("mock serves the gRPC connection"),
                _ = cancelled.cancelled() => {
                    connection.as_mut().graceful_shutdown();
                    connection.await.expect("mock gracefully closes the gRPC connection");
                }
            }
        });
        Self {
            address,
            requests,
            shutdown,
            server,
        }
    }

    async fn shutdown(mut self) -> DnsResourceRecordLookupRequest {
        self.shutdown.cancel();
        (&mut self.server)
            .await
            .expect("mock API task did not panic");
        self.requests
            .try_recv()
            .expect("mock received the upstream lookup")
    }
}

impl Drop for MockLookupApi {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn mock_lookup(
    request: Request<Incoming>,
    requests: mpsc::Sender<DnsResourceRecordLookupRequest>,
    records: Vec<DnsResourceRecord>,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
    assert_eq!(request.uri().path(), rpc::service_path!("LookupRecord"));
    let body = request.into_body().collect().await.unwrap().to_bytes();
    // The unary client sends one uncompressed message after the five-byte gRPC header.
    let payload = body.get(5..).expect("LookupRecord has a gRPC frame");
    requests
        .try_send(DnsResourceRecordLookupRequest::decode(payload).expect("LookupRecord decodes"))
        .expect("each test sends exactly one lookup");

    let response = DnsResourceRecordLookupResponse { records };
    let mut data = vec![0];
    data.extend_from_slice(&u32::try_from(response.encoded_len()).unwrap().to_be_bytes());
    response.encode(&mut data).unwrap();
    let mut trailers = hyper::HeaderMap::new();
    trailers.insert("grpc-status", header::HeaderValue::from_static("0"));
    let body = StreamBody::new(stream::iter([
        Ok::<_, Infallible>(Frame::data(Bytes::from(data))),
        Ok(Frame::trailers(trailers)),
    ]))
    .boxed_unsync();
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/grpc")
        .body(body)
        .unwrap())
}
