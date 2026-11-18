/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::{Context, eyre};
use model::nic_firmware::{NicFirmwareApproach, NicFirmwareProfileConfig, NicFirmwareProfileId};
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{NicFirmwareProfile, NicFirmwareProfileResponse};
use serde::Serialize;

use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::{async_write, async_write_table_as_csv, async_writeln};

pub(super) async fn write_mutation_result(
    response: NicFirmwareProfileResponse,
    id: &NicFirmwareProfileId,
    ctx: &mut RuntimeContext,
) -> CarbideCliResult<()> {
    let result = async {
        let profile = response
            .profile
            .ok_or_else(|| eyre!("core returned no NIC firmware profile"))?;
        write_profiles(vec![profile], ctx).await
    }
    .await;
    Ok(result.wrap_err_with(|| {
        format!(
            "core accepted the change to NIC firmware profile {id}, but its result could not be displayed; use the firmware nic profile show command with the same profile ID to inspect the definition before another write"
        )
    })?)
}

// Reusing the model config keeps JSON output compatible with create/update
// input. Its RPC conversion rejects definitions this client cannot represent.
#[derive(Serialize)]
struct ProfileOutput {
    id: String,
    config: NicFirmwareProfileConfig,
    version: String,
}

impl TryFrom<NicFirmwareProfile> for ProfileOutput {
    type Error = eyre::Report;

    fn try_from(profile: NicFirmwareProfile) -> Result<Self, Self::Error> {
        let config = profile.config.ok_or_else(|| {
            eyre!(
                "core returned NIC firmware profile {} without a config",
                profile.id
            )
        })?;
        Ok(Self {
            config: config.try_into().wrap_err_with(|| {
                format!(
                    "while attempting to decode NIC firmware profile {}",
                    profile.id
                )
            })?,
            id: profile.id,
            version: profile.version,
        })
    }
}

pub(super) async fn write_profiles(
    profiles: Vec<NicFirmwareProfile>,
    ctx: &mut RuntimeContext,
) -> CarbideCliResult<()> {
    let profiles = profiles
        .into_iter()
        .map(ProfileOutput::try_from)
        .collect::<eyre::Result<Vec<_>>>()?;
    match ctx.config.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&profiles)
                .wrap_err("while attempting to serialize NIC firmware profiles as JSON")?;
            async_writeln!(ctx.output_file, "{json}")
                .wrap_err("while attempting to write NIC firmware profiles")?;
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&profiles)
                .wrap_err("while attempting to serialize NIC firmware profiles as YAML")?;
            async_write!(ctx.output_file, "{yaml}")
                .wrap_err("while attempting to write NIC firmware profiles")?;
        }
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            let mut table = Table::new();
            table.set_titles(row![
                "Profile ID",
                "Profile Version",
                "Part Number",
                "PSID",
                "Firmware Version",
                "Approach"
            ]);
            for profile in profiles {
                for entry in profile.config.entries {
                    let approach = match entry.approach {
                        NicFirmwareApproach::Scout => "Scout",
                    };
                    table.add_row(row![
                        profile.id,
                        profile.version,
                        entry.firmware.part_number,
                        entry.firmware.psid,
                        entry.firmware.version,
                        approach
                    ]);
                }
            }
            match ctx.config.format {
                OutputFormat::Csv => async_write_table_as_csv!(ctx.output_file, table)
                    .wrap_err("while attempting to write NIC firmware profiles as CSV")?,
                _ => async_write!(ctx.output_file, "{table}")
                    .wrap_err("while attempting to write NIC firmware profiles")?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use model::nic_firmware::NicFirmwareProfileId;
    use rpc::admin_cli::OutputFormat;
    use rpc::forge::{
        NicFirmwareApproach, NicFirmwareArtifact, NicFirmwareProfile, NicFirmwareProfileConfig,
        NicFirmwareProfileEntry, NicFirmwareProfileResponse,
    };
    use rpc::forge_api_client::ForgeApiClient;
    use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
    use rpc::protos::mlx_device::FirmwareSpec;

    use super::write_mutation_result;
    use crate::cfg::cli_options::SortField;
    use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
    use crate::errors::CarbideCliError;
    use crate::rpc::ApiClient;

    /// Output failure must preserve the accepted change and its I/O cause so
    /// operators inspect the stored definition before attempting another write.
    #[tokio::test]
    async fn accepted_mutation_preserves_output_failure_and_recovery_guidance() {
        let id: NicFirmwareProfileId = "connectx-production".parse().expect("valid profile ID");
        let response = NicFirmwareProfileResponse {
            profile: Some(NicFirmwareProfile {
                id: id.to_string(),
                config: Some(NicFirmwareProfileConfig {
                    entries: vec![NicFirmwareProfileEntry {
                        firmware: Some(FirmwareSpec {
                            part_number: "MCX75310AAS-NEAT".into(),
                            psid: "MT_0000000838".into(),
                            version: "28.43.1014".into(),
                        }),
                        image: Some(NicFirmwareArtifact {
                            url: "https://firmware.invalid/image.bin".into(),
                            sha256: "ab".repeat(32),
                        }),
                        device_config: None,
                        approach: NicFirmwareApproach::Scout.into(),
                    }],
                }),
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

        let error = write_mutation_result(response, &id, &mut ctx)
            .await
            .expect_err("closed output must fail after accepting the complete response");
        let CarbideCliError::EyreReport(report) = error else {
            panic!("expected an EyreReport");
        };
        let message = report.to_string();
        assert!(
            message.contains(&format!(
                "core accepted the change to NIC firmware profile {id}"
            )),
            "{message}"
        );
        assert!(
            message.contains("use the firmware nic profile show command with the same profile ID"),
            "{message}"
        );
        assert!(
            message.contains("inspect the definition before another write"),
            "{message}"
        );
        let io_error = report
            .chain()
            .find_map(|cause| cause.downcast_ref::<std::io::Error>())
            .expect("I/O source remains in the error chain");
        assert_eq!(io_error.kind(), std::io::ErrorKind::BrokenPipe);
    }
}
