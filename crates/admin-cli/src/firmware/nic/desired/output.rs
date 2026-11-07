/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use chrono::{DateTime, Utc};
use eyre::{Context, eyre};
use model::nic_firmware::NicFirmwareApproach;
use prettytable::{Cell, Row, Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{self, GetDesiredNicFirmwareResponse, NicFirmwareSelectionScope};
use rpc::protos::mlx_device::{FirmwareSpec, MlxDeviceIdentity};
use serde::Serialize;

use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::{async_write, async_write_table_as_csv, async_writeln};

/// `DesiredOutput` preserves the server report without resolving selections or
/// interpreting errors as permission to inherit a different profile.
#[derive(Serialize)]
struct DesiredOutput {
    observed_at: Option<DateTime<Utc>>,
    targets: Vec<TargetOutput>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct TargetOutput {
    identity: MlxDeviceIdentity,
    result: Option<TargetResult>,
    site_selection_version: Option<String>,
}

#[derive(Serialize)]
enum TargetResult {
    Firmware(FirmwareOutput),
    Error(String),
}

#[derive(Serialize)]
struct FirmwareOutput {
    #[serde(with = "SelectionScope")]
    scope: NicFirmwareSelectionScope,
    profile_id: String,
    profile_version: String,
    entry: EntryOutput,
}

// The generated scope has no serde implementation. Keep the wire enum as the
// field's type, with named variants instead of its integer discriminant.
#[derive(Serialize)]
#[serde(remote = "NicFirmwareSelectionScope")]
enum SelectionScope {
    Site,
    Allocation,
    Machine,
    Card,
}

#[derive(Serialize)]
struct EntryOutput {
    firmware: FirmwareSpec,
    image: ArtifactOutput,
    device_config: Option<ArtifactOutput>,
    approach: NicFirmwareApproach,
}

#[derive(Serialize)]
struct ArtifactOutput {
    url: String,
    sha256: String,
}

impl From<forge::NicFirmwareArtifact> for ArtifactOutput {
    fn from(artifact: forge::NicFirmwareArtifact) -> Self {
        Self {
            url: artifact.url,
            sha256: artifact.sha256,
        }
    }
}

impl TryFrom<forge::NicFirmwareResolution> for FirmwareOutput {
    type Error = eyre::Report;

    fn try_from(resolution: forge::NicFirmwareResolution) -> Result<Self, Self::Error> {
        let scope = NicFirmwareSelectionScope::try_from(resolution.scope)
            .wrap_err("core returned an unsupported NIC firmware selection scope")?;
        let entry = resolution
            .entry
            .ok_or_else(|| eyre!("core returned a NIC firmware resolution without an entry"))?;
        let approach = match forge::NicFirmwareApproach::try_from(entry.approach)
            .wrap_err("core returned an unsupported NIC firmware approach")?
        {
            forge::NicFirmwareApproach::Scout => NicFirmwareApproach::Scout,
        };
        Ok(Self {
            scope,
            profile_id: resolution.profile_id,
            profile_version: resolution.profile_version,
            entry: EntryOutput {
                firmware: entry
                    .firmware
                    .ok_or_else(|| eyre!("core returned a NIC firmware entry without firmware"))?,
                image: entry
                    .image
                    .ok_or_else(|| eyre!("core returned a NIC firmware entry without an image"))?
                    .into(),
                device_config: entry.device_config.map(Into::into),
                approach,
            },
        })
    }
}

impl TryFrom<forge::NicFirmwareDeviceTarget> for TargetOutput {
    type Error = eyre::Report;

    fn try_from(target: forge::NicFirmwareDeviceTarget) -> Result<Self, Self::Error> {
        let identity = target
            .identity
            .ok_or_else(|| eyre!("core returned a NIC firmware target without identity"))?;
        if identity.device_info.is_none() {
            return Err(eyre!(
                "core returned a NIC firmware identity without device facts"
            ));
        }
        let result = match target.result {
            Some(forge::nic_firmware_device_target::Result::Firmware(firmware)) => {
                Some(TargetResult::Firmware(firmware.try_into()?))
            }
            Some(forge::nic_firmware_device_target::Result::Error(error)) => {
                Some(TargetResult::Error(error))
            }
            None => None,
        };
        Ok(Self {
            identity,
            result,
            site_selection_version: target.site_selection_version,
        })
    }
}

impl DesiredOutput {
    fn table(&self, format: OutputFormat) -> eyre::Result<Table> {
        let mut table = Table::new();
        let mut titles = row![
            "Device",
            "Base MAC",
            "Base GUID",
            "Part Number",
            "PSID",
            "Observed Firmware",
            "Target Firmware",
            "Status",
            "Scope",
            "Profile ID",
            "Profile Version",
            "Site Selection Version",
            "Error"
        ];
        let observed_at = self
            .observed_at
            .map(|timestamp| timestamp.to_rfc3339())
            .unwrap_or_else(|| "Not observed".to_string());
        let warnings = self.warnings.join("\n");
        if matches!(format, OutputFormat::Csv) {
            titles.add_cell(Cell::new("Observed At"));
            titles.add_cell(Cell::new("Warnings"));
        }
        table.set_titles(titles);
        for target in &self.targets {
            let device = target.identity.device_info.as_ref().ok_or_else(|| {
                eyre!("core returned a NIC firmware identity without device facts")
            })?;
            let (status, scope, firmware, profile_id, profile_version, error) = match &target.result
            {
                Some(TargetResult::Firmware(firmware)) => (
                    "Resolved",
                    match firmware.scope {
                        NicFirmwareSelectionScope::Site => "Site",
                        NicFirmwareSelectionScope::Allocation => "Allocation",
                        NicFirmwareSelectionScope::Machine => "Machine",
                        NicFirmwareSelectionScope::Card => "Card",
                    },
                    firmware.entry.firmware.version.as_str(),
                    firmware.profile_id.as_str(),
                    firmware.profile_version.as_str(),
                    "",
                ),
                Some(TargetResult::Error(error)) => ("Error", "", "", "", "", error.as_str()),
                None => ("Unmanaged", "", "", "", "", ""),
            };
            let mut row = row![
                device.pci_name,
                device.base_mac,
                device.base_guid.as_deref().unwrap_or(""),
                device.part_number,
                device.psid,
                device.fw_version_current,
                firmware,
                status,
                scope,
                profile_id,
                profile_version,
                target.site_selection_version.as_deref().unwrap_or(""),
                error
            ];
            if matches!(format, OutputFormat::Csv) {
                row.add_cell(Cell::new(&observed_at));
                row.add_cell(Cell::new(&warnings));
            }
            table.add_row(row);
        }
        // CSV cannot put report metadata before the header. Keep an explicit
        // metadata row when there are no devices so warnings are still visible.
        if matches!(format, OutputFormat::Csv) && self.targets.is_empty() {
            table.add_row(Row::new(
                [
                    "",
                    "",
                    "",
                    "",
                    "",
                    "",
                    "",
                    "No devices",
                    "",
                    "",
                    "",
                    "",
                    "",
                    &observed_at,
                    &warnings,
                ]
                .into_iter()
                .map(Cell::new)
                .collect(),
            ));
        }
        Ok(table)
    }
}

pub(super) async fn write_desired(
    response: GetDesiredNicFirmwareResponse,
    ctx: &mut RuntimeContext,
) -> CarbideCliResult<()> {
    let report = DesiredOutput {
        observed_at: response
            .observed_at
            .map(DateTime::<Utc>::try_from)
            .transpose()
            .wrap_err("core returned an invalid NIC observation timestamp")?,
        targets: response
            .targets
            .into_iter()
            .enumerate()
            .map(|(index, target)| {
                TargetOutput::try_from(target)
                    .wrap_err_with(|| format!("while attempting to decode NIC target {index}"))
            })
            .collect::<eyre::Result<Vec<_>>>()?,
        warnings: response.warnings,
    };
    match ctx.config.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&report)
                .wrap_err("while attempting to serialize desired NIC firmware as JSON")?;
            async_writeln!(ctx.output_file, "{json}")
                .wrap_err("while attempting to write desired NIC firmware")?;
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&report)
                .wrap_err("while attempting to serialize desired NIC firmware as YAML")?;
            async_write!(ctx.output_file, "{yaml}")
                .wrap_err("while attempting to write desired NIC firmware")?;
        }
        OutputFormat::AsciiTable => {
            let table = report.table(ctx.config.format)?;
            let observed_at = report
                .observed_at
                .map(|timestamp| timestamp.to_rfc3339())
                .unwrap_or_else(|| "Not observed".to_string());
            async_writeln!(ctx.output_file, "Scout observed at: {observed_at}")
                .wrap_err("while attempting to write the NIC observation time")?;
            for warning in &report.warnings {
                async_writeln!(ctx.output_file, "Warning: {warning}")
                    .wrap_err("while attempting to write a NIC firmware warning")?;
            }
            async_write!(ctx.output_file, "{table}")
                .wrap_err("while attempting to write desired NIC firmware")?;
        }
        OutputFormat::Csv => {
            async_write_table_as_csv!(ctx.output_file, report.table(ctx.config.format)?)
                .wrap_err("while attempting to write desired NIC firmware as CSV")?;
        }
    }
    Ok(())
}
