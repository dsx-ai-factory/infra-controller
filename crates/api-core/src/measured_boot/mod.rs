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

//!
//! Carbide API module specific to measured boot/machine attestation.

pub(crate) mod metrics_collector;
pub(crate) mod rpc;

#[cfg(test)]
mod tests;

// So far this helper is only used by measured boot functions. Similar
// function exist in model: try_convert_vec. Maybe at some point of
// time we need to unify all these conversions in helpers library.
pub(crate) fn convert_vec<T, R>(source: Vec<T>) -> Vec<R>
where
    R: From<T>,
{
    source.into_iter().map(R::from).collect()
}
