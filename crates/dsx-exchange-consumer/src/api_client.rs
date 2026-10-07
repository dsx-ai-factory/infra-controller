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

//! Carbide API client for submitting rack health reports.

use std::str::FromStr;

use async_trait::async_trait;
use carbide_uuid::rack::RackId;
use forge_tls::client_config::ClientCert;
use health_report::HealthReport;
use rpc::forge::{
    HealthReportApplyMode, HealthReportEntry, InsertRackHealthReportRequest,
    RemoveRackHealthReportRequest,
};
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use url::Url;

use crate::DsxConsumerError;

/// Source identifier for health report overrides from this consumer.
pub const HEALTH_REPORT_SOURCE: &str = "dsx-exchange-consumer";

/// Trait for submitting rack health reports.
#[async_trait]
pub trait RackHealthReportSink: Send + Sync {
    async fn insert_rack_health_report(
        &self,
        rack_id: &str,
        report: HealthReport,
    ) -> Result<(), DsxConsumerError>;

    async fn remove_rack_health_report(&self, rack_id: &str) -> Result<(), DsxConsumerError>;
}

/// API client wrapper for Carbide API communication.
#[derive(Clone)]
pub struct ApiClientWrapper {
    client: ForgeApiClient,
}

impl ApiClientWrapper {
    pub fn new(root_ca: String, client_cert: String, client_key: String, api_url: &Url) -> Self {
        let client_config = ForgeClientConfig::new(
            root_ca,
            Some(ClientCert {
                cert_path: client_cert,
                key_path: client_key,
            }),
        );
        let api_config = ApiConfig::new(api_url.as_str(), &client_config);

        let client = ForgeApiClient::new(&api_config);

        Self { client }
    }
}

#[async_trait]
impl RackHealthReportSink for ApiClientWrapper {
    async fn insert_rack_health_report(
        &self,
        rack_id: &str,
        report: HealthReport,
    ) -> Result<(), DsxConsumerError> {
        let rack_id = parse_rack_id(rack_id)?;
        let request = InsertRackHealthReportRequest {
            rack_id: Some(rack_id),
            health_report_entry: Some(HealthReportEntry {
                report: Some(report.into()),
                mode: HealthReportApplyMode::Merge.into(),
            }),
        };

        self.client.insert_rack_health_report(request).await?;

        Ok(())
    }

    async fn remove_rack_health_report(&self, rack_id: &str) -> Result<(), DsxConsumerError> {
        let rack_id = parse_rack_id(rack_id)?;
        let request = RemoveRackHealthReportRequest {
            rack_id: Some(rack_id),
            source: HEALTH_REPORT_SOURCE.to_string(),
        };

        self.client.remove_rack_health_report(request).await?;

        Ok(())
    }
}

/// Console sink for debugging - logs rack health reports to console.
pub struct ConsoleRackHealthSink;

#[async_trait]
impl RackHealthReportSink for ConsoleRackHealthSink {
    async fn insert_rack_health_report(
        &self,
        rack_id: &str,
        report: HealthReport,
    ) -> Result<(), DsxConsumerError> {
        tracing::info!(
            rack_id = %rack_id,
            success_count = report.successes.len(),
            alert_count = report.alerts.len(),
            "Inserting rack health override"
        );
        for alert in &report.alerts {
            tracing::warn!(rack_id = %rack_id, alert = ?alert, "Rack health alert");
        }
        Ok(())
    }

    async fn remove_rack_health_report(&self, rack_id: &str) -> Result<(), DsxConsumerError> {
        tracing::info!(
            rack_id = %rack_id,
            source = HEALTH_REPORT_SOURCE,
            "Removing rack health override"
        );
        Ok(())
    }
}

// for error mapping convenience
fn parse_rack_id(rack_id: &str) -> Result<RackId, DsxConsumerError> {
    RackId::from_str(rack_id).map_err(|e| {
        DsxConsumerError::Api(tonic::Status::invalid_argument(format!(
            "invalid rack ID: {e}"
        )))
    })
}
