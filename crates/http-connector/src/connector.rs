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
use std::error::Error as StdError;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use std::{fmt, io};

use futures_util::future::Either;
use hickory_resolver::proto::rr::Name;
use hyper::http::Uri;
use hyper::http::uri::Scheme;
use hyper_util::rt::TokioIo;
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::{Instant as TokioInstant, Sleep};
use tokio_socks::tcp::Socks5Stream;
use tracing::{info, trace, warn};
use tryhard::RetryFutureConfig;

use crate::resolver;
use crate::resolver::ForgeResolver;

type ConnectResult = Result<TokioIo<TcpStream>, ConnectError>;

/// ConnectorMetrics is intended as an ever-evolving metrics
/// container of sorts, to allow a caller to collect connection
/// level metrics for a ForgeClient. Since the underlying data
/// gets passed all over the place (and gets buried deep inside
/// of a hyper client), there were some considerations made here
/// in the underlying "inner" data being wrapped in Arcs, with
/// some being AtomicU32, and another case for <Arc<Mutex>> with
/// some HashMaps.
///
/// All said, it gets wrapped together with some getters, and at
/// the end of the day you just need to call metrics.clone() to
/// get another metrics instance with Arc::cloned references in
/// it.
/// Process-wide connect totals across every connector instance -- what
/// [`register_global_metrics`] exposes. The per-connector [`ConnectorMetrics`]
/// remain in-memory instrumentation (their per-addr maps are unbounded and
/// test-oriented); these totals are bumped on every connect regardless.
struct GlobalConnectTotals {
    attempts: AtomicU64,
    successes: AtomicU64,
    errors: AtomicU64,
}

static GLOBAL_TOTALS: GlobalConnectTotals = GlobalConnectTotals {
    attempts: AtomicU64::new(0),
    successes: AtomicU64::new(0),
    errors: AtomicU64::new(0),
};

/// Registers the process-wide TCP connect counters on `meter`:
/// `carbide_client_tcp_connect_attempts_total`, `_successes_total`, and
/// `_errors_total` (the exporter appends the `_total`). Call once, after the
/// meter provider exists; the totals accumulate from process start either way.
pub fn register_global_metrics(meter: &opentelemetry::metrics::Meter) {
    let instruments = [
        (
            "carbide_client_tcp_connect_attempts",
            "Number of outbound TCP connect attempts across all HTTP connectors",
            &GLOBAL_TOTALS.attempts,
        ),
        (
            "carbide_client_tcp_connect_successes",
            "Number of successful outbound TCP connects across all HTTP connectors",
            &GLOBAL_TOTALS.successes,
        ),
        (
            "carbide_client_tcp_connect_errors",
            "Number of failed outbound TCP connect attempts across all HTTP connectors",
            &GLOBAL_TOTALS.errors,
        ),
    ];
    for (name, description, total) in instruments {
        meter
            .u64_observable_counter(name)
            .with_description(description)
            .with_callback(move |observer| {
                observer.observe(total.load(Ordering::Relaxed), &[]);
            })
            .build();
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConnectorMetrics {
    inner: ConnectorMetricsInner,
}

impl ConnectorMetrics {
    // connect_success is called when the underlying TCP
    // connection attempt was successful, updating both
    // overall and addr-specific metrics.
    fn connect_success(&mut self, addr: SocketAddr, connect_start: Instant) {
        self.inner.connect_success(addr, connect_start);
    }

    // connect_error is called when the underlying TCP
    // connection attempt fails, updating both overall
    // and addr-specific metrics.
    fn connect_error(&mut self, addr: SocketAddr, connect_start: Instant, e: &ConnectError) {
        self.inner.connect_error(addr, connect_start, e);
    }

    pub fn get_total_attempts(&mut self) -> u32 {
        self.inner.total_attempts.load(Ordering::SeqCst)
    }

    pub fn get_total_successes(&mut self) -> u32 {
        self.inner.total_successes.load(Ordering::SeqCst)
    }

    pub fn get_total_errors(&mut self) -> u32 {
        self.inner.total_errors.load(Ordering::SeqCst)
    }

    pub fn get_attempts_for_addr(&mut self, addr: &SocketAddr) -> Option<u32> {
        self.inner
            .addr_maps
            .lock()
            .unwrap()
            .attempts_by_addr
            .get(addr)
            .copied()
    }

    pub fn get_successes_for_addr(&mut self, addr: &SocketAddr) -> Option<u32> {
        self.inner
            .addr_maps
            .lock()
            .unwrap()
            .successes_by_addr
            .get(addr)
            .copied()
    }

    pub fn get_errors_for_addr(&mut self, addr: &SocketAddr) -> Option<u32> {
        self.inner
            .addr_maps
            .lock()
            .unwrap()
            .errors_by_addr
            .get(addr)
            .copied()
    }
}

/// ConnectorMetricsInner is the "inner" data for
/// a ConnectorMetrics instance, containing Arc-wrapped
/// data for easily passing around wherever it ends
/// up going. Uses of mutexes are limited to places where
/// it is needed, such as HashMaps, which are stored in
/// their own struct. Integer types are just stored within
/// AtomicUsize and/or equivalents.
#[derive(Clone, Debug, Default)]
pub struct ConnectorMetricsInner {
    pub total_attempts: Arc<AtomicU32>,
    pub total_successes: Arc<AtomicU32>,
    pub total_errors: Arc<AtomicU32>,
    pub addr_maps: Arc<Mutex<ConnectorMetricsInnerMaps>>,
}

impl ConnectorMetricsInner {
    // connect_attempt is called any time there is a connection attempt,
    // whether success or fail, and as such, is simply called by
    // connect_success and connect_fail. This logs basic metrics that an
    // attempt was made, along with latency data.
    fn connect_attempt(&mut self, addr: SocketAddr, connect_start: Instant) {
        self.addr_maps
            .lock()
            .unwrap()
            .add_attempt_by_addr(addr, connect_start);
        self.total_attempts.fetch_add(1, Ordering::SeqCst);
        GLOBAL_TOTALS.attempts.fetch_add(1, Ordering::Relaxed);
    }

    // connect_success is the "inner" function for handling a
    // connect_success call. See the outer ConnectorMetrics::connect_success
    // for more details of what this does.
    fn connect_success(&mut self, addr: SocketAddr, connect_start: Instant) {
        self.connect_attempt(addr, connect_start);
        self.total_successes.fetch_add(1, Ordering::SeqCst);
        GLOBAL_TOTALS.successes.fetch_add(1, Ordering::Relaxed);
        self.addr_maps.lock().unwrap().add_success_by_addr(addr);
        trace!(endpoint_address = %addr, "connected");
    }

    // connect_error is the "inner" function for handling a
    // connect_error call. See the outer ConnectorMetrics::connect_error
    // for more details of what this does.
    fn connect_error(&mut self, addr: SocketAddr, connect_start: Instant, e: &ConnectError) {
        self.connect_attempt(addr, connect_start);
        self.total_errors.fetch_add(1, Ordering::SeqCst);
        GLOBAL_TOTALS.errors.fetch_add(1, Ordering::Relaxed);
        self.addr_maps.lock().unwrap().add_error_by_addr(addr);
        info!(endpoint_address = %addr, error = ?e, "connect error");
    }
}

/// ConnectorMetricsInnerMaps are the internal
/// "by addr" maps for tracking metrics, and are
/// wrapped together in an <Arc<Mutex>>, since
/// generally > 1 map needs to be updated for a
/// single operation (and also because they need
/// to be mutated within the Arc as well).
#[derive(Clone, Debug, Default)]
pub struct ConnectorMetricsInnerMaps {
    pub attempts_by_addr: HashMap<SocketAddr, u32>,
    pub successes_by_addr: HashMap<SocketAddr, u32>,
    pub errors_by_addr: HashMap<SocketAddr, u32>,
    // XXX: This currently grows infinitely. If for some
    // reason this ended up having millions and millions
    // of connection attempts, it would get big. Maybe
    // switch to an average or something.
    pub latency_by_addr: HashMap<SocketAddr, Vec<Duration>>,
}

impl ConnectorMetricsInnerMaps {
    // add_attempt_by_addr logs a single connection attempt (with
    // latency data), for a given SocketAddr.
    pub fn add_attempt_by_addr(&mut self, addr: SocketAddr, connect_start: Instant) {
        let connect_end = Instant::now();
        let connect_elapsed = connect_end.saturating_duration_since(connect_start);
        let attempts_for_addr = self.attempts_by_addr.entry(addr).or_default();
        let latency_for_addr = self.latency_by_addr.entry(addr).or_default();
        *attempts_for_addr += 1;
        latency_for_addr.push(connect_elapsed);
    }

    // add_success_by_addr logs a single connection success
    // for a given SocketAddr.
    pub fn add_success_by_addr(&mut self, addr: SocketAddr) {
        let successes_for_addr = self.successes_by_addr.entry(addr).or_insert(0);
        *successes_for_addr += 1;
    }

    // add_error_by_addr logs a single connection failure
    // for a given SocketAddr.
    pub fn add_error_by_addr(&mut self, addr: SocketAddr) {
        let errors_for_addr = self.errors_by_addr.entry(addr).or_insert(0);
        *errors_for_addr += 1;
    }
}

pub struct ConnectError {
    msg: Box<str>,
    cause: Option<Box<dyn StdError + Send + Sync>>,
}

struct ConnectingTcp<'a> {
    preferred: ConnectingTcpRemote,
    fallback: Option<ConnectingTcpFallback>,
    config: &'a Config,
}

struct ConnectingTcpFallback {
    delay: Sleep,
    remote: ConnectingTcpRemote,
}

struct ConnectingTcpRemote {
    addrs: resolver::SocketAddrs,
    connect_timeout: Option<Duration>,
    metrics: ConnectorMetrics,
}

impl ConnectingTcpRemote {
    fn new(
        addrs: resolver::SocketAddrs,
        connect_timeout: Option<Duration>,
        metrics: ConnectorMetrics,
    ) -> Self {
        // Family filtering can remove every address. Let
        // `ConnectingTcpRemote::connect` return its normal connection error
        // instead of dividing by zero here.
        let connect_timeout = connect_timeout.map(|t| t / addrs.len().max(1) as u32);

        Self {
            addrs,
            connect_timeout,
            metrics,
        }
    }
}

impl ConnectingTcpRemote {
    async fn connect(&mut self, config: &Config) -> Result<TcpStream, ConnectError> {
        let proxy_addr = proxy_address(config)?;
        let mut err = None;
        for addr in &mut self.addrs {
            let connect_start = Instant::now();
            let metric_addr = proxy_addr.unwrap_or(addr);
            match connect_address(addr, proxy_addr, config, self.connect_timeout).await {
                Ok(tcp) => {
                    self.metrics.connect_success(metric_addr, connect_start);
                    return Ok(tcp);
                }
                Err(e) => {
                    self.metrics.connect_error(metric_addr, connect_start, &e);
                    err = Some(e);
                }
            }
        }

        match err {
            Some(e) => Err(e),
            None => Err(ConnectError::new(
                "tcp connect error",
                std::io::Error::new(std::io::ErrorKind::NotConnected, "Network unreachable"),
            )),
        }
    }
}

/// Reject malformed proxy configuration before attempting individual addresses.
fn proxy_address(config: &Config) -> Result<Option<SocketAddr>, ConnectError> {
    config
        .socks5_proxy
        .as_deref()
        .map(SocketAddr::from_str)
        .transpose()
        .map_err(|error| ConnectError::new("invalid proxy setting", error))
}

/// `connect_address` keeps setup and connection failures in the same attempt,
/// so either failure lets the caller try the next eligible address.
async fn connect_address(
    addr: SocketAddr,
    proxy_addr: Option<SocketAddr>,
    config: &Config,
    timeout: Option<Duration>,
) -> Result<TcpStream, ConnectError> {
    match proxy_addr {
        Some(proxy) => connect_with_socks_proxy(proxy, addr, config.clone(), timeout)?.await,
        None => connect(&addr, config, timeout)?.await,
    }
}

fn bind_local_address(
    socket: &socket2::Socket,
    dst_addr: &SocketAddr,
    local_addr_ipv4: &Option<Ipv4Addr>,
    local_addr_ipv6: &Option<Ipv6Addr>,
) -> io::Result<()> {
    match (*dst_addr, local_addr_ipv4, local_addr_ipv6) {
        (SocketAddr::V4(_), Some(addr), _) => {
            let ip_addr: IpAddr = (*addr).into();
            socket.bind(&SocketAddr::new(ip_addr, 0).into())?;
        }
        (SocketAddr::V6(_), _, Some(addr)) => {
            let ip_addr: IpAddr = (*addr).into();
            socket.bind(&SocketAddr::new(ip_addr, 0).into())?;
        }
        _ => {
            if cfg!(windows) {
                // Windows requires a socket be bound before calling connect
                let any: SocketAddr = match *dst_addr {
                    SocketAddr::V4(_) => ([0, 0, 0, 0], 0).into(),
                    SocketAddr::V6(_) => ([0, 0, 0, 0, 0, 0, 0, 0], 0).into(),
                };
                socket.bind(&any.into())?;
            }
        }
    }

    Ok(())
}

fn connect(
    addr: &SocketAddr,
    config: &Config,
    connect_timeout: Option<Duration>,
) -> Result<impl Future<Output = Result<TcpStream, ConnectError>>, ConnectError> {
    // TODO(eliza): if Tokio's `TcpSocket` gains support for setting the
    // keepalive timeout, it would be nice to use that instead of socket2,
    // and avoid the unsafe `into_raw_fd`/`from_raw_fd` dance...
    use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};

    let domain = Domain::for_address(*addr);
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(ConnectError::message("tcp open error"))?;

    // When constructing a Tokio `TcpSocket` from a raw fd/socket, the user is
    // responsible for ensuring O_NONBLOCK is set.
    socket
        .set_nonblocking(true)
        .map_err(ConnectError::message("tcp set_nonblocking error"))?;

    if let Some(dur) = config.keep_alive_timeout {
        let mut conf = TcpKeepalive::new().with_time(dur);
        if let Some(dur) = config.keep_alive_interval {
            conf = conf.with_interval(dur);
        }
        if let Some(retries) = config.keep_alive_retries {
            conf = conf.with_retries(retries)
        }
        if let Err(e) = socket.set_tcp_keepalive(&conf) {
            warn!(error = %e, "tcp set_keepalive error");
        }
    }
    #[cfg(target_os = "linux")]
    socket
        .set_tcp_user_timeout(config.tcp_user_timeout)
        .map_err(ConnectError::message("set tcp_user_timeout error"))?;

    #[cfg(target_os = "linux")]
    // That this only works for some socket types, particularly AF_INET sockets.
    if let Some(interface) = &config.interface {
        socket
            .bind_device(Some(interface.as_bytes()))
            .map_err(ConnectError::message("tcp bind interface error"))?;
    }

    bind_local_address(
        &socket,
        addr,
        &config.local_address_ipv4,
        &config.local_address_ipv6,
    )
    .map_err(ConnectError::message("tcp bind local error"))?;

    #[cfg(unix)]
    // SAFETY: `into_raw_fd` consumes `socket` and transfers ownership of its descriptor;
    // `from_raw_fd` immediately assumes that ownership exactly once. The descriptor was also
    // configured as nonblocking above, as required by `TcpSocket`.
    let socket = unsafe {
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        TcpSocket::from_raw_fd(socket.into_raw_fd())
    };

    if config.reuse_address
        && let Err(e) = socket.set_reuseaddr(true)
    {
        warn!(error = %e, "tcp set_reuse_address error");
    }

    if let Some(size) = config.send_buffer_size
        && let Err(e) = socket.set_send_buffer_size(size.try_into().unwrap_or(u32::MAX))
    {
        warn!(error = %e, "tcp set_buffer_size error");
    }

    if let Some(size) = config.recv_buffer_size
        && let Err(e) = socket.set_recv_buffer_size(size.try_into().unwrap_or(u32::MAX))
    {
        warn!(error = %e, "tcp set_recv_buffer_size error");
    }

    let connect = socket.connect(*addr);
    Ok(async move {
        match connect_timeout {
            Some(dur) => match tokio::time::timeout(dur, connect).await {
                Ok(Ok(s)) => Ok(s),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(io::Error::new(io::ErrorKind::TimedOut, e)),
            },
            None => connect.await,
        }
        .map_err(ConnectError::message("tcp connect error"))
    })
}

