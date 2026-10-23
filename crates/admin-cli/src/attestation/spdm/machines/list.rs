// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_uuid::machine::MachineId;
use clap::Parser;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{
    ListSpdmMachineAttestationsRequest, ListSpdmMachineAttestationsResponse, SpdmAttestationStatus,
};
use serde::Serialize;
use tonic::Status;
use uuid::Uuid;

use crate::async_writeln;
use crate::attestation::spdm::write_output;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

#[cfg(test)]
mod tests;

const PREDICTION_WARNING: &str = "Predicted attestation status, for definitive attestation status use \"attestation spdm machines list --machine-id <your machine id>\"";

#[derive(Parser, Debug)]
#[command(group(clap::ArgGroup::new("status").args(["passed", "failed", "cancelled", "in_progress"])))]
#[command(
    long_about = "List predicted SPDM attestation statuses in machine-ID order.\n\n\
    Uses every device in the latest retained controller-triggered job for each machine, chosen by \
    descending start time and then descending job ID. External and older jobs are ignored; BMCs are not contacted. \
    Unfinished devices take precedence over failures, cancellations, then all-passed. \
    Without a status flag, lists all machines; machines without a controller job show unknown \
    with an empty job ID; JSON and YAML use null for both fields. Such machines are excluded by every status flag. \
    Only one status flag may be supplied. \
    A partially collected job is in progress even if a device has failed. \
    The CLI fetches all pages automatically. Predictions can change between pages. Prints the prediction warning once to stderr, \
    including when no machines match. Supports table, JSON, and YAML output; CSV is unsupported."
)]
#[command(after_long_help = "\
EXAMPLES:

List all machine attestation predictions:
    $ nico-admin-cli attestation spdm machines list

List machines predicted to have passed:
    $ nico-admin-cli attestation spdm machines list --passed

List machines predicted to have failed:
    $ nico-admin-cli attestation spdm machines list --failed

List machines predicted to have been cancelled:
    $ nico-admin-cli attestation spdm machines list --cancelled

List machines whose predicted attestation is in progress:
    $ nico-admin-cli attestation spdm machines list --in-progress

")]
pub(crate) struct Args {
    #[arg(
        long,
        help = "Only machines whose latest controller job passed for every device"
    )]
    passed: bool,
    #[arg(
        long,
        help = "Only machines whose latest controller job failed with no unfinished devices"
    )]
    failed: bool,
    #[arg(
        long,
        help = "Only machines whose latest controller job has cancellations, with no unfinished or failed devices"
    )]
    cancelled: bool,
    #[arg(
        long,
        help = "Only machines whose latest controller job has unfinished devices, even if another device failed"
    )]
    in_progress: bool,
}

/// The paginated listing used by the public command.
trait MachineAttestationClient {
    async fn list(
        &self,
        request: ListSpdmMachineAttestationsRequest,
    ) -> Result<ListSpdmMachineAttestationsResponse, Status>;
}

impl MachineAttestationClient for ApiClient {
    async fn list(
        &self,
        request: ListSpdmMachineAttestationsRequest,
    ) -> Result<ListSpdmMachineAttestationsResponse, Status> {
        self.0.list_spdm_machine_attestations(request).await
    }
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        // The listing RPC applies the server cap, avoiding a separate config lookup.
        let page_size = u32::try_from(ctx.config.page_size).unwrap_or(u32::MAX);
        let mut warnings: Box<dyn tokio::io::AsyncWrite + Unpin> = Box::new(tokio::io::stderr());
        self.execute(
            &ctx.api_client,
            ctx.config.format,
            page_size,
            &mut ctx.output_file,
            &mut warnings,
        )
        .await
    }
}

#[derive(Serialize)]
struct MachineView {
    machine_id: MachineId,
    attestation_status: Option<String>,
    job_id: Option<Uuid>,
}

impl Args {
    async fn execute(
        self,
        client: &impl MachineAttestationClient,
        format: OutputFormat,
        page_size: u32,
        output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
        warnings: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    ) -> CarbideCliResult<()> {
        // Clap permits one status flag; no flag leaves the server search unfiltered.
        let selector = if self.passed {
            Some(SpdmAttestationStatus::SpdmAttPassed as i32)
        } else if self.failed {
            Some(SpdmAttestationStatus::SpdmAttFailed as i32)
        } else if self.cancelled {
            Some(SpdmAttestationStatus::SpdmAttCancelled as i32)
        } else if self.in_progress {
            Some(SpdmAttestationStatus::SpdmAttInProgress as i32)
        } else {
            None
        };
        let mut page_token = String::new();
        let mut views = Vec::new();
        loop {
            let response = client
                .list(ListSpdmMachineAttestationsRequest {
                    attestation_status: selector,
                    page_size,
                    page_token,
                })
                .await?;
            for status in response.statuses {
                let machine_id = status.machine_id.ok_or_else(|| {
                    CarbideCliError::GenericError(
                        "SPDM machine prediction is missing its machine ID".to_string(),
                    )
                })?;
                let attestation_status = status
                    .attestation_status
                    .map(|value| {
                        SpdmAttestationStatus::try_from(value)
                            .map(|value| value.as_str_name().to_string())
                            .map_err(|_| {
                                CarbideCliError::GenericError(
                                    "unknown SPDM attestation status".to_string(),
                                )
                            })
                    })
                    .transpose()?;
                let job_id = status
                    .job_id
                    .map(|id| {
                        Uuid::try_from(id).map_err(|_| {
                            CarbideCliError::GenericError(
                                "invalid SPDM attestation job ID".to_string(),
                            )
                        })
                    })
                    .transpose()?;
                views.push(MachineView {
                    machine_id,
                    attestation_status,
                    job_id,
                });
            }
            // Tokens are opaque: pass them back unchanged with the same selector.
            page_token = response.next_page_token;
            if page_token.is_empty() {
                break;
            }
        }

        // Keep table and serialized output in the same deterministic order.
        views.sort_by_key(|view| view.machine_id);
        let mut table = Table::new();
        table.set_titles(row!["MACHINE ID", "ATTESTATION STATUS", "JOB ID"]);
        for view in &views {
            table.add_row(row![
                view.machine_id,
                view.attestation_status.as_deref().unwrap_or("unknown"),
                view.job_id.map(|id| id.to_string()).unwrap_or_default(),
            ]);
        }
        write_output(
            &views,
            table,
            "machine attestation predictions",
            format,
            output,
        )
        .await?;
        async_writeln!(warnings, "{PREDICTION_WARNING}")?;
        Ok(())
    }
}
