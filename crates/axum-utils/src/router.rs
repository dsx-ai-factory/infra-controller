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

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use tower::Service;

/// Calls an Axum router with a freshly constructed request.
///
/// Reconstructing the request avoids leaking path parameters extracted by an
/// outer router into an inner router with a different route shape.
pub async fn call_router_with_new_request(router: &mut Router, request: Request<Body>) -> Response {
    let (head, body) = request.into_parts();

    let mut builder = Request::builder().uri(&head.uri).method(&head.method);
    for (key, value) in &head.headers {
        builder = builder.header(key, value);
    }
    let inner_request = builder.body(body).expect("valid incoming HTTP request");

    router.call(inner_request).await.expect("Infallible error")
}
