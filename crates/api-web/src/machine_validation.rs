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
use axum::extract::{Path as AxumPath, Query, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
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
    context: String,
    status: String,
    start_time: String,
    end_time: String,
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
    validation_results: Vec<ValidationResult>,
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

    let validation_results = match state
        .get_machine_validation_results(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(results) => results
            .results
            .into_iter()
            .map(|r: forgerpc::MachineValidationResult| ValidationResult {
                validation_id: r.validation_id.unwrap_or_default().to_string(),
                name: r.name,
                test_id: r.test_id.unwrap_or_default(),
                context: r.context,
                status: r.exit_code.to_string(),
                start_time: r.start_time.unwrap_or_default().to_string(),
                end_time: r.end_time.unwrap_or_default().to_string(),
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
    // tracing::info!(%validation_results, "results_details");

    let tmpl = ValidationResults {
        validation_id,
        validation_results,
    };

    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
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
            .map(|vr| ValidationRun {
                machine_id: vr.machine_id.map(|id| id.to_string()).unwrap_or_default(),
                status:format!("{:?}", vr.status.unwrap_or_default().machine_validation_state.unwrap_or(
                    rpc::forge::machine_validation_status::MachineValidationState::Completed(
                        rpc::forge::machine_validation_status::MachineValidationCompleted::Success.into(),
                    ),
                )),
                context: vr.context.unwrap_or_default(),
                validation_id: vr.validation_id.unwrap_or_default().to_string(),
                start_time: vr.start_time.unwrap_or_default().to_string(),
                end_time: vr.end_time.unwrap_or_default().to_string(),
            })
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
