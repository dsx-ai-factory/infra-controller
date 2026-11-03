/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Compiled BMC capability drivers and the platform rules that select them.
//!
//! # Where driver helpers live
//!
//! - Pure functions, which do no I/O, are free functions next to their
//!   callers, such as `bios::support::compare` or `dell::on_reset`.
//! - Standard Redfish resources any capability may reach, such as the selected
//!   system's URI, chassis, and BIOS attribute reads and writes, are methods
//!   of `resources::RedfishResourcesExt` on `OpCx`.
//! - Standard Redfish workflows one capability's drivers share are methods of
//!   that capability's `Redfish<Capability>Ext` trait in its `support.rs`,
//!   such as `power::support::RedfishPowerExt::force_off_and_wait`.
//! - Vendor-specific mechanics are free functions taking `cx` in the vendor's
//!   module, such as `dell::job_outcome(cx, response)` or
//!   `boot_order::lenovo::network_group_first(cx)`.
//! - A helper used by only one module is a private free function there.

mod accounts;
mod attestation;
mod bios;
mod bmc_control;
mod boot_order;
mod console;
mod dell;
mod dpu;
mod drivers;
mod firmware;
mod lockdown;
mod power;
mod resources;
mod rules;
mod secure_boot;
mod selection;
mod storage;
mod super_nic;
#[cfg(test)]
mod test_support;

pub use drivers::{CatalogError, Driver, Drivers, PluginId, PluginIdError, SelectedDrivers};
pub use rules::{built_in_rules, rules_with_overrides};
pub use selection::{
    CapabilitySelection, DriverMap, MatchedRule, ResolvedSelection, Rule, RuleError, Rules,
    SelectionError, SelectionHash,
};
