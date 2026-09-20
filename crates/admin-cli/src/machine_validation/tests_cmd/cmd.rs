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

use std::fmt::Write;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge::{
    self as forgerpc, MachineValidationTestEnableDisableTestRequest,
    MachineValidationTestUpdateRequest, MachineValidationTestVerfiedRequest,
};
use prettytable::{Table, row};
use tokio::io::AsyncWrite;

use super::args::{
    AddTestOptions, EnableDisableTestOptions, ShowTestOptions, UpdateTestOptions, VerifyTestOptions,
};
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn show_tests(
    api_client: &ApiClient,
    args: ShowTestOptions,
    output_format: OutputFormat,
    extended: bool,
    output: &mut (dyn AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    let tests = api_client
        .get_machine_validation_tests(
            args.test_id,
            args.platforms,
            args.contexts,
            args.show_un_verfied,
        )
        .await?;
    if extended {
        crate::async_writeln!(
            output,
            "{}",
            render_tests_show_output(output_format == OutputFormat::Json, tests)?
        )?;
    } else {
        crate::async_writeln!(output, "{}", convert_tests_to_nice_table(tests.tests))?;
    }

    Ok(())
}

/// Renders the response consumed by the public `machine-validation tests show`
/// command without performing an API call.
fn render_tests_show_output(
    is_json: bool,
    test: forgerpc::MachineValidationTestsGetResponse,
) -> CarbideCliResult<String> {
    if is_json {
        test.tests
            .iter()
            .map(serde_json::to_string_pretty)
            .collect::<Result<Vec<_>, _>>()
            .map(|tests| tests.join("\n"))
            .map_err(Into::into)
    } else {
        convert_tests_to_nice_format(test.tests)
    }
}

fn convert_tests_to_nice_table(tests: Vec<forgerpc::MachineValidationTest>) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row![
        "TestId",
        "Name",
        "Command",
        "Timeout",
        "PluginType",
        "IsVerified",
        "Version",
        "IsEnabled",
    ]);

    for test in tests {
        table.add_row(row![
            test.test_id,
            test.name,
            test.command,
            test.timeout.unwrap_or_default().to_string(),
            test.plugin
                .as_ref()
                .map_or("", |plugin| plugin.r#type.as_str()),
            test.verified,
            test.version,
            test.is_enabled,
        ]);
    }

    table.into()
}

