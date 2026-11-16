/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use carbide_uuid::dns_zone::DnsZoneId;
use carbide_uuid::vpc::VpcId;
use chrono::{DateTime, Utc};
use dns_record::{Seconds, SoaRecord};
use model::dns::{DnsZone, Fqdn, ZoneTtl};
use sqlx::FromRow;

use crate::DatabaseError;

/// A row of the `dns_zones` table.
#[derive(Clone, Debug, FromRow, carbide_macros::DbTable)]
#[db_table(name = "dns_zones")]
pub(crate) struct DbDnsZone {
    pub id: DnsZoneId,
    pub name: String,
    pub vpc_id: Option<VpcId>,
    pub primary_ns: String,
    pub contact: String,
    pub serial: i64,
    pub minimum: i32,
    pub soa_ttl: i32,
    pub default_ttl: Option<ZoneTtl>,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

impl TryFrom<DbDnsZone> for DnsZone {
    type Error = DatabaseError;

    fn try_from(db: DbDnsZone) -> Result<Self, Self::Error> {
        let name = Fqdn::parse(&db.name).map_err(|error| {
            DatabaseError::internal(format!("DNS zone {} has an invalid name: {error}", db.id))
        })?;
        let serial = u32::try_from(db.serial).map_err(|error| {
            DatabaseError::internal(format!("DNS zone {} has an invalid serial: {error}", db.id))
        })?;
        Ok(DnsZone {
            id: db.id,
            vpc_id: db.vpc_id,
            soa: SoaRecord {
                primary_ns: db.primary_ns,
                contact: db.contact,
                serial,
                minimum: Seconds(db.minimum),
                ttl: Seconds(db.soa_ttl),
                // The table stores no refresh, retry or expire.
                ..SoaRecord::new(name.as_str().trim_end_matches('.'))
            },
            name,
            default_ttl: db.default_ttl,
            created: db.created,
            updated: db.updated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_zone_rejects_row_with_invalid_name() {
        let row = DbDnsZone {
            id: DnsZoneId::new(),
            name: "not a name".to_string(),
            vpc_id: None,
            primary_ns: "ns1.corp.example".to_string(),
            contact: "hostmaster.corp.example".to_string(),
            serial: 1,
            minimum: 30,
            soa_ttl: 3600,
            default_ttl: None,
            created: Utc::now(),
            updated: Utc::now(),
        };

        let error = DnsZone::try_from(row).expect_err("an invalid name must not decode");

        assert!(matches!(error, DatabaseError::Internal { .. }), "{error}");
    }
}
