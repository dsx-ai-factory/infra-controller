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

use carbide_uuid::network::NetworkSegmentId;
use model::metadata::Metadata;
use model::network_prefix::NewNetworkPrefix;
use model::vpc_prefix::VpcPrefixConfig;
use sqlx::{Connection, PgPool};

use super::*;
use crate::network_prefix;

#[crate::sqlx_test]
async fn prefix_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_prefix_queries(
        &mut api_connection,
        "10.70.0.0/24".parse()?,
        "10.70.0.0/31".parse()?,
    )
    .await?;

    // Keep the API connection and its prepared statements alive while a
    // separate migration connection adds columns to the populated tables.
    assert!(api_connection.cached_statements_size() > 0);
    let mut migration_connection = pool.acquire().await?;
    let mut migration = migration_connection.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE network_prefixes ADD COLUMN test_added_column uuid;
         ALTER TABLE network_vpc_prefixes ADD COLUMN test_added_column uuid;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_prefix_queries(
        &mut api_connection,
        "10.71.0.0/24".parse()?,
        "10.71.0.0/31".parse()?,
    )
    .await?;
    Ok(())
}

async fn exercise_prefix_queries(
    connection: &mut PgConnection,
    parent_prefix: IpNetwork,
    child_prefix: IpNetwork,
) -> Result<(), Box<dyn std::error::Error>> {
    let vpc_id = VpcId::new();
    let vpc_prefix_id = VpcPrefixId::new();
    let segment_id = NetworkSegmentId::new();
    let version = ConfigVersion::initial();
    let metadata = Metadata {
        name: "prefix before update".to_string(),
        description: "cached prefix query".to_string(),
        labels: HashMap::from([("test".to_string(), "column addition".to_string())]),
    };
    let mut txn = connection.begin().await?;
    sqlx::query("INSERT INTO vpcs (id, name, version) VALUES ($1, 'prefix query test', $2)")
        .bind(vpc_id)
        .bind(version)
        .execute(&mut *txn)
        .await?;
    sqlx::query(
        "INSERT INTO network_segments (id, name, version, vpc_id)
         VALUES ($1, 'prefix query test', $2, $3)",
    )
    .bind(segment_id)
    .bind(version)
    .bind(vpc_id)
    .execute(&mut *txn)
    .await?;

    let parent = persist(
        NewVpcPrefix {
            id: vpc_prefix_id,
            site_prefix_id: None,
            overlap_vpc_id: None,
            vpc_id,
            config: VpcPrefixConfig {
                prefix: parent_prefix,
            },
            metadata: metadata.clone(),
        },
        version,
        &mut txn,
    )
    .await?;
    assert_eq!(parent.id, vpc_prefix_id);
    assert_eq!(parent.vpc_id, vpc_id);
    assert_eq!(parent.config.prefix, parent_prefix);
    assert_eq!(parent.metadata, metadata);
    assert_eq!(parent.site_prefix_id, None);
    assert_eq!(parent.status.last_used_prefix, None);
    assert_eq!(
        parent.status.controller_state.value,
        VpcPrefixControllerState::Provisioning
    );
    assert_eq!(parent.status.controller_state_outcome, None);
    assert_eq!(parent.deleted, None);

    let children = network_prefix::create_for(
        &mut txn,
        &segment_id,
        &[NewNetworkPrefix {
            prefix: child_prefix,
            gateway: Some(child_prefix.broadcast()),
            dhcpv6_link_address: None,
            num_reserved: 1,
        }],
        None,
    )
    .await?;
    let [mut child]: [NetworkPrefix; 1] = children.try_into().expect("one created network prefix");
    assert_eq!(child.segment_id, segment_id);
    assert_eq!(child.prefix, child_prefix);
    assert_eq!(child.gateway, Some(child_prefix.broadcast()));
    assert_eq!(child.dhcpv6_link_address, None);
    assert_eq!(child.num_reserved, 1);
    assert_eq!(child.vpc_prefix_id, None);
    assert_eq!(child.vpc_prefix, None);
    assert_eq!(child.svi_ip, None);
    assert_eq!(child.num_free_ips, None);

    let svi_ip = child_prefix.ip();
    network_prefix::set_svi_ip(&mut txn, child.id, &svi_ip).await?;
    child.svi_ip = Some(svi_ip);
    let vpc_ids = vec![vpc_id];
    // Some readers select only unparented prefixes. Exercise their cached
    // queries before attaching the child to its VPC prefix.
    for (name, rows) in [
        (
            "find",
            vec![network_prefix::find(&mut txn, child.id).await?],
        ),
        (
            "containing_prefix",
            network_prefix::containing_prefix(&mut *txn, &child_prefix.to_string()).await?,
        ),
        (
            "find_by",
            network_prefix::find_by(
                &mut txn,
                ObjectColumnFilter::One(network_prefix::SegmentIdColumn, &segment_id),
            )
            .await?,
        ),
        (
            "find_allocation_occupancy",
            network_prefix::find_allocation_occupancy(&mut *txn, vpc_prefix_id, parent_prefix)
                .await?,
        ),
        (
            "find_by_vpc",
            network_prefix::find_by_vpc(&mut txn, vpc_id).await?,
        ),
        (
            "find_by_vpcs",
            network_prefix::find_by_vpcs(&mut txn, &vpc_ids).await?,
        ),
    ] {
        assert_eq!(rows.len(), 1, "{name}");
        assert_eq!(
            serde_json::to_value(&rows[0])?,
            serde_json::to_value(&child)?,
            "{name}"
        );
    }
    let attached = probe_segment_prefixes(child_prefix, &mut txn).await?;
    assert_eq!(attached.len(), 1);
    assert_eq!(attached[0].vpc_id, Some(vpc_id));
    assert_eq!(attached[0].segment_type, NetworkSegmentType::Tenant);
    assert_eq!(
        serde_json::to_value(&attached[0].prefix)?,
        serde_json::to_value(&child)?
    );

    network_prefix::set_vpc_prefix(&mut child, &mut txn, &vpc_prefix_id, &parent_prefix).await?;
    assert_eq!(child.vpc_prefix_id, Some(vpc_prefix_id));
    assert_eq!(child.vpc_prefix, Some(parent_prefix));
    update_last_used_prefix(&mut txn, &vpc_prefix_id, child_prefix).await?;
    let updated_metadata = Metadata {
        name: "prefix after update".to_string(),
        description: "updated cached prefix query".to_string(),
        labels: HashMap::from([("test".to_string(), "updated column addition".to_string())]),
    };
    let updated = update(
        &UpdateVpcPrefix {
            id: vpc_prefix_id,
            metadata: updated_metadata.clone(),
        },
        &mut txn,
    )
    .await?;
    assert_eq!(updated.metadata, updated_metadata);
    assert_eq!(updated.status.last_used_prefix, Some(child_prefix));

    // Only some readers compute capacity, so compare persisted columns here.
    for (name, rows) in [
        (
            "get_by_id",
            get_by_id(
                &mut txn,
                ObjectColumnFilter::One(IdColumn, &vpc_prefix_id),
                DeletedFilter::Exclude,
            )
            .await?,
        ),
        (
            "get_for_allocation_by_ids",
            get_for_allocation_by_ids(&mut txn, &[vpc_prefix_id]).await?,
        ),
        (
            "find_allocation_candidates",
            find_allocation_candidates(&mut txn, &vpc_ids).await?,
        ),
        (
            "lock_for_allocation",
            vec![
                lock_for_allocation(&mut txn, vpc_prefix_id)
                    .await?
                    .expect("active allocation candidate"),
            ],
        ),
        ("find_by_vpc", find_by_vpc(&mut txn, vpc_id).await?),
        ("find_by_vpcs", find_by_vpcs(&mut txn, &vpc_ids).await?),
        ("probe", probe(parent_prefix, &mut txn).await?),
    ] {
        assert_eq!(rows.len(), 1, "{name}");
        let row = &rows[0];
        assert_eq!(row.id, parent.id, "{name}");
        assert_eq!(row.vpc_id, parent.vpc_id, "{name}");
        assert_eq!(row.site_prefix_id, parent.site_prefix_id, "{name}");
        assert_eq!(row.config.prefix, parent_prefix, "{name}");
        assert_eq!(row.metadata, updated_metadata, "{name}");
        assert_eq!(row.status.last_used_prefix, Some(child_prefix), "{name}");
        assert_eq!(
            row.status.controller_state.value, parent.status.controller_state.value,
            "{name}"
        );
        assert_eq!(
            row.status.controller_state.version, parent.status.controller_state.version,
            "{name}"
        );
        assert_eq!(row.status.controller_state_outcome, None, "{name}");
        assert_eq!(row.deleted, None, "{name}");
    }

    let vpc_version: ConfigVersion = sqlx::query_scalar("SELECT version FROM vpcs WHERE id = $1")
        .bind(vpc_id)
        .fetch_one(&mut *txn)
        .await?;
    assert_eq!(
        mark_as_deleted(
            &DeleteVpcPrefix { id: vpc_prefix_id },
            vpc_version,
            &mut txn
        )
        .await?,
        vpc_prefix_id
    );
    txn.commit().await?;

    let stored_child = network_prefix::find(connection, child.id).await?;
    assert_eq!(
        serde_json::to_value(stored_child)?,
        serde_json::to_value(child)?
    );
    let stored_parent = get_for_allocation_by_ids(connection, &[vpc_prefix_id]).await?;
    assert_eq!(stored_parent.len(), 1);
    assert!(stored_parent[0].deleted.is_some());
    assert_eq!(stored_parent[0].metadata, updated_metadata);
    assert_eq!(stored_parent[0].status.last_used_prefix, Some(child_prefix));
    Ok(())
}