async fn proxy_connect(
    proxy_addr: SocketAddr,
    target: SocketAddr,
    config: &Config,
) -> Result<TcpStream, ConnectError> {
    let proxy_stream = connect(&proxy_addr, config, None)?.await?;

    Ok(Socks5Stream::connect_with_socket(proxy_stream, target)
        .await
        .map_err(|e| ConnectError::new("proxy connect error: {}", e))?
        .into_inner())
}

fn connect_with_socks_proxy(
    proxy_addr: SocketAddr,
    target: SocketAddr,
    config: Config,
    connect_timeout: Option<Duration>,
) -> Result<impl Future<Output = Result<TcpStream, ConnectError>>, ConnectError> {
    Ok(async move {
        let target_connect = proxy_connect(proxy_addr, target, &config);

        match connect_timeout {
            Some(dur) => match tokio::time::timeout(dur, target_connect).await {
                Ok(Ok(s)) => Ok(s),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(ConnectError::new(
                    "Proxy connect timed out",
                    io::Error::new(io::ErrorKind::TimedOut, e),
                )),
            },
            None => target_connect.await,
        }
    })
}

impl<'a> ConnectingTcp<'a> {
    fn new(
        remote_addrs: resolver::SocketAddrs,
        config: &'a Config,
        metrics: ConnectorMetrics,
    ) -> Self {
        trace!(?config, "ConnectingTcp config");

        if let Some(fallback_timeout) = config.happy_eyeballs_timeout {
            let (preferred_addrs, fallback_addrs) = remote_addrs
                .split_by_preference(config.local_address_ipv4, config.local_address_ipv6);
            if fallback_addrs.is_empty() {
                return ConnectingTcp {
                    preferred: ConnectingTcpRemote::new(
                        preferred_addrs,
                        config.connect_timeout,
                        metrics,
                    ),
                    fallback: None,
                    config,
                };
            }

            ConnectingTcp {
                preferred: ConnectingTcpRemote::new(
                    preferred_addrs,
                    config.connect_timeout,
                    metrics.clone(),
                ),
                fallback: Some(ConnectingTcpFallback {
                    delay: tokio::time::sleep(fallback_timeout),
                    remote: ConnectingTcpRemote::new(
                        fallback_addrs,
                        config.connect_timeout,
                        metrics,
                    ),
                }),
                config,
            }
        } else {
            let remote_addrs = eligible_addresses(remote_addrs, config);
            ConnectingTcp {
                preferred: ConnectingTcpRemote::new(remote_addrs, config.connect_timeout, metrics),
                fallback: None,
                config,
            }
        }
    }
}

impl ConnectingTcp<'_> {
    async fn connect(mut self) -> Result<TcpStream, ConnectError> {
        match self.fallback {
            None => self.preferred.connect(self.config).await,
            Some(mut fallback) => {
                let preferred_fut = self.preferred.connect(self.config);
                futures_util::pin_mut!(preferred_fut);

                let fallback_fut = fallback.remote.connect(self.config);
                futures_util::pin_mut!(fallback_fut);

                let fallback_delay = fallback.delay;
                futures_util::pin_mut!(fallback_delay);

                let (result, future) =
                    match futures_util::future::select(preferred_fut, fallback_delay).await {
                        Either::Left((result, _fallback_delay)) => {
                            (result, Either::Right(fallback_fut))
                        }
                        Either::Right(((), preferred_fut)) => {
                            // Delay is done, start polling both the preferred and the fallback
                            futures_util::future::select(preferred_fut, fallback_fut)
                                .await
                                .factor_first()
                        }
                    };

                if result.is_err() {
                    // Fallback to the remaining future (could be preferred or fallback)
                    // if we get an error
                    future.await
                } else {
                    result
                }
            }
        }
    }
}

impl ConnectError {
    fn new<S, E>(msg: S, cause: E) -> ConnectError
    where
        S: Into<Box<str>>,
        E: Into<Box<dyn StdError + Send + Sync>>,
    {
        ConnectError {
            msg: msg.into(),
            cause: Some(cause.into()),
        }
    }

    fn dns<E>(cause: E) -> ConnectError
    where
        E: Into<Box<dyn StdError + Send + Sync>>,
    {
        ConnectError::new("dns error", cause)
    }

    fn message<S, E>(msg: S) -> impl FnOnce(E) -> ConnectError
    where
        S: Into<Box<str>>,
        E: Into<Box<dyn StdError + Send + Sync>>,
    {
        move |cause| ConnectError::new(msg, cause)
    }
}

impl fmt::Debug for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref cause) = self.cause {
            f.debug_tuple("ConnectError")
                .field(&self.msg)
                .field(cause)
                .finish()
        } else {
            self.msg.fmt(f)
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)?;

        if let Some(ref cause) = self.cause {
            write!(f, ": {cause}")?;
        }

        Ok(())
    }
}

impl StdError for ConnectError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.cause.as_ref().map(|e| &**e as _)
    }
}

#[derive(Clone)]
pub struct ForgeHttpConnector {
    config: Arc<Config>,
    resolver: ForgeResolver,

    // Since the ForgeHttpConnector gets buried
    // deep inside a tower service connector inside
    // a hyper client inside a gRPC client, being
    // able to get at metrics data (and pass it
    // around, and mutate it across connection handling
    // mechanisms), metrics needs to be within a Mutex
    // within an Arc.
    metrics: ConnectorMetrics,
}

#[derive(Clone, Debug, Default)]
struct Config {
    connect_timeout: Option<Duration>,
    enforce_http: bool,
    happy_eyeballs_timeout: Option<Duration>,
    keep_alive_timeout: Option<Duration>,
    keep_alive_interval: Option<Duration>,
    keep_alive_retries: Option<u32>,
    tcp_user_timeout: Option<Duration>,
    local_address_ipv4: Option<Ipv4Addr>,
    local_address_ipv6: Option<Ipv6Addr>,
    nodelay: bool,
    reuse_address: bool,
    send_buffer_size: Option<usize>,
    recv_buffer_size: Option<usize>,
    #[cfg(target_os = "linux")]
    interface: Option<String>,
    socks5_proxy: Option<String>,
    connect_retries_max: Option<u32>,
    connect_retries_interval: Option<Duration>,
}

impl ForgeHttpConnector {
    #[must_use]
    pub fn new_with_resolver(resolver: ForgeResolver) -> Self {
        ForgeHttpConnector {
            config: Arc::new(Config::default()),
            resolver,
            metrics: ConnectorMetrics::default(),
        }
    }

    /// Option to enforce all `Uri`s have the `http` scheme.
    ///
    /// Enabled by default.
    #[inline]
    pub fn enforce_http(&mut self, is_enforced: bool) {
        self.config_mut().enforce_http = is_enforced;
    }

    /// Set both `SO_KEEPALIVE` to true, and `TCP_KEEPIDLE`  to the given duration.
    /// You must set this if using set_keepalive_interval or set_keepalive_retries.
    ///
    /// See https://blog.cloudflare.com/when-tcp-sockets-refuse-to-die/ for how
    /// the keep alive and tcp_user_timeout values work together.
    pub fn set_keepalive_time(&mut self, dur: Option<Duration>) {
        self.config_mut().keep_alive_timeout = dur;
    }

    /// Set TCP_KEEPINTVL
    /// Must also set_keepalive_time
    pub fn set_keepalive_interval(&mut self, dur: Option<Duration>) {
        self.config_mut().keep_alive_interval = dur;
    }

    /// Set TCP_KEEPCNT
    /// Must also set_keepalive_time
    pub fn set_keepalive_retries(&mut self, retries: Option<u32>) {
        self.config_mut().keep_alive_retries = retries;
    }

    /// Set TCP_USER_TIMEOUT
    pub fn set_tcp_user_timeout(&mut self, dur: Option<Duration>) {
        self.config_mut().tcp_user_timeout = dur;
    }

    /// Set that all sockets have `SO_NODELAY` set to the supplied value `nodelay`.
    ///
    /// Default is `false`.
    #[inline]
    pub fn set_nodelay(&mut self, nodelay: bool) {
        self.config_mut().nodelay = nodelay;
    }

    /// Sets the value of the `SO_SNDBUF` option on the socket.
    #[inline]
    pub fn set_send_buffer_size(&mut self, size: Option<usize>) {
        self.config_mut().send_buffer_size = size;
    }

    /// Sets the value of the `SO_RCVBUF` option on the socket.
    #[inline]
    pub fn set_recv_buffer_size(&mut self, size: Option<usize>) {
        self.config_mut().recv_buffer_size = size;
    }

    /// Set that all sockets are bound to the configured address before connection.
    ///
    /// If `None`, the sockets will not be bound.
    ///
    /// Default is `None`.
    #[inline]
    pub fn set_local_address(&mut self, addr: Option<IpAddr>) {
        let (v4, v6) = match addr {
            Some(IpAddr::V4(a)) => (Some(a), None),
            Some(IpAddr::V6(a)) => (None, Some(a)),
            _ => (None, None),
        };

        let cfg = self.config_mut();

        cfg.local_address_ipv4 = v4;
        cfg.local_address_ipv6 = v6;
    }

    /// Set that all sockets are bound to the configured IPv4 or IPv6 address (depending on host's
    /// preferences) before connection.
    #[inline]
    pub fn set_local_addresses(&mut self, addr_ipv4: Ipv4Addr, addr_ipv6: Ipv6Addr) {
        let cfg = self.config_mut();

        cfg.local_address_ipv4 = Some(addr_ipv4);
        cfg.local_address_ipv6 = Some(addr_ipv6);
    }

    /// Set the connect timeout.
    ///
    /// Each connection round divides this timeout among its resolved addresses
    /// (per family when Happy Eyeballs is enabled). With `Ipv4AndIpv6`, ready
    /// A-lookup addresses can start while AAAA is pending, including retries.
    /// New eligible addresses share one timeout with the current and remaining
    /// attempts, starting when they arrive or their queue is first admitted.
    /// A running attempt's deadline can shorten but never extend. Happy Eyeballs
    /// budgets each actual IP family separately; only a family receiving new
    /// addresses changes its budget. A reply without eligible addresses leaves
    /// existing deadlines intact. Later retries each receive a fresh timeout.
    ///
    /// DNS queries retain separate timeouts and retries, so this is not an
    /// overall request deadline. Opaque resolvers and nondefault lookup
    /// strategies finish their batch DNS lookup before any connection starts.
    ///
    /// Default is `None`.
    #[inline]
    pub fn set_connect_timeout(&mut self, dur: Option<Duration>) {
        self.config_mut().connect_timeout = dur;
    }

