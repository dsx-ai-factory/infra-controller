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

use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::State as AxumState;
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use hyper::http::StatusCode;
use model::firmware::DesiredFirmwareVersions;
use sqlx::types::Json as SqlxJson;

use super::Base;

const DESIRED_FIRMWARE_QUERY: &str = r#"
    SELECT vendor, model, versions, explicit_update_start_needed
    FROM desired_firmware
    ORDER BY vendor, model
"#;

#[derive(Template)]
#[template(path = "firmware_show.html")]
struct FirmwareShow {
    desired_firmware: Vec<DesiredFirmwareDisplay>,
}

struct DesiredFirmwareDisplay {
    vendor: String,
    model: String,
    versions: Vec<FirmwareVersionDisplay>,
    explicit_update_start_needed: bool,
}

struct FirmwareVersionDisplay {
    component: String,
    version: String,
}

#[derive(serde::Serialize)]
struct DesiredFirmware {
    vendor: String,
    model: String,
    versions: DesiredFirmwareVersions,
    explicit_update_start_needed: bool,
}

#[derive(sqlx::FromRow)]
struct DesiredFirmwareRow {
    vendor: String,
    model: String,
    versions: SqlxJson<DesiredFirmwareVersions>,
    explicit_update_start_needed: bool,
}

pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let desired_firmware = match fetch_desired_firmware(&state).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, "fetch desired firmware");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading desired firmware",
            )
                .into_response();
        }
    };

    let tmpl = FirmwareShow {
        desired_firmware: desired_firmware.iter().map(Into::into).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let desired_firmware = match fetch_desired_firmware(&state).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, "fetch desired firmware");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading desired firmware",
            )
                .into_response();
        }
    };

    (StatusCode::OK, Json(desired_firmware)).into_response()
}

async fn fetch_desired_firmware(api: &Api) -> Result<Vec<DesiredFirmware>, sqlx::Error> {
    sqlx::query_as::<_, DesiredFirmwareRow>(DESIRED_FIRMWARE_QUERY)
        .fetch_all(&api.database_connection)
        .await
        .map(|rows| rows.into_iter().map(Into::into).collect())
}

impl From<DesiredFirmwareRow> for DesiredFirmware {
    fn from(row: DesiredFirmwareRow) -> Self {
        Self {
            vendor: row.vendor,
            model: row.model,
            versions: row.versions.0,
            explicit_update_start_needed: row.explicit_update_start_needed,
        }
    }
}

impl From<&DesiredFirmware> for DesiredFirmwareDisplay {
    fn from(row: &DesiredFirmware) -> Self {
        Self {
            vendor: row.vendor.clone(),
            model: row.model.clone(),
            versions: display_versions(&row.versions),
            explicit_update_start_needed: row.explicit_update_start_needed,
        }
    }
}

fn display_versions(versions: &DesiredFirmwareVersions) -> Vec<FirmwareVersionDisplay> {
    let mut versions = versions
        .versions
        .iter()
        .map(|(component_type, version)| FirmwareVersionDisplay {
            component: component_type.to_string(),
            version: version.clone(),
        })
        .collect::<Vec<_>>();
    versions.sort_unstable_by(|left, right| left.component.cmp(&right.component));
    versions
}

impl Base for FirmwareShow {}
