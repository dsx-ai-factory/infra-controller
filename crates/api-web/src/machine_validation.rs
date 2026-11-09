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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path as AxumPath, Query, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use axum::{Extension, Json};
use carbide_api_core::{Api, AuthContext};
use carbide_uuid::machine_validation::MachineValidationId;
use hyper::http::StatusCode;
use rpc::forge::forge_server::Forge;
use rpc::forge::{self as forgerpc};

use super::Base;
use super::machine::ValidationRun;
use super::pagination::{self, PageContext, PaginationParams};

#[derive(Debug)]
struct ValidationResult {
    validation_id: String,
    name: String,
    test_id: String,
    attempt_id: Option<String>,
    status: String,
    start_time: String,
    start_time_sort: i64,
    start_time_short: String,
    start_date: String,
    end_time: String,
    duration: String,
    duration_ms: Option<i64>,
}

struct ValidateTest {
    id: String,
    version: String,
    name: String,
    description: String,
    contexts: String,
    supported_platforms: String,
    command: String,
    args: String,
    tags: String,
    is_verified: bool,
    is_enabled: bool,
}

struct ValidateTestDetails {
    test_id: String,
    version: String,
    name: String,
    description: String,
    contexts: String,
    supported_platforms: String,
    command: String,
    args: String,
    tags: String,
    is_verified: bool,
    is_enabled: bool,
    timeout: String,
    extra_output_file: String,
    extra_err_file: String,
    pre_condition: String,
    img_name: String,
    container_arg: String,
    external_config_file: String,
    components: String,
}

#[derive(Debug)]
struct ValidationResultDetail {
    validation_id: String,
    name: String,
    context: String,
    status: String,
    command: String,
    args: String,
    stdout: String,
    stderr: String,
    start_time: String,
    end_time: String,
}

#[derive(Debug)]
struct ValidationExternalConfig {
    name: String,
    description: String,
    version: String,
    timestamp: String,
}
use super::filters;
#[derive(Template)]
#[template(path = "validation_result_details.html")]
struct ValidationResultDetailDisplay {
    test_id: String,
    validation_id: MachineValidationId,
    validation_results: Vec<ValidationResultDetail>,
}

#[derive(Template)]
#[template(path = "validation_results.html")]
struct ValidationResults {
    validation_id: MachineValidationId,
    machine_id: Option<String>,
    validation_results: Vec<ValidationResult>,
    status: String,
    progress: String,
    context: String,
    current_tests: Vec<CurrentValidationTest>,
    attempt_logs: Vec<ValidationAttemptLog>,
}

struct ValidationAttemptLog {
    test_id: String,
    attempt_id: String,
    attempt: Option<i32>,
}

struct CurrentValidationTest {
    name: String,
    test_id: String,
    attempt_id: Option<String>,
    attempt: Option<i32>,
}

#[derive(Template)]
#[template(path = "validation_tests.html")]
struct ValidateTests {
    validation_tests: Vec<ValidateTest>,
    page: PageContext,
}
#[derive(Template)]
#[template(path = "validation_test_details.html")]
struct ValidateTestDetailsDisplay {
    validation_tests: Vec<ValidateTestDetails>,
}

#[derive(Template)]
#[template(path = "validation.html")]
struct ValidationRunDisplay {
    validation_runs: Vec<ValidationRun>,
    page: PageContext,
}
#[derive(Template)]
#[template(path = "validation_external_config.html")]
struct ValidationExternalConfigs {
    validation_configs: Vec<ValidationExternalConfig>,
    page: PageContext,
}

impl From<forgerpc::MachineValidationTest> for ValidateTest {
    fn from(test: forgerpc::MachineValidationTest) -> Self {
        ValidateTest {
            id: test.test_id,
            version: test.version,
            name: test.name,
            description: test.description.unwrap_or_default(),
            contexts: test.contexts.join(", "),
            supported_platforms: test.supported_platforms.join(", "),
            command: test.command,
            args: test.args,
            tags: test.custom_tags.join(", "),
            is_verified: test.verified,
            is_enabled: test.is_enabled,
        }
    }
}

