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

use clap::Parser;
use eyre::WrapErr;
use uuid::Uuid;

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

#[derive(Parser, Debug)]
#[command(about = "Request failure for an attestation job, including completed results")]
#[command(
    long_about = "Fail an attestation job, including completed results. Unfinished devices finish BMC evidence collection cleanup before failure takes effect. Terminal results immediately count as Failed. Later waive or fail requests replace the previous target and refresh the request time. Changing the request does not restart ongoing cleanup. Stored attestation results and completion times stay available as history. This request does not undo a machine lifecycle transition that has already completed."
)]
#[command(after_long_help = "\
EXAMPLES:

Fail a stuck SPDM attestation job after BMC cleanup:
    $ nico-admin-cli attestation spdm job fail 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(help = "Attestation job ID")]
    pub(super) job_id: Uuid,
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        // Terminal status reads honour the target immediately; unfinished work still needs cleanup.
        ctx.api_client
            .0
            .fail_attestation(self.job_id)
            .await
            .context("fail the SPDM attestation job")?;
        println!(
            "Failure requested; terminal results count as Failed and unfinished work will resolve after BMC cleanup"
        );
        Ok(())
    }
}
