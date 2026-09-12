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

//! Mounting the mock's gRPC services onto an `axum` router.

use std::sync::Arc;

use axum::Router;
use librms::protos::rack_manager::rack_manager_server::RackManagerServer;
use librms::protos::rack_manager_v2::rack_manager_v2_server::RackManagerV2Server;
use tonic::server::NamedService;

use crate::RmsMock;

/// Build a router serving both RMS services.
///
/// The services are mounted as plain `tower` services on an ordinary router
/// rather than through `tonic::service::Routes`, because `Routes` installs its
/// own catch-all fallback; merged into a host router that would replace the
/// host's fallback with a gRPC `UNIMPLEMENTED`.
///
/// Route paths are derived from each service's generated `NamedService::NAME`
/// so that a `librms` bump which renames a service is a compile-time change
/// here rather than a silent 404 at runtime.
pub fn router(mock: Arc<RmsMock>) -> Router {
    let v1_path = format!(
        "/{}/{{*rpc}}",
        <RackManagerServer<RmsMock> as NamedService>::NAME
    );
    let v2_path = format!(
        "/{}/{{*rpc}}",
        <RackManagerV2Server<RmsMock> as NamedService>::NAME
    );

    Router::new()
        .route_service(&v1_path, RackManagerServer::from_arc(mock.clone()))
        .route_service(&v2_path, RackManagerV2Server::from_arc(mock))
}
