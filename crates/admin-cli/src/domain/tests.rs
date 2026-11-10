/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::convert::Infallible;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_test_support::Outcome::{FailsWith, Yields};
use carbide_test_support::{Case, Check, check_cases_async, check_values};
use clap::Parser;
use futures::FutureExt;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use rpc::admin_cli::OutputFormat;
use rpc::carbide_api_client::CarbideApiClient;
use rpc::carbide_tls_client::{ApiConfig, CarbideClientConfig};
use rpc::forge::BuildInfo;
use rpc::protos::dns::{
    CreateDomainRequest, Domain, DomainDeletionRequest, DomainDeletionResult, DomainList,
    DomainSearchQuery, Metadata, UpdateDomainRequest,
};
use tokio::net::TcpListener;
use tonic::Code;

use super::Cmd;
use crate::async_write::CapturedOutput;
use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
use crate::cfg::dispatch::Dispatch;
use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::test_support::grpc_response;

const DOMAIN_ID: &str = "12345678-1234-5678-90ab-cdef01234567";
const VPC_ID: &str = "abcdef01-2345-6789-abcd-ef0123456789";
const NAME: &str = "compute.example.com";

#[derive(Debug, PartialEq)]
enum Call {
    Create(CreateDomainRequest),
    Delete(DomainDeletionRequest),
    Find(DomainSearchQuery),
    Update(UpdateDomainRequest),
}

#[derive(Clone)]
struct Reply {
    domain: Option<Domain>,
    error: Option<Code>,
}

impl Default for Reply {
    fn default() -> Self {
        Self {
            domain: Some(Domain {
                id: Some(DOMAIN_ID.parse().unwrap()),
                name: NAME.into(),
                vpc_id: Some(VPC_ID.parse().unwrap()),
                default_ttl: Some(600),
                metadata: Some(Metadata {
                    allow_axfr_from: vec!["192.0.2.10".into()],
                }),
                soa: Some("ns.example.com hostmaster.example.com 1 3600 600 86400 300".into()),
                ..Default::default()
            }),
            error: None,
        }
    }
}

fn parse(args: &[&str]) -> Result<Cmd, clap::Error> {
    let options = CliOptions::try_parse_from(
        ["nico-admin-cli", "domain"]
            .into_iter()
            .chain(args.iter().copied()),
    )?;
    let Some(CliCommand::Domain(command)) = options.commands else {
        panic!("expected the public Domain command");
    };
    Ok(command)
}

async fn dispatch(
    args: &[&str],
    format: OutputFormat,
    reply: Reply,
) -> (CarbideCliResult<()>, String, Vec<Call>) {
    let command = parse(args).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client_config = CarbideClientConfig {
        request_timeout: Some(Duration::from_secs(5)),
        ..Default::default()
    };
    let mut captured = CapturedOutput::new();
    let ctx = RuntimeContext {
        api_client: ApiClient(CarbideApiClient::new(&ApiConfig::new(
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
                service_fn(move |request| respond(request, reply.clone(), server_calls.clone())),
            )
            .await
            .expect("mock serves the Domain connection");
    });
    // Exercise parsing, dispatch and the real API client; only Core's replies
    // are controlled. Join the listener even when a regression panics.
    let result = AssertUnwindSafe(tokio::time::timeout(
        Duration::from_secs(5),
        command.dispatch(ctx),
    ))
    .catch_unwind()
    .await;
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "mock Domain server failed: {error}");
    }
    let result = result
        .expect("Domain dispatch did not panic")
        .expect("Domain dispatch finishes within five seconds");
    let output = String::from_utf8(captured.into_bytes().await).unwrap();
    let calls = std::mem::take(&mut *calls.lock().unwrap());
    (result, output, calls)
}

