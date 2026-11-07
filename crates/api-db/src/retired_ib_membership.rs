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

//! Durable records for IB memberships that should stay removed. A record
//! survives UFM removal and is deleted only when the exact membership is
//! assigned again.

use model::ib::IbMembership;
use model::ib_partition::PartitionKey;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// `record` records the exact membership in the transaction that retires it.
///
/// The caller must serialize the authoritative live `Instance` transition and
/// use the same transaction for both changes. Repeated records for the exact
/// membership are idempotent. Callers that process multiple memberships must
/// acquire retired-membership locks in stable fabric, PKey, and GUID order.
pub async fn record(txn: &mut PgConnection, membership: &IbMembership) -> DatabaseResult<()> {
    let query = "INSERT INTO retired_ib_memberships (fabric, pkey, guid)
        VALUES ($1, $2, $3)
        ON CONFLICT (fabric, pkey, guid) DO NOTHING";

    sqlx::query(query)
        .bind(&membership.fabric)
        .bind(i32::from(u16::from(membership.pkey)))
        .bind(&membership.guid)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// `find_recorded_candidates` returns candidates that have retired membership
/// records.
///
/// An empty candidate list returns without querying the database. Duplicate
/// candidates appear once in the result. Results are ordered by `fabric`,
/// PKey, and GUID.
pub async fn find_recorded_candidates(
    db: impl DbReader<'_>,
    candidates: &[IbMembership],
) -> DatabaseResult<Vec<IbMembership>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let query = "SELECT DISTINCT retired.fabric, retired.pkey, retired.guid
        FROM retired_ib_memberships AS retired
        INNER JOIN UNNEST($1::text[], $2::integer[], $3::text[])
            AS candidate(fabric, pkey, guid)
            ON candidate.fabric = retired.fabric
            AND candidate.pkey = retired.pkey
            AND candidate.guid = retired.guid
        ORDER BY retired.fabric, retired.pkey, retired.guid";

    let fabrics = candidates
        .iter()
        .map(|membership| membership.fabric.clone())
        .collect::<Vec<_>>();
    let pkeys = candidates
        .iter()
        .map(|membership| i32::from(u16::from(membership.pkey)))
        .collect::<Vec<_>>();
    let guids = candidates
        .iter()
        .map(|membership| membership.guid.clone())
        .collect::<Vec<_>>();

    let rows: Vec<(String, i32, String)> = sqlx::query_as(query)
        .bind(fabrics)
        .bind(pkeys)
        .bind(guids)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    rows.into_iter().map(membership_from_row).collect()
}

fn membership_from_row(
    (fabric, pkey, guid): (String, i32, String),
) -> DatabaseResult<IbMembership> {
    let pkey = u16::try_from(pkey)
        .ok()
        .and_then(|pkey| PartitionKey::try_from(pkey).ok())
        .ok_or_else(|| {
            DatabaseError::internal(format!(
                "retired_ib_memberships contains invalid pkey {pkey}"
            ))
        })?;
    Ok(IbMembership { fabric, pkey, guid })
}

