// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_uuid::machine::MachineId;
use clap::Parser;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::list_machine_attestation_statuses_request::Target;
use rpc::forge::{
    ListMachineAttestationStatusesRequest, ListMachineAttestationStatusesResponse,
    SpdmAttestationStatus, SpdmAttesterSelectionStatus, SpdmMachineAttestationFleetRequest,
    SpdmMachineAttestationMachineRequest,
};
use serde::Serialize;
use tonic::Status;

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
#[command(long_about = "List SPDM attestation statuses in machine-ID order.\n\n\
    Without --machine-id, uses every device in the latest retained controller-triggered job for each machine, chosen by \
    descending start time and then descending job ID. External and older jobs are ignored; BMCs are not contacted. \
    Unfinished devices take precedence over failures, cancellations, then all-passed. \
    Without a status flag, lists all machines; machines without a controller job show unknown \
    (JSON/YAML: null). Such machines are excluded by every status flag. \
    Only one status flag may be supplied. \
    A partially collected job is in progress even if a device has failed. \
    The CLI fetches all pages automatically. Predictions can change between pages. Prints the prediction warning once to stderr, \
    including when no machines match. \
    With --machine-id, selects current attesters using the resolved profile and BMC inventory, then aggregates \
    each selected device's latest result across all job origins. Missing results mean in progress. This read-only \
    assessment creates or changes no jobs and prints no prediction warning. The table shows attester selection status; JSON/YAML include the \
    attester_selection_status field. Empty or disabled selections have no verdict (table: not assessed; \
    JSON/YAML: null), with the reason in attester selection status. Partial selection reports the \
    selected subset's status and warns on stderr. Machine lookup, BMC and timeout failures return errors. \
    --machine-id conflicts with all status flags. Supports table, JSON, and YAML output; CSV is unsupported.")]
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

Assess one machine using its current attesters, with a 60-second timeout:
    $ nico-admin-cli attestation spdm machines list \
    --machine-id fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg --redfish-timeout-secs 60

")]
pub(crate) struct Args {
    #[arg(
        long,
        conflicts_with = "status",
        help = "Definitively assess this machine using its current attesters"
    )]
    machine_id: Option<MachineId>,
    #[arg(long, requires = "machine_id", value_parser = clap::value_parser!(u32).range(1..),
        help = "Timeout in seconds for the entire single-machine assessment; defaults to 30; requires --machine-id")]
    redfish_timeout_secs: Option<u32>,
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
        request: ListMachineAttestationStatusesRequest,
    ) -> Result<ListMachineAttestationStatusesResponse, Status>;
}

impl MachineAttestationClient for ApiClient {
    async fn list(
        &self,
        request: ListMachineAttestationStatusesRequest,
    ) -> Result<ListMachineAttestationStatusesResponse, Status> {
        self.0.list_machine_attestation_statuses(request).await
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
    #[serde(skip_serializing_if = "Option::is_none")]
    attester_selection_status: Option<String>,
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
        let single_machine = self.machine_id.is_some();
        let mut page_token = String::new();
        let mut views = Vec::new();
        loop {
            // Both paths use the same RPC; the oneof prevents mixed request modes.
            let target = if let Some(machine_id) = self.machine_id {
                Target::Machine(SpdmMachineAttestationMachineRequest {
                    machine_id: Some(machine_id),
                    redfish_timeout_secs: self.redfish_timeout_secs,
                })
            } else {
                Target::Fleet(SpdmMachineAttestationFleetRequest {
                    attestation_status: selector,
                    page_size,
                    page_token,
                })
            };
            let response = client
                .list(ListMachineAttestationStatusesRequest {
                    target: Some(target),
                })
                .await?;
            if single_machine
                && (response.statuses.len() != 1 || !response.next_page_token.is_empty())
            {
                return Err(CarbideCliError::GenericError(
                    "expected exactly one definitive machine assessment without pagination"
                        .to_string(),
                ));
            }
            for status in response.statuses {
                let machine_id = status.machine_id.ok_or_else(|| {
                    CarbideCliError::GenericError(
                        "SPDM machine status is missing its machine ID".to_string(),
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
                let attester_selection_status = status
                    .attester_selection_status
                    .map(|value| {
                        SpdmAttesterSelectionStatus::try_from(value)
                            .map(|value| value.as_str_name().to_string())
                            .map_err(|_| {
                                CarbideCliError::GenericError(
                                    "unknown SPDM attester selection status".to_string(),
                                )
                            })
                    })
                    .transpose()?;
                if single_machine
                    && (Some(machine_id) != self.machine_id || attester_selection_status.is_none())
                {
                    return Err(CarbideCliError::GenericError(
                        "invalid definitive machine assessment".to_string(),
                    ));
                }
                views.push(MachineView {
                    machine_id,
                    attestation_status,
                    attester_selection_status,
                });
            }
            // Tokens are opaque: pass them back unchanged with the same selector.
            page_token = response.next_page_token;
            if single_machine || page_token.is_empty() {
                break;
            }
        }

        // Keep table and serialized output in the same deterministic order.
        views.sort_by_key(|view| view.machine_id);
        let mut table = Table::new();
        if single_machine {
            table.set_titles(row![
                "MACHINE ID",
                "ATTESTATION STATUS",
                "ATTESTER SELECTION STATUS"
            ]);
        } else {
            table.set_titles(row!["MACHINE ID", "ATTESTATION STATUS"]);
        }
        for view in &views {
            if let Some(attester_selection_status) = &view.attester_selection_status {
                table.add_row(row![
                    view.machine_id,
                    view.attestation_status.as_deref().unwrap_or("not assessed"),
                    attester_selection_status
                ]);
            } else {
                table.add_row(row![
                    view.machine_id,
                    view.attestation_status.as_deref().unwrap_or("unknown")
                ]);
            }
        }
        write_output(
            &views,
            table,
            "machine attestation statuses",
            format,
            output,
        )
        .await?;
        if !single_machine {
            async_writeln!(warnings, "{PREDICTION_WARNING}")?;
        } else if views.iter().any(|view| {
            view.attester_selection_status.as_deref()
                == Some(SpdmAttesterSelectionStatus::PartiallySatisfied.as_str_name())
        }) {
            async_writeln!(
                warnings,
                "Warning: attestation status covers only the selected devices; the profile is partially satisfied"
            )?;
        }
        Ok(())
    }
}
