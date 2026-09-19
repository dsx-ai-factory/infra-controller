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
use std::sync::{Arc, Mutex};
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
use rpc::forge;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::net::TcpListener;

use super::Cmd;
use crate::async_write::CapturedOutput;
use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
use crate::cfg::dispatch::Dispatch;
use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

fn parse(args: &[&str]) -> Result<Cmd, clap::Error> {
    let options = CliOptions::try_parse_from(
        ["nico-admin-cli", "expected-rack-group"]
            .into_iter()
            .chain(args.iter().copied()),
    )?;
    let Some(CliCommand::ExpectedRackGroup(command)) = options.commands else {
        panic!("expected public expected-rack-group command");
    };
    Ok(command)
}

#[test]
fn required_arguments_are_validated_before_dispatch() {
    for (args, expected) in [
        (
            vec!["update", "group-01"],
            clap::error::ErrorKind::MissingRequiredArgument,
        ),
        (
            vec!["erase"],
            clap::error::ErrorKind::MissingRequiredArgument,
        ),
        (
            vec!["add", "group-01", "topology", "--member", "{}"],
            clap::error::ErrorKind::ValueValidation,
        ),
        (
            vec![
                "add",
                "group-01",
                "topology",
                "--member",
                r#"{"type":"NVSwitch","manufacturer":"NVIDIA","id":"switch-01"}"#,
            ],
            clap::error::ErrorKind::ValueValidation,
        ),
    ] {
        assert_eq!(parse(&args).unwrap_err().kind(), expected, "{args:?}");
    }
}

fn populated_group() -> forge::ExpectedRackGroup {
    forge::ExpectedRackGroup {
        rack_group_id: Some("nvl5-gp1-jhb01".parse().unwrap()),
        topology: "gb200_nvl72r1_c2g4".into(),
        rack_ids: vec!["rack-01".parse().unwrap(), "rack-02".parse().unwrap()],
        members: vec![forge::ExpectedRackGroupMember {
            r#type: "Switch".into(),
            manufacturer: "NVIDIA".into(),
            id: "switch-01".into(),
        }],
        metadata: Some(forge::Metadata {
            name: "nvl5-gp1-jhb01".into(),
            description: "test group".into(),
            labels: vec![forge::Label {
                key: "location.datacenter".into(),
                value: Some("JHB01".into()),
            }],
        }),
    }
}

#[tokio::test]
async fn writes_dispatch_expected_rpc_payloads() {
    let attributes = [
        "--rack-id",
        "rack-01",
        "--rack-id",
        "rack-02",
        "--member",
        r#"{"type":"Switch","manufacturer":"NVIDIA","id":"switch-01"}"#,
        "--meta-name",
        "nvl5-gp1-jhb01",
        "--meta-description",
        "test group",
        "--label",
        "location.datacenter:JHB01",
    ];
    for (args, method, expected) in [
        (
            ["add", "nvl5-gp1-jhb01", "gb200_nvl72r1_c2g4"]
                .into_iter()
                .chain(attributes)
                .collect::<Vec<_>>(),
            "AddExpectedRackGroup",
            populated_group(),
        ),
        (
            [
                "update",
                "nvl5-gp1-jhb01",
                "--topology",
                "gb200_nvl72r1_c2g4",
            ]
            .into_iter()
            .chain(attributes)
            .collect(),
            "UpdateExpectedRackGroup",
            populated_group(),
        ),
        (
            vec![
                "update",
                "nvl5-gp1-jhb01",
                "--topology",
                "gb200_nvl72r1_c2g4",
            ],
            "UpdateExpectedRackGroup",
            forge::ExpectedRackGroup {
                rack_group_id: populated_group().rack_group_id,
                topology: populated_group().topology,
                metadata: Some(forge::Metadata::default()),
                ..Default::default()
            },
        ),
    ] {
        let (result, _, calls) = dispatch(&args, OutputFormat::AsciiTable, method, vec![]).await;
        result.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            forge::ExpectedRackGroup::decode(calls[0].as_slice()).unwrap(),
            expected
        );
    }
    for (args, method) in [
        (vec!["delete", "nvl5-gp1-jhb01"], "DeleteExpectedRackGroup"),
        (vec!["erase", "--confirm"], "DeleteAllExpectedRackGroups"),
    ] {
        let (result, _, calls) = dispatch(&args, OutputFormat::AsciiTable, method, vec![]).await;
        result.unwrap();
        assert_eq!(calls.len(), 1);
        if method == "DeleteExpectedRackGroup" {
            assert_eq!(
                forge::ExpectedRackGroupRequest::decode(calls[0].as_slice())
                    .unwrap()
                    .rack_group_id,
                "nvl5-gp1-jhb01"
            );
        } else {
            assert!(calls[0].is_empty());
        }
    }
}

