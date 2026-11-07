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

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use sqlx::PgTransaction;

use crate::state_handler::StateHandlerError;

/// A DbWriteBatch exists to allow state controllers to enqueue write operations until the end of
/// processing, so that they don't need to hold a database connection open across long-running work.
/// If the state handler returns an error, the write operations are discarded, similarly to how a
/// transaction is rolled back. If a state handler returns successfully, the write operations are
/// all done at once inside a transaction before committing.
///
/// # Usage
///
/// You can pass a FnOnce closure that accepts a transaction, that will be called when your state handler is successful. For example:
///
/// ```ignore
/// let write_batch = DbWriteBatch::new();
/// write_batch.push(move |txn| async move {
///     db::machine::find_by_ip(txn, &Ipv4Addr::new(17, 0, 0, 1)).await
/// }.boxed());
///
/// // Later the controller will do:
/// write_batch.apply_all(&mut txn);
/// ```
///
/// You can also implement [`WriteOp`] manually for any given type, allowing write operations to be
/// reused.
#[derive(Default)]
pub struct DbWriteBatch {
    writes: Vec<Box<dyn WriteOp>>,
}

#[async_trait]
pub trait WriteOp: Send {
    async fn apply<'a, 't: 'a>(
        self: Box<Self>,
        txn: &'a mut PgTransaction<'t>,
    ) -> Result<(), StateHandlerError>;
}

impl std::fmt::Debug for DbWriteBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbWriteBatch")
            .field("writes", &self.writes.len())
            .finish()
    }
}

pub type WriteOpFn = Box<
    dyn for<'t> FnOnce(&'t mut PgTransaction) -> BoxFuture<'t, Result<(), StateHandlerError>>
        + Send
        + Sync
        + 'static,
>;

#[async_trait]
impl WriteOp for WriteOpFn {
    async fn apply<'a, 't: 'a>(
        self: Box<Self>,
        txn: &'a mut PgTransaction<'t>,
    ) -> Result<(), StateHandlerError> {
        (*self)(txn).await
    }
}

impl DbWriteBatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, op: impl WriteOp + 'static) {
        self.writes.push(Box::new(op));
    }

    pub async fn apply_all(self, txn: &mut PgTransaction<'_>) -> Result<(), StateHandlerError> {
        for w in self.writes {
            w.apply(txn).await?;
        }
        Ok(())
    }
}

impl From<Vec<Box<dyn WriteOp>>> for DbWriteBatch {
    fn from(writes: Vec<Box<dyn WriteOp>>) -> Self {
        Self { writes }
    }
}
