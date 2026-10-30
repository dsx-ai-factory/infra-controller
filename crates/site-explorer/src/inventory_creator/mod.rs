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

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use component_manager::MachineInfoProvider;
use mac_address::MacAddress;
use model::rack_type::RackProfileConfig;
use model::resource_pool::common::CommonPools;
use sqlx::PgPool;

use crate::errors::SiteExplorerResult;
use crate::explored_endpoint_index::ExploredEndpointIndex;
use crate::metrics::SiteExplorationMetrics;
use crate::{IdentifiedManagedHost, rack_order};

mod machine_creator;
mod power_shelf_creator;
mod switch_creator;

pub(super) use machine_creator::ConfiguredIngestionCandidate;
pub use machine_creator::MachineCreator;
pub use power_shelf_creator::PowerShelfCreator;
pub use switch_creator::SwitchCreator;

/// Settings used to create inventory from exploration results.
#[derive(Clone)]
pub struct Config {
    /// Whether machine creation is enabled at runtime.
    pub create_machines: Arc<AtomicBool>,
    /// Maximum number of machines created in one run.
    pub machines_created_per_run: u64,
    /// Managed hosts created at the same time within one run, before the
    /// clamp below the admin-lock admission permits.
    pub machine_creation_concurrency: u64,
    /// How often the switch and power shelf pass runs while hosts are still
    /// being created; zero disables the extra passes.
    pub switch_and_power_shelf_pass_interval: Duration,
    /// Whether power shelf creation is enabled at runtime.
    pub create_power_shelves: Arc<AtomicBool>,
    /// Maximum number of power shelves created in one run.
    pub power_shelves_created_per_run: u64,
    /// Whether switch creation is enabled at runtime.
    pub create_switches: Arc<AtomicBool>,
    /// Maximum number of switches created in one run.
    pub switches_created_per_run: u64,
    /// Whether host interfaces can use IPv6-only admin segments.
    pub admin_fnn_enabled: bool,
    /// How long retained boot interfaces remain applicable; `None` means forever.
    pub retained_boot_interface_window: Option<chrono::Duration>,
}

/// Creates inventory from identified hosts and explored endpoints.
pub(super) struct InventoryCreator {
    config: Config,
    machine_creator: MachineCreator,
    power_shelf_creator: PowerShelfCreator,
    switch_creator: SwitchCreator,
}

impl InventoryCreator {
    pub(super) fn new(
        database_connection: PgPool,
        config: Config,
        common_pools: Arc<CommonPools>,
        rack_profiles: Arc<RackProfileConfig>,
        machine_info_provider: Option<Arc<dyn MachineInfoProvider>>,
        dpf_enabled_at_site: bool,
    ) -> Self {
        Self {
            machine_creator: MachineCreator::new(
                database_connection.clone(),
                config.clone(),
                common_pools,
                rack_profiles,
                machine_info_provider,
                dpf_enabled_at_site,
            ),
            power_shelf_creator: PowerShelfCreator::new(
                database_connection.clone(),
                config.clone(),
            ),
            switch_creator: SwitchCreator::new(database_connection, config.clone()),
            config,
        }
    }

    /// Creates machines, power shelves, and switches in their existing order.
    pub(super) async fn create_inventory(
        &self,
        metrics: &mut SiteExplorationMetrics,
        identified_hosts: &mut [IdentifiedManagedHost],
        expected_endpoint_index: &ExploredEndpointIndex,
        already_ingested_bmc_ips: &HashSet<IpAddr>,
        explored_this_run: &HashSet<IpAddr>,
        suppressed_bmc_macs: &HashSet<MacAddress>,
    ) -> SiteExplorerResult<()> {
        if self.config.create_machines.load(Ordering::Relaxed) {
            // The identify phase hands the hosts over in hash order. Creating
            // them in rack order lets each run complete the racks it can.
            let rack_host_fill = rack_order::order_hosts_by_rack_completion(
                identified_hosts,
                |bmc_ip| {
                    let expected = expected_endpoint_index.matched_expected_machine(bmc_ip)?;
                    Some((&expected.bmc_mac_address, expected.data.rack_id.as_ref()?))
                },
                &expected_endpoint_index.expected_hosts_per_rack(),
                already_ingested_bmc_ips,
            );
            metrics.rack_host_fill = Some(rack_host_fill);
            tracing::info!(
                expected_rack_count = rack_host_fill.expected,
                all_hosts_ingested_rack_count = rack_host_fill.all_hosts_ingested,
                completable_this_run_rack_count = rack_host_fill.completable_this_run,
                "Ordered the identified hosts by rack completion"
            );

            // The switch and power shelf pass runs between host creations
            // whenever the run pauses for it, so devices that completed
            // preingestion do not wait for the end of a long creation phase.
            let mut creation = self.machine_creator.creation_run(
                identified_hosts,
                expected_endpoint_index,
                already_ingested_bmc_ips,
                explored_this_run,
            );
            let mut create_machines_latency = Duration::ZERO;
            loop {
                let start_create_hosts = Instant::now();
                let progress = creation.create_hosts(metrics).await;
                create_machines_latency += start_create_hosts.elapsed();
                match progress {
                    machine_creator::CreationProgress::Done => break,
                    machine_creator::CreationProgress::PassDue => {
                        tracing::info!(
                            created_machine_count = metrics.created_machines,
                            "Running the switch and power shelf pass between host creations"
                        );
                        if let Err(error) = self
                            .create_switches_and_power_shelves(
                                metrics,
                                expected_endpoint_index,
                                suppressed_bmc_macs,
                            )
                            .await
                        {
                            tracing::warn!(
                                %error,
                                "Switch and power shelf pass between host creations failed; the end-of-run pass retries"
                            );
                        }
                    }
                }
            }
            metrics.create_machines_latency = Some(create_machines_latency);
            metrics.record_phase_latency("create_machines", create_machines_latency);
        } else {
            // `create_machines` revisits ingested hosts explored this run on
            // its steady-state path; with it off, revisit them here so a
            // pending admin address repair is still made.
            let reconcile_admin_addresses_start = Instant::now();
            self.machine_creator
                .reconcile_explored_host_admin_addresses(
                    identified_hosts,
                    already_ingested_bmc_ips,
                    explored_this_run,
                )
                .await;
            metrics.record_phase_latency(
                "reconcile_explored_host_admin_addresses",
                reconcile_admin_addresses_start.elapsed(),
            );
        }

        let reconcile_desired_boot_interfaces_start = Instant::now();
        let reconcile_desired_boot_interfaces_res = self
            .machine_creator
            .reconcile_desired_boot_interfaces()
            .await;
        metrics.record_phase_latency(
            "reconcile_desired_boot_interfaces",
            reconcile_desired_boot_interfaces_start.elapsed(),
        );
        if let Err(error) = reconcile_desired_boot_interfaces_res {
            tracing::warn!(
                %error,
                "Desired boot-interface reconciliation pass failed; a later Site Explorer run will retry"
            );
        }

        self.create_switches_and_power_shelves(
            metrics,
            expected_endpoint_index,
            suppressed_bmc_macs,
        )
        .await?;

        Ok(())
    }

