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

use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use arc_swap::ArcSwap;
use carbide_utils::HostPortPair;
use carbide_utils::config::{
    as_duration, as_std_duration, deserialize_arc_atomic_bool, serialize_arc_atomic_bool,
};
use chrono::Duration;
use duration_str::{deserialize_duration, deserialize_duration_chrono};
use model::expected_machine::HostDpuPolicy;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// SiteExplorer related configuration for hardware discovery and ingestion.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SiteExplorerConfig {
    /// Whether SiteExplorer is enabled. Dynamically toggleable at runtime via SetDynamicConfig.
    #[serde(
        default = "SiteExplorerConfig::default_enabled",
        deserialize_with = "deserialize_arc_atomic_bool",
        serialize_with = "serialize_arc_atomic_bool"
    )]
    pub enabled: Arc<AtomicBool>,
    /// How long a retained boot interface pair stays applicable (`None` =
    /// forever). Deliberately not part of the `[site_explorer]` section
    /// (serde skips it): setup copies the top-level
    /// `retained_boot_interface_window` here so site-explorer's ingest paths
    /// honor the same knob as DHCP.
    #[serde(skip)]
    pub retained_boot_interface_window: Option<chrono::Duration>,
    /// The interval at which site explorer runs.
    #[serde(
        default = "SiteExplorerConfig::default_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval: std::time::Duration,
    /// The maximum amount of nodes that are explored concurrently.
    #[serde(default = "SiteExplorerConfig::default_concurrent_explorations")]
    pub concurrent_explorations: u64,
    /// How many routine (non-requested) endpoints should be explored in a single run.
    /// This bounds only the background refresh work: previously unseen endpoints
    /// and stale endpoints whose reports we want to update. Endpoints with the
    /// `exploration_requested` flag set are always attempted, regardless of this
    /// value, because operators rely on that flag for guaranteed next-tick attempts.
    /// Parallelism for both routine and requested explorations is still bounded by
    /// `concurrent_explorations`.
    /// If the value is set too high the site exploration will take a lot of time
    /// and the exploration report will be updated less frequent. Therefore it
    /// is recommended to reduce `run_interval` instead of increasing
    /// `explorations_per_run`.
    #[serde(default = "SiteExplorerConfig::default_explorations_per_run")]
    pub explorations_per_run: u64,

    /// When false, SiteExplorer skips creating ManagedHost state machines; the DPU agent (scout) must self-register via DiscoverMachine gRPC endpoint with create_machine=true
    #[serde(
        default = "SiteExplorerConfig::default_create_machines",
        deserialize_with = "deserialize_arc_atomic_bool",
        serialize_with = "serialize_arc_atomic_bool"
    )]
    pub create_machines: Arc<AtomicBool>,

    /// How many ManagedHosts should be created in a single run.
    #[serde(default = "SiteExplorerConfig::default_machines_created_per_run")]
    pub machines_created_per_run: u64,

    /// Whether SiteExplorer should rotate/update Switch NVOS admin credentials
    #[serde(
        default = "SiteExplorerConfig::default_rotate_switch_nvos_credentials",
        deserialize_with = "deserialize_arc_atomic_bool",
        serialize_with = "serialize_arc_atomic_bool"
    )]
    pub rotate_switch_nvos_credentials: Arc<AtomicBool>,

    /// DEPRECATED: Use `bmc_proxy` instead.
    /// The IP address to connect to instead of the BMC that made the dhcp request.
    /// This is a debug override and should not be used in production.
    pub override_target_ip: Option<String>,

    /// DEPRECATED: Use `bmc_proxy` instead.
    /// The port to connect to for redfish requests.
    /// This is a debug override and should not be used in production.
    pub override_target_port: Option<u16>,

    /// The host:port to use as a proxy when making BMC calls to all hosts in NICo. This is used
    /// for integration testing, and for local development with machine-a-tron/bmc-mock. Should not
    /// be used in production.
    #[serde(
        default,
        deserialize_with = "deserialize_bmc_proxy",
        serialize_with = "serialize_bmc_proxy"
    )]
    pub bmc_proxy: Arc<ArcSwap<Option<HostPortPair>>>,

    /// If set to `true`, the server will allow changes to the `bmc_proxy` setting at runtime.
    /// Defaults to true if the server is launched with `bmc_proxy` set, false otherwise.
    /// If explicitly set to true or false, that value is respected for the lifetime of the process.
    #[serde(default)]
    pub allow_changing_bmc_proxy: Option<bool>,

    /// Minimum time between consecutive force-restarts or BMC resets initiated by SiteExplorer.
    /// Default is 1 hour.
    #[serde(
        default = "SiteExplorerConfig::default_reset_rate_limit",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub reset_rate_limit: Duration,

    /// When true, non-DPU hosts use the `HostInband` admin network segment type instead of `Admin`.
    #[serde(
        default = "SiteExplorerConfig::default_admin_segment_type_non_dpu",
        deserialize_with = "deserialize_arc_atomic_bool",
        serialize_with = "serialize_arc_atomic_bool"
    )]
    pub admin_segment_type_non_dpu: Arc<AtomicBool>,

    /// Whether SiteExplorer should create PowerShelf objects for explored power
    /// shelves that match an `expected_power_shelves` record. Defaults to true;
    /// set false to disable power shelf ingestion site-wide.
    #[serde(
        default = "SiteExplorerConfig::default_create_power_shelves",
        deserialize_with = "deserialize_arc_atomic_bool",
        serialize_with = "serialize_arc_atomic_bool"
    )]
    pub create_power_shelves: Arc<AtomicBool>,

    /// How many Power Shelves should be created in a single run.
    /// Default is 1.
    #[serde(default = "SiteExplorerConfig::default_power_shelves_created_per_run")]
    pub power_shelves_created_per_run: u64,

    /// Whether SiteExplorer should create Switch objects for explored switches
    /// that match an `expected_switches` record. Defaults to true; set false to
    /// disable switch ingestion site-wide.
    #[serde(
        default = "SiteExplorerConfig::default_create_switches",
        deserialize_with = "deserialize_arc_atomic_bool",
        serialize_with = "serialize_arc_atomic_bool"
    )]
    pub create_switches: Arc<AtomicBool>,

    /// How many Switches should be created in a single run.
    /// Default is 9.
    #[serde(default = "SiteExplorerConfig::default_switches_created_per_run")]
    pub switches_created_per_run: u64,

    /// Site-wide host DPU policy. Per-host `Nic` and `Ignore` policies
    /// override it; per-host `Manage` inherits it for backward compatibility.
    /// `None` falls back to [`HostDpuPolicy::Manage`].
    ///
    /// The legacy `dpu_mode` field and values remain accepted during
    /// deserialization.
    #[serde(default, alias = "dpu_mode")]
    pub dpu_policy: Option<HostDpuPolicy>,

    /// Deprecated compatibility key. This setting is ignored; use `dpu_policy`.
    #[doc(hidden)]
    #[serde(default, rename = "force_dpu_nic_mode", skip_serializing)]
    pub deprecated_force_dpu_nic_mode: Option<bool>,

    /// Controls which Redfish client implementation is used
    /// for hardware discovery (LibRedfish, NvRedfish, or
    /// CompareResult for side-by-side validation).
    #[serde(default = "SiteExplorerConfig::default_explore_mode")]
    pub explore_mode: SiteExplorerExploreMode,
}

