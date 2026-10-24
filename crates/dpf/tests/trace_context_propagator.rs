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

//! Isolate process-global propagator changes from the W3C request tests.

use carbide_dpf::KubeRepository;
use http::{Request, Response};
use kube::Client;
use kube::client::Body;
use opentelemetry::propagation::text_map_propagator::FieldIter;
use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};
use opentelemetry::trace::{TraceContextExt, TracerProvider};
use opentelemetry::{Context, global};
use opentelemetry_sdk::trace::SdkTracerProvider;
use serde_json::{Value, json};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::prelude::*;

#[derive(Debug)]
struct VendorPropagator {
    fields: Vec<String>,
}

impl TextMapPropagator for VendorPropagator {
    fn inject_context(&self, context: &Context, injector: &mut dyn Injector) {
        injector.set(
            &self.fields[0],
            context.span().span_context().span_id().to_string(),
        );
    }

    fn extract_with_context(&self, context: &Context, _: &dyn Extractor) -> Context {
        context.clone()
    }

    fn fields(&self) -> FieldIter<'_> {
        FieldIter::new(&self.fields)
    }
}

#[tokio::test]
async fn installed_propagator_controls_annotation_fields() {
    let provider = SdkTracerProvider::builder().build();
    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::INFO)
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("carrier-test")))
        .init();
    let service = tower::service_fn(|request: Request<Body>| async move {
        Ok::<_, kube::Error>(Response::new(request.into_body()))
    });
    let repository = KubeRepository::with_client(Client::new(service, "test-ns"));
    let send = || async {
        let request = Request::post(
            "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices",
        )
        .header("content-type", "application/json")
        .body(Body::from(b"{ }".to_vec()))
        .unwrap();
        let response = repository.client().send(request).await.unwrap();
        response.into_body().collect_bytes().await.unwrap()
    };
    // A valid span with the default no-op propagator must not even add empty metadata.
    let unchanged = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        send().instrument(tracing::info_span!("no_propagator")),
    )
    .await
    .unwrap();
    assert_eq!(unchanged.as_ref(), b"{ }");

    global::set_text_map_propagator(VendorPropagator {
        fields: vec!["vendor-parent".into()],
    });
    let span = tracing::info_span!("vendor");
    let expected = span.context().span().span_context().span_id().to_string();
    let body = tokio::time::timeout(std::time::Duration::from_secs(5), send().instrument(span))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({
            "metadata": {"annotations": {"tracing.k8s.io/vendor-parent": expected}}
        })
    );
}
