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
use rpc::forge as pb;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::net::TcpListener;

use crate::async_write::CapturedOutput;
use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
use crate::cfg::dispatch::Dispatch;
use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
use crate::rpc::ApiClient;

#[tokio::test]
async fn public_sdn_reset_commands_preserve_job_ids_failures_and_status() {
    let switch_id = "sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60";
    let cases = [
        (
            "submit",
            vec![
                "reset-switch-sdn-factory-default",
                "--switch-id",
                switch_id,
                "--domain-name",
                "nvl-domain",
            ],
            None,
            false,
            true,
        ),
        (
            "partial submission",
            vec!["reset-switch-sdn-factory-default", "--switch-id", switch_id],
            None,
            true,
            false,
        ),
        (
            "pending",
            vec!["get-switch-sdn-factory-reset-status", "--job-id", "parent"],
            Some(pb::SwitchSdnFactoryResetState::Pending),
            false,
            true,
        ),
        (
            "completed",
            vec!["get-switch-sdn-factory-reset-status", "--job-id", "parent"],
            Some(pb::SwitchSdnFactoryResetState::Completed),
            false,
            true,
        ),
        (
            "failed",
            vec!["get-switch-sdn-factory-reset-status", "--job-id", "parent"],
            Some(pb::SwitchSdnFactoryResetState::Failed),
            false,
            false,
        ),
    ];
    for (label, args, state, partial, success) in cases {
        let expected_domain = if label == "submit" {
            Some("nvl-domain".to_owned())
        } else {
            None
        };
        let options = CliOptions::try_parse_from(
            ["nico-admin-cli", "component-manager"]
                .into_iter()
                .chain(args),
        )
        .unwrap();
        let Some(CliCommand::ComponentManager(command)) = options.commands else {
            panic!("component-manager command required");
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client_config = ForgeClientConfig {
            request_timeout: Some(Duration::from_secs(5)),
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
                    service_fn(move |request: hyper::Request<Incoming>| {
                        let expected_domain = expected_domain.clone();
                        async move {
                            let path = request.uri().path().to_owned();
                            let body = request.into_body().collect().await.unwrap().to_bytes();
                            let response = match path.as_str() {
                                "/forge.Forge/Version" => grpc_response(pb::BuildInfo::default()),
                                "/forge.Forge/ComponentResetSwitchSdnFactoryDefault" => {
                                    let request =
                                        pb::ComponentResetSwitchSdnFactoryDefaultRequest::decode(
                                            &body[5..],
                                        )
                                        .unwrap();
                                    assert_eq!(request.switch_ids.unwrap().ids.len(), 1);
                                    assert_eq!(request.domain_name, expected_domain);
                                    grpc_response(
                                        pb::ComponentResetSwitchSdnFactoryDefaultResponse {
                                            job_id: "parent".into(),
                                            submission_errors: if partial {
                                                vec!["rejected-node: unsupported target".into()]
                                            } else {
                                                vec![]
                                            },
                                        },
                                    )
                                }
                                "/forge.Forge/GetSwitchSdnFactoryResetStatus" => {
                                    assert_eq!(
                                        pb::GetSwitchSdnFactoryResetStatusRequest::decode(
                                            &body[5..]
                                        )
                                        .unwrap()
                                        .job_id,
                                        "parent"
                                    );
                                    let state = state.unwrap();
                                    grpc_response(pb::GetSwitchSdnFactoryResetStatusResponse {
                                        state: state as i32,
                                        error: (state == pb::SwitchSdnFactoryResetState::Failed)
                                            .then(|| "reset failed".into()),
                                    })
                                }
                                _ => panic!("unexpected RPC: {path}"),
                            };
                            Ok::<_, Infallible>(response)
                        }
                    }),
                )
                .await
                .unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(5), command.dispatch(ctx))
            .await
            .unwrap();
        server.abort();
        if let Err(error) = server.await {
            assert!(error.is_cancelled(), "{error}");
        }
        assert_eq!(result.is_ok(), success, "{label}: {result:?}");
        let output: serde_json::Value =
            serde_json::from_slice(&captured.into_bytes().await).unwrap();
        if let Some(state) = state {
            assert_eq!(output["state"], state.as_str_name(), "{label}");
            assert_eq!(output["error"].is_null(), success, "{label}");
        } else {
            assert_eq!(output["job_id"], "parent", "{label}");
            assert_eq!(
                output["submission_errors"].as_array().unwrap().len(),
                usize::from(partial),
                "{label}"
            );
        }
    }
}

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
