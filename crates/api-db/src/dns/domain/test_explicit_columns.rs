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

use model::dns::{DomainMetadata, Fqdn, ZoneTtl};
use sqlx::{Connection, PgPool};

use super::*;
use crate::dns::domain_metadata;

#[crate::sqlx_test]
async fn domain_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_domain_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API connection and its prepared statements while another
    // connection applies the schema change, just as a migration would.
    let mut migration_connection = pool.acquire().await?;
    let mut migration = migration_connection.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE domains ADD COLUMN test_added_column text;
         ALTER TABLE domain_metadata ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_domain_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_domain_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let first_input = NewDomain {
        default_ttl: Some(ZoneTtl::try_from(600)?),
        ..NewDomain::new("first.example.com")
    };
    let first = persist_first(&first_input, &mut txn)
        .await?
        .expect("the first domain is inserted");
    assert_eq!(first.name, first_input.name);
    assert_eq!(first.default_ttl, first_input.default_ttl);
    assert_eq!(
        serde_json::to_value(&first.soa)?,
        serde_json::to_value(&first_input.soa)?
    );

    let input = NewDomain {
        default_ttl: Some(ZoneTtl::try_from(900)?),
        ..NewDomain::new("0.10.in-addr.arpa")
    };
    let mut domain = persist(input.clone(), &mut txn).await?;
    assert_eq!(domain.name, input.name);
    assert_eq!(domain.default_ttl, input.default_ttl);
    assert_eq!(
        serde_json::to_value(&domain.soa)?,
        serde_json::to_value(&input.soa)?
    );
    assert_eq!(domain.deleted, None);

    let allowed_transfers = vec!["192.0.2.10".to_string(), "198.51.100.0/24".to_string()];
    sqlx::query(
        "UPDATE domain_metadata SET allow_axfr_from = $1
         WHERE id = (SELECT domain_metadata_id FROM domains WHERE id = $2)",
    )
    .bind(&allowed_transfers)
    .bind(domain.id)
    .execute(&mut *txn)
    .await?;

    domain.default_ttl = Some(ZoneTtl::try_from(1200)?);
    domain.increment_serial();
    let updated = update(&domain, &mut txn).await?;
    assert!(updated.updated > domain.updated);
    domain.updated = updated.updated;
    assert_eq!(
        serde_json::to_value(&updated)?,
        serde_json::to_value(&domain)?
    );

    for (operation, domains) in [
        (
            "find_by",
            find_by(&mut *txn, ObjectColumnFilter::One(IdColumn, &domain.id)).await?,
        ),
        (
            "find_longest_live_zone",
            find_longest_live_zone(&mut *txn, &Fqdn::parse("1.0.10.in-addr.arpa.")?.suffixes())
                .await?
                .into_iter()
                .collect(),
        ),
        (
            "find_reverse_zone_by_normalized_name",
            find_reverse_zone_by_normalized_name(&mut *txn, "0.10.IN-ADDR.ARPA.").await?,
        ),
    ] {
        assert_eq!(domains.len(), 1, "{operation}");
        assert_eq!(
            serde_json::to_value(&domains[0])?,
            serde_json::to_value(&domain)?,
            "{operation}"
        );
    }

    let metadata: DomainMetadata =
        domain_metadata::metadata_for_domain(&mut txn, "0.10.IN-ADDR.ARPA.")
            .await?
            .into();
    assert_eq!(metadata.allow_axfr_from, allowed_transfers);

    let deleted = delete(domain.clone(), &mut txn).await?;
    assert!(deleted.updated > domain.updated);
    domain.updated = deleted.updated;
    domain.deleted = Some(deleted.updated);
    assert_eq!(
        serde_json::to_value(&deleted)?,
        serde_json::to_value(&domain)?
    );
    let stored = find_by_uuid(&mut *txn, domain.id)
        .await?
        .expect("the soft-deleted domain is retained");
    assert_eq!(
        serde_json::to_value(&stored)?,
        serde_json::to_value(&domain)?
    );

    // Leave an empty table so `persist_first` inserts on both passes. The
    // connection retains its prepared statements after the rollback.
    txn.rollback().await?;
    Ok(())
}
