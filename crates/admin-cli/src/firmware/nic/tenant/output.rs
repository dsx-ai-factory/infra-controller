/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use config_version::ConfigVersion;
use eyre::{Context, eyre};
use model::nic_firmware::NicFirmwareProfileId;
use model::tenant::TenantOrganizationId;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::TenantNicFirmwareConfig;
use serde::Serialize;

use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::{async_write, async_write_table_as_csv, async_writeln};

pub(super) async fn write_mutation_result(
    config: Option<TenantNicFirmwareConfig>,
    tenant_org_id: &TenantOrganizationId,
    ctx: &mut RuntimeContext,
) -> CarbideCliResult<()> {
    Ok(write_config(config, tenant_org_id, ctx).await.wrap_err_with(|| {
        format!(
            "core accepted the NIC firmware selection change for tenant {tenant_org_id}, but its result could not be displayed; use firmware nic tenant show --tenant-org-id {tenant_org_id} before another write"
        )
    })?)
}

#[derive(Serialize)]
struct ConfigOutput {
    tenant_org_id: TenantOrganizationId,
    profile_id: Option<NicFirmwareProfileId>,
    config_version: ConfigVersion,
}

impl TryFrom<TenantNicFirmwareConfig> for ConfigOutput {
    type Error = eyre::Report;

    fn try_from(config: TenantNicFirmwareConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_org_id: config
                .tenant_organization_id
                .parse()
                .wrap_err("core returned an invalid tenant organization ID")?,
            profile_id: config
                .profile_id
                .map(|profile_id| profile_id.parse())
                .transpose()
                .wrap_err("core returned an invalid tenant NIC firmware profile ID")?,
            config_version: config
                .config_version
                .parse()
                .wrap_err("core returned an invalid tenant version")?,
        })
    }
}

