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
use std::future::{Future, poll_fn};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use std::vec;

use hickory_resolver::config::{LookupIpStrategy, ResolverConfig, ResolverOpts};
use hickory_resolver::net::NetError;
use hickory_resolver::net::runtime::iocompat::AsyncIoTokioAsStd;
use hickory_resolver::net::runtime::{RuntimeProvider, TokioHandle, TokioTime};
use hickory_resolver::proto::rr::domain::usage::ONION;
use hickory_resolver::proto::rr::{Name, RecordType};
use hickory_resolver::{ConnectionProvider, Resolver};
use hyper::service::Service;
use socket2::SockAddr;
use tokio::net::{TcpSocket, TcpStream as TokioTcpStream, UdpSocket as TokioUdpSocket};
use tracing::trace;

#[cfg(target_os = "linux")]
const MGMT_VRF_NAME: &[u8] = "mgmt".as_bytes();

type HickoryResolverFuture = Pin<Box<dyn Future<Output = Result<SocketAddrs, NetError>> + Send>>;

/// `ForgeRuntimeProvider` creates Hickory sockets with optional management-VRF binding.
/// Each TCP connection attempt uses the supplied timeout, or five seconds for `None`.
/// Expiry returns `TimedOut` and drops the connection future and its socket; cancelling
/// the caller's future also drops the attempt. Hickory starts its separate DNS query
/// timeout after the connection succeeds.
#[derive(Clone, Default)]
pub struct ForgeRuntimeProvider {
    handle: TokioHandle,
    use_mgmt_vrf: bool,
}

impl ForgeRuntimeProvider {
    pub fn new() -> Self {
        Self {
            handle: TokioHandle::default(),
            use_mgmt_vrf: false,
        }
    }
    #[must_use]
    pub fn use_mgmt_vrf(mut self, use_mgmt_frf: bool) -> Self {
        self.use_mgmt_vrf = use_mgmt_frf;
        self
    }

    fn create_tcp_socket(
        server_addr: SocketAddr,
        use_mgmt: bool,
    ) -> std::io::Result<socket2::Socket> {
        let socket = socket2::Socket::new(
            socket2::Domain::for_address(server_addr),
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        socket.set_reuse_address(true)?;
        socket.set_nonblocking(true)?;
        if use_mgmt {
            #[cfg(target_os = "linux")]
            socket.bind_device(Some(MGMT_VRF_NAME))?;
        }
        Ok(socket)
    }

    /// Applies the same DNS connect deadline to management-VRF and ordinary sockets.
    async fn connect_tcp_with_timeout(
        connection: impl Future<Output = std::io::Result<TokioTcpStream>>,
        timeout: Option<Duration>,
    ) -> std::io::Result<AsyncIoTokioAsStd<TokioTcpStream>> {
        // Match Hickory's five-second connect limit when none is supplied.
        let timeout = timeout.unwrap_or(Duration::from_secs(5));
        tokio::time::timeout(timeout, connection)
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "DNS TCP connect timed out")
            })?
            .map(AsyncIoTokioAsStd)
    }

    fn create_udp_socket(
        local_addr: SocketAddr,
        use_mgmt: bool,
    ) -> std::io::Result<socket2::Socket> {
        let socket = socket2::Socket::new(
            socket2::Domain::for_address(local_addr),
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        socket.set_nonblocking(true)?;
        if use_mgmt {
            #[cfg(target_os = "linux")]
            socket.bind_device(Some(MGMT_VRF_NAME))?;
        }
        Ok(socket)
    }
}

impl RuntimeProvider for ForgeRuntimeProvider {
    type Handle = TokioHandle;
    type Timer = TokioTime;
    type Udp = TokioUdpSocket;
    type Tcp = AsyncIoTokioAsStd<TokioTcpStream>;

    fn create_handle(&self) -> Self::Handle {
        self.handle.clone()
    }

    fn connect_tcp(
        &self,
        server_addr: SocketAddr,
        _bind_addr: Option<SocketAddr>,
        timeout: Option<Duration>,
    ) -> Pin<Box<dyn Send + Future<Output = std::io::Result<Self::Tcp>>>> {
        if self.use_mgmt_vrf {
            let socket = match ForgeRuntimeProvider::create_tcp_socket(server_addr, true) {
                Ok(socket) => socket,
                Err(io_err) => {
                    return Box::pin(async move { Err(io_err) });
                }
            };

            Box::pin(async move {
                let raw_fd = socket.into_raw_fd();
                // SAFETY: `into_raw_fd` consumes `socket` and transfers ownership of its
                // nonblocking descriptor; `from_raw_fd` immediately assumes that ownership
                // exactly once.
                let tcp_socket: TcpSocket = unsafe { TcpSocket::from_raw_fd(raw_fd) };

                Self::connect_tcp_with_timeout(tcp_socket.connect(server_addr), timeout).await
            })
        } else {
            Box::pin(async move {
                // Without management VRF, we can just use the default TCP socket
                Self::connect_tcp_with_timeout(TokioTcpStream::connect(server_addr), timeout).await
            })
        }
    }

    fn bind_udp(
        &self,
        local_addr: SocketAddr,
        _server_addr: SocketAddr,
    ) -> Pin<Box<dyn Send + Future<Output = std::io::Result<Self::Udp>>>> {
        if self.use_mgmt_vrf {
            let socket = match ForgeRuntimeProvider::create_udp_socket(local_addr, true) {
                Ok(socket) => socket,
                Err(io_err) => {
                    return Box::pin(async move { Err(io_err) });
                }
            };

            let sock_addr = SockAddr::from(local_addr);

            match socket.bind(&sock_addr) {
                Ok(_) => (),
                Err(io_err) => {
                    return Box::pin(async move { Err(io_err) });
                }
            }

            // Convert socket2::Socket -> UdpSocket
            let std_socket: std::net::UdpSocket = socket.into();
            // Convert std::net::UdpSocket to TokioUdpSocket
            let tokio_socket = match TokioUdpSocket::from_std(std_socket) {
                Ok(socket) => socket,
                Err(io_err) => {
                    return Box::pin(async move { Err(io_err) });
                }
            };

            Box::pin(async move { Ok(tokio_socket) })
        } else {
            // Without management VRF, we can just use the default UDP socket
            Box::pin(tokio::net::UdpSocket::bind(local_addr))
        }
    }
}

/// A hyper resolver using `hickory`'s [`TokioAsyncResolver`].
pub type ForgeResolver = HickoryResolver<ForgeRuntimeProvider>;

#[derive(Clone, Debug)]
pub struct ForgeResolverOpts {
    inner: ResolverOpts,
    use_mgmt_vrf: bool,
}

impl Default for ForgeResolverOpts {
    fn default() -> Self {
        let mut inner = ResolverOpts::default();
        // This was default in earlier hickory versions, maintain it here to avoid regressions in
        // improperly-setup dual-stack environments.
        inner.ip_strategy = LookupIpStrategy::Ipv4thenIpv6;

        Self {
            inner,
            use_mgmt_vrf: false,
        }
    }
}

