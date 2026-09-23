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

use std::cell::RefCell;
use std::convert::Infallible;
use std::panic::AssertUnwindSafe;

use carbide_test_support::Outcome::{Fails, FailsWith, Yields};
use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};
use clap::Parser;
use clap::error::ErrorKind;
use futures::{FutureExt, stream};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rpc::forge::{
    BuildInfo, Vpc, VpcConfig, VpcRetainedVniAllocation, VpcRoutingStateRequest, VpcStatus,
};
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::net::TcpListener;

use super::*;
use crate::async_write::CapturedOutput;
use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
use crate::cfg::dispatch::Dispatch;
use crate::cfg::runtime::RuntimeConfig;
use crate::errors::CarbideCliError;
use crate::vpc::Cmd;

const ID: &str = "12345678-1234-5678-90ab-cdef01234567";
const VERSION: &str = "V7-T1789080000000000";
const NEXT_VERSION: &str = "V8-T1789080000000001";
const CHANGE: &[&str] = &[
    "change-routing-profile",
    ID,
    "site/external",
    "--if-version-match",
    VERSION,
];
const RELEASE: &[&str] = &[
    "release-inactive-vni",
    ID,
    "--if-version-match",
    NEXT_VERSION,
    "--expected-inactive-vni",
    "4000",
    "--confirm-convergence",
];
const INTERACTIVE_RELEASE: &[&str] = &[
    "release-inactive-vni",
    ID,
    "--expected-inactive-vni",
    "4000",
    "--confirm-convergence",
];

fn parse(args: &[&str]) -> Result<Cmd, clap::Error> {
    let argv: Vec<_> = ["nico-admin-cli", "vpc"]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    let options = CliOptions::try_parse_from(argv)?;
    let Some(CliCommand::Vpc(command)) = options.commands else {
        panic!("expected the public VPC command path");
    };
    Ok(command)
}

async fn execute(
    args: &[&str],
    client: &RecordingClient,
    format: OutputFormat,
) -> (eyre::Result<()>, Vec<u8>) {
    execute_with_confirmation(args, client, format, async |_| {
        panic!("explicit-version commands must not prompt")
    })
    .await
}

async fn execute_with_confirmation(
    args: &[&str],
    client: &RecordingClient,
    format: OutputFormat,
    confirm: impl AsyncFnOnce(&str) -> eyre::Result<()>,
) -> (eyre::Result<()>, Vec<u8>) {
    let mut captured = CapturedOutput::new();
    let output = captured.writer();
    let result = match parse(args).unwrap() {
        Cmd::RoutingState(args) => args.execute(client, None, format, output).await,
        Cmd::ChangeRoutingProfile(args) => {
            args.execute(client, None, format, output, confirm).await
        }
        Cmd::ReleaseInactiveVni(args) => args.execute(client, None, format, output, confirm).await,
        _ => panic!("expected a routing command"),
    };
    (result, captured.into_bytes().await)
}

async fn dispatch_routing_state(state: VpcRoutingState) -> Vec<u8> {
    let command = parse(&["routing-state", ID]).expect("routing-state command parses");
    assert!(matches!(command, Cmd::RoutingState(_)));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock routing listener binds");
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

    // The normal client probes `Version` before reading the routing state.
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.expect("mock accepts a client");
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request| mock_routing_request(request, state.clone())),
            )
            .await
            .expect("mock serves the routing connection");
    });
    let result = AssertUnwindSafe(tokio::time::timeout(request_timeout, command.dispatch(ctx)))
        .catch_unwind()
        .await;
    // Join before asserting so failed or stalled dispatch cannot leave the listener running.
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "mock routing server failed: {error}");
    }
    result
        .expect("routing-state dispatch did not panic")
        .expect("routing-state dispatch completes within five seconds")
        .expect("routing-state dispatch succeeds");
    captured.into_bytes().await
}

