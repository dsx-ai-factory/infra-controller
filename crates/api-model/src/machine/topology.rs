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
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::bmc_info::BmcInfo;
use crate::hardware_info::HardwareInfo;

#[derive(Debug, Deserialize, Serialize, Clone, carbide_macros::DbTable)]
#[db_table(name = "machine_topologies")]
pub struct MachineTopology {
    pub machine_id: MachineId,
    /// Topology data that is stored in json format in the database column
    pub topology: TopologyData,
    pub created: DateTime<Utc>,
    /// The updated field is used when bom_validation is enabled
    /// It stores the last time an inventory update was accepted from scout.
    pub updated: DateTime<Utc>,
    pub topology_update_needed: bool,
}

impl<'r> FromRow<'r, PgRow> for MachineTopology {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        // The wrapper is required to teach sqlx to access the field
        // as a JSON field instead of a string.
        let topology: sqlx::types::Json<TopologyData> = row.try_get("topology")?;

        Ok(MachineTopology {
            machine_id: row.try_get("machine_id")?,
            topology: topology.0,
            created: row.try_get("created")?,
            updated: row.try_get("updated")?,
            topology_update_needed: row.try_get("topology_update_needed")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryData {
    /// Stores the hardware information that was fetched during discovery
    /// **Note that this field is renamed to uppercase because
    /// that is how the originally utilized protobuf message looked in serialized
    /// format**
    #[serde(rename = "Info")]
    pub info: HardwareInfo,
}

/// Describes the data format we store in the `topology` field of the `machine_topologies` table
///
/// Note that we don't need most of the fields here - they are just an artifact
/// of initially storing a protobuf message which also contained other data in this
/// field. For backward compatibility we emulate this behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyData {
    /// Stores the hardware information that was fetched during discovery
    pub discovery_data: DiscoveryData,
    /// The BMC information of the machine
    /// Note that this field is currently side-injected via the
    /// `crate::crate::db::ipmi::BmcMetaDataUpdateRequest::update_bmc_meta_data`
    /// Therefore no `write` function can be found here.
    pub bmc_info: BmcInfo,
}

impl MachineTopology {
    pub fn topology(&self) -> &TopologyData {
        &self.topology
    }

    pub fn into_topology(self) -> TopologyData {
        self.topology
    }

    pub fn created(&self) -> DateTime<Utc> {
        self.created
    }

    pub fn topology_update_needed(&self) -> bool {
        self.topology_update_needed
    }
}