#[derive(Clone)]
pub struct HickoryResolver<C: ConnectionProvider> {
    resolver: Arc<Resolver<C>>,
    /// Known search policy; absent for opaque resolvers and other lookup strategies.
    connection_lookup: Option<Arc<ConnectionLookupConfig>>,
}

/// Retains the search inputs needed to try both address families at each candidate name.
struct ConnectionLookupConfig {
    domain: Option<Name>,
    search: Vec<Name>,
    ndots: usize,
}

/// Ready addresses and an optional unfinished AAAA lookup for the same candidate.
pub(super) struct ConnectingAddrs {
    /// Ready addresses in lookup order.
    pub(super) primary: SocketAddrs,
    /// Owned AAAA lookup for the same candidate. Dropping it releases that future;
    /// Hickory may still finish transport work already submitted to its runtime.
    pub(super) fallback: Option<HickoryResolverFuture>,
}

/// A CNAME can select a search candidate without supplying an address in ANSWER.
/// Keep that distinct from an empty ANSWER, which permits the next candidate.
struct FamilyLookup {
    addrs: SocketAddrs,
    has_answers: bool,
}

/// Iterator over DNS lookup results.
#[derive(Clone)]
pub struct SocketAddrs {
    iter: vec::IntoIter<SocketAddr>,
}

impl Iterator for SocketAddrs {
    type Item = SocketAddr;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

impl ForgeResolverOpts {
    pub fn new() -> Self {
        Self {
            inner: ResolverOpts::default(),
            use_mgmt_vrf: false,
        }
    }

    #[must_use]
    pub fn timeout(mut self, d: Duration) -> Self {
        self.inner.timeout = d;
        self
    }

    #[must_use]
    pub fn use_mgmt_vrf(mut self) -> Self {
        let ignore_mgmt_vrf = std::env::var("IGNORE_MGMT_VRF").is_ok();
        self.use_mgmt_vrf = !ignore_mgmt_vrf;
        self
    }
}

/// Get the default resolver options as configured per crate features.
/// This allows us to enable DNSSEC conditionally.
fn default_opts() -> ForgeResolverOpts {
    ForgeResolverOpts::default()
}

impl ForgeResolver {
    /// Create a new [`ForgeResolver`] with the default config options.
    /// This must be run inside a Tokio runtime context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a resolver with the supplied DNS configuration and options.
    /// `Ipv4AndIpv6` retains search configuration so connection lookups can
    /// return unfinished AAAA work separately; other strategies use batch lookups.
    /// This must be run inside a Tokio runtime context.
    #[must_use]
    pub fn with_config_and_options(config: ResolverConfig, options: ForgeResolverOpts) -> Self {
        let connection_lookup =
            (options.inner.ip_strategy == LookupIpStrategy::Ipv4AndIpv6).then(|| {
                Arc::new(ConnectionLookupConfig {
                    domain: config.domain.clone(),
                    search: config.search.clone(),
                    ndots: options.inner.ndots,
                })
            });
        let rt = ForgeRuntimeProvider::new().use_mgmt_vrf(options.use_mgmt_vrf);
        let resolver = Resolver::builder_with_config(config, rt)
            .with_options(options.inner)
            .build()
            // From looking at the source, this should only happen if TlsConfig::new() fails (ie. if there's no working tls provider)
            .expect("BUG: Error building hickory-dns resolver, cannot handle");
        Self {
            resolver: Arc::new(resolver),
            connection_lookup,
        }
    }
}

impl Default for ForgeResolver {
    fn default() -> Self {
        Self::with_config_and_options(ResolverConfig::default(), default_opts())
    }
}

impl<C: ConnectionProvider> HickoryResolver<C> {
    /// Wraps a Hickory resolver, preserving its batch completion behavior.
    /// Its domain and search configuration are not exposed by Hickory, so
    /// connector lookups cannot safely resolve its families incrementally.
    #[must_use]
    pub fn from_async_resolver(async_resolver: Resolver<C>) -> Self {
        let resolver = Arc::new(async_resolver);

        Self {
            resolver,
            connection_lookup: None,
        }
    }

    /// Returns IPs from the A lookup promptly, retaining unfinished AAAA work.
    /// The connector must handle IP literals before calling this method.
    /// A nonempty ANSWER selects the candidate even if it supplies no IPs, so
    /// success may contain no addresses. Only candidates without ANSWERs from
    /// either lookup are skipped.
    /// Resolvers built through `from_async_resolver` and other lookup strategies
    /// retain their batch behavior. Recheck candidate selection and error precedence
    /// against Hickory's `LookupIpFuture` and `multi_lookup` when upgrading Hickory.
    pub(super) async fn resolve_for_connect(
        &self,
        name: Name,
    ) -> Result<ConnectingAddrs, NetError> {
        let Some(config) = &self.connection_lookup else {
            return Ok(ConnectingAddrs {
                primary: self.call(name).await?,
                fallback: None,
            });
        };

        let mut last_result = Err(NetError::from("can not lookup IPs for no names"));
        for name in config.build_names(name).into_iter().rev() {
            let mut ipv4 = self.lookup_family(name.clone(), RecordType::A);
            let mut ipv6 = self.lookup_family(name, RecordType::AAAA);
            let mut ready_ipv6 = None;

            // Both lookups make progress, but an early AAAA answer waits for A
            // so DNS response timing does not change the lookup order.
            let ipv4_result = poll_fn(|cx| {
                let ipv4_result = ipv4.as_mut().poll(cx);
                if ready_ipv6.is_none()
                    && let Poll::Ready(result) = ipv6.as_mut().poll(cx)
                {
                    ready_ipv6 = Some(result);
                }
                ipv4_result
            })
            .await;
            match ipv4_result {
                Ok(ipv4_lookup) if !ipv4_lookup.addrs.is_empty() => {
                    let (primary, fallback) = match ready_ipv6 {
                        Some(Ok(ipv6)) => (
                            SocketAddrs::new(ipv4_lookup.addrs.chain(ipv6.addrs).collect()),
                            None,
                        ),
                        Some(Err(error)) => {
                            tracing::debug!(%error, "AAAA lookup failed; retaining A lookup addresses");
                            (ipv4_lookup.addrs, None)
                        }
                        None => (
                            ipv4_lookup.addrs,
                            Some(
                                Box::pin(async move { ipv6.await.map(|lookup| lookup.addrs) })
                                    as HickoryResolverFuture,
                            ),
                        ),
                    };
                    return Ok(ConnectingAddrs { primary, fallback });
                }
                ipv4_result => {
                    let ipv6_result = match ready_ipv6 {
                        Some(result) => result,
                        None => ipv6.await,
                    };
                    let candidate_selected = matches!(&ipv4_result, Ok(lookup) if lookup.has_answers)
                        || matches!(&ipv6_result, Ok(lookup) if lookup.has_answers);
                    last_result = match ipv6_result {
                        Ok(ipv6_lookup) if !ipv6_lookup.addrs.is_empty() => {
                            return Ok(ConnectingAddrs {
                                primary: ipv6_lookup.addrs,
                                fallback: None,
                            });
                        }
                        Ok(empty) => Ok(empty.addrs),
                        // Like Hickory's combined lookup, prefer the A error
                        // when both families fail.
                        Err(_) => ipv4_result.map(|lookup| lookup.addrs),
                    };
                    if candidate_selected {
                        break;
                    }
                }
            }
        }

        last_result.map(|primary| ConnectingAddrs {
            primary,
            fallback: None,
        })
    }

