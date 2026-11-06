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

use ::rpc::admin_cli::OutputFormat;
use clap::{Parser, Subcommand, ValueEnum};
use eyre::WrapErr;
use prettytable::{Cell, Row, Table};

use super::common::{self, SwitchSelection, SwitchTargetArgs};
use crate::async_writeln;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Clone, Copy, Debug, ValueEnum)]
#[expect(
    clippy::enum_variant_names,
    reason = "Match the public reset action names"
)]
enum ResetActionArg {
    #[value(name = "bmc-reset")]
    BmcReset,
    #[value(name = "bmc-factory-reset")]
    BmcFactoryReset,
    #[value(name = "nvos-factory-reset")]
    NvosFactoryReset,
    #[value(name = "sdn-factory-reset")]
    SdnFactoryReset,
}

impl From<ResetActionArg> for rpc::forge::ComponentResetAction {
    fn from(action: ResetActionArg) -> Self {
        match action {
            ResetActionArg::BmcReset => Self::BmcReset,
            ResetActionArg::BmcFactoryReset => Self::BmcFactoryReset,
            ResetActionArg::NvosFactoryReset => Self::NvosFactoryReset,
            ResetActionArg::SdnFactoryReset => Self::SdnFactoryReset,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    long_about = "Dispatch a component reset directly, bypassing state-controller maintenance. The RMS backend supports sdn-factory-reset and nvos-factory-reset on switches. SDN reset disrupts fabric traffic and restores SDN defaults without reinstalling NVOS. NVOS reset erases operating-system configuration and reboots the switch. For an ingested switch, after NVOS reset reports COMPLETED, an operator must log in over SSH using factory-default credentials and restore the NVOS account and password recorded in NICo for that switch before resuming managed operations. Direct reset does not update the credentials stored in NICo or automatically rerun credential rotation. NVOS reset uses the RMS configured default NVL certificate domain and rejects --domain-name. Switches can be selected by ID or BMC MAC, including expected inventory before ingestion. Success means accepted and tracked, not completed. Per-device failures do not prevent other valid targets from being submitted. A submission error may mean rejection or an uncertain outcome; the reset may have started. Inspect the error and device state before retrying. Do not automatically resubmit. Status queries return the latest successfully recorded reset reference, which may belong to an earlier reset after a failed or ambiguous submission.",
    after_long_help = "\
EXAMPLES:

Reset a switch's SDN configuration:
    $ nico-admin-cli component-manager component-reset --action sdn-factory-reset switch \
    --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60

Reset NVOS configuration and reboot a switch:
    $ nico-admin-cli component-manager component-reset --action nvos-factory-reset switch \
    --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60

Reset by BMC MAC before ingestion, selecting an NVL certificate domain:
    $ nico-admin-cli component-manager component-reset --action sdn-factory-reset \
    --domain-name nvl-domain switch --mac-address 00:11:22:33:44:55

"
)]
pub(crate) struct ResetArgs {
    #[clap(
        long,
        value_enum,
        help = "Reset action; the RMS backend supports sdn-factory-reset and nvos-factory-reset"
    )]
    action: ResetActionArg,
    #[clap(
        long,
        help = "NVL certificate-domain selector for SDN reset only; omit to use the RMS configured domain"
    )]
    domain_name: Option<String>,
    #[clap(subcommand)]
    target: ResetTargetArgs,
}

#[derive(Subcommand, Debug)]
enum ResetTargetArgs {
    #[command(
        about = "Reset switches by ID or BMC MAC",
        after_long_help = "\
EXAMPLES:

Reset SDN configuration by switch ID:
    $ nico-admin-cli component-manager component-reset --action sdn-factory-reset switch \
    --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60

Reset NVOS configuration and reboot a switch:
    $ nico-admin-cli component-manager component-reset --action nvos-factory-reset switch \
    --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60

Reset by BMC MAC with an explicit NVL certificate domain:
    $ nico-admin-cli component-manager component-reset --action sdn-factory-reset \
    --domain-name nvl-domain switch --mac-address 00:11:22:33:44:55

"
    )]
    Switch(SwitchTargetArgs),
}

