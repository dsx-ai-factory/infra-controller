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
use std::time::Duration;

use clap::Parser;
use futures::stream;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rpc::admin_cli::OutputFormat;
use rpc::forge::{BuildInfo, RuntimeConfig as RpcRuntimeConfig, VersionRequest};
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::net::TcpListener;

use crate::async_write::CapturedOutput;
use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
use crate::cfg::dispatch::Dispatch;
use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
use crate::rpc::ApiClient;

/// Runs the public version command against a controlled Core response so the
/// test covers argument parsing, RPC dispatch, and the operator's actual table.
async fn run_version(site_fabric_null_routes: Option<Vec<String>>) -> String {
    // Enter through the public arguments so a helper-only rendering regression
    // cannot bypass command dispatch.
    let options =
        CliOptions::try_parse_from(["nico-admin-cli", "version", "--show-runtime-config"])
            .expect("public version command parses");
    let Some(CliCommand::Version(command)) = options.commands else {
        panic!("expected the public version command path");
    };
    assert!(command.show_runtime_config);

    // Supply a real local RPC endpoint and capture the command's output sink.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock version listener binds");
    let address = listener.local_addr().expect("mock listener has an address");
    let request_timeout = Duration::from_secs(5);
    let client_config = ForgeClientConfig {
        request_timeout: Some(request_timeout),
        ..Default::default()
    };
    let mut captured = CapturedOutput::new();
    let ctx = RuntimeContext {
        api_client: ApiClient(ForgeApiClient::new(&ApiConfig::new(
            &format!("http://{address}"),
            &client_config,
        ))),
        config: RuntimeConfig {
            format: OutputFormat::AsciiTable,
            request_timeout: client_config.request_timeout,
            page_size: 25,
            extended: false,
            cloud_unsafe_op: None,
            sort_by: SortField::PrimaryId,
        },
        output_file: std::mem::replace(captured.writer(), Box::new(tokio::io::sink())),
    };

    // Serve both the client's version probe and its runtime-config request.
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.expect("mock accepts a client");
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request| {
                    let routes = site_fabric_null_routes.clone();
                    async move { mock_version_request(request, routes).await }
                }),
            )
            .await
            .expect("mock serves the version connection");
    });

    // Bound the command and stop its server before inspecting the captured text.
    tokio::time::timeout(request_timeout, command.dispatch(ctx))
        .await
        .expect("version dispatch completes within five seconds")
        .expect("version dispatch succeeds");
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "mock version server failed: {error}");
    }

    String::from_utf8(captured.into_bytes().await).expect("version output is UTF-8")
}

/// Preserves the client's version-probe handshake while returning the selected
/// field presence only for the runtime-config request under test.
async fn mock_version_request(
    request: hyper::Request<Incoming>,
    site_fabric_null_routes: Option<Vec<String>>,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
    // Decode the actual request to distinguish connection setup from the command.
    assert_eq!(request.uri().path(), "/forge.Forge/Version");
    let body = request
        .into_body()
        .collect()
        .await
        .expect("version request body is readable")
        .to_bytes();
    let payload = body.get(5..).expect("version request has a gRPC frame");
    let request = VersionRequest::decode(payload).expect("version request decodes");

    // Only the explicit config request should observe the selected Core contract.
    Ok(grpc_response(if request.display_config {
        BuildInfo {
            runtime_config: Some(RpcRuntimeConfig {
                site_fabric_null_routes: site_fabric_null_routes
                    .map(|items| rpc::common::StringList { items }),
                ..Default::default()
            }),
            ..Default::default()
        }
    } else {
        // ForgeApiClient probes Version without runtime config while establishing
        // the connection before issuing the command's authoritative request.
        BuildInfo::default()
    }))
}

/// Encodes a successful unary gRPC response so the mock exercises the normal
/// client transport without replacing the command's API client.
fn grpc_response(message: impl prost::Message) -> Response<UnsyncBoxBody<Bytes, Infallible>> {
    // Prefix the protobuf payload with the uncompressed gRPC frame header.
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
    // A successful trailer is required for the client to accept the response.
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

/// Verifies operators can distinguish configured, disabled, and unsupported null
/// routes in the public table, including its column labels and empty cell.
#[tokio::test]
async fn public_version_command_prints_populated_empty_and_unsupported_effective_null_routes() {
    for (scenario, routes, expected_value) in [
        // A supported populated field must expose its effective CIDRs.
        (
            "populated",
            Some(vec!["192.0.2.0/24".to_string()]),
            "192.0.2.0/24",
        ),
        // Intentional disablement remains a supported field with an empty cell.
        ("empty", Some(vec![]), ""),
        // An older Core's missing field must not look like deliberate disablement.
        ("unsupported by old Core", None, "Unsupported"),
    ] {
        // Exercise public dispatch before checking the rendered table contract.
        let output = run_version(routes).await;
        assert!(
            output.lines().any(|line| {
                line.split('|').map(str::trim).collect::<Vec<_>>() == ["", "Property", "Value", ""]
            }),
            "{scenario}: missing runtime-config table headers: {output}"
        );
        assert!(
            output.ends_with('\n') && !output.ends_with("\n\n"),
            "{scenario}: the table must end with exactly one newline"
        );
        // Check the value in its actual column so an empty cell cannot be
        // confused with a missing row or an unsupported field.
        let row = output
            .lines()
            .find(|line| line.contains("| site_fabric_null_routes"))
            .unwrap_or_else(|| panic!("{scenario}: missing site_fabric_null_routes row: {output}"));
        let cells = row.split('|').map(str::trim).collect::<Vec<_>>();
        assert_eq!(cells.get(1), Some(&"site_fabric_null_routes"), "{scenario}");
        assert_eq!(cells.get(2), Some(&expected_value), "{scenario}: {row}");
    }
}
