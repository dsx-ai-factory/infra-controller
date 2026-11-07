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

//! State Controller implementation for Switches.

pub mod bom_validating;
pub mod certificate;
pub mod configuring;
pub mod context;
pub mod created;
mod decommissioning;
pub mod deleting;
pub mod endpoint;
pub mod error_state;
pub mod fetch_info;
pub mod handler;
pub mod initializing;
pub mod io;
pub mod maintenance;
pub mod metrics;
pub mod nvos_password_rotation;
pub mod ready;
pub mod reprovisioning;
pub mod rotating_bmc;
pub mod validating;
pub mod write_ops;