    /// Set timeout for [RFC 6555 (Happy Eyeballs)][RFC 6555] algorithm.
    ///
    /// If hostname resolves to both IPv4 and IPv6 addresses and connection
    /// cannot be established using preferred address family before timeout
    /// elapses, then connector will in parallel attempt connection using other
    /// address family.
    ///
    /// Each round measures the delay from its first preferred-family attempt.
    /// Exhausting that family starts the alternate immediately. Addresses that
    /// arrive before the delay expires wait for its original deadline.
    ///
    /// If `None`, parallel connection attempts are disabled.
    ///
    /// Default is `None`.
    ///
    /// [RFC 6555]: https://tools.ietf.org/html/rfc6555
    #[inline]
    pub fn set_happy_eyeballs_timeout(&mut self, dur: Option<Duration>) {
        self.config_mut().happy_eyeballs_timeout = dur;
    }

    /// Set that all socket have `SO_REUSEADDR` set to the supplied value `reuse_address`.
    ///
    /// Default is `false`.
    #[inline]
    pub fn set_reuse_address(&mut self, reuse_address: bool) -> &mut Self {
        self.config_mut().reuse_address = reuse_address;
        self
    }

    /// Set the maximum number of connection retries before failing.
    ///
    /// Retries made while AAAA is pending count toward this shared limit. Newly
    /// returned addresses get an initial attempt, but only the remaining retries.
    /// DNS completion without new eligible addresses preserves a running retry
    /// interval. Useful late addresses extend the current round; if they fail,
    /// the next retry starts after a fresh interval.
    ///
    /// The default is no retries.
    #[inline]
    pub fn set_connect_retries_max(&mut self, max: Option<u32>) -> &mut Self {
        self.config_mut().connect_retries_max = max;
        self
    }

    /// Set the connection retry interval, which is more than
    /// likely going to be a Duration::from_secs(<u64>).
    ///
    /// The default is no interval.
    #[inline]
    pub fn set_connect_retries_interval(&mut self, interval: Option<Duration>) -> &mut Self {
        self.config_mut().connect_retries_interval = interval;
        self
    }

    pub fn set_metrics(&mut self, metrics: ConnectorMetrics) {
        self.metrics = metrics;
    }

    /// Set the socks5 proxy to use for connections
    ///
    /// Default is `None`.
    #[inline]
    pub fn set_socks5_proxy(&mut self, socks5_proxy: Option<String>) -> &mut Self {
        self.config_mut().socks5_proxy = socks5_proxy;
        self
    }

    /// Sets the value for the `SO_BINDTODEVICE` option on this socket.
    ///
    /// If a socket is bound to an interface, only packets received from that particular
    /// interface are processed by the socket. Note that this only works for some socket
    /// types, particularly `AF_INET` sockets.
    ///
    /// On Linux it can be used to specify a [VRF], but the binary needs
    /// to either have `CAP_NET_RAW` or to be run as root.
    ///
    /// This function is only available on Android、Fuchsia and Linux.
    ///
    /// [VRF]: https://www.kernel.org/doc/Documentation/networking/vrf.txt
    #[cfg(target_os = "linux")]
    #[inline]
    pub fn set_interface<S: Into<String>>(&mut self, interface: S) -> &mut Self {
        self.config_mut().interface = Some(interface.into());
        self
    }

    #[cfg(not(target_os = "linux"))]
    pub fn set_interface<S: Into<String>>(&mut self, _interface: S) -> &mut Self {
        self
    }

    // private

    fn config_mut(&mut self) -> &mut Config {
        // If the are HttpConnector clones, this will clone the inner
        // config. So mutating the config won't ever affect previous
        // clones.
        Arc::make_mut(&mut self.config)
    }
}

static INVALID_NOT_HTTP: &str = "invalid URL, scheme is not http";
static INVALID_MISSING_SCHEME: &str = "invalid URL, scheme is missing";
static INVALID_MISSING_HOST: &str = "invalid URL, host is missing";

impl fmt::Debug for ForgeHttpConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpConnector").finish()
    }
}

impl tower_service::Service<Uri> for ForgeHttpConnector {
    type Response = TokioIo<TcpStream>;
    type Error = ConnectError;
    type Future = Pin<Box<dyn Future<Output = ConnectResult> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let mut self_ = self.clone();
        Box::pin(async move {
            let stream = self_.call_async(uri).await?;
            Ok(TokioIo::new(stream))
        })
    }
}

fn get_host_port<'u>(config: &Config, dst: &'u Uri) -> Result<(&'u str, u16), ConnectError> {
    trace!(
        scheme = ?dst.scheme(),
        host = ?dst.host(),
        port = ?dst.port(),
        "Http::connect"
    );

    if config.enforce_http {
        if dst.scheme() != Some(&Scheme::HTTP) {
            return Err(ConnectError {
                msg: INVALID_NOT_HTTP.into(),
                cause: None,
            });
        }
    } else if dst.scheme().is_none() {
        return Err(ConnectError {
            msg: INVALID_MISSING_SCHEME.into(),
            cause: None,
        });
    }

    let Some(host) = dst.host() else {
        return Err(ConnectError {
            msg: INVALID_MISSING_HOST.into(),
            cause: None,
        });
    };
    let port = match dst.port() {
        Some(port) => port.as_u16(),
        None => {
            if dst.scheme() == Some(&Scheme::HTTPS) {
                443
            } else {
                80
            }
        }
    };

    Ok((host, port))
}

impl From<Vec<std::net::SocketAddr>> for resolver::SocketAddrs {
    fn from(addrs: Vec<std::net::SocketAddr>) -> Self {
        resolver::SocketAddrs::new(addrs)
    }
}

/// Keep resolved addresses and consumed retries with the first connection result
/// so connection rounds made while DNS is pending do not restart the retry allowance.
struct ResolvedConnection {
    addrs: resolver::SocketAddrs,
    result: Result<TcpStream, ConnectError>,
    retries_used: u32,
}

/// Own a live socket independently of the scheduler's DNS and timer polls.
/// Adding addresses can shorten its deadline, but must not recreate the socket.
struct PendingTcpAttempt<'a> {
    connection: Pin<Box<dyn Future<Output = Result<TcpStream, ConnectError>> + Send + 'a>>,
    deadline: Option<TokioInstant>,
    started: Instant,
    metric_addr: SocketAddr,
}

/// Own one sequential queue or one actual IP family's racing queue. DNS query
/// buckets may contain either family, so they cannot serve as racing lanes.
struct ConnectingTcpLane<'a> {
    addrs: Vec<SocketAddr>,
    next: usize,
    pending: Option<PendingTcpAttempt<'a>>,
    budget_started_at: Option<TokioInstant>,
    metrics: ConnectorMetrics,
}

impl<'a> ConnectingTcpLane<'a> {
    /// Create the queue without opening its first socket yet.
    fn new(addrs: resolver::SocketAddrs, metrics: ConnectorMetrics) -> Self {
        Self {
            addrs: addrs.collect(),
            next: 0,
            pending: None,
            budget_started_at: None,
            metrics,
        }
    }

    /// An exhausted lane stays idle until DNS appends addresses or a retry starts.
    fn has_work(&self) -> bool {
        self.pending.is_some() || self.next < self.addrs.len()
    }

    /// Reserve time for newly arrived addresses without extending a live attempt.
    fn append(&mut self, addrs: Vec<SocketAddr>, timeout: Option<Duration>) {
        if addrs.is_empty() {
            return;
        }
        self.addrs.extend(addrs);
        let now = TokioInstant::now();
        self.budget_started_at = Some(now);
        if let Some(pending) = self.pending.as_mut() {
            let updated = connection_deadline(
                timeout,
                self.budget_started_at,
                now,
                self.addrs.len() - self.next + 1,
            );
            if let (Some(current), Some(updated)) = (pending.deadline, updated) {
                pending.deadline = Some(current.min(updated));
            }
        }
    }

    /// Poll one attempt, retaining it if DNS or the other family wins the select.
    async fn connect(
        &mut self,
        config: &'a Config,
        proxy_addr: Option<SocketAddr>,
    ) -> Result<TcpStream, ConnectError> {
        if self.pending.is_none() {
            let now = TokioInstant::now();
            if self.next == 0 && self.budget_started_at.is_some() {
                // DNS may fill an alternate queue before its Happy Eyeballs
                // delay expires. Its TCP budget starts only on admission.
                self.budget_started_at = Some(now);
            }
            let addr = self.addrs[self.next];
            self.next += 1;
            self.pending = Some(PendingTcpAttempt {
                connection: Box::pin(connect_address(addr, proxy_addr, config, None)),
                deadline: connection_deadline(
                    config.connect_timeout,
                    self.budget_started_at,
                    now,
                    if self.budget_started_at.is_some() {
                        self.addrs.len() - self.next + 1
                    } else {
                        self.addrs.len()
                    },
                ),
                started: Instant::now(),
                metric_addr: proxy_addr.unwrap_or(addr),
            });
        }
        let pending = self.pending.as_mut().expect("attempt started above");
        let result = match pending.deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, &mut pending.connection)
                .await
                .map_err(|error| {
                    ConnectError::new(
                        if proxy_addr.is_some() {
                            "Proxy connect timed out"
                        } else {
                            "tcp connect error"
                        },
                        io::Error::new(io::ErrorKind::TimedOut, error),
                    )
                })
                .and_then(|result| result),
            None => (&mut pending.connection).await,
        };
        match &result {
            Ok(_) => self
                .metrics
                .connect_success(pending.metric_addr, pending.started),
            Err(error) => self
                .metrics
                .connect_error(pending.metric_addr, pending.started, error),
        }
        self.pending = None;
        result
    }

    /// Only the coordinator restarts exhausted lanes, charging one shared retry.
    fn restart(&mut self) {
        self.next = 0;
        self.budget_started_at = None;
    }
}

impl ForgeHttpConnector {
    /// Connect ready addresses without waiting for the second DNS query.
    /// Pending DNS and every live socket stay owned by this connection.
    async fn connect_resolving(
        &self,
        primary: resolver::SocketAddrs,
        fallback: resolver::HickoryResolverFuture,
        port: u16,
    ) -> ResolvedConnection {
        let mut lane = ConnectingTcpLane::new(
            eligible_addresses(primary, &self.config),
            self.metrics.clone(),
        );
        let mut fallback = Some(fallback);
        let mut last_error = None;
        let config = &self.config;
        let mut retries_used = 0;
        let proxy_addr = match proxy_address(config) {
            Ok(proxy) => proxy,
            Err(error) => {
                return ResolvedConnection {
                    addrs: resolver::SocketAddrs::new(lane.addrs),
                    result: Err(error),
                    retries_used,
                };
            }
        };

        loop {
            if !lane.has_work() {
                if let Some(lookup) = fallback.as_mut() {
                    // Keep known addresses on their retry schedule while DNS
                    // is pending. Newly returned addresses still get an attempt.
                    let can_retry = !lane.addrs.is_empty()
                        && retries_used < config.connect_retries_max.unwrap_or(0);
                    let retry_delay =
                        tokio::time::sleep(config.connect_retries_interval.unwrap_or_default());
                    tokio::pin!(retry_delay);
                    let resolved = if can_retry {
                        tokio::select! {
                            biased;
                            resolved = lookup => Some(resolved),
                            () = &mut retry_delay => None,
                        }
                    } else {
                        Some(lookup.await)
                    };
                    let Some(resolved) = resolved else {
                        retries_used += 1;
                        lane.restart();
                        continue;
                    };
                    fallback = None;
                    match resolved
                        .map(|resolved| eligible_addresses(with_port(resolved, port), config))
                    {
                        Ok(resolved) if !resolved.is_empty() => {
                            lane.append(resolved.collect(), config.connect_timeout);
                        }
                        resolved => {
                            if let Err(error) = resolved {
                                tracing::debug!(%error, "pending DNS lookup failed");
                                if lane.addrs.is_empty() {
                                    last_error = Some(ConnectError::dns(error));
                                }
                            }
                            if can_retry {
                                // DNS added no work. Finish the original backoff
                                // before handing later retries to the caller.
                                retry_delay.await;
                                retries_used += 1;
                                lane.restart();
                            }
                        }
                    }
                    continue;
                }
                let error = last_error.unwrap_or_else(|| {
                    ConnectError::new(
                        "tcp connect error",
                        io::Error::new(io::ErrorKind::NotConnected, "network unreachable"),
                    )
                });
                return ResolvedConnection {
                    addrs: resolver::SocketAddrs::new(lane.addrs),
                    result: Err(error),
                    retries_used,
                };
            }

            tokio::select! {
                biased;
                result = lane.connect(config, proxy_addr) => {
                    match result {
                        Ok(tcp) => return ResolvedConnection {
                            addrs: resolver::SocketAddrs::new(lane.addrs),
                            result: Ok(tcp),
                            retries_used,
                        },
                        Err(error) => last_error = Some(error),
                    }
                }
                resolved = async {
                    match fallback.as_mut() {
                        Some(lookup) => lookup.await,
                        None => std::future::pending().await,
                    }
                } => {
                    fallback = None;
                    match resolved {
                        Ok(resolved) => lane.append(
                            eligible_addresses(with_port(resolved, port), config).collect(),
                            config.connect_timeout,
                        ),
                        Err(error) => tracing::debug!(%error, "pending DNS lookup failed"),
                    }
                }
            }
        }
    }

