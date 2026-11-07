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

/*!
 *  Code for working the measurement_trusted_machines and measurement_trusted_profiles
 *  tables in the database, leveraging the site-specific record types.
 *
 * This also provides code for importing/exporting (and working with) SiteModels.
 */

use measured_boot::site::SiteModel;
use sqlx::PgConnection;

use crate::DatabaseResult;
use crate::db_read::DbReader;
use crate::measured_boot::interface::bundle::{
    get_measurement_bundle_records, get_measurement_bundles_values, import_measurement_bundles,
    import_measurement_bundles_values,
};
use crate::measured_boot::interface::profile::{
    export_measurement_profile_records, export_measurement_system_profiles_attrs,
    import_measurement_system_profiles, import_measurement_system_profiles_attrs,
};

/// import takes a populated SiteModel and imports it by
/// populating the corresponding profile and bundle records
/// in the database.
pub async fn import(txn: &mut PgConnection, model: &SiteModel) -> DatabaseResult<()> {
    import_measurement_system_profiles(txn, &model.measurement_system_profiles).await?;
    import_measurement_system_profiles_attrs(txn, &model.measurement_system_profiles_attrs).await?;
    import_measurement_bundles(txn, &model.measurement_bundles).await?;
    import_measurement_bundles_values(txn, &model.measurement_bundles_values).await?;
    Ok(())
}

/// export builds a SiteModel from the records in the database.
pub async fn export<DB>(txn: &mut DB) -> DatabaseResult<SiteModel>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let measurement_system_profiles = export_measurement_profile_records(&mut *txn).await?;
    let measurement_system_profiles_attrs =
        export_measurement_system_profiles_attrs(&mut *txn).await?;
    let measurement_bundles = get_measurement_bundle_records(&mut *txn).await?;
    let measurement_bundles_values = get_measurement_bundles_values(&mut *txn).await?;

    Ok(SiteModel {
        measurement_system_profiles,
        measurement_system_profiles_attrs,
        measurement_bundles,
        measurement_bundles_values,
    })
}
