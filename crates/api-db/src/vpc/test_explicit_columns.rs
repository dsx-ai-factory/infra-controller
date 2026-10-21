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

use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::network_security_group::NetworkSecurityGroupId;
use model::metadata::Metadata;
use model::vpc::VpcConfig;
use model::vpc::routing_profile::VpcRoutingProfileOverrides;
use sqlx::{Connection, PgPool};

use super::*;

#[crate::sqlx_test]
async fn vpc_queries_survive_added_columns(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    crate::tenant::create_and_persist(
        "projection-tenant".to_string(),
        Metadata {
            name: "projection tenant".to_string(),
            ..Default::default()
        },
        None,
        &mut api_connection,
    )
    .await?;
    exercise_vpc_queries(&mut api_connection, 42000).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API connection's prepared statements while a migration
    // adds an unrelated column to the populated table.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE vpcs ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_vpc_queries(&mut api_connection, 43000).await?;
    Ok(())
}

async fn exercise_vpc_queries(
    connection: &mut PgConnection,
    vni: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let id = VpcId::new();
    let nsg_id = NetworkSecurityGroupId::from(uuid::Uuid::new_v4());
    sqlx::query(
        "INSERT INTO network_security_groups (id, tenant_organization_id, name, version)
         VALUES ($1, 'projection-tenant', $2, 'V1-T0')",
    )
    .bind(&nsg_id)
    .bind(id.to_string())
    .execute(&mut *txn)
    .await?;
    let metadata = Metadata {
        name: format!("projection-{id}"),
        description: "VPC projection test".to_string(),
        labels: HashMap::from([("role".to_string(), "tenant".to_string())]),
    };
    let overrides = VpcRoutingProfileOverrides {
        leak_default_route_from_underlay: Some(true),
        ..Default::default()
    };
    let mut vpc = persist(
        NewVpc {
            id,
            tenant_organization_id: "projection-tenant".to_string(),
            network_virtualization_type: VpcVirtualizationType::EthernetVirtualizer,
            metadata: metadata.clone(),
            network_security_group_id: Some(nsg_id.clone()),
            routing_profile_type: Some("initial".to_string()),
            routing_profile_overrides: Some(overrides.clone()),
            power_resource_group: Some("rack-a".to_string()),
            vni: Some(vni),
            slaac_enabled: true,
        },
        VpcStatus { vni: Some(vni) },
        &mut txn,
    )
    .await?;
    assert_eq!(vpc.id, id);
    assert_eq!(vpc.metadata, metadata);
    assert_eq!(
        vpc.config,
        VpcConfig {
            tenant_organization_id: "projection-tenant".to_string(),
            tenant_keyset_id: None,
            network_virtualization_type: VpcVirtualizationType::EthernetVirtualizer,
            network_security_group_id: Some(nsg_id),
            default_nvlink_logical_partition_id: None,
            vni: Some(vni),
            routing_profile_type: Some("initial".to_string()),
            routing_profile_overrides: Some(overrides),
            power_resource_group: Some("rack-a".to_string()),
            slaac_enabled: true,
        }
    );
    assert_eq!(vpc.status.vni, Some(vni));
    assert_eq!(vpc.version.version_nr(), 1);
    assert_eq!(vpc.created, vpc.updated);
    assert_eq!(vpc.deleted, None);

    let segment_id = NetworkSegmentId::new();
    sqlx::query(
        "INSERT INTO network_segments (id, name, version, vpc_id)
         VALUES ($1, $2, 'V1-T0', $3)",
    )
    .bind(segment_id)
    .bind(id.to_string())
    .bind(id)
    .execute(&mut *txn)
    .await?;

    for (operation, rows) in [
        (
            "find_by",
            find_by(&mut *txn, ObjectColumnFilter::One(IdColumn, &id)).await?,
        ),
        (
            "find_by_with_lock",
            find_by_with_lock(
                &mut txn,
                ObjectColumnFilter::One(IdColumn, &id),
                VpcRowLock::Mutation,
            )
            .await?,
        ),
        ("find_by_vni", find_by_vni(&mut txn, vni).await?),
        (
            "find_by_segment",
            find_by_segment(&mut *txn, segment_id)
                .await?
                .into_iter()
                .collect(),
        ),
    ] {
        assert_eq!(rows, vec![vpc.clone()], "{operation}");
    }

    let update_request = UpdateVpc {
        id,
        network_security_group_id: vpc.config.network_security_group_id.clone(),
        routing_profile_overrides: Some(VpcRoutingProfileOverrides {
            leak_tenant_host_routes_to_underlay: Some(false),
            ..Default::default()
        }),
        power_resource_group: Some(PowerResourceGroupUpdate::Set("rack-b".to_string())),
        if_version_match: Some(vpc.version),
        metadata: Metadata {
            name: format!("updated-{id}"),
            ..metadata
        },
    };
    let updated = update(&update_request, &mut txn).await?;
    vpc.metadata = update_request.metadata;
    assert_eq!(updated.version.version_nr(), vpc.version.version_nr() + 1);
    vpc.version = updated.version;
    vpc.config.routing_profile_overrides = update_request.routing_profile_overrides;
    vpc.config.power_resource_group = Some("rack-b".to_string());
    assert_eq!(updated, vpc);

    let updated = set_vni(&vpc, &mut txn, vni + 1).await?;
    vpc.config.vni = Some(vni + 1);
    vpc.status.vni = Some(vni + 1);
    assert_eq!(updated, vpc);

    let updated = change_routing_profile(
        &ChangeVpcRoutingProfile {
            id,
            if_version_match: vpc.version,
            routing_profile_type: "changed".to_string(),
            vni: Some(vni + 2),
        },
        &mut txn,
        vni + 2,
    )
    .await?;
    assert_eq!(updated.version.version_nr(), vpc.version.version_nr() + 1);
    vpc.version = updated.version;
    vpc.config.routing_profile_type = Some("changed".to_string());
    vpc.status.vni = Some(vni + 2);
    assert_eq!(updated, vpc);

    let updated = update_virtualization(
        &UpdateVpcVirtualization {
            id,
            if_version_match: Some(vpc.version),
            network_virtualization_type: VpcVirtualizationType::Fnn,
        },
        &mut txn,
    )
    .await?;
    assert_eq!(updated.version.version_nr(), vpc.version.version_nr() + 1);
    vpc.version = updated.version;
    vpc.config.network_virtualization_type = VpcVirtualizationType::Fnn;
    assert_eq!(updated, vpc);
    assert_eq!(
        find_by(&mut *txn, ObjectColumnFilter::One(IdColumn, &id)).await?,
        vec![vpc.clone()]
    );

    let deleted = try_delete(&mut txn, id).await?.expect("VPC is deleted");
    vpc.deleted = Some(vpc.updated);
    assert_eq!(deleted, vpc);
    txn.commit().await?;

    assert!(
        find_by(&mut *connection, ObjectColumnFilter::One(IdColumn, &id))
            .await?
            .is_empty()
    );
    assert_eq!(find_by_segment(connection, segment_id).await?, Some(vpc));
    Ok(())
}
