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
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use carbide_uuid::machine::MachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use chrono::{DateTime, Utc};
use hyper::http::StatusCode;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    MachineStateHistoriesRequest, PowerShelfStateHistoriesRequest, RackStateHistoriesRequest,
    SwitchStateHistoriesRequest,
};

use super::{Base, filters};

#[derive(Template)]
#[template(path = "state_history.html")]
struct StateHistory {
    id: String,
    /// The type of object that the history is for in humand readable
    /// form. E.g. Host, Switch, PowerShelf, etc.
    object_type: String,
    /// The base URL that is used for this type of object. E.g. `machine`
    object_url_path: String,
    history: StateHistoryTable,
}

/// State history, newest record first.
#[derive(Template)]
#[template(path = "state_history_table.html")]
pub(super) struct StateHistoryTable {
    pub(super) records: Vec<StateHistoryRecord>,
}

/// One state history record prepared for display.
struct StateHistoryEntry<'a> {
    record: &'a StateHistoryRecord,
    entered_at: String,
    duration: String,
    is_current: bool,
}

impl StateHistoryTable {
    fn entries(&self) -> Vec<StateHistoryEntry<'_>> {
        let entered_at: Vec<_> = self
            .records
            .iter()
            .map(|record| {
                record
                    .version
                    .parse::<::config_version::ConfigVersion>()
                    .ok()
                    .map(|version| version.timestamp())
            })
            .collect();
        let durations = history_durations(&entered_at);
        self.records
            .iter()
            .zip(entered_at)
            .zip(durations)
            .enumerate()
            .map(
                |(index, ((record, entered_at), duration))| StateHistoryEntry {
                    record,
                    entered_at: entered_at
                        .map(format_history_time)
                        .unwrap_or_else(|| record.version.clone()),
                    duration,
                    is_current: index == 0,
                },
            )
            .collect()
    }
}

/// How long each newest-first record lasted: until the next newer record, or
/// until now for the newest. Empty where a time is unknown.
pub(super) fn history_durations(entered_at: &[Option<DateTime<Utc>>]) -> Vec<String> {
    entered_at
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = if index == 0 {
                Some(Utc::now())
            } else {
                entered_at[index - 1]
            };
            match (start, end) {
                (Some(start), Some(end)) if end >= *start => {
                    ::config_version::format_duration(end - *start)
                }
                _ => String::new(),
            }
        })
        .collect()
}

