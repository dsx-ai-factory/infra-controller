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

//! `nv show system`: identity, health, version and the last reboot.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use chrono::Utc;
use serde_json::{Map, Value, json};

use super::NvueState;
use crate::json::JsonExt;

/// How NVOS prints a timestamp.
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// The base OS, kernel, ONIE and build date of the NVOS release every
/// profile runs.
const BASE_OS: &str = "NVOS Debian GNU/Linux 12 (bookworm)";
const KERNEL: &str = "6.1.0-22-2-amd64";
const ONIE: &str = "2023.11-5.3.0012-115200";
const BUILD_DATE: &str = "Thu Jul  9 16:57:29 UTC 2026";

/// `GET /nvue_v1/system`. NVOS names the product `nvos`; the system type is
/// under `/nvue_v1/platform`.
pub(super) async fn show(State(state): State<Arc<NvueState>>) -> Response {
    let config = &state.config;
    json!({
        "contact": null,
        "date-time": {
            "local-time": Utc::now().format(TIME_FORMAT).to_string(),
            "timezone": "Etc/UTC",
        },
        "health": { "status": state.health().status.as_str() },
        "hostname": config.hostname,
        "location": null,
        "product-name": "nvos",
        "product-release": config.product_release,
        "status": "System is ready",
        "uptime": state.uptime().as_secs(),
        "version": { "product-release": config.product_release },
    })
    .into_ok_response()
}

/// `GET /nvue_v1/system/version`.
pub(super) async fn version(State(state): State<Arc<NvueState>>) -> Response {
    let release = state.config.product_release;
    json!({
        "base-os": BASE_OS,
        "image": {
            "build-date": BUILD_DATE,
            "build-id": format!("nvos-{release}"),
        },
        "kernel": KERNEL,
        "onie": ONIE,
        "product-release": release,
    })
    .into_ok_response()
}

/// `GET /nvue_v1/system/health`.
pub(super) async fn health(State(state): State<Arc<NvueState>>) -> Response {
    let health = state.health();
    let issues: Map<String, Value> = health
        .issues
        .iter()
        .map(|(component, issue)| (component.clone(), json!({ "issue": issue })))
        .collect();
    json!({
        "issues": issues,
        "status": health.status.as_str(),
        "status-led": health.status.led(),
    })
    .into_ok_response()
}

/// `GET /nvue_v1/system/reboot/reason`: the operator reboot that booted the
/// simulated NVOS, which is when its state was created.
pub(super) async fn reboot_reason(State(state): State<Arc<NvueState>>) -> Response {
    json!({
        "gentime": state.booted_at.format(TIME_FORMAT).to_string(),
        "reason": "reboot",
        "user": "admin",
    })
    .into_ok_response()
}
