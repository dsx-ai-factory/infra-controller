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

use model::route_server::{RouteServer, RouteServerSourceType};

use crate as rpc;

// Impl to allow us to convert RouteServer instances
// into gRPC RouteServer messages for returning
// API responses.
impl From<RouteServer> for rpc::forge::RouteServer {
    fn from(rs: RouteServer) -> Self {
        Self {
            address: rs.address.to_string(),
            source_type: rs.source_type as i32,
        }
    }
}

// Impl to allow us to convert RouteServerSourceType instances
// into gRPC RouteServerSourceType messages for returning
// API responses.
impl From<RouteServerSourceType> for rpc::forge::RouteServerSourceType {
    fn from(source_type: RouteServerSourceType) -> Self {
        match source_type {
            RouteServerSourceType::ConfigFile => rpc::forge::RouteServerSourceType::ConfigFile,
            RouteServerSourceType::AdminApi => rpc::forge::RouteServerSourceType::AdminApi,
        }
    }
}

impl From<rpc::forge::RouteServerSourceType> for RouteServerSourceType {
    fn from(source_type: rpc::forge::RouteServerSourceType) -> Self {
        match source_type {
            rpc::forge::RouteServerSourceType::ConfigFile => RouteServerSourceType::ConfigFile,
            rpc::forge::RouteServerSourceType::AdminApi => RouteServerSourceType::AdminApi,
        }
    }
}