pub(super) async fn write_config(
    config: Option<TenantNicFirmwareConfig>,
    tenant_org_id: &TenantOrganizationId,
    ctx: &mut RuntimeContext,
) -> CarbideCliResult<()> {
    let config = config.ok_or_else(|| {
        eyre!("core response omitted NIC firmware configuration for tenant {tenant_org_id}")
    })?;
    let config = ConfigOutput::try_from(config)?;
    if &config.tenant_org_id != tenant_org_id {
        return Err(eyre!(
            "core returned NIC firmware configuration for tenant {} when {tenant_org_id} was requested",
            config.tenant_org_id
        )
        .into());
    }
    match ctx.config.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&config).wrap_err(
                "while attempting to serialize tenant NIC firmware configuration as JSON",
            )?;
            async_writeln!(ctx.output_file, "{json}")
                .wrap_err("while attempting to write tenant NIC firmware configuration")?;
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&config).wrap_err(
                "while attempting to serialize tenant NIC firmware configuration as YAML",
            )?;
            async_write!(ctx.output_file, "{yaml}")
                .wrap_err("while attempting to write tenant NIC firmware configuration")?;
        }
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            let mut table = Table::new();
            table.set_titles(row!["Tenant Org ID", "Tenant Version", "Profile ID"]);
            table.add_row(row![
                config.tenant_org_id,
                config.config_version,
                config
                    .profile_id
                    .map(|profile| profile.to_string())
                    .unwrap_or_default()
            ]);
            match ctx.config.format {
                OutputFormat::Csv => async_write_table_as_csv!(ctx.output_file, table).wrap_err(
                    "while attempting to write tenant NIC firmware configuration as CSV",
                )?,
                _ => async_write!(ctx.output_file, "{table}")
                    .wrap_err("while attempting to write tenant NIC firmware configuration")?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::FailsWith;
    use carbide_test_support::{Case, check_cases_async};
    use rpc::admin_cli::OutputFormat;
    use rpc::forge::TenantNicFirmwareConfig;
    use rpc::forge_api_client::ForgeApiClient;
    use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};

    use super::write_mutation_result;
    use crate::async_write::CapturedOutput;
    use crate::cfg::cli_options::SortField;
    use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
    use crate::errors::CarbideCliError;
    use crate::rpc::ApiClient;

    /// A successful RPC does not make its response trustworthy: reject an unusable
    /// token or another tenant's result without printing success or suggesting a retry.
    #[tokio::test]
    async fn accepted_mutation_rejects_invalid_versions_and_wrong_tenants_without_output() {
        check_cases_async(
            [
                // Valid identity lets this response reach the token validation boundary.
                Case {
                    scenario: "valid tenant identity does not mask an invalid version",
                    input: TenantNicFirmwareConfig {
                        tenant_organization_id: "fds34511233a".into(),
                        profile_id: Some("connectx-production".into()),
                        config_version: "invalid-version".into(),
                    },
                    expect: FailsWith("invalid version format: invalid-version".to_string()),
                },
                // Individually valid fields must still belong to the requested tenant.
                Case {
                    scenario: "valid response fields still require the requested tenant",
                    input: TenantNicFirmwareConfig {
                        tenant_organization_id: "other-tenant".into(),
                        profile_id: Some("connectx-production".into()),
                        config_version: "V8-T1789080000000001".into(),
                    },
                    expect: FailsWith(
                        "core returned NIC firmware configuration for tenant other-tenant when fds34511233a was requested"
                            .to_string(),
                    ),
                },
            ],
            |response| async move {
                let tenant_org_id = "fds34511233a".parse().expect("valid tenant ID");
                let mut captured = CapturedOutput::new();
                let mut ctx = RuntimeContext {
                    api_client: ApiClient(ForgeApiClient::new(&ApiConfig::new(
                        "http://127.0.0.1:1",
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
                    output_file: std::mem::replace(
                        captured.writer(),
                        Box::new(tokio::io::sink()),
                    ),
                };

                let result = write_mutation_result(Some(response), &tenant_org_id, &mut ctx).await;
                drop(ctx);
                assert!(captured.into_bytes().await.is_empty());
                result.map_err(|error| {
                    let CarbideCliError::EyreReport(report) = error else {
                        panic!("expected an EyreReport");
                    };
                    let message = report.to_string();
                    for expected in [
                        "core accepted the NIC firmware selection change for tenant fds34511233a",
                        "use firmware nic tenant show --tenant-org-id fds34511233a before another write",
                    ] {
                        assert!(message.contains(expected), "{message}");
                    }
                    report.root_cause().to_string()
                })
            },
        )
        .await;
    }

    /// Output can fail after Core commits; retain the I/O cause and direct the
    /// operator to reread rather than presenting the accepted write as retryable.
    #[tokio::test]
    async fn accepted_mutation_preserves_output_failure_and_recovery_guidance() {
        let tenant_org_id = "fds34511233a".parse().expect("valid tenant ID");
        let response = TenantNicFirmwareConfig {
            tenant_organization_id: "fds34511233a".into(),
            profile_id: Some("connectx-production".into()),
            config_version: "V8-T1789080000000001".into(),
        };
        // The valid response must reach the writer, where the closed reader
        // causes BrokenPipe. A decoding failure would not exercise that boundary.
        let (writer, reader) = tokio::io::duplex(64);
        drop(reader);
        let mut ctx = RuntimeContext {
            api_client: ApiClient(ForgeApiClient::new(&ApiConfig::new(
                "http://127.0.0.1:1",
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
            output_file: Box::new(writer),
        };

        let error = write_mutation_result(Some(response), &tenant_org_id, &mut ctx)
            .await
            .expect_err("closed output must fail");
        let CarbideCliError::EyreReport(report) = error else {
            panic!("expected an EyreReport");
        };
        let message = report.to_string();
        for expected in [
            "core accepted the NIC firmware selection change for tenant fds34511233a",
            "use firmware nic tenant show --tenant-org-id fds34511233a before another write",
        ] {
            assert!(message.contains(expected), "{message}");
        }
        let io_error = report
            .chain()
            .find_map(|cause| cause.downcast_ref::<std::io::Error>())
            .expect("I/O source remains in the error chain");
        assert_eq!(io_error.kind(), std::io::ErrorKind::BrokenPipe);
    }
}
