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

//! Hosting for the NMX-C mock on machine-a-tron's own listener.
//!
//! NMX-C is reached on the same HTTPS listener that serves the simulated BMCs,
//! rather than on a port of its own, so that it inherits that listener's TLS
//! material, HTTP/2 support and lifecycle. NICo addresses a rack's controller
//! at a switch NVOS address; routing every such address to this listener lets
//! the mock tell racks apart by the authority each request carries.

use std::sync::Arc;

use axum::Router;
use machine_a_tron::ControlState;
use nmxc_mock::{NmxcMock, NmxcMockConfig};

pub(super) struct HostedNmxcMock {
    mock: Arc<NmxcMock>,
}

impl HostedNmxcMock {
    /// Build the mock.
    ///
    /// There is no enable flag and no failure path: the service is always
    /// mounted, and a NICo that is not configured for NVLink never calls it.
    pub(super) fn start(config: NmxcMockConfig, control_state: &ControlState) -> Self {
        tracing::info!("Mounting the NMX-C mock on the bmc-mock listener");
        Self {
            mock: Arc::new(NmxcMock::new(Arc::new(control_state.clone()), config)),
        }
    }

    pub(super) fn router(&self) -> Router {
        nmxc_mock::router(self.mock.clone())
    }
}