    /// Queries one candidate through the existing resolver, sharing hosts, cache,
    /// and DNS settings between the two family lookups.
    /// Like `lookup_ip`, retain every ANSWER IP, including mixed record types.
    /// Ordinary candidates are fully qualified so Hickory does not expand each
    /// family independently. Onion names retain Hickory's local lookup exception.
    fn lookup_family(
        &self,
        name: Name,
        record_type: RecordType,
    ) -> Pin<Box<dyn Future<Output = Result<FamilyLookup, NetError>> + Send>> {
        let resolver = self.resolver.clone();
        Box::pin(async move {
            let response = resolver.lookup(name, record_type).await?;
            trace!(?response, "response from DNS Server");
            Ok(FamilyLookup {
                has_answers: !response.answers().is_empty(),
                addrs: SocketAddrs::new(
                    response
                        .answers()
                        .iter()
                        .filter_map(|record| record.data.ip_addr())
                        .map(|address| SocketAddr::new(address, 0))
                        .collect(),
                ),
            })
        })
    }
}

impl ConnectionLookupConfig {
    /// Returns search candidates in Hickory's reverse precedence order.
    // Recheck this against Hickory's private `Resolver::build_names` on upgrades:
    // independent family expansion could select addresses from different names.
    // Adapted from hickory-resolver 0.26.1, Copyright 2015-2019 Benjamin Fry
    // <benjaminfry@me.com>, under Apache-2.0.
    fn build_names(&self, name: Name) -> Vec<Name> {
        if name.is_fqdn()
            || ONION.zone_of(&name)
                && name
                    .trim_to(2)
                    .iter()
                    .next()
                    .is_some_and(|label| label.len() == 56)
        {
            return vec![name];
        }

        let raw_name_first = name.num_labels() as usize > self.ndots || name.is_localhost();
        let mut names = Vec::with_capacity(2 + self.search.len());
        if !raw_name_first {
            let mut fqdn = name.clone();
            fqdn.set_fqdn(true);
            names.push(fqdn);
        }

        for suffix in self.search.iter().rev().chain(self.domain.iter()) {
            match name.clone().append_domain(suffix) {
                Ok(candidate) => {
                    if !names.contains(&candidate) {
                        names.push(candidate);
                    }
                }
                Err(error) => tracing::debug!(
                    %name,
                    %suffix,
                    %error,
                    "skipping invalid DNS search candidate"
                ),
            }
        }

        if raw_name_first {
            let mut fqdn = name;
            fqdn.set_fqdn(true);
            names.push(fqdn);
        }
        names
    }
}

impl<C: ConnectionProvider> Service<Name> for HickoryResolver<C> {
    type Response = SocketAddrs;
    type Error = NetError;
    type Future = HickoryResolverFuture;

    fn call(&self, name: Name) -> Self::Future {
        let resolver = self.resolver.clone();

        Box::pin(async move {
            let response = resolver.lookup_ip(name.to_string()).await?;
            trace!(?response, "response from DNS Server");
            let addresses = response.iter();

            Ok(SocketAddrs {
                iter: addresses
                    .map(|ip_addr| SocketAddr::new(ip_addr, 0))
                    .collect::<Vec<_>>()
                    .into_iter(),
            })
        })
    }
}

impl SocketAddrs {
    pub(super) fn new(addrs: Vec<SocketAddr>) -> Self {
        SocketAddrs {
            iter: addrs.into_iter(),
        }
    }

    pub fn try_parse(host: &str, port: u16) -> Option<SocketAddrs> {
        if let Ok(addr) = host.parse::<Ipv4Addr>() {
            let addr = SocketAddrV4::new(addr, port);
            return Some(SocketAddrs {
                iter: vec![SocketAddr::V4(addr)].into_iter(),
            });
        }
        if let Ok(addr) = host.parse::<Ipv6Addr>() {
            let addr = SocketAddrV6::new(addr, port, 0, 0);
            return Some(SocketAddrs {
                iter: vec![SocketAddr::V6(addr)].into_iter(),
            });
        }
        None
    }

    #[inline]
    fn filter(self, predicate: impl FnMut(&SocketAddr) -> bool) -> SocketAddrs {
        SocketAddrs::new(self.iter.filter(predicate).collect())
    }

    pub(super) fn split_by_preference(
        self,
        local_addr_ipv4: Option<Ipv4Addr>,
        local_addr_ipv6: Option<Ipv6Addr>,
    ) -> (SocketAddrs, SocketAddrs) {
        match (local_addr_ipv4, local_addr_ipv6) {
            (Some(_), None) => (self.filter(SocketAddr::is_ipv4), SocketAddrs::new(vec![])),
            (None, Some(_)) => (self.filter(SocketAddr::is_ipv6), SocketAddrs::new(vec![])),
            _ => {
                let preferring_v6 = self
                    .iter
                    .as_slice()
                    .first()
                    .is_some_and(SocketAddr::is_ipv6);

                let (preferred, fallback) = self
                    .iter
                    .partition::<Vec<_>, _>(|addr| addr.is_ipv6() == preferring_v6);

                (SocketAddrs::new(preferred), SocketAddrs::new(fallback))
            }
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.iter.as_slice().is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.iter.as_slice().len()
    }
}

#[cfg(test)]
mod tests {
    use std::task::Poll;

    use carbide_test_support::Outcome::FailsWith;
    use carbide_test_support::{Case, Check, check_cases_async, check_values};
    use hickory_resolver::config::{NameServerConfig, ResolveHosts};
    use hickory_resolver::net::DnsError;
    use hickory_resolver::proto::op::{Message, Query};
    use hickory_resolver::proto::rr::rdata::CNAME;
    use hickory_resolver::proto::rr::{RData, Record};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::{Notify, mpsc};
    use tokio::task::JoinHandle;

    use super::*;

