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
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use carbide_axum_utils::router::call_router_with_new_request;
use tracing::instrument;

use crate::Callbacks;
use crate::availability::BmcAvailabilityState;
use crate::injection::InjectionStore;

pub(super) fn append<C: Callbacks>(
    mat_host_id: String,
    router: Router,
    injection: Arc<InjectionStore>,
    availability: Option<Arc<BmcAvailabilityState>>,
    callbacks: Arc<C>,
) -> Router {
    Router::new()
        .route("/{*all}", any(process::<C>))
        .with_state(Arc::new(Middleware {
            mat_host_id,
            inner: router,
            injection,
            availability,
            callbacks,
        }))
}

#[instrument(skip_all, fields(mat_host_id = %state.mat_host_id))]
async fn process<C: Callbacks>(
    State(state): State<Arc<Middleware<C>>>,
    request: Request<Body>,
) -> Response {
    let is_safe = request.method().is_safe();
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    // BMC self-reset window: while the (simulated) controller reboots,
    // NOTHING answers — before auth, before injection, before routing.
    //
    // A real BMC mid-reset yields transport errors first, then 503s while
    // its web server restarts. We can only produce the latter: all mocks
    // share one TLS listener (authority-multiplexed CombinedServer) with
    // pooled keep-alive connections, so by the time a handler runs it can
    // only return HTTP. Behaviorally equivalent for nico: its readiness
    // probe and error paths collapse transport errors and error statuses
    // into the same retry decision (machine-controller factory_reset.rs,
    // `wait_for_bmc`).
    if let Some(availability) = state.availability.as_ref()
        && availability.is_offline()
    {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    if let Some(short) = state.injection.pre_handle(&method, &path).await {
        return short;
    }

    let response = state.call_inner_router(request).await;
    let response = state.injection.post_handle(&path, response).await;

    if !response.status().is_success() {
        tracing::warn!(
            method = %method,
            path,
            http_status = %response.status(),
            "BMC mock request returned unsuccessful response"
        );
    }
    if !is_safe && response.status().is_success() {
        state.callbacks.state_refresh_indication();
    }
    response
}

struct Middleware<C: Callbacks> {
    mat_host_id: String,
    inner: Router,
    injection: Arc<InjectionStore>,
    availability: Option<Arc<BmcAvailabilityState>>,
    callbacks: Arc<C>,
}

impl<C: Callbacks> Middleware<C> {
    async fn call_inner_router(&self, request: Request<Body>) -> axum::response::Response {
        call_router_with_new_request(&mut self.inner.clone(), request).await
    }
}