impl Default for SiteExplorerConfig {
    fn default() -> Self {
        SiteExplorerConfig {
            enabled: Self::default_enabled(),
            retained_boot_interface_window: None,
            run_interval: Self::default_run_interval(),
            concurrent_explorations: Self::default_concurrent_explorations(),
            explorations_per_run: Self::default_explorations_per_run(),
            create_machines: Self::default_create_machines(),
            machines_created_per_run: Self::default_machines_created_per_run(),
            override_target_ip: None,
            override_target_port: None,
            bmc_proxy: bmc_proxy(None),
            allow_changing_bmc_proxy: None,
            reset_rate_limit: Self::default_reset_rate_limit(),
            admin_segment_type_non_dpu: Self::default_admin_segment_type_non_dpu(),
            create_power_shelves: Self::default_create_power_shelves(),
            power_shelves_created_per_run: Self::default_power_shelves_created_per_run(),
            create_switches: Self::default_create_switches(),
            switches_created_per_run: Self::default_switches_created_per_run(),
            rotate_switch_nvos_credentials: Self::default_rotate_switch_nvos_credentials(),
            dpu_policy: None,
            deprecated_force_dpu_nic_mode: None,
            explore_mode: Self::default_explore_mode(),
        }
    }
}

impl PartialEq for SiteExplorerConfig {
    fn eq(&self, other: &SiteExplorerConfig) -> bool {
        // Destructured without `..` so adding a field to the struct without
        // deciding how it compares here is a compile error, not a silent gap.
        let Self {
            enabled,
            retained_boot_interface_window,
            run_interval,
            concurrent_explorations,
            explorations_per_run,
            create_machines,
            machines_created_per_run,
            rotate_switch_nvos_credentials,
            override_target_ip,
            override_target_port,
            bmc_proxy,
            allow_changing_bmc_proxy,
            reset_rate_limit,
            admin_segment_type_non_dpu,
            create_power_shelves,
            power_shelves_created_per_run,
            create_switches,
            switches_created_per_run,
            dpu_policy,
            deprecated_force_dpu_nic_mode,
            explore_mode,
        } = self;

        enabled.load(AtomicOrdering::Relaxed) == other.enabled.load(AtomicOrdering::Relaxed)
            && *retained_boot_interface_window == other.retained_boot_interface_window
            && *run_interval == other.run_interval
            && *concurrent_explorations == other.concurrent_explorations
            && *explorations_per_run == other.explorations_per_run
            && create_machines.load(AtomicOrdering::Relaxed)
                == other.create_machines.load(AtomicOrdering::Relaxed)
            && *machines_created_per_run == other.machines_created_per_run
            && rotate_switch_nvos_credentials.load(AtomicOrdering::Relaxed)
                == other
                    .rotate_switch_nvos_credentials
                    .load(AtomicOrdering::Relaxed)
            && *override_target_ip == other.override_target_ip
            && *override_target_port == other.override_target_port
            && bmc_proxy.load_full() == other.bmc_proxy.load_full()
            && *allow_changing_bmc_proxy == other.allow_changing_bmc_proxy
            && *reset_rate_limit == other.reset_rate_limit
            && admin_segment_type_non_dpu.load(AtomicOrdering::Relaxed)
                == other
                    .admin_segment_type_non_dpu
                    .load(AtomicOrdering::Relaxed)
            && create_power_shelves.load(AtomicOrdering::Relaxed)
                == other.create_power_shelves.load(AtomicOrdering::Relaxed)
            && *power_shelves_created_per_run == other.power_shelves_created_per_run
            && create_switches.load(AtomicOrdering::Relaxed)
                == other.create_switches.load(AtomicOrdering::Relaxed)
            && *switches_created_per_run == other.switches_created_per_run
            && *dpu_policy == other.dpu_policy
            && *deprecated_force_dpu_nic_mode == other.deprecated_force_dpu_nic_mode
            && *explore_mode == other.explore_mode
    }
}

