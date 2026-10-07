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

mod spdm;

pub(crate) mod measured_boot;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;
use crate::cfg::measurement;

#[derive(Dispatch, Parser, Debug)]
pub(crate) enum Cmd {
    #[dispatch]
    #[clap(about = "Perform SPDM attestation", subcommand)]
    Spdm(spdm::Cmd),
    #[dispatch]
    #[clap(
        subcommand,
        about = "Work with measured boot data (bundles, journals, reports, profiles, site).",
        visible_alias = "mb"
    )]
    MeasuredBoot(measurement::Cmd),
}
