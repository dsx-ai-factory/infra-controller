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

use std::ops::Deref;

use carbide_instrument::Instrumentation;
use sqlx::PgPool;

use crate::db_read::PgPoolReader;
use crate::{DatabaseError, Transaction, TransactionVending, WithTransaction};

/// A database pool paired with the Event instrumentation of its service owner.
///
/// Clones share the pool and instrument cache. The owner must retain the meter
/// provider for all transactions and scrapes. SQLx reads and pool operations
/// use the same underlying pool.
/// Use [TransactionVending::txn_begin] or [WithTransaction::with_txn] for
/// instrumented transactions; methods reached through the SQLx pool retain
/// SQLx behavior.
#[derive(Debug, Clone)]
pub struct DatabaseConnection {
    pool: PgPool,
    instrumentation: Instrumentation,
}

impl DatabaseConnection {
    /// Binds a pool to the Event metric destination of its service owner.
    pub fn new(pool: PgPool, instrumentation: Instrumentation) -> Self {
        Self {
            pool,
            instrumentation,
        }
    }

    /// Returns the underlying SQLx pool for APIs requiring a bare pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Returns the Event metric destination inherited by managed transactions.
    pub fn instrumentation(&self) -> &Instrumentation {
        &self.instrumentation
    }
}

impl Deref for DatabaseConnection {
    type Target = PgPool;
    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

impl AsRef<PgPool> for DatabaseConnection {
    fn as_ref(&self) -> &PgPool {
        &self.pool
    }
}

impl From<DatabaseConnection> for PgPoolReader {
    fn from(database: DatabaseConnection) -> Self {
        database.pool.into()
    }
}

impl WithTransaction for DatabaseConnection {
    fn database_connection(&self) -> &DatabaseConnection {
        self
    }
}

impl TransactionVending for DatabaseConnection {
    #[track_caller]
    fn txn_begin(&self) -> impl Future<Output = Result<Transaction<'_>, DatabaseError>> {
        Transaction::begin_instrumented_at(
            &self.pool,
            self.instrumentation.clone(),
            std::panic::Location::caller(),
        )
    }
}
