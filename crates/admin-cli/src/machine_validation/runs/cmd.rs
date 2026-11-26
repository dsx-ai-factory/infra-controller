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

use std::collections::HashMap;
use std::time::Duration;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge as forgerpc;
use eyre::WrapErr as _;
use futures::{StreamExt, TryStreamExt, stream};
use prettytable::{Table, row};
use tokio::io::AsyncWrite;

use super::args::ShowRunsOptions;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn handle_runs_show(
    args: ShowRunsOptions,
    output_format: OutputFormat,
    api_client: &ApiClient,
    request_timeout: Option<Duration>,
    output: &mut (dyn AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    show_runs(is_json, api_client, args, request_timeout, output).await?;
    Ok(())
}

async fn show_runs(
    json: bool,
    api_client: &ApiClient,
    args: ShowRunsOptions,
    request_timeout: Option<Duration>,
    output: &mut (dyn AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    let runs = match api_client
        .get_machine_validation_runs(args.machine, args.history)
        .await
    {
        Ok(runs) => runs,
        Err(e) => return Err(e),
    };
    if json {
        crate::async_writeln!(
            output,
            "{}",
            serde_json::to_string_pretty(&runs).wrap_err("serialize machine validation runs")?
        )
        .wrap_err("write machine validation runs")?;
    } else {
        // Match the transport's 300-second default, but cover the entire response,
        // including body/trailers. The caller's request timeout overrides it per RPC.
        let timeout = request_timeout.unwrap_or(Duration::from_secs(300));
        let active_items = load_active_items(api_client, &runs, timeout).await?;
        crate::async_write!(
            output,
            "{}",
            convert_runs_to_nice_table(runs, &active_items)
        )
        .wrap_err("write machine validation runs")?;
    }
    Ok(())
}

// Limit each lookup phase to eight RPCs. Finish ID discovery before loading
// details so the phases cannot multiply concurrency across active runs.
const LOOKUP_CONCURRENCY: usize = 8;

type ActiveItems = HashMap<
    carbide_uuid::machine_validation::MachineValidationId,
    Vec<forgerpc::MachineValidationRunItem>,
>;

async fn load_active_items(
    client: &ApiClient,
    runs: &forgerpc::MachineValidationRunList,
    timeout: Duration,
) -> CarbideCliResult<ActiveItems> {
    let ids: Vec<_> = stream::iter(
        runs.runs
            .iter()
            .filter(|run| run.end_time.is_none())
            .filter_map(|run| run.validation_id),
    )
    .map(|id| async move {
        let response = tokio::time::timeout(
            timeout,
            client.0.find_machine_validation_run_item_ids(
                forgerpc::MachineValidationRunItemSearchFilter {
                    validation_id: Some(id),
                },
            ),
        )
        .await
        .wrap_err_with(|| {
            format!("finish run-item ID lookup for validation run {id} within {timeout:?}")
        })?
        .wrap_err_with(|| format!("load run-item IDs for validation run {id}"))?;
        Ok::<_, crate::errors::CarbideCliError>((id, response.run_item_ids))
    })
    .buffered(LOOKUP_CONCURRENCY)
    .try_collect()
    .await?;

    let details: Vec<_> = stream::iter(
        ids.into_iter()
            .flat_map(|(run_id, ids)| ids.into_iter().map(move |id| (run_id, id))),
    )
    .map(|(run_id, id)| async move {
        let item_id = id.value.clone();
        // One ID per request respects even the smallest site-configured limit.
        let response = tokio::time::timeout(
            timeout,
            client.0.find_machine_validation_run_items_by_ids(
                forgerpc::MachineValidationRunItemsByIdsRequest {
                    run_item_ids: vec![id],
                },
            ),
        )
        .await
        .wrap_err_with(|| format!("finish run-item {item_id} lookup within {timeout:?}"))?
        .wrap_err_with(|| format!("load run-item {item_id} for validation run {run_id}"))?;
        Ok::<_, crate::errors::CarbideCliError>((run_id, response.run_items))
    })
    .buffered(LOOKUP_CONCURRENCY)
    .try_collect()
    .await?;

    let mut active_items = ActiveItems::new();
    for (id, items) in details {
        active_items.entry(id).or_default().extend(items);
    }
    Ok(active_items)
}

fn convert_runs_to_nice_table(
    runs: forgerpc::MachineValidationRunList,
    active_items: &ActiveItems,
) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row![
        "Id",
        "MachineId",
        "StartTime",
        "EndTime",
        "Context",
        "State",
        "Completed/Total",
        "CurrentTest",
        "CurrentAttempt"
    ]);

    for run in runs.runs {
        let end_time = if let Some(run_end_time) = run.end_time {
            run_end_time.to_string()
        } else {
            "".to_string()
        };
        let progress = run
            .status
            .as_ref()
            .map(|status| format!("{}/{}", status.completed_tests, status.total))
            .unwrap_or_default();
        let status_state = run
            .status
            .as_ref()
            .and_then(|status| status.machine_validation_state.as_ref())
            .map(|state| format!("{state:?}"))
            .unwrap_or_else(|| "Unknown".to_string());
        let mut current_tests = Vec::new();
        let mut current_attempts = Vec::new();
        if run.end_time.is_none()
            && let Some(items) = run
                .validation_id
                .as_ref()
                .and_then(|id| active_items.get(id))
        {
            for item in items.iter().filter(|item| item.state == "Running") {
                current_tests.push(item.test_id.clone());
                current_attempts.push(match &item.current_attempt_id {
                    Some(id) => format!("{} ({})", item.attempt, id.value),
                    None => item.attempt.to_string(),
                });
            }
        }
        table.add_row(row![
            run.validation_id.unwrap_or_default(),
            run.machine_id.unwrap_or_default(),
            run.start_time.unwrap_or_default(),
            end_time,
            run.context.unwrap_or_default(),
            status_state,
            progress,
            current_tests.join(", "),
            current_attempts.join(", "),
        ]);
    }

    table.into()
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::panic::AssertUnwindSafe;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use ::rpc::forge::*;
    use ::rpc::forge_api_client::ForgeApiClient;
    use ::rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
    use carbide_uuid::machine_validation::MachineValidationId;
    use clap::Parser;
    use futures::{FutureExt, stream};
    use http_body_util::combinators::UnsyncBoxBody;
    use http_body_util::{BodyExt, StreamBody};
    use hyper::body::{Bytes, Frame, Incoming};
    use hyper::server::conn::http2;
    use hyper::service::service_fn;
    use hyper::{Response, header};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use prost::Message as _;
    use tokio::net::TcpListener;

    use crate::async_write::CapturedOutput;
    use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
    use crate::cfg::dispatch::Dispatch;
    use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
    use crate::rpc::ApiClient;

    #[derive(Clone, Default)]
    enum MockBehavior {
        #[default]
        Normal,
        Stall(&'static str, Arc<AtomicBool>),
        Concurrent(Arc<ConcurrencyProbe>),
    }

    struct ConcurrencyProbe {
        active: AtomicUsize,
        peak: AtomicUsize,
        barrier: tokio::sync::Barrier,
    }

    async fn public_runs_show_output(format: ::rpc::admin_cli::OutputFormat) -> String {
        public_runs_show_result(format, MockBehavior::Normal, Duration::from_secs(5))
            .await
            .expect("runs show dispatch succeeds")
    }

    async fn public_runs_show_result(
        format: ::rpc::admin_cli::OutputFormat,
        behavior: MockBehavior,
        request_timeout: Duration,
    ) -> crate::errors::CarbideCliResult<String> {
        let command = parse_public_runs_show_command();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock Machine Validation listener binds");
        let address = listener
            .local_addr()
            .expect("mock Machine Validation listener has an address");
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

        let load_details = format != ::rpc::admin_cli::OutputFormat::Json;
        let server = tokio::spawn(async move {
            let (connection, _) = listener.accept().await.expect("mock accepts a client");
            http2::Builder::new(TokioExecutor::new())
                .serve_connection(
                    TokioIo::new(connection),
                    service_fn(move |request| {
                        mock_runs_show_request(request, load_details, behavior.clone())
                    }),
                )
                .await
                .expect("mock serves the CLI request");
        });
        let result = AssertUnwindSafe(tokio::time::timeout(
            Duration::from_secs(10),
            command.dispatch(ctx),
        ))
        .catch_unwind()
        .await;
        server.abort();
        if let Err(error) = server.await {
            assert!(error.is_cancelled(), "mock server failed: {error}");
        }
        result
            .expect("runs show dispatch does not panic")
            .expect("runs show dispatch completes before the test guard")?;

        Ok(String::from_utf8(captured.into_bytes().await).expect("CLI output is valid UTF-8"))
    }

    #[tokio::test]
    async fn public_runs_show_command_renders_live_progress_and_terminal_empty_cells() {
        let display = public_runs_show_output(::rpc::admin_cli::OutputFormat::AsciiTable).await;
        let rows: Vec<Vec<_>> = display
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| line.trim_matches('|').split('|').map(str::trim).collect())
            .collect();

        assert_eq!(
            rows[0],
            [
                "Id",
                "MachineId",
                "StartTime",
                "EndTime",
                "Context",
                "State",
                "Completed/Total",
                "CurrentTest",
                "CurrentAttempt"
            ]
        );
        assert_eq!(rows[1][0], "11111111-1111-4111-8111-111111111111");
        assert_eq!(rows[1][3], "");
        assert_eq!(rows[1][4], "OnDemand");
        assert_eq!(rows[1][5], "InProgress(InProgress)");
        assert_eq!(rows[1][6], "0/1");
        assert_eq!(rows[1][7], "forge_live_test");
        assert_eq!(rows[1][8], "2 (33333333-3333-4333-8333-333333333333)");
        assert_eq!(rows[2][6], "1/1");
        assert_eq!(rows[2][7], "");
        assert_eq!(rows[2][8], "");
    }

    #[tokio::test]
    async fn public_runs_show_json_preserves_native_progress_without_item_lookups() {
        let display = public_runs_show_output(::rpc::admin_cli::OutputFormat::Json).await;
        let value: serde_json::Value = serde_json::from_str(&display).expect("native run JSON");
        assert_eq!(value["runs"].as_array().unwrap().len(), 2);
        assert_eq!(value["runs"][0]["status"]["total"], 1);
        assert_eq!(value["runs"][0]["status"]["completed_tests"], 0);
        assert_eq!(value["runs"][1]["status"]["completed_tests"], 1);
        assert!(value["runs"][0].get("current_test").is_none());
    }

    #[tokio::test]
    async fn public_runs_show_bounds_stalled_lookup_response_bodies() {
        for (path, context) in [
            (
                "/forge.Forge/FindMachineValidationRunItemIds",
                "finish run-item ID lookup",
            ),
            (
                "/forge.Forge/FindMachineValidationRunItemsByIds",
                "finish run-item 44444444",
            ),
        ] {
            let reached = Arc::new(AtomicBool::new(false));
            let error = public_runs_show_result(
                ::rpc::admin_cli::OutputFormat::AsciiTable,
                MockBehavior::Stall(path, reached.clone()),
                Duration::from_millis(500),
            )
            .await
            .expect_err("stalled response must hit the configured deadline");
            assert!(
                reached.load(Ordering::SeqCst),
                "lookup reached the stalled response"
            );
            let crate::errors::CarbideCliError::EyreReport(report) = error else {
                panic!("expected contextual deadline error: {error}");
            };
            assert!(report.to_string().contains(context), "{report:?}");
            assert!(
                report
                    .downcast_ref::<tokio::time::error::Elapsed>()
                    .is_some(),
                "{report:?}"
            );
        }
    }

    #[tokio::test]
    async fn public_runs_show_bounds_concurrency_and_preserves_item_run_association() {
        let probe = Arc::new(ConcurrencyProbe {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            barrier: tokio::sync::Barrier::new(super::LOOKUP_CONCURRENCY),
        });
        let display = public_runs_show_result(
            ::rpc::admin_cli::OutputFormat::AsciiTable,
            MockBehavior::Concurrent(probe.clone()),
            Duration::from_secs(5),
        )
        .await
        .expect("parallel lookups succeed");
        assert_eq!(probe.peak.load(Ordering::SeqCst), super::LOOKUP_CONCURRENCY);
        assert_eq!(probe.active.load(Ordering::SeqCst), 0);
        let rows: Vec<Vec<_>> = display
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| line.trim_matches('|').split('|').map(str::trim).collect())
            .collect();
        for (row, offset) in [(1, 0), (2, super::LOOKUP_CONCURRENCY)] {
            let indices = offset..offset + super::LOOKUP_CONCURRENCY;
            assert_eq!(
                rows[row][7],
                indices
                    .clone()
                    .map(|i| format!("test_{i:02}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            assert_eq!(
                rows[row][8],
                indices
                    .map(|i| format!("{} ({})", i + 1, mock_item_id(i)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    fn mock_item_id(index: usize) -> String {
        format!("44444444-4444-4444-8444-{index:012}")
    }

    fn parse_public_runs_show_command() -> crate::machine_validation::Cmd {
        let options = CliOptions::try_parse_from([
            "nico-admin-cli",
            "machine-validation",
            "runs",
            "show",
            "--machine",
            "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg",
        ])
        .expect("public runs show command parses");
        let Some(CliCommand::MachineValidation(command)) = options.commands else {
            panic!("expected Machine Validation command");
        };
        command
    }

    async fn mock_runs_show_request(
        request: hyper::Request<Incoming>,
        load_details: bool,
        behavior: MockBehavior,
    ) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
        if let MockBehavior::Stall(path, reached) = &behavior
            && request.uri().path() == *path
        {
            reached.store(true, Ordering::SeqCst);
            // Return HTTP/gRPC headers immediately, then never produce body or trailers.
            return Ok(Response::builder()
                .header(header::CONTENT_TYPE, "application/grpc+tonic")
                .body(
                    StreamBody::new(stream::pending::<Result<Frame<Bytes>, Infallible>>())
                        .boxed_unsync(),
                )
                .unwrap());
        }
        Ok(match request.uri().path() {
            "/forge.Forge/Version" => grpc_response(BuildInfo::default()),
            "/forge.Forge/GetMachineValidationRuns" => {
                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                let request = MachineValidationRunListGetRequest::decode(&bytes[5..]).unwrap();
                assert!(request.machine_id.is_some());
                grpc_response(MachineValidationRunList {
                    runs: vec![
                        MachineValidationRun {
                            validation_id: Some(
                                "11111111-1111-4111-8111-111111111111"
                                    .parse::<MachineValidationId>()
                                    .unwrap(),
                            ),
                            machine_id: request.machine_id,
                            context: Some("OnDemand".into()),
                            status: Some(MachineValidationStatus {
                                total: 1,
                                completed_tests: 0,
                                machine_validation_state: Some(
                                    machine_validation_status::MachineValidationState::InProgress(
                                        0,
                                    ),
                                ),
                            }),
                            ..Default::default()
                        },
                        MachineValidationRun {
                            validation_id: Some(
                                "22222222-2222-4222-8222-222222222222"
                                    .parse::<MachineValidationId>()
                                    .unwrap(),
                            ),
                            end_time: if matches!(behavior, MockBehavior::Concurrent(_)) {
                                None
                            } else {
                                Some(::rpc::Timestamp::default())
                            },
                            status: Some(MachineValidationStatus {
                                total: 1,
                                completed_tests: 1,
                                machine_validation_state: Some(
                                    machine_validation_status::MachineValidationState::Completed(0),
                                ),
                            }),
                            ..Default::default()
                        },
                    ],
                })
            }
            "/forge.Forge/FindMachineValidationRunItemIds" => {
                assert!(load_details, "JSON must not fetch additional run-item data");
                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                let request = MachineValidationRunItemSearchFilter::decode(&bytes[5..]).unwrap();
                let run_id = request.validation_id.unwrap().to_string();
                let indices = match &behavior {
                    MockBehavior::Concurrent(_) => {
                        let offset = if run_id.starts_with("11111111") {
                            0
                        } else {
                            super::LOOKUP_CONCURRENCY
                        };
                        offset..offset + super::LOOKUP_CONCURRENCY
                    }
                    _ => {
                        assert!(run_id.starts_with("11111111"));
                        // Retain the original fixture ID for the basic table test.
                        return Ok(grpc_response(MachineValidationRunItemIdList {
                            run_item_ids: vec![::rpc::common::Uuid {
                                value: "44444444-4444-4444-8444-444444444444".into(),
                            }],
                        }));
                    }
                };
                grpc_response(MachineValidationRunItemIdList {
                    run_item_ids: indices
                        .map(|i| ::rpc::common::Uuid {
                            value: mock_item_id(i),
                        })
                        .collect(),
                })
            }
            "/forge.Forge/FindMachineValidationRunItemsByIds" => {
                assert!(load_details, "JSON must not fetch additional run-item data");
                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                let request = MachineValidationRunItemsByIdsRequest::decode(&bytes[5..]).unwrap();
                assert_eq!(
                    request.run_item_ids.len(),
                    1,
                    "respect a server limit of one ID"
                );
                let item = if let MockBehavior::Concurrent(probe) = &behavior {
                    let id = &request.run_item_ids[0].value;
                    let index: usize = id.rsplit('-').next().unwrap().parse().unwrap();
                    let active = probe.active.fetch_add(1, Ordering::SeqCst) + 1;
                    probe.peak.fetch_max(active, Ordering::SeqCst);
                    assert!(active <= super::LOOKUP_CONCURRENCY);
                    probe.barrier.wait().await;
                    // Complete each window in reverse order to exercise ordered buffering.
                    tokio::time::sleep(Duration::from_millis(
                        (super::LOOKUP_CONCURRENCY - index % super::LOOKUP_CONCURRENCY) as u64 * 2,
                    ))
                    .await;
                    probe.active.fetch_sub(1, Ordering::SeqCst);
                    MachineValidationRunItem {
                        test_id: format!("test_{index:02}"),
                        state: "Running".into(),
                        attempt: u32::try_from(index + 1).unwrap(),
                        current_attempt_id: Some(request.run_item_ids[0].clone()),
                        ..Default::default()
                    }
                } else {
                    MachineValidationRunItem {
                        test_id: "forge_live_test".into(),
                        state: "Running".into(),
                        attempt: 2,
                        current_attempt_id: Some(::rpc::common::Uuid {
                            value: "33333333-3333-4333-8333-333333333333".into(),
                        }),
                        ..Default::default()
                    }
                };
                grpc_response(MachineValidationRunItemList {
                    run_items: vec![item],
                })
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
}
