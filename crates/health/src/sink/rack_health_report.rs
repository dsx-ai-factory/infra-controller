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
use carbide_uuid::rack::RackId;

use super::dedup_queue::DedupQueue;
use super::report_worker::run_report_worker;
use super::shutdown::{DeliveryCounts, SinkWorkers};
use super::{
    CollectorEvent, DataSink, EventContext, HealthReport, HealthReportSubmitted,
    HealthReportTarget, ReportSource,
};
use crate::HealthError;
use crate::api_client::ApiClientWrapper;
use crate::config::RackHealthReportSinkConfig;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RackHealthReportKey {
    id: RackId,
    source: ReportSource,
}

const SINK_TYPE: &str = "rack_health_report_sink";

pub struct RackHealthReportSink {
    queue: Arc<DedupQueue<RackHealthReportKey, Arc<HealthReport>>>,
    skip_empty_reports: bool,
}

impl RackHealthReportSink {
    /// Creates the sink and its submission workers, registering them with
    /// `sink_workers` for the shutdown flush.
    ///
    /// Returns an error when called without an active Tokio runtime.
    pub(crate) fn new(
        config: &RackHealthReportSinkConfig,
        sink_workers: &mut SinkWorkers,
    ) -> Result<Self, HealthError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|error| {
            HealthError::GenericError(format!(
                "rack health report sink requires active Tokio runtime: {error}"
            ))
        })?;

        let client = Arc::new(ApiClientWrapper::new(
            config.connection.root_ca.clone(),
            config.connection.client_cert.clone(),
            config.connection.client_key.clone(),
            &config.connection.api_url,
        ));

        let queue: Arc<DedupQueue<RackHealthReportKey, Arc<HealthReport>>> =
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
    key: RackHealthReportKey,
    report: Arc<HealthReport>,
) -> bool {
    match report.as_ref().try_into() {
        Ok(converted) => {
            let result = client.submit_rack_health_report(&key.id, converted).await;
            let delivered = result.is_ok();

            emit(HealthReportSubmitted {
                target: HealthReportTarget::Rack,
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
                rack_id = %key.id,
                "Failed to convert rack health report"
            );

            false
        }
    }
}

impl DataSink for RackHealthReportSink {
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

        if report.target != Some(HealthReportTarget::Rack) {
            return Ok(());
        }

        if self.skip_empty_reports && report.is_empty() {
            tracing::debug!(
                source = ?report.source,
                "Skipping empty rack health report"
            );
            return Ok(());
        }

        let Some(rack_id) = context.rack_id() else {
            tracing::warn!(
                endpoint_key = context.endpoint_key(),
                "Received rack-target HealthReport event without rack_id context"
            );
            return Err(HealthError::GenericError(
                "rack-target health report event without rack_id context".to_string(),
            ));
        };

        let key = RackHealthReportKey {
            id: rack_id.clone(),
            source: report.source,
        };
        self.queue.save_latest(key, Arc::clone(report));

        Ok(())
    }
}
