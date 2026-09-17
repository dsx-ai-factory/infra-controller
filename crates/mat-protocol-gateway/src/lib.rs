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

//! Multi-pod machine-a-tron protocol gateway.
//!
//! The gateway runs beside the Go `mat-k8s-controller` in one pod. The controller owns all
//! Kubernetes access and publishes a versioned list of machine-a-tron base URLs on a pod-local
//! endpoint. The gateway turns that list into UFM inventory sources, hosts the UFM API from
//! `ufm-mock`, learns from each instance's `/racks/status` which racks it simulates, and routes
//! RMS requests to that instance.
//!
//! The process is bound to the source set it started with: the sorted `(name, base_url)` pairs
//! of the controller's list. When the controller publishes a different set the process exits
//! with [`SOURCE_LIST_CHANGED_EXIT_CODE`] so Kubernetes restarts only this container. The
//! controller's generation counter is only a change hint because it restarts from zero with the
//! controller container.

mod config;
mod gateway;
mod health;
mod ownership;
mod rms_client;
mod rms_jobs;
mod rms_proxy;
mod sources;

pub use config::{
    ControllerConfig, GatewayConfig, OwnershipConfig, RmsConfig, SourceClientConfig, TlsConfig,
};
pub use gateway::{
    ExitReason, Gateway, SOURCE_LIST_CHANGED_EXIT_CODE, run, wait_for_source_list,
    watch_source_list,
};
pub use rms_proxy::RMS_VERSION;
pub use sources::{
    Source, SourceList, SourceListCheck, SourceListClient, SourceListError, SourceSetChange,
};
