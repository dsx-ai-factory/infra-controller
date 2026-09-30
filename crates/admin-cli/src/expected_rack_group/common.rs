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

use carbide_uuid::rack::RackGroupId;
use clap::Args;
use model::expected_rack_group::ExpectedRackGroupRack;
use rpc::forge;
use serde::{Deserialize, Serialize};

#[derive(Args, Debug)]
pub(super) struct Attributes {
    /// Rack JSON; repeat for each rack. Example:
    /// {"rack_id":"rack-01","members":[{"type":"Switch","manufacturer":"NVIDIA","id":"switch-01"}]}
    /// Both fields are required; use "members": [] for a rack without devices.
    /// Member type must be Compute, Switch, or PowerShelf. Omitting --rack supplies no racks.
    #[arg(long = "rack", value_parser = parse_rack)]
    racks: Vec<ExpectedRackGroupRack>,
    /// Metadata name (ASCII, at most 256 characters). Defaults to empty.
    #[arg(long)]
    meta_name: Option<String>,
    /// Metadata description (at most 1024 bytes). Defaults to empty.
    #[arg(long)]
    meta_description: Option<String>,
    /// Metadata label as KEY:VALUE; repeat for each label. Omission supplies no labels.
    #[arg(long = "label")]
    labels: Vec<String>,
}

fn parse_rack(value: &str) -> Result<ExpectedRackGroupRack, String> {
    serde_json::from_str(value).map_err(|e| format!("expected rack JSON {{rack_id, members}}: {e}"))
}

impl Attributes {
    pub(super) fn into_rpc(
        self,
        rack_group_id: RackGroupId,
        topology: String,
    ) -> forge::ExpectedRackGroup {
        ExpectedRackGroupJson {
            rack_group_id,
            topology,
            racks: self.racks,
            metadata: Some(forge::Metadata {
                name: self.meta_name.unwrap_or_default(),
                description: self.meta_description.unwrap_or_default(),
                labels: crate::metadata::parse_rpc_labels(self.labels),
            }),
        }
        .into()
    }
}

/// JSON import shape, compatible with `--format json expected-rack-group show`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExpectedRackGroupJson {
    rack_group_id: RackGroupId,
    topology: String,
    #[serde(default)]
    racks: Vec<ExpectedRackGroupRack>,
    #[serde(default)]
    metadata: Option<forge::Metadata>,
}

impl From<ExpectedRackGroupJson> for forge::ExpectedRackGroup {
    fn from(value: ExpectedRackGroupJson) -> Self {
        Self {
            rack_group_id: Some(value.rack_group_id),
            topology: value.topology,
            racks: value
                .racks
                .into_iter()
                .map(|rack| forge::ExpectedRackGroupRack {
                    rack_id: Some(rack.rack_id),
                    members: rack
                        .members
                        .into_iter()
                        .map(|member| forge::ExpectedRackGroupMember {
                            r#type: member.device_type.to_string(),
                            manufacturer: member.manufacturer,
                            id: member.id,
                        })
                        .collect(),
                })
                .collect(),
            metadata: value.metadata,
        }
    }
}