    /// Race actual IP families while DNS is pending, preserving each round's
    /// delay, live sockets, and one retry allowance across both queues.
    async fn connect_resolving_racing(
        &self,
        primary: resolver::SocketAddrs,
        fallback: resolver::HickoryResolverFuture,
        port: u16,
        delay: Duration,
    ) -> ResolvedConnection {
        let config = &self.config;
        // With only one eligible family there is nothing to race. Still poll
        // both lookups: either ANSWER can contain addresses of that family.
        if matches!(
            (config.local_address_ipv4, config.local_address_ipv6),
            (Some(_), None) | (None, Some(_))
        ) {
            return self.connect_resolving(primary, fallback, port).await;
        }
        let proxy_addr = match proxy_address(config) {
            Ok(proxy) => proxy,
            Err(error) => {
                return ResolvedConnection {
                    addrs: primary,
                    result: Err(error),
                    retries_used: 0,
                };
            }
        };
        let mut addrs = primary.clone().collect::<Vec<_>>();
        let (preferred, alternate) =
            primary.split_by_preference(config.local_address_ipv4, config.local_address_ipv6);
        let preferred_ipv4 = preferred.clone().next().is_none_or(|addr| addr.is_ipv4());
        let mut preferred = ConnectingTcpLane::new(preferred, self.metrics.clone());
        let mut alternate = ConnectingTcpLane::new(alternate, self.metrics.clone());
        let mut fallback = Some(fallback);
        let alternate_delay = tokio::time::sleep(delay);
        tokio::pin!(alternate_delay);
        let mut alternate_started = false;
        let mut retry_delay = None;
        let mut retries_used = 0;
        let mut last_error = None;

        loop {
            // Failure of the entire preferred queue releases the alternate
            // immediately; otherwise its delay remains tied to the first attempt.
            alternate_started |= !preferred.has_work() && alternate.has_work();
            if !preferred.has_work() && !alternate.has_work() {
                if fallback.is_none() && retry_delay.is_none() {
                    return ResolvedConnection {
                        addrs: resolver::SocketAddrs::new(addrs),
                        result: Err(last_error.unwrap_or_else(|| {
                            ConnectError::new(
                                "tcp connect error",
                                io::Error::new(io::ErrorKind::NotConnected, "network unreachable"),
                            )
                        })),
                        retries_used,
                    };
                }
                if retry_delay.is_none()
                    && !addrs.is_empty()
                    && retries_used < config.connect_retries_max.unwrap_or(0)
                {
                    retry_delay = Some(Box::pin(tokio::time::sleep(
                        config.connect_retries_interval.unwrap_or_default(),
                    )));
                }
            }

            tokio::select! {
                biased;
                result = preferred.connect(config, proxy_addr), if preferred.has_work() => {
                    match result {
                        Ok(tcp) => return ResolvedConnection {
                            addrs: resolver::SocketAddrs::new(addrs),
                            result: Ok(tcp),
                            retries_used,
                        },
                        Err(error) => last_error = Some(error),
                    }
                }
                result = alternate.connect(config, proxy_addr),
                    if alternate_started && alternate.has_work() => {
                    match result {
                        Ok(tcp) => return ResolvedConnection {
                            addrs: resolver::SocketAddrs::new(addrs),
                            result: Ok(tcp),
                            retries_used,
                        },
                        Err(error) => last_error = Some(error),
                    }
                }
                resolved = async {
                    match fallback.as_mut() {
                        Some(lookup) => lookup.await,
                        None => std::future::pending().await,
                    }
                } => {
                    fallback = None;
                    match resolved {
                        Ok(resolved) if !resolved.is_empty() => {
                            let resolved = with_port(resolved, port).collect::<Vec<_>>();
                            let (preferred_addrs, alternate_addrs) = resolved
                                .iter()
                                .copied()
                                .partition(|addr| addr.is_ipv4() == preferred_ipv4);
                            addrs.extend(resolved);
                            preferred.append(preferred_addrs, config.connect_timeout);
                            alternate.append(alternate_addrs, config.connect_timeout);
                            // These addresses have not had their first attempt.
                            // They do not consume a retry or wait for its backoff.
                            retry_delay = None;
                        }
                        resolved => {
                            if let Err(error) = resolved {
                                tracing::debug!(%error, "pending DNS lookup failed");
                                if addrs.is_empty() {
                                    last_error = Some(ConnectError::dns(error));
                                }
                            }
                            // An empty/error reply must not restart a backoff
                            // already running for the known addresses.
                        }
                    }
                }
                () = &mut alternate_delay, if !alternate_started => alternate_started = true,
                () = async {
                    match retry_delay.as_mut() {
                        Some(delay) => delay.await,
                        None => std::future::pending().await,
                    }
                } => {
                    retry_delay = None;
                    retries_used += 1;
                    preferred.restart();
                    alternate.restart();
                    alternate_started = false;
                    // Match the first attempt: `tokio::time::sleep` handles
                    // overflow, while adding the delay directly can panic.
                    alternate_delay.set(tokio::time::sleep(delay));
                }
            }
        }
    }

    async fn call_async(&mut self, dst: Uri) -> Result<TcpStream, ConnectError> {
        let config = &self.config;
        let (host, port) = get_host_port(config, &dst)?;
        let host = host.trim_start_matches('[').trim_end_matches(']');

        let mut retries_used = 0;
        let (addrs, mut first_result) =
            if let Some(addrs) = resolver::SocketAddrs::try_parse(host, port) {
                (addrs, None)
            } else {
                let dns_name: Name = host.parse().map_err(ConnectError::dns)?;
                let resolved = self
                    .resolver
                    .resolve_for_connect(dns_name)
                    .await
                    .map_err(ConnectError::dns)?;
                let primary = with_port(resolved.primary, port);
                match resolved.fallback {
                    Some(fallback) => {
                        trace!(destination_address = %dst, "establishing new tcp connection");
                        let completed = match config.happy_eyeballs_timeout {
                            Some(delay) => {
                                self.connect_resolving_racing(primary, fallback, port, delay)
                                    .await
                            }
                            None => self.connect_resolving(primary, fallback, port).await,
                        };
                        if completed.addrs.is_empty() {
                            // No eligible address means no connection to retry;
                            // preserve a lookup error for the bound family.
                            return completed.result;
                        }
                        retries_used = completed.retries_used;
                        (completed.addrs, Some(completed.result))
                    }
                    None => (primary, None),
                }
            };

        let addrs = eligible_addresses(addrs, config);
        if addrs.is_empty() {
            // An empty queue has no TCP attempt to retry. Keep proxy validation
            // and the batch connection error without spending retry backoff.
            return ConnectingTcp::new(addrs, config, self.metrics.clone())
                .connect()
                .await;
        }
        let remaining_retries = config.connect_retries_max.unwrap_or(0) - retries_used;
        let retry_config = RetryFutureConfig::new(remaining_retries).fixed_backoff(
            self.config
                .connect_retries_interval
                .unwrap_or(Duration::from_secs(0)),
        );

        tryhard::retry_fn(|| {
            // The incremental path already made the first round of attempts.
            // Feed that result into the same retry policy, without connecting twice.
            let first_result = first_result.take();
            let addrs = &addrs;
            let metrics = self.metrics.clone();
            let dst = &dst;
            async move {
                match first_result {
                    Some(result) => result,
                    None => {
                        trace!(destination_address = %dst, "establishing new tcp connection");
                        ConnectingTcp::new(addrs.clone(), config, metrics)
                            .connect()
                            .await
                    }
                }
            }
        })
        .with_config(retry_config)
        .await
    }
}

/// DNS supplies addresses without ports; use the request's port for either family.
fn with_port(addrs: resolver::SocketAddrs, port: u16) -> resolver::SocketAddrs {
    resolver::SocketAddrs::new(
        addrs
            .map(|mut addr| {
                addr.set_port(port);
                addr
            })
            .collect(),
    )
}

/// A sole source family restricts direct targets and Happy Eyeballs queues.
/// Sequential SOCKS targets remain unrestricted because the bind is on the proxy.
fn eligible_addresses(addrs: resolver::SocketAddrs, config: &Config) -> resolver::SocketAddrs {
    if (config.happy_eyeballs_timeout.is_some() || config.socks5_proxy.is_none())
        && matches!(
            (config.local_address_ipv4, config.local_address_ipv6),
            (Some(_), None) | (None, Some(_))
        )
    {
        addrs
            .split_by_preference(config.local_address_ipv4, config.local_address_ipv6)
            .0
    } else {
        addrs
    }
}