async fn mock_routing_request(
    request: hyper::Request<Incoming>,
    state: VpcRoutingState,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
    Ok(match request.uri().path() {
        "/forge.Forge/Version" => grpc_response(BuildInfo::default()),
        "/forge.Forge/GetVpcRoutingState" => {
            let body = request
                .into_body()
                .collect()
                .await
                .expect("routing-state request body is readable")
                .to_bytes();
            let payload = body.get(5..).expect("routing-state has a gRPC frame");
            let request =
                VpcRoutingStateRequest::decode(payload).expect("routing-state request decodes");
            assert_eq!(request.id, Some(ID.parse().unwrap()));
            grpc_response(state)
        }
        path => panic!("unexpected mock Forge method: {path}"),
    })
}

fn grpc_response(message: impl prost::Message) -> Response<UnsyncBoxBody<Bytes, Infallible>> {
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

fn initial_state() -> VpcRoutingState {
    VpcRoutingState {
        id: Some(ID.parse().unwrap()),
        version: VERSION.into(),
        routing_profile_type: Some("INTERNAL".into()),
        active_vni: 4000,
        retained_allocation: None,
    }
}

fn changed_state() -> VpcRoutingState {
    VpcRoutingState {
        version: NEXT_VERSION.into(),
        routing_profile_type: Some("site/external".into()),
        active_vni: 7000,
        retained_allocation: Some(VpcRetainedVniAllocation {
            pool_name: VPC_VNI.into(),
            vni: 4000,
        }),
        ..initial_state()
    }
}

fn released_state() -> VpcReleaseInactiveVniResult {
    VpcReleaseInactiveVniResult {
        vpc: Some(Vpc {
            id: Some(ID.parse().unwrap()),
            version: "V9-T1789080000000002".into(),
            status: Some(VpcStatus {
                vni: Some(7000),
                ..Default::default()
            }),
            config: Some(VpcConfig {
                routing_profile_type: Some("site/external".into()),
                vni: Some(1234),
                ..Default::default()
            }),
            ..Default::default()
        }),
        released_inactive_vni: 4000,
    }
}

#[derive(Debug, PartialEq)]
enum Request {
    Read(VpcId),
    Change(VpcChangeRoutingProfileRequest),
    Release(VpcReleaseInactiveVniRequest),
}

#[derive(Default)]
struct RecordingClient {
    state: VpcRoutingState,
    read_error: Option<Status>,
    change: Option<Result<VpcRoutingState, Status>>,
    release: Option<Result<VpcReleaseInactiveVniResult, Status>>,
    requests: RefCell<Vec<Request>>,
}

impl RoutingClient for RecordingClient {
    async fn routing_state(&self, id: VpcId) -> Result<VpcRoutingState, Status> {
        self.requests.borrow_mut().push(Request::Read(id));
        if let Some(error) = &self.read_error {
            return Err(error.clone());
        }
        Ok(self.state.clone())
    }

    async fn change_profile(
        &self,
        request: VpcChangeRoutingProfileRequest,
    ) -> Result<VpcRoutingState, Status> {
        self.requests.borrow_mut().push(Request::Change(request));
        self.change
            .clone()
            .expect("unexpected routing-profile mutation")
    }

    async fn release(
        &self,
        request: VpcReleaseInactiveVniRequest,
    ) -> Result<VpcReleaseInactiveVniResult, Status> {
        self.requests.borrow_mut().push(Request::Release(request));
        self.release
            .clone()
            .expect("unexpected inactive-VNI mutation")
    }
}

#[test]
fn only_mutations_without_explicit_versions_require_interactive_confirmation() {
    value_scenarios!(run = |args: &[&str]| parse(args).unwrap().requires_interactive_confirmation();
        "scripted mutations" {
            CHANGE => false,
            RELEASE => false,
        }
        "interactive mutations" {
            &CHANGE[..3] => true,
            INTERACTIVE_RELEASE => true,
        }
        "inspection" {
            &["routing-state", ID][..] => false,
        }
    );
}

#[test]
fn clap_validates_explicit_inputs_and_requires_convergence_acknowledgement() {
    scenarios!(run = |args: Vec<&str>| parse(&args).map(drop).map_err(|error| error.kind());
        "change requires a nonempty profile and validates an explicit version" {
            vec![CHANGE[0], ID, "", "--if-version-match", VERSION] => FailsWith(ErrorKind::InvalidValue),
            vec![CHANGE[0], ID, "CUSTOM", "--if-version-match", "invalid"] => FailsWith(ErrorKind::ValueValidation),
        }
        "exact VNI stays within the transition range" {
            [CHANGE, &["--vni", "0"]].concat() => FailsWith(ErrorKind::ValueValidation),
            [CHANGE, &["--vni", "16777216"]].concat() => FailsWith(ErrorKind::ValueValidation),
        }
        "release requires each independently approved input" {
            vec![RELEASE[0], ID, "--if-version-match", NEXT_VERSION, "--confirm-convergence"] => FailsWith(ErrorKind::MissingRequiredArgument),
            RELEASE[..6].to_vec() => FailsWith(ErrorKind::MissingRequiredArgument),
            vec![RELEASE[0], ID, "--if-version-match", NEXT_VERSION, "--expected-inactive-vni", "0", "--confirm-convergence"] => FailsWith(ErrorKind::ValueValidation),
        }
    );
}

#[tokio::test]
async fn routing_state_command_renders_populated_and_absent_fields_without_losing_json() {
    let unnamed = VpcRoutingState {
        routing_profile_type: None,
        active_vni: 0,
        ..initial_state()
    };
    for (state, values) in [
        (
            changed_state(),
            [ID, NEXT_VERSION, "site/external", "7000", VPC_VNI, "4000"],
        ),
        (unnamed, [ID, VERSION, "", "0", "", ""]),
    ] {
        let client = RecordingClient {
            state,
            ..Default::default()
        };
        let output = dispatch_routing_state(client.state.clone()).await;
        let display = String::from_utf8(output).unwrap();
        let rows: Vec<Vec<_>> = display
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| line.trim_matches('|').split('|').map(str::trim).collect())
            .collect();
        let expected: Vec<_> = [
            "VPC ID",
            "Version",
            "Routing Profile",
            "Active VNI",
            "Retained Pool",
            "Retained VNI",
        ]
        .into_iter()
        .zip(values)
        .map(|(field, value)| vec![field, value])
        .collect();
        assert_eq!(rows[0], ["Field", "Value"]);
        assert_eq!(rows[1..], expected);

        let (result, output) = execute(&["routing-state", ID], &client, OutputFormat::Json).await;
        result.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            serde_json::to_value(&client.state).unwrap()
        );
        assert_eq!(
            *client.requests.borrow(),
            [Request::Read(ID.parse().unwrap())]
        );
    }
}

