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

//! Mounting the mock's gRPC service onto an `axum` router.

use std::sync::Arc;

use axum::{Router, middleware};
use libnmxc::nmxc_model::nmx_controller_server::NmxControllerServer;
use tonic::server::NamedService;

use crate::NmxcMock;
use crate::authority::record_authority;

/// Build a router serving `NMX_Controller`.
///
/// The service is mounted as a plain `tower` service on an ordinary router
/// rather than through `tonic::service::Routes`, because `Routes` installs its
/// own catch-all fallback; merged into a host router that would replace the
/// host's fallback with a gRPC `UNIMPLEMENTED`.
///
/// The route path is derived from the generated `NamedService::NAME` so that
/// a proto change which renames the service is a compile-time change here
/// rather than a silent 404 at runtime.
///
/// The authority middleware runs ahead of tonic, which drops the request URI
/// before a service method sees it; the host must not rebuild the request
/// between its listener and this router, or the authority is gone.
pub fn router(mock: Arc<NmxcMock>) -> Router {
    let path = format!(
        "/{}/{{*rpc}}",
        <NmxControllerServer<NmxcMock> as NamedService>::NAME
    );

    Router::new()
        .route_service(&path, NmxControllerServer::from_arc(mock))
        .layer(middleware::from_fn(record_authority))
}
