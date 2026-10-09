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

use carbide_uuid::vpc::VpcId;
use sqlx::PgConnection;

/// Inserts a minimal VPC row for DNS ownership tests.
///
/// The version must parse as a `ConfigVersion` because domain validation reads
/// the row back through `vpc::find_by_with_lock`. Every fixture VPC shares one
/// organization because domain ownership validation does not depend on it.
pub(crate) async fn insert_vpc(conn: &mut PgConnection, name: &str) -> VpcId {
    sqlx::query_scalar(
        "INSERT INTO vpcs (name, version, organization_id) VALUES ($1, 'V1-T0', 'dns-test') RETURNING id",
    )
    .bind(name)
    .fetch_one(conn)
    .await
    .expect("insert fixture VPC")
}
