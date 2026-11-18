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
use std::net::IpAddr;

use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use mac_address::MacAddress;
use serde::Deserialize;
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};
use uuid::Uuid;

use crate::metadata::{Metadata, default_metadata_for_deserializer};

#[derive(Clone, Default, Deserialize)] // Do not add Debug here. It contains password.
pub struct ExpectedPowerShelf {
    #[serde(default)]
    pub expected_power_shelf_id: Option<Uuid>,
    pub bmc_mac_address: MacAddress,
    pub bmc_username: String,
    pub serial_number: String,
    pub bmc_password: String,
    pub bmc_ip_address: Option<IpAddr>,
    #[serde(default = "default_metadata_for_deserializer")]
    pub metadata: Metadata,
    pub rack_id: Option<RackId>,
    /// When true, site-explorer skips BMC password rotation and stores the
    /// factory-default credentials in Vault as-is.
    #[serde(default)]
    pub bmc_retain_credentials: Option<bool>,
}

impl<'r> FromRow<'r, PgRow> for ExpectedPowerShelf {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("metadata_labels")?;
        let metadata = Metadata {
            name: row.try_get("metadata_name")?,
            description: row.try_get("metadata_description")?,
            labels: labels.0,
        };

        Ok(ExpectedPowerShelf {
            expected_power_shelf_id: row.try_get("expected_power_shelf_id")?,
            bmc_mac_address: row.try_get("bmc_mac_address")?,
            bmc_username: row.try_get("bmc_username")?,
            serial_number: row.try_get("serial_number")?,
            bmc_password: row.try_get("bmc_password")?,
            bmc_ip_address: row.try_get("bmc_ip_address")?,
            metadata,
            rack_id: row.try_get("rack_id")?,
            bmc_retain_credentials: row.try_get("bmc_retain_credentials")?,
        })
    }
}

#[derive(FromRow)]
pub struct LinkedExpectedPowerShelf {
    pub serial_number: String,
    pub bmc_mac_address: MacAddress, // from expected_power_shelves table
    pub power_shelf_id: Option<PowerShelfId>, // The power shelf
    pub expected_power_shelf_id: Option<Uuid>, // The expected power shelf ID
    pub address: Option<IpAddr>,     // The explored BMC endpoint IP
    pub rack_id: Option<RackId>,     // The rack this power shelf belongs to
}

/// A request to identify an ExpectedPowerShelf by either ID or MAC address.
#[derive(Debug, Clone)]
pub struct ExpectedPowerShelfRequest {
    pub expected_power_shelf_id: Option<Uuid>,
    pub bmc_mac_address: Option<MacAddress>,
}

#[cfg(test)]
mod test_decode;
