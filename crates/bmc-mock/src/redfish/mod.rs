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

pub(crate) mod account_service;
pub(crate) mod assembly;
pub(crate) mod bios;
pub(crate) mod boot_option;
pub(crate) mod chassis;
mod collection;
pub(crate) mod computer_system;
pub(crate) mod ethernet_interface;
pub(crate) mod event_service;
pub(crate) mod host_interface;
pub(crate) mod leak_detector;
pub(crate) mod log_service;
pub(crate) mod manager;
mod manager_network_protocol;
pub(crate) mod memory;
pub(crate) mod network_adapter;
pub(crate) mod network_device_function;
pub(crate) mod oem;
pub(crate) mod pcie_device;
mod power_subsystem;
pub(crate) mod power_supply;
pub(crate) mod processor;
pub(crate) mod resource;
mod secure_boot;
pub(crate) mod sensor;
pub(crate) mod serial_console;
pub(crate) mod serial_interface;
pub(crate) mod service_root;
pub(crate) mod session_service;
pub(crate) mod software_inventory;
mod storage;
pub(crate) mod task_service;
pub(crate) mod telemetry_service;
mod thermal_subsystem;
pub(crate) mod update_service;
pub(crate) mod virtual_media;

pub(crate) mod expander_router;

pub(super) use collection::Collection;
use resource::Resource;

trait Builder {
    fn maybe_with<T, V>(self, f: fn(Self, &V) -> Self, v: &Option<T>) -> Self
    where
        T: AsRef<V>,
        V: ?Sized,
        Self: Sized,
    {
        if let Some(v) = v {
            f(self, v.as_ref())
        } else {
            self
        }
    }

    fn add_str_field(self, name: &str, value: &str) -> Self
    where
        Self: Sized,
    {
        self.apply_patch(serde_json::json!({ name: value }))
    }

    fn apply_patch(self, patch: serde_json::Value) -> Self;
}
