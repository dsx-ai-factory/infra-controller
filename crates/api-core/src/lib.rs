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
//! The Carbide API server implementation.
//!
//! This crate holds the [`Api`] service (the gRPC `Forge` implementation and all
//! the business logic behind it), plus the request listener and runtime service
//! assembly. The `carbide-api` crate owns process bootstrap, while the admin web
//! UI lives in the separate `carbide-api-web` crate.

// It's too cumbersome for tests to adhere to these, which are less important in testing anyway.
// Keep this exemption limited to test builds so enabling `test-support` under `--all-features`
// does not hide violations in production modules.
#![cfg_attr(
    any(test, feature = "test-support"),
    allow(txn_held_across_await, txn_without_commit)
)]

// NOTE on pub vs non-pub mods:
//
// Most of this crate is private ("mod", not "pub mod"), so that we get working dead-code detection:
// If modules here are public, rust will not find dead code for anything marked `pub` within the
// module. We make public only the minimum surface needed by our two dependents:
//   - the `carbide-api` composition crate, which needs the hidden bootstrap interface,
//     configuration loading, and the admin UI route builder; and
//   - the `carbide-api-web` crate, which needs the `Api` service type and a few shared types
//     (`AuthContext`, `CarbideError`, `LogStream`/`LogLine`, `NUM_REQUIRED_APPROVALS`, and the
//     `cfg::file` config types).
// Anything that doesn't need to cross a crate boundary should stay private.

mod admission;
mod api;
mod attestation;
mod auth;
mod bmc_proxy;
#[doc(hidden)]
pub mod bootstrap;
pub mod cfg;
mod compat;
mod credentials;
mod db_init;
mod dhcp;
mod dpa;
mod dpf_services;
mod dynamic_settings;
mod errors;
mod ethernet_virtualization;
mod handlers;
mod instance;
mod ipxe;
mod listener;
mod logging;
mod machine_identity;
mod machine_update_manager;
mod machine_validation;
mod measured_boot;
mod mqtt_state_change_hook;
mod network_segment;
mod node_auth;
mod scout_stream;
pub mod secrets;
mod setup;
mod storage;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(test)]
mod tests;

use std::sync::OnceLock;

// Allow carbide_macros::sqlx_test to be referred as #[crate::sqlx_test]
#[cfg(test)]
pub(crate) use carbide_macros::sqlx_test;
// TODO: temporary while migrating db to its own crate
pub(crate) use db::DatabaseError;
// Save typing
pub(crate) use errors::CarbideResult;

pub use crate::admission::AdminAdmissionControl;
pub use crate::api::{Api, DefaultCredential};
pub use crate::auth::AuthContext;
use crate::cfg::file::ToolLink;
pub use crate::dynamic_settings::DynamicSettings;
pub use crate::errors::CarbideError;
pub use crate::handlers::redfish::NUM_REQUIRED_APPROVALS;
pub use crate::listener::AdminUiRoutesBuilder;
pub use crate::logging::stream::{LogLine, LogStream};

/// Process-global tool list rendered in the admin web UI's "Tools" sidebar.
///
/// This lives here (rather than in `carbide-api-web`) because it is derived from
/// the parsed [`cfg::file::CarbideConfig`] during server startup, before the web
/// layer exists. The web layer reads it back via [`configured_tools`]. It is a
/// write-once `OnceLock` because `base.html` is rendered by more than 70 page
/// structs, and threading the list through all of them (and every test fixture)
/// is far more invasive than a global read.
static TOOLS: OnceLock<Vec<ToolLink>> = OnceLock::new();

/// Initialize the global tool list. Call once during startup before serving any
/// web requests. Subsequent calls are ignored.
pub(crate) fn init_tools(tools: Vec<ToolLink>) {
    let _ = TOOLS.set(tools);
}

/// The configured external tool links, for the admin UI's "Tools" sidebar.
///
/// Empty when no tools are configured or when [`init_tools`] has not been called
/// (e.g. unit tests).
pub fn configured_tools() -> &'static [ToolLink] {
    TOOLS.get().map(Vec::as_slice).unwrap_or(&[])
}

/// Process-global site name shown in the admin web UI's sidebar header
/// ("NICo • <site>"). Same write-once rationale as [`TOOLS`].
static SITE_NAME: OnceLock<Option<String>> = OnceLock::new();

/// Initialize the global site name from the config's `sitename` field. Call
/// once during startup before serving any web requests. Subsequent calls are
/// ignored.
pub(crate) fn init_site_name(site_name: Option<String>) {
    let _ = SITE_NAME.set(site_name);
}

/// The configured site name, for the admin UI's sidebar header. `None` when
/// the config doesn't set `sitename` or when [`init_site_name`] has not been
/// called (e.g. unit tests).
pub fn configured_site_name() -> Option<&'static str> {
    SITE_NAME.get().and_then(|name| name.as_deref())
}

/// Process-global URL template for the "Logs" link on machine and endpoint
/// detail pages
static LOGS_LINK_TEMPLATE: OnceLock<String> = OnceLock::new();

/// Initialize the global logs link template. Call once during startup before
/// serving any web requests. Subsequent calls are ignored.
pub(crate) fn init_logs_link_template(template: String) {
    let _ = LOGS_LINK_TEMPLATE.set(template);
}

/// The configured URL template for the "Logs" link on machine and endpoint
/// detail pages. The placeholder `{search}` should be replaced with the
/// machine ID or BMC IP by the caller.
///
/// Empty when not configured or when [`init_logs_link_template`] has not been
/// called (e.g. unit tests).
pub fn configured_logs_link_template() -> &'static str {
    LOGS_LINK_TEMPLATE.get().map(String::as_str).unwrap_or("")
}
