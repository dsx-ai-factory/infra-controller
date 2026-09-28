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
//! HTTP/1 accept loop with a header read timeout.
//!
//! `axum::serve` builds hyper connections without a timer, which disables
//! hyper's header read timeout, and its HTTP version sniff has no timeout at
//! all, so a UEFI HTTP client that idles on a keep-alive connection or never
//! sends a request holds its file descriptor until restart. PXE clients speak
//! HTTP/1.1 only, so this loop serves HTTP/1 directly with a timer installed
//! and closes connections that deliver no request head within the timeout.

use std::convert::Infallible;
use std::io;
use std::time::Duration;

use axum::Extension;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, Response};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::Service;

/// How long a connection may sit without delivering a complete request head,
/// whether it is new or idle between keep-alive requests, before the server
/// closes it.
pub(crate) const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Serves `app` on `listener`. Each connection carries `ConnectInfo<SocketAddr>`
/// for the peer, matching what `Router::into_make_service_with_connect_info`
/// provides.
pub(crate) async fn serve<S>(
    listener: TcpListener,
    app: S,
    header_read_timeout: Duration,
) -> io::Result<()>
where
    S: Service<Request<Body>, Response = Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout);

    loop {
        let (stream, peer_address) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) if is_connection_error(&error) => continue,
            Err(error) => {
                // Typically fd exhaustion; back off so the loop does not spin.
                tracing::error!(%error, "accept failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let builder = builder.clone();
        let app = tower::ServiceBuilder::new()
            .map_request(|request: Request<Incoming>| request.map(Body::new))
            .layer(Extension(ConnectInfo(peer_address)))
            .service(app.clone());
        tokio::spawn(async move {
            if let Err(error) = builder
                .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app))
                .await
            {
                tracing::debug!(%error, %peer_address, "connection closed with error");
            }
        });
    }
}

fn is_connection_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::Router;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_millis(200);
    const WAIT: Duration = Duration::from_secs(5);

    #[derive(Debug, Clone, Copy)]
    enum Step {
        /// Send a request and expect a 200 whose body echoes the client address.
        Request,
        /// Send nothing and expect the server to close the connection.
        IdleUntilClosed,
    }

    async fn start_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.to_string() }),
        );
        tokio::spawn(serve(listener, app, TEST_TIMEOUT));
        addr
    }

    async fn read_until(stream: &mut TcpStream, needle: &str) -> String {
        let mut buf = Vec::new();
        tokio::time::timeout(WAIT, async {
            loop {
                let mut chunk = [0u8; 1024];
                let n = stream.read(&mut chunk).await.unwrap();
                assert_ne!(n, 0, "connection closed before {needle:?} arrived");
                buf.extend_from_slice(&chunk[..n]);
                if String::from_utf8_lossy(&buf).contains(needle) {
                    break;
                }
            }
        })
        .await
        .expect("timed out waiting for response");
        String::from_utf8_lossy(&buf).into_owned()
    }

    async fn run_steps(server: SocketAddr, steps: &[Step]) {
        let mut stream = TcpStream::connect(server).await.unwrap();
        let client = stream.local_addr().unwrap().to_string();
        for step in steps {
            match step {
                Step::Request => {
                    stream
                        .write_all(b"GET / HTTP/1.1\r\nHost: pxe\r\n\r\n")
                        .await
                        .unwrap();
                    let response = read_until(&mut stream, &client).await;
                    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
                }
                Step::IdleUntilClosed => {
                    let mut buf = [0u8; 1];
                    let read = tokio::time::timeout(WAIT, stream.read(&mut buf))
                        .await
                        .expect("server did not close the idle connection");
                    assert_eq!(read.unwrap(), 0, "expected EOF from server");
                }
            }
        }
    }

    #[tokio::test]
    async fn idle_connections_are_closed_after_header_read_timeout() {
        let server = start_server().await;
        let cases: &[(&str, &[Step])] = &[
            ("never sends a request", &[Step::IdleUntilClosed]),
            (
                "idles after keep-alive requests",
                &[Step::Request, Step::Request, Step::IdleUntilClosed],
            ),
        ];
        for (scenario, steps) in cases {
            let started = tokio::time::Instant::now();
            run_steps(server, steps).await;
            assert!(
                started.elapsed() >= TEST_TIMEOUT,
                "{scenario}: closed before the timeout elapsed"
            );
        }
    }
}