impl From<forgerpc::MachineValidationTest> for ValidateTestDetails {
    fn from(test: forgerpc::MachineValidationTest) -> Self {
        ValidateTestDetails {
            test_id: test.test_id,
            version: test.version,
            name: test.name,
            description: test.description.unwrap_or_default(),
            contexts: test.contexts.join(", "),
            supported_platforms: test.supported_platforms.join(", "),
            command: test.command,
            args: test.args,
            tags: test.custom_tags.join(", "),
            is_verified: test.verified,
            is_enabled: test.is_enabled,
            timeout: test.timeout.unwrap_or_default().to_string(),
            extra_output_file: test.extra_output_file.unwrap_or_default(),
            extra_err_file: test.extra_err_file.unwrap_or_default(),
            pre_condition: test.pre_condition.unwrap_or_default(),
            img_name: test.img_name.unwrap_or_default(),
            container_arg: test.container_arg.unwrap_or_default(),
            external_config_file: test.external_config_file.unwrap_or_default(),
            components: test.components.join(", "),
        }
    }
}

impl From<forgerpc::MachineValidationExternalConfig> for ValidationExternalConfig {
    fn from(test: forgerpc::MachineValidationExternalConfig) -> Self {
        ValidationExternalConfig {
            name: test.name,
            description: test.description.unwrap_or_default(),
            version: test.version,
            timestamp: test.timestamp.unwrap_or_default().to_string(),
        }
    }
}
pub(super) async fn results(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(validation_id): AxumPath<String>,
) -> Response {
    let validation_id: MachineValidationId = match validation_id.parse() {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid validation_id {validation_id}: {e}"),
            )
                .into_response();
        }
    };
    let request = tonic::Request::new(forgerpc::MachineValidationGetRequest {
        validation_id: Some(validation_id),
        include_history: false,
        machine_id: None,
    });
    tracing::info!(machine_validation_id = %validation_id, "results");

    let mut validation_results: Vec<ValidationResult> = match state
        .get_machine_validation_results(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(results) => results
            .results
            .into_iter()
            .map(|r: forgerpc::MachineValidationResult| {
                let start = r
                    .start_time
                    .and_then(|time| chrono::DateTime::<chrono::Utc>::try_from(time).ok());
                let end = r
                    .end_time
                    .and_then(|time| chrono::DateTime::<chrono::Utc>::try_from(time).ok());
                let elapsed = start
                    .zip(end)
                    .filter(|(start, end)| end >= start)
                    .map(|(start, end)| end - start);
                let duration = elapsed
                    .and_then(|elapsed| elapsed.to_std().ok())
                    .map(carbide_utils::duration::format_duration_compact)
                    .unwrap_or_else(|| "N/A".to_string());
                ValidationResult {
                    validation_id: r.validation_id.unwrap_or_default().to_string(),
                    name: r.name,
                    test_id: r.test_id.unwrap_or_default(),
                    attempt_id: None,
                    status: r.exit_code.to_string(),
                    start_time: r
                        .start_time
                        .map(|time| time.to_string())
                        .unwrap_or_default(),
                    start_time_sort: start
                        .map(|time| time.timestamp_micros())
                        .unwrap_or_default(),
                    start_time_short: start
                        .map(|time| time.format("%H:%M:%S").to_string())
                        .unwrap_or_else(|| "N/A".to_string()),
                    start_date: start
                        .map(|time| time.format("%Y-%m-%d").to_string())
                        .unwrap_or_default(),
                    end_time: r.end_time.map(|time| time.to_string()).unwrap_or_default(),
                    duration,
                    duration_ms: elapsed.map(|elapsed| elapsed.num_milliseconds()),
                }
            })
            .collect(),
        Err(err) => {
            tracing::error!(error = %err, machine_validation_id = %validation_id, "get_validation_results failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to get validation results",
            )
                .into_response();
        }
    };
    let mut db_reader = state.db_reader();
    let run = match db::machine_validation::find_by(
        &mut db_reader,
        db::ObjectColumnFilter::One(db::machine_validation::IdColumn, &validation_id),
    )
    .await
    {
        Ok(runs) => runs
            .into_iter()
            .next()
            .map(forgerpc::MachineValidationRun::from),
        Err(err) => {
            tracing::error!(error = %err, %validation_id, "Failed to load validation run");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load validation run",
            )
                .into_response();
        }
    };
    let status = run
        .as_ref()
        .and_then(|run| run.status.as_ref())
        .and_then(|status| status.machine_validation_state.as_ref())
        .map(|state| format!("{state:?}"))
        .unwrap_or_else(|| "Unknown".to_string());
    let progress = run
        .as_ref()
        .and_then(|run| run.status.as_ref())
        .map(|status| format!("{}/{}", status.completed_tests, status.total))
        .unwrap_or_default();
    let machine_id = run
        .as_ref()
        .and_then(|run| run.machine_id.as_ref())
        .map(|id| id.to_string());
    let context = run
        .as_ref()
        .and_then(|run| run.context.clone())
        .unwrap_or_else(|| "N/A".to_string());
    let attempts = match db::machine_validation_execution::find_attempts_by_run_id(
        &mut db_reader,
        &validation_id,
    )
    .await
    {
        Ok(attempts) => attempts,
        Err(err) => {
            tracing::error!(error = %err, %validation_id, "Failed to load validation attempts");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load validation attempts",
            )
                .into_response();
        }
    };
    // Planned items still have a zero counter while their first attempt is numbered 1.
    let attempt_numbers: HashMap<_, _> = attempts
        .iter()
        .map(|attempt| (attempt.attempt_id.to_string(), attempt.attempt_number))
        .collect();
    // Result ingestion records attempt 1. Match its terminal report rather than
    // assuming the latest attempt produced the result (which can predate a retry).
    let result_attempts: HashMap<_, _> = attempts
        .into_iter()
        .filter_map(|attempt| {
            if attempt.attempt_number != 1 || !attempt.terminal {
                return None;
            }
            Some((
                (
                    attempt.test_id,
                    rpc::Timestamp::from(attempt.started_at?).to_string(),
                    rpc::Timestamp::from(attempt.ended_at?).to_string(),
                    attempt.exit_code?.to_string(),
                ),
                attempt.attempt_id.to_string(),
            ))
        })
        .collect();
    for result in &mut validation_results {
        result.attempt_id = result_attempts
            .get(&(
                result.test_id.clone(),
                result.start_time.clone(),
                result.end_time.clone(),
                result.status.clone(),
            ))
            .cloned();
    }
    let mut current_tests = Vec::new();
    let mut attempt_logs = Vec::new();
    if let Some(run) = run.as_ref() {
        let items = match db::machine_validation_execution::find_run_items_by_run_id(
            &mut db_reader,
            &validation_id,
        )
        .await
        {
            Ok(items) => items,
            Err(err) => {
                tracing::error!(error = %err, %validation_id, "Failed to load validation run items");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to load validation run items",
                )
                    .into_response();
            }
        };
        attempt_logs = items
            .iter()
            .filter_map(|item| {
                item.current_attempt_id.map(|id| {
                    let attempt_id = id.to_string();
                    ValidationAttemptLog {
                        test_id: item.test_id.clone(),
                        attempt: attempt_numbers.get(&attempt_id).copied(),
                        attempt_id,
                    }
                })
            })
            .collect();
        current_tests = items
            .into_iter()
            .filter(|item| {
                run.end_time.is_none()
                    && item.state
                        == model::machine_validation::MachineValidationRunItemState::Running
            })
            .map(|item| CurrentValidationTest {
                name: if item.display_name.is_empty() {
                    item.test_id.clone()
                } else {
                    item.display_name
                },
                test_id: item.test_id,
                attempt_id: item.current_attempt_id.map(|id| id.to_string()),
                attempt: item
                    .current_attempt_id
                    .and_then(|id| attempt_numbers.get(&id.to_string()).copied()),
            })
            .collect();
    }

    let mut log_attempt_ids: HashSet<_> = attempt_logs
        .iter()
        .map(|log| log.attempt_id.clone())
        .collect();
    for result in &validation_results {
        if let Some(id) = &result.attempt_id
            && log_attempt_ids.insert(id.clone())
        {
            attempt_logs.push(ValidationAttemptLog {
                test_id: result.test_id.clone(),
                attempt_id: id.clone(),
                attempt: Some(1),
            });
        }
    }

    let tmpl = ValidationResults {
        validation_id,
        machine_id,
        validation_results,
        status,
        progress,
        context,
        current_tests,
        attempt_logs,
    };

    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

