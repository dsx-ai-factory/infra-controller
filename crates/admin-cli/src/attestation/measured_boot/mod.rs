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

//!
//! Measured Boot attestation (`attestation/measured_boot`): subcommand dispatchers,
//! args, and backing functions for `... attestation measured-boot` (alias `mb`).

pub(crate) mod bundle;
mod global;
pub(crate) mod journal;
pub(crate) mod machine;
pub(crate) mod profile;
pub(crate) mod report;
pub(crate) mod site;

use carbide_uuid::machine::MachineId;
use measured_boot::{ToTable, set_summary};
use serde::Serialize;

use crate::cfg::dispatch::Dispatch;
use crate::cfg::measurement::Cmd;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

// `Cmd` keeps a hand-written `Dispatch` (the one exception to the derive):
// it must set the measured-boot summary flag exactly once -- derived from
// `ctx.config.extended` -- before delegating to any subgroup, and the
// `Dispatch` derive has no way to express that pre-delegation side effect.
impl Dispatch for Cmd {
    async fn dispatch(self, ctx: RuntimeContext) -> CarbideCliResult<()> {
        set_summary(!ctx.config.extended);

        match self {
            Cmd::Bundle(c) => c.dispatch(ctx).await,
            Cmd::Journal(c) => c.dispatch(ctx).await,
            Cmd::Report(c) => c.dispatch(ctx).await,
            Cmd::Machine(c) => c.dispatch(ctx).await,
            Cmd::Profile(c) => c.dispatch(ctx).await,
            Cmd::Site(c) => c.dispatch(ctx).await,
        }
    }
}

#[derive(Serialize)]
struct MachineIdList(Vec<MachineId>);

impl ToTable for MachineIdList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["machine_id"]);
        for machine_id in self.0.iter() {
            table.add_row(prettytable::row![machine_id]);
        }
        Ok(table.to_string())
    }
}
