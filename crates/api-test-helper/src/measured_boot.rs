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

//! Measured-boot fixture creation through the authenticated test API client.

use std::net::SocketAddr;

use eyre::ContextCompat;
use rpc::protos::measured_boot::{
    CreateMeasurementBundleRequest, CreateMeasurementSystemProfileRequest, MeasurementBundlePb,
    MeasurementSystemProfilePb,
};

use crate::api_client;

/// Creates a measured-boot system profile on a randomly selected test API server.
pub async fn create_profile(
    carbide_api_addrs: &[SocketAddr],
    request: CreateMeasurementSystemProfileRequest,
) -> eyre::Result<MeasurementSystemProfilePb> {
    api_client::call(
        carbide_api_addrs,
        "CreateMeasurementSystemProfile",
        |mut client| async move { client.create_measurement_system_profile(request).await },
    )
    .await?
    .system_profile
    .context("measured-boot profile creation returned no profile")
}

/// Creates a measured-boot bundle on a randomly selected test API server.
pub async fn create_bundle(
    carbide_api_addrs: &[SocketAddr],
    request: CreateMeasurementBundleRequest,
) -> eyre::Result<MeasurementBundlePb> {
    api_client::call(
        carbide_api_addrs,
        "CreateMeasurementBundle",
        |mut client| async move { client.create_measurement_bundle(request).await },
    )
    .await?
    .bundle
    .context("measured-boot bundle creation returned no bundle")
}
