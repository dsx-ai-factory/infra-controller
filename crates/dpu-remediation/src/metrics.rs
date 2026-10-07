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

//! Correlated logs and bounded failure counters for the remediation executor.
//!
//! These Events share one counter keyed by the stage that stopped an executor
//! pass. Machine and remediation identifiers remain log context so they do not
//! create unbounded metric series.

use carbide_instrument::{Event, LabelValue, MetricFamily};
use carbide_uuid::dpu_remediations::RemediationId;
use carbide_uuid::machine::MachineId;

/// Where one executor pass stopped, as the bounded `failure_stage` label
/// shared by the Events below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum RemediationExecutorFailureStage {
    StatusDecode,
    Apply,
    ResponseValidation,
    Fetch,
    ClientCreation,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_dpu_remediation_executor_failures_total",
    kind = counter,
    component = "forge-dpu-agent",
    describe = "Number of DPU remediation executor failures, by failure stage."
)]
pub(crate) struct DpuRemediationExecutorFailures {
    failure_stage: RemediationExecutorFailureStage,
}

#[derive(Event)]
#[event(
    event_name = "dpu_remediation_status_output_decode_failed",
    metric_family = DpuRemediationExecutorFailures,
    log = error,
    message = "Unable to deserialize json into hashmap from status output file"
)]
pub(crate) struct RemediationStatusOutputDecodeFailed {
    #[label]
    failure_stage: RemediationExecutorFailureStage,
    #[context]
    dpu_machine_id: MachineId,
    #[context]
    remediation_id: RemediationId,
    #[context]
    error: String,
}