async fn respond(
    request: Request<Incoming>,
    reply: Reply,
    calls: Arc<Mutex<Vec<Call>>>,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
    let path = request.uri().path().to_string();
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let payload = body.get(5..).expect("unary gRPC request frame");
    let call = match path.as_str() {
        "/forge.Forge/Version" => return Ok(grpc_response(BuildInfo::default())),
        "/forge.Forge/CreateDomain" => Call::Create(CreateDomainRequest::decode(payload).unwrap()),
        "/forge.Forge/DeleteDomain" => {
            Call::Delete(DomainDeletionRequest::decode(payload).unwrap())
        }
        "/forge.Forge/FindDomain" => Call::Find(DomainSearchQuery::decode(payload).unwrap()),
        "/forge.Forge/UpdateDomain" => Call::Update(UpdateDomainRequest::decode(payload).unwrap()),
        _ => panic!("unexpected RPC: {path}"),
    };
    calls.lock().unwrap().push(call);
    if let Some(code) = reply.error {
        return Ok(Response::builder()
            .header("content-type", "application/grpc")
            .header("grpc-status", (code as i32).to_string())
            .header("grpc-message", "domain request rejected")
            .body(Empty::<Bytes>::new().boxed_unsync())
            .unwrap());
    }
    Ok(match path.as_str() {
        "/forge.Forge/CreateDomain" | "/forge.Forge/UpdateDomain" => {
            grpc_response(reply.domain.unwrap())
        }
        "/forge.Forge/FindDomain" => grpc_response(DomainList {
            domains: reply.domain.into_iter().collect(),
        }),
        "/forge.Forge/DeleteDomain" => grpc_response(DomainDeletionResult {}),
        _ => unreachable!(),
    })
}

#[tokio::test]
async fn create_preserves_default_and_explicit_requests() {
    check_cases_async(
        [
            Case {
                scenario: "omission keeps the site defaults",
                input: vec!["create", NAME],
                expect: Yields(vec![Call::Create(CreateDomainRequest {
                    name: NAME.into(),
                    vpc_id: None,
                    default_ttl: None,
                })]),
            },
            Case {
                scenario: "VPC ownership and explicit TTL reach Core",
                input: vec!["create", NAME, "--vpc-id", VPC_ID, "--default-ttl", "600"],
                expect: Yields(vec![Call::Create(CreateDomainRequest {
                    name: NAME.into(),
                    vpc_id: Some(VPC_ID.parse().unwrap()),
                    default_ttl: Some(600),
                })]),
            },
        ],
        |args| async move {
            let (result, _, calls) = dispatch(&args, OutputFormat::Json, Reply::default()).await;
            result.map(|()| calls).map_err(|error| error.to_string())
        },
    )
    .await;
}

#[tokio::test]
async fn create_writes_the_complete_response_to_the_selected_output() {
    let reply = Reply::default();
    let expected = serde_json::to_value(reply.domain.as_ref().unwrap()).unwrap();
    let (result, output, _) = dispatch(&["create", NAME], OutputFormat::Json, reply).await;
    result.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output).unwrap(),
        expected
    );

    for (vpc_id, ttl, expected_vpc, expected_ttl) in [
        (Some(VPC_ID.parse().unwrap()), Some(600), VPC_ID, "600s"),
        (None, None, "none (infrastructure)", "site default"),
    ] {
        let mut reply = Reply::default();
        let domain = reply.domain.as_mut().unwrap();
        domain.vpc_id = vpc_id;
        domain.default_ttl = ttl;
        let (result, output, _) =
            dispatch(&["create", NAME], OutputFormat::AsciiTable, reply).await;
        result.unwrap();
        for (label, value) in [
            ("ID", DOMAIN_ID),
            ("NAME", NAME),
            ("VPC", expected_vpc),
            ("DEFAULT TTL", expected_ttl),
        ] {
            assert!(
                output.lines().any(|line| line
                    .split_once(':')
                    .is_some_and(|(key, cell)| key.trim() == label && cell.trim() == value)),
                "missing {label}: {value} in {output}",
            );
        }
    }
}

#[tokio::test]
async fn delete_dispatches_the_id_before_writing_success() {
    check_cases_async(
        [
            Case {
                scenario: "JSON deletion result",
                input: OutputFormat::Json,
                expect: Yields(format!("{{\"deleted\":\"{DOMAIN_ID}\"}}\n")),
            },
            Case {
                scenario: "human-readable deletion result",
                input: OutputFormat::AsciiTable,
                expect: Yields(format!("Deleted domain {DOMAIN_ID}\n")),
            },
        ],
        |format| async move {
            let (result, output, calls) =
                dispatch(&["delete", DOMAIN_ID], format, Reply::default()).await;
            assert_eq!(
                calls,
                vec![Call::Delete(DomainDeletionRequest {
                    id: Some(DOMAIN_ID.parse().unwrap()),
                })],
            );
            result.map(|()| output).map_err(|error| error.to_string())
        },
    )
    .await;
}

