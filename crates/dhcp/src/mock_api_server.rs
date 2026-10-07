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

/// A hyper / TCP server that pretends to be carbide-api, for unit testing.
/// It responds to DHCP_DISCOVERY messages with a DHCP_OFFER of 172.20.0.{x}/32, where x is the
/// last byte of the MAC address sent in the DISCOVERY packet.
///
/// Included for unit tests or when the `test-support` feature is enabled.
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use ::rpc::forge as rpc;
use http_body_util::BodyExt;
use hyper::body::{Body as HttpBody, Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response, body, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mac_address::MacAddress;
use prost::Message;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[cfg(test)]
use crate::machine::Machine;

pub const ENDPOINT_DISCOVER_DHCP: &str = ::rpc::service_path!("DiscoverDhcp");
pub const ENDPOINT_EXPIRE_DHCP_LEASE: &str = ::rpc::service_path!("ExpireDhcpLease");

// Contents of the response
pub const DHCP_RESPONSE_FQDN: &str = "december-nitrogen.forge.local";
const DHCP_RESPONSE_ADDR_PREFIX: &str = "172.20.0";
const DHCP_V6_RESPONSE_PREFIX: &str = "2001:db8::";
const DHCP_V6_RESPONSE_NTP_SERVERS: [&str; 2] = ["2001:db8::124", "2001:db8::125"];

fn base_dhcp_response_for_family(
    mac_address: MacAddress,
    address_family: rpc::AddressFamily,
) -> rpc::DhcpRecord {
    let (address, prefix, gateway, ntp_servers) = match address_family {
        rpc::AddressFamily::V6 => (
            address_to_offer_v6(mac_address),
            "2001:db8::/64".to_string(),
            None,
            DHCP_V6_RESPONSE_NTP_SERVERS
                .iter()
                .map(ToString::to_string)
                .collect(),
        ),
        _ => (
            address_to_offer(mac_address),
            "172.20.0.0/24".to_string(),
            Some("172.20.0.1".to_string()),
            vec!["198.51.100.10".to_string(), "198.51.100.11".to_string()],
        ),
    };

    rpc::DhcpRecord {
        machine_id: None,
        machine_interface_id: Some("88750d14-00fa-4d21-9fbc-d562046bc194".parse().unwrap()),
        segment_id: Some("267d40d1-75ba-4fee-bf76-a2ec2ce293fd".parse().unwrap()),
        subdomain_id: Some("023138e1-ebf1-4ef7-8a2c-bbce928a1601".parse().unwrap()),
        fqdn: DHCP_RESPONSE_FQDN.to_string(),
        mac_address: mac_address.to_string(),
        address,
        mtu: 1490,
        prefix,
        gateway,
        booturl: None,
        last_invalidation_time: None,
        ntp_servers,
    }
}

fn dhcp_response_with_override(
    mac_address_str: &str,
    address_override: Option<String>,
    fqdn_override: Option<String>,
    address_family: rpc::AddressFamily,
) -> Vec<u8> {
    let mac_address = mac_address_str.parse::<MacAddress>().unwrap();

    let mut r = base_dhcp_response_for_family(mac_address, address_family);

    if let Some(addr) = address_override {
        r.address = addr;
    }
    if let Some(fqdn) = fqdn_override {
        r.fqdn = fqdn;
    }

    // Specialization of response based on mac address
    // Meant to be extended, if let ()... isn't what we want here
    #[allow(clippy::single_match)]
    match mac_address.bytes() {
        [_, _, _, _, _, 0xaa] => {
            r.booturl =
                "https://api-specified-ipxe-url.forge/public/blobs/internal/x86_64/ipxe.efi"
                    .to_string()
                    .into();
        }
        _ => {}
    }

    let mut out = Vec::with_capacity(224);
    out.push(0); // Message is not compressed
    out.extend_from_slice(&(r.encoded_len() as u32).to_be_bytes());
    r.encode(&mut out).unwrap();
    out
}

fn validate_discovery_contract(
    discovery: &rpc::DhcpDiscovery,
) -> (rpc::AddressFamily, Option<rpc::MessageKind>) {
    let routing_address = discovery
        .link_address
        .as_deref()
        .unwrap_or(&discovery.relay_address);

    match (discovery.address_family, discovery.message_kind) {
        (None, None) => {
            assert!(
                routing_address.parse::<Ipv4Addr>().is_ok(),
                "legacy DiscoverDhcp calls must route with IPv4 relay/link address: {routing_address}"
            );
            assert!(
                discovery.duid.is_none(),
                "legacy IPv4 DiscoverDhcp calls must not include a DUID"
            );
            (rpc::AddressFamily::V4, None)
        }
        (Some(_), None) | (None, Some(_)) => {
            panic!("address_family and message_kind must be provided together")
        }
        (Some(address_family), Some(message_kind)) => {
            let address_family = rpc::AddressFamily::try_from(address_family)
                .expect("DiscoverDhcp address_family must be known");
            let message_kind = rpc::MessageKind::try_from(message_kind)
                .expect("DiscoverDhcp message_kind must be known");
            assert_ne!(
                address_family,
                rpc::AddressFamily::Unspecified,
                "DiscoverDhcp address_family must be specified"
            );
            assert_ne!(
                message_kind,
                rpc::MessageKind::Unspecified,
                "DiscoverDhcp message_kind must be specified"
            );

            match address_family {
                rpc::AddressFamily::V4 => {
                    assert_eq!(
                        message_kind,
                        rpc::MessageKind::V4Discover,
                        "ADDRESS_FAMILY_V4 requires MESSAGE_KIND_V4_DISCOVER"
                    );
                    assert!(
                        routing_address.parse::<Ipv4Addr>().is_ok(),
                        "ADDRESS_FAMILY_V4 requires an IPv4 relay/link address: {routing_address}"
                    );
                    assert!(
                        discovery.duid.is_none(),
                        "DHCPv4 DiscoverDhcp calls must not include a DUID"
                    );
                }
                rpc::AddressFamily::V6 => {
                    assert!(
                        matches!(
                            message_kind,
                            rpc::MessageKind::V6Solicit
                                | rpc::MessageKind::V6Request
                                | rpc::MessageKind::V6InfoRequest
                        ),
                        "ADDRESS_FAMILY_V6 requires a DHCPv6 message_kind"
                    );
                    assert!(
                        routing_address.parse::<Ipv6Addr>().is_ok(),
                        "ADDRESS_FAMILY_V6 requires an IPv6 relay/link address: {routing_address}"
                    );
                    assert!(
                        discovery
                            .duid
                            .as_deref()
                            .is_some_and(|duid| !duid.is_empty()),
                        "DHCPv6 DiscoverDhcp calls must include a non-empty DUID"
                    );
                }
                rpc::AddressFamily::Unspecified => unreachable!(),
            }

            (address_family, Some(message_kind))
        }
    }
}

// Given a MAC address, make the IP address we should offer it
fn address_to_offer(mac: MacAddress) -> String {
    format!("{}.{}", DHCP_RESPONSE_ADDR_PREFIX, mac.bytes()[5])
}

fn address_to_offer_v6(mac: MacAddress) -> String {
    format!("{}{:x}", DHCP_V6_RESPONSE_PREFIX, mac.bytes()[5])
}

// Does this Machine match the result we expected?
#[cfg(test)]
pub(super) fn matches_mock_response(machine: &Machine) -> bool {
    machine.inner.fqdn == DHCP_RESPONSE_FQDN
        && machine.inner.address == address_to_offer(machine.discovery_info.mac_address)
}

pub struct MockAPIServer {
    calls: Arc<Mutex<HashMap<String, usize>>>,
    handle: JoinHandle<Result<(), hyper::Error>>,
    tx: Option<tokio::sync::oneshot::Sender<()>>,
    local_addr: String,
    #[cfg(test)]
    inject_failure: Arc<Mutex<bool>>,
    discoveries: Arc<Mutex<Vec<rpc::DhcpDiscovery>>>,
    expired_leases: Arc<Mutex<Vec<rpc::ExpireDhcpLeaseRequest>>>,
    /// Per-MAC override for the `address` field of the DhcpRecord response.
    /// A value of `""` is meaningful: it simulates a Machine with no address
    /// binding, which allocation hooks should treat as "refuse to allocate".
    address_overrides: Arc<Mutex<HashMap<String, String>>>,
    /// Per-MAC override for the `fqdn` field of the DhcpRecord response.
    fqdn_overrides: Arc<Mutex<HashMap<String, String>>>,
}

#[derive(Debug)]
enum MockAPIServerError {
    MockAPIFetchMachineError,
}

impl std::error::Error for MockAPIServerError {}

impl std::fmt::Display for MockAPIServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "MockAPIServer injected test error")
    }
}

