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

use std::borrow::Cow;
use std::sync::Arc;

use nv_redfish::ServiceRoot;
use nv_redfish::core::Bmc;

use crate::HealthError;
use crate::collectors::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;
use crate::sink::{CollectorEvent, DataSink, EventContext, FirmwareInfo};

pub struct FirmwareCollectorConfig {
    pub data_sink: Option<Arc<dyn DataSink>>,
}

pub struct FirmwareCollector<B: Bmc> {
    bmc: Arc<B>,
    event_context: EventContext,
    data_sink: Option<Arc<dyn DataSink>>,
}

impl<B: Bmc + 'static> PeriodicCollector<B> for FirmwareCollector<B> {
    type Config = FirmwareCollectorConfig;

    fn new_runner(
        bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let event_context = EventContext::from_endpoint(endpoint.as_ref(), "firmware_collector");
        Ok(Self {
            bmc,
            event_context,
            data_sink: config.data_sink,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        self.run_firmware_iteration().await
    }

    fn collector_type(&self) -> &'static str {
        "firmware_collector"
    }

    async fn stop(&mut self) {
        self.emit_event(CollectorEvent::CollectorRemoved);
    }
}

impl<B: Bmc + 'static> FirmwareCollector<B> {
    fn emit_event(&self, event: CollectorEvent) {
        if let Some(data_sink) = &self.data_sink {
            data_sink.handle_event(&self.event_context, &event);
        }
    }

    async fn run_firmware_iteration(&self) -> Result<IterationResult, HealthError> {
        let service_root = ServiceRoot::new(self.bmc.clone()).await?;
        let Some(update_service) = service_root.update_service().await? else {
            return Ok(IterationResult {
                refresh_triggered: true,
                entity_count: Some(0),
                fetch_failures: 0,
            });
        };
        let firmware_inventories = update_service.firmware_inventories().await?;

        let mut firmware_count = 0;

        for firmware_item in firmware_inventories.iter().flatten() {
            let firmware_data = firmware_item.raw();

            let Some(version) = firmware_data.version.clone().flatten() else {
                tracing::debug!(
                    firmware_id = %firmware_data.id,
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "Skipping firmware with no version"
                );
                continue;
            };

            let component = firmware_data.name.clone();
            let attributes = vec![
                (Cow::Borrowed("firmware_name"), component.clone()),
                (Cow::Borrowed("version"), version.clone()),
            ];

            self.emit_event(CollectorEvent::Firmware(FirmwareInfo {
                component,
                version,
                attributes,
            }));
            firmware_count += 1;
        }

        Ok(IterationResult {
            refresh_triggered: true,
            entity_count: Some(firmware_count),
            fetch_failures: 0,
        })
    }
}