#[tokio::test]
async fn mutation_errors_propagate_without_success_output() {
    check_cases_async(
        [
            (
                "invalid creation",
                vec!["create", NAME],
                Code::InvalidArgument,
            ),
            (
                "missing deletion",
                vec!["delete", DOMAIN_ID],
                Code::NotFound,
            ),
            (
                "referenced domain",
                vec!["delete", DOMAIN_ID],
                Code::FailedPrecondition,
            ),
            (
                "unauthorized deletion",
                vec!["delete", DOMAIN_ID],
                Code::PermissionDenied,
            ),
        ]
        .map(|(scenario, args, code)| Case {
            scenario,
            input: (args, code),
            expect: FailsWith(code),
        }),
        |(args, code)| async move {
            let (result, output, calls) = dispatch(
                &args,
                OutputFormat::Json,
                Reply {
                    error: Some(code),
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(calls.len(), 1, "exactly one mutation attempt");
            assert!(output.is_empty(), "failed RPC printed success: {output}");
            result.map_err(|error| {
                let original = match &error {
                    CarbideCliError::EyreReport(report) => report
                        .downcast_ref::<CarbideCliError>()
                        .expect("operation context must retain the original typed error"),
                    other => other,
                };
                let CarbideCliError::ApiInvocationError(status) = original else {
                    panic!("expected the original RPC status, got {error}");
                };
                assert_eq!(status.message(), "domain request rejected");
                status.code()
            })
        },
    )
    .await;
}

#[tokio::test]
async fn show_preserves_all_and_id_selectors() {
    check_cases_async(
        [
            ("all domains", vec!["show"], None),
            ("legacy all selector", vec!["show", "--all"], None),
            (
                "one domain",
                vec!["show", DOMAIN_ID],
                Some(DOMAIN_ID.parse().unwrap()),
            ),
        ]
        .map(|(scenario, args, id)| Case {
            scenario,
            input: args,
            expect: Yields(vec![Call::Find(DomainSearchQuery { id, name: None })]),
        }),
        |args| async move {
            let (result, _, calls) = dispatch(&args, OutputFormat::Json, Reply::default()).await;
            result.map(|()| calls).map_err(|error| error.to_string())
        },
    )
    .await;
}

#[tokio::test]
async fn show_missing_domain_retains_the_not_found_error() {
    let (result, _, calls) = dispatch(
        &["show", DOMAIN_ID],
        OutputFormat::Json,
        Reply {
            domain: None,
            error: None,
        },
    )
    .await;
    assert!(matches!(result, Err(CarbideCliError::DomainNotFound)));
    assert_eq!(
        calls,
        vec![Call::Find(DomainSearchQuery {
            id: Some(DOMAIN_ID.parse().unwrap()),
            name: None,
        })],
    );
}

#[tokio::test]
async fn update_changes_only_the_requested_ttl_without_a_stale_read() {
    let (result, _, calls) = dispatch(
        &["update", DOMAIN_ID, "--default-ttl", "600"],
        OutputFormat::Json,
        Reply::default(),
    )
    .await;
    result.unwrap();
    assert_eq!(
        calls,
        vec![Call::Update(UpdateDomainRequest {
            domain: Some(Domain {
                id: Some(DOMAIN_ID.parse().unwrap()),
                default_ttl: Some(600),
                ..Default::default()
            }),
        })],
    );
}

#[test]
fn invalid_arguments_are_rejected_before_dispatch() {
    check_values(
        [
            ("missing deletion ID", vec!["delete"]),
            ("malformed deletion ID", vec!["delete", "not-a-domain-id"]),
            (
                "malformed VPC ID",
                vec!["create", NAME, "--vpc-id", "invalid"],
            ),
            ("invalid TTL", vec!["create", NAME, "--default-ttl", "-1"]),
            ("conflicting selectors", vec!["show", DOMAIN_ID, "--all"]),
        ]
        .map(|(scenario, input)| Check {
            scenario,
            input,
            expect: false,
        }),
        |args| parse(&args).is_ok(),
    );
}
