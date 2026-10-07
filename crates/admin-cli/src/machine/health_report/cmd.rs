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

use std::str::FromStr;

use ::rpc::admin_cli::OutputFormat;
use chrono::Utc;
use health_report::{
    HealthAlertClassification, HealthProbeAlert, HealthProbeId, HealthProbeSuccess, HealthReport,
};

use super::args::{Args, HealthReportTemplates};
use crate::errors::CarbideCliResult;
use crate::health_utils;
use crate::rpc::ApiClient;

pub(crate) fn get_empty_template() -> HealthReport {
    HealthReport {
        source: "".to_string(),
        triggered_by: None,
        observed_at: Some(Utc::now()),
        successes: vec![HealthProbeSuccess {
            id: HealthProbeId::from_str("test").expect("should be valid HealthProbeId"),
            target: Some("".to_string()),
        }],
        alerts: vec![HealthProbeAlert {
            id: HealthProbeId::from_str("test").expect("should be valid HealthProbeId"),
            target: None,
            in_alert_since: None,
            message: "".to_string(),
            tenant_message: None,
            classifications: vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::prevent_host_state_changes(),
                HealthAlertClassification::suppress_external_alerting(),
            ],
        }],
    }
}

pub(crate) fn get_health_report(
    template: HealthReportTemplates,
    message: Option<String>,
) -> HealthReport {
    let mut report = HealthReport {
        source: "admin-cli".to_string(),
        triggered_by: None,
        observed_at: Some(Utc::now()),
        successes: vec![],
        alerts: vec![HealthProbeAlert {
            id: HealthProbeId::from_str("Maintenance").expect("should be valid HealthProbeId"),
            target: None,
            in_alert_since: None,
            message: message.unwrap_or_default(),
            tenant_message: None,
            classifications: vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::suppress_external_alerting(),
            ],
        }],
    };

    match template {
        HealthReportTemplates::HostUpdate => {
            report.source = "host-update".to_string();
            report.alerts[0].id = HealthProbeId::from_str("HostUpdateInProgress")
                .expect("should be valid HealthProbeId");
            report.alerts[0].target = Some("admin-cli".to_string());
        }
        HealthReportTemplates::InternalMaintenance => {
            report.source = "maintenance".to_string();
            report.alerts[0]
                .classifications
                .push(HealthAlertClassification::exclude_from_state_machine_sla());
        }
        HealthReportTemplates::StopRebootForAutomaticRecoveryFromStateMachine => {
            report.source = "manual-maintenance".to_string();
            report.alerts[0].target = Some("admin-cli".to_string());
            report.alerts[0].classifications = vec![
                HealthAlertClassification::stop_reboot_for_automatic_recovery_from_state_machine(),
            ];
        }
        HealthReportTemplates::OutForRepair => {
            report.source = "manual-maintenance".to_string();
            report.alerts[0].target = Some("OutForRepair".to_string());
            report.alerts[0]
                .classifications
                .push(HealthAlertClassification::exclude_from_state_machine_sla());
        }
        HealthReportTemplates::Degraded => {
            report.source = "manual-maintenance".to_string();
            report.alerts[0].target = Some("Degraded".to_string());
        }
        HealthReportTemplates::Validation => {
            report.source = "manual-maintenance".to_string();
            report.alerts[0].target = Some("Validation".to_string());
            report.alerts[0].classifications =
                vec![HealthAlertClassification::suppress_external_alerting()];
        }
        HealthReportTemplates::SuppressExternalAlerting => {
            report.source = "suppress-paging".to_string();
            report.alerts[0].target = Some("SuppressExternalAlerting".to_string());
            report.alerts[0].classifications =
                vec![HealthAlertClassification::suppress_external_alerting()];
        }
        HealthReportTemplates::MarkHealthy => {
            report.source = "admin-cli".to_string();
            report.alerts.clear();
        }
        // Template to indicate that the instance is identified as unhealthy by the tenant and
        // should be fixed before returning to the tenant.
        HealthReportTemplates::TenantReportedIssue => {
            report.source = "tenant-reported-issue".to_string();
            report.alerts[0].id = HealthProbeId::from_str("TenantReportedIssue")
                .expect("TenantReportedIssue is a valid non-empty HealthProbeId");
            report.alerts[0].target = Some("tenant-reported".to_string());
            report.alerts[0].classifications = vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::suppress_external_alerting(),
            ];
        }

        // Template to indicate that the instance is identified as unhealthy and
        // is ready to be picked for OnlineRepair without releasing the instance.
        // Adds `PreventInstanceDeletion` so carbide-api refuses `ReleaseInstance` until this merge is cleared
        // (admin machine force-delete is unchanged). Merge source `request-online-repair` is separate
        // from `tenant-reported-issue`.
        HealthReportTemplates::RequestOnlineRepair => {
            report.source = health_report::REQUEST_ONLINE_REPAIR_MERGE_SOURCE.to_string();
            report.alerts[0].id = HealthProbeId::from_str("RequestOnlineRepair")
                .expect("RequestOnlineRepair is a valid non-empty HealthProbeId");
            report.alerts[0].target =
                Some(health_report::REQUEST_ONLINE_REPAIR_MERGE_SOURCE.to_string());
            report.alerts[0].classifications = vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::suppress_external_alerting(),
                HealthAlertClassification::prevent_instance_deletion(),
            ];
        }

        // Template to indicate that the instance is identified as unhealthy and
        // is ready to be picked by Repair System for diagnosis and fix.
        HealthReportTemplates::RequestRepair => {
            report.source = health_report::REPAIR_REQUEST_MERGE_SOURCE.to_string();
            report.alerts[0].id = HealthProbeId::from_str("RequestRepair")
                .expect("RequestRepair is a valid non-empty HealthProbeId");
            report.alerts[0].target = Some("repair-requested".to_string());
            report.alerts[0].classifications = vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::suppress_external_alerting(),
            ];
        }
    }

    report
}

