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

use super::collector_logs::ExportLogsServiceRequest;
use super::collector_logs::logs_service_client::LogsServiceClient;
use super::convert::build_export_request;
use super::{OtlpExport, OtlpSignal, run_drain};
use crate::config::OtlpTargetConfig;
use crate::sink::otlp::OtlpQueue;
use crate::sink::{CollectorEvent, EventContext};

pub(crate) struct OtlpDrainTask {
    queue: Arc<OtlpQueue>,
    target: OtlpTargetConfig,
}

impl OtlpDrainTask {
    pub(crate) fn new(queue: Arc<OtlpQueue>, target: OtlpTargetConfig) -> Self {
        Self { queue, target }
    }

    pub(crate) async fn run(self) {
        let include_alert_details = self.target.include_alert_details;
        run_drain(self.queue, self.target, OtlpSignal::Logs, move |channel| {
            LogsExport {
                client: LogsServiceClient::new(channel),
                include_alert_details,
            }
        })
        .await;
    }
}

/// Log export to one target.
#[derive(Clone)]
struct LogsExport {
    client: LogsServiceClient<Channel>,
    include_alert_details: bool,
}

impl OtlpExport for LogsExport {
    type Item = (EventContext, CollectorEvent);
    type Request = ExportLogsServiceRequest;

    fn build(&self, items: &[Self::Item], observed_nanos: u64) -> Self::Request {
        build_export_request(items, observed_nanos, self.include_alert_details)
    }

    fn record_count(request: &Self::Request) -> usize {
        request
            .resource_logs
            .iter()
            .flat_map(|rl| &rl.scope_logs)
            .map(|sl| sl.log_records.len())
            .sum()
    }

    fn encoded_len(request: &Self::Request) -> usize {
        prost::Message::encoded_len(request)
    }

    async fn send(&mut self, request: Self::Request) -> Result<(), tonic::Status> {
        self.client.export(request).await.map(drop)
    }
}
