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

use carbide_test_support::{Case, Outcome, check_cases_async};
use clap::Parser;
use futures::stream;
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
use rpc::protos::mlx_device::{
    MlxAdminDeviceIdentitiesRequest, MlxAdminDeviceIdentitiesResponse, MlxDeviceIdentity,
    MlxDeviceIdentityReport, MlxDeviceInfo,
};
use tokio::net::TcpListener;

use crate::async_write::CapturedOutput;
use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
use crate::cfg::dispatch::Dispatch;
use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

const HOST_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
const DPU_ID: &str = "fm100ds3gfip02lfgleidqoitqgh8d8mdc4a3j2tdncbjrfjtvrrhn2kleg";
const SECOND_DPU_ID: &str = "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg";

fn stored_identities() -> MlxAdminDeviceIdentitiesResponse {
    MlxAdminDeviceIdentitiesResponse {
        report: Some(MlxDeviceIdentityReport {
            observed_at: Some(
                "2026-09-29T01:02:03Z"
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .unwrap()
                    .into(),
            ),
            devices: vec![
                MlxDeviceIdentity {
                    device_info: Some(MlxDeviceInfo {
                        pci_name: "0000:01:00.0".to_string(),
                        base_mac: "02:11:22:33:44:55".to_string(),
                        base_guid: Some("021122fffe334455".to_string()),
                        ..Default::default()
                    }),
                    managed_dpu_machine_ids: vec![DPU_ID.parse().unwrap()],
                },
                MlxDeviceIdentity {
                    device_info: Some(MlxDeviceInfo {
                        pci_name: "0000:02:00.0".to_string(),
                        ..Default::default()
                    }),
                    managed_dpu_machine_ids: vec![],
                },
                MlxDeviceIdentity {
                    device_info: Some(MlxDeviceInfo {
                        pci_name: "0000:03:00.0".to_string(),
                        base_mac: "02:11:22:33:44:66".to_string(),
                        base_guid: Some("not-a-guid".to_string()),
                        ..Default::default()
                    }),
                    managed_dpu_machine_ids: vec![
                        DPU_ID.parse().unwrap(),
                        SECOND_DPU_ID.parse().unwrap(),
                    ],
                },
            ],
        }),
    }
}

#[tokio::test]
async fn identities_command_renders_stored_evidence_and_missing_observations() {
    for response in [
        stored_identities(),
        MlxAdminDeviceIdentitiesResponse::default(),
    ] {
        let (result, output) =
            dispatch_identities(OutputFormat::AsciiTable, response.clone()).await;
        result.expect("stored identity command succeeds");
        if response.report.is_none() {
            assert_eq!(output, "No stored NIC observation\n");
            continue;
        }

        assert!(output.contains("Scout observed at: 2026-09-29T01:02:03Z"));
        assert!(output.contains("firmware/reset eligibility is not established"));
        let rows: Vec<Vec<&str>> = output
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| line.trim_matches('|').split('|').map(str::trim).collect())
            .collect();
        assert_eq!(
            rows[0],
            ["PCI Name", "Base MAC", "Base GUID", "Managed DPU"]
        );
        assert_eq!(
            rows[1],
            [
                "0000:01:00.0",
                "02:11:22:33:44:55",
                "021122fffe334455",
                DPU_ID
            ]
        );
        assert_eq!(rows[2], ["0000:02:00.0", "", "", "Unknown"]);
        assert_eq!(
            rows[3],
            [
                "0000:03:00.0",
                "02:11:22:33:44:66",
                "not-a-guid",
                "Conflicting:"
            ]
        );
        assert_eq!(rows[4], ["", "", "", DPU_ID]);
        assert_eq!(rows[5], ["", "", "", SECOND_DPU_ID]);
    }
}

