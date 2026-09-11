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

//! `RackManagerV2` service implementation.
//!
//! V2 is a separate gRPC service with a single method. Note that V1 declares a
//! method of the same name taking different message types; keeping the two
//! impls in separate files makes it hard to reach for the wrong one.

use librms::protos::rack_manager_v2::rack_manager_v2_server::RackManagerV2;

use crate::{RmsMock, rms_v2};

#[tonic::async_trait]
impl RackManagerV2 for RmsMock {
    async fn configure_scale_up_fabric_manager(
        &self,
        _request: tonic::Request<rms_v2::ConfigureScaleUpFabricManagerRequest>,
    ) -> std::result::Result<
        tonic::Response<rms_v2::ConfigureScaleUpFabricManagerResponse>,
        tonic::Status,
    > {
        Err(tonic::Status::unimplemented(
            "the machine-a-tron RMS mock does not yet implement configure_scale_up_fabric_manager",
        ))
    }
}
