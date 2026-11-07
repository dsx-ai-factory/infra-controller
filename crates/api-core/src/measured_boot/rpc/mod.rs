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

/*!
 * The `rpc` module provides the gRPC layer to connect the Carbide
 * API to the underlying measured_boot code (model, interface, dto).
 *
 * This includes handlers for all gRPC calls, for all aspects of
 * measured boot:
 *  - `bundle`: Measurement bundles.
 *  - `journal`: Measurement journals.
 *  - `machine`: Mock machines (will eventually go away).
 *  - `profile`: System profiles.
 *  - `report`: Machine measurement reports.
 *  - `site`: Site management.
 */

pub(crate) mod bundle;
pub(crate) mod journal;
pub(crate) mod machine;
pub(crate) mod profile;
pub(crate) mod report;
pub(crate) mod site;
