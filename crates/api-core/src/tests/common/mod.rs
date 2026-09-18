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

//! Contains common functionality between integration tests

pub(in crate::tests) mod api_fixtures;
// Only this crate's own `#[cfg(test)]` test cases use these attestation helpers (the `test-support`
// consumers don't), so gate the module out of test-support-only builds to keep dead-code detection
// honest and avoid pulling in unused imports.
#[cfg(test)]
pub(in crate::tests) mod attestation;
pub(in crate::tests) mod endpoint;
pub(in crate::tests) mod network_segment;
pub(in crate::tests) mod overlay_address;
pub(in crate::tests) mod postgres;
pub(in crate::tests) mod rpc_builder;
pub(in crate::tests) mod sqlx_fixtures;
