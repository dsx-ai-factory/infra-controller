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

use model::resource_pool::{ResourcePoolSnapshot, ResourcePoolStats};

use crate as rpc;

impl From<ResourcePoolStats> for rpc::forge::ResourcePoolStats {
    fn from(stats: ResourcePoolStats) -> Self {
        Self {
            used: stats.used as u64,
            free: stats.free as u64,
            auto_assign_used: stats.auto_assign_used as u64,
            auto_assign_free: stats.auto_assign_free as u64,
            non_auto_assign_used: stats.non_auto_assign_used as u64,
            non_auto_assign_free: stats.non_auto_assign_free as u64,
        }
    }
}

impl From<ResourcePoolSnapshot> for rpc::forge::ResourcePool {
    fn from(rp: ResourcePoolSnapshot) -> Self {
        rpc::forge::ResourcePool {
            name: rp.name,
            min: rp.min,
            max: rp.max,
            total: (rp.stats.free + rp.stats.used) as u64,
            allocated: rp.stats.used as u64,
        }
    }
}
