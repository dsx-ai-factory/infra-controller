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
use std::collections::{BTreeSet, HashSet};
use std::iter::Peekable;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use carbide_instrument::emit;
use carbide_resource_pool::ResourcePoolRegistry;
use carbide_uuid::machine::{HostMachineId, MachineId, PredictedHostMachineId};
use carbide_uuid::rack::RackId;
use component_manager::MachineInfoProvider;
use db::Transaction;
use db::machine_interface::MachineInterfaces;
use futures_util::StreamExt;
use futures_util::future::join_all;
use futures_util::stream::FuturesUnordered;
use mac_address::MacAddress;
use model::bmc_info::BmcInfo;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::hardware_info::HardwareInfo;
use model::machine::machine_id::host_id_from_dpu_hardware_info;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    AnyMachine, CURRENT_STATE_MODEL_VERSION, ConfigureAstraState, LoadSnapshotOptions,
    MachineInterfaceSnapshot, ManagedHostState, dpf_based_dpu_provisioning_possible,
};
use model::machine_interface_address::MachineInterfaceAssociation;
use model::network_segment::NetworkSegmentType;
use model::predicted_machine_interface::NewPredictedMachineInterface;
use model::rack_type::RackProfileConfig;
use model::resource_pool::common::CommonPools;
use model::site_explorer::{
    EndpointExplorationReport, ExploredDpu, ExploredManagedHost, HostPrimaryInterfaceSelection,
};
use sqlx::{PgConnection, PgPool};
use tokio_util::sync::CancellationToken;

use super::Config;
use crate::IdentifiedManagedHost;
use crate::boot_interfaces::{initial_selection_source, zero_dpu_primary_mac};
use crate::dpu_loopback::{PreparedDpuLoopback, acquire_admin_admission};
use crate::endpoint_exploration_report_ext::EndpointExplorationReportExt;
use crate::errors::{SiteExplorerError, SiteExplorerResult};
use crate::explored_endpoint_index::ExploredEndpointIndex;
use crate::machine_interfaces::MachineInterfacesExt;
use crate::managed_host::{ManagedHost, ZeroDpuHostLookup, find_zero_dpu_host};
use crate::metrics::{
    ManagedHostCreationFinished, ManagedHostCreationStage, ManagedHostCreationStageFinished,
    SiteExplorationMetrics, SiteExplorerHostAdminAddressReconcileFailed,
};

mod host_identification;
pub(crate) use host_identification::ConfiguredIngestionCandidate;

/// Creates machines from site-explorer managed-host reports.
pub struct MachineCreator {
    database_connection: PgPool,
    config: Config,
    /// In-flight creations per `create_machines` call: the configured value,
    /// clamped below the admin-lock admission permits.
    creation_concurrency: usize,
    resource_pool_registry: ResourcePoolRegistry,
    pub(super) preparation_cancel_token: CancellationToken,
    common_pools: Arc<CommonPools>,
    rack_profiles: Arc<RackProfileConfig>,
    machine_info_provider: Option<Arc<dyn MachineInfoProvider>>,
    dpf_enabled_at_site: bool,
}

impl MachineCreator {
    /// Creates a machine creator with an optional machine-information provider.
    /// The registry selects DPU IPv4 backends for preparation outside entity
    /// transactions. Direct calls have an uncancelled owner token; Site Explorer
    /// installs its shutdown token before starting production creation work.
    pub fn new(
        database_connection: PgPool,
        config: Config,
        resource_pool_registry: ResourcePoolRegistry,
        common_pools: Arc<CommonPools>,
        rack_profiles: Arc<RackProfileConfig>,
        machine_info_provider: Option<Arc<dyn MachineInfoProvider>>,
        dpf_enabled_at_site: bool,
    ) -> Self {
        let creation_concurrency = creation_concurrency(
            config.machine_creation_concurrency,
            db::machine_interface::admin_lock_admission_permits(),
        );
        Self {
            database_connection,
            config,
            creation_concurrency,
            resource_pool_registry,
            preparation_cancel_token: CancellationToken::new(),
            common_pools,
            rack_profiles,
            machine_info_provider,
            dpf_enabled_at_site,
        }
    }

