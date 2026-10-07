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
use clap::builder::BoolishValueParser;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Enable OTLP trace/span export:
    $ nico-admin-cli set tracing-enabled true

Disable OTLP trace/span export:
    $ nico-admin-cli set tracing-enabled false

")]
pub(crate) struct Args {
    #[arg(num_args = 1, value_parser = BoolishValueParser::new(), action = clap::ArgAction::Set, value_name = "true|false")]
    pub(super) value: bool,
}
