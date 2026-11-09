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
use futures::FutureExt;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use rpc::admin_cli::OutputFormat;
use rpc::forge::{
    BuildInfo, NamedRoutingProfile, NamedRoutingProfileConfig, NamedRoutingProfileList,
    RoutingProfileNameList, RoutingProfileSearchFilter, RoutingProfilesByNamesRequest,
};
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
use crate::test_support::grpc_response;

#[derive(Debug, PartialEq)]
enum Call {
    List(RoutingProfileSearchFilter),
    Fetch(Vec<String>),
}

fn parse(args: &[&str]) -> Result<Cmd, clap::Error> {
    let opts = CliOptions::try_parse_from(
        ["nico-admin-cli", "routing-profile"]
            .into_iter()
            .chain(args.iter().copied()),
    )?;
    let Some(CliCommand::RoutingProfile(command)) = opts.commands else {
        panic!("expected routing-profile command");
    };
    Ok(command)
}

#[test]
fn help_omits_unused_output_flags() {
    use carbide_test_support::{Check, check_values};

    check_values(
        ["", "show"].map(|subcommand| Check {
            scenario: subcommand,
            input: subcommand,
            expect: (clap::error::ErrorKind::DisplayHelp, false, false),
        }),
        |subcommand| {
            let mut args = vec!["nico-admin-cli", "routing-profile"];
            if !subcommand.is_empty() {
                args.push(subcommand);
            }
            args.push("--help");
            let error = CliOptions::try_parse_from(args).unwrap_err();
            let help = error.to_string();
            (
                error.kind(),
                help.contains("--extended"),
                help.contains("--sort-by"),
            )
        },
    );
    // Hiding inherited options must not break parsing existing invocations.
    assert!(parse(&["show", "--extended", "--sort-by", "state"]).is_ok());
    let other_help = CliOptions::try_parse_from(["nico-admin-cli", "machine", "show", "--help"])
        .unwrap_err()
        .to_string();
    assert!(other_help.contains("--extended"));
    assert!(other_help.contains("--sort-by"));
}

fn fixture_profiles() -> Vec<NamedRoutingProfile> {
    vec![
        NamedRoutingProfile {
            name: "EXPLICIT".into(),
            config: Some(NamedRoutingProfileConfig {
                internal: Some(false),
                access_tier: Some(0),
                route_target_imports: Some(rpc::common::RouteTargets { values: vec![] }),
                ..Default::default()
            }),
        },
        NamedRoutingProfile {
            name: "site/custom".into(),
            config: Some(NamedRoutingProfileConfig::default()),
        },
    ]
}

async fn dispatch_routing_profile_request(
    args: &[&str],
    format: OutputFormat,
    profiles: Vec<NamedRoutingProfile>,
) -> (CarbideCliResult<()>, String, Vec<Call>) {
    let command = parse(args).unwrap();
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
            format,
            request_timeout: client_config.request_timeout,
            page_size: 25,
            extended: false,
            cloud_unsafe_op: None,
            sort_by: SortField::PrimaryId,
        },
        output_file: std::mem::replace(captured.writer(), Box::new(tokio::io::sink())),
    };
    let calls = Arc::new(Mutex::new(Vec::new()));
    let server_calls = calls.clone();
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.unwrap();
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request| {
                    handle_mock_routing_profile_response(
                        request,
                        profiles.clone(),
                        server_calls.clone(),
                    )
                }),
            )
            .await
            .expect("mock serves the routing profile connection");
    });
    let result = AssertUnwindSafe(tokio::time::timeout(
        Duration::from_secs(5),
        command.dispatch(ctx),
    ))
    .catch_unwind()
    .await;
    // Join even if dispatch panics so no listener survives a failed test.
    server.abort();
    if let Err(error) = server.await {
        assert!(
            error.is_cancelled(),
            "mock routing profile server failed: {error}"
        );
    }
    let result = result
        .expect("routing profile dispatch did not panic")
        .expect("routing profile dispatch finishes within five seconds");
    let output = String::from_utf8(captured.into_bytes().await).unwrap();
    let calls = std::mem::take(&mut *calls.lock().unwrap());
    (result, output, calls)
}

