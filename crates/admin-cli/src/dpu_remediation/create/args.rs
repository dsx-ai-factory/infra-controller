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

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create a remediation from a script:
    $ nico-admin-cli dpu-remediation create --script-filename ./remediate.sh

Create a remediation that retries up to three times:
    $ nico-admin-cli dpu-remediation create --script-filename ./remediate.sh --retries 3

Create a remediation with descriptive metadata and a label:
    $ nico-admin-cli dpu-remediation create --script-filename ./remediate.sh \
    --meta-name \"clear-eeprom\" --meta-description \"Clears stale EEPROM state\" --label DATACENTER:XYZ

")]
pub(crate) struct Args {
    #[clap(help = "The filename of the script to run", long)]
    pub(super) script_filename: String,
    #[clap(
        help = "specify the amount of retries for the remediation, defaults to no retries",
        long
    )]
    pub(super) retries: Option<u32>,
    #[clap(
        long = "meta-name",
        value_name = "META_NAME",
        help = "The name that should be used as part of the Metadata for newly created Remediations.  Completely optional."
    )]
    meta_name: Option<String>,

    #[clap(
        long = "meta-description",
        value_name = "META_DESCRIPTION",
        help = "The description that should be used as part of the Metadata for newly created Remediations.  Completely optional."
    )]
    meta_description: Option<String>,

    #[clap(
        long = "label",
        value_name = "LABEL",
        help = "A label that will be added as metadata for the newly created Remediation. The labels key and value must be separated by a : character. E.g. DATACENTER:XYZ.  Completely optional.",
        action = clap::ArgAction::Append
    )]
    labels: Option<Vec<String>>,
}

impl Args {
    pub(super) fn into_metadata(self) -> Option<::rpc::forge::Metadata> {
        if self.labels.is_none() && self.meta_name.is_none() && self.meta_description.is_none() {
            return None;
        }

        let mut labels = Vec::new();
        if let Some(list) = &self.labels {
            for label in list {
                let label = match label.split_once(':') {
                    Some((k, v)) => rpc::forge::Label {
                        key: k.trim().to_string(),
                        value: Some(v.trim().to_string()),
                    },
                    None => rpc::forge::Label {
                        key: label.trim().to_string(),
                        value: None,
                    },
                };
                labels.push(label);
            }
        }

        Some(::rpc::forge::Metadata {
            name: self.meta_name.unwrap_or_default(),
            description: self.meta_description.unwrap_or_default(),
            labels,
        })
    }
}