#[derive(serde::Deserialize)]
pub(super) struct AttemptLogQuery {
    #[serde(default)]
    after_sequence: u32,
}

pub(super) async fn attempt_logs(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(attempt_id): AxumPath<String>,
    Query(query): Query<AttemptLogQuery>,
    auth_context: Option<Extension<AuthContext>>,
) -> Response {
    let Some(Extension(auth_context)) = auth_context else {
        return (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    };
    let mut request = tonic::Request::new(forgerpc::MachineValidationAttemptLogGetRequest {
        attempt_id: Some(rpc::common::Uuid { value: attempt_id }),
        after_sequence: query.after_sequence,
        limit: 100,
    });
    request.extensions_mut().insert(auth_context);
    match state.get_machine_validation_attempt_logs(request).await {
        Ok(response) => {
            let logs = response.into_inner();
            let chunks: Vec<_> = logs.chunks.into_iter().map(|chunk| serde_json::json!({
                "sequence": chunk.sequence,
                "stream": if chunk.stream == forgerpc::MachineValidationAttemptLogStream::Stderr as i32 { "stderr" } else { "stdout" },
                "content": chunk.content,
            })).collect();
            (
                [(http::header::CACHE_CONTROL, "no-store")],
                Json(serde_json::json!({ "chunks": chunks, "has_more": logs.has_more })),
            )
                .into_response()
        }
        Err(error) => {
            let status = match error.code() {
                tonic::Code::Unauthenticated => StatusCode::UNAUTHORIZED,
                tonic::Code::PermissionDenied => StatusCode::FORBIDDEN,
                tonic::Code::NotFound => StatusCode::NOT_FOUND,
                tonic::Code::InvalidArgument => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            tracing::warn!(%error, "Failed to load validation attempt logs");
            (status, "Failed to load validation attempt logs").into_response()
        }
    }
}

pub(super) async fn result_details(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath((validation_id, test_id)): AxumPath<(String, String)>,
) -> Response {
    let validation_id: MachineValidationId = match validation_id.parse() {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid validation_id {validation_id}: {e}"),
            )
                .into_response();
        }
    };
    let request = tonic::Request::new(forgerpc::MachineValidationGetRequest {
        validation_id: Some(validation_id),
        include_history: false,
        machine_id: None,
    });

    let validation_results = match state
        .get_machine_validation_results(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(results) => results
            .results
            .into_iter()
            .filter(|r| r.test_id.as_ref() == Some(&test_id))
            .map(
                |r: forgerpc::MachineValidationResult| ValidationResultDetail {
                    validation_id: r.validation_id.unwrap_or_default().to_string(),
                    name: r.name,
                    context: r.context,
                    status: r.exit_code.to_string(),
                    command: r.command,
                    args: r.args,
                    stdout: r.std_out,
                    stderr: r.std_err,
                    start_time: r.start_time.unwrap_or_default().to_string(),
                    end_time: r.end_time.unwrap_or_default().to_string(),
                },
            )
            .collect(),
        Err(err) => {
            tracing::error!(error = %err, machine_validation_id = %validation_id, "get_validation_results failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to get validation results",
            )
                .into_response();
        }
    };

    let tmpl = ValidationResultDetailDisplay {
        test_id,
        validation_id,
        validation_results,
    };

    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_tests_html(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<PaginationParams>,
) -> Response {
    let validate_tests = match fetch_validation_tests(state, None).await {
        Ok(tests) => tests,
        Err(err) => {
            tracing::error!(error = %err, "fetch_validation_tests");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading validation tests",
            )
                .into_response();
        }
    };

    let all_tests: Vec<ValidateTest> = validate_tests.into_iter().map(ValidateTest::from).collect();
    let (info, validation_tests) = pagination::paginate_vec(all_tests, &params);

    let tmpl = ValidateTests {
        validation_tests,
        page: PageContext::new(info, "/admin/machinevalidation/tests"),
    };

    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_tests_details_html(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(test_id): AxumPath<String>,
) -> Response {
    let validate_tests = match fetch_validation_tests(state, Some(test_id)).await {
        Ok(tests) => tests,
        Err(err) => {
            tracing::error!(error = %err, "fetch_validation_tests");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading validation tests",
            )
                .into_response();
        }
    };

    let tmpl = ValidateTestDetailsDisplay {
        validation_tests: validate_tests
            .into_iter()
            .map(ValidateTestDetails::from)
            .collect(),
    };

    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}
