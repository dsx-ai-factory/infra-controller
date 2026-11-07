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

use carbide_instrument::{Outcome, emit};
use carbide_uuid::power_shelf::PowerShelfId;

use super::dedup_queue::DedupQueue;
use super::report_worker::run_report_worker;
use super::shutdown::{DeliveryCounts, SinkWorkers};
use super::{
    CollectorEvent, DataSink, EventContext, HealthReport, HealthReportSubmitted,
    HealthReportTarget, ReportSource,
};
use crate::HealthError;
use crate::api_client::ApiClientWrapper;
use crate::config::PowerShelfHealthReportSinkConfig;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PowerShelfHealthReportKey {
    id: PowerShelfId,
    source: ReportSource,
}

const SINK_TYPE: &str = "power_shelf_health_report_sink";

pub struct PowerShelfHealthReportSink {
    queue: Arc<DedupQueue<PowerShelfHealthReportKey, Arc<HealthReport>>>,
    skip_empty_reports: bool,
}

impl PowerShelfHealthReportSink {
    /// Creates the sink and its submission workers, registering them with
    /// `sink_workers` for the shutdown flush.
    ///
    /// Returns an error when called without an active Tokio runtime.
    pub(crate) fn new(
        config: &PowerShelfHealthReportSinkConfig,
        sink_workers: &mut SinkWorkers,
    ) -> Result<Self, HealthError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|error| {
            HealthError::GenericError(format!(
                "power shelf health report sink requires active Tokio runtime: {error}"
            ))
        })?;

        let client = Arc::new(ApiClientWrapper::new(
            config.connection.root_ca.clone(),
            config.connection.client_cert.clone(),
            config.connection.client_key.clone(),
            &config.connection.api_url,
        ));

        let queue: Arc<DedupQueue<PowerShelfHealthReportKey, Arc<HealthReport>>> =
            Arc::new(DedupQueue::new());

        let counts = DeliveryCounts::default();
        let handles = (0..config.workers)
            .map(|worker_id| {
                let client = Arc::clone(&client);
                handle.spawn(run_report_worker(
                    Arc::clone(&queue),
                    counts.clone(),
                    sink_workers.flush_token(),
                    move |key, report| submit_report(Arc::clone(&client), worker_id, key, report),
                ))
            })
            .collect();

        let queued = Arc::clone(&queue);
        sink_workers.register(SINK_TYPE, None, move || queued.len(), counts, handles);

        Ok(Self {
            queue,
            skip_empty_reports: config.skip_empty_reports,
        })
    }
}

async fn submit_report(
    client: Arc<ApiClientWrapper>,
    worker_id: usize,
    key: PowerShelfHealthReportKey,
    report: Arc<HealthReport>,
) -> bool {
    match report.as_ref().try_into() {
        Ok(converted) => {
            let result = client
                .submit_power_shelf_health_report(&key.id, converted)
                .await;
            let delivered = result.is_ok();
            emit(HealthReportSubmitted {
                target: HealthReportTarget::PowerShelf,
                outcome: Outcome::from(&result),
                id: key.id.to_string(),
                worker_id,
                error: result.err().map(|e| e.to_string()).unwrap_or_default(),
            });
            delivered
        }
        Err(error) => {
            tracing::warn!(
                ?error,
                worker_id,
                power_shelf_id = %key.id,
                "Failed to convert power shelf health report"
            );
            false
        }
    }
}

impl DataSink for PowerShelfHealthReportSink {
    fn sink_type(&self) -> &'static str {
        SINK_TYPE
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        let CollectorEvent::HealthReport(report) = event else {
            return Ok(());
        };

        if report.target != Some(HealthReportTarget::PowerShelf) {
            return Ok(());
        }

        if self.skip_empty_reports && report.is_empty() {
            tracing::debug!(
                source = ?report.source,
                "Skipping empty power shelf health report"
            );
            return Ok(());
        }

        let power_shelf_id = if let Some(power_shelf_id) = context.power_shelf_id() {
            power_shelf_id
        } else {
            tracing::warn!(
                endpoint_key = context.endpoint_key(),
                "Received power-shelf-target HealthReport event without power_shelf_id context"
            );
            return Err(HealthError::GenericError(
                "power-shelf-target health report event without power_shelf_id context".to_string(),
            ));
        };

        let key = PowerShelfHealthReportKey {
            id: power_shelf_id,
            source: report.source,
        };
        self.queue.save_latest(key, Arc::clone(report));

        Ok(())
    }
}
