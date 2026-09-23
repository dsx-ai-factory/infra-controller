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

use std::cmp::{max, min};

use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::vpc::VpcId;
use carbide_uuid::vpc_peering::VpcPeeringId;
use ipnetwork::IpNetwork;
use model::vpc::VpcPeering;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::DatabaseError;
use crate::db_read::DbReader;

pub async fn create(
    txn: &mut PgConnection,
    vpc_id_1: VpcId,
    vpc_id_2: VpcId,
    id: VpcPeeringId,
) -> Result<VpcPeering, DatabaseError> {
    let uuid1: Uuid = vpc_id_1.into();
    let uuid2: Uuid = vpc_id_2.into();
    let vpc1_id: Uuid;
    let vpc2_id: Uuid;
    match uuid1.cmp(&uuid2) {
        std::cmp::Ordering::Equal => {
            return Err(DatabaseError::InvalidArgument(
                "Cannot create a peering between the same VPC".to_string(),
            ));
        }
        std::cmp::Ordering::Less | std::cmp::Ordering::Greater => {
            // IDs of peer VPCs should follow canonical ordering
            vpc1_id = min(uuid1, uuid2);
            vpc2_id = max(uuid1, uuid2);
        }
    }

    let query = r#"
            INSERT INTO vpc_peerings (id, vpc1_id, vpc2_id)
            SELECT $1, $2, $3
            WHERE NOT EXISTS (
                SELECT 1 FROM vpc_peerings WHERE id = $1 OR (vpc1_id = $2 AND vpc2_id = $3)
            )
            RETURNING *
        "#;

    match sqlx::query_as::<_, VpcPeering>(query)
        .bind(id)
        .bind(vpc1_id)
        .bind(vpc2_id)
        .fetch_one(txn)
        .await
    {
        Ok(vpc_peering) => Ok(vpc_peering),
        Err(sqlx::Error::RowNotFound) => Err(DatabaseError::AlreadyFoundError {
            kind: "VpcPeering",
            id: format!("id={id} between vpc1_id={vpc_id_1} and vpc2_id={vpc_id_2}"),
        }),

        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

pub async fn find_ids(
    txn: &mut PgConnection,
    vpc_id: Option<VpcId>,
) -> Result<Vec<VpcPeeringId>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT id FROM vpc_peerings");

    if let Some(vpc_id) = vpc_id {
        let vpc_id: Uuid = vpc_id.into();
        builder.push(" WHERE vpc1_id = ");
        builder.push_bind(vpc_id);
        builder.push(" OR vpc2_id = ");
        builder.push_bind(vpc_id);
    }

    let query = builder.build_query_as();
    let vpc_peering_ids: Vec<VpcPeeringId> = query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("vpc_peering::find_ids", e))?;

    Ok(vpc_peering_ids)
}

pub async fn find_by_ids(
    txn: &mut PgConnection,
    ids: Vec<VpcPeeringId>,
) -> Result<Vec<VpcPeering>, DatabaseError> {
    let query = "SELECT * FROM vpc_peerings WHERE id=ANY($1)";
    let vpc_peering_list = sqlx::query_as::<_, VpcPeering>(query)
        .bind(ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(vpc_peering_list)
}

pub async fn delete(
    txn: &mut PgConnection,
    vpc_peer_id: VpcPeeringId,
) -> Result<VpcPeering, DatabaseError> {
    let query = "DELETE FROM vpc_peerings WHERE id=$1 RETURNING *";
    let vpc_peering = sqlx::query_as::<_, VpcPeering>(query)
        .bind(vpc_peer_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(vpc_peering)
}

pub async fn get_vpc_peer_ids(
    txn: &mut PgConnection,
    vpc_id: VpcId,
) -> Result<Vec<VpcId>, DatabaseError> {
    let query = r#"
            SELECT
                CASE
                    WHEN vp.vpc1_id = $1 THEN vp.vpc2_id
                    ELSE vp.vpc1_id
                END AS vpc_peer_id
            FROM vpc_peerings vp
            WHERE vp.vpc1_id = $1 OR vp.vpc2_id = $1
        "#;

    let vpc_id: Uuid = vpc_id.into();
    let vpc_peer_ids = sqlx::query_scalar(query)
        .bind(vpc_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(vpc_peer_ids)
}

pub async fn get_vpc_peer_vnis(
    txn: &mut PgConnection,
    vpc_id: VpcId,
    virtualization_types: Vec<VpcVirtualizationType>,
) -> Result<Vec<(VpcId, i32)>, DatabaseError> {
    let query = r#"
            SELECT vpcs.id, (vpcs.status->>'vni')::integer
            FROM vpc_peerings vp
            JOIN vpcs ON vpcs.id = CASE
                WHEN vp.vpc1_id = $1 THEN vp.vpc2_id
                ELSE vp.vpc1_id
            END
            WHERE (vp.vpc1_id = $1 OR vp.vpc2_id = $1)
              AND vpcs.network_virtualization_type = ANY($2)
        "#;

    let vpc_id: Uuid = vpc_id.into();
    let peer_vpc_vnis = sqlx::query_as(query)
        .bind(vpc_id)
        .bind(virtualization_types)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(peer_vpc_vnis)
}

pub async fn delete_by_vpc_id(txn: &mut PgConnection, vpc_id: VpcId) -> Result<(), DatabaseError> {
    let query = "DELETE FROM vpc_peerings vp WHERE vp.vpc1_id =$1 OR vp.vpc2_id = $1 RETURNING *";

    let vpc_id: Uuid = vpc_id.into();
    sqlx::query_as::<_, VpcPeering>(query)
        .bind(vpc_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn get_prefixes_by_vpcs(
    txn: &mut PgConnection,
    vpcs: &Vec<VpcId>,
) -> Result<Vec<String>, DatabaseError> {
    let vpc_prefixes = crate::vpc_prefix::find_by_vpcs(txn, vpcs)
        .await?
        .into_iter()
        .map(|vpc_prefix| vpc_prefix.config.prefix.to_string());
    let vpc_segment_prefixes = crate::network_prefix::find_by_vpcs(txn, vpcs)
        .await?
        .into_iter()
        .map(|segment_prefix| segment_prefix.prefix.to_string());

    Ok(vpc_prefixes.chain(vpc_segment_prefixes).collect())
}

/// `get_retained_prefixes_by_vpcs` returns address space grouped by its source
/// VPC for peering admission. It includes deleting VPC prefixes and segment
/// prefixes until their rows are removed, since DPUs may still use their routes.
/// Linked segment prefixes are represented by their enclosing VPC prefix.
pub async fn get_retained_prefixes_by_vpcs(
    txn: impl DbReader<'_>,
    vpc_ids: &[VpcId],
) -> Result<Vec<(VpcId, IpNetwork)>, DatabaseError> {
    let query = "SELECT vpc_id, prefix FROM network_vpc_prefixes WHERE vpc_id = ANY($1)
        UNION ALL
        SELECT ns.vpc_id, np.prefix FROM network_prefixes np
        JOIN network_segments ns ON ns.id = np.segment_id
        WHERE np.vpc_prefix_id IS NULL AND ns.vpc_id = ANY($1)";
    sqlx::query_as(query)
        .bind(vpc_ids)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}