    /// A never-ready dial must report `TimedOut` without relying on OS routing
    /// to simulate a blackholed DNS server.
    #[tokio::test(start_paused = true)]
    async fn dns_tcp_connect_uses_supplied_or_default_timeout() {
        check_cases_async(
            [
                Case {
                    scenario: "supplied timeout shorter than the default",
                    input: Some(Duration::from_millis(10)),
                    expect: FailsWith((std::io::ErrorKind::TimedOut, Duration::from_millis(10))),
                },
                Case {
                    scenario: "supplied timeout longer than the default",
                    input: Some(Duration::from_secs(6)),
                    expect: FailsWith((std::io::ErrorKind::TimedOut, Duration::from_secs(6))),
                },
                Case {
                    scenario: "omitted timeout uses the default",
                    input: None,
                    expect: FailsWith((std::io::ErrorKind::TimedOut, Duration::from_secs(5))),
                },
            ],
            |timeout| async move {
                let expected = timeout.unwrap_or(Duration::from_secs(5));
                let start = tokio::time::Instant::now();
                let connection =
                    ForgeRuntimeProvider::connect_tcp_with_timeout(std::future::pending(), timeout);
                tokio::pin!(connection);
                assert!(
                    futures_util::poll!(&mut connection).is_pending(),
                    "connect completed immediately for {timeout:?}"
                );
                tokio::time::advance(expected - Duration::from_millis(1)).await;
                // `advance` moves the clock but does not guarantee expired timers
                // have been processed. Wait for a timer at each boundary before polling.
                tokio::time::sleep_until(start + expected - Duration::from_millis(1)).await;
                assert!(
                    futures_util::poll!(&mut connection).is_pending(),
                    "connect expired before {expected:?} for {timeout:?}"
                );
                tokio::time::advance(Duration::from_millis(1)).await;
                tokio::time::sleep_until(start + expected).await;
                let Poll::Ready(result) = futures_util::poll!(&mut connection) else {
                    panic!("connect remained pending at {expected:?} for {timeout:?}");
                };
                result
                    .map(drop)
                    .map_err(|error| (error.kind(), start.elapsed()))
            },
        )
        .await;
    }

    /// An immediate dial failure keeps its original cause and does not wait for the deadline.
    #[tokio::test(start_paused = true)]
    async fn dns_tcp_connect_preserves_an_immediate_error() {
        let start = tokio::time::Instant::now();
        let original_error = std::io::Error::from_raw_os_error(111);
        let original_kind = original_error.kind();
        let original_message = original_error.to_string();
        let result = ForgeRuntimeProvider::connect_tcp_with_timeout(
            std::future::ready(Err(original_error)),
            Some(Duration::from_secs(5)),
        )
        .await;
        let Err(error) = result else {
            panic!("failed connect must preserve its error");
        };
        assert_eq!(error.kind(), original_kind);
        assert_eq!(error.to_string(), original_message);
        assert_eq!(error.raw_os_error(), Some(111));
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    /// Exercise the provider entrypoint; the socket-family tests bypass its deadline.
    #[tokio::test]
    async fn dns_tcp_connect_returns_a_usable_stream() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
            let server_addr = listener.local_addr()?;
            let AsyncIoTokioAsStd(mut client) = ForgeRuntimeProvider::new()
                .connect_tcp(server_addr, None, Some(Duration::from_secs(5)))
                .await?;
            let (mut server, client_addr) = listener.accept().await?;
            assert_eq!(client.peer_addr()?, server_addr);
            assert_eq!(client.local_addr()?, client_addr);

            client.write_all(b"query").await?;
            let mut received = [0; 5];
            server.read_exact(&mut received).await?;
            assert_eq!(&received, b"query");
            Ok::<(), std::io::Error>(())
        })
        .await
        .expect("DNS TCP exchange timed out")
        .expect("DNS TCP exchange failed");
    }

    /// Opts tests into incremental lookups without changing constructor defaults.
    fn ipv4_and_ipv6_options() -> ForgeResolverOpts {
        let mut options = ForgeResolverOpts::default();
        options.inner.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
        options
    }

    /// Constructor defaults are caller-visible policy; the incremental lookup
    /// capability must not change existing connection behavior by itself.
    #[tokio::test]
    async fn resolver_option_constructors_keep_their_lookup_strategies() {
        check_values(
            [
                Check {
                    scenario: "default retains the legacy IPv4-then-IPv6 strategy",
                    input: ForgeResolverOpts::default(),
                    expect: LookupIpStrategy::Ipv4thenIpv6,
                },
                Check {
                    scenario: "new retains Hickory's IPv6-first batch strategy",
                    input: ForgeResolverOpts::new(),
                    expect: LookupIpStrategy::Ipv6AndIpv4,
                },
            ],
            |options| options.inner.ip_strategy,
        );
        assert!(ForgeResolver::default().connection_lookup.is_none());
    }

    /// Local DNS fixture with recorded questions and an owned, joined responder task.
    /// Explicit records keep address selection independent of external DNS.
    struct DnsServer {
        resolver: ForgeResolver,
        shutdown: mpsc::Sender<()>,
        task: JoinHandle<Vec<Query>>,
    }

    /// Holds one family's replies without blocking the responder or its shutdown.
    struct ReplyGate {
        record_type: RecordType,
        queried: Notify,
        other_replied: Notify,
        release: Notify,
    }

    impl DnsServer {
        /// Holds one family until the test observes the required resolver state.
        async fn start_gated(
            record_type: RecordType,
            options: ForgeResolverOpts,
        ) -> (Self, Arc<ReplyGate>) {
            let gate = Arc::new(ReplyGate {
                record_type,
                queried: Notify::new(),
                other_replied: Notify::new(),
                release: Notify::new(),
            });
            let server = Self::start_with_config(
                vec![
                    address_record("service.test.", Ipv4Addr::LOCALHOST),
                    address_record("service.test.", Ipv6Addr::LOCALHOST),
                ],
                ResolverConfig::default(),
                options,
                Some(gate.clone()),
                vec![],
                None,
            )
            .await;
            (server, gate)
        }

