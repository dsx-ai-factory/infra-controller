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

use carbide_uuid::machine::MachineId;
use futures::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rpc::forge::{BuildInfo, MachineStatesByIdsRequest, RuntimeConfig, VersionRequest};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::Barrier;
use tonic::Status;

/// `Exchange` specifies one expected Core request and its controlled reply.
pub(super) struct Exchange {
    pub(super) method: &'static str,
    pub(super) request: Vec<u8>,
    pub(super) response: Result<Vec<u8>, Status>,
}

impl Exchange {
    fn matches_state_page(&self, method: &str, payload: &[u8]) -> bool {
        if self.method != method {
            return false;
        }
        matches!(
            method,
            "/forge.Forge/FindMachineStatesByIds" | "/forge.Forge/FindMachinesByIds"
        ) && self.request == payload
    }
}

/// A fixed group must arrive together; selected response bodies then stay pending.
#[derive(Clone, Copy)]
pub(super) struct ConcurrentStatePages {
    pub(super) ids: [MachineId; 4],
    pub(super) pending_bodies: [bool; 4],
}

/// Transport controls for command-specific paging and response-deadline tests.
#[derive(Clone, Copy)]
pub(super) struct CommandOptions {
    pub(super) max_find_by_ids: u32,
    pub(super) request_timeout_secs: Option<u64>,
    // Version stalls only the runtime-config read, leaving connection setup usable.
    pub(super) pending_body_method: Option<&'static str>,
    pub(super) runtime_config_error: Option<tonic::Code>,
    pub(super) unordered_state_pages: bool,
    pub(super) concurrent_state_pages: Option<ConcurrentStatePages>,
}

impl Default for CommandOptions {
    fn default() -> Self {
        Self {
            max_find_by_ids: 1,
            request_timeout_secs: None,
            pending_body_method: None,
            runtime_config_error: None,
            unordered_state_pages: false,
            concurrent_state_pages: None,
        }
    }
}

/// `run_command` checks the real binary against a private, bounded RPC script.
pub(super) async fn run_command(
    args: &[&str],
    stdin: Option<&[u8]>,
    exchanges: Vec<Exchange>,
) -> Output {
    run_command_with_options(args, stdin, exchanges, CommandOptions::default()).await
}

/// `run_command_with_options` checks exact requests, allowing reordered state
/// pages only when explicitly enabled by the caller.
pub(super) async fn run_command_with_options(
    args: &[&str],
    stdin: Option<&[u8]>,
    exchanges: Vec<Exchange>,
    options: CommandOptions,
) -> Output {
    let unordered_state_pages =
        options.unordered_state_pages || options.concurrent_state_pages.is_some();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind private Core listener");
    let api_url = format!("http://{}", listener.local_addr().unwrap());
    let mut expected_calls: Vec<_> = exchanges
        .iter()
        .map(|exchange| (exchange.method.to_owned(), exchange.request.clone()))
        .collect();
    let pending = Arc::new(Mutex::new(VecDeque::from(exchanges)));
    let server_pending = Arc::clone(&pending);
    let received = Arc::new(Mutex::new(Vec::new()));
    let server_received = Arc::clone(&received);
    let compact_page_barrier = options
        .concurrent_state_pages
        .map(|pages| Arc::new(Barrier::new(pages.ids.len())));
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.expect("accept CLI connection");
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request: Request<Incoming>| {
                    let pending = Arc::clone(&server_pending);
                    let received = Arc::clone(&server_received);
                    let compact_page_barrier = compact_page_barrier.clone();
                    async move {
                        let method = request.uri().path().to_string();
                        let mut pending_body = options.pending_body_method == Some(method.as_str());
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
                            pending_body &= request.display_config;
                            let reply = Ok(BuildInfo {
                                runtime_config: request.display_config.then_some(RuntimeConfig {
                                    max_find_by_ids: options.max_find_by_ids,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }
                            .encode_to_vec());
                            if request.display_config {
                                options.runtime_config_error.map_or(reply, |code| {
                                    Err(Status::new(code, "batch limit unavailable"))
                                })
                            } else {
                                reply
                            }
                        } else {
                            // Compare requests after joining the server. A failed
                            // stream must not hide an extra mutation or token lookup.
                            received
                                .lock()
                                .expect("RPC history mutex poisoned")
                                .push((method.clone(), body[5..].to_vec()));
                            let mut pending = pending.lock().expect("RPC script mutex poisoned");
                            let exchange = if unordered_state_pages {
                                let index = pending
                                    .iter()
                                    .take_while(|exchange| exchange.method == method)
                                    .position(|exchange| {
                                        exchange.matches_state_page(&method, &body[5..])
                                    });
                                index.and_then(|index| pending.remove(index))
                            } else {
                                pending.pop_front()
                            };
                            exchange
                                .map(|exchange| exchange.response)
                                .unwrap_or_else(|| Err(Status::internal("unexpected RPC")))
                        };
                        if let Some(pages) = options.concurrent_state_pages
                            && method == "/forge.Forge/FindMachineStatesByIds"
                        {
                            let request = MachineStatesByIdsRequest::decode(&body[5..])
                                .expect("decode compact page");
                            if let Some(index) = pages.ids.iter().position(|id| {
                                request.machine_ids.as_slice() == std::slice::from_ref(id)
                            }) {
                                compact_page_barrier
                                    .as_ref()
                                    .expect("compact page barrier configured")
                                    .wait()
                                    .await;
                                pending_body |= pages.pending_bodies[index];
                            }
                        }
                        let response = if pending_body {
                            // Send headers but never complete the body or trailers.
                            Response::builder()
                                .header(header::CONTENT_TYPE, "application/grpc+tonic")
                                .body(
                                    StreamBody::new(stream::pending::<
                                        Result<Frame<Bytes>, Infallible>,
                                    >())
                                    .boxed(),
                                )
                                .expect("valid pending gRPC response")
                        } else {
                            grpc_response(response).map(BodyExt::boxed)
                        };
                        Ok::<_, Infallible>(response)
                    }
                }),
            )
            .await
            .expect("serve CLI connection");
    });
    let output = tokio::time::timeout(Duration::from_secs(10), async {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nico-admin-cli"));
        command
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
            .kill_on_drop(true);
        if let Some(seconds) = options.request_timeout_secs {
            command.env("FORGE_CLIENT_REQUEST_TIMEOUT_SECS", seconds.to_string());
        }
        let mut child = command.spawn()?;
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
    let mut received = received.lock().expect("RPC history mutex poisoned");
    if unordered_state_pages {
        assert_eq!(
            received.first(),
            expected_calls.first(),
            "first Core request"
        );
        // Reordering within a method must not move a compact request past the
        // switch to full reads or conceal an additional request.
        for calls in [&mut *received, &mut expected_calls] {
            for group in calls.chunk_by_mut(|left, right| left.0 == right.0) {
                group.sort_unstable();
            }
        }
    }
    assert_eq!(
        *received,
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
