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
use hyper::service::Service;
use hyper_util::rt::TokioIo;
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::Sleep;
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
        let connect_timeout = connect_timeout.map(|t| t / addrs.len() as u32);

        Self {
            addrs,
            connect_timeout,
            metrics,
        }
    }
}

impl ConnectingTcpRemote {
    async fn connect(&mut self, config: &Config) -> Result<TcpStream, ConnectError> {
        let mut err = None;
        for addr in &mut self.addrs {
            if let Some(proxy) = config.socks5_proxy.as_deref() {
                let proxy_addr = SocketAddr::from_str(proxy)
                    .map_err(|e| ConnectError::new("Invalid proxy setting", e))?;
                let connect_start = Instant::now();
                match connect_with_socks_proxy(
                    proxy_addr,
                    addr,
                    config.clone(),
                    self.connect_timeout,
                )?
                .await
                {
                    Ok(tcp) => {
                        self.metrics.connect_success(proxy_addr, connect_start);
                        return Ok(tcp);
                    }
                    Err(e) => {
                        self.metrics.connect_error(proxy_addr, connect_start, &e);
                        err = Some(e);
                    }
                }
            } else {
                let connect_start = Instant::now();
                match connect(&addr, config, self.connect_timeout)?.await {
                    Ok(tcp) => {
                        self.metrics.connect_success(addr, connect_start);
                        return Ok(tcp);
                    }
                    Err(e) => {
                        self.metrics.connect_error(addr, connect_start, &e);
                        err = Some(e);
                    }
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
    /// If a domain resolves to multiple IP addresses, the timeout will be
    /// evenly divided across them.
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
    /// If `None`, parallel connection attempts are disabled.
    ///
    /// Default is 300 milliseconds.
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
    //type Error = Box<dyn Error + Send + Sync>;
    type Error = ConnectError;
    //type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + Sync>>;
    type Future = Pin<Box<dyn Future<Output = ConnectResult> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // ready!(self.resolver.poll_ready(cx)).map_err(ConnectError::dns)?;
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

impl ForgeHttpConnector {
    async fn call_async(&mut self, dst: Uri) -> Result<TcpStream, ConnectError> {
        let config = &self.config;
        let (host, port) = get_host_port(config, &dst)?;
        let host = host.trim_start_matches('[').trim_end_matches(']');

        let addrs = if let Some(addrs) = resolver::SocketAddrs::try_parse(host, port) {
            addrs
        } else {
            let dns_name: Name = host.parse().map_err(ConnectError::dns)?;
            let addrs = self
                .resolver
                .call(dns_name)
                .await
                .map_err(ConnectError::dns)?;

            let addrs: Vec<SocketAddr> = addrs
                .map(|mut addr| {
                    addr.set_port(port);
                    addr
                })
                .collect();
            resolver::SocketAddrs::new(addrs)
        };

        let retry_config = RetryFutureConfig::new(self.config.connect_retries_max.unwrap_or(0))
            .fixed_backoff(
                self.config
                    .connect_retries_interval
                    .unwrap_or(Duration::from_secs(0)),
            );

        tryhard::retry_fn(|| {
            trace!(destination_address = %dst, "establishing new tcp connection");
            let c = ConnectingTcp::new(addrs.clone(), config, self.metrics.clone());
            c.connect()
        })
        .with_config(retry_config)
        .await
    }
}
