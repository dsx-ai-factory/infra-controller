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
use carbide_uuid::switch::SwitchId;

use super::dedup_queue::DedupQueue;
use super::{
    CollectorEvent, DataSink, EventContext, HealthReport, HealthReportSubmitted,
    HealthReportTarget, ReportSource,
};
use crate::HealthError;
use crate::api_client::ApiClientWrapper;
use crate::config::SwitchHealthReportSinkConfig;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SwitchHealthReportKey {
    id: SwitchId,
    source: ReportSource,
}

pub struct SwitchHealthReportSink {
    queue: Arc<DedupQueue<SwitchHealthReportKey, Arc<HealthReport>>>,
    skip_empty_reports: bool,
}

impl SwitchHealthReportSink {
    pub fn new(config: &SwitchHealthReportSinkConfig) -> Result<Self, HealthError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|error| {
            HealthError::GenericError(format!(
                "switch health report sink requires active Tokio runtime: {error}"
            ))
        })?;

        let client = Arc::new(ApiClientWrapper::new(
            config.connection.root_ca.clone(),
            config.connection.client_cert.clone(),
            config.connection.client_key.clone(),
            &config.connection.api_url,
        ));

        let queue: Arc<DedupQueue<SwitchHealthReportKey, Arc<HealthReport>>> =
            Arc::new(DedupQueue::new());

        for worker_id in 0..config.workers {
            let worker_client = Arc::clone(&client);
            let worker_queue = Arc::clone(&queue);
            handle.spawn(async move {
                loop {
                    let (key, report) = worker_queue.next().await;

                    match report.as_ref().try_into() {
                        Ok(converted) => {
                            let result = worker_client
                                .submit_switch_health_report(&key.id, converted)
                                .await;
                            emit(HealthReportSubmitted {
                                target: HealthReportTarget::Switch,
                                outcome: Outcome::from(&result),
                                id: key.id.to_string(),
                                worker_id,
                                error: result.err().map(|e| e.to_string()).unwrap_or_default(),
                            });
                        }
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                worker_id,
                                switch_id = %key.id,
                                "Failed to convert switch health report"
                            );
                        }
                    }
                }
            });
        }

        Ok(Self {
            queue,
            skip_empty_reports: config.skip_empty_reports,
        })
    }
}

impl DataSink for SwitchHealthReportSink {
    fn sink_type(&self) -> &'static str {
        "switch_health_report_sink"
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        let CollectorEvent::HealthReport(report) = event else {
            return Ok(());
        };

        if report.target != Some(HealthReportTarget::Switch) {
            return Ok(());
        }

        if self.skip_empty_reports && report.is_empty() {
            tracing::debug!(
                source = ?report.source,
                "Skipping empty switch health report"
            );
            return Ok(());
        }

        let switch_id = if let Some(switch_id) = context.switch_id() {
            switch_id
        } else {
            tracing::warn!(
                endpoint_key = context.endpoint_key(),
                "Received switch-target HealthReport event without switch_id context"
            );
            return Err(HealthError::GenericError(
                "switch-target health report event without switch_id context".to_string(),
            ));
        };

        let key = SwitchHealthReportKey {
            id: switch_id,
            source: report.source,
        };
        self.queue.save_latest(key, Arc::clone(report));

        Ok(())
    }
}
