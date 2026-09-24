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

use db::ObjectColumnFilter;
use model::dns::NewDomain;

use crate as db;
use crate::DatabaseError;

#[crate::sqlx_test]
async fn create_delete_valid_domain(pool: sqlx::PgPool) {
    let mut txn = pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let test_name = "nv.metal.net".to_string();

    let domain = db::dns::domain::persist(NewDomain::new(test_name), &mut txn).await;

    assert!(domain.is_ok());

    let deleted = db::dns::domain::delete(domain.unwrap(), &mut txn)
        .await
        .unwrap();
    assert_eq!(deleted.deleted, Some(deleted.updated));

    let domains = db::dns::domain::find_by(
        txn.as_mut(),
        ObjectColumnFilter::<db::dns::domain::IdColumn>::All,
    )
    .await
    .unwrap();

    assert_eq!(domains.len(), 0);

    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn normalized_reverse_zone_names_are_unique(pool: sqlx::PgPool) {
    // Dotted and non-dotted names identify the same reverse zone, so the
    // database must reject a second live spelling through the partial index.
    let mut txn = pool.begin().await.unwrap();
    db::dns::domain::persist(NewDomain::new("0.10.in-addr.arpa"), txn.as_mut())
        .await
        .unwrap();

    let duplicate =
        db::dns::domain::persist(NewDomain::new("0.10.in-addr.arpa."), txn.as_mut()).await;

    assert!(matches!(
        duplicate,
        Err(DatabaseError::Sqlx(error))
            if matches!(
                &error.source,
                sqlx::Error::Database(database_error)
                    if database_error.is_unique_violation()
                        && database_error.constraint()
                            == Some("domains_live_reverse_zone_name_key")
            )
    ));
}

#[crate::sqlx_test]
async fn reverse_zone_search_uses_normalized_identity(pool: sqlx::PgPool) {
    // Reverse-zone lookup accepts case and root-dot spelling differences, but
    // forward-domain lookup keeps its historical exact-name contract.
    let mut txn = pool.begin().await.unwrap();
    let reverse = db::dns::domain::persist(NewDomain::new("0.10.in-addr.arpa."), txn.as_mut())
        .await
        .unwrap();
    let forward = db::dns::domain::persist(NewDomain::new("tenant.example.com"), txn.as_mut())
        .await
        .unwrap();

    for name in [
        "0.10.in-addr.arpa",
        "0.10.in-addr.arpa.",
        "0.10.IN-ADDR.ARPA.",
    ] {
        let matches = db::dns::domain::find_by_name(txn.as_mut(), name)
            .await
            .unwrap();
        assert_eq!(matches.len(), 1, "reverse-zone spelling {name}");
        assert_eq!(matches[0].id, reverse.id, "reverse-zone spelling {name}");
    }

    let exact_forward = db::dns::domain::find_by_name(txn.as_mut(), "tenant.example.com")
        .await
        .unwrap();
    assert_eq!(exact_forward.len(), 1);
    assert_eq!(exact_forward[0].id, forward.id);
    assert!(
        db::dns::domain::find_by_name(txn.as_mut(), "tenant.example.com.")
            .await
            .unwrap()
            .is_empty(),
        "forward-domain searches retain exact name semantics"
    );
}

#[crate::sqlx_test]
async fn create_invalid_domain_case(pool: sqlx::PgPool) {
    let mut txn = pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let test_name = "DwRt".to_string();

    let domain = db::dns::domain::persist(NewDomain::new(test_name), &mut txn).await;

    txn.commit().await.unwrap();

    assert!(matches!(domain, Err(DatabaseError::InvalidArgument(_))));
}

#[crate::sqlx_test]
async fn create_invalid_domain_regex(pool: sqlx::PgPool) {
    let mut txn = pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let domain =
        db::dns::domain::persist(NewDomain::new("ihaveaspace.com ".to_string()), &mut txn).await;

    txn.commit().await.unwrap();

    assert!(matches!(domain, Err(DatabaseError::InvalidArgument(_))));
}

#[crate::sqlx_test]
async fn find_domain(pool: sqlx::PgPool) {
    let mut txn = pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let test_name = "nvfind.metal.net".to_string();

    let domain = db::dns::domain::persist(NewDomain::new(test_name), &mut txn).await;

    txn.commit().await.unwrap();

    assert!(domain.is_ok());

    let mut txn = pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let domains = db::dns::domain::find_by(
        txn.as_mut(),
        ObjectColumnFilter::<db::dns::domain::IdColumn>::All,
    )
    .await
    .unwrap();

    assert_eq!(domains.len(), 1);
}

#[crate::sqlx_test]
async fn update_domain(pool: sqlx::PgPool) {
    let mut txn = pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let test_name = "nv.metal.net".to_string();

    let domain = db::dns::domain::persist(NewDomain::new(test_name), &mut txn).await;

    txn.commit().await.unwrap();

    assert!(domain.is_ok());

    let updated_name = "updated.metal.net".to_string();

    let mut updated_domain = domain.unwrap();

    updated_domain.name = updated_name;
    updated_domain.increment_serial();

    let mut txn = pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let update_result = db::dns::domain::update(&updated_domain, &mut txn).await;

    txn.commit().await.unwrap();

    assert!(update_result.is_ok());
}

#[crate::sqlx_test]
async fn stale_domain_snapshot_cannot_overwrite_a_newer_update(pool: sqlx::PgPool) {
    // Even inside one transaction, the first write advances the optimistic
    // timestamp so a second write from the original snapshot is rejected.
    let mut txn = pool.begin().await.unwrap();
    let original_name = "0.10.in-addr.arpa";
    let original = db::dns::domain::persist(NewDomain::new(original_name), txn.as_mut())
        .await
        .unwrap();
    let mut first = original.clone();
    let mut stale = original;

    first.name = "1.10.in-addr.arpa".to_string();
    db::dns::domain::update(&first, txn.as_mut()).await.unwrap();

    // Both writes happen in one transaction, so this also proves the update
    // token advances independently of PostgreSQL's transaction timestamp.
    stale.name = "2.10.in-addr.arpa".to_string();
    let result = db::dns::domain::update(&stale, txn.as_mut()).await;
    assert!(matches!(
        result,
        Err(DatabaseError::ConcurrentModificationError("domain", _))
    ));
}

#[crate::sqlx_test]
async fn stale_domain_snapshot_cannot_delete_a_newer_row(pool: sqlx::PgPool) {
    // Reverse-zone cleanup must not delete a row that changed after the caller
    // selected the snapshot used to acquire its lock.
    let mut txn = pool.begin().await.unwrap();
    let original = db::dns::domain::persist(NewDomain::new("3.10.in-addr.arpa"), txn.as_mut())
        .await
        .unwrap();
    let mut updated = original.clone();
    let stale = original;

    updated.name = "4.10.in-addr.arpa".to_string();
    db::dns::domain::update(&updated, txn.as_mut())
        .await
        .unwrap();

    let result = db::dns::domain::delete(stale, txn.as_mut()).await;
    assert!(matches!(
        result,
        Err(DatabaseError::ConcurrentModificationError("domain", _))
    ));
}

// `ZoneTtl` rejects out-of-range values on decode, so a row holding one would
// be unreadable and its zone unservable. The column's CHECK stops such a value
// being written by anything other than nico-api, while NULL stays allowed.
#[crate::sqlx_test]
async fn default_ttl_column_rejects_out_of_range_values(pool: sqlx::PgPool) {
    let mut txn = pool.begin().await.unwrap();
    let domain = db::dns::domain::persist(NewDomain::new("ttl-check.example"), txn.as_mut())
        .await
        .unwrap();

    for out_of_range in [29, 86_401] {
        // A failed statement aborts its transaction, so run each attempt in
        // a savepoint that is rolled back before the next one.
        let mut attempt = sqlx::Acquire::begin(&mut txn).await.unwrap();
        let error = sqlx::query("UPDATE domains SET default_ttl = $1 WHERE id = $2")
            .bind(out_of_range)
            .bind(domain.id)
            .execute(attempt.as_mut())
            .await
            .expect_err("the CHECK constraint rejects out-of-range values");
        attempt.rollback().await.unwrap();
        assert!(
            error
                .as_database_error()
                .is_some_and(|db_error| db_error.is_check_violation()),
            "{out_of_range} fails the CHECK constraint, got {error}"
        );
    }
}
