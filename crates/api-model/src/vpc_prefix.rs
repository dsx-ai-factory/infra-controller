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
use std::collections::HashMap;
use std::time::Duration;

use carbide_uuid::site_prefix::SitePrefixId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use ipnetwork::IpNetwork;
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::metadata::Metadata;
use crate::{DeletedFilter, StateSla};

const PROVISIONING_SLA: Duration = Duration::from_secs(15 * 60);
const DELETING_DBDELETE_SLA: Duration = Duration::from_secs(15 * 60);

/// State of a VPC prefix as tracked by the controller.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum VpcPrefixControllerState {
    Provisioning,
    Ready,
    Deleting {
        deletion_state: VpcPrefixDeletionState,
    },
}

/// Possible substates while deleting a VPC prefix.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum VpcPrefixDeletionState {
    DrainNetworkPrefixes { delete_at: DateTime<Utc> },
    DBDelete,
}

/// Returns the SLA for the current VPC prefix controller state.
pub fn state_sla(state: &VpcPrefixControllerState, state_version: &ConfigVersion) -> StateSla {
    // Compare the controller state's version timestamp against the current time.
    let time_in_state = chrono::Utc::now()
        .signed_duration_since(state_version.timestamp())
        .to_std()
        .unwrap_or(Duration::from_secs(60 * 60 * 24));

    // Only bounded controller work has an SLA; dependency drains can wait indefinitely.
    match state {
        VpcPrefixControllerState::Provisioning => {
            StateSla::with_sla(PROVISIONING_SLA, time_in_state)
        }
        VpcPrefixControllerState::Ready => StateSla::no_sla(),
        VpcPrefixControllerState::Deleting {
            deletion_state: VpcPrefixDeletionState::DrainNetworkPrefixes { .. },
        } => StateSla::no_sla(),
        VpcPrefixControllerState::Deleting {
            deletion_state: VpcPrefixDeletionState::DBDelete,
        } => StateSla::with_sla(DELETING_DBDELETE_SLA, time_in_state),
    }
}

#[derive(Clone, Debug)]
pub struct VpcPrefix {
    pub id: VpcPrefixId,
    pub site_prefix_id: Option<SitePrefixId>,
    pub vpc_id: VpcId,
    pub config: VpcPrefixConfig,
    pub metadata: Metadata,
    pub status: VpcPrefixStatus,
    pub deleted: Option<DateTime<Utc>>,
}

impl VpcPrefix {
    /// Returns whether the VPC prefix was marked for asynchronous deletion.
    pub fn is_marked_as_deleted(&self) -> bool {
        // A non-null deletion timestamp is the durable soft-delete marker.
        self.deleted.is_some()
    }
}

#[derive(Clone, Debug)]
pub struct VpcPrefixConfig {
    pub prefix: IpNetwork,
}

#[derive(Clone, Debug)]
pub struct VpcPrefixStatus {
    pub controller_state: Versioned<VpcPrefixControllerState>,
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,
    pub last_used_prefix: Option<IpNetwork>,
    pub total_31_segments: u32,
    pub available_31_segments: u32,
    pub total_linknet_segments: u64,
    pub available_linknet_segments: u64,
}

impl<'r> sqlx::FromRow<'r, PgRow> for VpcPrefix {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let id = row.try_get("id")?;
        let site_prefix_id = row.try_get("site_prefix_id")?;
        let prefix = row.try_get("prefix")?;
        let name = row.try_get("name")?;
        let vpc_id = row.try_get("vpc_id")?;
        let last_used_prefix = row.try_get("last_used_prefix")?;
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;
        let description: String = row.try_get("description")?;
        let controller_state: sqlx::types::Json<VpcPrefixControllerState> =
            row.try_get("controller_state")?;
        let controller_state_outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
            row.try_get("controller_state_outcome")?;

        Ok(VpcPrefix {
            id,
            site_prefix_id,
            config: VpcPrefixConfig { prefix },
            metadata: Metadata {
                name,
                description,
                labels: labels.0,
            },
            vpc_id,
            status: VpcPrefixStatus {
                controller_state: Versioned::new(
                    controller_state.0,
                    row.try_get("controller_state_version")?,
                ),
                controller_state_outcome: controller_state_outcome.map(|outcome| outcome.0),
                last_used_prefix,
                total_31_segments: 0,
                available_31_segments: 0,
                total_linknet_segments: 0,
                available_linknet_segments: 0,
            },
            deleted: row.try_get("deleted")?,
        })
    }
}

#[derive(Clone, Debug)]
pub enum PrefixMatch {
    Exact(IpNetwork),
    Contains(IpNetwork),
    ContainedBy(IpNetwork),
}

#[derive(Clone, Debug, Default)]
pub struct VpcPrefixSearch {
    pub vpc_id: Option<VpcId>,
    pub site_prefix_id: Option<SitePrefixId>,
    pub name: Option<String>,
    pub prefix_match: Option<PrefixMatch>,
    pub deleted_filter: DeletedFilter,
}

/// NewVpcPrefix represents a VPC prefix resource before it's persisted to the
/// database.
pub struct NewVpcPrefix {
    pub id: VpcPrefixId,
    pub site_prefix_id: Option<SitePrefixId>,
    pub vpc_id: VpcId,
    /// VPC scope selected by Core after checking tenant-prefix eligibility.
    /// `None` keeps the prefix globally exclusive; API requests cannot set this field.
    pub overlap_vpc_id: Option<VpcId>,
    pub config: VpcPrefixConfig,
    pub metadata: Metadata,
}

pub struct UpdateVpcPrefix {
    pub id: VpcPrefixId,
    // This is all we support updating at the moment. In the future we might
    // also implement prefix resizing, and at that point we'll need to use
    // Option for all the fields.
    pub metadata: Metadata,
}

pub struct DeleteVpcPrefix {
    pub id: VpcPrefixId,
}
