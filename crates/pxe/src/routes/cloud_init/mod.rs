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

//! The cloud-init documents this service serves, one sub-prefix per consumer:
//!
//! | Prefix | Consumer | Documents |
//! | :----- | :------- | :-------- |
//! | `dpu/` | A BlueField DPU fetching its kickstart | `user-data` |
//! | `tenant/` | An assigned instance's NoCloud datasource | `user-data`, `meta-data`, `vendor-data`, `network-config` |
//! | `scout/` | A host booting the discovery OS | `user-data`, `meta-data` |
//!
//! Separate prefixes rather than one set of routes that infers the caller,
//! because a machine mid-transition can resolve to more than one consumer. The
//! URL is fixed at boot, so the prefix already says which it is.

use std::collections::HashMap;

use axum::Router;
use carbide_instrument::emit;

use crate::common::AppState;
use crate::metrics::{BootEndpoint, OutcomeReason, PxeCloudInitRequestFailed};

pub(crate) mod dpu;
pub(crate) mod scout;
pub(crate) mod tenant;

/// The generic-failure funnel: the client always receives the same error
/// template, while the caller's `reason` label records what was actually
/// missing.
///
/// The Scout routes do not use this -- the error template would stop NoCloud
/// bringing the datasource up at all, costing the snippets entirely.
fn log_and_generate_generic_error(
    error: String,
    reason: OutcomeReason,
    endpoint: BootEndpoint,
) -> (String, HashMap<String, String>) {
    emit(PxeCloudInitRequestFailed {
        endpoint,
        reason,
        error,
    });
    let mut template_data: HashMap<String, String> = HashMap::new();
    template_data.insert(
        "error".to_string(),
        "An error occurred while rendering the request".to_string(),
    );
    ("error".to_string(), template_data) // Send a generic error back
}

/// Builds the route table for every cloud-init consumer under `path_prefix`.
pub(crate) fn get_router(path_prefix: &str) -> Router<AppState> {
    Router::new()
        .merge(dpu::get_router(&format!("{path_prefix}/dpu")))
        .merge(tenant::get_router(&format!("{path_prefix}/tenant")))
        .merge(scout::get_router(&format!("{path_prefix}/scout")))
}
