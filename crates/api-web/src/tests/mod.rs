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
use carbide_test_harness::prelude::TestHarness;
use hyper::http::Request;
use hyper::http::request::Builder;

use crate::{WebAuthMode, routes_with_auth_mode};

mod component_detail;
pub(super) mod env;
mod explored_endpoint;
mod firmware;
mod health;
mod ipam;
mod managed_host;
mod network_segment;
mod vpc;

fn make_test_app(test_harness: &TestHarness) -> Router {
    let r = routes_with_auth_mode(test_harness.api_arc(), WebAuthMode::None, None).unwrap();
    Router::new().nest_service("/admin", r)
}

/// Builder for admin UI requests (tests explicitly disable in-process auth).
fn web_request_builder() -> Builder {
    Request::builder().header("Host", "with.the.most")
}