async fn handle_mock_routing_profile_response(
    request: Request<Incoming>,
    profiles: Vec<NamedRoutingProfile>,
    calls: Arc<Mutex<Vec<Call>>>,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
    let path = request.uri().path().to_string();
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let payload = body.get(5..).unwrap();
    Ok(match path.as_str() {
        "/forge.Forge/Version" => grpc_response(BuildInfo {
            runtime_config: Some(rpc::forge::RuntimeConfig {
                max_find_by_ids: 1,
                ..Default::default()
            }),
            ..Default::default()
        }),
        "/forge.Forge/FindRoutingProfileNames" => {
            let filter = RoutingProfileSearchFilter::decode(payload).unwrap();
            calls.lock().unwrap().push(Call::List(filter));
            grpc_response(RoutingProfileNameList {
                names: profiles.into_iter().map(|profile| profile.name).collect(),
            })
        }
        "/forge.Forge/FindRoutingProfilesByNames" => {
            let request = RoutingProfilesByNamesRequest::decode(payload).unwrap();
            assert_eq!(request.names.len(), 1, "must honor server's batch limit");
            calls
                .lock()
                .unwrap()
                .push(Call::Fetch(request.names.clone()));
            grpc_response(NamedRoutingProfileList {
                profiles: profiles
                    .into_iter()
                    .filter(|profile| request.names.contains(&profile.name))
                    .collect(),
            })
        }
        other => panic!("unexpected RPC: {other}"),
    })
}

#[tokio::test]
async fn show_forwards_filters_and_fetches_discovered_profiles() {
    for (args, filter) in [
        (
            vec!["show", "--minimum-access-tier", "1"],
            RoutingProfileSearchFilter {
                minimum_access_tier: Some(1),
                ..Default::default()
            },
        ),
        (
            vec![
                "show",
                "--internal",
                "false",
                "--minimum-access-tier",
                "4294967295",
            ],
            RoutingProfileSearchFilter {
                internal: Some(false),
                minimum_access_tier: Some(u32::MAX),
            },
        ),
    ] {
        let (result, output, calls) = dispatch_routing_profile_request(
            &args,
            OutputFormat::Json,
            fixture_profiles()[..1].to_vec(),
        )
        .await;
        result.unwrap();
        let profiles: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(profiles.as_array().unwrap().len(), 1);
        assert_eq!(profiles[0]["name"], "EXPLICIT");
        assert_eq!(profiles[0]["config"]["internal"], false);
        assert_eq!(
            calls,
            [Call::List(filter), Call::Fetch(vec!["EXPLICIT".into()])],
            "{args:?}"
        );
    }
}

#[tokio::test]
async fn show_lists_then_batches_and_preserves_configured_presence() {
    let (result, output, calls) =
        dispatch_routing_profile_request(&["show"], OutputFormat::Json, fixture_profiles()).await;
    result.unwrap();
    assert_eq!(
        calls,
        [
            Call::List(RoutingProfileSearchFilter::default()),
            Call::Fetch(vec!["EXPLICIT".into()]),
            Call::Fetch(vec!["site/custom".into()]),
        ]
    );
    let profiles: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(profiles[0]["config"]["internal"], false);
    assert_eq!(profiles[0]["config"]["access_tier"], 0);
    assert_eq!(
        profiles[0]["config"]["route_target_imports"]["values"],
        serde_json::json!([])
    );
    assert!(profiles[1]["config"]["internal"].is_null());
    assert!(profiles[1]["config"]["route_target_imports"].is_null());
}

#[tokio::test]
async fn explicit_names_skip_discovery_and_missing_names_fail_without_output() {
    for (args, expected, missing) in [
        (
            vec![
                "show",
                "--name",
                "site/custom",
                "EXPLICIT",
                "--name",
                "site/custom",
            ],
            vec!["EXPLICIT", "site/custom"],
            None,
        ),
        (
            vec![
                "show",
                "--name",
                "EXPLICIT",
                "missing",
                "--internal",
                "true",
            ],
            vec!["EXPLICIT", "missing"],
            Some("missing"),
        ),
    ] {
        let (result, output, calls) =
            dispatch_routing_profile_request(&args, OutputFormat::Json, fixture_profiles()).await;
        assert_eq!(
            calls,
            expected
                .into_iter()
                .map(|name| Call::Fetch(vec![name.into()]))
                .collect::<Vec<_>>()
        );
        if let Some(missing) = missing {
            assert!(result.unwrap_err().to_string().contains(missing));
            assert!(output.is_empty());
        } else {
            result.unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&output)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
        }
    }
}

