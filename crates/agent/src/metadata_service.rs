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

use crate::instance_metadata_endpoint::{InstanceMetadataRouterStateImpl, get_fmds_router};
use crate::instrumentation::{AgentMetricsState, WithTracingLayer};

pub(super) fn spawn_metadata_service(
    metadata_service_address: String,
    metrics_state: Arc<AgentMetricsState>,
    state: Arc<InstanceMetadataRouterStateImpl>,
) -> Result<(), Box<dyn std::error::Error>> {
    let instance_metadata_state = state;

    start_server(
        metadata_service_address,
        Router::new()
            .nest(
                "/latest",
                get_fmds_router(instance_metadata_state.clone())
                    .with_tracing_layer(metrics_state.clone()),
            )
            .nest(
                "/2009-04-04",
                get_fmds_router(instance_metadata_state).with_tracing_layer(metrics_state),
            ),
    )
    .expect("metadata server panicked");

    Ok(())
}

/// Spawns a background task to run an axum server listening on given socket, and returns.
fn start_server(address: String, router: Router) -> Result<(), Box<dyn std::error::Error>> {
    let addr: std::net::SocketAddr = address.parse()?;
    let server = axum_server::Server::bind(addr);

    tokio::spawn(async move {
        if let Err(err) = server.serve(router.into_make_service()).await {
            eprintln!("Error while serving: {err}");
        }
    });

    Ok(())
}