impl From<ResetArgs> for rpc::forge::ComponentResetRequest {
    fn from(args: ResetArgs) -> Self {
        use rpc::forge::component_reset_request::Target;
        let target = match args.target {
            ResetTargetArgs::Switch(target) => match target.into_selection() {
                SwitchSelection::SwitchIds(ids) => Target::SwitchIds(ids),
                SwitchSelection::Macs(macs) => Target::SwitchBmcMacs(macs),
            },
        };
        Self {
            target: Some(target),
            action: rpc::forge::ComponentResetAction::from(args.action) as i32,
            domain_name: args.domain_name,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    long_about = "Get the tracked reset status for each device and the required reset action. The RMS backend supports SDN and NVOS reset status for switches. NICo looks up its latest successfully recorded RMS job reference for the selected action; callers do not supply a job ID. After a failed or ambiguous submission, this reference may still belong to an earlier reset. A missing reference or unavailable RMS job does not establish whether a reset ran. RMS restart or job expiry can make previously observed completion unavailable. Failed resets and observation errors print results and exit unsuccessfully.",
    after_long_help = "\
EXAMPLES:

Get SDN reset status for a switch:
    $ nico-admin-cli component-manager get-reset-status --action sdn-factory-reset switch \
    --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60

Get NVOS reset status by BMC MAC, including a switch that has not been ingested:
    $ nico-admin-cli -f json component-manager get-reset-status --action nvos-factory-reset switch \
    --mac-address 00:11:22:33:44:55

"
)]
pub(crate) struct StatusArgs {
    #[clap(
        long,
        value_enum,
        help = "Reset action to inspect; the RMS backend supports sdn-factory-reset and nvos-factory-reset"
    )]
    action: ResetActionArg,
    #[clap(subcommand)]
    target: StatusTargetArgs,
}

#[derive(Subcommand, Debug)]
enum StatusTargetArgs {
    #[command(
        about = "Get switch reset status by ID or BMC MAC",
        after_long_help = "\
EXAMPLES:

Get SDN reset status by switch ID:
    $ nico-admin-cli component-manager get-reset-status --action sdn-factory-reset switch \
    --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60

Get NVOS reset status by BMC MAC:
    $ nico-admin-cli component-manager get-reset-status --action nvos-factory-reset switch \
    --mac-address 00:11:22:33:44:55

"
    )]
    Switch(SwitchTargetArgs),
}

impl From<StatusArgs> for rpc::forge::GetComponentResetStatusRequest {
    fn from(args: StatusArgs) -> Self {
        use rpc::forge::get_component_reset_status_request::Target;
        let target = match args.target {
            StatusTargetArgs::Switch(target) => match target.into_selection() {
                SwitchSelection::SwitchIds(ids) => Target::SwitchIds(ids),
                SwitchSelection::Macs(macs) => Target::SwitchBmcMacs(macs),
            },
        };
        Self {
            target: Some(target),
            action: rpc::forge::ComponentResetAction::from(args.action) as i32,
        }
    }
}

fn reset_action_name(action: i32) -> &'static str {
    match rpc::forge::ComponentResetAction::try_from(action) {
        Ok(rpc::forge::ComponentResetAction::BmcReset) => "bmc-reset",
        Ok(rpc::forge::ComponentResetAction::BmcFactoryReset) => "bmc-factory-reset",
        Ok(rpc::forge::ComponentResetAction::NvosFactoryReset) => "nvos-factory-reset",
        Ok(rpc::forge::ComponentResetAction::SdnFactoryReset) => "sdn-factory-reset",
        Ok(rpc::forge::ComponentResetAction::Unspecified) | Err(_) => "unknown",
    }
}

