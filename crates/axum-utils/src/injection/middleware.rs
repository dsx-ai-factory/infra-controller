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

use axum::Router;
use axum::body::Body;
use axum::extract::{OriginalUri, Request, State};
use axum::response::Response;
use axum::routing::any;

use super::InjectionStore;
use crate::router::call_router_with_new_request;

/// Wraps a router with request and response injection handling.
pub fn injection_router(inner: Router, store: Arc<InjectionStore>) -> Router {
    Router::new()
        .route("/{*all}", any(process))
        .with_state(InjectionMiddleware { inner, store })
}

async fn process(State(mut state): State<InjectionMiddleware>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    // Axum strips the mounting prefix from `request.uri()` inside a nested router and preserves
    // the client-visible URI in `OriginalUri`. Injection selectors describe external paths, so
    // prefer the original path when the two differ and fall back for non-nested routers.
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map_or_else(|| request.uri().path(), |uri| uri.0.path())
        .to_string();
    if let Some(response) = state.store.pre_handle(&method, &path).await {
        return response;
    }

    let response = call_router_with_new_request(&mut state.inner, request).await;
    state.store.post_handle(&path, response).await
}

#[derive(Clone)]
struct InjectionMiddleware {
    inner: Router,
    store: Arc<InjectionStore>,
}