    /// Starts creating a new ManagedHost (Host `Machine` and DPU `Machine`
    /// pair) for each ManagedHost that was identified and that doesn't have a
    /// corresponding `Machine` yet. `CreationRun::create_hosts` drives it.
    ///
    /// Hosts whose BMCs are all ingested are left to the independent host reconciler.
    pub(super) fn creation_run<'a>(
        &'a self,
        explored_managed_hosts: &'a mut [IdentifiedManagedHost],
        expected_explored_endpoint_index: &'a ExploredEndpointIndex,
        ingested_bmc_ips: &HashSet<IpAddr>,
    ) -> CreationRun<'a> {
        let mut skipped_existing_hosts = 0usize;
        let mut pending = Vec::new();
        let mut racks_to_ensure = BTreeSet::new();
        for identified in explored_managed_hosts.iter_mut() {
            if host_bmc_ips(&identified.explored_host)
                .all(|bmc_ip| ingested_bmc_ips.contains(bmc_ip))
            {
                skipped_existing_hosts += 1;
                continue;
            }
            let host_bmc_ip = &identified.explored_host.host_bmc_ip;
            if !ingested_bmc_ips.contains(host_bmc_ip)
                && let Some(rack_id) = expected_explored_endpoint_index
                    .matched_expected_machine(host_bmc_ip)
                    .and_then(|expected| expected.data.rack_id.as_ref())
            {
                racks_to_ensure.insert(rack_id.clone());
            }
            pending.push(identified);
        }
        if skipped_existing_hosts > 0 {
            tracing::info!(
                skipped_existing_hosts,
                "Skipped existing hosts during machine creation"
            );
        }

        CreationRun {
            creator: self,
            expected_explored_endpoint_index,
            pending: pending.into_iter().peekable(),
            budget: CreationBudget::new(
                self.config.machines_created_per_run,
                self.creation_concurrency,
            ),
            racks_to_ensure: racks_to_ensure.into_iter().collect(),
            pass_interval: self.config.switch_and_power_shelf_pass_interval,
            last_pass: Instant::now(),
        }
    }

    /// One creation attempt on `identified`, with the Event for its outcome.
    async fn create_identified_host(
        &self,
        identified: &mut IdentifiedManagedHost,
        expected_machine: Option<&ExpectedMachine>,
    ) -> CreationAttempt {
        let IdentifiedManagedHost {
            explored_host,
            report,
            primary_interface_selection,
        } = identified;
        let start = Instant::now();
        let result = self
            .create_managed_host_with_selection(
                explored_host,
                report,
                expected_machine,
                *primary_interface_selection,
                &self.database_connection,
            )
            .await;
        CreationAttempt {
            created: matches!(result, Ok(ManagedHostCreation::Created { .. })),
            event: creation_finished_event(explored_host, result, start.elapsed()),
        }
    }

    /// Creates the rack rows of `rack_ids` in one short transaction before
    /// the first host is dispatched, so concurrent hosts of a new rack find
    /// the row instead of racing to insert it. It runs while nothing is in
    /// flight and takes no segment lock. A failure is logged and not retried
    /// in this run; `ensure_rack_exists` in the host transaction remains the
    /// fallback.
    async fn ensure_racks_before_dispatch(&self, rack_ids: &[RackId]) {
        let result = async {
            let mut txn = Transaction::begin(&self.database_connection).await?;
            for rack_id in rack_ids {
                crate::ensure_rack_exists(&mut txn, rack_id).await?;
            }
            txn.commit().await?;
            SiteExplorerResult::Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(
                rack_count = rack_ids.len(),
                %error,
                "Could not ensure the racks before dispatching their hosts; the host transactions retry"
            );
        }
    }

    /// Creates machines for an identified managed host.
    ///
    /// Existing hosts are left unchanged.
    ///
    /// Returns `true` only when new machines have been created.
    ///
    /// Prepares missing DPU IPv4 loopbacks through the configured backend before
    /// the machine transaction, then attaches them with the network config.
    ///
    /// Requires a matching `expected_machine` entry.
    pub async fn create_managed_host(
        &self,
        explored_host: &ExploredManagedHost,
        report: &mut EndpointExplorationReport,
        expected_machine: Option<&ExpectedMachine>,
        pool: &PgPool,
    ) -> SiteExplorerResult<bool> {
        let creation = self
            .create_managed_host_with_selection(explored_host, report, expected_machine, None, pool)
            .await?;
        Ok(matches!(creation, ManagedHostCreation::Created { .. }))
    }

    /// Creates a managed host with its transient report selection available for
    /// the initial desired boot interface metadata.
    ///
    /// The source is recorded only when its selected MAC becomes the durable
    /// target; target resolution may legitimately choose another interface.
    ///
    /// Records an observation for each attempted stage: admission, the creation
    /// transaction, and after-commit repair when a host was committed. Rejected
    /// preparation reaches none of those stages.
    async fn create_managed_host_with_selection(
        &self,
        explored_host: &ExploredManagedHost,
        report: &mut EndpointExplorationReport,
        expected_machine: Option<&ExpectedMachine>,
        primary_interface_selection: Option<HostPrimaryInterfaceSelection>,
        pool: &PgPool,
    ) -> SiteExplorerResult<ManagedHostCreation> {
        let Some(expected_machine) = expected_machine else {
            tracing::warn!(
                host_bmc_ip_address = %explored_host.host_bmc_ip,
                "Refusing to create managed host, expected machines entry not found"
            );
            return Ok(ManagedHostCreation::SkippedUnexpected);
        };

        let dpu_ids = explored_host
            .dpus
            .iter()
            .map(|dpu| dpu.machine_id_if_valid_report())
            .collect::<Result<Vec<_>, _>>()?;
        // Resolve siblings together so later provider calls cannot age the
        // first reservation past its attachment deadline. `join_all` drains every
        // result and retains report order for Integrated assignment below.
        let preparations = join_all(dpu_ids.into_iter().map(|machine_id| {
            PreparedDpuLoopback::prepare(
                pool,
                &self.resource_pool_registry,
                &self.common_pools,
                machine_id.into(),
                &self.preparation_cancel_token,
            )
        }))
        .await;
        let mut loopbacks = Vec::with_capacity(preparations.len());
        let mut preparation_error = None;
        for preparation in preparations {
            match preparation {
                Ok(prepared) => loopbacks.push(prepared),
                Err(error) if preparation_error.is_none() => preparation_error = Some(error),
                Err(_) => {}
            }
        }

        let creation = async {
            if let Some(error) = preparation_error {
                return Err(error.into());
            }
            let admission_start = Instant::now();
            let has_provider_reservation = loopbacks
                .iter()
                .any(PreparedDpuLoopback::has_provider_reservation);
            let admission =
                acquire_admin_admission(has_provider_reservation, &self.preparation_cancel_token)
                    .await;
            record_creation_stage(
                ManagedHostCreationStage::AdmissionWait,
                admission_start.elapsed(),
            );
            let admission = admission?;
            let transaction_start = Instant::now();
            let creation = self
                .create_managed_host_in_transaction(
                    pool,
                    explored_host,
                    report,
                    expected_machine,
                    primary_interface_selection,
                    &loopbacks,
                )
                .await;
            record_creation_stage(
                ManagedHostCreationStage::Transaction,
                transaction_start.elapsed(),
            );
            Ok::<_, SiteExplorerError>((creation?, admission))
        }
        .await;
        // Attached requests ignore cancellation. This also abandons a
        // preparation a concurrent writer made unnecessary before commit.
        for prepared in &loopbacks {
            if let Err(error) = prepared.cancel(pool).await {
                tracing::error!(machine_id = %prepared.machine_id(), %error, "Could not save DPU loopback cancellation");
            }
        }
        let (creation, _admin_admission) = creation?;

        if let ManagedHostCreation::Created { host_machine_id } = creation {
            // Normalize host admin address ownership now that every DPU-backed
            // host interface is attached and the primary flags are committed.
            let repair_start = Instant::now();
            reconcile_host_admin_addresses_after_commit(pool, &host_machine_id).await;
            record_creation_stage(
                ManagedHostCreationStage::AfterCommitRepair,
                repair_start.elapsed(),
            );
        }

        Ok(creation)
    }

    /// Initializes a new host's desired target in its inventory transaction.
    async fn initialize_desired_boot_interface(
        txn: &mut PgConnection,
        machine_id: &HostMachineId,
        primary_interface_selection: Option<HostPrimaryInterfaceSelection>,
        declared_primary: Option<MacAddress>,
    ) -> SiteExplorerResult<()> {
        let interfaces = MachineInterfaces::load(txn, machine_id).await?;
        if let Some(target) = interfaces.proposed_boot_interface_target(None) {
            if !db::machine::try_lock_group_for_update(txn, machine_id.as_machine_id()).await? {
                return Err(db::DatabaseError::FailedPrecondition(format!(
                    "machine group for {machine_id} changed or is busy; retry",
                ))
                .into());
            }
            let selection_source =
                initial_selection_source(&target, primary_interface_selection, declared_primary);
            db::machine_desired_boot_interface::initialize_if_unset(
                txn,
                machine_id,
                &target,
                selection_source,
            )
            .await?;
        }
        Ok(())
    }

    /// Creates inventory in one transaction, holding admin segment locks in
    /// allocation mode for new DPU-backed hosts. Existing hosts
    /// cause a read-only rollback. After a new host commits, the caller
    /// normalizes its admin addresses.
    async fn create_managed_host_in_transaction(
        &self,
        pool: &PgPool,
        explored_host: &ExploredManagedHost,
        report: &mut EndpointExplorationReport,
        expected_machine: &ExpectedMachine,
        primary_interface_selection: Option<HostPrimaryInterfaceSelection>,
        loopbacks: &[PreparedDpuLoopback<'_>],
    ) -> SiteExplorerResult<ManagedHostCreation> {
        let machine_data = Some(&expected_machine.data);
        let mut managed_host = ManagedHost::init(explored_host);
        let mut txn = Transaction::begin(pool).await?;
        for prepared in loopbacks {
            prepared.check_identity(&mut txn).await?;
        }

        // Zero-dpu case: If the explored host had no DPUs, we can create the machine now
        if managed_host.explored_host.dpus.is_empty() {
            if let Some(machine_id) = self
                .create_zero_dpu_host(
                    &mut txn,
                    &managed_host,
                    report,
                    expected_machine.bmc_mac_address,
                    machine_data,
                )
                .await?
            {
                managed_host.machine_id = Some(machine_id);
            } else {
                txn.rollback().await?;
                return Ok(ManagedHostCreation::SkippedExisting);
            }
            tracing::info!("Created managed_host with zero DPUs");
        }

        let mut dpu_ids = vec![];
        for dpu_report in managed_host.explored_host.dpus.iter() {
            // machine_id_if_valid_report makes sure that all optional fields on dpu_report are
            // actually set (like the machine-id etc) and returns the machine_id if everything
            // is valid.
            let dpu_machine_id = dpu_report.machine_id_if_valid_report()?;
            dpu_ids.push(dpu_machine_id);
        }

        let host_associations = db::machine::DpuHostAssociations::load(&mut txn, &dpu_ids).await?;

        if host_associations.common_host_machine_id()?.is_some() {
            // A stale creation candidate can already have an attached host.
            // Validate the complete DPU set to avoid silently accepting partial
            // or conflicting inventory, then leave existing state unchanged.
            txn.rollback().await?;
            return Ok(ManagedHostCreation::SkippedExisting);
        }

        // Advisory-lock the admin segments, in the allocators' mode, before the
        // first machine-interface row write (`attach_dpu_to_host` allocates
        // each DPU-backed host link), so the allocator's own segment locks
        // re-acquire held keys and this transaction never requests a segment
        // lock after a row lock.
        if !managed_host.explored_host.dpus.is_empty() {
            db::machine_interface::lock_all_admin_segments_for_allocation(txn.as_pgconn()).await?;
            // IPv4 allocation uses shared segment locks; force cleanup needs
            // them exclusively. Recheck after that fence and hold it through
            // creation, so an intervening create/delete cannot become absence.
            for prepared in loopbacks {
                prepared.check_identity(&mut txn).await?;
            }
        }

        for (dpu_report, dpu_machine_id) in
            managed_host.explored_host.dpus.iter().zip(dpu_ids.iter())
        {
            self
                .create_dpu(&mut txn, dpu_report)
                .await?
                .ok_or_else(|| {
                    SiteExplorerError::internal(format!(
                        "BUG: DPU machine {} was already found, but we already verified that it did not exist?",
                        dpu_machine_id,
                    ))
                })?;

            let host_machine_id = self
                .attach_dpu_to_host(&mut txn, &managed_host, dpu_report, machine_data)
                .await?;
            managed_host.machine_id = Some(host_machine_id);
        }

        // Every host link exists before the machine-group configuration updates.
        for prepared in loopbacks {
            prepared.attach(&mut txn, true).await?;
        }

        // Now since all DPUs are created, update host and DPUs state correctly.
        let host_machine_id =
            managed_host
                .machine_id
                .ok_or(SiteExplorerError::internal(format!(
                    "Failed to get machine ID for host: {managed_host:#?}"
                )))?;

        db::machine::update_state(
            &mut txn,
            &host_machine_id,
            &ManagedHostState::ConfigureAstra {
                configure_astra_state: ConfigureAstraState::EnableNics,
            },
        )
        .await?;

        if self
            .dpf_based_dpu_provisioning_possible(&mut txn, &host_machine_id)
            .await?
        {
            db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &host_machine_id).await?;
        }

        let mut rack_profile_id = None;
        if let Some(rack_id) = machine_data.and_then(|d| d.rack_id.as_ref()) {
            tracing::info!(%rack_id, %host_machine_id, "Ensuring rack exists for host machine");
            if let Some(rack) = crate::ensure_rack_exists(&mut txn, rack_id).await? {
                tracing::info!(
                    %rack_id,
                    %host_machine_id,
                    rack = ?rack,
                    "Rack exists"
                );
                rack_profile_id = rack.rack_profile_id;
            }
        }

        // Own a declared integrated boot NIC so a managed-DPU host can boot from it
        // while its DPUs stay managed: the NIC becomes the host's HostInband
        // primary and the DPU admin links go dormant in the reconcile below.
        // Only for hosts with explored DPUs -- a zero-DPU host's NICs (including
        // a declared primary) are already owned by `create_zero_dpu_host`.
        if !managed_host.explored_host.dpus.is_empty() {
            self.own_declared_host_boot_nic(&mut txn, &host_machine_id, report, machine_data)
                .await?;
        }

        Self::initialize_desired_boot_interface(
            &mut txn,
            &host_machine_id,
            primary_interface_selection,
            machine_data.and_then(ExpectedMachineData::declared_primary_mac),
        )
        .await?;

        if let (Some(rack_id), Some(machine_info_provider)) =
            (&expected_machine.data.rack_id, &self.machine_info_provider)
        {
            let Some(rack_profile_id) = rack_profile_id.as_ref() else {
                return Err(SiteExplorerError::InvalidArgument(format!(
                    "rack {rack_id} has no rack_profile_id for RMS slot and tray lookup for host machine {host_machine_id}"
                )));
            };

            let Some(rack_profile) = self.rack_profiles.get(rack_profile_id.as_str()) else {
                return Err(SiteExplorerError::InvalidArgument(format!(
                    "rack profile {rack_profile_id} is not configured for RMS slot and tray lookup for host machine {host_machine_id}"
                )));
            };

            machine_info_provider
                .validate_profile(rack_profile)
                .map_err(|error| SiteExplorerError::InvalidArgument(error.to_string()))?;
        }

        txn.commit().await?;

        Ok(ManagedHostCreation::Created { host_machine_id })
    }

    async fn dpf_based_dpu_provisioning_possible(
        &self,
        txn: &mut PgConnection,
        host_machine_id: &HostMachineId,
    ) -> SiteExplorerResult<bool> {
        let managed_host =
            db::managed_host::load_snapshot(txn, host_machine_id, LoadSnapshotOptions::default())
                .await?
                .ok_or_else(|| {
                    SiteExplorerError::internal(format!(
                        "managed host {host_machine_id} disappeared while being created"
                    ))
                })?;

        Ok(dpf_based_dpu_provisioning_possible(
            &managed_host,
            self.dpf_enabled_at_site,
            false,
        ))
    }

    /// Creates a zero-DPU host when its interface inventory is still unowned.
    /// Returns a machine ID only when a new host has been created.
    async fn create_zero_dpu_host(
        &self,
        txn: &mut PgConnection,
        managed_host: &ManagedHost<'_>,
        report: &mut EndpointExplorationReport,
        bmc_mac_address: MacAddress,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<Option<HostMachineId>> {
        let mac_addresses = report.host_mac_addresses_for_predicted_machine(machine_data);

        if !matches!(
            find_zero_dpu_host(txn, bmc_mac_address, &mac_addresses).await?,
            ZeroDpuHostLookup::NotFound
        ) {
            return Ok(None);
        }

        let declared_primary = machine_data.and_then(|data| data.declared_primary_mac());
        let primary_mac = zero_dpu_primary_mac(
            txn,
            None,
            &mac_addresses,
            declared_primary,
            self.config.retained_boot_interface_window,
        )
        .await?;

        // Resolve the machine ID before borrowing interface IDs from the report.
        let machine_id = match managed_host.machine_id {
            Some(machine_id) => machine_id,
            None => PredictedHostMachineId::try_from(*report.generate_machine_id(true)?.unwrap())?
                .into(),
        };
        let report_boot_interface_ids: Vec<(MacAddress, &str)> = mac_addresses
            .iter()
            .filter_map(|mac| report.find_interface_id_for_mac(*mac).map(|id| (*mac, id)))
            .collect();

        tracing::info!(%machine_id, "Minted predicted host ID for zero-DPU machine");

        let existing_machine = db::machine::find_one(
            &mut *txn,
            &machine_id,
            MachineSearchConfig {
                include_predicted_host: true,
                ..Default::default()
            },
        )
        .await?;

        if let Some(existing_machine) = existing_machine {
            // There's already a machine with this ID, but we already looked above for machines with
            // the same MAC address as this one, so something's weird here. Log this host's mac
            // addresses and the ones from the colliding hosts to help in diagnosis.
            let existing_macs = existing_machine
                .status
                .hardware_info
                .as_ref()
                .map(|hw| hw.all_mac_addresses())
                .unwrap_or_default();
            tracing::warn!(
                %machine_id,
                existing_mac_addresses = ?existing_macs,
                predicted_host_mac_addresses = ?mac_addresses,
                "Predicted host already exists, with different mac addresses from this one. Potentially multiple machines with same serial number?"
            );
            return Ok(None);
        }

        self.create_machine_from_explored_managed_host(
            txn,
            managed_host,
            &machine_id,
            machine_data,
        )
        .await?;

        crate::host_interfaces::reconcile(
            txn,
            &machine_id,
            &mac_addresses,
            &report_boot_interface_ids,
            primary_mac,
        )
        .await?;

        Ok(Some(machine_id))
    }

    /// Owns a declared integrated (non-DPU) host NIC as a managed-DPU host's
    /// HostInband boot interface, so a host with managed DPUs can still boot from
    /// an integrated NIC. The NIC carries `primary` into `machine_interfaces` on
    /// its first DHCP; the DPUs stay explored and linked, and their admin links
    /// go dormant in `reconcile_admin_addresses_for_host` once this NIC is the
    /// primary.
    ///
    /// Mirrors the host-NIC ownership in `create_zero_dpu_host`, but for the
    /// one declared NIC reached from the managed-DPU path. No-op when nothing is
    /// declared, or when the declared NIC is already owned (e.g. a declared DPU
    /// host-PF, which `attach_dpu_to_host` already owns).
    async fn own_declared_host_boot_nic(
        &self,
        txn: &mut PgConnection,
        host_machine_id: &HostMachineId,
        report: &EndpointExplorationReport,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<()> {
        let Some(declared_mac) = machine_data.and_then(|data| data.declared_primary_mac()) else {
            return Ok(());
        };

        if let Some(existing) = db::machine_interface::find_by_mac_address(&mut *txn, declared_mac)
            .await?
            .into_iter()
            .next()
        {
            if let Some(existing_machine_id) = existing.machine_id {
                // Owned by THIS host already (e.g. a declared DPU host-PF): its
                // primary flag is settled by the DPU attach / promotion paths.
                // Owned by a DIFFERENT machine: the declaration names a MAC that
                // already belongs elsewhere -- surface it rather than silently
                // dropping the declared boot NIC (mirrors create_zero_dpu_host).
                if existing_machine_id != host_machine_id.into() {
                    return Err(SiteExplorerError::AlreadyFoundError {
                        kind: "MachineInterface",
                        id: declared_mac.to_string(),
                    });
                }
                return Ok(());
            }
            // The integrated NIC leased before ingestion: adopt its anonymous row
            // as the host's sole primary, demoting the interim DPU primary so the
            // two never collide on `one_primary_interface_per_machine`.
            db::machine_interface::demote_primary_interfaces_for_machine(host_machine_id, txn)
                .await?;
            if !existing.primary_interface {
                db::machine_interface::set_primary_interface(&existing.id, true, txn).await?;
            }
            db::machine_interface::associate_interface_with_machine(
                &existing.id,
                MachineInterfaceAssociation::Machine((*host_machine_id).into()),
                txn,
            )
            .await?;
            tracing::info!(
                declared_mac_address = %declared_mac, %host_machine_id,
                "Adopted declared integrated boot NIC as the managed-DPU host's primary",
            );
            return Ok(());
        }

        // A prediction may already exist from a prior ingestion of this host --
        // don't mint a second one. One owned by a different machine is the same
        // contradiction as the machine_interface case above, so surface it rather
        // than silently dropping the declaration.
        if let Some(existing_prediction) =
            db::predicted_machine_interface::find_by_mac_address(&mut *txn, declared_mac).await?
        {
            if existing_prediction.machine_id != (*host_machine_id).into() {
                return Err(SiteExplorerError::AlreadyFoundError {
                    kind: "PredictedMachineInterface",
                    id: declared_mac.to_string(),
                });
            }
            return Ok(());
        }

        // Not yet leased: mint a HostInband prediction carrying primary, so the
        // NIC is adopted and made primary on its first DHCP (the promotion demotes
        // the interim DPU primary).
        let boot_interface_id = report.find_interface_id_for_mac(declared_mac);
        db::predicted_machine_interface::create(
            NewPredictedMachineInterface {
                machine_id: host_machine_id,
                mac_address: declared_mac,
                expected_network_segment_type: NetworkSegmentType::HostInband,
                boot_interface_id,
                primary_interface: true,
            },
            txn,
        )
        .await?;
        tracing::info!(
            declared_mac_address = %declared_mac, %host_machine_id,
            "Minted HostInband boot-NIC prediction for managed-DPU host's declared integrated primary",
        );
        Ok(())
    }

    // create_dpu does everything needed to create a DPU as part of a newly discovered managed host.
    // If the DPU does not exist in the machines table, the function creates a new DPU machine and
    // configures it appropriately, returning the new `Machine`.
    // If the DPU already exists in the machines table, this is a no-op and returns `None`.
    //
    // `db::machine::create` can persist pool-backed defaults on the new row.
    // The full `network_config` reconciliation and version bump wait until
    // `attach_dpu_to_host` wires the host link in `machine_interfaces`; only
    // then can `try_update_network_config` observe both siblings and keep their
    // versions equal.
    async fn create_dpu(
        &self,
        txn: &mut PgConnection,
        explored_dpu: &ExploredDpu,
    ) -> SiteExplorerResult<Option<AnyMachine>> {
        if let Some(dpu_machine) = self.create_dpu_machine(txn, explored_dpu).await? {
            Self::ensure_dpu_oob_interface_ownership(txn, explored_dpu).await?;
            let dpu_machine_id: &MachineId = explored_dpu.report.machine_id.as_ref().unwrap();
            let dpu_bmc_info = explored_dpu.bmc_info();
            let dpu_hw_info = explored_dpu.hardware_info()?;
            self.update_machine_topology(txn, dpu_machine_id, dpu_bmc_info, dpu_hw_info)
                .await?;
            return Ok(Some(dpu_machine));
        }
        Ok(None)
    }

    // 1) Create a machine for this host using the passed machine_id
    // 2) Update the "machine_topologies" table with the bmc info for this host
    async fn create_machine_from_explored_managed_host(
        &self,
        txn: &mut PgConnection,
        managed_host: &ManagedHost<'_>,
        predicted_machine_id: &MachineId,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<()> {
        _ = db::machine::create(
            txn,
            Some(&self.common_pools),
            predicted_machine_id,
            ManagedHostState::Created,
            machine_data,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        let hardware_info = HardwareInfo::default();
        self.update_machine_topology(
            txn,
            predicted_machine_id,
            managed_host.explored_host.bmc_info(),
            hardware_info,
        )
        .await
    }

    // Ensure the DPU's OOB interface is owned as soon as it appears. If DHCP has already created
    // the interface, associate it directly. Otherwise record a trusted prediction that DHCP can
    // promote atomically. DiscoverMachine requires this ownership before returning credentials, so
    // deferring the association to a later Site Explorer sweep would strand the DPU in discovery.
    async fn ensure_dpu_oob_interface_ownership(
        txn: &mut PgConnection,
        explored_dpu: &ExploredDpu,
    ) -> SiteExplorerResult<bool> {
        let dpu_machine_id: &MachineId = explored_dpu.report.machine_id.as_ref().unwrap();
        let oob_net0_mac = explored_dpu.report.dpu_oob_mac_address();

        if let Some(oob_net0_mac) = oob_net0_mac {
            return db::machine_interface::ensure_dpu_oob_interface_ownership(
                txn,
                dpu_machine_id,
                oob_net0_mac,
            )
            .await
            .map_err(|error| match error {
                db::DatabaseError::AlreadyFoundError { kind, id } => {
                    SiteExplorerError::AlreadyFoundError { kind, id }
                }
                error => error.into(),
            });
        }

        Ok(false)
    }

    // create_dpu_machine creates a machine for the DPU as specified by dpu_machine_id. Returns an Optional Machine indicating whether the function created a new machine (returns None if a machine already existed for this DPU).
    // if an entry exists in the machines table with a machine ID which matches dpu_machine_id, a machine has already been created for this DPU. Returns None.
    // if an entry doesnt exist in the machine table, the site explorer will add an entry in the machines table for the DPU and update its network config appropriately (allocating a loop ip address etc). Return the newly created machine.
    async fn create_dpu_machine(
        &self,
        txn: &mut PgConnection,
        explored_dpu: &ExploredDpu,
    ) -> SiteExplorerResult<Option<AnyMachine>> {
        let dpu_machine_id = explored_dpu.report.machine_id.as_ref().unwrap();
        match db::machine::find_one(&mut *txn, dpu_machine_id, MachineSearchConfig::default())
            .await?
        {
            // Do nothing if machine exists. It'll be reprovisioned via redfish
            Some(_existing_machine) => Ok(None),
            None => match db::machine::create(
                txn,
                Some(&self.common_pools),
                dpu_machine_id,
                ManagedHostState::Created,
                None,
                CURRENT_STATE_MODEL_VERSION,
            )
            .await
            {
                Ok(machine) => {
                    tracing::info!(machine_id = %dpu_machine_id, "Created DPU machine");
                    Ok(Some(machine))
                }
                Err(e) => {
                    tracing::error!(error = %e, "Can't create DPU machine");
                    Err(e.into())
                }
            },
        }
    }

    async fn attach_dpu_to_host(
        &self,
        txn: &mut PgConnection,
        explored_host: &ManagedHost<'_>,
        explored_dpu: &ExploredDpu,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<HostMachineId> {
        let dpu_hw_info = explored_dpu.hardware_info()?;
        // Create Host proactively.
        // In case host interface is created, this method will return existing one, instead
        // creating new everytime.
        let host_machine_interface =
            db::machine_interface::create_host_machine_dpu_interface_proactively(
                txn,
                Some(&dpu_hw_info),
                explored_dpu.report.machine_id.as_ref().unwrap(),
                self.config.admin_fnn_enabled,
                self.config.retained_boot_interface_window,
            )
            .await?;

        if host_machine_interface.machine_id.is_some() {
            return Err(SiteExplorerError::internal(format!(
                "The host's machine interface for DPU {} already has the machine ID set--something is wrong: {:#?}",
                explored_dpu.report.machine_id.as_ref().unwrap(),
                host_machine_interface
            )));
        }

        let host_machine_id = self
            .configure_host_machine(
                txn,
                explored_host,
                &host_machine_interface,
                explored_dpu,
                machine_data,
            )
            .await?;

        db::machine_interface::associate_interface_with_machine(
            &host_machine_interface.id,
            MachineInterfaceAssociation::Machine(host_machine_id.into()),
            txn,
        )
        .await?;

        Ok(host_machine_id)
    }

    async fn update_machine_topology(
        &self,
        txn: &mut PgConnection,
        machine_id: &MachineId,
        mut bmc_info: BmcInfo,
        hardware_info: HardwareInfo,
    ) -> SiteExplorerResult<()> {
        let _topology =
            db::machine_topology::create_or_update(txn, machine_id, &hardware_info).await?;

        // Forge scout will update this topology with a full information.
        db::machine_topology::set_topology_update_needed(txn, machine_id, true).await?;

        // call enrich_mac_address to fill the MAC address info from the machine_interfaces table
        db::bmc_metadata::enrich_mac_address(
            &mut bmc_info,
            "SiteExplorer::update_machine_topology".to_string(),
            txn,
            machine_id,
            true,
        )
        .await?;

        db::bmc_metadata::update_bmc_network_into_machine_interfaces(
            txn,
            machine_id,
            &mut bmc_info,
        )
        .await?;

        Ok(())
    }

    // configure_host_machine configures the host's machine with the specific interface. It returns the host's machine ID.
    //
    // Normally, a host will have a single machine interface because the majority of hosts (for now) have a single DPU.
    // If a host has multiple DPUs, the host machine will have a machine interface for each DPU.
    // However, all of the host machine interfaces must be attached to the same host machine (and host machine-id).
    // Until this point, all of these interfaces will be marked as the "primary" interface by default.
    //
    // configure_host_machine handles two cases:
    // 1) host_machine_interface is the primary interface for this host: generate the machine ID for this host and use it to actually create the machine for the host.
    // 2) host_machine_interface is *not* the primary interface for this host: set "primary_interface" to false for this machine interface. Return the host ID generated from (1)
    //
    // The first DPU that we attach to the host is designated as the primary DPU; the associate host machine interface is designated is the primary interface.
    // Therefore, the primary interface is guaranteed to be configured prior to any secondary interface.
    #[allow(clippy::too_many_arguments)]
    async fn configure_host_machine(
        &self,
        txn: &mut PgConnection,
        explored_host: &ManagedHost<'_>,
        host_machine_interface: &MachineInterfaceSnapshot,
        explored_dpu: &ExploredDpu,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<HostMachineId> {
        match &explored_host.machine_id {
            Some(host_machine_id) => {
                // This is not the primary interface for this host
                // The primary interface *must* have already been created for this host (otherwise something very bad has happened)
                db::machine_interface::set_primary_interface(
                    &host_machine_interface.id,
                    false,
                    txn,
                )
                .await?;
                Ok(*host_machine_id)
            }
            None => {
                // This is the primary interface for the host.
                // 1. Generate the ID for the host from *this* DPU's hw info
                // 2. Add an entry for this host in the machines table (with a machine-id from (1)).
                let host_machine_id = self
                    .create_host_from_dpu_hw_info(
                        txn,
                        explored_host.explored_host,
                        explored_dpu,
                        machine_data,
                    )
                    .await?;

                tracing::info!(
                    ?host_machine_interface.id,
                    machine_id = %host_machine_id,
                    "Created host machine proactively in site-explorer",
                );

                db::machine_interface::set_primary_interface(&host_machine_interface.id, true, txn)
                    .await?;
                Ok(host_machine_id.into())
            }
        }
    }

    // 1) Generate the host's machine ID from the DPU's hardware info
    // 2) Create a machine for this host using the machine ID from (1)
    // 3) Update the "machine_topologies" table with the bmc info for this host
    async fn create_host_from_dpu_hw_info(
        &self,
        txn: &mut PgConnection,
        explored_host: &ExploredManagedHost,
        explored_dpu: &ExploredDpu,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<PredictedHostMachineId> {
        let dpu_hw_info = explored_dpu.hardware_info()?;
        let predicted_machine_id = host_id_from_dpu_hardware_info(&dpu_hw_info).map_err(|e| {
            SiteExplorerError::InvalidArgument(format!("hardware info missing: {e}"))
        })?;

        let _host_machine = db::machine::create(
            txn,
            Some(&self.common_pools),
            &predicted_machine_id,
            ManagedHostState::Created,
            machine_data,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;

        let host_bmc_info = explored_host.bmc_info();
        let host_hardware_info = HardwareInfo::default();
        self.update_machine_topology(
            txn,
            &predicted_machine_id,
            host_bmc_info,
            host_hardware_info,
        )
        .await?;

        Ok(PredictedHostMachineId::try_from(predicted_machine_id)?)
    }
}

/// One finished `create_identified_host` call.
struct CreationAttempt {
    created: bool,
    event: ManagedHostCreationFinished,
}

/// The hosts of one run that still need a creation attempt, with the run's
/// creation budget. Built by `MachineCreator::creation_run`.
pub(super) struct CreationRun<'a> {
    creator: &'a MachineCreator,
    expected_explored_endpoint_index: &'a ExploredEndpointIndex,
    /// Hosts with a BMC still without a machine, in creation order. Each
    /// attempt counts toward the cap.
    pending: Peekable<std::vec::IntoIter<&'a mut IdentifiedManagedHost>>,
    budget: CreationBudget,
    /// Distinct racks of the pending hosts still without a machine, in id
    /// order. The first `create_hosts` call takes and creates them.
    racks_to_ensure: Vec<RackId>,
    pass_interval: Duration,
    last_pass: Instant,
}

/// Why `CreationRun::create_hosts` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CreationProgress {
    /// Every pending host within the cap has had its attempt.
    Done,
    /// `switch_and_power_shelf_pass_interval` has elapsed and hosts remain to
    /// dispatch: pending hosts within the cap. Nothing is in
    /// flight, so the caller can run the switch and power shelf pass and call
    /// `create_hosts` again.
    PassDue,
}

impl<'a> CreationRun<'a> {
    /// Creates the pending hosts in order, up to `machine_creation_concurrency`
    /// at once, each in its own transaction under its own admission permit,
    /// and awaits every in-flight creation before returning.
    /// `machines_created_per_run` is applied at dispatch and counts in-flight
    /// creations. The first call creates the racks the pending hosts need
    /// before it dispatches anything,
    /// unless the cap lets it dispatch no pending host. Once
    /// `switch_and_power_shelf_pass_interval` has elapsed since the run
    /// started or since the last pass, dispatching stops, the in-flight
    /// creations drain, and this returns `PassDue` while hosts remain to
    /// dispatch. A zero interval never pauses.
    pub(super) async fn create_hosts(
        &mut self,
        metrics: &mut SiteExplorationMetrics,
    ) -> CreationProgress {
        let creator = self.creator;
        let expected_explored_endpoint_index = self.expected_explored_endpoint_index;
        let racks_to_ensure = std::mem::take(&mut self.racks_to_ensure);
        if !racks_to_ensure.is_empty() && !self.budget.exhausted() {
            creator.ensure_racks_before_dispatch(&racks_to_ensure).await;
        }
        let creation_attempt = move |identified: &'a mut IdentifiedManagedHost| {
            let expected_machine = expected_explored_endpoint_index
                .matched_expected_machine(&identified.explored_host.host_bmc_ip);
            creator.create_identified_host(identified, expected_machine)
        };
        let mut in_flight = FuturesUnordered::new();
        let mut pass_due = false;
        loop {
            while !pass_due && self.budget.can_dispatch() {
                let Some(identified) = self.pending.next() else {
                    break;
                };
                self.budget.dispatched();
                in_flight.push(creation_attempt(identified));
            }
            let Some(attempt) = in_flight.next().await else {
                break;
            };
            self.budget.finished(attempt.created);
            if attempt.created {
                metrics.created_machines += 1;
            }
            emit(attempt.event);
            if !self.pass_interval.is_zero() && self.last_pass.elapsed() >= self.pass_interval {
                pass_due = true;
            }
        }

        let hosts_remain = self.pending.peek().is_some() && !self.budget.exhausted();
        if pass_due && hosts_remain {
            self.last_pass = Instant::now();
            CreationProgress::PassDue
        } else {
            CreationProgress::Done
        }
    }
}

/// The per-run creation cap applied at dispatch. In-flight creations count
/// against the cap, so `created` never exceeds it, and a finished attempt
/// that created nothing frees its slot and keeps the cap.
#[derive(Debug)]
struct CreationBudget {
    cap: u64,
    concurrency: usize,
    in_flight: usize,
    created: u64,
}

impl CreationBudget {
    fn new(cap: u64, concurrency: usize) -> Self {
        Self {
            cap,
            concurrency,
            in_flight: 0,
            created: 0,
        }
    }

    /// Whether another creation may start now.
    fn can_dispatch(&self) -> bool {
        self.in_flight < self.concurrency && !self.exhausted()
    }

    /// Whether the cap leaves nothing more to dispatch, counting in-flight
    /// creations.
    fn exhausted(&self) -> bool {
        self.created + (self.in_flight as u64) >= self.cap
    }

    fn dispatched(&mut self) {
        self.in_flight += 1;
    }

    fn finished(&mut self, created: bool) {
        self.in_flight -= 1;
        if created {
            self.created += 1;
        }
    }
}

/// The in-flight creation bound: the configured value, at least 1 and at
/// most one below the admission permit count, so the other gated flows keep
/// a permit while creation runs. With a single permit one creation still
/// runs and holds it.
fn creation_concurrency(configured: u64, admission_permits: usize) -> usize {
    let ceiling = admission_permits.saturating_sub(1).max(1);
    let configured = usize::try_from(configured).unwrap_or(usize::MAX).max(1);
    if configured > ceiling {
        tracing::warn!(
            configured,
            ceiling,
            admission_permits,
            "machine_creation_concurrency exceeds the admin-lock admission permits minus one; using the ceiling"
        );
    }
    configured.min(ceiling)
}

/// What one `create_managed_host_with_selection` call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedHostCreation {
    /// A host machine and its DPU machines were created.
    Created { host_machine_id: HostMachineId },
    /// The host already exists or its interfaces are owned; no state was changed.
    SkippedExisting,
    /// No `expected_machines` record matched the host.
    SkippedUnexpected,
}

/// The Event for one finished creation attempt on `host`.
fn creation_finished_event(
    host: &ExploredManagedHost,
    result: SiteExplorerResult<ManagedHostCreation>,
    latency: Duration,
) -> ManagedHostCreationFinished {
    match result {
        Ok(ManagedHostCreation::Created { host_machine_id }) => {
            ManagedHostCreationFinished::Created {
                latency,
                host_bmc_ip_address: host.host_bmc_ip,
                host_machine_id,
                dpu_count: host.dpus.len(),
            }
        }
        Ok(ManagedHostCreation::SkippedExisting) => {
            ManagedHostCreationFinished::SkippedExisting { latency }
        }
        Ok(ManagedHostCreation::SkippedUnexpected) => {
            ManagedHostCreationFinished::SkippedUnexpected { latency }
        }
        Err(error) => ManagedHostCreationFinished::Failed {
            latency,
            host_bmc_ip_address: host.host_bmc_ip,
            error: error.to_string(),
        },
    }
}

fn record_creation_stage(stage: ManagedHostCreationStage, latency: Duration) {
    emit(ManagedHostCreationStageFinished { stage, latency });
}

/// Initializes host admin address ownership in its own transaction after
/// managed-host creation commits.
///
/// `reconcile_admin_addresses_for_host` takes the admin segment advisory
/// locks before any row lock when it has a repair to make, so it cannot run
/// inside the managed-host transaction, which locks interface and machine
/// rows first.
async fn reconcile_host_admin_addresses_after_commit(
    pool: &PgPool,
    host_machine_id: &HostMachineId,
) {
    if let Err(error) =
        db::machine_interface::reconcile_admin_addresses_after_commit(pool, host_machine_id).await
    {
        emit(SiteExplorerHostAdminAddressReconcileFailed {
            host_machine_id: *host_machine_id,
            error: error.to_string(),
        });
    }
}

/// The BMC IPs of `host`: its own BMC, then each DPU BMC.
fn host_bmc_ips(host: &ExploredManagedHost) -> impl Iterator<Item = &IpAddr> {
    std::iter::once(&host.host_bmc_ip).chain(host.dpus.iter().map(|dpu| &dpu.bmc_ip))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    struct BudgetCase {
        cap: u64,
        concurrency: usize,
        /// Outcomes of the attempts finished after the first dispatch round.
        finished: Vec<bool>,
    }

    #[derive(Debug, PartialEq)]
    struct BudgetState {
        dispatched: usize,
        created: u64,
        can_dispatch: bool,
        exhausted: bool,
    }

    #[test]
    fn creation_budget_cases() {
        check_values(
            [
                Check {
                    scenario: "the cap counts in-flight creations",
                    input: BudgetCase {
                        cap: 2,
                        concurrency: 3,
                        finished: vec![],
                    },
                    expect: BudgetState {
                        dispatched: 2,
                        created: 0,
                        can_dispatch: false,
                        exhausted: true,
                    },
                },
                Check {
                    scenario: "an attempt that created nothing frees its slot and keeps the cap",
                    input: BudgetCase {
                        cap: 2,
                        concurrency: 3,
                        finished: vec![false],
                    },
                    expect: BudgetState {
                        dispatched: 2,
                        created: 0,
                        can_dispatch: true,
                        exhausted: false,
                    },
                },
                Check {
                    scenario: "creations spend the cap",
                    input: BudgetCase {
                        cap: 2,
                        concurrency: 3,
                        finished: vec![true, true],
                    },
                    expect: BudgetState {
                        dispatched: 2,
                        created: 2,
                        can_dispatch: false,
                        exhausted: true,
                    },
                },
                Check {
                    scenario: "concurrency 1 dispatches one host at a time",
                    input: BudgetCase {
                        cap: 4,
                        concurrency: 1,
                        finished: vec![true],
                    },
                    expect: BudgetState {
                        dispatched: 1,
                        created: 1,
                        can_dispatch: true,
                        exhausted: false,
                    },
                },
                Check {
                    scenario: "a full concurrency limit prevents another creation",
                    input: BudgetCase {
                        cap: 4,
                        concurrency: 1,
                        finished: vec![],
                    },
                    expect: BudgetState {
                        dispatched: 1,
                        created: 0,
                        can_dispatch: false,
                        exhausted: false,
                    },
                },
                Check {
                    scenario: "a zero cap dispatches no creation",
                    input: BudgetCase {
                        cap: 0,
                        concurrency: 3,
                        finished: vec![],
                    },
                    expect: BudgetState {
                        dispatched: 0,
                        created: 0,
                        can_dispatch: false,
                        exhausted: true,
                    },
                },
            ],
            |BudgetCase {
                 cap,
                 concurrency,
                 finished,
             }| {
                let mut budget = CreationBudget::new(cap, concurrency);
                let mut dispatched = 0;
                while budget.can_dispatch() {
                    budget.dispatched();
                    dispatched += 1;
                }
                for created in finished {
                    budget.finished(created);
                }
                BudgetState {
                    dispatched,
                    created: budget.created,
                    can_dispatch: budget.can_dispatch(),
                    exhausted: budget.exhausted(),
                }
            },
        );
    }

    #[test]
    fn creation_concurrency_stays_below_the_admission_permits() {
        check_values(
            [
                Check {
                    scenario: "a value below the ceiling is used as configured",
                    input: (4, 16),
                    expect: 4,
                },
                Check {
                    scenario: "a value at the permit count is clamped to one below it",
                    input: (16, 16),
                    expect: 15,
                },
                Check {
                    scenario: "zero creates hosts one at a time",
                    input: (0, 16),
                    expect: 1,
                },
                Check {
                    scenario: "a single permit still allows one creation",
                    input: (4, 1),
                    expect: 1,
                },
            ],
            |(configured, admission_permits)| creation_concurrency(configured, admission_permits),
        );
    }
}