#[tokio::test]
async fn show_renders_public_table_and_json_contract() {
    let empty = forge::ExpectedRackGroup {
        rack_group_id: Some("empty-group".parse().unwrap()),
        topology: "empty-topology".into(),
        ..Default::default()
    };
    let groups = forge::ExpectedRackGroupList {
        expected_rack_groups: vec![populated_group(), empty],
    };
    let (result, output, calls) = dispatch(
        &["show"],
        OutputFormat::AsciiTable,
        "FindExpectedRackGroupsByIds",
        groups.encode_to_vec(),
    )
    .await;
    result.unwrap();
    assert_eq!(calls.len(), 2);
    for (call, group) in calls.iter().zip(&groups.expected_rack_groups) {
        assert_eq!(
            forge::ExpectedRackGroupsByIdsRequest::decode(call.as_slice())
                .unwrap()
                .rack_group_ids,
            vec![group.rack_group_id.clone().unwrap()]
        );
    }
    let rows: Vec<Vec<_>> = output
        .lines()
        .filter(|line| line.starts_with('|'))
        .map(|line| line.split('|').skip(1).take(7).map(str::trim).collect())
        .collect();
    assert_eq!(
        rows,
        vec![
            vec![
                "Rack Group ID",
                "Topology",
                "Rack IDs",
                "Members",
                "Name",
                "Description",
                "Labels"
            ],
            vec![
                "nvl5-gp1-jhb01",
                "gb200_nvl72r1_c2g4",
                "rack-01, rack-02",
                "Switch / NVIDIA / switch-01",
                "nvl5-gp1-jhb01",
                "test group",
                "\"location.datacenter:JHB01\""
            ],
            vec!["empty-group", "empty-topology", "", "", "", "", ""],
        ]
    );

    let (result, output, calls) = dispatch(
        &["show", "nvl5-gp1-jhb01"],
        OutputFormat::Json,
        "GetExpectedRackGroup",
        populated_group().encode_to_vec(),
    )
    .await;
    result.unwrap();
    assert_eq!(
        forge::ExpectedRackGroupRequest::decode(calls[0].as_slice())
            .unwrap()
            .rack_group_id,
        "nvl5-gp1-jhb01"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output).unwrap(),
        serde_json::to_value(populated_group()).unwrap()
    );

    let (result, output, _) = dispatch(
        &["show"],
        OutputFormat::AsciiTable,
        "FindExpectedRackGroupsByIds",
        forge::ExpectedRackGroupList::default().encode_to_vec(),
    )
    .await;
    result.unwrap();
    assert_eq!(
        output.lines().filter(|line| line.starts_with('|')).count(),
        1
    );
    assert!(output.contains("Rack Group ID"));
}

#[tokio::test]
async fn show_renders_yaml_and_csv() {
    let mut group = populated_group();
    group.metadata.as_mut().unwrap().description = "quoted \"value\",\nsecond line".into();
    let empty = forge::ExpectedRackGroup {
        rack_group_id: Some("empty-group".parse().unwrap()),
        topology: "empty-topology".into(),
        ..Default::default()
    };
    let groups = forge::ExpectedRackGroupList {
        expected_rack_groups: vec![group.clone(), empty],
    };
    for (args, method, payload, expected) in [
        (
            vec!["show", "nvl5-gp1-jhb01"],
            "GetExpectedRackGroup",
            group.encode_to_vec(),
            serde_json::to_value(&group).unwrap(),
        ),
        (
            vec!["show"],
            "FindExpectedRackGroupsByIds",
            groups.encode_to_vec(),
            serde_json::to_value(&groups).unwrap(),
        ),
        (
            vec!["show"],
            "FindExpectedRackGroupsByIds",
            forge::ExpectedRackGroupList::default().encode_to_vec(),
            serde_json::to_value(forge::ExpectedRackGroupList::default()).unwrap(),
        ),
    ] {
        let (result, output, _) = dispatch(&args, OutputFormat::Yaml, method, payload).await;
        result.unwrap();
        assert_eq!(
            serde_yaml::from_str::<serde_json::Value>(&output).unwrap(),
            expected
        );
    }
    for inventory in [groups, forge::ExpectedRackGroupList::default()] {
        let (result, output, _) = dispatch(
            &["show"],
            OutputFormat::Csv,
            "FindExpectedRackGroupsByIds",
            inventory.encode_to_vec(),
        )
        .await;
        result.unwrap();
        let mut reader = csv::Reader::from_reader(output.as_bytes());
        assert_eq!(
            reader.headers().unwrap(),
            &csv::StringRecord::from(vec![
                "Rack Group ID",
                "Topology",
                "Rack IDs",
                "Members",
                "Name",
                "Description",
                "Labels",
            ])
        );
        let rows = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(rows.len(), inventory.expected_rack_groups.len());
        if !rows.is_empty() {
            assert_eq!(
                rows[0],
                csv::StringRecord::from(vec![
                    "nvl5-gp1-jhb01",
                    "gb200_nvl72r1_c2g4",
                    "rack-01, rack-02",
                    "Switch / NVIDIA / switch-01",
                    "nvl5-gp1-jhb01",
                    "quoted \"value\",\nsecond line",
                    "\"location.datacenter:JHB01\"",
                ])
            );
            assert_eq!(
                rows[1],
                csv::StringRecord::from(vec!["empty-group", "empty-topology", "", "", "", "", ""])
            );
        }
    }
}

