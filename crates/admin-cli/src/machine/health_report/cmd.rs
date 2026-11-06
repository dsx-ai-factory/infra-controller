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

pub fn get_empty_template() -> HealthReport {
    HealthReport {
        source: "".to_string(),
        triggered_by: None,
        observed_at: Some(Utc::now()),
        successes: vec![HealthProbeSuccess {
            id: HealthProbeId::from_str("test").unwrap(),
            target: Some("".to_string()),
        }],
        alerts: vec![HealthProbeAlert {
            id: HealthProbeId::from_str("test").unwrap(),
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

pub fn get_health_report(template: HealthReportTemplates, message: Option<String>) -> HealthReport {
    let mut report = HealthReport {
        source: "admin-cli".to_string(),
        triggered_by: None,
        observed_at: Some(Utc::now()),
        successes: vec![],
        alerts: vec![HealthProbeAlert {
            id: HealthProbeId::from_str("Maintenance").unwrap(),
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
            report.alerts[0].id = HealthProbeId::from_str("HostUpdateInProgress").unwrap();
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
        // Template applied before a compute tray is physically moved or replaced. Besides
        // holding the host out of allocation like `OutForRepair`, it tells the NVLink
        // partition monitor to remove the host's GPUs from every NVLink partition. It uses
        // its own source so that adding it does not replace an existing `manual-maintenance`
        // hold, such as `OutForRepair` or a stop-reboot guard.
        HealthReportTemplates::ComputeTrayDueForSwap => {
            report.source = "compute-tray-due-for-swap".to_string();
            report.alerts[0].target = Some("ComputeTrayDueForSwap".to_string());
            report.alerts[0].classifications.extend([
                HealthAlertClassification::exclude_from_state_machine_sla(),
                HealthAlertClassification::isolate_from_nvlink_partitions(),
            ]);
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

pub async fn handle_health_report(
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
                println!("{}", serde_json::to_string_pretty(&report).unwrap());
                return Ok(());
            }

            api_client
                .machine_insert_health_report_override(
                    options.machine_id,
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
    use std::str::FromStr;

    use health_report::{HealthAlertClassification, HealthProbeId};

    use super::*;

    #[test]
    fn test_tenant_reported_issue_template() {
        let report = get_health_report(
            HealthReportTemplates::TenantReportedIssue,
            Some("Customer reported network connectivity issues".to_string()),
        );

        assert_eq!(report.source, "tenant-reported-issue");
        assert_eq!(report.alerts.len(), 1);

        let alert = &report.alerts[0];
        assert_eq!(
            alert.id,
            HealthProbeId::from_str("TenantReportedIssue").unwrap()
        );
        assert_eq!(alert.target, Some("tenant-reported".to_string()));
        assert_eq!(
            alert.message,
            "Customer reported network connectivity issues"
        );
        assert!(alert.tenant_message.is_none());

        // Check classifications
        assert_eq!(alert.classifications.len(), 2);
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::prevent_allocations())
        );
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::suppress_external_alerting())
        );
    }

    #[test]
    fn test_request_repair_template() {
        let report = get_health_report(
            HealthReportTemplates::RequestRepair,
            Some("Hardware diagnostics indicate memory failure".to_string()),
        );

        assert_eq!(report.source, health_report::REPAIR_REQUEST_MERGE_SOURCE);
        assert_eq!(report.alerts.len(), 1);

        let alert = &report.alerts[0];
        assert_eq!(alert.id, HealthProbeId::from_str("RequestRepair").unwrap());
        assert_eq!(alert.target, Some("repair-requested".to_string()));
        assert_eq!(
            alert.message,
            "Hardware diagnostics indicate memory failure"
        );
        assert!(alert.tenant_message.is_none());

        // Check classifications
        assert_eq!(alert.classifications.len(), 2);
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::prevent_allocations())
        );
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::suppress_external_alerting())
        );
    }

    #[test]
    fn test_tenant_reported_issue_template_with_empty_message() {
        let report = get_health_report(HealthReportTemplates::TenantReportedIssue, None);

        assert_eq!(report.source, "tenant-reported-issue");
        assert_eq!(report.alerts[0].message, "");
    }

    #[test]
    fn test_request_repair_template_with_empty_message() {
        let report = get_health_report(HealthReportTemplates::RequestRepair, None);

        assert_eq!(report.source, health_report::REPAIR_REQUEST_MERGE_SOURCE);
        assert_eq!(report.alerts[0].message, "");
    }

    #[test]
    fn test_new_templates_have_suppress_external_alerting() {
        // Verify both new templates include SuppressExternalAlerting classification
        let tenant_report = get_health_report(
            HealthReportTemplates::TenantReportedIssue,
            Some("test".to_string()),
        );
        let repair_report = get_health_report(
            HealthReportTemplates::RequestRepair,
            Some("test".to_string()),
        );

        // Both should suppress external alerting
        assert!(
            tenant_report.alerts[0]
                .classifications
                .contains(&HealthAlertClassification::suppress_external_alerting())
        );
        assert!(
            repair_report.alerts[0]
                .classifications
                .contains(&HealthAlertClassification::suppress_external_alerting())
        );

        let request_online_repair = get_health_report(
            HealthReportTemplates::RequestOnlineRepair,
            Some("test".to_string()),
        );
        assert!(
            request_online_repair.alerts[0]
                .classifications
                .contains(&HealthAlertClassification::suppress_external_alerting())
        );
        assert!(
            request_online_repair.alerts[0]
                .classifications
                .contains(&HealthAlertClassification::prevent_instance_deletion())
        );
    }

    #[test]
    fn test_request_online_repair_template() {
        let report = get_health_report(
            HealthReportTemplates::RequestOnlineRepair,
            Some("Online repair handoff for stuck repair workflow".to_string()),
        );

        assert_eq!(
            report.source,
            health_report::REQUEST_ONLINE_REPAIR_MERGE_SOURCE
        );
        assert_eq!(report.alerts.len(), 1);

        let alert = &report.alerts[0];
        assert_eq!(
            alert.id,
            HealthProbeId::from_str("RequestOnlineRepair").unwrap()
        );
        assert_eq!(
            alert.target,
            Some(health_report::REQUEST_ONLINE_REPAIR_MERGE_SOURCE.to_string())
        );
        assert_eq!(
            alert.message,
            "Online repair handoff for stuck repair workflow"
        );
        assert!(alert.tenant_message.is_none());

        assert_eq!(alert.classifications.len(), 3);
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::prevent_allocations())
        );
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::suppress_external_alerting())
        );
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::prevent_instance_deletion())
        );
    }

    #[test]
    fn test_request_online_repair_template_with_empty_message() {
        let report = get_health_report(HealthReportTemplates::RequestOnlineRepair, None);

        assert_eq!(
            report.source,
            health_report::REQUEST_ONLINE_REPAIR_MERGE_SOURCE
        );
        assert_eq!(report.alerts[0].message, "");
    }

    #[test]
    fn test_internal_maintenance_template_excludes_from_state_machine_sla() {
        let report = get_health_report(HealthReportTemplates::InternalMaintenance, None);

        assert_eq!(report.source, "maintenance");
        let alert = &report.alerts[0];
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::exclude_from_state_machine_sla()),
            "InternalMaintenance should carry ExcludeFromStateMachineSla"
        );
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::prevent_allocations())
        );
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::suppress_external_alerting())
        );
    }

    #[test]
    fn test_out_for_repair_template_excludes_from_state_machine_sla() {
        let report = get_health_report(HealthReportTemplates::OutForRepair, None);

        assert_eq!(report.source, "manual-maintenance");
        let alert = &report.alerts[0];
        assert_eq!(alert.target, Some("OutForRepair".to_string()));
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::exclude_from_state_machine_sla()),
            "OutForRepair should carry ExcludeFromStateMachineSla"
        );
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::prevent_allocations())
        );
        assert!(
            alert
                .classifications
                .contains(&HealthAlertClassification::suppress_external_alerting())
        );
    }

    #[test]
    fn test_compute_tray_due_for_swap_template_isolates_from_nvlink_partitions() {
        let report = get_health_report(HealthReportTemplates::ComputeTrayDueForSwap, None);

        // A source of its own, so adding it never replaces a `manual-maintenance` hold.
        assert_eq!(report.source, "compute-tray-due-for-swap");
        let alert = &report.alerts[0];
        assert_eq!(alert.target, Some("ComputeTrayDueForSwap".to_string()));
        assert_eq!(
            alert.classifications,
            vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::suppress_external_alerting(),
                HealthAlertClassification::exclude_from_state_machine_sla(),
                HealthAlertClassification::isolate_from_nvlink_partitions(),
            ]
        );
    }
}
