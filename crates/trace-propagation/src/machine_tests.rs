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

use std::collections::BTreeMap;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider, SpanData};
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;

use super::MachineTraceAttributes;

fn capture() -> (SdkTracerProvider, InMemorySpanExporter, tracing::Dispatch) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("machine-test")));
    (provider, exporter, tracing::Dispatch::new(subscriber))
}

fn identities(span: &SpanData) -> BTreeMap<&str, String> {
    span.attributes
        .iter()
        .filter(|attribute| attribute.key.as_str().starts_with("nico."))
        .map(|attribute| (attribute.key.as_str(), attribute.value.to_string()))
        .collect()
}

#[test]
fn identities_are_optional_canonical_and_can_be_resolved_later() {
    let (_provider, exporter, dispatch) = capture();
    tracing::dispatcher::with_default(&dispatch, || {
        let span = tracing::info_span!(
            "discovery",
            object_id = "legacy",
            forge.machine_id = "legacy"
        );
        MachineTraceAttributes {
            bmc_ip: Some("::ffff:192.0.2.1".parse().unwrap()),
            ..Default::default()
        }
        .record_on(&span);
        let _entered = span.enter();
        // A child already exists when the parent learns the final identity.
        let child = tracing::info_span!("query");
        MachineTraceAttributes {
            machine_id: Some("stable"),
            predicted_id: Some("predicted"),
            managed_host_id: Some("stable"),
            managed_host_bmc_ip: Some("2001:db8::1".parse().unwrap()),
            ..Default::default()
        }
        .record_on(&span);
        MachineTraceAttributes::default().record_on(&span);
        drop(child);
    });
    let spans = exporter.get_finished_spans().unwrap();
    let operation = spans.iter().find(|span| span.name == "discovery").unwrap();
    assert_eq!(
        identities(operation),
        BTreeMap::from([
            ("nico.machine.id", "stable".into()),
            ("nico.machine.predicted_id", "predicted".into()),
            ("nico.machine.bmc.ip", "192.0.2.1".into()),
            ("nico.managed_host.id", "stable".into()),
            ("nico.managed_host.bmc.ip", "2001:db8::1".into()),
        ])
    );
    for key in ["object_id", "forge.machine_id"] {
        assert!(
            operation
                .attributes
                .iter()
                .any(|attribute| attribute.key.as_str() == key
                    && attribute.value.to_string() == "legacy")
        );
    }
    let child = spans.iter().find(|span| span.name == "query").unwrap();
    assert!(identities(child).is_empty());
    assert_eq!(child.parent_span_id, operation.span_context.span_id());
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_machine_subtrees_keep_identity_only_on_their_operations() {
    let (_provider, exporter, dispatch) = capture();
    // WithSubscriber only scopes polling; child spans also need this dispatcher
    // during task teardown to close their iteration parent.
    let _subscriber = tracing::dispatcher::set_default(&dispatch);
    async {
        let root = tracing::info_span!("iteration");
        async {
            let mut tasks = Vec::new();
            for id in ["machine-a", "machine-b"] {
                let operation = tracing::info_span!("machine", object_id = id);
                MachineTraceAttributes {
                    machine_id: Some(id),
                    ..Default::default()
                }
                .record_on(&operation);
                tasks.push(tokio::spawn(
                    async {
                        tokio::task::yield_now().await;
                        let child = tracing::info_span!("rpc");
                        async { tokio::task::yield_now().await }
                            .instrument(child)
                            .await;
                    }
                    .instrument(operation)
                    .with_current_subscriber(),
                ));
            }
            for task in tasks {
                task.await.unwrap();
            }
        }
        .instrument(root)
        .await;
    }
    .with_subscriber(dispatch)
    .await;
    let spans = exporter.get_finished_spans().unwrap();
    let root = spans.iter().find(|span| span.name == "iteration").unwrap();
    assert!(identities(root).is_empty());
    let operations: Vec<_> = spans.iter().filter(|span| span.name == "machine").collect();
    assert_eq!(operations.len(), 2);
    for (id, operation) in ["machine-a", "machine-b"].into_iter().map(|id| {
        (
            id,
            operations
                .iter()
                .find(|span| {
                    identities(span).get("nico.machine.id").map(String::as_str) == Some(id)
                })
                .unwrap(),
        )
    }) {
        assert_eq!(
            identities(operation),
            BTreeMap::from([("nico.machine.id", id.to_owned())])
        );
        assert_eq!(operation.parent_span_id, root.span_context.span_id());
        let children: Vec<_> = spans
            .iter()
            .filter(|span| span.parent_span_id == operation.span_context.span_id())
            .collect();
        assert_eq!(children.len(), 1);
        assert!(identities(children[0]).is_empty());
        assert_eq!(
            children[0].span_context.trace_id(),
            root.span_context.trace_id()
        );
    }
}

#[test]
fn tagging_without_a_subscriber_is_a_noop() {
    MachineTraceAttributes {
        machine_id: Some("machine"),
        ..Default::default()
    }
    .record_on(&tracing::Span::none());
}
