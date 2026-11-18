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
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

// RouteServerSourceType exists because route server addresses are
// stored with a source type annotating where the address was sourced
// from, currently either the Carbide config file (ConfigFile), or via
// the API (AdminApi). This allows route servers to be independently
// managed by either the config file (update config and restart),
// the API (make forge-admin-cli calls to dynamically update), or
// both. The nice thing is it's entirely up to the site operator
// as to how they want to manage them.
#[derive(Copy, Debug, Eq, Hash, PartialEq, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "route_server_source_type")]
#[sqlx(rename_all = "snake_case")]
pub enum RouteServerSourceType {
    ConfigFile,
    AdminApi,
}

// RouteServer is a sqlx-mapped struct modeling a
// route_servers row in the database, containing the
// IpAddr address and source_type.
#[derive(FromRow, carbide_macros::DbTable)]
#[db_table(name = "route_servers")]
pub struct RouteServer {
    pub address: IpAddr,
    pub source_type: RouteServerSourceType,
}
