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

#[derive(Parser, Debug)]
#[clap(group(
        ArgGroup::new("release_instance")
        .required(true)
        .args(&["instance", "machine", "label_key"])))]
#[command(after_long_help = "\
EXAMPLES:

Release an instance by id:
    $ nico-admin-cli instance release --instance 12345678-1234-5678-90ab-cdef01234567

Release the instance on a machine:
    $ nico-admin-cli instance release --machine fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0

Release every instance matching a label:
    $ nico-admin-cli instance release --label-key role --label-value training

")]
pub(crate) struct Args {
    #[clap(short, long)]
    pub(super) instance: Option<String>,

    #[clap(short, long)]
    pub(super) machine: Option<MachineId>,

    #[clap(long, help = "The key of label instance to query")]
    pub(super) label_key: Option<String>,

    #[clap(long, help = "The value of label instance to query")]
    pub(super) label_value: Option<String>,
}
