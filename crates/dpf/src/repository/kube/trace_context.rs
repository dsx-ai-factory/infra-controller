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

//! Trace propagation at the Kubernetes client's request boundary.

use http::{Method, Request, header};
use kube::Client;
use kube::client::Body;
use opentelemetry::propagation::Injector;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::{Context, global};
use serde_json::{Map, Value, json};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Writes propagator-owned fields directly into the annotation carrier.
struct AnnotationInjector<'a> {
    annotations: &'a mut Map<String, Value>,
    merge_patch: bool,
    written: bool,
}

impl Injector for AnnotationInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        let key = format!("tracing.k8s.io/{key}");
        if !value.is_empty() {
            self.annotations.insert(key, value.into());
        } else if self.merge_patch {
            // An omitted merge-patch field would retain stale propagation state on the server.
            self.annotations.insert(key, Value::Null);
        } else {
            self.annotations.remove(&key);
        }
        self.written = true;
    }
}

/// Wrap an existing transport so production and supplied clients share the same interception.
pub(super) fn wrap_client(client: Client) -> Client {
    let namespace = client.default_namespace().to_owned();
    let valid_until = *client.valid_until();
    let service = tower::service_fn(move |request: Request<Body>| {
        let client = client.clone();
        // Tower enters the queued request's span for Service::call. Capture its context before
        // returning the future that reads the body, rather than relying on the worker's later span.
        let context = tracing::Span::current().context();
        async move {
            let request = inject_request(request, &context).await?;
            client.send(request).await
        }
    });
    Client::new(service, namespace).with_valid_until(valid_until)
}

/// Select writes whose JSON object bodies may carry annotations.
/// The body parser leaves non-object bodies and explicit deletions unchanged.
fn supported_write(request: &Request<Body>) -> bool {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH
    ) && matches!(
        content_type,
        Some("application/json" | "application/apply-patch+yaml" | "application/merge-patch+json")
    )
}

async fn inject_request(request: Request<Body>, context: &Context) -> kube::Result<Request<Body>> {
    if !supported_write(&request) || !context.span().span_context().is_valid() {
        return Ok(request);
    }
    let merge_patch = request.headers()[header::CONTENT_TYPE] == "application/merge-patch+json";
    let (mut parts, body) = request.into_parts();
    let original = body.collect_bytes().await?;
    // kube serializes typed Apply payloads as JSON even with the apply-patch+yaml content type.
    // Leave other encodings and malformed requests for the API server to interpret.
    let mut value: Value = match serde_json::from_slice(&original) {
        Ok(value) => value,
        Err(_) => return Ok(Request::from_parts(parts, original.into())),
    };
    let annotations = value
        .as_object_mut()
        .and_then(|object| {
            object
                .entry("metadata")
                .or_insert_with(|| json!({}))
                .as_object_mut()
        })
        .and_then(|metadata| {
            metadata
                .entry("annotations")
                .or_insert_with(|| json!({}))
                .as_object_mut()
        });
    let Some(annotations) = annotations else {
        // Preserve explicit deletion and invalid metadata without partially rewriting the body.
        return Ok(Request::from_parts(parts, original.into()));
    };
    let mut injector = AnnotationInjector {
        annotations,
        merge_patch,
        written: false,
    };
    global::get_text_map_propagator(|propagator| propagator.inject_context(context, &mut injector));
    if !injector.written {
        return Ok(Request::from_parts(parts, original.into()));
    }
    let body = serde_json::to_vec(&value).map_err(kube::Error::SerdeError)?;
    // The transport computes the length of the rewritten body.
    parts.headers.remove(header::CONTENT_LENGTH);
    Ok(Request::from_parts(parts, body.into()))
}
