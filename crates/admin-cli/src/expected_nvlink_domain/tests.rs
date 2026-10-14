/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::net::TcpListener;

use super::*;
use crate::async_write::CapturedOutput;
use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
use crate::cfg::runtime::RuntimeConfig;
use crate::rpc::ApiClient;

fn parse(args: &[&str]) -> Result<Cmd, clap::Error> {
    let options = CliOptions::try_parse_from(
        ["nico-admin-cli", "expected-nvlink-domain"]
            .into_iter()
            .chain(args.iter().copied()),
    )?;
    let Some(CliCommand::ExpectedNvlinkDomain(command)) = options.commands else {
        panic!("expected domain command")
    };
    Ok(command)
}

#[test]
fn help_omits_unused_output_flags() {
    use carbide_test_support::{Check, check_values};

    check_values(
        ["", "show", "create", "delete"].map(|subcommand| Check {
            scenario: subcommand,
            input: subcommand,
            expect: (clap::error::ErrorKind::DisplayHelp, false, false),
        }),
        |subcommand| {
            let mut args = vec!["nico-admin-cli", "expected-nvlink-domain"];
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

#[test]
fn invalid_invocations() {
    for args in [
        vec!["create", "group"],
        vec!["create", "group", "--member", "invalid"],
        vec!["delete", "invalid"],
        vec![
            "show",
            "12345678-1234-5678-90ab-cdef01234567",
            "--rack-group-id",
            "group",
        ],
    ] {
        assert!(parse(&args).is_err(), "{args:?}");
    }
}

#[tokio::test]
async fn public_commands_and_output() {
    let domain = forge::ExpectedNvLinkDomain {
        id: Some(Uuid::new_v4().into()),
        rack_group_id: Some(RackGroupId::new("group")),
        name: Some("mini-a".into()),
        member_bmc_mac_addresses: vec!["00:11:22:33:44:55".into(), "00:11:22:33:44:66".into()],
    };
    let id = domain.id.as_ref().unwrap().value.as_str();
    let (output, calls) = dispatch(
        &[
            "create",
            "group",
            "--name",
            "mini-a",
            "--member",
            "00:11:22:33:44:55",
            "--member",
            "00:11:22:33:44:66",
        ],
        OutputFormat::Json,
        domain.clone(),
    )
    .await;
    let request = forge::CreateExpectedNvLinkDomainRequest::decode(calls[0].1.as_slice()).unwrap();
    assert_eq!(request.name, domain.name);
    assert_eq!(request.rack_group_id, domain.rack_group_id);
    assert_eq!(
        request.member_bmc_mac_addresses,
        domain.member_bmc_mac_addresses
    );
    assert!(output.contains(id));

    for format in [OutputFormat::AsciiTable, OutputFormat::Csv] {
        for name in [Some("mini-a".into()), None] {
            let expected = forge::ExpectedNvLinkDomain {
                name: name.clone(),
                ..domain.clone()
            };
            let (output, _) = dispatch(&["show", id], format, expected).await;
            for value in [
                "Domain ID",
                "Rack Group ID",
                "Name",
                "Member BMC MACs",
                id,
                "group",
                "00:11:22:33:44:55",
                "00:11:22:33:44:66",
            ] {
                assert!(output.contains(value), "missing {value}: {output}");
            }
            if name.is_some() {
                assert!(output.contains("mini-a"));
            } else if format == OutputFormat::Csv {
                assert!(output.contains("group,,"));
            } else {
                assert!(!output.contains("mini-a"));
                assert!(output.lines().any(|line| {
                    line.contains(id)
                        && line
                            .split('|')
                            .nth(3)
                            .is_some_and(|cell| cell.trim().is_empty())
                }));
            }
        }
    }
    let (_, calls) = dispatch(
        &["show", "--rack-group-id", "group"],
        OutputFormat::Json,
        domain.clone(),
    )
    .await;
    assert_eq!(calls[0].0, "/forge.Forge/FindExpectedNvLinkDomainIds");
    assert_eq!(
        forge::ExpectedNvLinkDomainSearchFilter::decode(calls[0].1.as_slice())
            .unwrap()
            .rack_group_id,
        domain.rack_group_id
    );
    assert_eq!(calls[1].0, "/forge.Forge/FindExpectedNvLinkDomainsByIds");
    let (_, calls) = dispatch(&["delete", id], OutputFormat::AsciiTable, domain.clone()).await;
    assert_eq!(calls[0].0, "/forge.Forge/DeleteExpectedNvLinkDomain");
    assert_eq!(
        forge::ExpectedNvLinkDomainRequest::decode(calls[0].1.as_slice())
            .unwrap()
            .id,
        domain.id
    );
}

async fn dispatch(
    args: &[&str],
    format: OutputFormat,
    domain: forge::ExpectedNvLinkDomain,
) -> (String, Vec<(String, Vec<u8>)>) {
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
                    let domain = domain.clone();
                    async move {
                        let path = request.uri().path().to_string();
                        let body = request.into_body().collect().await.unwrap().to_bytes();
                        let encoded = if path == "/forge.Forge/Version" {
                            forge::BuildInfo::default().encode_to_vec()
                        } else {
                            calls
                                .lock()
                                .unwrap()
                                .push((path.clone(), body[5..].to_vec()));
                            match path.as_str() {
                                "/forge.Forge/CreateExpectedNvLinkDomain"
                                | "/forge.Forge/GetExpectedNvLinkDomain" => domain.encode_to_vec(),
                                "/forge.Forge/DeleteExpectedNvLinkDomain" => vec![],
                                "/forge.Forge/FindExpectedNvLinkDomainIds" => {
                                    forge::ExpectedNvLinkDomainIdList {
                                        ids: vec![domain.id.unwrap()],
                                    }
                                    .encode_to_vec()
                                }
                                "/forge.Forge/FindExpectedNvLinkDomainsByIds" => {
                                    forge::ExpectedNvLinkDomainList {
                                        domains: vec![domain],
                                    }
                                    .encode_to_vec()
                                }
                                _ => panic!("unexpected RPC {path}"),
                            }
                        };
                        let mut data = vec![0];
                        data.extend_from_slice(
                            &u32::try_from(encoded.len()).unwrap().to_be_bytes(),
                        );
                        data.extend(encoded);
                        let mut trailers = hyper::HeaderMap::new();
                        trailers
                            .insert("grpc-status", hyper::header::HeaderValue::from_static("0"));
                        let body = StreamBody::new(stream::iter([
                            Ok::<_, Infallible>(Frame::data(Bytes::from(data))),
                            Ok(Frame::trailers(trailers)),
                        ]))
                        .boxed_unsync();
                        Ok::<_, Infallible>(
                            Response::builder()
                                .header("content-type", "application/grpc+tonic")
                                .body(body)
                                .unwrap(),
                        )
                    }
                }),
            )
            .await
            .unwrap();
    });
    let config = ForgeClientConfig {
        request_timeout: Some(Duration::from_secs(5)),
        ..Default::default()
    };
    let mut captured = CapturedOutput::new();
    let context = RuntimeContext {
        api_client: ApiClient(ForgeApiClient::new(&ApiConfig::new(
            &format!("http://{address}"),
            &config,
        ))),
        config: RuntimeConfig {
            format,
            request_timeout: config.request_timeout,
            page_size: 1,
            extended: false,
            cloud_unsafe_op: None,
            sort_by: SortField::PrimaryId,
        },
        output_file: std::mem::replace(captured.writer(), Box::new(tokio::io::sink())),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        parse(args).unwrap().dispatch(context),
    )
    .await;
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "{error}");
    }
    result.unwrap().unwrap();
    let output = String::from_utf8(captured.into_bytes().await).unwrap();
    let calls = calls.lock().unwrap().clone();
    (output, calls)
}
