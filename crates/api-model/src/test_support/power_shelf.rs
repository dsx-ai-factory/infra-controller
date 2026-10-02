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

//! Power shelf model fixtures.

use crate::power_shelf::PowerShelfConfig;

/// Returns a default power shelf config with a unique name.
pub fn default_config() -> PowerShelfConfig {
    power_shelf_config(&format!("Test Power Shelf {}", uuid::Uuid::new_v4()))
}

/// Returns a power shelf config with capacity 100 and voltage 240.
pub fn power_shelf_config(name: &str) -> PowerShelfConfig {
    PowerShelfConfig {
        name: name.to_string(),
        capacity: Some(100),
        voltage: Some(240),
    }
}
