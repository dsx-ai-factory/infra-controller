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
use std::ops::Deref;
use std::sync::LazyLock;

use carbide_metrics_utils::OtelView;
use eyre::WrapErr;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Meter, MeterProvider};
use opentelemetry_prometheus::ExporterBuilder;
use opentelemetry_sdk::metrics::{Aggregation, InstrumentKind, SdkMeterProvider};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_NAMESPACE};
use prometheus::Registry;

static SINGLETON: LazyLock<InstrumentationSingleton> =
    LazyLock::new(InstrumentationSingleton::init_for_dpu_agent);

struct InstrumentationSingleton {
    // SdkMeterProvider has an inner Arc, no need to Arc-wrap it ourselves.
    _meter_provider: SdkMeterProvider,

    // Registry has an inner Arc, no need to Arc-wrap it.
    prometheus_registry: Registry,

    // Meter has an inner Arc.
    dpu_agent_meter: Meter,
}

impl InstrumentationSingleton {
    // Build the standard instrumentation config for dpu-agent.
    fn try_init_for_dpu_agent(set_global_meter: bool) -> eyre::Result<Self> {
        let prometheus_registry = Registry::new();
        let exporter = ExporterBuilder::default()
            .with_registry(prometheus_registry.clone())
            .without_scope_info()
            .without_target_info()
            .build()
            .context("could not build prometheus exporter")?;

        // This defines attributes that are set on the exported logs **and** metrics
        let resource_attributes = opentelemetry_sdk::Resource::builder()
            .with_attributes([
                KeyValue::new(SERVICE_NAME, "dpu-agent"),
                KeyValue::new(SERVICE_NAMESPACE, "forge-system"),
            ])
            .build();

        let meter_provider = SdkMeterProvider::builder()
            .with_reader(exporter)
            .with_resource(resource_attributes)
            .with_view(
                create_retry_histogram_view().context("couldn't create retry histogram view")?,
            )
            .with_view(
                create_network_latency_view().context("couldn't create network latency view")?,
            )
            .with_view(create_network_loss_view().context("couldn't create network loss view")?)
            .build();

        let dpu_agent_meter = meter_provider.meter("forge-dpu-agent");

        // We expect our internal users to use the interfaces inside this module,
        // but if there are other OpenTelemetry users in our dependencies, let's
        // make sure they pick up our provider.
        if set_global_meter {
            opentelemetry::global::set_meter_provider(meter_provider.clone());
        }

        Ok(InstrumentationSingleton {
            _meter_provider: meter_provider,
            prometheus_registry,
            dpu_agent_meter,
        })
    }

    // Note that this function will panic if its inner try_ function returns
    // an error, which is probably caused by one of its builders returning
    // an error. We generally don't expect this to happen outside of breaking
    // upgrades of the crates involved. There's a unit test below that should
    // ensure it always succeeds.
    fn init_for_dpu_agent() -> Self {
        Self::try_init_for_dpu_agent(true).unwrap()
    }
}

pub fn get_prometheus_registry() -> Registry {
    let s = SINGLETON.deref();
    s.prometheus_registry.clone()
}

pub fn get_dpu_agent_meter() -> Meter {
    let s = SINGLETON.deref();
    s.dpu_agent_meter.clone()
}

/// Configures a View for Histograms that describe retries or attempts for
/// operations The view reconfigures the histogram to use a small set of buckets
/// that track the exact amount of retry attempts up to 3, and 2 additional
/// buckets up to 10. This is more useful than the default histogram range where
/// the lowest sets of buckets are 0, 5, 10, 25
fn create_retry_histogram_view() -> carbide_metrics_utils::Result<OtelView> {
    carbide_metrics_utils::new_view(
        "*_(attempts|retries)_*",
        Some(InstrumentKind::Histogram),
        Aggregation::ExplicitBucketHistogram {
            boundaries: vec![0.0, 1.0, 2.0, 3.0, 5.0, 10.0],
            record_min_max: true,
        },
    )
}

fn create_network_latency_view() -> carbide_metrics_utils::Result<OtelView> {
    carbide_metrics_utils::new_view(
        "*_network_latency*",
        None,
        Aggregation::ExplicitBucketHistogram {
            boundaries: vec![
                0.01, 0.02, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 1.0, 5.0, 10.0, 100.0, 500.0, 1000.0,
            ],
            record_min_max: true,
        },
    )
}

fn create_network_loss_view() -> carbide_metrics_utils::Result<OtelView> {
    carbide_metrics_utils::new_view(
        "*_network_loss_percentage*",
        None,
        Aggregation::ExplicitBucketHistogram {
            boundaries: vec![0.2, 0.4, 0.6, 0.8, 1.0],
            record_min_max: true,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_view_creators() {
        // All of these calls should be infallible in practice. We don't expect
        // errors from them outside of crate upgrade breakage.
        let _retry_histogram_view = create_retry_histogram_view()
            .expect("The create_retry_histogram_view() function must not fail");
        let _network_latency_view = create_network_latency_view()
            .expect("The create_network_latency_view() function must not fail");
        let _network_loss_view = create_network_loss_view()
            .expect("The create_network_loss_view() function must not fail");
    }

    #[test]
    fn test_singleton_init_function() {
        // `false` keeps this construction test from replacing the process-global
        // provider. Event instruments cache that provider on first use, and the
        // `MetricsCapture` tests in this binary must keep reading the same one.
        let _s = InstrumentationSingleton::try_init_for_dpu_agent(false).expect(
            "The instrumentaion singleton's initialization function must not return an error",
        );
    }

    #[test]
    fn test_singletons_are_send_and_sync() {
        // Anything we're potentially handing out singletons of will have to be
        // Send and Sync, so let's make sure they all typecheck as such.
        require_send_and_sync::<Registry>();
        require_send_and_sync::<Meter>();
        require_send_and_sync::<InstrumentationSingleton>();
    }

    fn require_send_and_sync<T>()
    where
        T: Send + Sync,
    {
    }
}
