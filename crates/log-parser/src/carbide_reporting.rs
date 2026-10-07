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
use std::collections::VecDeque;
use std::path::Path;
use std::str::FromStr;

use ::rpc::forge::{self as rpc};
use ::rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig, ForgeClientT};
use carbide_uuid::machine::MachineId;
use forge_tls::client_config::ClientCert;
use health_report::{
    HealthAlertClassification, HealthProbeAlert, HealthProbeId, HealthProbeSuccess, HealthReport,
};

use crate::Event;

#[derive(thiserror::Error, Debug)]
pub(super) enum ReportingError {
    #[error("unable to connect to carbide API: {0}")]
    ApiConnectFailed(String),

    #[error("the API call to the forge API server returned {0}")]
    ApiInvocationError(tonic::Status),

    #[error("generic error: {0}")]
    GenericError(String),

    #[error("error while handling json: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("tokio task join error {0}")]
    TokioJoinError(#[from] tokio::task::JoinError),
}

pub(crate) fn get_client_cert_info(
    client_cert_path: Option<String>,
    client_key_path: Option<String>,
) -> ClientCert {
    if let (Some(client_key_path), Some(client_cert_path)) = (client_key_path, client_cert_path) {
        return ClientCert {
            cert_path: client_cert_path,
            key_path: client_key_path,
        };
    }
    // this is the location for most k8s pods
    if Path::new("/var/run/secrets/spiffe.io/tls.crt").exists()
        && Path::new("/var/run/secrets/spiffe.io/tls.key").exists()
    {
        return ClientCert {
            cert_path: "/var/run/secrets/spiffe.io/tls.crt".to_string(),
            key_path: "/var/run/secrets/spiffe.io/tls.key".to_string(),
        };
    }
    // if you make it here, you'll just have to tell me where the client cert is.
    panic!(
        r###"Unknown client cert location. Set (will be read in same sequence.)
           1. --client-cert-path and --client-key-path flag or
           2. a file existing at "/var/run/secrets/spiffe.io/tls.crt" and "/var/run/secrets/spiffe.io/tls.key"."###
    )
}

pub(crate) fn get_forge_root_ca_path(forge_root_ca_path: Option<String>) -> String {
    // First from command line, second env var.
    if let Some(forge_root_ca_path) = forge_root_ca_path {
        return forge_root_ca_path;
    }
    // this is the location for most k8s pods
    if Path::new("/var/run/secrets/spiffe.io/ca.crt").exists() {
        return "/var/run/secrets/spiffe.io/ca.crt".to_string();
    }
    // if you make it here, you'll just have to tell me where the root CA is.
    panic!(
        r###"Unknown FORGE_ROOT_CA_PATH. Set (will be read in same sequence.)
           1. --forge-root-ca-path flag or
           2. a file existing at "/var/run/secrets/spiffe.io/ca.crt"."###
    )
}

pub(crate) async fn create_forge_client(
    root_ca: String,
    client_cert: String,
    client_key: String,
    api_url: String,
) -> Result<ForgeClientT, ReportingError> {
    let client_config = ForgeClientConfig::new(
        root_ca,
        Some(ClientCert {
            cert_path: client_cert,
            key_path: client_key,
        }),
    );

    let api_config = ApiConfig::new(&api_url, &client_config);

    let client = forge_tls_client::ForgeTlsClient::retry_build(&api_config)
        .await
        .map_err(|err| ReportingError::ApiConnectFailed(err.to_string()))?;
    Ok(client)
}

async fn send_one_report(
    client: &mut ForgeClientT,
    machine_id: &mut String,
    report: &HealthReport,
) -> Result<(), ReportingError> {
    if (report.alerts.is_empty() && report.successes.is_empty())
        || report.observed_at.is_none()
        || machine_id.is_empty()
    {
        return Ok(());
    }

    let request = tonic::Request::new(rpc::InsertMachineHealthReportRequest {
        machine_id: MachineId::from_str(machine_id).ok(),
        health_report_entry: Some(rpc::HealthReportEntry {
            mode: rpc::HealthReportApplyMode::Merge.into(),
            report: Some(report.clone().into()),
        }),
    });
    client
        .insert_machine_health_report(request)
        .await
        .map_err(ReportingError::ApiInvocationError)?;
    machine_id.clear();
    Ok(())
}

pub(crate) async fn send_health_alerts(
    client: &mut ForgeClientT,
    events: &VecDeque<Event>,
    pipeline: &str,
    file_name: &str,
) -> Result<(), ReportingError> {
    let mut report = HealthReport {
        source: format!("log-parser.{pipeline}.{file_name}"),
        triggered_by: None,
        observed_at: None,
        successes: vec![],
        alerts: vec![],
    };

    // send one health report per machine with all alerts found
    let mut machine_id: String = "".to_string();
    for event in events {
        if event.machine_id != machine_id && !machine_id.is_empty() {
            // send off the existing report
            send_one_report(client, &mut machine_id, &report).await?;
            report.successes.clear();
            report.alerts.clear();
            report.observed_at = None;
        }
        if event.cleared {
            continue;
        }
        machine_id = event.machine_id.clone();
        if event.alert {
            let alert = HealthProbeAlert {
                id: HealthProbeId::from_str(event.name.clone().as_str())
                    .map_err(|e| ReportingError::GenericError(e.to_string()))?,
                in_alert_since: Some(event.timestamp),
                message: format!(
                    "[{}] {}",
                    event.severity,
                    event.description.clone().unwrap_or("".to_string())
                ),
                tenant_message: None,
                classifications: vec![
                    HealthAlertClassification::from_str("SerialConsole").unwrap(),
                ],
                target: event.target.clone(),
            };
            report.alerts.push(alert);
        } else {
            let success = HealthProbeSuccess {
                id: HealthProbeId::from_str(event.name.clone().as_str())
                    .map_err(|e| ReportingError::GenericError(e.to_string()))?,
                target: event.target.clone(),
            };
            report.successes.push(success);
        }
        // observed at is correctly being set
        report.observed_at = Some(event.timestamp);
    }
    if !machine_id.is_empty() {
        send_one_report(client, &mut machine_id, &report).await?;
    }

    Ok(())
}