#[tokio::test]
async fn show_empty_filtered_inventory_does_not_send_empty_batch() {
    let (result, output, calls) = dispatch_routing_profile_request(
        &["show", "--internal", "true", "--minimum-access-tier", "1"],
        OutputFormat::Json,
        vec![],
    )
    .await;
    result.unwrap();
    assert_eq!(output.trim(), "[]");
    assert_eq!(
        calls,
        [Call::List(RoutingProfileSearchFilter {
            internal: Some(true),
            minimum_access_tier: Some(1),
        })]
    );
}

#[tokio::test]
async fn single_name_renders_all_properties_in_ascii_csv_and_yaml() {
    for name in ["EXPLICIT", "site/custom"] {
        for format in [
            OutputFormat::AsciiTable,
            OutputFormat::Csv,
            OutputFormat::Yaml,
        ] {
            let (result, output, _) = dispatch_routing_profile_request(
                &["show", "--name", name, name],
                format,
                fixture_profiles(),
            )
            .await;
            result.unwrap();
            assert!(output.contains(name));
            let expected = if name == "EXPLICIT" { "false" } else { "unset" };
            match format {
                OutputFormat::AsciiTable => {
                    assert!(!output.contains('|'));
                    assert_eq!(output.matches(name).count(), 1);
                    let fields: std::collections::BTreeMap<_, _> = output
                        .lines()
                        .map(|line| line.split_once(": ").unwrap())
                        .map(|(key, value)| (key.trim(), value))
                        .collect();
                    assert_eq!(fields["INTERNAL"], expected);
                    assert!(fields.contains_key("TENANT PREFIX OVERLAP ELIGIBLE"));
                }
                OutputFormat::Csv => {
                    assert!(output.starts_with("Name,Property,Configured value"));
                    assert!(output.contains(&format!("{name},internal,{expected}")));
                    assert!(output.contains("tenant_prefix_overlap_eligible"));
                }
                OutputFormat::Yaml => {
                    let value: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();
                    assert_eq!(value.as_sequence().unwrap().len(), 1);
                    assert_eq!(
                        value[0]["config"]["internal"].as_bool(),
                        if name == "EXPLICIT" {
                            Some(false)
                        } else {
                            None
                        }
                    );
                }
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn invalid_invocations_remain_clap_errors() {
    use carbide_test_support::{Check, check_values};
    use clap::error::ErrorKind::{
        InvalidSubcommand, InvalidValue, UnknownArgument, ValueValidation,
    };

    check_values(
        [
            ("removed list command", vec!["list"], InvalidSubcommand),
            (
                "invalid internal value",
                vec!["show", "--internal", "maybe"],
                InvalidValue,
            ),
            (
                "negative tier",
                vec!["show", "--minimum-access-tier", "-1"],
                UnknownArgument,
            ),
            (
                "tier above API maximum",
                vec!["show", "--minimum-access-tier", "4294967296"],
                ValueValidation,
            ),
            (
                "positional name is unsupported",
                vec!["show", "INTERNAL"],
                UnknownArgument,
            ),
            (
                "name requires a value",
                vec!["show", "--name"],
                InvalidValue,
            ),
        ]
        .map(|(scenario, input, kind)| Check {
            scenario,
            input,
            expect: (kind, 2),
        }),
        |args| {
            let error = parse(&args).unwrap_err();
            (error.kind(), error.exit_code())
        },
    );
}

#[tokio::test]
async fn named_profiles_apply_all_filters_with_configured_defaults() {
    for (filters, expected) in [
        (vec!["--internal", "false"], vec!["EXPLICIT", "site/custom"]),
        (vec!["--internal", "true"], vec!["INTERNAL"]),
        (
            vec!["--minimum-access-tier", "0"],
            vec!["EXPLICIT", "INTERNAL", "site/custom"],
        ),
        (
            vec!["--minimum-access-tier", "1"],
            vec!["EXPLICIT", "INTERNAL"],
        ),
        (
            vec!["--internal", "false", "--minimum-access-tier", "1"],
            vec!["EXPLICIT"],
        ),
        (
            vec!["--internal", "true", "--minimum-access-tier", "3"],
            vec![],
        ),
    ] {
        let mut profiles = fixture_profiles();
        profiles[0].config.as_mut().unwrap().access_tier = Some(3);
        profiles.push(NamedRoutingProfile {
            name: "INTERNAL".into(),
            config: Some(NamedRoutingProfileConfig {
                internal: Some(true),
                access_tier: Some(2),
                ..Default::default()
            }),
        });
        let mut args = vec!["show", "--name", "EXPLICIT", "INTERNAL", "site/custom"];
        args.extend(filters);
        let (result, output, calls) =
            dispatch_routing_profile_request(&args, OutputFormat::Json, profiles).await;
        result.unwrap();
        assert_eq!(
            calls,
            [
                Call::Fetch(vec!["EXPLICIT".into()]),
                Call::Fetch(vec!["INTERNAL".into()]),
                Call::Fetch(vec!["site/custom".into()]),
            ]
        );
        let values: serde_json::Value = serde_json::from_str(&output).unwrap();
        let names: Vec<_> = values
            .as_array()
            .unwrap()
            .iter()
            .map(|profile| profile["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, expected, "{args:?}");
    }
}

#[tokio::test]
async fn list_views_share_ascii_and_csv_rows() {
    let mut fixtures = fixture_profiles();
    let config = fixtures[0].config.as_mut().unwrap();
    config.access_tier = Some(1);
    config.leak_default_route_from_underlay = Some(true);
    config.leak_tenant_host_routes_to_underlay = Some(false);
    config.tenant_leak_communities_accepted = Some(true);
    config.tenant_prefix_overlap_eligible = true;
    for (args, profiles, expected) in [
        (
            vec!["show"],
            fixtures.clone(),
            vec!["EXPLICIT", "site/custom"],
        ),
        (vec!["show"], fixtures[..1].to_vec(), vec!["EXPLICIT"]),
        (vec!["show"], vec![], vec![]),
        (
            vec!["show", "--name", "EXPLICIT", "site/custom"],
            fixtures.clone(),
            vec!["EXPLICIT", "site/custom"],
        ),
        (
            vec![
                "show",
                "--name",
                "EXPLICIT",
                "site/custom",
                "--minimum-access-tier",
                "1",
            ],
            fixtures.clone(),
            vec!["EXPLICIT"],
        ),
    ] {
        for format in [OutputFormat::AsciiTable, OutputFormat::Csv] {
            let (result, output, _) =
                dispatch_routing_profile_request(&args, format, profiles.clone()).await;
            result.unwrap();
            let rows: Vec<Vec<_>> = if format == OutputFormat::AsciiTable {
                output
                    .lines()
                    .filter(|line| line.starts_with('|'))
                    .map(|line| line.trim_matches('|').split('|').map(str::trim).collect())
                    .collect()
            } else {
                output
                    .lines()
                    .map(|line| line.split(',').collect())
                    .collect()
            };
            assert_eq!(
                rows[0],
                [
                    "Name",
                    "Internal",
                    "Access tier",
                    "Leak default from underlay",
                    "Leak hosts to underlay"
                ]
            );
            assert_eq!(rows.len(), expected.len() + 1, "{args:?}");
            for (row, name) in rows[1..].iter().zip(expected.iter()) {
                let fields = if *name == "EXPLICIT" {
                    vec!["EXPLICIT", "false", "1", "true", "false"]
                } else {
                    vec!["site/custom", "unset", "unset", "unset", "unset"]
                };
                assert_eq!(*row, fields, "{args:?}");
            }
        }
    }
}