pub(super) fn format_history_time(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

#[derive(Debug, serde::Serialize)]
pub(super) struct StateHistoryRecord {
    state: String,
    version: String,
}

impl From<::rpc::forge::MachineEvent> for StateHistoryRecord {
    fn from(record: ::rpc::forge::MachineEvent) -> Self {
        Self {
            state: record.event,
            version: record.version,
        }
    }
}

impl From<::rpc::forge::StateHistoryRecord> for StateHistoryRecord {
    fn from(record: ::rpc::forge::StateHistoryRecord) -> Self {
        Self {
            state: record.state,
            version: record.version,
        }
    }
}

macro_rules! define_show_state_history_handlers {
    (
        // name of the generated rendering function
        $fn_name:ident,
        // name of the generated json handler function
        $fn_name_json:ident,
        /// The function to fetch objects
        fetch_fn_name = $fetch_fn_name:ident,
        // Type of object that the macro is generating functions for as
        // displayed in the UI. This should be a string literal
        object_type_display = $object_type_display:literal,
        // Path segment after "admin/", e.g. "machine" or "power-shelf"
        object_url_path = $object_url_path:literal,
    ) => {
        pub(super) async fn $fn_name(
            AxumState(state): AxumState<Arc<Api>>,
            AxumPath(id): AxumPath<String>,
        ) -> Response {
            let (machine_id, records) = match $fetch_fn_name(&state, &id).await {
                Ok((id, records)) => (id, records),
                Err((code, msg)) => return (code, msg).into_response(),
            };

            let records = records.into_iter().map(Into::into).collect();

            let display = StateHistory {
                id: machine_id.to_string(),
                object_type: $object_type_display.to_string(),
                object_url_path: $object_url_path.to_string(),
                history: StateHistoryTable { records },
            };

            (StatusCode::OK, Html(display.render().unwrap())).into_response()
        }

        pub(super) async fn $fn_name_json(
            AxumState(state): AxumState<Arc<Api>>,
            AxumPath(id): AxumPath<String>,
        ) -> Response {
            let (_machine_id, health_records) = match $fetch_fn_name(&state, &id).await {
                Ok((id, records)) => (id, records),
                Err((code, msg)) => return (code, msg).into_response(),
            };
            (StatusCode::OK, Json(health_records)).into_response()
        }
    };
}

macro_rules! define_fetch_state_history_records {
    (
        // name of the generated function
        $visibility:vis $fn_name:ident,
        // type of the ID (MachineId, SwitchId, ...)
        id_type = $Id:ty,
        // canonical structured log field for this ID type
        id_log_field = $id_log_field:ident,
        // field in the request holding the vector of IDs (machine_ids, switch_ids, ...)
        id_vec_field = $ids_field:ident,
        // RPC method on Api (find_machine_state_histories, find_switch_state_histories, ...)
        api_method = $api_method:ident,
        // request type passed to tonic::Request::new
        request_type = $RequestType:ident,
        // event type in the Vec in the result
        record_type = $Record:ty,
    ) => {
        /// Fetches state history records for object with ID `id_str`
        $visibility async fn $fn_name(
            api: &Api,
            id_str: &str,
        ) -> Result<($Id, Vec<$Record>), (http::StatusCode, String)> {
            use std::str::FromStr;

            let Ok(id) = <$Id>::from_str(id_str) else {
                return Err((http::StatusCode::BAD_REQUEST, "invalid id".to_string()));
            };

            let mut histories = match api
                .$api_method(::tonic::Request::new(
                    $RequestType
                    {
                        $ids_field: vec![id.clone()],
                    },
                ))
                .await
            {
                Ok(response) => response.into_inner().histories,
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        $id_log_field = %id,
                        operation = stringify!($api_method),
                        "State history request failed"
                    );
                    return Err((
                        http::StatusCode::INTERNAL_SERVER_ERROR,
                        concat!("Failed ", stringify!($api_method)).to_string(),
                    ));
                }
            };

            let mut records = histories
                .remove(&id.to_string())
                .unwrap_or_default()
                .records;

            // History is delivered oldest-first; reverse for display ordering
            records.reverse();

            Ok((id, records))
        }
    };
}

define_fetch_state_history_records!(
    fetch_machine_state_history_records,
    id_type = MachineId,
    id_log_field = machine_id,
    id_vec_field = machine_ids,
    api_method = find_machine_state_histories,
    request_type = MachineStateHistoriesRequest,
    record_type = ::rpc::forge::MachineEvent,
);

define_fetch_state_history_records!(
    pub(super) fetch_power_shelf_state_history_records,
    id_type = PowerShelfId,
    id_log_field = power_shelf_id,
    id_vec_field = power_shelf_ids,
    api_method = find_power_shelf_state_histories,
    request_type = PowerShelfStateHistoriesRequest,
    record_type = ::rpc::forge::StateHistoryRecord,
);

define_fetch_state_history_records!(
    pub(super) fetch_rack_state_history_records,
    id_type = RackId,
    id_log_field = rack_id,
    id_vec_field = rack_ids,
    api_method = find_rack_state_histories,
    request_type = RackStateHistoriesRequest,
    record_type = ::rpc::forge::StateHistoryRecord,
);

define_fetch_state_history_records!(
    pub(super) fetch_switch_state_history_records,
    id_type = SwitchId,
    id_log_field = switch_id,
    id_vec_field = switch_ids,
    api_method = find_switch_state_histories,
    request_type = SwitchStateHistoriesRequest,
    record_type = ::rpc::forge::StateHistoryRecord,
);

define_show_state_history_handlers!(
    show_machine_state_history,
    show_machine_state_history_json,
    fetch_fn_name = fetch_machine_state_history_records,
    object_type_display = "Host",
    object_url_path = "machine",
);

define_show_state_history_handlers!(
    show_power_shelf_state_history,
    show_power_shelf_state_history_json,
    fetch_fn_name = fetch_power_shelf_state_history_records,
    object_type_display = "Power Shelf",
    object_url_path = "power-shelf",
);

define_show_state_history_handlers!(
    show_rack_state_history,
    show_rack_state_history_json,
    fetch_fn_name = fetch_rack_state_history_records,
    object_type_display = "Rack",
    object_url_path = "rack",
);

define_show_state_history_handlers!(
    show_switch_state_history,
    show_switch_state_history_json,
    fetch_fn_name = fetch_switch_state_history_records,
    object_type_display = "Switch",
    object_url_path = "switch",
);

impl super::Base for StateHistory {}
