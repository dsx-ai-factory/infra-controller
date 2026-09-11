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

//! Redfish resource representations, configured service state, and HTTP protocol behavior.
//!
//! # Module layout
//!
//! Give each modeled `EntityType` from the Redfish CSDL description its own module in
//! `redfish`, using a flat layout such as `computer_system`, `chassis`, and
//! `ethernet_interface`. Do not mirror the resource URI hierarchy with nested modules:
//! an entity type may be exposed beneath several different parent resources.
//! Collection types and their helpers belong in the same module as their member entity
//! type; do not create a separate module for each collection. The shared `collection`
//! module contains generic collection-building support.
//!
//! Follow the established pattern in the other Redfish modules for resource/collection
//! helpers, builders, models, config/state, and route wiring. Inspect the closest existing
//! modules before adding a new entity type and reuse their conventions, subject to the
//! architectural rules below. Vendor-specific extensions use the `oem/<vendor>` namespace.
//!
//! # Boundary with hardware profiles
//!
//! This module owns resource/collection URI construction, wire schemas and builders,
//! request validation, response semantics, resource lookup, and state transitions.
//! Hardware profiles supply concrete resource IDs, inventory, initial values, and supported
//! capabilities through configs. Keep new code independent of `hw` types and `HardwareType`;
//! hardware-to-Redfish conversion belongs in `hw`.
//!
//! A handler must consume the configured resource/state. It must not detect hardware using
//! a vendor, model, serial, resource ID, or URI, build a platform profile, or enable a feature
//! merely because a request arrived. Resource IDs identify configured resources; they are
//! not a mechanism for selecting platform behavior.
//!
//! Vendor-specific wire formats and actions belong in `oem/<vendor>`. Their implementation
//! may use vendor-specific state, provided the feature and its initial configuration are
//! selected before requests are served. Dispatch on that selected state or protocol mode.
//! Prefer mode names describing protocol behavior, such as `BootOrderMode::ViaSettings`,
//! over hardware-model names. A vendor identity alone must not imply all of its capabilities.
//!
//! # Config and state
//!
//! Keep immutable capability/initial-value configuration separate from mutable state.
//! Construct state with `from_config` or `new`, fully initialized for the selected features.
//! Do not require a later platform-specific initialization call. Runtime changes update
//! configured values or transitions, including staged changes applied by explicit events.
//! Optional OEM state is valid; runtime platform recognition is not.
//!
//! For optional collection support, use the following semantics:
//!
//! - `None`: unsupported; omit the parent link and return 404 for the collection and members.
//! - `Some(vec![])`: supported empty collection; advertise it and return an empty collection.
//! - `Some(members)`: supported collection; advertise and serve its configured members.
//!
//! An optional feature should similarly have one config/state source of truth for its link,
//! resource, and actions. A registered route does not make a feature supported: handlers
//! must check capability and resource existence. Do not create state for an unsupported
//! optional feature, or conflate absence with a supported feature whose value is disabled.
//!
//! # Adding a resource or stateful feature
//!
//! 1. Define resource/collection helpers and the wire builder/model in the owning resource
//!    module. Reuse `Resource`, `Collection`, `Builder`, and JSON response helpers.
//! 2. Add the required fields to the owning config. For an optional feature, model support
//!    explicitly, for example with `Option<FeatureConfig>`. Hardware profiles provide values.
//! 3. Add mutable state only when needed. Construct it from the config and keep mutations
//!    in that resource/service's state implementation. Use callbacks for external effects.
//! 4. Wire routes through the owning resource/service's `add_routes`. Handlers look up the
//!    configured parent and feature, return 404 when absent, and execute protocol behavior.
//!    Build links and collection membership from the same config used by those lookups.
//! 5. Enable the feature in the relevant `hw` profiles. Check distinct observable contracts,
//!    including unsupported access and state changes, without duplicating the same case
//!    matrix at every layer.
//!
//! `chassis` demonstrates config-driven inventory, lookup, optional links, and responses;
//! `computer_system` demonstrates explicitly configured boot-order modes. Use those specific
//! patterns rather than assuming every neighboring implementation respects these rules.
//! Existing hardware dependencies or vendor inference are exceptions to refactor separately,
//! not precedents for new code. See `hw/mod.rs` for the platform-side extension template.

pub(crate) mod account_service;
pub(crate) mod assembly;
pub(crate) mod bios;
pub(crate) mod boot_option;
pub(crate) mod chassis;
mod collection;
pub(crate) mod computer_system;
pub(crate) mod ethernet_interface;
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