#[tokio::test]
async fn json_export_round_trips_and_count_guard_prevents_replace() {
    let groups = forge::ExpectedRackGroupList {
        expected_rack_groups: vec![populated_group()],
    };
    let (result, output, _) = dispatch(
        &["show"],
        OutputFormat::Json,
        "FindExpectedRackGroupsByIds",
        groups.encode_to_vec(),
    )
    .await;
    result.unwrap();
    let path =
        std::env::temp_dir().join(format!("expected-rack-group-{}.json", uuid::Uuid::new_v4()));
    for (json, expected) in [
        (output, Some(groups)),
        (
            r#"{"expected_rack_groups":[],"expected_rack_groups_count":0}"#.into(),
            Some(forge::ExpectedRackGroupList::default()),
        ),
        (
            r#"{"expected_rack_groups":[],"expected_rack_groups_count":1}"#.into(),
            None,
        ),
        (
            r#"{"expected_rack_groups":[{"rack_group_id":"group","topology":"topology","rack_id":["rack-01"]}]}"#.into(),
            None,
        ),
        (
            r#"{"expected_rack_groups":[],"expected_rack_group_count":1}"#.into(),
            None,
        ),
    ] {
        std::fs::write(&path, json).unwrap();
        let (result, _, calls) = dispatch(
            &["replace-all", "--filename", path.to_str().unwrap()],
            OutputFormat::Json,
            "ReplaceAllExpectedRackGroups",
            vec![],
        )
        .await;
        match expected {
            Some(expected) => {
                result.unwrap();
                assert_eq!(calls.len(), 1);
                assert_eq!(
                    forge::ExpectedRackGroupList::decode(calls[0].as_slice()).unwrap(),
                    expected
                );
            }
            None => {
                assert!(result.is_err());
                assert!(calls.is_empty());
            }
        }
    }
    std::fs::remove_file(path).unwrap();
}

async fn dispatch(
    args: &[&str],
    format: OutputFormat,
    method: &'static str,
    response: Vec<u8>,
) -> (CarbideCliResult<()>, String, Vec<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let server_calls = calls.clone();
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.unwrap();
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request: Request<Incoming>| {
                    let calls = server_calls.clone();
                    let response = response.clone();
                    async move {
                        let response = if request.uri().path() == "/forge.Forge/Version" {
                            forge::BuildInfo::default().encode_to_vec()
                        } else if request.uri().path() == "/forge.Forge/FindExpectedRackGroupIds" {
                            let groups =
                                forge::ExpectedRackGroupList::decode(response.as_slice()).unwrap();
                            forge::ExpectedRackGroupIdList {
                                rack_group_ids: groups
                                    .expected_rack_groups
                                    .into_iter()
                                    .map(|g| g.rack_group_id.unwrap())
                                    .collect(),
                            }
                            .encode_to_vec()
                        } else {
                            assert_eq!(request.uri().path(), format!("/forge.Forge/{method}"));
                            let body = request.into_body().collect().await.unwrap().to_bytes();
                            assert_eq!(body.first(), Some(&0));
                            calls.lock().unwrap().push(body[5..].to_vec());
                            if method == "FindExpectedRackGroupsByIds" {
                                let ids = forge::ExpectedRackGroupsByIdsRequest::decode(&body[5..])
                                    .unwrap()
                                    .rack_group_ids;
                                let mut groups =
                                    forge::ExpectedRackGroupList::decode(response.as_slice())
                                        .unwrap();
                                groups
                                    .expected_rack_groups
                                    .retain(|g| ids.contains(g.rack_group_id.as_ref().unwrap()));
                                groups.encode_to_vec()
                            } else {
                                response
                            }
                        };
                        Ok::<_, Infallible>(grpc_response(response))
                    }
                }),
            )
            .await
            .unwrap();
    });
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
            format,
            request_timeout: client_config.request_timeout,
            page_size: 1,
            extended: false,
            cloud_unsafe_op: None,
            sort_by: SortField::PrimaryId,
        },
        output_file: std::mem::replace(captured.writer(), Box::new(tokio::io::sink())),
    };
    let result = AssertUnwindSafe(tokio::time::timeout(
        Duration::from_secs(5),
        parse(args).unwrap().dispatch(ctx),
    ))
    .catch_unwind()
    .await;
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "{error}");
    }
    let result = result
        .expect("dispatch did not panic")
        .expect("dispatch completes");
    let output = String::from_utf8(captured.into_bytes().await).unwrap();
    let calls = calls.lock().unwrap().clone();
    (result, output, calls)
}

fn grpc_response(encoded: Vec<u8>) -> Response<UnsyncBoxBody<Bytes, Infallible>> {
    let mut data = vec![0];
    data.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
    data.extend(encoded);
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
