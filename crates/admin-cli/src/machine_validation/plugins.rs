/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use ::rpc::forge::{
    self as forgerpc, MachineValidationTestEnableDisableTestRequest,
    MachineValidationTestFullHostApprovalRequest, MachineValidationTestVerfiedRequest,
};
use clap::{Parser, ValueEnum};

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

#[derive(Parser, Debug)]
pub(crate) enum Args {
    #[clap(about = "Create an OCI Machine Validation plugin")]
    Create(CreateArgs),
    #[clap(about = "Verify a plugin revision")]
    Verify(RevisionArgs),
    #[clap(about = "Approve full host access for a verified plugin revision")]
    ApproveFullHost(RevisionArgs),
    #[clap(about = "Enable a plugin revision")]
    Enable(RevisionArgs),
    #[clap(about = "Disable a plugin revision")]
    Disable(RevisionArgs),
}

#[derive(Parser, Debug)]
#[command(
    after_long_help = "EXAMPLES:\n\n    Create an unprivileged plugin:\n\n        $ nico-admin-cli machine-validation plugins create --name gpu-health --image registry.example.com/plugins/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --entrypoint /plugin/entrypoint --entrypoint check-gpus --context Discovery --platform HGX-B200 --parameters '{\"expectedGpuCount\":8}'\n\n    Create a privileged plugin with a writable host-root mount:\n\n        $ nico-admin-cli machine-validation plugins create --name host-gpu-health --image registry.example.com/plugins/host-gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --entrypoint /plugin/entrypoint --context Discovery --platform HGX-B200 --parameters '{\"expectedGpuCount\":8}' --privileged --host-access-full\n\n    Create a HealthValidation adapter revision:\n\n        $ nico-admin-cli machine-validation plugins create --name dsx-health --type container --container-plugin-adapter DSXHealthValidation --image ghcr.io/dsx-ai-factory/health-validation@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --entrypoint /health-validation --arg run --context OnDemand --platform GB200 --parameters '{\"gpu_count\":4}'\n"
)]
pub(crate) struct CreateArgs {
    #[clap(long)]
    name: String,
    #[clap(long = "type", value_enum, default_value_t = PluginType::Container)]
    plugin_type: PluginType,
    #[clap(long, value_parser = clap::builder::PossibleValuesParser::new(machine_validation_adapters::adapter_names()), help = "Container contract adapter; omit for the standard NICo contract")]
    container_plugin_adapter: Option<String>,
    #[clap(long)]
    image: String,
    #[clap(long, required = true)]
    entrypoint: Vec<String>,
    #[clap(
        long = "arg",
        help = "Append a command argument; repeat as needed, using --arg=--flag for flags"
    )]
    command_args: Vec<String>,
    #[clap(long, default_value = "{}")]
    parameters: String,
    #[clap(long, default_value = "OnDemand")]
    context: Vec<String>,
    #[clap(long)]
    platform: Vec<String>,
    #[clap(long, default_value_t = 7200)]
    timeout: i64,
    #[clap(long)]
    privileged: bool,
    #[clap(long)]
    host_access_full: bool,
}