impl MockAPIServer {
    // Start a Hyper HTTP/2 server as a task on give runtime
    pub async fn start() -> MockAPIServer {
        // :0 asks the kernel to assign an unused port
        // Gitlab CI (or some part of our config of it) does not support IPv6
        let addr = SocketAddr::V4(SocketAddrV4::from_str("127.0.0.1:0").unwrap());

        #[cfg(test)]
        let inject_failure = Arc::new(Mutex::new(false));
        #[cfg(test)]
        let failure_injector = Some(inject_failure.clone());
        #[cfg(not(test))]
        let failure_injector: Option<Arc<Mutex<bool>>> = None;
        let i2 = failure_injector;
        let calls = Arc::new(Mutex::new(HashMap::new()));
        let c2 = calls.clone();
        let discoveries = Arc::new(Mutex::new(Vec::new()));
        let d2 = discoveries.clone();
        let expired_leases = Arc::new(Mutex::new(Vec::new()));
        let e2 = expired_leases.clone();
        let address_overrides = Arc::new(Mutex::new(HashMap::<String, String>::new()));
        let a2 = address_overrides.clone();
        let fqdn_overrides = Arc::new(Mutex::new(HashMap::<String, String>::new()));
        let f2 = fqdn_overrides.clone();
        let listener = TcpListener::bind(addr).await.unwrap();
        let local_addr = listener.local_addr().unwrap().to_string();
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            loop {
                let c3 = c2.clone();
                let i3 = i2.clone();
                let d3 = d2.clone();
                let e3 = e2.clone();
                let a3 = a2.clone();
                let f3 = f2.clone();
                tokio::select! {
                    result = listener.accept() => {
                        let (stream, _) = result.unwrap();
                        tokio::spawn(async move {
                            http2::Builder::new(TokioExecutor::new()).serve_connection(TokioIo::new(stream), service_fn(move |req: Request<body::Incoming>| {
                                let c3 = c3.clone();
                                let i3 = i3.clone();
                                let d3 = d3.clone();
                                let e3 = e3.clone();
                                let a3 = a3.clone();
                                let f3 = f3.clone();
                                async move {
                                    Ok::<Response<GrpcBody>, hyper::Error>(MockAPIServer::handler(req, c3.clone(), i3.clone(), d3.clone(), e3.clone(), a3.clone(), f3.clone()).await.unwrap())
                                }
                            })).await.inspect_err(|e| eprintln!("ERROR: {e:?}")).unwrap()
                        });
                    }
                    _ = &mut rx => {
                        break;
                    }
                }
            }
            Ok::<(), hyper::Error>(())
        });
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await; // let it start
        MockAPIServer {
            calls,
            handle,
            local_addr: format!("http://{local_addr}"),
            tx: Some(tx),
            #[cfg(test)]
            inject_failure,
            discoveries,
            expired_leases,
            address_overrides,
            fqdn_overrides,
        }
    }

    /// Override what address the mock returns for a specific MAC on subsequent
    /// `DiscoverDhcp` calls. Pass `""` to simulate "Machine has no address binding".
    pub fn set_address_override(&self, mac_address: &str, address: &str) {
        self.address_overrides
            .lock()
            .unwrap()
            .insert(normalized_mac_key(mac_address), address.to_string());
    }

    /// Override what FQDN the mock returns for a specific MAC on subsequent
    /// `DiscoverDhcp` calls. Pass `""` to simulate an options-only record with
    /// no API-owned hostname.
    pub fn set_fqdn_override(&self, mac_address: &str, fqdn: &str) {
        self.fqdn_overrides
            .lock()
            .unwrap()
            .insert(normalized_mac_key(mac_address), fqdn.to_string());
    }

    // The HTTP address of the server
    pub fn local_http_addr(&self) -> &str {
        &self.local_addr
    }

    #[cfg(test)]
    pub(super) fn set_inject_failure(&mut self, fail: bool) {
        *self.inject_failure.lock().unwrap() = fail;
    }

    // Number of times the given endpoint has been hit
    pub fn calls_for(&self, endpoint: &str) -> usize {
        let l = self.calls.lock().unwrap();
        if l.contains_key(endpoint) {
            *l.get(endpoint).unwrap()
        } else {
            0
        }
    }

    pub fn discoveries(&self) -> Vec<rpc::DhcpDiscovery> {
        self.discoveries.lock().unwrap().clone()
    }

    pub fn expired_leases(&self) -> Vec<rpc::ExpireDhcpLeaseRequest> {
        self.expired_leases.lock().unwrap().clone()
    }

    async fn handler(
        req: Request<Incoming>,
        calls: Arc<Mutex<HashMap<String, usize>>>,
        fail: Option<Arc<Mutex<bool>>>,
        discoveries: Arc<Mutex<Vec<rpc::DhcpDiscovery>>>,
        expired_leases: Arc<Mutex<Vec<rpc::ExpireDhcpLeaseRequest>>>,
        address_overrides: Arc<Mutex<HashMap<String, String>>>,
        fqdn_overrides: Arc<Mutex<HashMap<String, String>>>,
    ) -> Result<Response<GrpcBody>, MockAPIServerError> {
        let path = req.uri().path();
        calls
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .and_modify(|e| *e += 1)
            .or_insert(1);
        match path {
            // Add the endpoints you need here
            ENDPOINT_DISCOVER_DHCP => {
                let inject_failure = fail.as_ref().is_some_and(|fail| *fail.lock().unwrap());
                if inject_failure {
                    Err(MockAPIServerError::MockAPIFetchMachineError)
                } else {
                    Ok(grpc_response(
                        MockAPIServer::discover_dhcp(
                            req,
                            discoveries,
                            address_overrides,
                            fqdn_overrides,
                        )
                        .await,
                    ))
                }
            }
            ENDPOINT_EXPIRE_DHCP_LEASE => {
                let input_bytes = req.into_body().collect().await.unwrap().to_bytes();
                let request = rpc::ExpireDhcpLeaseRequest::decode(input_bytes.slice(5..)).unwrap();
                expired_leases.lock().unwrap().push(request.clone());
                respond(rpc::ExpireDhcpLeaseResponse {
                    ip_address: request.ip_address,
                    status: rpc::ExpireDhcpLeaseStatus::Released.into(),
                })
            }
            path if path == ::rpc::service_path!("Echo") => respond(rpc::EchoResponse {
                message: "dhcp_echo".into(),
            }),
            path if path == ::rpc::service_path!("Version") => respond(rpc::BuildInfo::default()),
            _ => panic!("DHCP -> API wrong uri: {}", req.uri().path()),
        }
    }

    async fn discover_dhcp(
        req: Request<Incoming>,
        discoveries: Arc<Mutex<Vec<rpc::DhcpDiscovery>>>,
        address_overrides: Arc<Mutex<HashMap<String, String>>>,
        fqdn_overrides: Arc<Mutex<HashMap<String, String>>>,
    ) -> Vec<u8> {
        let input_bytes = req.into_body().collect().await.unwrap().to_bytes();

        // slice is to strip the gRPC parts: 1 byte is_compressed and a 4 byte message length
        let disco = rpc::DhcpDiscovery::decode(input_bytes.slice(5..)).unwrap();
        let (address_family, message_kind) = validate_discovery_contract(&disco);
        discoveries.lock().unwrap().push(disco.clone());
        let mac_key = normalized_mac_key(&disco.mac_address);
        let override_for_mac = address_overrides
            .lock()
            .unwrap()
            .get(&mac_key)
            .cloned()
            .or_else(|| {
                (address_family == rpc::AddressFamily::V6
                    && message_kind == Some(rpc::MessageKind::V6InfoRequest))
                .then_some(String::new())
            });
        let fqdn_override_for_mac = fqdn_overrides.lock().unwrap().get(&mac_key).cloned();

        dhcp_response_with_override(
            &disco.mac_address,
            override_for_mac,
            fqdn_override_for_mac,
            address_family,
        )
    }
}

