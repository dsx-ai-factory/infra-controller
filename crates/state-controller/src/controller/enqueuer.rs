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

use ::db::DatabaseError;

use super::db;
use crate::io::StateControllerIO;

/// Allows to request state handling for objects of a certain type
#[derive(Debug, Clone)]
pub struct Enqueuer<IO: StateControllerIO> {
    pool: sqlx::PgPool,
    _phantom_object: std::marker::PhantomData<IO::ObjectId>,
}

impl<IO: StateControllerIO> Enqueuer<IO> {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            _phantom_object: std::marker::PhantomData,
        }
    }

    /// Requests state handling for the given object
    pub async fn enqueue_object(&self, object_id: &IO::ObjectId) -> Result<bool, DatabaseError> {
        let mut conn = self.pool.acquire().await.map_err(DatabaseError::acquire)?;

        let num_enqueued = db::queue_objects(
            &mut conn,
            IO::DB_QUEUED_OBJECTS_TABLE_NAME,
            &[object_id.to_string()],
        )
        .await?;

        Ok(num_enqueued == 1)
    }
}
