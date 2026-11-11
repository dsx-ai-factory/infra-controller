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

use model::firmware::{DesiredFirmwareVersions, Firmware};
use serde::Serialize;
use sqlx::PgConnection;

use super::DatabaseError;

/// One `desired_firmware` row: the default version of each component with
/// known firmware, keyed by the report's vendor and model.
#[derive(Debug, Serialize)]
pub struct DesiredFirmwareRow {
    pub vendor: String,
    pub model: String,
    pub versions: DesiredFirmwareVersions,
    pub explicit_update_start_needed: bool,
}

/// The `desired_firmware` rows for `models`. A model without known firmware
/// gets no row.
pub fn desired_firmware_rows<'a>(
    models: impl IntoIterator<Item = &'a Firmware>,
) -> Vec<DesiredFirmwareRow> {
    models
        .into_iter()
        .filter_map(|model| {
            let mut model = model.clone();
            model
                .components
                .retain(|_, component| !component.known_firmware.is_empty());
            if model.components.is_empty() {
                return None;
            }
            Some(DesiredFirmwareRow {
                vendor: model.vendor.to_pascalcase(),
                model: model.model.clone(),
                explicit_update_start_needed: model.explicit_start_needed,
                versions: DesiredFirmwareVersions::from(model),
            })
        })
        .collect()
}

/// snapshot_desired_firmware will replace the desired_firmware table with one matching the given Firmware models
pub async fn snapshot_desired_firmware(
    txn: &mut PgConnection,
    models: &[Firmware],
) -> Result<(), DatabaseError> {
    let query = "DELETE FROM desired_firmware";
    sqlx::query(query)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    let query = "INSERT INTO desired_firmware (vendor, model, versions, explicit_update_start_needed) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING";
    for row in desired_firmware_rows(models) {
        sqlx::query(query)
            .bind(row.vendor)
            .bind(row.model)
            .bind(sqlx::types::Json(row.versions))
            .bind(row.explicit_update_start_needed)
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
    }

    Ok(())
}