async fn fetch_validation_tests(
    api: Arc<Api>,
    test_id: Option<String>,
) -> Result<Vec<forgerpc::MachineValidationTest>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::MachineValidationTestsGetRequest {
        supported_platforms: Vec::new(),
        contexts: Vec::new(),
        test_id,
        verified: Some(true),
        ..forgerpc::MachineValidationTestsGetRequest::default()
    });
    api.get_machine_validation_tests(request)
        .await
        .map(|response| response.into_inner().tests)
}

pub(super) async fn runs(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<PaginationParams>,
) -> Response {
    let validation_request = tonic::Request::new(rpc::forge::MachineValidationRunListGetRequest {
        machine_id: None,
        include_history: false,
    });

    let validation_runs: Vec<ValidationRun> = match state
        .get_machine_validation_runs(validation_request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(results) => results
            .runs
            .into_iter()
            .rev()
            .map(ValidationRun::from)
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err,"get_machine_validation_runs failed");
            Vec::new()
        }
    };

    let (info, validation_runs) = pagination::paginate_vec(validation_runs, &params);

    let tmpl = ValidationRunDisplay {
        validation_runs,
        page: PageContext::new(info, "/admin/machinevalidation/runs"),
    };

    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn external_configs(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<PaginationParams>,
) -> Response {
    let request = tonic::Request::new(rpc::forge::GetMachineValidationExternalConfigsRequest {
        names: Vec::new(),
    });

    let validation_configs: Vec<ValidationExternalConfig> = match state
        .get_machine_validation_external_configs(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(configs) => configs
            .configs
            .into_iter()
            .map(|c| ValidationExternalConfig {
                name: c.name,
                description: c.description.unwrap_or_default(),
                version: c.version,
                timestamp: c.timestamp.unwrap_or_default().to_string(),
            })
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err,"get_machine_validation_external_configs failed");
            Vec::new()
        }
    };

    let (info, validation_configs) = pagination::paginate_vec(validation_configs, &params);

    let tmpl = ValidationExternalConfigs {
        validation_configs,
        page: PageContext::new(info, "/admin/machinevalidation/external-configs"),
    };

    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

impl super::Base for ValidationResultDetailDisplay {}
impl super::Base for ValidationResults {}
impl super::Base for ValidateTests {}
impl super::Base for ValidateTestDetailsDisplay {}
impl super::Base for ValidationRunDisplay {}
impl super::Base for ValidationExternalConfigs {}
