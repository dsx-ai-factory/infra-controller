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

use carbide_uuid::rack::{RackGroupId, RackId};
use clap::Args;
use model::expected_rack_group::ExpectedRackGroupMember;
use rpc::forge;
use serde::{Deserialize, Serialize};

#[derive(Args, Debug)]
pub(super) struct Attributes {
    /// Rack ID; repeat for each rack. Omission supplies an empty list.
    #[arg(long = "rack-id")]
    rack_ids: Vec<RackId>,
    /// Device JSON with type, manufacturer and id; repeat for each device. Omission supplies an empty list.
    /// Type must be Compute, Switch, or PowerShelf.
    #[arg(long = "member", value_parser = parse_member)]
    members: Vec<ExpectedRackGroupMember>,
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

fn parse_member(value: &str) -> Result<ExpectedRackGroupMember, String> {
    serde_json::from_str(value)
        .map_err(|e| format!("expected device JSON {{type, manufacturer, id}}: {e}"))
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
            rack_ids: self.rack_ids,
            members: self.members,
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
    rack_ids: Vec<RackId>,
    #[serde(default)]
    members: Vec<ExpectedRackGroupMember>,
    #[serde(default)]
    metadata: Option<forge::Metadata>,
}

impl From<ExpectedRackGroupJson> for forge::ExpectedRackGroup {
    fn from(value: ExpectedRackGroupJson) -> Self {
        Self {
            rack_group_id: Some(value.rack_group_id),
            topology: value.topology,
            rack_ids: value.rack_ids,
            members: value
                .members
                .into_iter()
                .map(|m| forge::ExpectedRackGroupMember {
                    r#type: m.device_type.to_string(),
                    manufacturer: m.manufacturer,
                    id: m.id,
                })
                .collect(),
            metadata: value.metadata,
        }
    }
}