        /// Uses the supplied search rules and timeout with a local nameserver,
        /// excluding system hosts so each test controls every available answer.
        async fn start_with_config(
            answers: Vec<Record>,
            mut config: ResolverConfig,
            mut options: ForgeResolverOpts,
            reply_gate: Option<Arc<ReplyGate>>,
            additionals: Vec<Record>,
            answer_override: Option<(Query, Vec<Record>)>,
        ) -> Self {
            let socket = TokioUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind DNS server");
            let address = socket.local_addr().expect("DNS server address");
            let mut name_server = NameServerConfig::udp(address.ip());
            name_server.connections[0].port = address.port();
            config.name_servers = vec![name_server];
            options.inner.use_hosts_file = ResolveHosts::Never;
            let resolver = ForgeResolver::with_config_and_options(config, options);
            let (shutdown, mut shutdown_rx) = mpsc::channel::<()>(1);
            let task = tokio::spawn(async move {
                let mut queries = Vec::new();
                let mut buffer = [0; 4096];
                let mut gate_open = reply_gate.is_none();
                let mut pending_replies: Vec<(Vec<u8>, SocketAddr)> = Vec::new();
                loop {
                    let (length, peer_address) = tokio::select! {
                        _ = shutdown_rx.recv() => break,
                        _ = async {
                            reply_gate.as_ref().unwrap().release.notified().await;
                        }, if !gate_open => {
                            gate_open = true;
                            for (response, peer_address) in pending_replies.drain(..) {
                                socket.send_to(&response, peer_address)
                                    .await.expect("send gated DNS response");
                            }
                            continue;
                        }
                        received = socket.recv_from(&mut buffer) => {
                            received.expect("receive DNS query")
                        }
                    };
                    let request = Message::from_vec(&buffer[..length]).expect("decode DNS query");
                    let query = request.queries.first().expect("DNS question");
                    queries.push(query.clone());
                    let mut response =
                        Message::response(request.metadata.id, request.metadata.op_code);
                    response.metadata.authoritative = true;
                    response.metadata.recursion_available = true;
                    response.metadata.recursion_desired = request.metadata.recursion_desired;
                    response.add_query(query.clone());
                    if let Some((_, answers)) = answer_override.as_ref().filter(|(q, _)| q == query)
                    {
                        for answer in answers {
                            response.add_answer(answer.clone());
                        }
                    } else {
                        for answer in &answers {
                            if answer.name == *query.name()
                                && (answer.record_type() == query.query_type()
                                    || answer.record_type() == RecordType::CNAME)
                            {
                                response.add_answer(answer.clone());
                            }
                        }
                    }
                    for additional in &additionals {
                        if additional.record_type() == query.query_type() {
                            response.add_additional(additional.clone());
                        }
                    }
                    response.edns = request.edns;
                    let response = response.to_vec().expect("encode DNS response");
                    if let Some(gate) = &reply_gate
                        && query.query_type() == gate.record_type
                        && !gate_open
                    {
                        pending_replies.push((response, peer_address));
                        gate.queried.notify_one();
                        continue;
                    }
                    socket
                        .send_to(&response, peer_address)
                        .await
                        .expect("send DNS response");
                    if let Some(gate) = &reply_gate
                        && query.query_type() != gate.record_type
                    {
                        gate.other_replied.notify_one();
                    }
                }
                queries
            });
            Self {
                resolver,
                shutdown,
                task,
            }
        }

        /// Bounds responder shutdown and joins its task so failures remain visible.
        async fn stop(mut self) -> Vec<Query> {
            drop(self.shutdown);
            match tokio::time::timeout(Duration::from_secs(5), &mut self.task).await {
                Ok(result) => result.expect("DNS server task"),
                Err(_) => {
                    // Keep ownership through cleanup, including a stalled responder.
                    self.task.abort();
                    let result = self.task.await;
                    panic!("DNS server did not stop: {result:?}");
                }
            }
        }
    }

    /// Explicit owner names let search tests detect addresses from the wrong candidate.
    fn address_record(name: &str, address: impl Into<std::net::IpAddr>) -> Record {
        let address: std::net::IpAddr = address.into();
        Record::from_rdata(name.parse().unwrap(), 60, RData::from(address))
    }

    /// Drives the lookup until the fixture observes the required
    /// event. Completion before that event would bypass the branch under test.
    async fn pending_until<F: Future + ?Sized, R: Future>(
        mut operation: Pin<&mut F>,
        ready: R,
    ) -> R::Output {
        tokio::pin!(ready);
        poll_fn(|cx| {
            assert!(
                operation.as_mut().poll(cx).is_pending(),
                "operation completed before gate release"
            );
            ready.as_mut().poll(cx)
        })
        .await
    }

