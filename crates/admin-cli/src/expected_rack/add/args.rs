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
use serde::{Deserialize, Serialize};

use crate::metadata::parse_rpc_labels;

#[derive(Parser, Debug, Serialize, Deserialize)]
#[command(after_long_help = "\
EXAMPLES:

Add a rack already declared in an expected rack group:
    $ nico-admin-cli expected-rack add rack-01

Add an expected rack with a metadata name and a label:
    $ nico-admin-cli expected-rack add rack-01 --meta-name rack-01 --label DATACENTER:XYZ

")]
pub(crate) struct Args {
    #[clap(help = "Rack ID of the expected rack")]
    rack_id: RackId,

    #[clap(
        long = "meta-name",
        value_name = "META_NAME",
        help = "The name that should be used as part of the Metadata for newly created Rack. If empty, the Rack Id will be used"
    )]
    meta_name: Option<String>,

    #[clap(
        long = "meta-description",
        value_name = "META_DESCRIPTION",
        help = "The description that should be used as part of the Metadata for newly created Rack"
    )]
    meta_description: Option<String>,

    #[clap(
        long = "label",
        value_name = "LABEL",
        help = "A label that will be added as metadata for the newly created Rack. The labels key and value must be separated by a : character. E.g. DATACENTER:XYZ",
        action = clap::ArgAction::Append
    )]
    labels: Option<Vec<String>>,
}

impl From<Args> for rpc::forge::ExpectedRack {
    fn from(value: Args) -> Self {
        let labels = parse_rpc_labels(value.labels.unwrap_or_default());
        let metadata = rpc::forge::Metadata {
            name: value.meta_name.unwrap_or_default(),
            description: value.meta_description.unwrap_or_default(),
            labels,
        };
        rpc::forge::ExpectedRack {
            rack_id: Some(value.rack_id),
            rack_profile_id: None,
            metadata: Some(metadata),
        }
    }
}