impl CreateArgs {
    fn into_request(self) -> forgerpc::MachineValidationTestAddRequest {
        let mut entrypoint = self.entrypoint;
        entrypoint.extend(self.command_args);
        forgerpc::MachineValidationTestAddRequest {
            name: self.name,
            description: None,
            contexts: self.context,
            img_name: None,
            execute_in_host: None,
            container_arg: None,
            command: String::new(),
            args: String::new(),
            extra_err_file: None,
            external_config_file: None,
            pre_condition: None,
            timeout: Some(self.timeout),
            extra_output_file: None,
            supported_platforms: self.platform,
            read_only: None,
            custom_tags: Vec::new(),
            components: Vec::new(),
            is_enabled: None,
            plugin: Some(forgerpc::MachineValidationPlugin {
                r#type: self.plugin_type.as_str().to_owned(),
                container_plugin_adapter: self.container_plugin_adapter,
                image: self.image,
                entrypoint,
                parameters_json: self.parameters,
                privileged: self.privileged,
                host_access_full: self.host_access_full,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PluginType {
    Container,
}

impl PluginType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Container => "container",
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    after_long_help = "EXAMPLES:\n\n    Verify a plugin revision:\n\n        $ nico-admin-cli machine-validation plugins verify --test-id gpu-health --version V1-T1720000000000000\n\n    Approve full-host access after verification:\n\n        $ nico-admin-cli machine-validation plugins approve-full-host --test-id gpu-health --version V1-T1720000000000000\n\n    Enable an approved plugin revision:\n\n        $ nico-admin-cli machine-validation plugins enable --test-id gpu-health --version V1-T1720000000000000\n\n    Disable a plugin revision:\n\n        $ nico-admin-cli machine-validation plugins disable --test-id gpu-health --version V1-T1720000000000000\n"
)]
pub(crate) struct RevisionArgs {
    #[clap(long)]
    test_id: String,
    #[clap(long)]
    version: String,
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        match self {
            Self::Create(args) => {
                let response = ctx
                    .api_client
                    .0
                    .add_machine_validation_test(args.into_request())
                    .await?;
                println!(
                    "Created plugin revision: {} {}",
                    response.test_id, response.version
                );
            }
            Self::Verify(args) => {
                let response = ctx
                    .api_client
                    .0
                    .machine_validation_test_verfied(MachineValidationTestVerfiedRequest {
                        test_id: args.test_id,
                        version: args.version,
                    })
                    .await?;
                println!("{}", response.message);
            }
            Self::ApproveFullHost(args) => {
                let response = ctx
                    .api_client
                    .0
                    .machine_validation_test_approve_full_host(
                        MachineValidationTestFullHostApprovalRequest {
                            test_id: args.test_id,
                            version: args.version,
                        },
                    )
                    .await?;
                println!("{}", response.message);
            }
            Self::Enable(args) => {
                let response = ctx
                    .api_client
                    .0
                    .machine_validation_test_enable_disable_test(
                        MachineValidationTestEnableDisableTestRequest {
                            test_id: args.test_id,
                            version: args.version,
                            is_enabled: true,
                        },
                    )
                    .await?;
                println!("{}", response.message);
            }
            Self::Disable(args) => {
                let response = ctx
                    .api_client
                    .0
                    .machine_validation_test_enable_disable_test(
                        MachineValidationTestEnableDisableTestRequest {
                            test_id: args.test_id,
                            version: args.version,
                            is_enabled: false,
                        },
                    )
                    .await?;
                println!("{}", response.message);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    use super::Args;

    #[test]
    fn create_keeps_adapter_selection_and_ordered_arguments_in_the_request() {
        let Args::Create(args) = Args::try_parse_from([
            "plugins", "create", "--name", "dsx-health", "--type", "container",
            "--container-plugin-adapter", "DSXHealthValidation", "--image", "example.com/health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--entrypoint", "/custom/hv", "--arg", "run", "--arg=--execution-context", "--arg", "OPERATOR_INFRA",
            "--parameters", r#"{"gpu_count":4}"#,
        ]).unwrap() else { panic!("expected create"); };
        let plugin = args.into_request().plugin.unwrap();
        assert_eq!(plugin.r#type, "container");
        assert_eq!(
            plugin.container_plugin_adapter.as_deref(),
            Some("DSXHealthValidation")
        );
        assert_eq!(
            plugin.entrypoint,
            ["/custom/hv", "run", "--execution-context", "OPERATOR_INFRA"]
        );
        assert!(!plugin.privileged);
    }

    #[test]
    fn legacy_command_syntax_defaults_to_the_standard_container_contract() {
        let Args::Create(args) = Args::try_parse_from([
            "plugins",
            "create",
            "--name",
            "custom",
            "--image",
            "example.com/health",
            "--entrypoint",
            "/check",
            "--entrypoint",
            "run",
        ])
        .unwrap() else {
            panic!("expected create");
        };
        let plugin = args.into_request().plugin.unwrap();
        assert_eq!(plugin.r#type, "container");
        assert!(plugin.container_plugin_adapter.is_none());
        assert_eq!(plugin.entrypoint, ["/check", "run"]);
    }

    #[test]
    fn unknown_adapter_is_rejected_before_sending_a_request() {
        let error = Args::try_parse_from([
            "plugins",
            "create",
            "--name",
            "custom",
            "--image",
            "example.com/health",
            "--entrypoint",
            "/check",
            "--container-plugin-adapter",
            "unknown",
        ])
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn parses_a_full_host_plugin_definition() {
        let args = Args::try_parse_from([
            "plugins",
            "create",
            "--name",
            "gpu-health",
            "--image",
            "registry.example.com/plugins/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--entrypoint",
            "/plugin/entrypoint",
            "--privileged",
            "--host-access-full",
        ]);
        assert!(args.is_ok());
    }

    #[test]
    fn rejects_an_unsupported_plugin_type_before_calling_the_api() {
        let error = Args::try_parse_from([
            "plugins",
            "create",
            "--name",
            "gpu-health",
            "--type",
            "script",
            "--image",
            "registry.example.com/plugins/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--entrypoint",
            "/plugin/entrypoint",
        ])
        .expect_err("unsupported plugin type must fail CLI parsing");

        assert_eq!(error.kind(), ErrorKind::InvalidValue);
        assert!(error.to_string().contains("possible values: container"));
    }

    #[test]
    fn plugin_lifecycle_help_includes_worked_examples() {
        let mut command = Args::command();
        let help = command
            .find_subcommand_mut("verify")
            .expect("verify command exists")
            .render_long_help()
            .to_string();

        assert!(help.contains("plugins verify --test-id gpu-health"));
        assert!(help.contains("plugins approve-full-host --test-id gpu-health"));
        assert!(help.contains("plugins enable --test-id gpu-health"));
        assert!(help.contains("plugins disable --test-id gpu-health"));
    }
}