#[tokio::test]
async fn identities_command_preserves_structured_response() {
    for (format, response) in [
        (OutputFormat::Json, stored_identities()),
        (
            OutputFormat::Yaml,
            MlxAdminDeviceIdentitiesResponse::default(),
        ),
    ] {
        let (result, output) = dispatch_identities(format, response.clone()).await;
        match format {
            OutputFormat::Json => {
                result.expect("JSON identity command succeeds");
                assert!(!output.contains("identity_status"));
                assert_eq!(
                    serde_json::from_str::<MlxAdminDeviceIdentitiesResponse>(&output).unwrap(),
                    response
                );
            }
            OutputFormat::Yaml => {
                result.expect("YAML identity command succeeds");
                assert_eq!(
                    serde_yaml::from_str::<MlxAdminDeviceIdentitiesResponse>(&output).unwrap(),
                    response
                );
            }
            OutputFormat::AsciiTable | OutputFormat::Csv => {
                unreachable!("table formats have their own rendering checks")
            }
        }
    }
}

#[tokio::test]
async fn identities_command_exports_table_as_csv() {
    check_cases_async(
        [
            Case {
                scenario: "device facts and conflicting owners",
                input: stored_identities(),
                expect: Outcome::Yields(vec![
                    csv::StringRecord::from(vec![
                        "0000:01:00.0",
                        "02:11:22:33:44:55",
                        "021122fffe334455",
                        DPU_ID,
                    ]),
                    csv::StringRecord::from(vec!["0000:02:00.0", "", "", "Unknown"]),
                    csv::StringRecord::from(vec![
                        "0000:03:00.0",
                        "02:11:22:33:44:66",
                        "not-a-guid",
                        &format!("Conflicting:\n{DPU_ID}\n{SECOND_DPU_ID}"),
                    ]),
                ]),
            },
            Case {
                scenario: "no stored observation produces headers only",
                input: MlxAdminDeviceIdentitiesResponse::default(),
                expect: Outcome::Yields(vec![]),
            },
        ],
        |response| async move {
            let (result, output) = dispatch_identities(OutputFormat::Csv, response).await;
            result.map_err(|error| error.to_string())?;
            let mut reader = csv::Reader::from_reader(output.as_bytes());
            assert_eq!(
                reader.headers().expect("CSV headers"),
                &csv::StringRecord::from(vec!["PCI Name", "Base MAC", "Base GUID", "Managed DPU"])
            );
            reader
                .records()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())
        },
    )
    .await;
}

async fn dispatch_identities(
    format: OutputFormat,
    response: MlxAdminDeviceIdentitiesResponse,
) -> (CarbideCliResult<()>, String) {
    let options =
        CliOptions::try_parse_from(["nico-admin-cli", "mlx", "info", "identities", HOST_ID])
            .expect("public identity command parses");
    let Some(CliCommand::Mlx(command)) = options.commands else {
        panic!("expected the public mlx command");
    };
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
            format,
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
                service_fn(move |request| mock_identity_request(request, response.clone())),
            )
            .await
            .expect("mock serves the identity connection");
    });
    let result = tokio::time::timeout(request_timeout, command.dispatch(ctx)).await;
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "mock identity server failed: {error}");
    }
    let result = result.expect("identity dispatch finishes within five seconds");
    (
        result,
        String::from_utf8(captured.into_bytes().await).unwrap(),
    )
}

async fn mock_identity_request(
    request: Request<Incoming>,
    response: MlxAdminDeviceIdentitiesResponse,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
    Ok(match request.uri().path() {
        "/forge.Forge/Version" => grpc_response(BuildInfo::default()),
        "/forge.Forge/MlxAdminShowDeviceIdentities" => {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(body.first(), Some(&0));
            let request = MlxAdminDeviceIdentitiesRequest::decode(body.get(5..).unwrap()).unwrap();
            assert_eq!(request.machine_id, Some(HOST_ID.parse().unwrap()));
            grpc_response(response)
        }
        path => panic!("unexpected mock Forge method: {path}"),
    })
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
