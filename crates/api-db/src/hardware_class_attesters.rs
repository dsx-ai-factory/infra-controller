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
use model::site_explorer::AttesterSet;
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

/// Records an attester set against a hardware class, and reports whether the
/// class had not carried this set before.
///
/// Re-seeing a set is the common case and leaves the row alone, so `first_seen`
/// stays the first observation. A caller acts on the `true` only: a class
/// growing a second set means hardware under one policy stopped matching its
/// peers.
pub async fn record(
    txn: &mut PgConnection,
    hardware_class: &str,
    attesters: &AttesterSet,
) -> DatabaseResult<bool> {
    let query = r#"
        INSERT INTO hardware_class_attesters (hardware_class, attester_digest, attester_ids)
        VALUES ($1, $2, $3::jsonb)
        ON CONFLICT (hardware_class, attester_digest) DO NOTHING
    "#;
    let inserted = sqlx::query(query)
        .bind(hardware_class)
        .bind(&attesters.digest)
        .bind(sqlx::types::Json(&attesters.ids))
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    Ok(inserted.rows_affected() > 0)
}

#[cfg(test)]
mod test {
    use chrono::{DateTime, Utc};
    use model::site_explorer::{ComponentIntegrityEntry, EndpointExplorationReport};

    use super::*;

    const CLASS: &str = "nvidia_dgx-gb300";

    fn entry(id: &str, integrity_type: &str) -> ComponentIntegrityEntry {
        ComponentIntegrityEntry {
            id: id.to_string(),
            component_integrity_type: integrity_type.to_string(),
            component_integrity_enabled: true,
        }
    }

    fn attester_set(entries: Vec<ComponentIntegrityEntry>) -> AttesterSet {
        EndpointExplorationReport {
            component_integrities: Some(entries),
            ..Default::default()
        }
        .attester_set()
        .expect("a reported collection yields a set")
    }

    async fn first_seen(txn: &mut PgConnection, digest: &str) -> DateTime<Utc> {
        sqlx::query_scalar(
            "SELECT first_seen FROM hardware_class_attesters WHERE attester_digest = $1",
        )
        .bind(digest)
        .fetch_one(txn)
        .await
        .expect("read first_seen")
    }

    /// A class that grows a second set has to keep both, and the first row's
    /// `first_seen` has to survive: it is what dates the drift.
    #[crate::sqlx_test]
    async fn a_second_set_is_recorded_beside_the_first(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let eight = attester_set((0..8).map(|n| entry(&format!("GPU_{n}"), "SPDM")).collect());
        let seven = attester_set((0..7).map(|n| entry(&format!("GPU_{n}"), "SPDM")).collect());

        assert!(record(&mut txn, CLASS, &eight).await.unwrap());
        let originally_seen = first_seen(&mut txn, &eight.digest).await;

        assert!(
            record(&mut txn, CLASS, &seven).await.unwrap(),
            "a tray reporting one attester fewer is a set the class has not carried"
        );
        assert!(
            !record(&mut txn, CLASS, &eight).await.unwrap(),
            "re-seeing a set must report nothing new, so the event stays silent"
        );
        assert_eq!(
            first_seen(&mut txn, &eight.digest).await,
            originally_seen,
            "re-seeing a set must not restamp when it was first observed"
        );

        let recorded: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM hardware_class_attesters WHERE hardware_class = $1",
        )
        .bind(CLASS)
        .fetch_one(&mut *txn)
        .await
        .unwrap();
        assert_eq!(recorded, 2);
    }

    /// Two classes can be built around the same baseboard and report the same
    /// attesters, so the digest is keyed per class rather than on its own.
    #[crate::sqlx_test]
    async fn one_digest_can_belong_to_two_classes(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let shared = attester_set(vec![entry("ERoT_BMC_0", "SPDM")]);

        assert!(record(&mut txn, CLASS, &shared).await.unwrap());
        assert!(
            record(&mut txn, "supermicro_ars-121l", &shared)
                .await
                .unwrap()
        );
    }
}