#[tokio::test]
async fn change_command_preserves_explicit_or_confirmed_versions_and_vni_selection() {
    struct ChangeCase {
        scenario: &'static str,
        explicit_version: Option<&'static str>,
        selection: Option<u32>,
        profile: &'static str,
        before: VpcRoutingState,
        after: VpcRoutingState,
    }

    let reverse_before = VpcRoutingState {
        version: VERSION.into(),
        ..changed_state()
    };
    let reverse_after = VpcRoutingState {
        version: NEXT_VERSION.into(),
        retained_allocation: Some(VpcRetainedVniAllocation {
            pool_name: EXTERNAL_VPC_VNI.into(),
            vni: 7000,
        }),
        ..initial_state()
    };
    for ChangeCase {
        scenario,
        explicit_version,
        selection,
        profile,
        before,
        after,
    } in [
        ChangeCase {
            scenario: "explicit version with automatic VNI",
            explicit_version: Some(VERSION),
            selection: None,
            profile: "site/external",
            before: initial_state(),
            after: changed_state(),
        },
        ChangeCase {
            scenario: "explicit version with exact VNI",
            explicit_version: Some(VERSION),
            selection: Some(16_777_215),
            profile: "site/external",
            before: initial_state(),
            after: VpcRoutingState {
                active_vni: 16_777_215,
                ..changed_state()
            },
        },
        ChangeCase {
            scenario: "explicit version with retained reversal",
            explicit_version: Some(VERSION),
            selection: None,
            profile: "INTERNAL",
            before: reverse_before,
            after: reverse_after,
        },
        ChangeCase {
            scenario: "confirmed version with automatic VNI",
            explicit_version: None,
            selection: None,
            profile: "site/external",
            before: initial_state(),
            after: changed_state(),
        },
        ChangeCase {
            scenario: "confirmed version with exact VNI",
            explicit_version: None,
            selection: Some(7000),
            profile: "site/external",
            before: initial_state(),
            after: changed_state(),
        },
    ] {
        let selected_vni = selection.map(|vni| vni.to_string());
        let mut args = vec![CHANGE[0], ID, profile];
        if let Some(version) = explicit_version {
            args.extend(["--if-version-match", version]);
        }
        if let Some(vni) = &selected_vni {
            args.extend(["--vni", vni]);
        }
        let client = RecordingClient {
            state: before,
            change: Some(Ok(after.clone())),
            ..Default::default()
        };
        let mut confirmed = false;
        let (result, output) =
            execute_with_confirmation(&args, &client, OutputFormat::Json, async |prompt| {
                assert!(
                    explicit_version.is_none(),
                    "{scenario}: explicit-version command prompted"
                );
                assert!(
                    prompt.contains(&serde_json::to_string_pretty(&client.state).unwrap()),
                    "{scenario}: observed state missing from prompt"
                );
                assert!(
                    prompt.contains(&format!("--if-version-match {VERSION}")),
                    "{scenario}: original version missing from prompt"
                );
                assert!(
                    prompt.contains(profile),
                    "{scenario}: profile missing from prompt"
                );
                if let Some(vni) = &selected_vni {
                    assert!(
                        prompt.contains(vni),
                        "{scenario}: exact VNI missing from prompt"
                    );
                } else {
                    assert!(
                        prompt.contains("automatic"),
                        "{scenario}: automatic VNI selection missing from prompt"
                    );
                }
                tokio::task::yield_now().await;
                confirmed = true;
                Ok(())
            })
            .await;
        result.unwrap_or_else(|error| panic!("{scenario}: {error:#}"));
        assert_eq!(confirmed, explicit_version.is_none(), "{scenario}");
        assert_eq!(
            *client.requests.borrow(),
            [
                Request::Read(ID.parse().unwrap()),
                Request::Change(VpcChangeRoutingProfileRequest {
                    id: Some(ID.parse().unwrap()),
                    if_version_match: Some(VERSION.into()),
                    routing_profile_type: profile.into(),
                    vni: selection
                })
            ],
            "{scenario}"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            serde_json::to_value(after).unwrap(),
            "{scenario}"
        );
    }
}