fn reset_state_name(state: i32) -> &'static str {
    match rpc::forge::ComponentResetState::try_from(state) {
        Ok(rpc::forge::ComponentResetState::Queued) => "queued",
        Ok(rpc::forge::ComponentResetState::InProgress) => "in-progress",
        Ok(rpc::forge::ComponentResetState::Completed) => "completed",
        Ok(rpc::forge::ComponentResetState::Failed) => "failed",
        Ok(rpc::forge::ComponentResetState::Unknown) | Err(_) => "unknown",
    }
}

impl Run for ResetArgs {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: rpc::forge::ComponentResetRequest = self.into();
        let response = ctx
            .api_client
            .0
            .component_reset(request)
            .await
            .context("submit component resets")?;
        if ctx.config.format == OutputFormat::Json {
            let results: Vec<_> = response
                .results
                .iter()
                .map(|result| common::component_result_json(Some(result)))
                .collect();
            async_writeln!(
                ctx.output_file,
                "{}",
                serde_json::to_string_pretty(&results)
                    .context("serialize component reset results")?
            )
            .context("write component reset results")?;
        } else {
            let mut table = Table::new();
            table.set_titles(Row::new(vec![
                Cell::new("Component ID / MAC"),
                Cell::new("Status"),
                Cell::new("Error"),
            ]));
            for result in &response.results {
                let (id, status, error) = common::component_result_fields(Some(result));
                table.add_row(Row::new(vec![
                    Cell::new(&id),
                    Cell::new(&status),
                    Cell::new(&error),
                ]));
            }
            async_writeln!(ctx.output_file, "{table}").context("write component reset results")?;
        }
        let (failures, details) =
            common::component_failure_count_and_summary(response.results.iter().map(Some));
        if failures > 0 {
            return Err(CarbideCliError::GenericError(format!(
                "{failures} component reset request(s) failed{details}"
            )));
        }
        Ok(())
    }
}

impl Run for StatusArgs {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: rpc::forge::GetComponentResetStatusRequest = self.into();
        let response = ctx
            .api_client
            .0
            .get_component_reset_status(request)
            .await
            .context("get component reset status")?;
        if ctx.config.format == OutputFormat::Json {
            let statuses: Vec<_> = response
                .statuses
                .iter()
                .map(|status| {
                    serde_json::json!({
                        "result": common::component_result_json(status.result.as_ref()),
                        "action": reset_action_name(status.action),
                        "action_code": status.action,
                        "state": reset_state_name(status.state),
                        "state_code": status.state,
                    })
                })
                .collect();
            async_writeln!(
                ctx.output_file,
                "{}",
                serde_json::to_string_pretty(&statuses)
                    .context("serialize component reset status")?
            )
            .context("write component reset status")?;
        } else {
            let mut table = Table::new();
            table.set_titles(Row::new(vec![
                Cell::new("Component ID / MAC"),
                Cell::new("Action"),
                Cell::new("Result"),
                Cell::new("State"),
                Cell::new("Error"),
            ]));
            for status in &response.statuses {
                let (id, result, error) = common::component_result_fields(status.result.as_ref());
                table.add_row(Row::new(vec![
                    Cell::new(&id),
                    Cell::new(reset_action_name(status.action)),
                    Cell::new(&result),
                    Cell::new(reset_state_name(status.state)),
                    Cell::new(&error),
                ]));
            }
            async_writeln!(ctx.output_file, "{table}").context("write component reset status")?;
        }
        let (failures, details) = common::component_failure_count_and_summary(
            response
                .statuses
                .iter()
                .map(|status| status.result.as_ref()),
        );
        if failures > 0 {
            return Err(CarbideCliError::GenericError(format!(
                "{failures} component reset status result(s) failed{details}"
            )));
        }
        if response.statuses.iter().any(|status| {
            !matches!(
                rpc::forge::ComponentResetState::try_from(status.state),
                Ok(rpc::forge::ComponentResetState::Queued
                    | rpc::forge::ComponentResetState::InProgress
                    | rpc::forge::ComponentResetState::Completed)
            )
        }) {
            return Err(CarbideCliError::GenericError(
                "a component reset failed or its outcome is unknown".into(),
            ));
        }
        Ok(())
    }
}
