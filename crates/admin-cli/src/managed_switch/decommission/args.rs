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

use carbide_uuid::switch::SwitchId;
use clap::Parser;

/// Start the managed-switch decommissioning workflow.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Start decommissioning a ready managed switch:
    $ nico-admin-cli managed-switch decommission sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60

")]
pub(crate) struct Args {
    #[clap(help = "ID of the ready managed switch to decommission")]
    pub(super) switch_id: SwitchId,
}
