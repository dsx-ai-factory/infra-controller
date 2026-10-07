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

use carbide_uuid::machine::MachineId;
use clap::{ArgGroup, Parser, ValueEnum};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List a machine's health report entries:
    $ nico-admin-cli machine health-report show 12345678-1234-5678-90ab-cdef01234567

Add a health report entry from a template:
    $ nico-admin-cli machine health-report add 12345678-1234-5678-90ab-cdef01234567 \
    --template internal-maintenance --message \"Firmware upgrade in progress\"

Remove a health report entry by its source:
    $ nico-admin-cli machine health-report remove 12345678-1234-5678-90ab-cdef01234567 \
    internal-maintenance

Print an empty health report template:
    $ nico-admin-cli machine health-report print-empty-template

")]
pub(crate) enum Args {
    #[clap(about = "List the health report entries")]
    Show { machine_id: MachineId },
    #[clap(about = "Insert a health report entry")]
    Add(HealthAddOptions),
    #[clap(about = "Print an empty health report template, which user can modify and use")]
    PrintEmptyTemplate,
    #[clap(about = "Remove a health report entry")]
    Remove {
        machine_id: MachineId,
        report_source: String,
    },
}

#[derive(Parser, Debug)]
#[clap(group(ArgGroup::new("health_report_source").required(true).args(&["health_report", "template"])))]
pub(crate) struct HealthAddOptions {
    pub(super) machine_id: MachineId,
    #[clap(long, help = "New health report as json")]
    pub(super) health_report: Option<String>,
    #[clap(
        long,
        help = "Predefined Template name. Use host-update for DPU Reprovision"
    )]
    pub(super) template: Option<HealthReportTemplates>,
    #[clap(long, help = "Message to be filled in template.")]
    pub(super) message: Option<String>,
    #[clap(long, help = "Replace all other health reports with this source")]
    pub(super) replace: bool,
    #[clap(long, help = "Print the template that is going to be send to carbide")]
    pub(super) print_only: bool,
}

#[derive(ValueEnum, Parser, Debug, Clone)]
pub(crate) enum HealthReportTemplates {
    HostUpdate,
    InternalMaintenance,
    OutForRepair,
    Degraded,
    Validation,
    SuppressExternalAlerting,
    MarkHealthy,
    StopRebootForAutomaticRecoveryFromStateMachine,
    TenantReportedIssue,
    RequestOnlineRepair,
    RequestRepair,
}
