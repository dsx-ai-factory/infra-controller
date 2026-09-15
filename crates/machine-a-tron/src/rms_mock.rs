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

//! Hosting for the RMS mock on machine-a-tron's own listener.
//!
//! RMS is reached on the same HTTPS listener that serves the simulated BMCs,
//! rather than on a port of its own, so that it inherits that listener's TLS
//! material, HTTP/2 support and lifecycle. gRPC needs HTTP/2, which the
//! listener already negotiates over ALPN for its existing traffic.

use std::sync::Arc;

use axum::Router;
use machine_a_tron::ControlState;
use rms_mock::{RmsMock, RmsMockConfig};

pub(super) struct HostedRmsMock {
    mock: Arc<RmsMock>,
}

impl HostedRmsMock {
    /// Build the mock.
    ///
    /// There is no enable flag and no failure path: the services are always
    /// mounted, and a NICo that is not configured to use RMS simply never
    /// calls them.
    pub(super) fn start(config: RmsMockConfig, control_state: &ControlState) -> Self {
        tracing::info!("Mounting the RMS mock on the bmc-mock listener");
        // The control state is the mock's window onto the simulated
        // hardware; RMS keeps no inventory of its own.
        Self {
            mock: Arc::new(RmsMock::new(Arc::new(control_state.clone()), config)),
        }
    }

    pub(super) fn router(&self) -> Router {
        rms_mock::router(self.mock.clone())
    }
}