    /// Identifies and creates the power shelves and switches whose endpoints
    /// have completed preingestion. Runs once at the end of every run and,
    /// while hosts are still being created, each time
    /// `switch_and_power_shelf_pass_interval` has elapsed since the run
    /// started or since the previous pass and the in-flight host creations
    /// have finished, so a switch or shelf that completes preingestion during
    /// a long host creation phase waits about one interval instead of for the
    /// end of the phase. The interval is checked when a host attempt completes
    /// and restarts when the pass becomes due, before the pass runs. The
    /// per-run latency totals and the per-run caps accumulate over the passes
    /// of one run.
    async fn create_switches_and_power_shelves(
        &self,
        metrics: &mut SiteExplorationMetrics,
        expected_endpoint_index: &ExploredEndpointIndex,
        suppressed_bmc_macs: &HashSet<MacAddress>,
    ) -> SiteExplorerResult<()> {
        let identify_power_shelves_to_ingest_start = Instant::now();
        let explored_power_shelves = self
            .power_shelf_creator
            .identify_power_shelves_to_ingest(expected_endpoint_index, suppressed_bmc_macs)
            .await?;
        metrics.record_phase_latency(
            "identify_power_shelves_to_ingest",
            identify_power_shelves_to_ingest_start.elapsed(),
        );

        if self.config.create_power_shelves.load(Ordering::Relaxed) {
            let start_create_power_shelves = Instant::now();
            let create_power_shelves_res = self
                .power_shelf_creator
                .create_power_shelves(metrics, explored_power_shelves, expected_endpoint_index)
                .await;
            let create_power_shelves_latency = start_create_power_shelves.elapsed();
            metrics.create_power_shelves_latency = Some(
                metrics.create_power_shelves_latency.unwrap_or_default()
                    + create_power_shelves_latency,
            );
            metrics.record_phase_latency("create_power_shelves", create_power_shelves_latency);
            create_power_shelves_res?;
        } else if !explored_power_shelves.is_empty() {
            tracing::info!(
                power_shelf_count = explored_power_shelves.len(),
                "Identified power shelves during exploration but create_power_shelves=false; skipping PowerShelf creation. \
                 Set [site_explorer] create_power_shelves=true and declare matching expected_power_shelves records to ingest them."
            );
        }

        let identify_switches_to_ingest_start = Instant::now();
        let explored_switches = self
            .switch_creator
            .identify_switches_to_ingest(expected_endpoint_index, suppressed_bmc_macs)
            .await?;
        metrics.record_phase_latency(
            "identify_switches_to_ingest",
            identify_switches_to_ingest_start.elapsed(),
        );

        if self.config.create_switches.load(Ordering::Relaxed) {
            let start_create_switches = Instant::now();
            let create_switches_res = self
                .switch_creator
                .create_switches(metrics, &explored_switches, expected_endpoint_index)
                .await;
            let create_switches_latency = start_create_switches.elapsed();
            metrics.create_switches_latency =
                Some(metrics.create_switches_latency.unwrap_or_default() + create_switches_latency);
            metrics.record_phase_latency("create_switches", create_switches_latency);
            create_switches_res?;
        } else if !explored_switches.is_empty() {
            tracing::info!(
                switch_count = explored_switches.len(),
                "Identified switches during exploration but create_switches=false; skipping Switch creation. \
                 Set [site_explorer] create_switches=true and declare matching expected_switches records to ingest them."
            );
        }

        Ok(())
    }
}