pub(super) async fn handle_health_report(
    command: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    match command {
        Args::Show { machine_id } => {
            let response = api_client.machine_list_health_reports(machine_id).await?;
            health_utils::display_health_reports(response.health_report_entries, output_format)?;
        }
        Args::Add(options) => {
            let report = health_utils::resolve_health_report(
                options.template,
                options.health_report,
                options.message,
            )?;

            if options.print_only {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }

            api_client
                .machine_insert_health_report_override(
                    &options.machine_id,
                    report.into(),
                    options.replace,
                )
                .await?;
        }
        Args::Remove {
            machine_id,
            report_source,
        } => {
            api_client
                .machine_remove_health_report(machine_id, report_source)
                .await?;
        }
        Args::PrintEmptyTemplate => {
            health_utils::print_empty_template();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use carbide_test_support::{Check, check_values};

    use super::*;

    struct TemplateInput {
        template: HealthReportTemplates,
        message: Option<&'static str>,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ReportProjection {
        source: String,
        triggered_by: Option<String>,
        observed_at_present: bool,
        successes: Vec<SuccessProjection>,
        alerts: Vec<AlertProjection>,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct SuccessProjection {
        id: String,
        target: Option<String>,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct AlertProjection {
        id: String,
        target: Option<String>,
        in_alert_since_present: bool,
        message: String,
        tenant_message: Option<String>,
        classifications: Vec<String>,
    }

    struct ExpectedReport<'a> {
        source: &'a str,
        alert_id: &'a str,
        target: Option<&'a str>,
        message: &'a str,
        classifications: &'a [&'a str],
    }

    fn project_report(report: HealthReport) -> ReportProjection {
        let HealthReport {
            source,
            triggered_by,
            observed_at,
            successes,
            alerts,
        } = report;

        ReportProjection {
            source,
            triggered_by,
            observed_at_present: observed_at.is_some(),
            successes: successes
                .into_iter()
                .map(|success| {
                    let HealthProbeSuccess { id, target } = success;
                    SuccessProjection {
                        id: id.as_str().to_string(),
                        target,
                    }
                })
                .collect(),
            alerts: alerts
                .into_iter()
                .map(|alert| {
                    let HealthProbeAlert {
                        id,
                        target,
                        in_alert_since,
                        message,
                        tenant_message,
                        classifications,
                    } = alert;
                    AlertProjection {
                        id: id.as_str().to_string(),
                        target,
                        in_alert_since_present: in_alert_since.is_some(),
                        message,
                        tenant_message,
                        classifications: classifications
                            .into_iter()
                            .map(|classification| classification.as_str().to_string())
                            .collect(),
                    }
                })
                .collect(),
        }
    }

    fn expected_report(expected: ExpectedReport<'_>) -> ReportProjection {
        ReportProjection {
            source: expected.source.to_string(),
            triggered_by: None,
            observed_at_present: true,
            successes: vec![],
            alerts: vec![AlertProjection {
                id: expected.alert_id.to_string(),
                target: expected.target.map(str::to_string),
                in_alert_since_present: false,
                message: expected.message.to_string(),
                tenant_message: None,
                classifications: expected
                    .classifications
                    .iter()
                    .map(|classification| (*classification).to_string())
                    .collect(),
            }],
        }
    }

    fn expected_report_without_alerts(source: &str) -> ReportProjection {
        ReportProjection {
            source: source.to_string(),
            triggered_by: None,
            observed_at_present: true,
            successes: vec![],
            alerts: vec![],
        }
    }

    #[test]
    fn health_report_templates() {
        let message = "test message";

        let checks = [
            Check {
                scenario: "host update",
                input: TemplateInput {
                    template: HealthReportTemplates::HostUpdate,
                    message: Some(message),
                },
                expect: expected_report(ExpectedReport {
                    source: "host-update",
                    alert_id: "HostUpdateInProgress",
                    target: Some("admin-cli"),
                    message,
                    classifications: &["PreventAllocations", "SuppressExternalAlerting"],
                }),
            },
            Check {
                scenario: "internal maintenance",
                input: TemplateInput {
                    template: HealthReportTemplates::InternalMaintenance,
                    message: None,
                },
                expect: expected_report(ExpectedReport {
                    source: "maintenance",
                    alert_id: "Maintenance",
                    target: None,
                    message: "",
                    classifications: &[
                        "PreventAllocations",
                        "SuppressExternalAlerting",
                        "ExcludeFromStateMachineSla",
                    ],
                }),
            },
            Check {
                scenario: "out for repair",
                input: TemplateInput {
                    template: HealthReportTemplates::OutForRepair,
                    message: Some(message),
                },
                expect: expected_report(ExpectedReport {
                    source: "manual-maintenance",
                    alert_id: "Maintenance",
                    target: Some("OutForRepair"),
                    message,
                    classifications: &[
                        "PreventAllocations",
                        "SuppressExternalAlerting",
                        "ExcludeFromStateMachineSla",
                    ],
                }),
            },
            Check {
                scenario: "degraded",
                input: TemplateInput {
                    template: HealthReportTemplates::Degraded,
                    message: Some(message),
                },
                expect: expected_report(ExpectedReport {
                    source: "manual-maintenance",
                    alert_id: "Maintenance",
                    target: Some("Degraded"),
                    message,
                    classifications: &["PreventAllocations", "SuppressExternalAlerting"],
                }),
            },
            Check {
                scenario: "validation",
                input: TemplateInput {
                    template: HealthReportTemplates::Validation,
                    message: Some(message),
                },
                expect: expected_report(ExpectedReport {
                    source: "manual-maintenance",
                    alert_id: "Maintenance",
                    target: Some("Validation"),
                    message,
                    classifications: &["SuppressExternalAlerting"],
                }),
            },
            Check {
                scenario: "suppress external alerting",
                input: TemplateInput {
                    template: HealthReportTemplates::SuppressExternalAlerting,
                    message: Some(message),
                },
                expect: expected_report(ExpectedReport {
                    source: "suppress-paging",
                    alert_id: "Maintenance",
                    target: Some("SuppressExternalAlerting"),
                    message,
                    classifications: &["SuppressExternalAlerting"],
                }),
            },
            Check {
                scenario: "mark healthy",
                input: TemplateInput {
                    template: HealthReportTemplates::MarkHealthy,
                    message: Some(message),
                },
                expect: expected_report_without_alerts("admin-cli"),
            },
            Check {
                scenario: "stop automatic recovery reboot",
                input: TemplateInput {
                    template: HealthReportTemplates::StopRebootForAutomaticRecoveryFromStateMachine,
                    message: Some(message),
                },
                expect: expected_report(ExpectedReport {
                    source: "manual-maintenance",
                    alert_id: "Maintenance",
                    target: Some("admin-cli"),
                    message,
                    classifications: &["StopRebootForAutomaticRecoveryFromStateMachine"],
                }),
            },
            Check {
                scenario: "tenant reported issue",
                input: TemplateInput {
                    template: HealthReportTemplates::TenantReportedIssue,
                    message: Some(message),
                },
                expect: expected_report(ExpectedReport {
                    source: "tenant-reported-issue",
                    alert_id: "TenantReportedIssue",
                    target: Some("tenant-reported"),
                    message,
                    classifications: &["PreventAllocations", "SuppressExternalAlerting"],
                }),
            },
            Check {
                scenario: "online repair request",
                input: TemplateInput {
                    template: HealthReportTemplates::RequestOnlineRepair,
                    message: Some(message),
                },
                expect: expected_report(ExpectedReport {
                    source: "request-online-repair",
                    alert_id: "RequestOnlineRepair",
                    target: Some("request-online-repair"),
                    message,
                    classifications: &[
                        "PreventAllocations",
                        "SuppressExternalAlerting",
                        "PreventInstanceDeletion",
                    ],
                }),
            },
            Check {
                scenario: "repair request",
                input: TemplateInput {
                    template: HealthReportTemplates::RequestRepair,
                    message: Some(message),
                },
                expect: expected_report(ExpectedReport {
                    source: "repair-request",
                    alert_id: "RequestRepair",
                    target: Some("repair-requested"),
                    message,
                    classifications: &["PreventAllocations", "SuppressExternalAlerting"],
                }),
            },
        ];

        let covered_templates = checks
            .iter()
            .map(|check| format!("{:?}", check.input.template))
            .collect::<HashSet<_>>();
        let defined_templates = <HealthReportTemplates as clap::ValueEnum>::value_variants()
            .iter()
            .map(|template| format!("{template:?}"))
            .collect::<HashSet<_>>();
        assert_eq!(
            covered_templates, defined_templates,
            "every health-report template must have a table row"
        );

        check_values(checks, |TemplateInput { template, message }| {
            project_report(get_health_report(template, message.map(str::to_string)))
        });
    }

    #[test]
    fn empty_health_report_template() {
        assert_eq!(
            project_report(get_empty_template()),
            ReportProjection {
                source: String::new(),
                triggered_by: None,
                observed_at_present: true,
                successes: vec![SuccessProjection {
                    id: "test".to_string(),
                    target: Some(String::new()),
                }],
                alerts: vec![AlertProjection {
                    id: "test".to_string(),
                    target: None,
                    in_alert_since_present: false,
                    message: String::new(),
                    tenant_message: None,
                    classifications: vec![
                        "PreventAllocations".to_string(),
                        "PreventHostStateChanges".to_string(),
                        "SuppressExternalAlerting".to_string(),
                    ],
                }],
            }
        );
    }
}
