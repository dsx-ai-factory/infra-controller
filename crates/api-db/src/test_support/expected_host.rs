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

//! Shared bodies for the expected-host CRUD suites.
//!
//! `expected_machines`, `expected_switches`, and `expected_power_shelves` are
//! distinct row types with their own constructors and update paths, so most of
//! their per-entity tests stay per-entity. The delete-then-confirm-absent flow,
//! however, is identical down to the operation signatures
//! (`delete_by_mac(txn, mac)` and `find_by_bmc_mac_address(txn, mac)`), so it
//! lives here once and each entity's `#[sqlx_test]` hands in those two
//! operations as async closures.

use mac_address::MacAddress;
use sqlx::PgConnection;

use crate::DatabaseResult;

/// Deletes the row identified by `mac`, commits, then asserts in a fresh
/// transaction that it can no longer be found.
///
/// `delete` and `find` are each entity's own `delete_by_mac` /
/// `find_by_bmc_mac_address`; `T` is whatever row type the finder returns. The
/// caller owns the per-test pool (one pool per `#[sqlx_test]`), so it is passed
/// in and the two transactions are opened here.
pub(crate) async fn assert_delete_by_mac_removes_row<T>(
    pool: &sqlx::PgPool,
    mac: MacAddress,
    delete: impl AsyncFnOnce(&mut PgConnection, MacAddress) -> DatabaseResult<()>,
    find: impl AsyncFnOnce(&mut PgConnection, MacAddress) -> DatabaseResult<Option<T>>,
) {
    let mut txn = pool
        .begin()
        .await
        .expect("unable to create transaction on database pool");

    delete(&mut txn, mac)
        .await
        .expect("Error deleting expected host");

    txn.commit().await.expect("Failed to commit transaction");

    let mut txn = pool
        .begin()
        .await
        .expect("unable to create transaction on database pool");

    assert!(
        find(&mut txn, mac)
            .await
            .expect("lookup after delete failed")
            .is_none(),
        "expected host should be gone after delete",
    );
}
