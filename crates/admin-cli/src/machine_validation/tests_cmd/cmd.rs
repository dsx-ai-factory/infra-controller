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
        let _ = show_tests_details(output_format == OutputFormat::Json, tests);
    } else {
        convert_tests_to_nice_table(tests.tests).printstd();
    }

    Ok(())
}

fn show_tests_details(
    is_json: bool,
    test: forgerpc::MachineValidationTestsGetResponse,
) -> CarbideCliResult<()> {
    if is_json {
        for test in test.tests {
            println!("{}", serde_json::to_string_pretty(&test)?);
        }
    } else {
        println!(
            "{}",
            convert_tests_to_nice_format(test.tests).unwrap_or_else(|x| x.to_string())
        );
    }
    Ok(())
}

fn convert_tests_to_nice_table(tests: Vec<forgerpc::MachineValidationTest>) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row![
        "TestId",
        "Name",
        "Command",
        "Timeout",
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
    use ::rpc::forge::{MachineValidationPlugin, MachineValidationTest};

    use super::convert_tests_to_nice_format;

    #[test]
    fn detailed_test_output_includes_plugin_definition() {
        let output = convert_tests_to_nice_format(vec![MachineValidationTest {
            plugin: Some(MachineValidationPlugin {
                image: "registry.example.com/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                entrypoint: vec!["/plugin/entrypoint".to_string(), "--check-gpus".to_string()],
                parameters_json: r#"{"expectedGpuCount":8}"#.to_string(),
                privileged: true,
                host_access_full: true,
            }),
            ..Default::default()
        }])
        .expect("plugin details render");

        assert!(output.contains("PluginImage"));
        assert!(output.contains("/plugin/entrypoint"));
        assert!(output.contains("PluginParameters"));
        assert!(output.contains("PluginPrivileged: true"));
        assert!(output.contains("PluginHostAccessFull: true"));
    }
}
