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
use clap::{ArgGroup, Parser};

use crate::machine::HealthReportTemplates;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List the health report sources for a DPU:
    $ nico-admin-cli dpu health-report show 12345678-1234-5678-90ab-cdef01234567

Add a health report source from a predefined template:
    $ nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 \
    --template internal-maintenance

Remove a health report source from a DPU:
    $ nico-admin-cli dpu health-report remove 12345678-1234-5678-90ab-cdef01234567 \
    internal-maintenance

")]
pub(crate) enum Args {
    #[clap(about = "List health report sources for a DPU")]
    #[command(after_long_help = "\
EXAMPLES:

List the health report sources for a DPU:
    $ nico-admin-cli dpu health-report show 12345678-1234-5678-90ab-cdef01234567

")]
    Show { dpu_id: MachineId },
    #[clap(about = "Insert a health report source for a DPU")]
    Add(HealthAddOptions),
    #[clap(about = "Print an empty health report template")]
    PrintEmptyTemplate,
    #[clap(about = "Remove a health report source from a DPU")]
    #[command(after_long_help = "\
EXAMPLES:

Remove a health report source from a DPU:
    $ nico-admin-cli dpu health-report remove 12345678-1234-5678-90ab-cdef01234567 \
    internal-maintenance

")]
    Remove {
        dpu_id: MachineId,
        report_source: String,
    },
}

#[derive(Parser, Debug)]
#[clap(group(ArgGroup::new("health_report_source").required(true).args(&["health_report", "template"])))]
#[command(after_long_help = "\
EXAMPLES:

Add a health report source from a predefined template:
    $ nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 \
    --template internal-maintenance

Add a template-based source with a custom message:
    $ nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 \
    --template out-for-repair --message \"awaiting replacement part\"

Add a raw JSON health report source:
    $ nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 \
    --health-report '{\"status\":\"Degraded\"}'

Replace the DPU's health contribution with this source:
    $ nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 \
    --template internal-maintenance --replace

Preview the report without sending it to carbide:
    $ nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 \
    --template internal-maintenance --print-only

")]
pub(crate) struct HealthAddOptions {
    pub(super) dpu_id: MachineId,
    #[clap(long, help = "New health report as json")]
    pub(super) health_report: Option<String>,
    #[clap(long, help = "Predefined Template name")]
    pub(super) template: Option<HealthReportTemplates>,
    #[clap(long, help = "Message to be filled in template.")]
    pub(super) message: Option<String>,
    #[clap(long, help = "Replace the DPU health contribution with this source")]
    pub(super) replace: bool,
    #[clap(long, help = "Print the template that is going to be sent to carbide")]
    pub(super) print_only: bool,
}