    /// Pending AAAA work must stay on the selected search candidate, then populate
    /// the shared cache so clones and public batch calls need no further DNS traffic.
    #[tokio::test]
    async fn pending_aaaa_retains_the_selected_name_and_shared_cache() {
        let gate = Arc::new(ReplyGate {
            record_type: RecordType::AAAA,
            queried: Notify::new(),
            other_replied: Notify::new(),
            release: Notify::new(),
        });
        let server = DnsServer::start_with_config(
            vec![
                address_record("service.first.test.", Ipv4Addr::LOCALHOST),
                address_record("service.first.test.", Ipv6Addr::LOCALHOST),
                address_record("service.second.test.", Ipv6Addr::from(2_u128)),
            ],
            ResolverConfig::from_parts(
                None,
                vec![
                    "first.test.".parse().unwrap(),
                    "second.test.".parse().unwrap(),
                ],
                vec![],
            ),
            ipv4_and_ipv6_options(),
            Some(gate.clone()),
            vec![],
            None,
        )
        .await;
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let name: Name = "service".parse().unwrap();
            let resolved = server.resolver.resolve_for_connect(name.clone()).await?;
            let ipv4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
            let ipv6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 0));
            assert_eq!(resolved.primary.collect::<Vec<_>>(), vec![ipv4]);
            let mut fallback = resolved.fallback.expect("AAAA is still gated");
            pending_until(fallback.as_mut(), gate.queried.notified()).await;
            gate.release.notify_one();
            assert_eq!(fallback.await?.collect::<Vec<_>>(), vec![ipv6]);

            // Resolver clones and the public batch interface use the same
            // cache populated by the two family lookups.
            let resolver = server.resolver.clone();
            let cached = resolver.resolve_for_connect(name.clone()).await?;
            assert!(cached.fallback.is_none());
            assert_eq!(cached.primary.collect::<Vec<_>>(), vec![ipv4, ipv6]);
            assert_eq!(
                resolver.call(name).await?.collect::<Vec<_>>(),
                vec![ipv4, ipv6]
            );
            Ok::<(), NetError>(())
        })
        .await;
        let queries = server.stop().await;
        result
            .expect("gated lookup timed out")
            .expect("gated lookup failed");
        assert_eq!(queries.len(), 2, "cached lookups issued more queries");
        for record_type in [RecordType::A, RecordType::AAAA] {
            assert!(queries.contains(&Query::query(
                "service.first.test.".parse().unwrap(),
                record_type,
            )));
        }
    }

    /// Dropping the result must release the pending lookup's resolver owner:
    /// the returned result, not a detached task, controls that future's lifetime.
    #[tokio::test]
    async fn dropping_connecting_addresses_releases_the_pending_lookup_owner() {
        let (server, gate) =
            DnsServer::start_gated(RecordType::AAAA, ipv4_and_ipv6_options()).await;
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let owners_before = Arc::strong_count(&server.resolver.resolver);
            let mut resolved = server
                .resolver
                .resolve_for_connect("service.test.".parse().unwrap())
                .await
                .expect("A lookup completes");
            assert_eq!(
                resolved.primary.next(),
                Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            );
            let fallback = resolved.fallback.as_mut().expect("AAAA is still gated");
            pending_until(fallback.as_mut(), gate.queried.notified()).await;
            assert_eq!(
                Arc::strong_count(&server.resolver.resolver),
                owners_before + 1
            );
            drop(resolved);
            assert_eq!(Arc::strong_count(&server.resolver.resolver), owners_before);
        })
        .await;
        server.stop().await;
        result.expect("pending lookup owner test timed out");
    }

    /// Held AAAA replies must keep TCP idle even with A cached. Releasing an empty
    /// reply must not discard that usable A address or lose the caller's URI port.
    #[tokio::test]
    async fn connector_waits_for_empty_aaaa_and_preserves_a_at_the_uri_port() {
        let gate = Arc::new(ReplyGate {
            record_type: RecordType::AAAA,
            queried: Notify::new(),
            other_replied: Notify::new(),
            release: Notify::new(),
        });
        let server = DnsServer::start_with_config(
            vec![address_record("service.test.", Ipv4Addr::LOCALHOST)],
            ResolverConfig::default(),
            ipv4_and_ipv6_options(),
            Some(gate.clone()),
            vec![],
            None,
        )
        .await;
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let listen_address = listener.local_addr().unwrap();
            let primary = server
                .resolver
                .lookup_family("service.test.".parse().unwrap(), RecordType::A)
                .await
                .unwrap();
            assert_eq!(
                primary.addrs.collect::<Vec<_>>(),
                vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]
            );
            let mut connector =
                crate::connector::ForgeHttpConnector::new_with_resolver(server.resolver.clone());
            let uri = format!("http://service.test.:{}/", listen_address.port())
                .parse()
                .unwrap();
            let mut connection = tower_service::Service::call(&mut connector, uri);
            pending_until(connection.as_mut(), gate.queried.notified()).await;

            // A is cached and AAAA is held: TCP must stay idle throughout
            // this window. The post-release accept below checks the same path.
            assert!(
                pending_until(
                    connection.as_mut(),
                    tokio::time::timeout(Duration::from_millis(50), listener.accept()),
                )
                .await
                .is_err(),
                "TCP connected before the AAAA reply"
            );
            gate.release.notify_one();
            let stream = connection.await.expect("empty AAAA retains the A address");
            assert_eq!(stream.inner().peer_addr().unwrap(), listen_address);
            listener
                .accept()
                .await
                .expect("accept connection after AAAA completes");
        })
        .await;
        server.stop().await;
        result.expect("gated connector test timed out");
    }

    /// Hickory accepts both address types in one ANSWER; filtering by query type
    /// would discard addresses that the public batch lookup still returns.
    #[tokio::test]
    async fn mixed_answer_ips_match_hickory_batch_lookup() {
        let expected = vec![
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
        ];
        for public_batch in [false, true] {
            let name: Name = "service.test.".parse().unwrap();
            let server = DnsServer::start_with_config(
                vec![],
                ResolverConfig::default(),
                ipv4_and_ipv6_options(),
                None,
                vec![],
                Some((
                    Query::query(name.clone(), RecordType::A),
                    vec![
                        address_record("service.test.", Ipv4Addr::LOCALHOST),
                        address_record("service.test.", Ipv6Addr::LOCALHOST),
                    ],
                )),
            )
            .await;
            let result = tokio::time::timeout(Duration::from_secs(10), async {
                if public_batch {
                    return server
                        .resolver
                        .call(name)
                        .await
                        .unwrap()
                        .collect::<Vec<_>>();
                }
                let resolved = server.resolver.resolve_for_connect(name).await.unwrap();
                let addresses = resolved.primary.collect::<Vec<_>>();
                if let Some(fallback) = resolved.fallback {
                    assert!(matches!(fallback.await, Err(error) if error.is_no_records_found()));
                }
                addresses
            })
            .await;
            server.stop().await;
            assert_eq!(
                result.expect("mixed ANSWER lookup timed out"),
                expected,
                "public batch: {public_batch}"
            );
        }
    }

    /// DNS reply timing must not promote IPv6 ahead of the preferred IPv4 family.
    #[tokio::test]
    async fn cached_aaaa_waits_for_a_and_keeps_ipv4_first() {
        let (server, gate) = DnsServer::start_gated(RecordType::A, ipv4_and_ipv6_options()).await;
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let ipv6 = server
                .resolver
                .lookup_family("service.test.".parse().unwrap(), RecordType::AAAA)
                .await
                .unwrap();
            assert_eq!(
                ipv6.addrs.collect::<Vec<_>>(),
                vec![SocketAddr::from((Ipv6Addr::LOCALHOST, 0))]
            );
            let lookup = server
                .resolver
                .resolve_for_connect("service.test.".parse().unwrap());
            tokio::pin!(lookup);
            pending_until(lookup.as_mut(), gate.queried.notified()).await;
            gate.release.notify_one();
            lookup.await
        })
        .await;
        server.stop().await;
        let resolved = result
            .expect("gated lookup timed out")
            .expect("gated lookup failed");
        assert!(
            resolved.fallback.is_none(),
            "already available AAAA remained pending"
        );
        assert_eq!(
            resolved.primary.collect::<Vec<_>>(),
            vec![
                SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
            ]
        );
    }

    /// Both families must come from the selected search name, including when
    /// a later candidate is supplied by hosts rather than DNS.
    #[tokio::test]
    async fn incremental_lookup_preserves_search_and_hosts_selection() {
        struct Case {
            scenario: &'static str,
            domain: Option<&'static str>,
            search: &'static [&'static str],
            name: &'static str,
            answers: Vec<Record>,
            additionals: Vec<Record>,
            hosts: &'static str,
            expected: Result<Vec<SocketAddr>, Query>,
            queried_names: &'static [&'static str],
        }

        let ipv4 = Ipv4Addr::LOCALHOST;
        let ipv6 = Ipv6Addr::LOCALHOST;
        let other_ipv4 = Ipv4Addr::new(127, 0, 0, 2);
        let other_ipv6 = Ipv6Addr::from(2_u128);
        let cases = [
            Case {
                scenario: "domain precedes search domains",
                domain: Some("domain.test."),
                search: &["search.test."],
                name: "service",
                answers: vec![
                    address_record("service.domain.test.", ipv4),
                    address_record("service.domain.test.", ipv6),
                    address_record("service.search.test.", other_ipv4),
                ],
                additionals: vec![],
                hosts: "",
                expected: Ok(vec![(ipv4, 0).into(), (ipv6, 0).into()]),
                queried_names: &["service.domain.test."],
            },
            Case {
                scenario: "families stay on the first matching search candidate",
                domain: None,
                search: &["first.test.", "second.test."],
                name: "service",
                answers: vec![
                    address_record("service.first.test.", ipv4),
                    address_record("service.second.test.", other_ipv6),
                ],
                additionals: vec![],
                hosts: "",
                expected: Ok(vec![(ipv4, 0).into()]),
                queried_names: &["service.first.test."],
            },
            Case {
                scenario: "hosts override DNS on a later search candidate",
                domain: None,
                search: &["missing.test.", "hosts.test."],
                name: "service",
                answers: vec![
                    address_record("service.hosts.test.", other_ipv4),
                    address_record("service.hosts.test.", other_ipv6),
                ],
                additionals: vec![],
                hosts: "127.0.0.1 service.hosts.test\n::1 service.hosts.test\n",
                expected: Ok(vec![(ipv4, 0).into(), (ipv6, 0).into()]),
                queried_names: &["service.missing.test."],
            },
            Case {
                scenario: "FQDN bypasses domain and search expansion",
                domain: Some("domain.test."),
                search: &["search.test."],
                name: "service.",
                answers: vec![
                    address_record("service.", ipv4),
                    address_record("service.domain.test.", other_ipv4),
                ],
                additionals: vec![],
                hosts: "",
                expected: Ok(vec![(ipv4, 0).into()]),
                queried_names: &["service."],
            },
            Case {
                scenario: "earlier AAAA-only candidate precedes a later IPv4 answer",
                domain: None,
                search: &["first.test.", "second.test."],
                name: "service",
                answers: vec![
                    address_record("service.first.test.", ipv6),
                    address_record("service.second.test.", other_ipv4),
                ],
                additionals: vec![],
                hosts: "",
                expected: Ok(vec![(ipv6, 0).into()]),
                queried_names: &["service.first.test."],
            },
            Case {
                scenario: "exhausted candidates retain the final A query error",
                domain: None,
                search: &["missing.test."],
                name: "service",
                answers: vec![],
                additionals: vec![],
                hosts: "",
                expected: Err(Query::query("service.".parse().unwrap(), RecordType::A)),
                queried_names: &["service.missing.test.", "service."],
            },
            Case {
                scenario: "CNAME target supplies both families for the selected candidate",
                domain: None,
                search: &["alias.test.", "decoy.test."],
                name: "service",
                answers: vec![
                    Record::from_rdata(
                        "service.alias.test.".parse().unwrap(),
                        60,
                        RData::CNAME(CNAME("target.test.".parse().unwrap())),
                    ),
                    address_record("target.test.", ipv4),
                    address_record("target.test.", ipv6),
                    address_record("service.decoy.test.", other_ipv4),
                ],
                additionals: vec![],
                hosts: "",
                expected: Ok(vec![(ipv4, 0).into(), (ipv6, 0).into()]),
                queried_names: &["service.alias.test.", "target.test."],
            },
            Case {
                scenario: "CNAME selects the candidate even when its IPs are only additional records",
                domain: None,
                search: &["alias.test.", "decoy.test."],
                name: "service",
                answers: vec![
                    Record::from_rdata(
                        "service.alias.test.".parse().unwrap(),
                        60,
                        RData::CNAME(CNAME("target.test.".parse().unwrap())),
                    ),
                    address_record("service.decoy.test.", other_ipv4),
                ],
                additionals: vec![
                    address_record("target.test.", ipv4),
                    address_record("target.test.", ipv6),
                ],
                hosts: "",
                expected: Ok(vec![]),
                queried_names: &["service.alias.test."],
            },
            Case {
                scenario: "additional records alone do not select a search candidate",
                domain: None,
                search: &["empty.test.", "selected.test."],
                name: "service",
                answers: vec![address_record("service.selected.test.", other_ipv4)],
                additionals: vec![address_record("service.empty.test.", ipv4)],
                hosts: "",
                expected: Ok(vec![(other_ipv4, 0).into()]),
                queried_names: &["service.empty.test.", "service.selected.test."],
            },
        ];

        for case in cases {
            let config = ResolverConfig::from_parts(
                case.domain.map(|name| name.parse().unwrap()),
                case.search
                    .iter()
                    .map(|name| name.parse().unwrap())
                    .collect(),
                vec![],
            );
            let mut server = DnsServer::start_with_config(
                case.answers,
                config,
                ipv4_and_ipv6_options(),
                None,
                case.additionals,
                None,
            )
            .await;
            let mut hosts = hickory_resolver::Hosts::default();
            hosts.read_hosts_conf(case.hosts.as_bytes()).unwrap();
            Arc::get_mut(&mut server.resolver.resolver)
                .expect("test resolver is not shared yet")
                .set_hosts(Arc::new(hosts));

            let result = tokio::time::timeout(Duration::from_secs(10), async {
                let resolved = server
                    .resolver
                    .resolve_for_connect(case.name.parse().unwrap())
                    .await?;
                let mut addresses = resolved.primary.collect::<Vec<_>>();
                if let Some(fallback) = resolved.fallback {
                    match fallback.await {
                        Ok(fallback) => addresses.extend(fallback),
                        Err(error) => assert!(error.is_no_records_found(), "{error}"),
                    }
                }
                Ok::<_, NetError>(addresses)
            })
            .await;
            let queries = server.stop().await;
            let addresses = result
                .unwrap_or_else(|_| panic!("{}: lookup timed out", case.scenario))
                .map_err(|error| match error {
                    NetError::Dns(DnsError::NoRecordsFound(no_records)) => *no_records.query,
                    error => panic!("{}: unexpected lookup error: {error}", case.scenario),
                });
            assert_eq!(addresses, case.expected, "{}", case.scenario);
            let expected_queries = case
                .queried_names
                .iter()
                .flat_map(|name| {
                    [RecordType::A, RecordType::AAAA]
                        .map(|record_type| Query::query(name.parse().unwrap(), record_type))
                })
                .collect::<Vec<_>>();
            assert_eq!(
                queries.len(),
                expected_queries.len(),
                "{}: {queries:?}",
                case.scenario
            );
            for query in expected_queries {
                assert!(
                    queries.contains(&query),
                    "{}: missing {query:?}",
                    case.scenario
                );
            }
        }
    }

    /// These boundaries protect the copied candidate policy from selecting a
    /// different name when ndots, duplicate suffixes, or name length matter.
    #[test]
    fn candidate_names_keep_hickory_precedence() {
        let domain = "domain.test.".parse::<Name>().unwrap();
        let search = vec!["search.test.".parse::<Name>().unwrap()];
        let onion = format!("{}.onion", "a".repeat(56)).parse::<Name>().unwrap();
        let long_name = format!("{}.service", "a".repeat(63))
            .parse::<Name>()
            .unwrap();
        let long_suffix = format!("{}.{}.{}", "b".repeat(63), "c".repeat(63), "d".repeat(63))
            .parse::<Name>()
            .unwrap();
        let mut raw_long_name = long_name.clone();
        raw_long_name.set_fqdn(true);
        check_values(
            [
                Check {
                    scenario: "localhost is tried before search even below ndots",
                    input: (
                        "localhost".parse().unwrap(),
                        Some(domain.clone()),
                        search.clone(),
                    ),
                    expect: vec![
                        "localhost.".parse().unwrap(),
                        "localhost.domain.test.".parse().unwrap(),
                        "localhost.search.test.".parse().unwrap(),
                    ],
                },
                Check {
                    scenario: "onion v3 names never expand",
                    input: (onion.clone(), Some(domain.clone()), search.clone()),
                    expect: vec![onion],
                },
                Check {
                    scenario: "more labels than ndots tries the raw name first",
                    input: (
                        "a.b.test".parse().unwrap(),
                        Some(domain.clone()),
                        search.clone(),
                    ),
                    expect: vec![
                        "a.b.test.".parse().unwrap(),
                        "a.b.test.domain.test.".parse().unwrap(),
                        "a.b.test.search.test.".parse().unwrap(),
                    ],
                },
                Check {
                    scenario: "domain and repeated search suffixes are tried once",
                    input: (
                        "service".parse().unwrap(),
                        Some(domain.clone()),
                        vec![domain, search[0].clone(), search[0].clone()],
                    ),
                    expect: vec![
                        "service.domain.test.".parse().unwrap(),
                        "service.search.test.".parse().unwrap(),
                        "service.".parse().unwrap(),
                    ],
                },
                Check {
                    scenario: "oversized appended name does not discard valid candidates",
                    input: (
                        long_name.clone(),
                        None,
                        vec![search[0].clone(), long_suffix],
                    ),
                    expect: vec![long_name.append_domain(&search[0]).unwrap(), raw_long_name],
                },
            ],
            |(name, domain, search)| {
                ConnectionLookupConfig {
                    domain,
                    search,
                    ndots: 2,
                }
                .build_names(name)
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
            },
        );
    }

    /// Public lookups and resolvers without retained search configuration keep
    /// the configured batch ordering and completion behavior.
    #[tokio::test]
    async fn batch_lookups_keep_their_order_and_completion_behavior() {
        struct Case {
            scenario: &'static str,
            options: ForgeResolverOpts,
            opaque: bool,
            public_batch: bool,
            expected: Vec<SocketAddr>,
        }
        let ipv4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let ipv6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 0));
        let cases = [
            Case {
                scenario: "opaque resolver keeps its configured IPv4-first batch",
                options: ipv4_and_ipv6_options(),
                opaque: true,
                public_batch: false,
                expected: vec![ipv4, ipv6],
            },
            Case {
                scenario: "new retains Hickory's IPv6-first batch",
                options: ForgeResolverOpts::new(),
                opaque: false,
                public_batch: false,
                expected: vec![ipv6, ipv4],
            },
            Case {
                scenario: "public Service lookup remains a batch on an incremental resolver",
                options: ipv4_and_ipv6_options(),
                opaque: false,
                public_batch: true,
                expected: vec![ipv4, ipv6],
            },
        ];
        for case in cases {
            let (server, gate) = DnsServer::start_gated(RecordType::AAAA, case.options).await;
            let resolver = if case.opaque {
                ForgeResolver::from_async_resolver(server.resolver.resolver.as_ref().clone())
            } else {
                server.resolver.clone()
            };
            let result = tokio::time::timeout(Duration::from_secs(10), async {
                // A second lookup confirms A is usable from the shared cache;
                // the batch call must still wait for the gated AAAA answer.
                let lookup = async {
                    let name = "service.test.".parse().unwrap();
                    if case.public_batch {
                        assert!(resolver.connection_lookup.is_some());
                        Ok(ConnectingAddrs {
                            primary: resolver.call(name).await?,
                            fallback: None,
                        })
                    } else {
                        resolver.resolve_for_connect(name).await
                    }
                };
                tokio::pin!(lookup);
                pending_until(lookup.as_mut(), async {
                    gate.queried.notified().await;
                    gate.other_replied.notified().await;
                    let primary = resolver
                        .lookup_family("service.test.".parse().unwrap(), RecordType::A)
                        .await
                        .unwrap();
                    assert_eq!(primary.addrs.collect::<Vec<_>>(), vec![ipv4]);
                })
                .await;
                gate.release.notify_one();
                lookup.await
            })
            .await;
            server.stop().await;
            let resolved = result
                .unwrap_or_else(|_| panic!("{}: lookup timed out", case.scenario))
                .unwrap_or_else(|error| panic!("{}: lookup failed: {error}", case.scenario));
            assert!(resolved.fallback.is_none(), "{}", case.scenario);
            assert_eq!(
                resolved.primary.collect::<Vec<_>>(),
                case.expected,
                "{}",
                case.scenario
            );
        }
    }

    #[tokio::test]
    async fn tcp_socket_connects_with_the_server_address_family() {
        for listen_addr in [
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
        ] {
            tokio::time::timeout(Duration::from_secs(5), async {
                let listener = TcpListener::bind(listen_addr).await?;
                let server_addr = listener.local_addr()?;
                let socket = ForgeRuntimeProvider::create_tcp_socket(server_addr, false)?;
                let mut client = TcpSocket::from_std_stream(socket.into())
                    .connect(server_addr)
                    .await?;
                let (mut server, _) = listener.accept().await?;

                client.write_all(b"query").await?;
                let mut received = [0; 5];
                server.read_exact(&mut received).await?;
                assert_eq!(&received, b"query");

                server.write_all(b"reply").await?;
                client.read_exact(&mut received).await?;
                assert_eq!(&received, b"reply");
                Ok::<(), std::io::Error>(())
            })
            .await
            .unwrap_or_else(|_| panic!("TCP exchange timed out for {listen_addr}"))
            .unwrap_or_else(|error| panic!("TCP exchange failed for {listen_addr}: {error}"));
        }
    }

    #[tokio::test]
    async fn udp_socket_binds_with_the_local_address_family() {
        for local_addr in [
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
        ] {
            tokio::time::timeout(Duration::from_secs(5), async {
                let server = TokioUdpSocket::bind(local_addr).await?;
                let server_addr = server.local_addr()?;
                let socket = ForgeRuntimeProvider::create_udp_socket(local_addr, false)?;
                socket.bind(&SockAddr::from(local_addr))?;
                let client = TokioUdpSocket::from_std(socket.into())?;

                client.send_to(b"query", server_addr).await?;
                let mut received = [0; 5];
                let (length, client_addr) = server.recv_from(&mut received).await?;
                assert_eq!(&received[..length], b"query");

                server.send_to(b"reply", client_addr).await?;
                let (length, peer_addr) = client.recv_from(&mut received).await?;
                assert_eq!(&received[..length], b"reply");
                assert_eq!(peer_addr, server_addr);
                Ok::<(), std::io::Error>(())
            })
            .await
            .unwrap_or_else(|_| panic!("UDP exchange timed out for {local_addr}"))
            .unwrap_or_else(|error| panic!("UDP exchange failed for {local_addr}: {error}"));
        }
    }
}
