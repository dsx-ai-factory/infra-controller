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

use model::site_explorer::SiteExplorationReport;

use crate::DatabaseError;
use crate::db_read::DbReader;

/// Fetches the latest site exploration report from the database
pub async fn fetch<DB>(db: &mut DB) -> Result<SiteExplorationReport, DatabaseError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let endpoints = crate::explored_endpoints::find_all(&mut *db).await?;
    let managed_hosts = crate::explored_managed_host::find_all(&mut *db).await?;
    let last_run = crate::site_explorer_run_status::fetch(&mut *db).await?;
    Ok(SiteExplorationReport {
        last_run,
        endpoints,
        managed_hosts,
    })
}