#[tokio::test]
async fn release_command_preserves_exact_identity_and_reports_cleanup() {
    for args in [RELEASE, INTERACTIVE_RELEASE] {
        let client = RecordingClient {
            state: changed_state(),
            release: Some(Ok(released_state())),
            ..Default::default()
        };
        let mut confirmed = false;
        let (result, output) =
            execute_with_confirmation(args, &client, OutputFormat::Json, async |prompt| {
                assert_eq!(
                    args, INTERACTIVE_RELEASE,
                    "explicit-version command prompted"
                );
                assert!(prompt.contains(&serde_json::to_string_pretty(&client.state).unwrap()));
                assert!(prompt.contains(&format!("--if-version-match {NEXT_VERSION}")));
                assert!(prompt.contains("no longer use"));
                assert!(prompt.contains("DPUs"));
                assert!(prompt.contains("fabric"));
                assert!(prompt.contains("4000"));
                tokio::task::yield_now().await;
                confirmed = true;
                Ok(())
            })
            .await;
        result.unwrap();
        assert_eq!(confirmed, args == INTERACTIVE_RELEASE);
        assert_eq!(
            *client.requests.borrow(),
            [
                Request::Read(ID.parse().unwrap()),
                Request::Release(VpcReleaseInactiveVniRequest {
                    id: Some(ID.parse().unwrap()),
                    if_version_match: Some(NEXT_VERSION.into()),
                    expected_inactive_vni: Some(4000)
                })
            ]
        );
        let mut expected = serde_json::to_value(VpcRoutingState {
            version: "V9-T1789080000000002".into(),
            retained_allocation: None,
            ..changed_state()
        })
        .unwrap();
        expected["released_inactive_vni"] = 4000.into();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn failed_routing_reads_stop_before_mutation() {
    for (client, advice) in [
        (
            RecordingClient {
                state: VpcRoutingState {
                    id: Some("abcdef01-2345-6789-abcd-ef0123456789".parse().unwrap()),
                    ..initial_state()
                },
                ..Default::default()
            },
            "core returned a missing or different VPC identity",
        ),
        (
            RecordingClient {
                state: initial_state(),
                read_error: Some(Status::unimplemented("injected unsupported read")),
                ..Default::default()
            },
            "core does not support GetVpcRoutingState",
        ),
    ] {
        let (result, output) = execute(CHANGE, &client, OutputFormat::Json).await;
        let error = result.unwrap_err();
        assert!(error.to_string().contains(advice), "{advice}: {error:#}");
        if let Some(expected) = &client.read_error {
            let cause = error
                .downcast_ref::<Status>()
                .expect("tonic cause is preserved");
            assert_eq!(cause.code(), expected.code());
            assert_eq!(cause.message(), expected.message());
            assert!(error.to_string().contains(ID), "{error:#}");
        }
        assert!(output.is_empty(), "{advice}");
        assert_eq!(
            *client.requests.borrow(),
            [Request::Read(ID.parse().unwrap())],
            "{advice}"
        );
    }
}

#[tokio::test]
async fn mutation_failures_preserve_the_cause_and_never_retry() {
    for (args, code, advice) in [
        (CHANGE, Code::Unavailable, "may have committed"),
        (CHANGE, Code::Unimplemented, "does not support"),
        (
            CHANGE,
            Code::FailedPrecondition,
            "stale version does not prove",
        ),
        (RELEASE, Code::Unavailable, "may have committed"),
        (
            &CHANGE[..3],
            Code::FailedPrecondition,
            "stale version does not prove",
        ),
        (INTERACTIVE_RELEASE, Code::Unavailable, "may have committed"),
    ] {
        let client = RecordingClient {
            state: if args[0] == RELEASE[0] {
                changed_state()
            } else {
                initial_state()
            },
            change: Some(Err(Status::new(code, "injected failure"))),
            release: Some(Err(Status::new(code, "injected failure"))),
            ..Default::default()
        };
        let mut confirmed = false;
        let (result, output) =
            execute_with_confirmation(args, &client, OutputFormat::Json, async |_| {
                assert!(!args.contains(&"--if-version-match"));
                confirmed = true;
                Ok(())
            })
            .await;
        let error = result.unwrap_err();
        assert!(error.to_string().contains(advice), "{error:#}");
        assert!(
            error
                .to_string()
                .contains(&format!("vpc routing-state {ID}"))
        );
        assert_eq!(error.downcast_ref::<Status>().unwrap().code(), code);
        assert_eq!(client.requests.borrow().len(), 2);
        assert_eq!(confirmed, !args.contains(&"--if-version-match"));
        match &client.requests.borrow()[1] {
            Request::Change(request) => {
                assert_eq!(request.if_version_match.as_deref(), Some(VERSION))
            }
            Request::Release(request) => {
                assert_eq!(request.if_version_match.as_deref(), Some(NEXT_VERSION))
            }
            Request::Read(_) => panic!("mutation must use the first observed version"),
        }
        assert!(output.is_empty());
    }
}

#[tokio::test]
async fn confirmation_failures_stop_before_mutation() {
    for (args, message) in [
        (&CHANGE[..3], "operation cancelled"),
        (INTERACTIVE_RELEASE, "confirmation input unavailable"),
    ] {
        let client = RecordingClient {
            state: if args[0] == RELEASE[0] {
                changed_state()
            } else {
                initial_state()
            },
            ..Default::default()
        };
        let mut prompted = false;
        let (result, output) =
            execute_with_confirmation(args, &client, OutputFormat::Json, async |_| {
                prompted = true;
                Err(eyre::eyre!(message))
            })
            .await;
        assert!(prompted);
        assert!(result.unwrap_err().to_string().contains(message));
        assert_eq!(
            *client.requests.borrow(),
            [Request::Read(ID.parse().unwrap())]
        );
        assert!(output.is_empty());
    }
}

#[test]
fn confirmation_requires_yes_and_preserves_output_failures() {
    scenarios!(run = |answer: &str| {
        let mut input = answer.as_bytes();
        let mut output = Vec::new();
        let result = read_confirmation("Approve this allocation", &mut input, &mut output);
        assert_eq!(output, b"Approve this allocation\n\nType yes to proceed: ");
        result.map_err(drop)
    };
        "explicit approval" {
            "yes\n" => Yields(()),
            " yes \n" => Yields(()),
        }
        "rejection or closed input" {
            "no\n" => Fails,
            "" => Fails,
        }
    );

    let mut input = b"yes\n".as_slice();
    let mut bytes: [u8; 0] = [];
    let mut output = bytes.as_mut_slice();
    let error = read_confirmation("Approve this allocation", &mut input, &mut output).unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::WriteZero
    );
    assert_eq!(
        input, b"yes\n",
        "failed prompt output must not consume approval"
    );
}

#[tokio::test]
async fn invalid_acknowledgement_reports_possible_commit_without_retry_or_success_output() {
    let client = RecordingClient {
        state: initial_state(),
        change: Some(Ok(VpcRoutingState {
            active_vni: 7001,
            ..changed_state()
        })),
        ..Default::default()
    };
    let (result, output) = execute(
        &[CHANGE, &["--vni", "7000"]].concat(),
        &client,
        OutputFormat::Json,
    )
    .await;
    let error = result.unwrap_err();
    assert!(error.to_string().contains("may have committed"));
    assert!(
        error
            .to_string()
            .contains(&format!("vpc routing-state {ID}"))
    );
    assert_eq!(client.requests.borrow().len(), 2);
    assert!(output.is_empty());
}

#[tokio::test]
async fn mismatched_observations_stop_before_mutation() {
    for (args, state) in [
        (CHANGE, changed_state()),
        (
            RELEASE,
            VpcRoutingState {
                version: VERSION.into(),
                ..changed_state()
            },
        ),
        (
            RELEASE,
            VpcRoutingState {
                retained_allocation: Some(VpcRetainedVniAllocation {
                    pool_name: VPC_VNI.into(),
                    vni: 4001,
                }),
                ..changed_state()
            },
        ),
    ] {
        let client = RecordingClient {
            state,
            ..Default::default()
        };
        let (result, output) = execute(args, &client, OutputFormat::Json).await;
        assert!(result.is_err());
        assert_eq!(
            *client.requests.borrow(),
            [Request::Read(ID.parse().unwrap())]
        );
        assert!(output.is_empty());
    }
}

#[test]
fn change_acknowledgements_reject_malformed_or_mismatched_state() {
    let Cmd::ChangeRoutingProfile(command) = parse(&[CHANGE, &["--vni", "7000"]].concat()).unwrap()
    else {
        panic!("change command");
    };
    type Invalidate = fn(&mut VpcRoutingState);
    let cases: &[(&str, Invalidate)] = &[
        ("missing identity", |state| state.id = None),
        ("malformed version", |state| {
            state.version = "invalid".into()
        }),
        ("version not advanced", |state| {
            state.version = VERSION.into()
        }),
        ("different profile", |state| {
            state.routing_profile_type = Some("INTERNAL".into())
        }),
        ("missing retained allocation", |state| {
            state.retained_allocation = None
        }),
        ("unknown pool", |state| {
            state.retained_allocation.as_mut().unwrap().pool_name = "other-pool".into()
        }),
        ("wrong previous VNI", |state| {
            state.retained_allocation.as_mut().unwrap().vni = 4001
        }),
    ];
    check_cases(
        cases.iter().map(|(scenario, edit)| Case {
            scenario,
            input: edit,
            expect: Fails,
        }),
        |edit| {
            let mut after = changed_state();
            edit(&mut after);
            command
                .validate_result(&initial_state(), &after, VERSION.parse().unwrap())
                .map_err(drop)
        },
    );
}

#[test]
fn release_acknowledgements_require_the_exact_cleanup_and_unchanged_active_configuration() {
    let Cmd::ReleaseInactiveVni(command) = parse(RELEASE).unwrap() else {
        panic!("release command");
    };
    type Invalidate = fn(&mut VpcReleaseInactiveVniResult);
    let cases: &[(&str, Invalidate)] = &[
        ("missing VPC", |result| result.vpc = None),
        ("different VPC", |result| {
            result.vpc.as_mut().unwrap().id =
                Some("abcdef01-2345-6789-abcd-ef0123456789".parse().unwrap())
        }),
        ("version not advanced", |result| {
            result.vpc.as_mut().unwrap().version = NEXT_VERSION.into()
        }),
        ("different released VNI", |result| {
            result.released_inactive_vni = 4001
        }),
        ("missing status", |result| {
            result.vpc.as_mut().unwrap().status = None
        }),
        ("missing configuration", |result| {
            result.vpc.as_mut().unwrap().config = None
        }),
        ("active VNI changed", |result| {
            result.vpc.as_mut().unwrap().status.as_mut().unwrap().vni = Some(7001)
        }),
        ("profile changed", |result| {
            result
                .vpc
                .as_mut()
                .unwrap()
                .config
                .as_mut()
                .unwrap()
                .routing_profile_type = Some("INTERNAL".into())
        }),
    ];
    check_cases(
        cases.iter().map(|(scenario, edit)| Case {
            scenario,
            input: edit,
            expect: Fails,
        }),
        |edit| {
            let mut result = released_state();
            edit(&mut result);
            command
                .validate_result(&changed_state(), &result, NEXT_VERSION.parse().unwrap())
                .map_err(drop)
        },
    );
}

#[tokio::test]
async fn mutation_run_paths_require_cloud_unsafe_acknowledgement_before_io() {
    let mut ctx = RuntimeContext {
        api_client: ApiClient(ForgeApiClient::new(&ApiConfig::new(
            "invalid-unconnected-url",
            &ForgeClientConfig::default(),
        ))),
        config: RuntimeConfig {
            format: OutputFormat::Json,
            request_timeout: None,
            page_size: 25,
            extended: false,
            cloud_unsafe_op: None,
            sort_by: SortField::PrimaryId,
        },
        output_file: Box::new(tokio::io::sink()),
    };
    for args in [CHANGE, RELEASE, &CHANGE[..3], INTERACTIVE_RELEASE] {
        let error = match parse(args).unwrap() {
            Cmd::ChangeRoutingProfile(command) => command.run(&mut ctx).await.unwrap_err(),
            Cmd::ReleaseInactiveVni(command) => command.run(&mut ctx).await.unwrap_err(),
            _ => panic!("mutation command"),
        };
        assert!(matches!(error, CarbideCliError::CloudUnsafeOp));
    }
}

#[tokio::test(start_paused = true)]
async fn whole_rpc_attempt_uses_the_configured_timeout() {
    for (configured, expected) in [
        (None, Duration::from_secs(300)),
        (Some(Duration::from_secs(60)), Duration::from_secs(60)),
        (Some(Duration::from_secs(600)), Duration::from_secs(600)),
    ] {
        let started = tokio::time::Instant::now();
        let error = rpc_attempt(std::future::pending::<Result<(), Status>>(), configured)
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::DeadlineExceeded);
        assert_eq!(started.elapsed(), expected);
    }
}
