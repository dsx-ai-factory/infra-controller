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
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

mod health_report;
mod intrusion_events;
mod leak_events;
mod nmxc_domain_state;
mod rack_leak;
pub use health_report::HealthReportProcessor;
pub use intrusion_events::BmcIntrusionEventProcessor;
pub use leak_events::LeakEventProcessor;
pub use nmxc_domain_state::NmxcDomainStateProcessor;
pub use rack_leak::RackLeakProcessor;

use crate::HealthError;
use crate::metrics::{ComponentMetrics, MetricsManager};
use crate::sink::{CollectorEvent, DataSink, EventContext};

pub trait EventProcessor: Send + Sync {
    fn processor_type(&self) -> &'static str;
    fn process_event(&self, context: &EventContext, event: &CollectorEvent) -> Vec<CollectorEvent>;
}

struct PendingEvent<'a> {
    event: Cow<'a, CollectorEvent>,
    blocked_processors: Vec<bool>,
}

pub struct EventProcessingPipeline {
    processors: Vec<Arc<dyn EventProcessor>>,
    sink: Arc<dyn DataSink>,
    component_metrics: Arc<ComponentMetrics>,
}

impl EventProcessingPipeline {
    pub fn new(
        processors: Vec<Arc<dyn EventProcessor>>,
        sink: Arc<dyn DataSink>,
        metrics_manager: Arc<MetricsManager>,
    ) -> Self {
        debug_assert!(
            !processors.is_empty(),
            "EventProcessingPipeline should only be used when processors are configured"
        );
        Self {
            processors,
            sink,
            component_metrics: metrics_manager.component_metrics(),
        }
    }

    fn next_events(
        &self,
        context: &EventContext,
        current_event: &CollectorEvent,
        blocked_processors: &[bool],
        queue: &mut VecDeque<PendingEvent>,
    ) {
        for (processor_idx, processor) in self.processors.iter().enumerate() {
            if blocked_processors[processor_idx] {
                continue;
            }

            let start = Instant::now();
            let emitted = processor.process_event(context, current_event);
            self.component_metrics.record_operation(
                crate::metrics::ComponentKind::Processor,
                processor.processor_type(),
                start.elapsed(),
                true,
            );
            if emitted.is_empty() {
                continue;
            }

            for event in emitted {
                let mut next_blocked_processors = blocked_processors.to_vec();
                next_blocked_processors[processor_idx] = true;
                queue.push_back(PendingEvent {
                    event: Cow::Owned(event),
                    blocked_processors: next_blocked_processors,
                });
            }
        }
    }
}

impl DataSink for EventProcessingPipeline {
    fn sink_type(&self) -> &'static str {
        "event_processing_pipeline"
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        let mut queue = VecDeque::from(vec![PendingEvent {
            event: Cow::Borrowed(event),
            blocked_processors: vec![false; self.processors.len()],
        }]);

        while let Some(current) = queue.pop_front() {
            self.sink.try_handle_event(context, &current.event)?;
            self.next_events(
                context,
                &current.event,
                &current.blocked_processors,
                &mut queue,
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::BmcAddr;
    use crate::metrics::MetricsManager;

    struct CountingSink {
        counter: Arc<AtomicUsize>,
    }

    impl DataSink for CountingSink {
        fn sink_type(&self) -> &'static str {
            "counting_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            _event: &CollectorEvent,
        ) -> Result<(), HealthError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct SelfReemittingProcessor {
        counter: Arc<AtomicUsize>,
    }

    impl EventProcessor for SelfReemittingProcessor {
        fn processor_type(&self) -> &'static str {
            "self_reemitting_processor"
        }

        fn process_event(
            &self,
            _context: &EventContext,
            event: &CollectorEvent,
        ) -> Vec<CollectorEvent> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            vec![event.clone()]
        }
    }

    fn context() -> EventContext {
        EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: Some(MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac")),
            },
            collector_type: "test",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    #[test]
    fn processor_does_not_reconsume_its_own_descendants() {
        let processor_counter = Arc::new(AtomicUsize::new(0));
        let sink_counter = Arc::new(AtomicUsize::new(0));
        let metrics_manager =
            Arc::new(MetricsManager::new("test").expect("should create metrics manager"));
        let pipeline = EventProcessingPipeline::new(
            vec![Arc::new(SelfReemittingProcessor {
                counter: processor_counter.clone(),
            })],
            Arc::new(CountingSink {
                counter: sink_counter.clone(),
            }),
            metrics_manager,
        );

        let event = CollectorEvent::Metric(
            crate::sink::MetricSample {
                key: "k".to_string(),
                name: "n".to_string(),
                metric_type: "gauge".to_string(),
                unit: "count".to_string(),
                value: 1.0,
                labels: Vec::new(),
                context: None,
            }
            .into(),
        );
        pipeline.handle_event(&context(), &event);

        assert_eq!(processor_counter.load(Ordering::SeqCst), 1);
        assert_eq!(sink_counter.load(Ordering::SeqCst), 2);
    }
}
