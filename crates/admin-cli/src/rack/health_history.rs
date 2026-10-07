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

use carbide_uuid::rack::RackId;
use clap::Parser;

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::health_utils::display_health_history;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show health history for a rack:
    $ nico-admin-cli rack health-history ipp6-b03-gb-nvl-124-mini2

")]
pub(crate) struct Args {
    #[clap(help = "Rack ID to show health history for")]
    rack_id: RackId,
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let history = ctx
            .api_client
            .get_rack_health_history(self.rack_id.clone())
            .await?;
        display_health_history(self.rack_id.as_ref(), history, ctx.config.format)
    }
}
