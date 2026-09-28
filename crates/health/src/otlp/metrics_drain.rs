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

use tonic::transport::Channel;

use super::collector_metrics::ExportMetricsServiceRequest;
use super::collector_metrics::metrics_service_client::MetricsServiceClient;
use super::convert::build_metrics_export_request;
use super::{OtlpExport, OtlpSignal, run_drain};
use crate::config::OtlpTargetConfig;
use crate::sink::otlp::OtlpMetricsQueue;
use crate::sink::{EventContext, MetricSample};

pub(crate) struct OtlpMetricsDrainTask {
    queue: Arc<OtlpMetricsQueue>,
    target: OtlpTargetConfig,
    metric_name_prefix: String,
}

impl OtlpMetricsDrainTask {
    pub(crate) fn new(
        queue: Arc<OtlpMetricsQueue>,
        target: OtlpTargetConfig,
        metric_name_prefix: String,
    ) -> Self {
        Self {
            queue,
            target,
            metric_name_prefix,
        }
    }

    pub(crate) async fn run(self) {
        let metric_name_prefix: Arc<str> = self.metric_name_prefix.into();
        run_drain(
            self.queue,
            self.target,
            OtlpSignal::Metrics,
            move |channel| MetricsExport {
                client: MetricsServiceClient::new(channel),
                metric_name_prefix: metric_name_prefix.clone(),
            },
        )
        .await;
    }
}

/// Metric export to one target.
#[derive(Clone)]
struct MetricsExport {
    client: MetricsServiceClient<Channel>,
    metric_name_prefix: Arc<str>,
}

impl OtlpExport for MetricsExport {
    type Item = (EventContext, MetricSample);
    type Request = ExportMetricsServiceRequest;

    fn build(&self, items: &[Self::Item], observed_nanos: u64) -> Self::Request {
        build_metrics_export_request(items, observed_nanos, &self.metric_name_prefix)
    }

    fn record_count(request: &Self::Request) -> usize {
        request
            .resource_metrics
            .iter()
            .flat_map(|rm| &rm.scope_metrics)
            .map(|sm| sm.metrics.len())
            .sum()
    }

    fn encoded_len(request: &Self::Request) -> usize {
        prost::Message::encoded_len(request)
    }

    async fn send(&mut self, request: Self::Request) -> Result<(), tonic::Status> {
        self.client.export(request).await.map(drop)
    }
}
