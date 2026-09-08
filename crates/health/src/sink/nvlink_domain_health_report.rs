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

//! Ordered submission of NVLink domain health reports to the NICo API.
//!
//! The queue retains the latest pending report for each domain and source. A
//! single worker prevents an older alert submission from completing after its
//! recovery and restoring stale health state.

use std::sync::Arc;

use carbide_instrument::{Outcome, emit};
use carbide_uuid::nvlink::NvLinkDomainId;
use tokio_util::sync::CancellationToken;

use super::dedup_queue::DedupQueue;
use super::{
    CollectorEvent, DataSink, EventContext, HealthReport, HealthReportSubmitted,
    HealthReportTarget, ReportSource,
};
use crate::HealthError;
use crate::api_client::ApiClientWrapper;
use crate::config::NvLinkDomainHealthReportSinkConfig;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct NvLinkDomainHealthReportKey {
    id: NvLinkDomainId,
    source: ReportSource,
}

/// Submits `NvLinkDomain`-target health reports to the NICo API.
pub struct NvLinkDomainHealthReportSink {
    queue: Arc<DedupQueue<NvLinkDomainHealthReportKey, Arc<HealthReport>>>,
    cancel_token: CancellationToken,
}

impl NvLinkDomainHealthReportSink {
    /// Creates the sink and its ordered submission worker.
    ///
    /// Returns an error when called without an active Tokio runtime.
    pub fn new(config: &NvLinkDomainHealthReportSinkConfig) -> Result<Self, HealthError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|error| {
            HealthError::GenericError(format!(
                "NVLink domain health report sink requires active Tokio runtime: {error}"
            ))
        })?;

        let client = ApiClientWrapper::new(
            config.connection.root_ca.clone(),
            config.connection.client_cert.clone(),
            config.connection.client_key.clone(),
            &config.connection.api_url,
        );

        let queue: Arc<DedupQueue<NvLinkDomainHealthReportKey, Arc<HealthReport>>> =
            Arc::new(DedupQueue::new());

        let cancel_token = CancellationToken::new();
        let worker_cancel_token = cancel_token.clone();
        let worker_queue = Arc::clone(&queue);

        // A single worker preserves alert and recovery order for each domain.
        // Its cancellation token bounds the worker lifetime to the sink.
        handle.spawn(async move {
            loop {
                let Some((key, report)) = worker_cancel_token
                    .run_until_cancelled(worker_queue.next())
                    .await
                else {
                    return;
                };

                match report.as_ref().try_into() {
                    Ok(converted) => {
                        let Some(result) = worker_cancel_token
                            .run_until_cancelled(
                                client.submit_nvlink_domain_health_report(&key.id, converted),
                            )
                            .await
                        else {
                            return;
                        };

                        emit(HealthReportSubmitted {
                            target: HealthReportTarget::NvLinkDomain,
                            outcome: Outcome::from(&result),
                            id: key.id.to_string(),
                            worker_id: 0,
                            error: result
                                .err()
                                .map(|error| error.to_string())
                                .unwrap_or_default(),
                        });
                    }
                    Err(error) => {
                        tracing::warn!(
                            ?error,
                            nvlink_domain_id = %key.id,
                            "Failed to convert NVLink domain health report"
                        );
                    }
                }
            }
        });

        Ok(Self {
            queue,
            cancel_token,
        })
    }
}

impl Drop for NvLinkDomainHealthReportSink {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

impl DataSink for NvLinkDomainHealthReportSink {
    fn sink_type(&self) -> &'static str {
        "nvlink_domain_health_report_sink"
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        let CollectorEvent::HealthReport(report) = event else {
            return Ok(());
        };

        if report.target != Some(HealthReportTarget::NvLinkDomain) {
            return Ok(());
        }

        let Some(domain_id) = context.nvlink_domain_uuid() else {
            tracing::warn!(
                endpoint_key = context.endpoint_key(),
                "Received NVLink-domain-target HealthReport event without domain context"
            );

            return Err(HealthError::GenericError(
                "NVLink-domain-target health report event without domain context".to_string(),
            ));
        };

        let key = NvLinkDomainHealthReportKey {
            id: domain_id,
            source: report.source,
        };

        self.queue.save_latest(key, Arc::clone(report));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::str::FromStr;
    use std::time::Duration;

    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::{BmcAddr, EndpointMetadata, SwitchData, SwitchEndpointRole};
    use crate::sink::{HealthReportAlert, HealthReportSuccess, Probe};

    #[test]
    fn queued_recovery_replaces_alert_for_the_same_domain_source() {
        let domain_id = NvLinkDomainId::from_str("9f4b45ec-705a-4af4-89f7-a112bc9c8f4e")
            .expect("valid NVLink domain ID");

        let queue = Arc::new(DedupQueue::new());

        let sink = NvLinkDomainHealthReportSink {
            queue: Arc::clone(&queue),
            cancel_token: CancellationToken::new(),
        };

        let context = EventContext {
            endpoint_key: "00:11:22:33:44:55".to_string(),
            addr: BmcAddr {
                ip: IpAddr::from_str("10.0.0.1").expect("valid IP address"),
                port: Some(443),
                mac: MacAddress::from_str("00:11:22:33:44:55").expect("valid MAC address"),
            },
            collector_type: "nmxc",
            metadata: Some(EndpointMetadata::Switch(SwitchData {
                id: None,
                serial: "SW-001".to_string(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: Some(domain_id),
                endpoint_role: SwitchEndpointRole::Host,
                is_primary: true,
                nmxc_enabled: true,
                nmxt_enabled: false,
            })),
            rack_id: None,
            labels: Default::default(),
        };

        let alert = CollectorEvent::HealthReport(Arc::new(HealthReport {
            source: ReportSource::NmxcDomainState,
            target: Some(HealthReportTarget::NvLinkDomain),
            observed_at: None,
            successes: Vec::new(),
            alerts: vec![HealthReportAlert {
                attribution: None,
                probe_id: Probe::NmxControllerHealth,
                target: None,
                message: "NMX-C controller health is degraded".to_string(),
                classifications: Vec::new(),
            }],
        }));

        let recovery = CollectorEvent::HealthReport(Arc::new(HealthReport {
            source: ReportSource::NmxcDomainState,
            target: Some(HealthReportTarget::NvLinkDomain),
            observed_at: None,
            successes: vec![HealthReportSuccess {
                attribution: None,
                probe_id: Probe::NmxControllerHealth,
                target: None,
            }],
            alerts: Vec::new(),
        }));

        sink.try_handle_event(&context, &alert)
            .expect("alert should be queued");

        sink.try_handle_event(&context, &recovery)
            .expect("recovery should be queued");

        let (key, report) = queue.pop().expect("latest report should remain queued");

        assert_eq!(key.id, domain_id);
        assert_eq!(key.source, ReportSource::NmxcDomainState);
        assert!(report.alerts.is_empty());
        assert_eq!(report.successes.len(), 1);
        assert!(queue.pop().is_none());
    }

    #[tokio::test]
    async fn dropping_sink_releases_submission_worker_queue() {
        let sink = NvLinkDomainHealthReportSink::new(&Default::default())
            .expect("sink should start inside a Tokio runtime");

        let queue = Arc::downgrade(&sink.queue);

        drop(sink);

        tokio::time::timeout(Duration::from_secs(1), async {
            while queue.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("submission worker should release its queue after sink shutdown");
    }
}
