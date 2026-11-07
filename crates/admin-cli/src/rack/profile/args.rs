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

use super::show;

#[derive(Parser, Debug, Clone)]
pub(crate) enum Args {
    #[clap(
        about = "List configured rack profiles",
        long_about = "List rack profiles from the effective runtime configuration. Rack profiles are not persisted rack resources. To add or change a profile, update the runtime configuration."
    )]
    List(super::list::Args),
    #[clap(about = "Show rack profile for a given rack")]
    Show(show::Args),
}