fn convert_tests_to_nice_format(
    tests: Vec<forgerpc::MachineValidationTest>,
) -> CarbideCliResult<String> {
    let width = 14;
    let mut lines = String::new();
    if tests.is_empty() {
        return Ok(lines);
    }
    // data.clear();
    for test in tests {
        writeln!(
            &mut lines,
            "\t------------------------------------------------------------------------"
        )?;
        let contexts = match serde_json::to_string(&test.contexts) {
            Ok(msg) => msg,
            Err(_) => "[]".to_string(),
        };
        let platforms = match serde_json::to_string(&test.supported_platforms) {
            Ok(msg) => msg,
            Err(_) => "[]".to_string(),
        };
        let custom_tags = match serde_json::to_string(&test.custom_tags) {
            Ok(msg) => msg,
            Err(_) => "[]".to_string(),
        };
        let components = match serde_json::to_string(&test.components) {
            Ok(msg) => msg,
            Err(_) => "[]".to_string(),
        };
        let plugin = test.plugin.unwrap_or_default();
        let plugin_entrypoint =
            serde_json::to_string(&plugin.entrypoint).unwrap_or_else(|_| "[]".to_string());

        let details = vec![
            ("TestId", test.test_id),
            ("Name", test.name),
            ("Description", test.description.unwrap_or_default()),
            ("Command", test.command),
            ("Args", test.args),
            ("Contexts", contexts),
            ("PreCondition", test.pre_condition.unwrap_or_default()),
            (
                "TimeOut",
                test.timeout.map(|t| t.to_string()).unwrap_or_default(),
            ),
            ("CustomTags", custom_tags),
            ("Components", components),
            ("SupportedPlatforms", platforms),
            ("ImageName", test.img_name.unwrap_or_default()),
            ("ContainerArgs", test.container_arg.unwrap_or_default()),
            (
                "ExecuteInHost",
                test.execute_in_host.unwrap_or_default().to_string(),
            ),
            ("ExtraErrorFile", test.extra_err_file.unwrap_or_default()),
            (
                "ExtraOutPutFile",
                test.extra_output_file.unwrap_or_default(),
            ),
            (
                "ExternalConfigFile",
                test.external_config_file.unwrap_or_default(),
            ),
            ("Version", test.version.to_string()),
            ("LastModifiedAt", test.last_modified_at),
            ("LastModifiedBy", test.modified_by),
            ("IsVerified", test.verified.to_string()),
            ("IsReadOnly", test.read_only.to_string()),
            ("IsEnabled", test.is_enabled.to_string()),
            ("FullHostApproved", test.full_host_approved.to_string()),
            ("PluginType", plugin.r#type),
            ("PluginImage", plugin.image),
            ("PluginEntrypoint", plugin_entrypoint),
            ("PluginParameters", plugin.parameters_json),
            ("PluginPrivileged", plugin.privileged.to_string()),
            ("PluginHostAccessFull", plugin.host_access_full.to_string()),
        ];

        for (key, value) in details {
            writeln!(&mut lines, "{key:<width$}: {value}")?;
        }
        writeln!(
            &mut lines,
            "\t------------------------------------------------------------------------"
        )?;
    }
    Ok(lines)
}

pub(super) async fn machine_validation_test_verfied(
    api_client: &ApiClient,
    options: VerifyTestOptions,
) -> CarbideCliResult<()> {
    api_client
        .0
        .machine_validation_test_verfied(MachineValidationTestVerfiedRequest {
            test_id: options.test_id,
            version: options.version,
        })
        .await?;
    Ok(())
}

pub(super) async fn machine_validation_test_enable(
    api_client: &ApiClient,
    options: EnableDisableTestOptions,
) -> CarbideCliResult<()> {
    api_client
        .0
        .machine_validation_test_enable_disable_test(
            MachineValidationTestEnableDisableTestRequest {
                test_id: options.test_id,
                version: options.version,
                is_enabled: true,
            },
        )
        .await?;
    Ok(())
}

pub(super) async fn machine_validation_test_disable(
    api_client: &ApiClient,
    options: EnableDisableTestOptions,
) -> CarbideCliResult<()> {
    api_client
        .0
        .machine_validation_test_enable_disable_test(
            MachineValidationTestEnableDisableTestRequest {
                test_id: options.test_id,
                version: options.version,
                is_enabled: false,
            },
        )
        .await?;
    Ok(())
}

pub(super) async fn machine_validation_test_update(
    api_client: &ApiClient,
    options: UpdateTestOptions,
) -> CarbideCliResult<()> {
    let payload = forgerpc::machine_validation_test_update_request::Payload {
        contexts: options.contexts,
        img_name: options.img_name,
        execute_in_host: options.execute_in_host,
        container_arg: options.container_arg,
        command: options.command,
        args: options.args,
        extra_err_file: options.extra_err_file,
        external_config_file: options.external_config_file,
        pre_condition: options.pre_condition,
        timeout: options.timeout,
        extra_output_file: options.extra_output_file,
        supported_platforms: options.supported_platforms,
        custom_tags: options.custom_tags,
        components: options.components,
        is_enabled: options.is_enabled,
        plugin: None,
        description: options.description,
        verified: None,
        name: None,
    };
    api_client
        .0
        .update_machine_validation_test(MachineValidationTestUpdateRequest {
            test_id: options.test_id,
            version: options.version,
            payload: Some(payload),
        })
        .await?;
    Ok(())
}

pub(super) async fn machine_validation_test_add(
    api_client: &ApiClient,
    options: AddTestOptions,
) -> CarbideCliResult<()> {
    let mut contexts = vec!["OnDemand".to_string()];
    if !options.contexts.is_empty() {
        contexts = options.contexts;
    }

    let mut supported_platforms = vec!["New_Sku".to_string()];
    if !options.supported_platforms.is_empty() {
        supported_platforms = options.supported_platforms;
    }
    let mut description = Some("new test case".to_string());
    if options.description.is_some() {
        description = options.description;
    }
    let request = forgerpc::MachineValidationTestAddRequest {
        name: options.name,
        description,
        contexts,
        img_name: options.img_name,
        execute_in_host: options.execute_in_host,
        container_arg: options.container_arg,
        command: options.command,
        args: options.args,
        extra_err_file: options.extra_err_file,
        external_config_file: options.external_config_file,
        pre_condition: options.pre_condition,
        timeout: options.timeout,
        extra_output_file: options.extra_output_file,
        supported_platforms,
        read_only: options.read_only,
        custom_tags: options.custom_tags,
        components: options.components,
        is_enabled: options.is_enabled,
        plugin: None,
    };
    api_client.0.add_machine_validation_test(request).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::panic::AssertUnwindSafe;
    use std::time::Duration;

    use ::rpc::forge::{
        BuildInfo, MachineValidationPlugin, MachineValidationTest,
        MachineValidationTestsGetRequest, MachineValidationTestsGetResponse,
    };
    use ::rpc::forge_api_client::ForgeApiClient;
    use ::rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
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

    use super::render_tests_show_output;
    use crate::async_write::CapturedOutput;
    use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
    use crate::cfg::dispatch::Dispatch;
    use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
    use crate::rpc::ApiClient;

    #[test]
    fn tests_show_detail_output_renders_populated_and_empty_plugin_types() {
        let output = render_tests_show_output(
            false,
            MachineValidationTestsGetResponse {
                tests: vec![
                    MachineValidationTest {
                        test_id: "plugin-test".to_string(),
                        plugin: Some(MachineValidationPlugin {
                            r#type: "container".to_string(),
                            image: "registry.example.com/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                            entrypoint: vec!["/plugin/entrypoint".to_string(), "--check-gpus".to_string()],
                            parameters_json: r#"{"expectedGpuCount":8}"#.to_string(),
                            privileged: true,
                            host_access_full: true,
                        }),
                        ..Default::default()
                    },
                    MachineValidationTest {
                        test_id: "legacy-test".to_string(),
                        ..Default::default()
                    },
                ],
            },
        )
        .expect("tests show details render");

        assert!(output.contains("PluginImage"));
        assert!(output.contains("PluginType"));
        assert!(output.contains("container"));
        assert!(output.contains("TestId        : legacy-test"));
        assert!(output.contains("PluginType    : \n"));
        assert!(output.contains("/plugin/entrypoint"));
        assert!(output.contains("PluginParameters"));
        assert!(output.contains("PluginPrivileged: true"));
        assert!(output.contains("PluginHostAccessFull: true"));
    }

    #[tokio::test]
    async fn public_tests_show_command_renders_plugin_type_cells() {
        let command = parse_public_tests_show_command();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock Machine Validation listener binds");
        let address = listener
            .local_addr()
            .expect("mock Machine Validation listener has an address");
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
                format: ::rpc::admin_cli::OutputFormat::AsciiTable,
                request_timeout: client_config.request_timeout,
                page_size: 25,
                extended: false,
                cloud_unsafe_op: None,
                sort_by: SortField::PrimaryId,
            },
            output_file: std::mem::replace(captured.writer(), Box::new(tokio::io::sink())),
        };

        let server = tokio::spawn(async move {
            let (connection, _) = listener.accept().await.expect("mock accepts a client");
            http2::Builder::new(TokioExecutor::new())
                .serve_connection(
                    TokioIo::new(connection),
                    service_fn(mock_tests_show_request),
                )
                .await
                .expect("mock serves the CLI request");
        });
        let result = AssertUnwindSafe(tokio::time::timeout(request_timeout, command.dispatch(ctx)))
            .catch_unwind()
            .await;
        server.abort();
        if let Err(error) = server.await {
            assert!(error.is_cancelled(), "mock server failed: {error}");
        }
        result
            .expect("tests show dispatch does not panic")
            .expect("tests show dispatch completes within the request timeout")
            .expect("tests show dispatch succeeds");

        let display =
            String::from_utf8(captured.into_bytes().await).expect("CLI output is valid UTF-8");
        let rows: Vec<Vec<_>> = display
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| line.trim_matches('|').split('|').map(str::trim).collect())
            .collect();

        assert_eq!(
            rows[0],
            [
                "TestId",
                "Name",
                "Command",
                "Timeout",
                "PluginType",
                "IsVerified",
                "Version",
                "IsEnabled"
            ]
        );
        assert_eq!(rows[1][0], "plugin-test");
        assert_eq!(rows[1][4], "container");
        assert_eq!(rows[2][0], "legacy-test");
        assert_eq!(rows[2][4], "");
    }

    fn parse_public_tests_show_command() -> crate::machine_validation::Cmd {
        let options =
            CliOptions::try_parse_from(["nico-admin-cli", "machine-validation", "tests", "show"])
                .expect("public tests show command parses");
        let Some(CliCommand::MachineValidation(command)) = options.commands else {
            panic!("expected the public Machine Validation command path");
        };
        command
    }

    async fn mock_tests_show_request(
        request: hyper::Request<Incoming>,
    ) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
        Ok(match request.uri().path() {
            "/forge.Forge/Version" => grpc_response(BuildInfo::default()),
            "/forge.Forge/GetMachineValidationTests" => {
                let body = request
                    .into_body()
                    .collect()
                    .await
                    .expect("tests show request body is readable")
                    .to_bytes();
                let payload = body.get(5..).expect("tests show has a gRPC frame");
                let request = MachineValidationTestsGetRequest::decode(payload)
                    .expect("tests show request decodes");
                assert_eq!(request.verified, Some(true));
                grpc_response(MachineValidationTestsGetResponse {
                    tests: vec![
                        MachineValidationTest {
                            test_id: "plugin-test".to_string(),
                            plugin: Some(MachineValidationPlugin {
                                r#type: "container".to_string(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        MachineValidationTest {
                            test_id: "legacy-test".to_string(),
                            ..Default::default()
                        },
                    ],
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