/// Return a stable mock map key for MAC strings that may differ only by case.
fn normalized_mac_key(mac_address: &str) -> String {
    mac_address.to_ascii_lowercase()
}

impl Drop for MockAPIServer {
    // Stop the Hyper server
    fn drop(&mut self) {
        let _ = self.tx.take().expect("missing tx").send(());
        self.handle.abort();
    }
}

struct GrpcBody {
    data: Option<Bytes>,
    trailers: Option<hyper::HeaderMap>,
}

impl GrpcBody {
    fn new(data: Vec<u8>) -> Self {
        let mut trailers = hyper::HeaderMap::new();
        trailers.insert(
            header::HeaderName::from_static("grpc-status"),
            header::HeaderValue::from_static("0"),
        );

        Self {
            data: Some(Bytes::from(data)),
            trailers: Some(trailers),
        }
    }
}

impl HttpBody for GrpcBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if let Some(data) = this.data.take() {
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        if let Some(trailers) = this.trailers.take() {
            return Poll::Ready(Some(Ok(Frame::trailers(trailers))));
        }

        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        self.data.is_none() && self.trailers.is_none()
    }
}

fn grpc_response(body: Vec<u8>) -> Response<GrpcBody> {
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "application/grpc+tonic")
        .body(GrpcBody::new(body))
        .unwrap()
}

/// Takes an rpc object (built from rpc/proto/forge.proto) and turns into into a gRPC response
fn respond(out: impl prost::Message) -> Result<Response<GrpcBody>, MockAPIServerError> {
    let msg_len = out.encoded_len() as u32;
    let mut body = Vec::with_capacity(1 + 4 + msg_len as usize);
    // first byte is compression: 0 means none
    body.push(0u8);
    // next four bytes are length as bigendian u32
    body.extend_from_slice(&msg_len.to_be_bytes());
    // and finally the message
    out.encode(&mut body).unwrap();

    Ok(grpc_response(body))
}
