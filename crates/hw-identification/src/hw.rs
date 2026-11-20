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

//! Describes what each hardware platform or product family reports in inventory.
//! Every hardware module and its inventory constants are part of the public catalog.
//!
//! Keep hardware facts in the module for the model that reports them. For example,
//! ThinkSystem SR650 V4 reports the exact part number `SN37C16116` for its BlueField-3
//! DPU. A BlueField family can instead describe its part numbers with a slice of
//! prefixes. These patterns describe the values a caller can observe.
//!
//! Put decisions about those observations in the module that consumes them.
//! For host BlueField identification, `host_bluefield::defaults` owns the rule table, rule IDs,
//! eligible inventory sources, match order, and the product assigned by each rule.
//! [`crate::host_bluefield::HostBlueFieldRules`] matches a part number at its inventory source.

pub mod lenovo_thinksystem_sr650_v4;
pub mod lenovo_thinksystem_sr675_v3;
pub mod nvidia_bluefield;
pub mod nvidia_bluefield_2;
pub mod nvidia_bluefield_3;
pub mod nvidia_bluefield_4;
