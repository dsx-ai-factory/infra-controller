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

use carbide_uuid::domain::DomainId;
use carbide_uuid::network::NetworkPrefixId;
use chrono::{DateTime, Utc};
use model::network_segment::AllocationStrategy;
use sqlx::{Connection, PgPool};

use super::*;

#[crate::sqlx_test]
async fn network_segment_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = pool.acquire().await?;
    let mut txn = connection.begin().await?;
    let domain_id: DomainId =
        sqlx::query_scalar("INSERT INTO domains (name) VALUES ('projection.example') RETURNING id")
            .fetch_one(&mut *txn)
            .await?;
    let vpc_id: VpcId = sqlx::query_scalar(
        "INSERT INTO vpcs (name, version) VALUES ('projection-vpc', 'V1-T0') RETURNING id",
    )
    .fetch_one(&mut *txn)
    .await?;
    let id = NetworkSegmentId::new();
    sqlx::query(
        "INSERT INTO network_segments (
            id, name, subdomain_id, vpc_id, mtu, version,
            controller_state, controller_state_version, controller_state_outcome,
            vlan_id, vni_id, network_segment_type, can_stretch, allocation_strategy,
            infer_slaac_eui64_addresses, created, updated, deleted)
         VALUES ($1, 'projection-segment', $2, $3, 9000, 'V7-T1000',
            '{\"state\":\"ready\"}', 'V3-T2000',
            '{\"outcome\":\"wait\",\"reason\":\"waiting for configuration\"}',
            73, 42000, 'tenant', true, 'reserved', true,
            '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z', '2026-01-03T00:00:00Z')",
    )
    .bind(id)
    .bind(domain_id)
    .bind(vpc_id)
    .execute(&mut *txn)
    .await?;
    let prefix_id = NetworkPrefixId::new();
    sqlx::query(
        "INSERT INTO network_prefixes (id, segment_id, prefix, gateway, num_reserved)
         VALUES ($1, $2, '192.0.2.0/24', '192.0.2.1', 2)",
    )
    .bind(prefix_id)
    .bind(id)
    .execute(&mut *txn)
    .await?;
    let history = crate::state_history::persist(
        &mut txn,
        crate::state_history::StateHistoryTableId::NetworkSegment,
        &id,
        &NetworkSegmentControllerState::Ready,
        "V3-T2000".parse()?,
    )
    .await?;
    txn.commit().await?;

    for after_migration in [false, true] {
        if after_migration {
            assert!(connection.cached_statements_size() > 0);
            // Retain both prepared SELECTs while another connection commits
            // an unrelated column addition, as an API upgrade would.
            let mut migration = pool.begin().await?;
            sqlx::raw_sql(
                "SET LOCAL lock_timeout = '5s';
                 ALTER TABLE network_segments ADD COLUMN test_added_column text;",
            )
            .execute(&mut *migration)
            .await?;
            migration.commit().await?;
        }

        for include_history in [false, true] {
            let rows = find_by(
                &mut *connection,
                ObjectColumnFilter::One(IdColumn, &id),
                NetworkSegmentSearchConfig {
                    include_history,
                    ..Default::default()
                },
            )
            .await?;
            assert_eq!(rows.len(), 1);
            let segment = &rows[0];
            assert_eq!(segment.id, id);
            assert_eq!(segment.version, "V7-T1000".parse()?);
            assert_eq!(segment.config.name, "projection-segment");
            assert_eq!(segment.config.subdomain_id, Some(domain_id));
            assert_eq!(segment.config.vpc_id, Some(vpc_id));
            assert_eq!(segment.config.mtu, 9000);
            assert_eq!(segment.config.segment_type, NetworkSegmentType::Tenant);
            // These fields have decoder fallbacks, so non-default values
            // catch an omitted column that would otherwise decode silently.
            assert_eq!(
                segment.config.allocation_strategy,
                AllocationStrategy::Reserved
            );
            assert_eq!(segment.status.vlan_id, Some(73));
            assert_eq!(segment.status.vni, Some(42000));
            assert!(segment.config.infer_slaac_eui64_addresses);
            assert_eq!(segment.status.can_stretch, Some(true));
            assert_eq!(
                segment.status.controller_state.value,
                NetworkSegmentControllerState::Ready
            );
            assert_eq!(segment.status.controller_state.version, "V3-T2000".parse()?);
            assert_eq!(
                segment.status.controller_state_outcome,
                Some(PersistentStateHandlerOutcome::Wait {
                    reason: "waiting for configuration".to_string(),
                    source_ref: None,
                })
            );
            assert_eq!(
                segment.created,
                "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>()?
            );
            assert_eq!(
                segment.updated,
                "2026-01-02T00:00:00Z".parse::<DateTime<Utc>>()?
            );
            assert_eq!(segment.deleted, Some("2026-01-03T00:00:00Z".parse()?));

            assert_eq!(segment.prefixes.len(), 1);
            let prefix = &segment.prefixes[0];
            assert_eq!(prefix.id, prefix_id);
            assert_eq!(prefix.segment_id, id);
            assert_eq!(prefix.prefix, "192.0.2.0/24".parse()?);
            assert_eq!(prefix.gateway, Some("192.0.2.1".parse()?));
            assert_eq!(prefix.num_reserved, 2);
            assert_eq!(prefix.num_free_ips, None);

            if include_history {
                assert_eq!(segment.status.history.len(), 1);
                let found = &segment.status.history[0];
                assert_eq!(found.state, history.state);
                assert_eq!(found.state_version, history.state_version);
                assert_eq!(found.time, history.time);
            } else {
                assert!(segment.status.history.is_empty());
            }
        }
    }
    Ok(())
}
