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
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourcePoolDef {
    #[serde(default)]
    pub ranges: Vec<Range>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(rename = "type")]
    pub pool_type: ResourcePoolType,
    #[serde(default)]
    pub delegate_prefix_len: Option<u8>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: String,
    pub end: String,
    #[serde(default = "default_true")]
    pub auto_assign: bool,
}

#[derive(Debug, Deserialize, Serialize, Copy, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResourcePoolType {
    Ipv4,
    Ipv6,
    Ipv6Prefix,
    Integer,
}

fn default_true() -> bool {
    true
}