impl RemediationStatusOutputDecodeFailed {
    pub(crate) fn new(
        dpu_machine_id: MachineId,
        remediation_id: RemediationId,
        error: String,
    ) -> Self {
        Self {
            failure_stage: RemediationExecutorFailureStage::StatusDecode,
            dpu_machine_id,
            remediation_id,
            error,
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "dpu_remediation_apply_failed",
    metric_family = DpuRemediationExecutorFailures,
    log = error,
    message = "Remediation failed"
)]
pub(crate) struct RemediationApplyFailed {
    #[label]
    failure_stage: RemediationExecutorFailureStage,
    #[context]
    dpu_machine_id: MachineId,
    #[context]
    remediation_id: RemediationId,
    #[context]
    error: String,
}

impl RemediationApplyFailed {
    pub(crate) fn new(
        dpu_machine_id: MachineId,
        remediation_id: RemediationId,
        error: String,
    ) -> Self {
        Self {
            failure_stage: RemediationExecutorFailureStage::Apply,
            dpu_machine_id,
            remediation_id,
            error,
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "dpu_remediation_response_invalid",
    metric_family = DpuRemediationExecutorFailures,
    log = error,
    message = "received a response with one of id or script but not both, skipping, will retry next loop"
)]
pub(crate) struct RemediationResponseInvalid {
    #[label]
    failure_stage: RemediationExecutorFailureStage,
    #[context]
    dpu_machine_id: MachineId,
    #[context(value)]
    has_script: bool,
    #[context(value)]
    has_remediation_id: bool,
}

impl RemediationResponseInvalid {
    pub(crate) fn new(
        dpu_machine_id: MachineId,
        has_script: bool,
        has_remediation_id: bool,
    ) -> Self {
        Self {
            failure_stage: RemediationExecutorFailureStage::ResponseValidation,
            dpu_machine_id,
            has_script,
            has_remediation_id,
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "dpu_remediation_fetch_failed",
    metric_family = DpuRemediationExecutorFailures,
    log = error,
    message = "Remediation executor unable to fetch next remediation this loop, will retry next loop"
)]
pub(crate) struct RemediationFetchFailed {
    #[label]
    failure_stage: RemediationExecutorFailureStage,
    #[context]
    dpu_machine_id: MachineId,
    #[context]
    error: String,
}

impl RemediationFetchFailed {
    pub(crate) fn new(dpu_machine_id: MachineId, error: String) -> Self {
        Self {
            failure_stage: RemediationExecutorFailureStage::Fetch,
            dpu_machine_id,
            error,
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "dpu_remediation_client_creation_failed",
    metric_family = DpuRemediationExecutorFailures,
    log = error,
    message = "Remediation executor unable to create forge client this loop, will retry next loop"
)]
pub(crate) struct RemediationClientCreationFailed {
    #[label]
    failure_stage: RemediationExecutorFailureStage,
    #[context]
    dpu_machine_id: MachineId,
    #[context]
    error: String,
}

impl RemediationClientCreationFailed {
    pub(crate) fn new(dpu_machine_id: MachineId, error: String) -> Self {
        Self {
            failure_stage: RemediationExecutorFailureStage::ClientCreation,
            dpu_machine_id,
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use carbide_instrument::emit;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;

    use super::*;

    const FAILURE_METRIC: &str = "carbide_dpu_remediation_executor_failures_total";
    const DPU_MACHINE_ID: &str = "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30";
    const REMEDIATION_ID: &str = "00000000-0000-0000-0000-000000000000";

    enum FailureCase {
        StatusDecode,
        Apply,
        ResponseValidation,
        Fetch,
        ClientCreation,
    }

    #[derive(Debug, PartialEq)]
    struct FailureObservation {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        failure_stage: Option<String>,
        dpu_machine_id: Option<String>,
        remediation_id: Option<String>,
        error: Option<String>,
        has_script: Option<String>,
        has_remediation_id: Option<String>,
        counter_delta: f64,
    }

    #[test]
    fn executor_failures_log_and_count_by_stage() {
        value_scenarios!(
            run = |case| {
                let dpu_machine_id = MachineId::from_str(DPU_MACHINE_ID).unwrap();
                let remediation_id = RemediationId::nil();
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| match case {
                    FailureCase::StatusDecode => {
                        emit(RemediationStatusOutputDecodeFailed::new(
                            dpu_machine_id,
                            remediation_id,
                            "expected value at line 1 column 1".to_string(),
                        ));
                    }
                    FailureCase::Apply => {
                        emit(RemediationApplyFailed::new(
                            dpu_machine_id,
                            remediation_id,
                            "transport error".to_string(),
                        ));
                    }
                    FailureCase::ResponseValidation => {
                        emit(RemediationResponseInvalid::new(dpu_machine_id, true, false));
                    }
                    FailureCase::Fetch => {
                        emit(RemediationFetchFailed::new(
                            dpu_machine_id,
                            "service unavailable".to_string(),
                        ));
                    }
                    FailureCase::ClientCreation => {
                        emit(RemediationClientCreationFailed::new(
                            dpu_machine_id,
                            "invalid endpoint".to_string(),
                        ));
                    }
                });
                assert_eq!(logs.len(), 1, "each failure should write one terminal record");
                let log = logs.first().expect("failure Event did not log");
                let failure_stage = log.field("failure_stage").map(str::to_string);

                FailureObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    failure_stage: failure_stage.clone(),
                    dpu_machine_id: log.field("dpu_machine_id").map(str::to_string),
                    remediation_id: log.field("remediation_id").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                    has_script: log.field("has_script").map(str::to_string),
                    has_remediation_id: log
                        .field("has_remediation_id")
                        .map(str::to_string),
                    counter_delta: metrics.counter_delta(
                        FAILURE_METRIC,
                        &[("failure_stage", failure_stage.as_deref().unwrap())],
                    ),
                }
            };
            "status output cannot be decoded" {
                FailureCase::StatusDecode => FailureObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "dpu_remediation_status_output_decode_failed".to_string(),
                    message: "Unable to deserialize json into hashmap from status output file".to_string(),
                    event_name: Some("dpu_remediation_status_output_decode_failed".to_string()),
                    metric_name: Some(FAILURE_METRIC.to_string()),
                    failure_stage: Some("status_decode".to_string()),
                    dpu_machine_id: Some(DPU_MACHINE_ID.to_string()),
                    remediation_id: Some(REMEDIATION_ID.to_string()),
                    error: Some("expected value at line 1 column 1".to_string()),
                    has_script: None,
                    has_remediation_id: None,
                    counter_delta: 1.0,
                },
            }
            "local apply or report path fails" {
                FailureCase::Apply => FailureObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "dpu_remediation_apply_failed".to_string(),
                    message: "Remediation failed".to_string(),
                    event_name: Some("dpu_remediation_apply_failed".to_string()),
                    metric_name: Some(FAILURE_METRIC.to_string()),
                    failure_stage: Some("apply".to_string()),
                    dpu_machine_id: Some(DPU_MACHINE_ID.to_string()),
                    remediation_id: Some(REMEDIATION_ID.to_string()),
                    error: Some("transport error".to_string()),
                    has_script: None,
                    has_remediation_id: None,
                    counter_delta: 1.0,
                },
            }
            "Forge response has only one required field" {
                FailureCase::ResponseValidation => FailureObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "dpu_remediation_response_invalid".to_string(),
                    message: "received a response with one of id or script but not both, skipping, will retry next loop".to_string(),
                    event_name: Some("dpu_remediation_response_invalid".to_string()),
                    metric_name: Some(FAILURE_METRIC.to_string()),
                    failure_stage: Some("response_validation".to_string()),
                    dpu_machine_id: Some(DPU_MACHINE_ID.to_string()),
                    remediation_id: None,
                    error: None,
                    has_script: Some("true".to_string()),
                    has_remediation_id: Some("false".to_string()),
                    counter_delta: 1.0,
                },
            }
            "next remediation fetch fails" {
                FailureCase::Fetch => FailureObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "dpu_remediation_fetch_failed".to_string(),
                    message: "Remediation executor unable to fetch next remediation this loop, will retry next loop".to_string(),
                    event_name: Some("dpu_remediation_fetch_failed".to_string()),
                    metric_name: Some(FAILURE_METRIC.to_string()),
                    failure_stage: Some("fetch".to_string()),
                    dpu_machine_id: Some(DPU_MACHINE_ID.to_string()),
                    remediation_id: None,
                    error: Some("service unavailable".to_string()),
                    has_script: None,
                    has_remediation_id: None,
                    counter_delta: 1.0,
                },
            }
            "Forge client cannot be created" {
                FailureCase::ClientCreation => FailureObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "dpu_remediation_client_creation_failed".to_string(),
                    message: "Remediation executor unable to create forge client this loop, will retry next loop".to_string(),
                    event_name: Some("dpu_remediation_client_creation_failed".to_string()),
                    metric_name: Some(FAILURE_METRIC.to_string()),
                    failure_stage: Some("client_creation".to_string()),
                    dpu_machine_id: Some(DPU_MACHINE_ID.to_string()),
                    remediation_id: None,
                    error: Some("invalid endpoint".to_string()),
                    has_script: None,
                    has_remediation_id: None,
                    counter_delta: 1.0,
                },
            }
        );
    }
}