/// Divide the TCP time remaining among the current and later attempts. Before
/// DNS supplies more eligible addresses, divide the original timeout among the
/// initial addresses. Afterwards, remaining attempts share one timeout from
/// that answer, or from admission if the queue has not started yet.
fn connection_deadline(
    timeout: Option<Duration>,
    budget_started_at: Option<TokioInstant>,
    now: TokioInstant,
    remaining_addresses: usize,
) -> Option<TokioInstant> {
    timeout.and_then(|timeout| {
        let remaining = budget_started_at
            .and_then(|finished| finished.checked_add(timeout))
            .map(|deadline| deadline.saturating_duration_since(now))
            .unwrap_or(timeout);
        now.checked_add(remaining / remaining_addresses as u32)
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use carbide_test_support::{Check, check_values};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    use super::*;

    /// Setup failure counts as an attempt in both modes; sequential mode must
    /// also continue through the same address list to reach IPv6.
    #[tokio::test]
    async fn ipv4_bind_failure_still_tries_ipv6() {
        for (scenario, happy_eyeballs_timeout) in [
            ("sequential", None),
            ("Happy Eyeballs", Some(Duration::from_millis(50))),
        ] {
            tokio::time::timeout(Duration::from_secs(5), async {
                let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
                    .await
                    .expect("bind IPv6 fixture");
                let ipv6 = listener.local_addr().expect("IPv6 fixture address");
                let ipv4 = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), ipv6.port());
                let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
                connector.set_local_addresses(Ipv4Addr::new(192, 0, 2, 254), Ipv6Addr::LOCALHOST);
                connector.set_connect_timeout(Some(Duration::from_secs(2)));
                connector.set_happy_eyeballs_timeout(happy_eyeballs_timeout);
                let mut metrics = connector.metrics.clone();

                // Prove setup itself fails before accepting IPv6 recovery as
                // evidence; a later connection refusal would exercise another path.
                let setup_error = match connect(&ipv4, &connector.config, None) {
                    Err(error) => error,
                    Ok(_) => panic!("{scenario}: fixture must fail during socket setup"),
                };
                assert_eq!(
                    setup_error.msg.as_ref(),
                    "tcp bind local error",
                    "{scenario}"
                );

                let client = ConnectingTcp::new(
                    vec![ipv4, ipv6].into(),
                    &connector.config,
                    connector.metrics.clone(),
                )
                .connect()
                .await
                .expect(scenario);
                assert_eq!(
                    client.peer_addr().expect("connected address"),
                    ipv6,
                    "{scenario}"
                );
                let (server, _) = listener.accept().await.expect("accept IPv6 connection");
                assert_eq!(
                    server.peer_addr().expect("accepted client address").ip(),
                    IpAddr::V6(Ipv6Addr::LOCALHOST),
                    "{scenario}"
                );
                assert_eq!(metrics.get_total_attempts(), 2, "{scenario}");
                assert_eq!(metrics.get_total_errors(), 1, "{scenario}");
                assert_eq!(metrics.get_total_successes(), 1, "{scenario}");
                assert_eq!(metrics.get_errors_for_addr(&ipv4), Some(1), "{scenario}");
                assert_eq!(metrics.get_successes_for_addr(&ipv6), Some(1), "{scenario}");
            })
            .await
            .unwrap_or_else(|_| panic!("{scenario}: connection timed out"));
        }
    }

    /// A reachable IPv6 peer must not bypass the caller's IPv4-only source bind.
    #[tokio::test]
    async fn sequential_bind_failure_does_not_escape_the_source_family() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
                .await
                .expect("bind IPv6 fixture");
            let ipv6 = listener.local_addr().expect("IPv6 fixture address");
            let ipv4 = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), ipv6.port());
            let config = Config {
                local_address_ipv4: Some(Ipv4Addr::new(192, 0, 2, 254)),
                connect_timeout: Some(Duration::from_secs(2)),
                ..Config::default()
            };
            let mut metrics = ConnectorMetrics::default();

            // Prove IPv6 is reachable so filtering, not a network failure,
            // accounts for rejecting the opposite-family address below.
            let reachable = TcpStream::connect(ipv6).await.expect("reach IPv6 fixture");
            let (accepted, _) = listener.accept().await.expect("accept IPv6 control");
            drop((reachable, accepted));
            let setup_error = match connect(&ipv4, &config, None) {
                Err(error) => error,
                Ok(_) => panic!("fixture must fail during IPv4 socket setup"),
            };
            assert_eq!(setup_error.msg.as_ref(), "tcp bind local error");

            let error = ConnectingTcp::new(vec![ipv4, ipv6].into(), &config, metrics.clone())
                .connect()
                .await
                .expect_err("IPv6 is ineligible with only an IPv4 source configured");
            assert_eq!(error.msg.as_ref(), "tcp bind local error");
            assert_eq!(metrics.get_total_attempts(), 1);
            assert_eq!(metrics.get_errors_for_addr(&ipv4), Some(1));
            assert_eq!(metrics.get_attempts_for_addr(&ipv6), None);
        })
        .await
        .expect("source-family restriction test timed out");
    }

    /// Awaited TCP refusal must preserve its source when exhausted and allow
    /// the next address in the same family when one remains.
    #[tokio::test]
    async fn tcp_refusal_preserves_the_error_and_tries_a_later_address() {
        for (scenario, happy_eyeballs_timeout) in [
            ("sequential", None),
            ("Happy Eyeballs", Some(Duration::from_millis(50))),
        ] {
            tokio::time::timeout(Duration::from_secs(5), async {
                // Keep this port bound but not listening. Releasing a listener
                // would let another process claim the port before we connect.
                let refusing_socket = TcpSocket::new_v4().expect("create refusing socket");
                refusing_socket
                    .bind((Ipv4Addr::LOCALHOST, 0).into())
                    .expect("bind refusing socket");
                let refused = refusing_socket.local_addr().expect("refusing address");
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .await
                    .expect("bind accepting socket");
                let accepting = listener.local_addr().expect("accepting address");
                let config = Config {
                    happy_eyeballs_timeout,
                    connect_timeout: Some(Duration::from_secs(2)),
                    ..Config::default()
                };
                let error =
                    ConnectingTcp::new(vec![refused].into(), &config, ConnectorMetrics::default())
                        .connect()
                        .await
                        .expect_err("the only address refuses TCP connections");
                assert_eq!(error.msg.as_ref(), "tcp connect error", "{scenario}");
                assert_eq!(
                    error
                        .source()
                        .and_then(|source| source.downcast_ref::<io::Error>())
                        .map(io::Error::kind),
                    Some(io::ErrorKind::ConnectionRefused),
                    "{scenario}"
                );

                let mut metrics = ConnectorMetrics::default();
                let client =
                    ConnectingTcp::new(vec![refused, accepting].into(), &config, metrics.clone())
                        .connect()
                        .await
                        .expect(scenario);
                assert_eq!(client.peer_addr().expect("connected address"), accepting);
                let (_server, _) = listener.accept().await.expect("accept later connection");
                assert_eq!(metrics.get_total_attempts(), 2, "{scenario}");
                assert_eq!(metrics.get_total_errors(), 1, "{scenario}");
                assert_eq!(metrics.get_total_successes(), 1, "{scenario}");
                assert_eq!(metrics.get_errors_for_addr(&refused), Some(1), "{scenario}");
                assert_eq!(
                    metrics.get_successes_for_addr(&accepting),
                    Some(1),
                    "{scenario}"
                );
            })
            .await
            .unwrap_or_else(|_| panic!("{scenario}: refusal recovery timed out"));
        }
    }

    /// SOCKS target rejection advances in target order, while TCP metrics stay
    /// attributed to the proxy address rather than either target.
    #[tokio::test]
    async fn socks_rejection_tries_the_next_target_with_proxy_metrics() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind SOCKS fixture");
            let proxy = listener.local_addr().expect("SOCKS fixture address");
            let targets = [
                std::net::SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 443),
                std::net::SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 2), 443),
            ];
            let config = Config {
                socks5_proxy: Some(proxy.to_string()),
                connect_timeout: Some(Duration::from_secs(2)),
                ..Config::default()
            };
            let mut metrics = ConnectorMetrics::default();
            let serve_proxy = async {
                // The fixture implements only no-auth SOCKS5 IPv4 CONNECT.
                // Reply 5 rejects the first target; reply 0 accepts the second.
                for (target, reply) in targets.iter().zip([5, 0]) {
                    let (mut stream, _) = listener.accept().await.expect("accept proxy connection");
                    let mut greeting = [0; 3];
                    stream
                        .read_exact(&mut greeting)
                        .await
                        .expect("SOCKS greeting");
                    assert_eq!(greeting, [5, 1, 0]);
                    stream
                        .write_all(&[5, 0])
                        .await
                        .expect("select no authentication");

                    let mut request = [0; 10];
                    stream
                        .read_exact(&mut request)
                        .await
                        .expect("SOCKS CONNECT");
                    assert_eq!(&request[..4], &[5, 1, 0, 1]);
                    assert_eq!(&request[4..8], &target.ip().octets());
                    assert_eq!(&request[8..], &target.port().to_be_bytes());
                    stream
                        .write_all(&[5, reply, 0, 1, 127, 0, 0, 1, 0, 0])
                        .await
                        .expect("reply to SOCKS CONNECT");
                    if reply == 0 {
                        stream
                            .write_all(b"proxy")
                            .await
                            .expect("write proxy payload");
                    }
                }
            };
            let addresses = targets.into_iter().map(SocketAddr::V4).collect::<Vec<_>>();
            let connect = ConnectingTcp::new(addresses.into(), &config, metrics.clone()).connect();
            let ((), client) = tokio::join!(serve_proxy, connect);
            let mut client = client.expect("second SOCKS target succeeds");
            assert_eq!(client.peer_addr().expect("proxy peer address"), proxy);
            let mut payload = [0; 5];
            client
                .read_exact(&mut payload)
                .await
                .expect("read proxy payload");
            assert_eq!(&payload, b"proxy");
            assert_eq!(metrics.get_total_attempts(), 2);
            assert_eq!(metrics.get_total_errors(), 1);
            assert_eq!(metrics.get_total_successes(), 1);
            assert_eq!(metrics.get_attempts_for_addr(&proxy), Some(2));
            assert_eq!(metrics.get_errors_for_addr(&proxy), Some(1));
            assert_eq!(metrics.get_successes_for_addr(&proxy), Some(1));
            for target in targets {
                assert_eq!(metrics.get_attempts_for_addr(&SocketAddr::V4(target)), None);
            }
        })
        .await
        .expect("SOCKS target recovery timed out");
    }

    /// Family filtering can leave no destinations even with a finite timeout;
    /// this must report absence without creating or recording a socket attempt.
    #[tokio::test]
    async fn empty_filtered_addresses_return_an_error_without_an_attempt() {
        let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
        connector.set_local_address(Some(Ipv4Addr::LOCALHOST.into()));
        connector.set_happy_eyeballs_timeout(Some(Duration::from_millis(50)));
        connector.set_connect_timeout(Some(Duration::from_secs(2)));
        let mut metrics = connector.metrics.clone();
        let addresses = vec!["[::1]:443".parse().expect("IPv6 fixture")].into();
        let error = ConnectingTcp::new(addresses, &connector.config, connector.metrics.clone())
            .connect()
            .await
            .expect_err("no address matches the local IPv4 binding");
        assert_eq!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<io::Error>())
                .map(io::Error::kind),
            Some(io::ErrorKind::NotConnected)
        );
        assert_eq!(metrics.get_total_attempts(), 0);
    }

    /// Proxy parsing precedes address exhaustion, including when no destination
    /// survives filtering, so callers retain the configuration error.
    #[tokio::test]
    async fn invalid_proxy_fails_without_attempting_addresses() {
        for (scenario, addresses, local_address_ipv4, happy_eyeballs_timeout) in [
            (
                "nonempty addresses",
                vec![
                    "127.0.0.1:443".parse().expect("IPv4 fixture"),
                    "[::1]:443".parse().expect("IPv6 fixture"),
                ],
                None,
                None,
            ),
            (
                "empty filtered addresses",
                vec!["[::1]:443".parse().expect("IPv6 fixture")],
                Some(Ipv4Addr::LOCALHOST),
                Some(Duration::from_millis(50)),
            ),
        ] {
            let config = Config {
                socks5_proxy: Some("invalid-proxy".to_string()),
                local_address_ipv4,
                happy_eyeballs_timeout,
                ..Config::default()
            };
            let mut metrics = ConnectorMetrics::default();
            let connection =
                ConnectingTcp::new(addresses.into(), &config, metrics.clone()).connect();
            tokio::pin!(connection);
            let Poll::Ready(result) = futures_util::poll!(connection) else {
                panic!("{scenario}: invalid proxy must fail before connecting");
            };
            let error = result.expect_err("malformed proxy must be rejected");
            assert_eq!(error.msg.as_ref(), "invalid proxy setting", "{scenario}");
            assert!(
                error
                    .source()
                    .expect("proxy parse error")
                    .is::<std::net::AddrParseError>(),
                "{scenario}"
            );
            assert_eq!(metrics.get_total_attempts(), 0, "{scenario}");
        }
    }

    /// DNS completion starts one TCP budget, divided among addresses that still
    /// need an attempt. Time spent waiting for DNS does not erase late IPv6.
    #[test]
    fn pending_addresses_share_the_post_dns_budget() {
        let start = TokioInstant::now();
        let budget = Duration::from_secs(6);
        let cases = [
            ("initial A share", None, start, 2, Some(start + budget / 2)),
            (
                "current address and newly returned AAAA",
                Some(start),
                start,
                2,
                Some(start + budget / 2),
            ),
            (
                "later attempts share only remaining time",
                Some(start),
                start + Duration::from_secs(2),
                2,
                Some(start + Duration::from_secs(4)),
            ),
            (
                "exhausted phase has no extra budget",
                Some(start),
                start + Duration::from_secs(7),
                1,
                Some(start + Duration::from_secs(7)),
            ),
        ];
        check_values(
            cases
                .into_iter()
                .map(|(scenario, finished, now, addresses, expect)| Check {
                    scenario,
                    input: (finished, now, addresses),
                    expect,
                }),
            |(finished, now, addresses)| {
                connection_deadline(Some(budget), finished, now, addresses)
            },
        );
        assert_eq!(connection_deadline(None, Some(start), start, 1), None);
        for budget_started_at in [None, Some(start)] {
            assert_eq!(
                connection_deadline(Some(Duration::MAX), budget_started_at, start, 1),
                None,
                "an unrepresentable deadline must not panic"
            );
        }
    }

    /// `await_io` keeps the test runnable while sockets wait for the OS.
    /// Otherwise Tokio's paused clock can jump to a connection deadline before
    /// loopback I/O runs. The wall-clock bound catches a stuck fixture too.
    async fn await_io<T>(operation: impl Future<Output = T>) -> T {
        tokio::pin!(operation);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Poll::Ready(result) = futures_util::poll!(&mut operation) {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "fixture I/O exceeded five seconds"
            );
            tokio::task::yield_now().await;
        }
    }

    /// `while_connecting` drives the connection alongside each fixture step.
    /// A connection failure must not leave the fixture waiting for socket I/O.
    async fn while_connecting<T>(
        connection: Pin<&mut impl Future<Output = ResolvedConnection>>,
        operation: impl Future<Output = T>,
    ) -> T {
        await_io(async {
            tokio::select! {
                biased;
                result = connection => panic!("connection ended before fixture step: {:?}", result.result),
                result = operation => result,
            }
        })
        .await
    }

    /// `accept_socks_target` observes the requested address before replying.
    /// Tests can then hold that handshake open or allow the connection through.
    async fn accept_socks_target(proxy: &TcpListener, expected: SocketAddr) -> TcpStream {
        let (mut socket, _) = proxy.accept().await.expect("accept SOCKS client");
        let mut greeting = [0; 3];
        socket
            .read_exact(&mut greeting)
            .await
            .expect("SOCKS greeting");
        assert_eq!(greeting, [5, 1, 0]);
        socket
            .write_all(&[5, 0])
            .await
            .expect("select no authentication");
        let mut request = [0; 4];
        socket
            .read_exact(&mut request)
            .await
            .expect("SOCKS request");
        assert_eq!(&request[..3], &[5, 1, 0]);
        let ip = match request[3] {
            1 => {
                let mut octets = [0; 4];
                socket.read_exact(&mut octets).await.expect("IPv4 target");
                IpAddr::V4(Ipv4Addr::from(octets))
            }
            4 => {
                let mut octets = [0; 16];
                socket.read_exact(&mut octets).await.expect("IPv6 target");
                IpAddr::V6(Ipv6Addr::from(octets))
            }
            other => panic!("unexpected SOCKS address type {other}"),
        };
        let port = socket.read_u16().await.expect("SOCKS target port");
        assert_eq!(SocketAddr::new(ip, port), expected);
        socket
    }

    /// `accept_socks_connection` completes a handshake held by the fixture.
    async fn accept_socks_connection(socket: &mut TcpStream) {
        await_io(socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1]))
            .await
            .expect("accept SOCKS connection");
    }

    /// `assert_no_queued_connection` checks the accept queue with a later probe.
    /// A single pending Tokio accept could just mean reactor readiness has not
    /// arrived yet. Keep the production future suspended while the probe runs.
    async fn assert_no_queued_connection(listener: &TcpListener) {
        let address = listener.local_addr().expect("listener probe address");
        let probe = await_io(TcpStream::connect(address))
            .await
            .expect("connect listener probe");
        let (_socket, peer) = await_io(listener.accept())
            .await
            .expect("accept listener probe");
        assert_eq!(
            peer,
            probe.local_addr().expect("probe client address"),
            "another connection preceded the probe"
        );
    }

    /// Absolute checkpoints keep DNS and connection timers relative to one start.
    async fn advance_to(deadline: TokioInstant) {
        tokio::time::advance(deadline.saturating_duration_since(TokioInstant::now())).await;
    }

    /// A real stalled SOCKS handshake pins the IPv4 timeout before and after
    /// AAAA addresses arrive. IPv6 must retain its share of one TCP budget.
    #[tokio::test(start_paused = true)]
    async fn pending_ipv6_preserves_connection_deadlines() {
        struct Case {
            scenario: &'static str,
            dns_at: u64,
            ipv4_deadline: u64,
            complete_ipv6: bool,
        }
        for case in [
            Case {
                scenario: "early AAAA shortens IPv4",
                dns_at: 2,
                ipv4_deadline: 5,
                complete_ipv6: true,
            },
            Case {
                scenario: "late AAAA does not extend IPv4",
                dns_at: 5,
                ipv4_deadline: 6,
                complete_ipv6: true,
            },
            Case {
                scenario: "AAAA after IPv4 timeout still gets an attempt",
                dns_at: 9,
                ipv4_deadline: 6,
                complete_ipv6: true,
            },
            Case {
                scenario: "stalled families exhaust one post-DNS budget",
                dns_at: 2,
                ipv4_deadline: 5,
                complete_ipv6: false,
            },
        ] {
            let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind SOCKS fixture");
            let proxy_addr = proxy.local_addr().expect("SOCKS fixture address");
            let ipv4 = "192.0.2.1:443".parse().expect("IPv4 fixture");
            let ipv6 = "[2001:db8::1]:443".parse().expect("IPv6 fixture");
            let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
            connector.set_socks5_proxy(Some(proxy_addr.to_string()));
            connector.set_connect_timeout(Some(Duration::from_secs(6)));
            let mut metrics = connector.metrics.clone();
            let (answer, pending) = oneshot::channel();
            let fallback = Box::pin(async move { pending.await.expect("release AAAA answer") });
            let start = TokioInstant::now();
            let mut connection =
                Box::pin(connector.connect_resolving(vec![ipv4].into(), fallback, 443));
            let mut held_ipv4 =
                while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv4)).await;

            if case.dns_at > case.ipv4_deadline {
                advance_to(start + Duration::from_secs(case.ipv4_deadline)).await;
                assert!(
                    futures_util::poll!(&mut connection).is_pending(),
                    "{}",
                    case.scenario
                );
                assert_eq!(
                    metrics.get_errors_for_addr(&proxy_addr),
                    Some(1),
                    "{}",
                    case.scenario
                );
            }
            advance_to(start + Duration::from_secs(case.dns_at)).await;
            assert!(
                answer.send(Ok(vec![ipv6].into())).is_ok(),
                "connection dropped AAAA receiver"
            );
            assert!(
                futures_util::poll!(&mut connection).is_pending(),
                "{}",
                case.scenario
            );
            if case.dns_at < case.ipv4_deadline {
                advance_to(
                    start + Duration::from_secs(case.ipv4_deadline) - Duration::from_millis(1),
                )
                .await;
                assert!(
                    futures_util::poll!(&mut connection).is_pending(),
                    "{}",
                    case.scenario
                );
                assert_eq!(
                    metrics.get_errors_for_addr(&proxy_addr),
                    None,
                    "{}: IPv4 must still be live",
                    case.scenario
                );
                assert_no_queued_connection(&proxy).await;
                advance_to(start + Duration::from_secs(case.ipv4_deadline)).await;
            }
            let mut server =
                while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv6)).await;
            assert_eq!(
                metrics.get_errors_for_addr(&proxy_addr),
                Some(1),
                "{}",
                case.scenario
            );
            assert_eq!(
                await_io(held_ipv4.read(&mut [0]))
                    .await
                    .expect("read expired IPv4 socket"),
                0
            );

            // IPv6 needs nearly all the time left after IPv4. Resetting the
            // phase budget or letting IPv4 consume it changes this checkpoint.
            let phase_deadline = start + Duration::from_secs(case.dns_at + 6);
            advance_to(phase_deadline - Duration::from_millis(1)).await;
            assert!(
                futures_util::poll!(&mut connection).is_pending(),
                "{}: reserved IPv6 share",
                case.scenario
            );
            let completed = if case.complete_ipv6 {
                accept_socks_connection(&mut server).await;
                await_io(connection).await
            } else {
                advance_to(phase_deadline).await;
                await_io(connection).await
            };
            assert_eq!(completed.retries_used, 0, "{}", case.scenario);
            assert_eq!(
                completed.addrs.collect::<Vec<_>>(),
                [ipv4, ipv6],
                "{}: retain addresses for retries",
                case.scenario
            );
            if case.complete_ipv6 {
                let mut client = completed.result.expect(case.scenario);
                await_io(async {
                    client.write_all(b"ok").await.expect("write through proxy");
                    let mut received = [0; 2];
                    server
                        .read_exact(&mut received)
                        .await
                        .expect("read through proxy");
                    assert_eq!(&received, b"ok");
                })
                .await;
                assert_eq!(
                    metrics.get_successes_for_addr(&proxy_addr),
                    Some(1),
                    "{}",
                    case.scenario
                );
            } else {
                let error = completed.result.expect_err(case.scenario);
                assert_eq!(
                    error
                        .source()
                        .and_then(|source| source.downcast_ref::<io::Error>())
                        .map(io::Error::kind),
                    Some(io::ErrorKind::TimedOut)
                );
                assert_eq!(
                    metrics.get_errors_for_addr(&proxy_addr),
                    Some(2),
                    "{}",
                    case.scenario
                );
                assert_eq!(TokioInstant::now(), phase_deadline);
            }
            assert_eq!(metrics.get_total_attempts(), 2, "{}", case.scenario);
        }
    }

    /// Two stalled IPv4 addresses retain their shares after negative AAAA.
    /// Useful AAAA instead shares the remaining budget with the later IPv4 attempt.
    #[tokio::test(start_paused = true)]
    async fn multiple_ipv4_addresses_share_the_remaining_budget() {
        struct Case {
            scenario: &'static str,
            dns_result: Result<resolver::SocketAddrs, hickory_resolver::net::NetError>,
            dns_at: u64,
            racing: bool,
            primary_has_second: bool,
            first_ipv4_deadline: u64,
            second_ipv4_deadline: u64,
            ipv6_deadline: Option<u64>,
        }
        let first_ipv4 = "192.0.2.1:443".parse().expect("first IPv4 fixture");
        let second_ipv4 = "192.0.2.2:443".parse().expect("second IPv4 fixture");
        let ipv6 = "[2001:db8::1]:443".parse().expect("IPv6 fixture");
        for Case {
            scenario,
            dns_result,
            dns_at,
            racing,
            primary_has_second,
            first_ipv4_deadline,
            second_ipv4_deadline,
            ipv6_deadline,
        } in [
            Case {
                scenario: "empty AAAA",
                dns_result: Ok(Vec::new().into()),
                dns_at: 2,
                racing: false,
                primary_has_second: true,
                first_ipv4_deadline: 3,
                second_ipv4_deadline: 6,
                ipv6_deadline: None,
            },
            Case {
                scenario: "failed AAAA",
                dns_result: Err(hickory_resolver::net::NetError::from("AAAA lookup failed")),
                dns_at: 2,
                racing: false,
                primary_has_second: true,
                first_ipv4_deadline: 3,
                second_ipv4_deadline: 6,
                ipv6_deadline: None,
            },
            Case {
                scenario: "useful AAAA shares time with remaining IPv4",
                dns_result: Ok(vec![ipv6].into()),
                dns_at: 1,
                racing: false,
                primary_has_second: true,
                first_ipv4_deadline: 3,
                second_ipv4_deadline: 5,
                ipv6_deadline: Some(7),
            },
            Case {
                scenario: "racing late IPv4 shortens the live attempt",
                dns_result: Ok(vec![second_ipv4].into()),
                dns_at: 1,
                racing: true,
                primary_has_second: false,
                first_ipv4_deadline: 4,
                second_ipv4_deadline: 7,
                ipv6_deadline: None,
            },
            Case {
                scenario: "racing negative AAAA preserves IPv4 deadlines",
                dns_result: Err(hickory_resolver::net::NetError::from("AAAA lookup failed")),
                dns_at: 2,
                racing: true,
                primary_has_second: true,
                first_ipv4_deadline: 3,
                second_ipv4_deadline: 6,
                ipv6_deadline: None,
            },
        ] {
            let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind SOCKS fixture");
            let proxy_addr = proxy.local_addr().expect("SOCKS fixture address");
            let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
            connector.set_socks5_proxy(Some(proxy_addr.to_string()));
            connector.set_connect_timeout(Some(Duration::from_secs(6)));
            let mut metrics = connector.metrics.clone();
            let (answer, pending) = oneshot::channel();
            let fallback = Box::pin(async move { pending.await.expect("release AAAA result") });
            let start = TokioInstant::now();
            let primary = if primary_has_second {
                vec![first_ipv4, second_ipv4]
            } else {
                vec![first_ipv4]
            };
            let mut connection = Box::pin(async {
                if racing {
                    connector
                        .connect_resolving_racing(
                            primary.into(),
                            fallback,
                            443,
                            Duration::from_secs(1),
                        )
                        .await
                } else {
                    connector
                        .connect_resolving(primary.into(), fallback, 443)
                        .await
                }
            });
            let mut first =
                while_connecting(connection.as_mut(), accept_socks_target(&proxy, first_ipv4))
                    .await;

            advance_to(start + Duration::from_secs(dns_at)).await;
            assert!(answer.send(dns_result).is_ok(), "{scenario}");
            assert!(
                futures_util::poll!(&mut connection).is_pending(),
                "{scenario}"
            );
            advance_to(start + Duration::from_secs(first_ipv4_deadline) - Duration::from_millis(1))
                .await;
            assert!(
                futures_util::poll!(&mut connection).is_pending(),
                "{scenario}"
            );
            assert_eq!(metrics.get_total_errors(), 0, "{scenario}");
            assert_no_queued_connection(&proxy).await;

            advance_to(start + Duration::from_secs(first_ipv4_deadline)).await;
            let mut second = while_connecting(
                connection.as_mut(),
                accept_socks_target(&proxy, second_ipv4),
            )
            .await;
            assert_eq!(metrics.get_total_errors(), 1, "{scenario}");
            assert_eq!(
                await_io(first.read(&mut [0]))
                    .await
                    .expect("first socket EOF"),
                0
            );

            advance_to(
                start + Duration::from_secs(second_ipv4_deadline) - Duration::from_millis(1),
            )
            .await;
            assert!(
                futures_util::poll!(&mut connection).is_pending(),
                "{scenario}"
            );
            assert_eq!(metrics.get_total_errors(), 1, "{scenario}");
            assert_no_queued_connection(&proxy).await;
            advance_to(start + Duration::from_secs(second_ipv4_deadline)).await;
            let mut expected_addrs = vec![first_ipv4, second_ipv4];
            let mut last = if let Some(deadline) = ipv6_deadline {
                let third =
                    while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv6)).await;
                assert_eq!(metrics.get_total_errors(), 2, "{scenario}");
                assert_eq!(
                    await_io(second.read(&mut [0]))
                        .await
                        .expect("second socket EOF"),
                    0,
                );
                expected_addrs.push(ipv6);
                advance_to(start + Duration::from_secs(deadline) - Duration::from_millis(1)).await;
                assert!(
                    futures_util::poll!(&mut connection).is_pending(),
                    "{scenario}"
                );
                assert_eq!(metrics.get_total_errors(), 2, "{scenario}");
                advance_to(start + Duration::from_secs(deadline)).await;
                third
            } else {
                second
            };
            let completed = await_io(connection).await;
            let error = completed.result.expect_err(scenario);
            assert_eq!(error.msg.as_ref(), "Proxy connect timed out", "{scenario}");
            assert!(
                error
                    .source()
                    .and_then(|source| source.downcast_ref::<io::Error>())
                    .and_then(io::Error::get_ref)
                    .is_some_and(|source| source.is::<tokio::time::error::Elapsed>()),
                "{scenario}: preserve the timer failure cause",
            );
            assert_eq!(
                error
                    .source()
                    .and_then(|source| source.downcast_ref::<io::Error>())
                    .map(io::Error::kind),
                Some(io::ErrorKind::TimedOut),
                "{scenario}",
            );
            assert_eq!(
                TokioInstant::now(),
                start + Duration::from_secs(ipv6_deadline.unwrap_or(second_ipv4_deadline)),
                "{scenario}"
            );
            assert_eq!(
                completed.addrs.collect::<Vec<_>>(),
                expected_addrs,
                "{scenario}"
            );
            assert_eq!(completed.retries_used, 0, "{scenario}");
            assert_eq!(
                metrics.get_total_attempts(),
                expected_addrs.len() as u32,
                "{scenario}"
            );
            assert_eq!(
                metrics.get_total_errors(),
                expected_addrs.len() as u32,
                "{scenario}"
            );
            assert_eq!(
                await_io(last.read(&mut [0]))
                    .await
                    .expect("last socket EOF"),
                0
            );
        }
    }

    /// A failed AAAA lookup cannot cancel or replace an IPv4 handshake that can
    /// still succeed. Complete the original socket after observing the DNS error.
    #[tokio::test(start_paused = true)]
    async fn failed_aaaa_keeps_the_live_ipv4_connection() {
        for (scenario, racing) in [("sequential", false), ("Happy Eyeballs", true)] {
            let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind SOCKS fixture");
            let proxy_addr = proxy.local_addr().expect("SOCKS fixture address");
            let ipv4 = "192.0.2.1:443".parse().expect("IPv4 fixture");
            let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
            connector.set_socks5_proxy(Some(proxy_addr.to_string()));
            connector.set_connect_timeout(Some(Duration::from_secs(6)));
            let mut metrics = connector.metrics.clone();
            let (answer, pending) = oneshot::channel();
            let fallback = Box::pin(async move { pending.await.expect("release AAAA error") });
            let start = TokioInstant::now();
            let mut connection = Box::pin(async {
                if racing {
                    connector
                        .connect_resolving_racing(
                            vec![ipv4].into(),
                            fallback,
                            443,
                            Duration::from_secs(1),
                        )
                        .await
                } else {
                    connector
                        .connect_resolving(vec![ipv4].into(), fallback, 443)
                        .await
                }
            });
            let mut server =
                while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv4)).await;
            advance_to(start + Duration::from_secs(1)).await;
            assert!(
                answer
                    .send(Err(hickory_resolver::net::NetError::from(
                        "AAAA lookup failed"
                    )))
                    .is_ok()
            );
            assert!(
                futures_util::poll!(&mut connection).is_pending(),
                "{scenario}"
            );
            advance_to(start + Duration::from_secs(2)).await;
            assert!(
                futures_util::poll!(&mut connection).is_pending(),
                "{scenario}"
            );
            assert_no_queued_connection(&proxy).await;
            accept_socks_connection(&mut server).await;
            let completed = await_io(connection).await;
            let mut client = completed.result.expect(scenario);
            await_io(async {
                client
                    .write_all(b"ok")
                    .await
                    .expect("write through original socket");
                let mut received = [0; 2];
                server
                    .read_exact(&mut received)
                    .await
                    .expect("read original socket");
                assert_eq!(&received, b"ok", "{scenario}");
            })
            .await;
            assert_eq!(completed.addrs.collect::<Vec<_>>(), [ipv4], "{scenario}");
            assert_eq!(metrics.get_total_attempts(), 1, "{scenario}");
            assert_eq!(metrics.get_total_successes(), 1, "{scenario}");
            assert_eq!(metrics.get_total_errors(), 0, "{scenario}");
        }
    }

    /// `LookupState` distinguishes an active lookup from one never polled.
    #[derive(Default)]
    struct LookupState {
        polled: AtomicBool,
        dropped: AtomicBool,
    }

    /// `PendingLookup` never resolves, but exposes polling and cancellation.
    struct PendingLookup(Arc<LookupState>);

    impl Future for PendingLookup {
        type Output = Result<resolver::SocketAddrs, hickory_resolver::net::NetError>;

        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            self.0.polled.store(true, Ordering::SeqCst);
            Poll::Pending
        }
    }

    impl Drop for PendingLookup {
        fn drop(&mut self) {
            self.0.dropped.store(true, Ordering::SeqCst);
        }
    }

    /// The connector drops its polled lookup future when it succeeds or leaves.
    /// This does not assert cancellation of Hickory's shared DNS transport.
    #[tokio::test(start_paused = true)]
    async fn pending_lookup_is_dropped_with_its_connection() {
        for (scenario, complete_ipv4, racing) in [
            ("IPv4 succeeds", true, false),
            ("caller cancels sequential connection", false, false),
            ("caller cancels racing connection", false, true),
        ] {
            let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind SOCKS fixture");
            let proxy_addr = proxy.local_addr().expect("SOCKS fixture address");
            let ipv4 = "192.0.2.1:443".parse().expect("IPv4 fixture");
            let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
            connector.set_socks5_proxy(Some(proxy_addr.to_string()));
            let lookup = Arc::new(LookupState::default());
            let fallback = Box::pin(PendingLookup(Arc::clone(&lookup)));
            let mut connection = Box::pin(async {
                if racing {
                    connector
                        .connect_resolving_racing(
                            vec![ipv4].into(),
                            fallback,
                            443,
                            Duration::from_secs(1),
                        )
                        .await
                } else {
                    connector
                        .connect_resolving(vec![ipv4].into(), fallback, 443)
                        .await
                }
            });
            let mut server =
                while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv4)).await;
            assert!(
                lookup.polled.load(Ordering::SeqCst),
                "{scenario}: lookup must be active"
            );
            assert!(!lookup.dropped.load(Ordering::SeqCst), "{scenario}");
            if complete_ipv4 {
                accept_socks_connection(&mut server).await;
                let completed = await_io(connection).await;
                assert_eq!(
                    completed
                        .result
                        .expect(scenario)
                        .peer_addr()
                        .expect("proxy peer address"),
                    proxy_addr
                );
            } else {
                drop(connection);
                assert_eq!(
                    await_io(server.read(&mut [0]))
                        .await
                        .expect("read canceled connection"),
                    0
                );
            }
            assert!(
                lookup.dropped.load(Ordering::SeqCst),
                "{scenario}: owned lookup future must be dropped"
            );
        }
    }

    /// AAAA arriving during a live IPv4 handshake must not restart its socket
    /// or racing delay. Mixed answers must append to each actual family's queue.
    #[tokio::test(start_paused = true)]
    async fn happy_eyeballs_delay_starts_with_ipv4_not_aaaa() {
        struct Case {
            scenario: &'static str,
            mixed_answers: bool,
            timeout: u64,
            delay: u64,
            admitted_at: u64,
            complete_at: u64,
        }
        for Case {
            scenario,
            mixed_answers,
            timeout,
            delay,
            admitted_at,
            complete_at,
        } in [
            Case {
                scenario: "late IPv6",
                mixed_answers: false,
                timeout: 20,
                delay: 4,
                admitted_at: 4,
                complete_at: 4,
            },
            Case {
                scenario: "mixed ready and late answers",
                mixed_answers: true,
                timeout: 20,
                delay: 4,
                admitted_at: 4,
                complete_at: 4,
            },
            Case {
                scenario: "preferred timeout admits cold IPv6 with a fresh budget",
                mixed_answers: false,
                timeout: 6,
                delay: 10,
                admitted_at: 6,
                complete_at: 11,
            },
        ] {
            let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind SOCKS fixture");
            let proxy_addr = proxy.local_addr().expect("SOCKS fixture address");
            let ipv4: SocketAddr = "192.0.2.1:443".parse().expect("IPv4 fixture");
            let ipv6: SocketAddr = "[2001:db8::1]:443".parse().expect("IPv6 fixture");
            let late_ipv4 = "192.0.2.2:443".parse().expect("late IPv4 fixture");
            let late_ipv6 = "[2001:db8::2]:443".parse().expect("late IPv6 fixture");
            let primary = if mixed_answers {
                vec![ipv4, ipv6]
            } else {
                vec![ipv4]
            };
            let late = if mixed_answers {
                vec![late_ipv4, late_ipv6]
            } else {
                vec![ipv6]
            };
            let expected_addrs: Vec<_> = primary.iter().chain(&late).copied().collect();
            let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
            connector.set_socks5_proxy(Some(proxy_addr.to_string()));
            connector.set_connect_timeout(Some(Duration::from_secs(timeout)));
            let mut metrics = connector.metrics.clone();
            let (answer, pending) = oneshot::channel();
            let fallback = Box::pin(async move { pending.await.expect("release AAAA answer") });
            let start = TokioInstant::now();
            let mut connection = Box::pin(connector.connect_resolving_racing(
                primary.into(),
                fallback,
                443,
                Duration::from_secs(delay),
            ));
            let mut held_ipv4 =
                while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv4)).await;
            advance_to(start + Duration::from_secs(2)).await;
            assert!(
                answer.send(Ok(late.into())).is_ok(),
                "connection dropped AAAA receiver"
            );
            assert!(futures_util::poll!(&mut connection).is_pending());
            advance_to(start + Duration::from_secs(admitted_at) - Duration::from_millis(1)).await;
            assert!(futures_util::poll!(&mut connection).is_pending());
            assert_no_queued_connection(&proxy).await;
            advance_to(start + Duration::from_secs(admitted_at)).await;
            let mut server =
                while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv6)).await;
            assert_eq!(
                metrics.get_total_errors(),
                u32::from(admitted_at == timeout),
                "{scenario}: preferred timeout releases the alternate early"
            );
            if mixed_answers {
                // The next IPv6 attempt comes from the late mixed answer. Its
                // preceding IPv4 address belongs behind the still-live IPv4 socket.
                await_io(server.write_all(&[5, 5, 0, 1, 127, 0, 0, 1, 0, 1]))
                    .await
                    .expect("refuse initial IPv6");
                server =
                    while_connecting(connection.as_mut(), accept_socks_target(&proxy, late_ipv6))
                        .await;
            }
            advance_to(start + Duration::from_secs(complete_at)).await;
            assert!(
                futures_util::poll!(&mut connection).is_pending(),
                "{scenario}"
            );
            assert_eq!(
                metrics.get_total_errors(),
                u32::from(admitted_at == timeout) + u32::from(mixed_answers),
                "{scenario}: cold IPv6 keeps its budget until admission + timeout",
            );
            accept_socks_connection(&mut server).await;
            let completed = await_io(connection).await;
            assert_eq!(
                completed
                    .result
                    .expect("IPv6 race succeeds")
                    .peer_addr()
                    .expect("proxy peer address"),
                proxy_addr
            );
            assert_eq!(
                completed.addrs.collect::<Vec<_>>(),
                expected_addrs,
                "{scenario}"
            );
            assert_eq!(completed.retries_used, 0);
            assert_eq!(
                TokioInstant::now(),
                start + Duration::from_secs(complete_at)
            );
            assert_eq!(
                await_io(held_ipv4.read(&mut [0]))
                    .await
                    .expect("read canceled IPv4 socket"),
                0
            );
            assert_eq!(
                metrics.get_total_attempts(),
                1 + u32::from(mixed_answers) + u32::from(admitted_at == timeout)
            );
            assert_eq!(metrics.get_total_successes(), 1);
        }
    }

    /// A refused SOCKS attempt recovers on its IPv4 retry schedule. An empty or
    /// failed AAAA response during backoff must not restart that interval.
    /// An overflowing delay must not become a short delay on retry, and IPv4
    /// exhaustion must still admit IPv6 immediately.
    #[tokio::test(start_paused = true)]
    async fn ipv4_retries_keep_their_schedule_during_dns() {
        enum RetryConnection {
            Ipv4,
            DelayedIpv6,
            Ipv6AfterIpv4Failure,
        }
        for (scenario, delay, retry_connection, dns_result) in [
            (
                "sequential with pending AAAA",
                None,
                RetryConnection::Ipv4,
                None,
            ),
            (
                "Happy Eyeballs with pending AAAA",
                Some(Duration::from_secs(1)),
                RetryConnection::Ipv4,
                None,
            ),
            (
                "Happy Eyeballs resets the mixed-answer retry delay",
                Some(Duration::from_secs(1)),
                RetryConnection::DelayedIpv6,
                None,
            ),
            (
                "overflow delay retry admits IPv6 after IPv4 exhaustion",
                Some(Duration::MAX),
                RetryConnection::Ipv6AfterIpv4Failure,
                None,
            ),
            (
                "sequential with empty AAAA during backoff",
                None,
                RetryConnection::Ipv4,
                Some(Ok(Vec::new().into())),
            ),
            (
                "Happy Eyeballs with failed AAAA during backoff",
                Some(Duration::from_secs(1)),
                RetryConnection::Ipv4,
                Some(Err(hickory_resolver::net::NetError::from(
                    "AAAA lookup failed",
                ))),
            ),
        ] {
            let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind SOCKS fixture");
            let proxy_addr = proxy.local_addr().expect("SOCKS fixture address");
            let ipv4 = "192.0.2.1:443".parse().expect("IPv4 fixture");
            let ipv6 = "[2001:db8::1]:443".parse().expect("IPv6 fixture");
            let mixed_primary = !matches!(retry_connection, RetryConnection::Ipv4);
            let primary = if mixed_primary {
                vec![ipv4, ipv6]
            } else {
                vec![ipv4]
            };
            let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
            connector.set_socks5_proxy(Some(proxy_addr.to_string()));
            connector.set_connect_retries_max(Some(1));
            connector.set_connect_retries_interval(Some(Duration::from_secs(3)));
            let mut metrics = connector.metrics.clone();
            let lookup = Arc::new(LookupState::default());
            let (answer, pending) = oneshot::channel();
            let dns_completes = dns_result.is_some();
            let fallback: resolver::HickoryResolverFuture = if dns_completes {
                Box::pin(async move { pending.await.expect("release AAAA result") })
            } else {
                Box::pin(PendingLookup(Arc::clone(&lookup)))
            };
            let mut connection = Box::pin(async {
                if let Some(delay) = delay {
                    connector
                        .connect_resolving_racing(primary.clone().into(), fallback, 443, delay)
                        .await
                } else {
                    connector
                        .connect_resolving(primary.clone().into(), fallback, 443)
                        .await
                }
            });
            let mut first =
                while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv4)).await;
            await_io(first.write_all(&[5, 5, 0, 1, 127, 0, 0, 1, 0, 1]))
                .await
                .expect("refuse first SOCKS attempt");
            if mixed_primary {
                let mut alternate =
                    while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv6)).await;
                await_io(alternate.write_all(&[5, 5, 0, 1, 127, 0, 0, 1, 0, 1]))
                    .await
                    .expect("refuse first IPv6 attempt");
            }
            let initial_errors = 1 + u32::from(mixed_primary);
            while_connecting(connection.as_mut(), async {
                while metrics.get_total_errors() < initial_errors {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            let failed_at = TokioInstant::now();
            if let Some(result) = dns_result {
                advance_to(failed_at + Duration::from_secs(1)).await;
                assert!(answer.send(result).is_ok(), "{scenario}");
                assert!(
                    futures_util::poll!(&mut connection).is_pending(),
                    "{scenario}"
                );
            }
            advance_to(failed_at + Duration::from_secs(3) - Duration::from_millis(1)).await;
            assert!(
                futures_util::poll!(&mut connection).is_pending(),
                "{scenario}"
            );
            assert_no_queued_connection(&proxy).await;
            advance_to(failed_at + Duration::from_secs(3)).await;
            let mut second =
                while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv4)).await;
            if !dns_completes {
                assert!(lookup.polled.load(Ordering::SeqCst), "{scenario}");
                assert!(
                    !lookup.dropped.load(Ordering::SeqCst),
                    "{scenario}: AAAA still pending"
                );
            }
            let retry_ipv4_fails =
                matches!(retry_connection, RetryConnection::Ipv6AfterIpv4Failure);
            let completed_at = failed_at
                + Duration::from_secs(3)
                + match retry_connection {
                    RetryConnection::Ipv4 => Duration::ZERO,
                    RetryConnection::DelayedIpv6 => Duration::from_secs(1),
                    RetryConnection::Ipv6AfterIpv4Failure => Duration::from_secs(60),
                };
            if mixed_primary {
                if retry_ipv4_fails {
                    // Wait past the finite delay and retry interval so a short
                    // overflow fallback cannot pass by admitting IPv6 early.
                    advance_to(completed_at).await;
                    assert!(
                        futures_util::poll!(&mut connection).is_pending(),
                        "{scenario}"
                    );
                    assert_no_queued_connection(&proxy).await;
                    // IPv4 exhaustion must bypass the remaining delay.
                    await_io(second.write_all(&[5, 5, 0, 1, 127, 0, 0, 1, 0, 1]))
                        .await
                        .expect("refuse retried IPv4");
                } else {
                    advance_to(completed_at - Duration::from_millis(1)).await;
                    assert!(
                        futures_util::poll!(&mut connection).is_pending(),
                        "{scenario}"
                    );
                    assert_no_queued_connection(&proxy).await;
                    advance_to(completed_at).await;
                }
                let mut alternate =
                    while_connecting(connection.as_mut(), accept_socks_target(&proxy, ipv6)).await;
                accept_socks_connection(&mut alternate).await;
            } else {
                accept_socks_connection(&mut second).await;
            }
            let completed = await_io(connection).await;
            assert_eq!(
                completed
                    .result
                    .expect(scenario)
                    .peer_addr()
                    .expect("proxy peer address"),
                proxy_addr
            );
            assert_eq!(completed.retries_used, 1, "{scenario}");
            assert_eq!(TokioInstant::now(), completed_at, "{scenario}");
            assert_eq!(completed.addrs.collect::<Vec<_>>(), primary, "{scenario}");
            let expected_errors = initial_errors + u32::from(retry_ipv4_fails);
            assert_eq!(
                metrics.get_total_attempts(),
                expected_errors + 1,
                "{scenario}"
            );
            assert_eq!(metrics.get_total_errors(), expected_errors, "{scenario}");
            assert_eq!(metrics.get_total_successes(), 1, "{scenario}");
            // A refused retry is counted above; only the still-live losing
            // attempt needs this cancellation check.
            if matches!(retry_connection, RetryConnection::DelayedIpv6) {
                assert_eq!(
                    await_io(second.read(&mut [0]))
                        .await
                        .expect("losing retry socket EOF"),
                    0
                );
            }
            if !dns_completes {
                assert!(lookup.dropped.load(Ordering::SeqCst), "{scenario}");
            }
        }
    }

    /// An ineligible late answer adds no work and cannot restart IPv4 backoff.
    #[tokio::test(start_paused = true)]
    async fn ineligible_late_addresses_preserve_the_ipv4_retry_deadline() {
        let refusing = TcpSocket::new_v4().expect("create refusing socket");
        refusing
            .bind((Ipv4Addr::LOCALHOST, 0).into())
            .expect("reserve refusing address");
        let ipv4 = refusing.local_addr().expect("refusing address");
        let ipv6 = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), ipv4.port());
        let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
        connector.set_local_address(Some(Ipv4Addr::LOCALHOST.into()));
        connector.set_connect_retries_max(Some(1));
        connector.set_connect_retries_interval(Some(Duration::from_secs(3)));
        let mut metrics = connector.metrics.clone();
        let (answer, pending) = oneshot::channel();
        let fallback = Box::pin(async move { pending.await.expect("release ineligible answer") });
        let mut connection =
            Box::pin(connector.connect_resolving(vec![ipv4].into(), fallback, ipv4.port()));
        while_connecting(connection.as_mut(), async {
            while metrics.get_total_errors() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let failed_at = TokioInstant::now();
        advance_to(failed_at + Duration::from_secs(1)).await;
        assert!(answer.send(Ok(vec![ipv6].into())).is_ok());
        assert!(futures_util::poll!(&mut connection).is_pending());
        advance_to(failed_at + Duration::from_secs(3) - Duration::from_millis(1)).await;
        assert!(futures_util::poll!(&mut connection).is_pending());
        assert_eq!(metrics.get_attempts_for_addr(&ipv4), Some(1));
        assert_eq!(metrics.get_attempts_for_addr(&ipv6), None);

        advance_to(failed_at + Duration::from_secs(3)).await;
        let completed = await_io(connection).await;
        let error = completed.result.expect_err("IPv4 retry is refused");
        assert_eq!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<io::Error>())
                .map(io::Error::kind),
            Some(io::ErrorKind::ConnectionRefused)
        );
        assert_eq!(TokioInstant::now(), failed_at + Duration::from_secs(3));
        assert_eq!(completed.retries_used, 1);
        assert_eq!(completed.addrs.collect::<Vec<_>>(), [ipv4]);
        assert_eq!(metrics.get_total_attempts(), 2);
        assert_eq!(metrics.get_errors_for_addr(&ipv4), Some(2));
        assert_eq!(metrics.get_attempts_for_addr(&ipv6), None);
    }

    /// Sequential SOCKS binds the proxy socket, not the target's family. Both
    /// ready and late IPv6 targets remain eligible through an IPv4-bound proxy.
    #[tokio::test(start_paused = true)]
    async fn sequential_socks_keeps_opposite_family_targets_with_a_source_bind() {
        let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind SOCKS fixture");
        let proxy_addr = proxy.local_addr().expect("SOCKS fixture address");
        let ready = "[2001:db8::1]:443".parse().expect("ready IPv6 target");
        let late = "[2001:db8::2]:443".parse().expect("late IPv6 target");
        let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
        connector.set_socks5_proxy(Some(proxy_addr.to_string()));
        connector.set_local_address(Some(Ipv4Addr::LOCALHOST.into()));
        let mut metrics = connector.metrics.clone();
        let (answer, pending) = oneshot::channel();
        let fallback = Box::pin(async move { pending.await.expect("release late target") });
        let mut connection =
            Box::pin(connector.connect_resolving(vec![ready].into(), fallback, 443));
        let mut first =
            while_connecting(connection.as_mut(), accept_socks_target(&proxy, ready)).await;
        assert_eq!(
            first.peer_addr().expect("bound proxy client").ip(),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        await_io(first.write_all(&[5, 5, 0, 1, 127, 0, 0, 1, 0, 1]))
            .await
            .expect("refuse ready target");
        while_connecting(connection.as_mut(), async {
            while metrics.get_total_errors() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(answer.send(Ok(vec![late].into())).is_ok());
        let mut second =
            while_connecting(connection.as_mut(), accept_socks_target(&proxy, late)).await;
        accept_socks_connection(&mut second).await;
        let completed = await_io(connection).await;
        assert_eq!(
            completed
                .result
                .expect("late target succeeds")
                .peer_addr()
                .expect("proxy peer"),
            proxy_addr
        );
        assert_eq!(completed.addrs.collect::<Vec<_>>(), [ready, late]);
        assert_eq!(completed.retries_used, 0);
        assert_eq!(metrics.get_total_attempts(), 2);
        assert_eq!(metrics.get_errors_for_addr(&proxy_addr), Some(1));
        assert_eq!(metrics.get_successes_for_addr(&proxy_addr), Some(1));
        assert_eq!(metrics.get_attempts_for_addr(&ready), None);
        assert_eq!(metrics.get_attempts_for_addr(&late), None);
    }

    /// A socket setup failure must not discard the still-pending DNS lookup;
    /// its later usable address receives an attempt without restarting DNS.
    #[tokio::test]
    async fn ipv4_bind_failure_still_tries_pending_ipv6() {
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind IPv6 fixture");
        let ipv6 = listener.local_addr().expect("IPv6 fixture address");
        let ipv4 = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), ipv6.port());
        let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
        connector.set_local_addresses(Ipv4Addr::new(192, 0, 2, 254), Ipv6Addr::LOCALHOST);
        connector.set_connect_timeout(Some(Duration::from_secs(2)));
        let mut metrics = connector.metrics.clone();
        let (answer, pending) = oneshot::channel();
        let fallback = Box::pin(async move { pending.await.expect("release AAAA answer") });
        let mut connection =
            Box::pin(connector.connect_resolving(vec![ipv4].into(), fallback, ipv6.port()));
        assert!(futures_util::poll!(&mut connection).is_pending());
        assert_eq!(
            metrics.get_errors_for_addr(&ipv4),
            Some(1),
            "IPv4 setup failed before DNS finished"
        );
        assert!(
            answer.send(Ok(vec![ipv6].into())).is_ok(),
            "connection dropped AAAA receiver"
        );
        let completed = await_io(connection).await;
        assert_eq!(completed.addrs.collect::<Vec<_>>(), [ipv4, ipv6]);
        let client = completed.result.expect("pending IPv6 succeeds");
        assert_eq!(client.peer_addr().expect("connected IPv6 address"), ipv6);
        let (server, _) = await_io(listener.accept())
            .await
            .expect("accept IPv6 connection");
        assert_eq!(
            server.peer_addr().expect("bound client address").ip(),
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        );
        assert_eq!(metrics.get_total_attempts(), 2);
        assert_eq!(metrics.get_errors_for_addr(&ipv4), Some(1));
        assert_eq!(metrics.get_successes_for_addr(&ipv6), Some(1));
    }

    /// A sole source family filters ready and late addresses; the racing entry
    /// delegates to sequential scheduling when only one family is eligible.
    #[tokio::test]
    async fn source_family_admission_applies_to_ready_and_late_answers() {
        for (scenario, racing, delayed, local) in [
            (
                "sequential ready IPv4",
                false,
                false,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
            ),
            (
                "sequential late IPv4",
                false,
                true,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
            ),
            (
                "racing ready IPv6",
                true,
                false,
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ),
            (
                "racing late IPv6",
                true,
                true,
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ),
        ] {
            let ipv6_listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
                .await
                .expect("bind IPv6 fixture");
            let ipv6 = ipv6_listener.local_addr().expect("IPv6 fixture address");
            let ipv4_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind IPv4 fixture");
            let ipv4 = ipv4_listener.local_addr().expect("IPv4 fixture address");
            let (expected, excluded, unused_listener) = if local.is_ipv4() {
                (ipv4, ipv6, &ipv6_listener)
            } else {
                (ipv6, ipv4, &ipv4_listener)
            };
            let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
            connector.set_local_address(Some(local));
            connector.set_happy_eyeballs_timeout(racing.then_some(Duration::from_millis(10)));
            connector.set_connect_timeout(Some(Duration::from_secs(2)));
            connector.set_connect_retries_max(Some(2));
            let mut metrics = connector.metrics.clone();
            let lookup = Arc::new(LookupState::default());
            let (answer, pending) = oneshot::channel();
            let fallback: resolver::HickoryResolverFuture = if delayed {
                Box::pin(async move { pending.await.expect("release mixed AAAA answer") })
            } else {
                Box::pin(PendingLookup(Arc::clone(&lookup)))
            };
            let primary = if delayed {
                vec![excluded]
            } else {
                vec![excluded, expected]
            };
            let mut connection = Box::pin(async {
                if racing {
                    connector
                        .connect_resolving_racing(
                            primary.into(),
                            fallback,
                            expected.port(),
                            Duration::from_millis(10),
                        )
                        .await
                } else {
                    connector
                        .connect_resolving(primary.into(), fallback, expected.port())
                        .await
                }
            });
            if delayed {
                assert!(
                    futures_util::poll!(&mut connection).is_pending(),
                    "{scenario}"
                );
                assert_eq!(metrics.get_total_attempts(), 0, "{scenario}");
                assert!(
                    answer.send(Ok(vec![excluded, expected].into())).is_ok(),
                    "{scenario}: connection dropped pending answer"
                );
            }
            let completed = await_io(connection).await;
            assert_eq!(
                completed.retries_used, 0,
                "{scenario}: no empty retry rounds"
            );
            let client = completed.result.expect(scenario);
            assert_eq!(
                client.peer_addr().expect("selected peer address"),
                expected,
                "{scenario}"
            );
            assert_eq!(
                client.local_addr().expect("bound local address").ip(),
                local,
                "{scenario}"
            );
            if !delayed {
                assert!(lookup.dropped.load(Ordering::SeqCst), "{scenario}");
            }
            assert_no_queued_connection(unused_listener).await;
            assert_eq!(metrics.get_total_attempts(), 1, "{scenario}");
            assert_eq!(metrics.get_total_errors(), 0, "{scenario}");
        }
    }

    /// A malformed proxy cannot be repaired by AAAA finishing, so both paths
    /// must reject it before polling DNS or consuming a connection attempt.
    #[tokio::test]
    async fn invalid_proxy_fails_without_waiting_for_pending_aaaa() {
        for (scenario, racing) in [("sequential families", false), ("Happy Eyeballs", true)] {
            let mut connector = ForgeHttpConnector::new_with_resolver(ForgeResolver::default());
            connector.set_socks5_proxy(Some("invalid-proxy".to_string()));
            let mut metrics = connector.metrics.clone();
            let lookup = Arc::new(LookupState::default());
            let fallback = Box::pin(PendingLookup(Arc::clone(&lookup)));
            let ipv4 = "192.0.2.1:443".parse().expect("IPv4 fixture");
            let mut connection = Box::pin(async {
                if racing {
                    connector
                        .connect_resolving_racing(
                            vec![ipv4].into(),
                            fallback,
                            443,
                            Duration::from_secs(1),
                        )
                        .await
                } else {
                    connector
                        .connect_resolving(vec![ipv4].into(), fallback, 443)
                        .await
                }
            });
            let Poll::Ready(completed) = futures_util::poll!(&mut connection) else {
                panic!("{scenario}: invalid proxy must fail without awaiting DNS");
            };
            let error = completed.result.expect_err(scenario);
            assert_eq!(error.msg.as_ref(), "invalid proxy setting", "{scenario}");
            assert!(
                error
                    .source()
                    .expect("proxy parse error")
                    .is::<std::net::AddrParseError>(),
                "{scenario}"
            );
            assert_eq!(completed.retries_used, 0, "{scenario}");
            assert_eq!(metrics.get_total_attempts(), 0, "{scenario}");
            assert!(!lookup.polled.load(Ordering::SeqCst), "{scenario}");
            assert!(lookup.dropped.load(Ordering::SeqCst), "{scenario}");
        }
    }
}
