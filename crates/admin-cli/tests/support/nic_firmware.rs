/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! A private Core endpoint records requests from the real admin CLI process.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rpc::forge::{BuildInfo, RuntimeConfig, VersionRequest};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::Command;
use tonic::Status;

/// `Exchange` specifies one expected Core request and its controlled reply.
pub(super) struct Exchange {
    pub(super) method: &'static str,
    pub(super) request: Vec<u8>,
    pub(super) response: Result<Vec<u8>, Status>,
}

/// `run_command` checks the real binary against a private, bounded RPC script.
pub(super) async fn run_command(
    args: &[&str],
    stdin: Option<&[u8]>,
    exchanges: Vec<Exchange>,
) -> Output {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind private Core listener");
    let api_url = format!("http://{}", listener.local_addr().unwrap());
    let expected_calls: Vec<_> = exchanges
        .iter()
        .map(|exchange| (exchange.method.to_owned(), exchange.request.clone()))
        .collect();
    let pending = Arc::new(Mutex::new(VecDeque::from(exchanges)));
    let server_pending = Arc::clone(&pending);
    let received = Arc::new(Mutex::new(Vec::new()));
    let server_received = Arc::clone(&received);
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.expect("accept CLI connection");
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request: Request<Incoming>| {
                    let pending = Arc::clone(&server_pending);
                    let received = Arc::clone(&server_received);
                    async move {
                        let method = request.uri().path().to_string();
                        let body = request
                            .into_body()
                            .collect()
                            .await
                            .expect("read RPC request")
                            .to_bytes();
                        assert_eq!(body.first(), Some(&0), "uncompressed request: {method}");
                        let response = if method == "/forge.Forge/Version" {
                            // Core includes runtime config only when explicitly
                            // requested; the initial connection probe cannot set the cap.
                            let request =
                                VersionRequest::decode(&body[5..]).expect("decode version request");
                            Ok(BuildInfo {
                                runtime_config: request.display_config.then_some(RuntimeConfig {
                                    max_find_by_ids: 1,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }
                            .encode_to_vec())
                        } else {
                            // Compare requests after joining the server. A failed
                            // stream must not hide an extra mutation or token lookup.
                            received
                                .lock()
                                .expect("RPC history mutex poisoned")
                                .push((method, body[5..].to_vec()));
                            pending
                                .lock()
                                .expect("RPC script mutex poisoned")
                                .pop_front()
                                .map(|exchange| exchange.response)
                                .unwrap_or_else(|| Err(Status::internal("unexpected RPC")))
                        };
                        Ok::<_, Infallible>(grpc_response(response))
                    }
                }),
            )
            .await
            .expect("serve CLI connection");
    });
    let output = tokio::time::timeout(Duration::from_secs(10), async {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nico-admin-cli"))
            // Per-user config and proxies must not change the private endpoint.
            .env_clear()
            .args([
                "--api-url",
                &api_url,
                "--root-ca-path",
                "/unused/ca.crt",
                "--client-cert-path",
                "/unused/client.crt",
                "--client-key-path",
                "/unused/client.key",
            ])
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        if let Some(input) = stdin {
            let mut pipe = child.stdin.take().expect("stdin was piped");
            pipe.write_all(input).await?;
        }
        child.wait_with_output().await
    })
    .await;
    // Stop and join the connection task before inspecting its request history.
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "mock Core server failed: {error}");
    }
    let output = output
        .expect("CLI completes within ten seconds")
        .expect("run CLI");
    assert_eq!(
        *received.lock().expect("RPC history mutex poisoned"),
        expected_calls,
        "Core requests differ: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// `grpc_response` builds success and failure trailers for the real transport.
fn grpc_response(
    reply: Result<Vec<u8>, Status>,
) -> Response<impl hyper::body::Body<Data = Bytes, Error = Infallible>> {
    let mut trailers = hyper::HeaderMap::new();
    let mut frames = Vec::new();
    match reply {
        Ok(payload) => {
            let mut data = vec![0];
            data.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
            data.extend_from_slice(&payload);
            frames.push(Ok(Frame::data(Bytes::from(data))));
            trailers.insert("grpc-status", header::HeaderValue::from_static("0"));
        }
        Err(error) => {
            trailers.insert("grpc-status", (error.code() as i32).into());
            trailers.insert(
                "grpc-message",
                error.message().parse().expect("ASCII diagnostic"),
            );
        }
    }
    frames.push(Ok(Frame::trailers(trailers)));
    Response::builder()
        .header(header::CONTENT_TYPE, "application/grpc+tonic")
        .body(StreamBody::new(stream::iter(frames)))
        .expect("valid gRPC response")
}

/// `assert_success` includes the operator's diagnostic when a command fails.
pub(super) fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
