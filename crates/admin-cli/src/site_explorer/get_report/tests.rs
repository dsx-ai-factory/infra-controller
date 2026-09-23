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
use std::panic::AssertUnwindSafe;
use std::time::Duration;

use clap::Parser;
use futures::{FutureExt, stream};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use rpc::admin_cli::OutputFormat;
use rpc::forge::BuildInfo;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use rpc::site_explorer::{
    ExploredDpu, ExploredEndpoint, ExploredEndpointList, ExploredEndpointsByIdsRequest,
    ExploredManagedHost, ExploredManagedHostIdList, ExploredManagedHostList,
    ExploredManagedHostsByIdsRequest,
};
use tokio::net::TcpListener;

use crate::async_write::CapturedOutput;
use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
use crate::cfg::dispatch::Dispatch;
use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
use crate::rpc::ApiClient;
use crate::site_explorer::Cmd;

fn parse(command: &str, address: &str) -> Result<Cmd, clap::Error> {
    let options = CliOptions::try_parse_from([
        "nico-admin-cli",
        "site-explorer",
        "get-report",
        command,
        address,
    ])?;
    let Some(CliCommand::SiteExplorer(command)) = options.commands else {
        panic!("expected the public site-explorer command");
    };
    Ok(command)
}

#[test]
fn report_selectors_reject_invalid_ip_addresses() {
    for command in ["endpoint", "managed-host"] {
        let error = parse(command, "not-an-ip").expect_err("invalid IP is rejected");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::ValueValidation,
            "{command}"
        );
    }
}

#[tokio::test]
async fn report_selectors_match_ip_addresses() {
    let hosts = vec![
        ExploredManagedHost {
            host_bmc_ip: "2001:db8::10".to_string(),
            dpus: vec![ExploredDpu {
                bmc_ip: "2001:db8::20".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ExploredManagedHost {
            host_bmc_ip: "192.0.2.10".to_string(),
            ..Default::default()
        },
    ];
    struct Case {
        command: &'static str,
        selector: &'static str,
        expected: serde_json::Value,
    }
    for case in [
        Case {
            command: "endpoint",
            selector: "2001:0DB8:0:0:0:0:0:10",
            expected: serde_json::to_value(ExploredEndpoint {
                address: "2001:db8::10".to_string(),
                ..Default::default()
            })
            .unwrap(),
        },
        Case {
            command: "managed-host",
            selector: "2001:0DB8:0:0:0:0:0:10",
            expected: serde_json::to_value(&hosts[0]).unwrap(),
        },
        Case {
            command: "managed-host",
            selector: "2001:0DB8:0:0:0:0:0:20",
            expected: serde_json::to_value(&hosts[0]).unwrap(),
        },
        Case {
            command: "endpoint",
            selector: "192.0.2.10",
            expected: serde_json::to_value(ExploredEndpoint {
                address: "192.0.2.10".to_string(),
                ..Default::default()
            })
            .unwrap(),
        },
    ] {
        let output =
            dispatch_report(parse(case.command, case.selector).unwrap(), hosts.clone()).await;
        assert_eq!(output, case.expected, "{} {}", case.command, case.selector);
    }
}

async fn dispatch_report(command: Cmd, hosts: Vec<ExploredManagedHost>) -> serde_json::Value {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
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
            format: OutputFormat::Json,
            request_timeout: client_config.request_timeout,
            page_size: 25,
            extended: false,
            cloud_unsafe_op: None,
            sort_by: SortField::PrimaryId,
        },
        output_file: std::mem::replace(captured.writer(), Box::new(tokio::io::sink())),
    };
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.unwrap();
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request| mock_report_request(request, hosts.clone())),
            )
            .await
            .expect("mock serves the report connection");
    });
    let result = AssertUnwindSafe(tokio::time::timeout(request_timeout, command.dispatch(ctx)))
        .catch_unwind()
        .await;
    // Join even if dispatch fails so no listener survives a failed test.
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "mock report server failed: {error}");
    }
    result
        .expect("report dispatch did not panic")
        .expect("report dispatch finishes within five seconds")
        .expect("report dispatch succeeds");
    serde_json::from_slice(&captured.into_bytes().await).expect("command writes JSON")
}

async fn mock_report_request(
    request: Request<Incoming>,
    hosts: Vec<ExploredManagedHost>,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
    Ok(match request.uri().path() {
        "/forge.Forge/Version" => grpc_response(BuildInfo::default()),
        "/forge.Forge/FindExploredManagedHostIds" => grpc_response(ExploredManagedHostIdList {
            host_ids: hosts.into_iter().map(|host| host.host_bmc_ip).collect(),
        }),
        "/forge.Forge/FindExploredManagedHostsByIds" => {
            let request: ExploredManagedHostsByIdsRequest = decode_request(request).await;
            grpc_response(ExploredManagedHostList {
                managed_hosts: hosts
                    .into_iter()
                    .filter(|host| request.host_ids.contains(&host.host_bmc_ip))
                    .collect(),
            })
        }
        "/forge.Forge/FindExploredEndpointsByIds" => {
            let request: ExploredEndpointsByIdsRequest = decode_request(request).await;
            let endpoints = hosts.into_iter().flat_map(|host| {
                std::iter::once(host.host_bmc_ip).chain(host.dpus.into_iter().map(|dpu| dpu.bmc_ip))
            });
            grpc_response(ExploredEndpointList {
                endpoints: endpoints
                    .filter(|address| request.endpoint_ids.contains(address))
                    .map(|address| ExploredEndpoint {
                        address,
                        ..Default::default()
                    })
                    .collect(),
            })
        }
        path => panic!("unexpected mock Forge method: {path}"),
    })
}

async fn decode_request<T: Message + Default>(request: Request<Incoming>) -> T {
    let body = request.into_body().collect().await.unwrap().to_bytes();
    // The default client sends one uncompressed gRPC frame per unary request.
    assert_eq!(body.first(), Some(&0));
    T::decode(body.get(5..).expect("request has a gRPC frame")).expect("request decodes")
}

fn grpc_response(message: impl Message) -> Response<UnsyncBoxBody<Bytes, Infallible>> {
    let mut data = vec![0];
    data.extend_from_slice(&u32::try_from(message.encoded_len()).unwrap().to_be_bytes());
    message.encode(&mut data).unwrap();
    let mut trailers = hyper::HeaderMap::new();
    trailers.insert("grpc-status", header::HeaderValue::from_static("0"));
    let body = StreamBody::new(stream::iter([
        Ok::<_, Infallible>(Frame::data(Bytes::from(data))),
        Ok(Frame::trailers(trailers)),
    ]))
    .boxed_unsync();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/grpc+tonic")
        .body(body)
        .unwrap()
}