/// `remove_for_reuse` removes the retired record in the transaction that
/// assigns the exact membership to a live `Instance`.
///
/// Callers must hold the locks that serialize assignment and verify the current
/// `Instance` in this transaction. Removing the membership from UFM is not
/// enough to delete the record. Callers that process multiple memberships must
/// acquire retired-membership locks in stable fabric, PKey, and GUID order.
/// Returns `true` when the exact record existed and was deleted.
pub async fn remove_for_reuse(
    txn: &mut PgConnection,
    membership: &IbMembership,
) -> DatabaseResult<bool> {
    let query = "DELETE FROM retired_ib_memberships
        WHERE fabric = $1 AND pkey = $2 AND guid = $3";

    sqlx::query(query)
        .bind(&membership.fabric)
        .bind(i32::from(u16::from(membership.pkey)))
        .bind(&membership.guid)
        .execute(txn)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|e| DatabaseError::query(query, e))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, check_cases_async};
    use model::ib::IbMembership;
    use model::ib_partition::PartitionKey;

    use super::{find_recorded_candidates, record, remove_for_reuse};

    fn membership(fabric: &str, pkey: u16, guid: &str) -> IbMembership {
        IbMembership {
            fabric: fabric.to_string(),
            pkey: PartitionKey::try_from(pkey).expect("test PKey must be valid"),
            guid: guid.to_string(),
        }
    }

    #[crate::sqlx_test]
    async fn migration_enforces_the_pkey_range(pool: sqlx::PgPool) {
        check_cases_async(
            [
                Case {
                    scenario: "minimum PKey",
                    input: 0,
                    expect: Yields(()),
                },
                Case {
                    scenario: "maximum PKey",
                    input: 32767,
                    expect: Yields(()),
                },
                Case {
                    scenario: "negative PKey",
                    input: -1,
                    expect: Fails,
                },
                Case {
                    scenario: "PKey above the maximum",
                    input: 32768,
                    expect: Fails,
                },
            ],
            |pkey| {
                let pool = pool.clone();
                async move {
                    sqlx::query(
                        "INSERT INTO retired_ib_memberships (fabric, pkey, guid) \
                         VALUES ('fabric-a', $1, $1::text)",
                    )
                    .bind(pkey)
                    .execute(&pool)
                    .await
                    .map(|_| ())
                    .map_err(drop)
                }
            },
        )
        .await;
    }

    #[crate::sqlx_test]
    async fn repeated_record_is_idempotent(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let membership = membership("fabric-a", 0x101, "guid-a");

        record(txn.as_mut(), &membership).await.unwrap();
        record(txn.as_mut(), &membership).await.unwrap();

        assert_eq!(
            find_recorded_candidates(txn.as_mut(), &[membership.clone(), membership.clone()])
                .await
                .unwrap(),
            vec![membership]
        );
        assert!(
            find_recorded_candidates(txn.as_mut(), &[])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[crate::sqlx_test]
    async fn find_recorded_candidates_match_exact_memberships(pool: sqlx::PgPool) {
        let candidates = [
            membership("fabric-a", 0x101, "guid-a"),
            membership("fabric-b", 0x102, "guid-b"),
        ];
        let false_cross_product_match = membership("fabric-a", 0x102, "guid-b");
        let mut txn = pool.begin().await.unwrap();
        for membership in candidates
            .iter()
            .chain(std::iter::once(&false_cross_product_match))
        {
            record(txn.as_mut(), membership).await.unwrap();
        }

        assert_eq!(
            find_recorded_candidates(txn.as_mut(), &candidates)
                .await
                .unwrap(),
            candidates
        );
    }

    #[crate::sqlx_test]
    async fn record_remains_after_machine_and_instance_deletion(pool: sqlx::PgPool) {
        let membership = membership("fabric-a", 0x101, "guid-a");
        let mut txn = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO machines (id, dpf) \
             VALUES ('retired-membership-machine', '{}'::jsonb)",
        )
        .execute(txn.as_mut())
        .await
        .unwrap();
        sqlx::query("INSERT INTO instances (machine_id) VALUES ('retired-membership-machine')")
            .execute(txn.as_mut())
            .await
            .unwrap();
        record(txn.as_mut(), &membership).await.unwrap();
        txn.commit().await.unwrap();

        let mut txn = pool.begin().await.unwrap();
        sqlx::query("DELETE FROM instances WHERE machine_id = 'retired-membership-machine'")
            .execute(txn.as_mut())
            .await
            .unwrap();
        sqlx::query("DELETE FROM machines WHERE id = 'retired-membership-machine'")
            .execute(txn.as_mut())
            .await
            .unwrap();
        txn.commit().await.unwrap();

        assert_eq!(
            find_recorded_candidates(&pool, std::slice::from_ref(&membership))
                .await
                .unwrap(),
            vec![membership]
        );
    }

    #[crate::sqlx_test]
    async fn reuse_removes_only_the_matching_record(pool: sqlx::PgPool) {
        let exact = membership("fabric-a", 0x101, "guid-a");
        let retained = [
            membership("fabric-b", 0x101, "guid-a"),
            membership("fabric-a", 0x102, "guid-a"),
            membership("fabric-a", 0x101, "guid-b"),
        ];
        let mut txn = pool.begin().await.unwrap();
        for candidate in std::iter::once(&exact).chain(&retained) {
            record(txn.as_mut(), candidate).await.unwrap();
        }

        assert!(remove_for_reuse(txn.as_mut(), &exact).await.unwrap());
        assert!(!remove_for_reuse(txn.as_mut(), &exact).await.unwrap());

        let missing = membership("fabric-a", 0x103, "guid-a");
        assert_eq!(
            find_recorded_candidates(
                txn.as_mut(),
                &[
                    exact,
                    retained[0].clone(),
                    retained[1].clone(),
                    retained[2].clone(),
                    missing,
                ],
            )
            .await
            .unwrap(),
            vec![
                retained[2].clone(),
                retained[1].clone(),
                retained[0].clone()
            ],
        );
    }
}
