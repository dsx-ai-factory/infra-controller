/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::{Context, eyre};
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{NicFirmwareSiteSelection, NicFirmwareSiteSelectionResponse};
use serde::Serialize;

use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::{async_write, async_write_table_as_csv, async_writeln};

pub(super) async fn write_mutation_result(
    response: NicFirmwareSiteSelectionResponse,
    ctx: &mut RuntimeContext,
) -> CarbideCliResult<()> {
    let result = async {
        let selection = response
            .site_selection
            .ok_or_else(|| eyre!("core returned no site NIC firmware selection"))?;
        write_selections(vec![selection], ctx).await
    }
    .await;
    Ok(result.wrap_err(
        "core accepted the site NIC firmware selection, but its result could not be displayed; use firmware nic site list to inspect the hardware pair before another write",
    )?)
}

#[derive(Serialize)]
struct SelectionOutput {
    hardware: HardwareOutput,
    profile_id: String,
    version: String,
}

#[derive(Serialize)]
struct HardwareOutput {
    part_number: String,
    psid: String,
}

impl TryFrom<NicFirmwareSiteSelection> for SelectionOutput {
    type Error = eyre::Report;

    fn try_from(selection: NicFirmwareSiteSelection) -> Result<Self, Self::Error> {
        let hardware = selection.hardware.ok_or_else(|| {
            eyre!(
                "core returned a site NIC firmware selection for profile {} without hardware",
                selection.profile_id
            )
        })?;
        Ok(Self {
            hardware: HardwareOutput {
                part_number: hardware.part_number,
                psid: hardware.psid,
            },
            profile_id: selection.profile_id,
            version: selection.version,
        })
    }
}

pub(super) async fn write_selections(
    selections: Vec<NicFirmwareSiteSelection>,
    ctx: &mut RuntimeContext,
) -> CarbideCliResult<()> {
    let selections = selections
        .into_iter()
        .map(SelectionOutput::try_from)
        .collect::<eyre::Result<Vec<_>>>()?;
    match ctx.config.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&selections)
                .wrap_err("while attempting to serialize site NIC firmware selections as JSON")?;
            async_writeln!(ctx.output_file, "{json}")
                .wrap_err("while attempting to write site NIC firmware selections")?;
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&selections)
                .wrap_err("while attempting to serialize site NIC firmware selections as YAML")?;
            async_write!(ctx.output_file, "{yaml}")
                .wrap_err("while attempting to write site NIC firmware selections")?;
        }
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            let mut table = Table::new();
            table.set_titles(row![
                "Part Number",
                "PSID",
                "Profile ID",
                "Selection Version"
            ]);
            for selection in selections {
                table.add_row(row![
                    selection.hardware.part_number,
                    selection.hardware.psid,
                    selection.profile_id,
                    selection.version
                ]);
            }
            match ctx.config.format {
                OutputFormat::Csv => async_write_table_as_csv!(ctx.output_file, table)
                    .wrap_err("while attempting to write site NIC firmware selections as CSV")?,
                _ => async_write!(ctx.output_file, "{table}")
                    .wrap_err("while attempting to write site NIC firmware selections")?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rpc::admin_cli::OutputFormat;
    use rpc::forge::{
        NicFirmwareHardware, NicFirmwareSiteSelection, NicFirmwareSiteSelectionResponse,
    };
    use rpc::forge_api_client::ForgeApiClient;
    use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};

    use super::write_mutation_result;
    use crate::cfg::cli_options::SortField;
    use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
    use crate::errors::CarbideCliError;
    use crate::rpc::ApiClient;

    /// A failed output write must preserve Core's acceptance and its I/O cause
    /// so operators inspect the saved selection before attempting another write.
    #[tokio::test]
    async fn accepted_selection_preserves_output_failure_and_recovery_guidance() {
        // A complete response reaches the writer rather than failing to decode.
        let response = NicFirmwareSiteSelectionResponse {
            site_selection: Some(NicFirmwareSiteSelection {
                hardware: Some(NicFirmwareHardware {
                    part_number: "MCX75310AAS-NEAT".into(),
                    psid: "MT_0000000838".into(),
                }),
                profile_id: "connectx-production".into(),
                version: "V7-T1789080000000000".into(),
            }),
        };
        // Closing the reader makes the first output write fail with `BrokenPipe`.
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

        let error = write_mutation_result(response, &mut ctx)
            .await
            .expect_err("closed output must fail after accepting the complete response");
        let CarbideCliError::EyreReport(report) = error else {
            panic!("expected an EyreReport");
        };
        let message = report.to_string();
        assert!(
            message.contains("core accepted the site NIC firmware selection"),
            "{message}"
        );
        assert!(
            message.contains("use firmware nic site list to inspect the hardware pair"),
            "{message}"
        );
        assert!(message.contains("before another write"), "{message}");
        let io_error = report
            .chain()
            .find_map(|cause| cause.downcast_ref::<std::io::Error>())
            .expect("I/O source remains in the error chain");
        assert_eq!(io_error.kind(), std::io::ErrorKind::BrokenPipe);
    }
}