impl SiteExplorerConfig {
    pub const fn default_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(120)
    }

    pub fn default_enabled() -> Arc<AtomicBool> {
        Arc::new(true.into())
    }

    pub fn default_create_machines() -> Arc<AtomicBool> {
        Arc::new(true.into())
    }

    /// Redfish endpoints probed in parallel per exploration cycle. Bounds the
    /// concurrent load placed on BMCs.
    pub const fn default_concurrent_explorations() -> u64 {
        100
    }

    /// Endpoints selected for exploration per cycle. Identification and machine
    /// creation run only at the end of a completed cycle, so this trades sweep
    /// breadth against how frequently creation runs.
    pub const fn default_explorations_per_run() -> u64 {
        360
    }

    /// Machines created per completed exploration cycle. Caps ingestion rate
    /// independently of how many hosts exploration has identified.
    pub const fn default_machines_created_per_run() -> u64 {
        100
    }

    pub fn default_rotate_switch_nvos_credentials() -> Arc<AtomicBool> {
        Arc::new(false.into())
    }

    pub const fn default_reset_rate_limit() -> Duration {
        Duration::hours(1)
    }

    pub fn default_admin_segment_type_non_dpu() -> Arc<AtomicBool> {
        Arc::new(false.into())
    }

    pub fn default_create_power_shelves() -> Arc<AtomicBool> {
        Arc::new(true.into())
    }

    pub const fn default_power_shelves_created_per_run() -> u64 {
        1
    }

    pub fn default_create_switches() -> Arc<AtomicBool> {
        Arc::new(true.into())
    }

    pub const fn default_switches_created_per_run() -> u64 {
        9
    }

    pub const fn default_explore_mode() -> SiteExplorerExploreMode {
        SiteExplorerExploreMode::NvRedfish
    }
}

pub fn bmc_proxy(s: Option<HostPortPair>) -> Arc<ArcSwap<Option<HostPortPair>>> {
    Arc::new(ArcSwap::new(Arc::new(s)))
}

/// Selects the Redfish client backend used by SiteExplorer
/// for BMC discovery.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum SiteExplorerExploreMode {
    /// Use the libredfish Rust client.
    #[serde(rename = "libredfish")]
    LibRedfish,
    /// Use the NVIDIA-specific Redfish client.
    #[serde(rename = "nv-redfish")]
    NvRedfish,
    /// Run both clients and compare results for validation.
    #[serde(rename = "compare-result")]
    CompareResult,
}

pub fn deserialize_bmc_proxy<'de, D>(
    deserializer: D,
) -> Result<Arc<ArcSwap<Option<HostPortPair>>>, D::Error>
where
    D: Deserializer<'de>,
{
    let p = Option::deserialize(deserializer)?;
    Ok(Arc::new(ArcSwap::new(Arc::new(p))))
}

pub fn serialize_bmc_proxy<S>(
    val: &Arc<ArcSwap<Option<HostPortPair>>>,
    s: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if let Some(val) = val.load().deref().deref() {
        s.serialize_str(val.to_string().as_str())
    } else {
        s.serialize_none()
    }
}
