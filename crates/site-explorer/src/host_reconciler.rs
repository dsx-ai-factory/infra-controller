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
//! Reconciles persisted host state independently of inventory creation.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use carbide_instrument::{Event, Instrumentation, LabelValue};
use carbide_resource_pool::ResourcePoolRegistry;
use carbide_uuid::machine::{HostMachineId, MachineInterfaceId};
use db::TransactionVending as _;
use db::machine_interface::MachineInterfaces;
use futures_util::future::join_all;
use itertools::Itertools;
use mac_address::MacAddress;
use model::machine_boot_interface::{
    BootInterfaceSelectionAuthority, InterfaceSelectionSource, MachineBootInterface,
    MachineBootInterfaceTarget,
};
use model::resource_pool::common::CommonPools;
use model::site_explorer::{
    EndpointExplorationReport, EndpointType, ExploredDpu, ExploredManagedHost,
};
use sqlx::{PgConnection, PgPool};
use tokio_util::sync::CancellationToken;

use crate::boot_interfaces::{NicBootInterfaces, initial_selection_source, zero_dpu_primary_mac};
use crate::dpu_asn::PreparedDpuAsn;
use crate::dpu_loopback::{PreparedDpuLoopback, acquire_admin_admission};
use crate::errors::SiteExplorerResult;
use crate::explored_endpoint_index::ExploredEndpointIndex;
use crate::inventory_creator::{attempt_deadline, reconcile_host_admin_addresses_after_commit};
use crate::machine_interfaces::MachineInterfacesExt;
use crate::managed_host::{ZeroDpuHostLookup, find_zero_dpu_host};

const DESIRED_BOOT_INTERFACE_RECONCILE_PAGE_SIZE: i64 = 100;

/// Reconciles persisted state of existing hosts.
pub(super) struct HostReconciler {
    instrumentation: Instrumentation,
    database_connection: PgPool,
    retained_boot_interface_window: Option<chrono::Duration>,
    /// Deadline of one admin address repair once it holds its admission
    /// permit.
    admin_address_repair_timeout: Duration,
    resource_pool_registry: ResourcePoolRegistry,
    common_pools: Arc<CommonPools>,
    pub(super) preparation_cancel_token: CancellationToken,
}

impl db::TransactionVending for HostReconciler {
    #[track_caller]
    fn txn_begin(
        &self,
    ) -> impl Future<Output = Result<db::Transaction<'static>, db::DatabaseError>> {
        db::Transaction::begin_instrumented(&self.database_connection, &self.instrumentation)
    }
}

impl HostReconciler {
    pub(super) fn new(
        database_connection: PgPool,
        retained_boot_interface_window: Option<chrono::Duration>,
        admin_address_repair_timeout: Duration,
        resource_pool_registry: ResourcePoolRegistry,
        common_pools: Arc<CommonPools>,
        instrumentation: Instrumentation,
    ) -> Self {
        Self {
            instrumentation,
            database_connection,
            retained_boot_interface_window,
            admin_address_repair_timeout,
            resource_pool_registry,
            common_pools,
            preparation_cancel_token: CancellationToken::new(),
        }
    }

    /// Reconciles existing hosts selected from shared observations,
    /// then sweeps incomplete desired boot interfaces.
    ///
    /// The notification contains probed BMC addresses, including failed probes.
    /// NIC IDs retry from cached reports without a fresh probe; inventory repair
    /// retains probe selection and also retries hosts with an unowned BMC.
    /// Work selection uses neither ExpectedMachine nor
    /// creation queues, caps, or the `create_machines` setting.
    ///
    /// Host reports include fresh, preingestion-ready observations rejected by
    /// matching; BMC suppression has already been applied. The BMC ownership
    /// snapshot was loaded before creation. Sharing these inputs avoids reloading
    /// the inventory; persisted interface ownership is checked before each repair.
    /// Only successfully refreshed reports can attach a missing BMC; cached reports still retry NIC ID publication.
    pub(super) async fn run_single_iteration<'a>(
        &self,
        hosts: impl Iterator<Item = &'a ExploredManagedHost>,
        host_reports: impl Iterator<Item = (IpAddr, &'a EndpointExplorationReport)>,
        endpoint_index: &ExploredEndpointIndex,
        ingested_bmc_ips: &HashSet<IpAddr>,
        explored_this_run: &HashSet<IpAddr>,
        refreshed_this_run: &HashSet<IpAddr>,
    ) -> SiteExplorerResult<()> {
        let start = Instant::now();
        self.reconcile_hosts_with_dpus(
            hosts,
            endpoint_index,
            ingested_bmc_ips,
            explored_this_run,
            refreshed_this_run,
        )
        .await;
        let deferred_boot_hosts = self
            .reconcile_host_reports(
                host_reports,
                endpoint_index,
                ingested_bmc_ips,
                explored_this_run,
            )
            .await?;
        self.instrumentation.emit(HostReconciliationPhaseFinished {
            phase: HostReconciliationPhase::ExploredHosts,
            duration: start.elapsed(),
        });

        let start = Instant::now();
        let result = self
            .reconcile_desired_boot_interfaces(&deferred_boot_hosts)
            .await;
        self.instrumentation.emit(HostReconciliationPhaseFinished {
            phase: HostReconciliationPhase::DesiredBootInterfaces,
            duration: start.elapsed(),
        });
        result
    }

    async fn reconcile_hosts_with_dpus<'a>(
        &self,
        hosts: impl Iterator<Item = &'a ExploredManagedHost>,
        endpoint_index: &ExploredEndpointIndex,
        ingested_bmc_ips: &HashSet<IpAddr>,
        explored_this_run: &HashSet<IpAddr>,
        refreshed_this_run: &HashSet<IpAddr>,
    ) {
        let hosts = hosts
            .filter(|host| {
                !host.dpus.is_empty()
                    && std::iter::once(&host.host_bmc_ip)
                        .chain(host.dpus.iter().map(|dpu| &dpu.bmc_ip))
                        .any(|ip| explored_this_run.contains(ip) || !ingested_bmc_ips.contains(ip))
            })
            .sorted_by_key(|host| host.host_bmc_ip);
        for host in hosts {
            if let Err(error) = self
                .reconcile_host_with_dpus(host, endpoint_index, refreshed_this_run)
                .await
            {
                self.instrumentation
                    .emit(HostReconciliationFailed::ExploredHost {
                        host_bmc_ip_address: host.host_bmc_ip,
                        error: error.to_string(),
                    });
            }
        }
    }

    /// Reconciles reported boot IDs and zero-DPU inventory so incomplete DPU pairing cannot leave the selected NIC's ID stale.
    /// Returns affected host IDs so `reconcile_desired_boot_interfaces` can defer them for this iteration.
    /// Conflicting canonical IDs across one BMC's addresses preserve its ID stores and defer report-driven inventory until exploration resolves the conflict.
    async fn reconcile_host_reports<'a>(
        &self,
        host_reports: impl Iterator<Item = (IpAddr, &'a EndpointExplorationReport)>,
        endpoint_index: &ExploredEndpointIndex,
        ingested_bmc_ips: &HashSet<IpAddr>,
        explored_this_run: &HashSet<IpAddr>,
    ) -> SiteExplorerResult<HashSet<HostMachineId>> {
        let mut deferred_boot_hosts = HashSet::new();
        // Creation policy may exclude an existing zero-DPU host from matching
        // after its ExpectedMachine is removed. Use the shared BMC reports and
        // validate persisted interface ownership before refreshing an existing host.
        let host_reports = host_reports
            .filter(|(address, report)| {
                report.endpoint_type == EndpointType::Bmc
                    && !report.is_dpu()
                    && !report.is_switch()
                    && !report.is_power_shelf()
                    && endpoint_index.underlay_interface(address).is_some()
            })
            .filter_map(|(address, report)| {
                endpoint_index.underlay_interface(&address).map(|bmc| {
                    (
                        address,
                        report,
                        bmc.id,
                        NicBootInterfaces::new(address, report),
                    )
                })
            })
            .sorted_by_key(|(address, _, _, _)| *address)
            .collect_vec();

        // Compare canonical observations before publishing either alias; report mutation times do not order successful hardware observations.
        let mut reported_ids = HashMap::new();
        let mut conflicting_bmcs = HashSet::new();
        for (_, _, bmc_interface_id, interfaces) in &host_reports {
            for interface in interfaces
                .iter()
                .flat_map(|interfaces| &interfaces.interfaces)
            {
                if let Some(previous) = reported_ids.insert(
                    (*bmc_interface_id, interface.mac_address),
                    interface.interface_id.as_str(),
                ) && previous != interface.interface_id.as_str()
                {
                    conflicting_bmcs.insert(*bmc_interface_id);
                }
            }
        }

        for (address, report, bmc_interface_id, interfaces) in &host_reports {
            // Preserve all ID stores when aliases disagree, including MACs that a concurrent selection could make authoritative.
            let published = if conflicting_bmcs.contains(bmc_interface_id) {
                Err(db::DatabaseError::FailedPrecondition(format!(
                    "conflicting Redfish interface IDs across addresses of BMC interface {bmc_interface_id}; refresh its reports"
                )).into())
            } else if let Some(interfaces) = interfaces {
                // Retry retained reports even without another probe, committing each host independently.
                self.publish_reported_boot_interface_ids(*bmc_interface_id, interfaces)
                    .await
            } else {
                // Incomplete reports retain IDs while preserving ordinary inventory reconciliation.
                Ok(())
            };
            if let Err(error) = published {
                self.instrumentation
                    .emit(HostReconciliationFailed::ExploredHost {
                        host_bmc_ip_address: *address,
                        error: error.to_string(),
                    });
                // The index predates creation and BMC addresses may move; reload this interface's current host by ID before deferring incomplete intent.
                // A deleted interface has no host to defer, so continue reconciling the other reports.
                if let Some(host_id) = db::machine_interface::find_by(
                    &self.database_connection,
                    db::ObjectColumnFilter::One(db::machine_interface::IdColumn, bmc_interface_id),
                )
                .await?
                .pop()
                .and_then(|interface| interface.machine_id)
                .and_then(|id| HostMachineId::try_from(id).ok())
                {
                    deferred_boot_hosts.insert(host_id);
                }
                continue;
            }
            // Inventory and admin changes keep their existing probe-driven work selection.
            if (explored_this_run.contains(address) || !ingested_bmc_ips.contains(address))
                && endpoint_index
                    .underlay_interface(address)
                    .is_some_and(|interface| interface.machine_id.is_some())
                && let Err(error) = self
                    .reconcile_host_report(*address, report, endpoint_index)
                    .await
            {
                self.instrumentation
                    .emit(HostReconciliationFailed::ExploredHost {
                        host_bmc_ip_address: *address,
                        error: error.to_string(),
                    });
            }
        }
        Ok(deferred_boot_hosts)
    }

    /// Publishes reported NIC IDs with their selected same-MAC desired target so lifecycle readers cannot see a partially refreshed Pair.
    async fn publish_reported_boot_interface_ids(
        &self,
        bmc_interface_id: MachineInterfaceId,
        interfaces: &NicBootInterfaces,
    ) -> SiteExplorerResult<()> {
        // Admit interface and prediction changes before the desired writer acquires machine-group locks.
        let mut txn = self.txn_begin().await?;
        if let Some(machine_id) = interfaces.persist(&mut txn, bmc_interface_id).await? {
            Self::refresh_reported_boot_interface_id(
                txn.as_pgconn(),
                &machine_id,
                &interfaces.interfaces,
            )
            .await?;
        }
        txn.commit().await?;
        Ok(())
    }

    /// Reconciles zero-DPU inventory changes without selecting new boot intent for managed-DPU hosts.
    async fn reconcile_host_report(
        &self,
        address: IpAddr,
        report: &EndpointExplorationReport,
        endpoint_index: &ExploredEndpointIndex,
    ) -> SiteExplorerResult<()> {
        let Some(bmc_interface) = endpoint_index.underlay_interface(&address) else {
            return Ok(());
        };
        let machine_data = endpoint_index
            .matched_expected_machine(&address)
            .map(|expected| &expected.data);
        let mac_addresses = report.host_mac_addresses_for_predicted_machine(machine_data);
        let declared_primary = machine_data.and_then(|data| data.declared_primary_mac());

        let mut _admin_admission = None;
        let mut txn = self.txn_begin().await?;
        let zero_dpu_host: Option<(HostMachineId, _)> =
            match find_zero_dpu_host(txn.as_pgconn(), bmc_interface.mac_address, &mac_addresses)
                .await?
            {
                ZeroDpuHostLookup::Predicted(machine_id) => {
                    Some((machine_id.into(), Some(machine_id)))
                }
                ZeroDpuHostLookup::Stable(machine_id) => Some((machine_id.into(), None)),
                ZeroDpuHostLookup::NotFound | ZeroDpuHostLookup::Conflict => None,
            };
        let Some(machine_id) = zero_dpu_host.map(|(machine_id, _)| machine_id).or_else(|| {
            bmc_interface
                .machine_id
                .and_then(|machine_id| HostMachineId::try_from(machine_id).ok())
        }) else {
            txn.rollback().await?;
            return Ok(());
        };
        let predicted_id = zero_dpu_host.and_then(|(_, predicted_id)| predicted_id);
        if let Some(predicted_id) = predicted_id {
            // Release the read transaction before waiting for admission so the
            // queue cannot occupy pool connections. Revalidate after the wait.
            txn.rollback().await?;
            _admin_admission = Some(db::machine_interface::admin_lock_admission().await);
            txn = self.txn_begin().await?;
            if !matches!(
                find_zero_dpu_host(txn.as_pgconn(), bmc_interface.mac_address, &mac_addresses)
                    .await?,
                ZeroDpuHostLookup::Predicted(current_id) if current_id == predicted_id
            ) {
                txn.rollback().await?;
                return Ok(());
            }
            let primary_mac = zero_dpu_primary_mac(
                txn.as_pgconn(),
                Some(predicted_id),
                &mac_addresses,
                declared_primary,
                self.retained_boot_interface_window,
            )
            .await?;
            let reported_interfaces = NicBootInterfaces::new(address, report);
            let report_boot_interface_ids = reported_interfaces
                .as_ref()
                .into_iter()
                .flat_map(|interfaces| &interfaces.interfaces)
                .map(|interface| (interface.mac_address, interface.interface_id.as_str()))
                .collect::<Vec<_>>();
            // Preserve the lock order against DHCP and admin address repair.
            db::machine_interface::lock_all_admin_segments(txn.as_pgconn()).await?;
            crate::host_interfaces::reconcile(
                txn.as_pgconn(),
                &machine_id,
                &mac_addresses,
                &report_boot_interface_ids,
                primary_mac,
                if primary_mac.is_some() && primary_mac == declared_primary {
                    InterfaceSelectionSource::ExpectedMachine
                } else {
                    InterfaceSelectionSource::LegacyUnknown
                },
            )
            .await?;
        }
        if zero_dpu_host.is_some() {
            Self::reconcile_desired_boot_interface(
                txn.as_pgconn(),
                &machine_id,
                predicted_id.and(declared_primary),
            )
            .await?;
        }
        txn.commit().await?;
        Ok(())
    }

    async fn reconcile_host_with_dpus(
        &self,
        host: &ExploredManagedHost,
        endpoint_index: &ExploredEndpointIndex,
        refreshed_this_run: &HashSet<IpAddr>,
    ) -> SiteExplorerResult<()> {
        let dpu_ids = host
            .dpus
            .iter()
            .map(ExploredDpu::machine_id_if_valid_report)
            .collect::<Result<Vec<_>, _>>()?;

        let host_associations =
            db::machine::DpuHostAssociations::load(&self.database_connection, &dpu_ids).await?;
        let Some(host_machine_id) = host_associations.common_host_machine_id()? else {
            return Ok(());
        };

        // Prepare outside the entity transaction, preserving backend selection and
        // DPU lifetime checks for existing hosts as well as new inventory.
        let preparations = join_all(dpu_ids.iter().map(|dpu_id| async move {
            tokio::join!(
                PreparedDpuLoopback::prepare(
                    &self.database_connection,
                    &self.resource_pool_registry,
                    &self.common_pools,
                    (*dpu_id).into(),
                    None,
                    &self.preparation_cancel_token,
                    self.instrumentation.clone(),
                ),
                PreparedDpuAsn::prepare(
                    &self.database_connection,
                    &self.resource_pool_registry,
                    &self.common_pools,
                    (*dpu_id).into(),
                    &self.preparation_cancel_token,
                ),
            )
        }))
        .await;
        let mut loopbacks = Vec::with_capacity(preparations.len());
        let mut asns = Vec::with_capacity(preparations.len());
        let mut preparation_error = None;
        for (loopback, asn) in preparations {
            match loopback {
                Ok(prepared) => loopbacks.push(prepared),
                Err(error) if preparation_error.is_none() => preparation_error = Some(error),
                Err(_) => {}
            }
            match asn {
                Ok(prepared) => asns.push(prepared),
                Err(error) if preparation_error.is_none() => preparation_error = Some(error),
                Err(_) => {}
            }
        }
        let result = async {
            if let Some(error) = preparation_error {
                return Err(error.into());
            }
            let needs_bounded_admission = loopbacks
                .iter()
                .any(PreparedDpuLoopback::has_provider_reservation)
                || asns.iter().any(PreparedDpuAsn::has_provider_reservation);
            let _admission =
                acquire_admin_admission(needs_bounded_admission, &self.preparation_cancel_token)
                    .await?;
            let mut txn = self.txn_begin().await?;
            // Provider preparation may outlive the attachment snapshot.
            let current_associations =
                db::machine::DpuHostAssociations::load(&mut txn, &dpu_ids).await?;
            if current_associations.common_host_machine_id()? != Some(host_machine_id) {
                return Err(db::DatabaseError::FailedPrecondition(
                    "DPU host attachment changed during DPU resource preparation; retry".to_owned(),
                )
                .into());
            }
            for prepared in &loopbacks {
                prepared.check_identity(txn.as_pgconn()).await?;
            }
            for prepared in &asns {
                prepared.check_identity(txn.as_pgconn()).await?;
            }
            for (dpu, dpu_id) in host.dpus.iter().zip(&dpu_ids) {
                if let Some(mac_address) = dpu.report.dpu_oob_mac_address() {
                    db::machine_interface::ensure_dpu_oob_interface_ownership(
                        txn.as_pgconn(),
                        dpu_id,
                        mac_address,
                    )
                    .await?;
                }
            }
            // API-created machines can precede BMC exploration; attach only missing BMCs without replacing discovery or PF inventory.
            for (machine_id, bmc_ip) in std::iter::once((host_machine_id.into(), host.host_bmc_ip))
                .chain(
                    host.dpus
                        .iter()
                        .zip(&dpu_ids)
                        .map(|(dpu, id)| ((*id).into(), dpu.bmc_ip)),
                )
            {
                // Cached reports can outlive the original address owner; only an accepted fresh probe may attach a missing BMC.
                if !refreshed_this_run.contains(&bmc_ip) {
                    continue;
                }
                let Some(observed) = endpoint_index.underlay_interface(&bmc_ip) else {
                    continue;
                };
                db::bmc_metadata::associate_missing_bmc_interface(
                    txn.as_pgconn(),
                    &machine_id,
                    observed.id,
                    bmc_ip,
                )
                .await?;
            }
            // The later desired-interface sweep runs after reported IDs are published, avoiding completion with an obsolete ID.
            for prepared in &loopbacks {
                prepared.attach(&mut txn, false).await?;
            }
            for prepared in &asns {
                prepared.attach(&mut txn).await?;
            }
            txn.commit().await?;
            Ok::<_, crate::errors::SiteExplorerError>(())
        }
        .await;
        // Cancel unused reservations only after the entity transaction has ended.
        for prepared in &loopbacks {
            if let Err(error) = prepared.cancel(&self.database_connection).await {
                tracing::error!(machine_id = %prepared.machine_id(), %error, "Could not save DPU loopback cancellation");
            }
        }
        for prepared in &asns {
            if let Err(error) = prepared.cancel(&self.database_connection).await {
                tracing::error!(machine_id = %prepared.machine_id(), error = %error, "Could not save DPU ASN cancellation");
            }
        }
        result?;

        // Admin address repair takes segment locks before row locks, so it needs
        // its own transaction. Wait for admission before it occupies a connection.
        let _admin_admission = db::machine_interface::admin_lock_admission().await;
        reconcile_host_admin_addresses_after_commit(
            &self.database_connection,
            &host_machine_id,
            attempt_deadline(self.admin_address_repair_timeout),
            &self.instrumentation,
        )
        .await;

        Ok(())
    }

    /// Refreshes the selected MAC's reported Redfish ID without reselecting the NIC, requiring fresh verification when a completed Pair changes because Dell targets its ID directly.
    ///
    /// Callers must supply the same complete pairs used for this BMC's interface-ID writes.
    async fn refresh_reported_boot_interface_id(
        txn: &mut PgConnection,
        machine_id: &HostMachineId,
        interfaces: &[MachineBootInterface],
    ) -> SiteExplorerResult<()> {
        // Read the selected MAC only after group admission so concurrent selection writers cannot leave refreshed IDs beside stale intent.
        if !db::machine::try_lock_group_for_update(txn, machine_id.as_machine_id()).await? {
            return Err(db::DatabaseError::FailedPrecondition(format!(
                "machine group for {machine_id} changed or is busy; retry",
            ))
            .into());
        }

        // Only a complete report pair can supersede the selected NIC's last-known-good ID.
        let Some(desired) = db::machine_desired_boot_interface::get(&mut *txn, machine_id).await?
        else {
            return Ok(());
        };
        let selected_mac = desired.value.mac_address();
        let Some(interface_id) = interfaces
            .iter()
            .find(|interface| interface.mac_address == selected_mac)
            .map(|interface| interface.interface_id.as_str())
            .filter(|interface_id| Some(*interface_id) != desired.value.interface_id())
        else {
            return Ok(());
        };

        match desired.value {
            // MAC-only enrichment strengthens the same target without discarding current verification.
            MachineBootInterfaceTarget::MacOnly(_) => {
                db::machine_desired_boot_interface::enrich_interface_id(
                    txn,
                    machine_id,
                    selected_mac,
                    interface_id,
                )
                .await?;
            }
            // A changed Pair ID must be applied and verified again while retaining the selection decision.
            MachineBootInterfaceTarget::Pair(_) => {
                db::machine_desired_boot_interface::force_reconcile(
                    txn,
                    machine_id,
                    &MachineBootInterfaceTarget::Pair(MachineBootInterface {
                        mac_address: selected_mac,
                        interface_id: interface_id.to_string(),
                    }),
                    BootInterfaceSelectionAuthority::Existing,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Initializes the durable desired boot interface from Site Explorer's settled
    /// interface ownership, or enriches a persisted target that has only a MAC
    /// with a newly observed Redfish interface ID.
    ///
    /// The initial read does not lock, so a completed pair does not serialize
    /// unrelated machine updates on every exploration. The mutation helpers lock
    /// and recheck the two allowed transitions: unset to initialized, and a
    /// `MacOnly` target to a `Pair` for the same MAC.
    async fn reconcile_desired_boot_interface(
        txn: &mut PgConnection,
        machine_id: &HostMachineId,
        declared_primary: Option<MacAddress>,
    ) -> SiteExplorerResult<()> {
        let desired = db::machine_desired_boot_interface::get(&mut *txn, machine_id).await?;

        if matches!(
            desired.as_ref().map(|desired| &desired.value),
            Some(MachineBootInterfaceTarget::Pair(_))
        ) {
            return Ok(());
        }

        let interfaces = MachineInterfaces::load(txn, machine_id).await?;
        let proposed_target = interfaces
            .proposed_boot_interface_target(desired.as_ref().map(|desired| &desired.value));

        match (
            desired.as_ref().map(|desired| &desired.value),
            proposed_target,
        ) {
            (None, Some(target)) => {
                if !db::machine::try_lock_group_for_update(txn, machine_id.as_machine_id()).await? {
                    return Err(db::DatabaseError::FailedPrecondition(format!(
                        "machine group for {machine_id} changed or is busy; retry",
                    ))
                    .into());
                }
                // Keep pending BF4 publication at the controller checkpoint while same-MAC enrichment remains available below.
                if db::machine::bf4_host_pf_initialization_pending(txn, machine_id).await? {
                    return Ok(());
                }
                let selection_source = initial_selection_source(&target, None, declared_primary);
                db::machine_desired_boot_interface::initialize_if_unset(
                    txn,
                    machine_id,
                    &target,
                    selection_source,
                )
                .await?;
            }
            (
                Some(MachineBootInterfaceTarget::MacOnly(mac_address)),
                Some(MachineBootInterfaceTarget::Pair(target)),
            ) => {
                debug_assert_eq!(*mac_address, target.mac_address);
                if !db::machine::try_lock_group_for_update(txn, machine_id.as_machine_id()).await? {
                    return Err(db::DatabaseError::FailedPrecondition(format!(
                        "machine group for {machine_id} changed or is busy; retry",
                    ))
                    .into());
                }

                db::machine_desired_boot_interface::enrich_interface_id(
                    txn,
                    machine_id,
                    *mac_address,
                    &target.interface_id,
                )
                .await?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Runs one best-effort pass over hosts with incomplete desired boot interfaces.
    ///
    /// This runs independently of `create_machines`: operators may disable
    /// creation, and a host may no longer have an `ExpectedMachine` or a fresh
    /// observation. MachineCreator initializes each new host's desired target
    /// in the inventory transaction. Keyset pages prevent
    /// permanently MAC-only hosts from starving later IDs, while one
    /// transaction per machine bounds locks and preserves completed progress.
    async fn reconcile_desired_boot_interfaces(
        &self,
        deferred_boot_hosts: &HashSet<HostMachineId>,
    ) -> SiteExplorerResult<()> {
        let mut after_id = None;
        loop {
            let machine_ids = db::machine_desired_boot_interface::find_incomplete_machine_ids(
                &self.database_connection,
                after_id.as_ref(),
                DESIRED_BOOT_INTERFACE_RECONCILE_PAGE_SIZE,
            )
            .await?;
            let Some(last_id) = machine_ids.last() else {
                break;
            };
            after_id = Some(*last_id);

            for machine_id in machine_ids {
                // Retry report publication before completing this host's intent from potentially obsolete stored IDs.
                if deferred_boot_hosts.contains(&machine_id) {
                    continue;
                }
                let mut txn = self.txn_begin().await.inspect_err(|error| {
                    self.instrumentation
                        .emit(HostReconciliationFailed::DesiredBootInterface {
                            host_machine_id: machine_id,
                            error: error.to_string(),
                        });
                })?;
                match Self::reconcile_desired_boot_interface(txn.as_pgconn(), &machine_id, None)
                    .await
                {
                    Ok(()) => {
                        if let Err(error) = txn.commit().await {
                            self.instrumentation.emit(
                                HostReconciliationFailed::DesiredBootInterface {
                                    host_machine_id: machine_id,
                                    error: error.to_string(),
                                },
                            );
                        }
                    }
                    Err(error) => {
                        txn.rollback_or_log(
                            "site-explorer desired boot-interface reconciliation failure",
                        )
                        .await;
                        self.instrumentation
                            .emit(HostReconciliationFailed::DesiredBootInterface {
                                host_machine_id: machine_id,
                                error: error.to_string(),
                            });
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, LabelValue)]
enum HostReconciliationPhase {
    ExploredHosts,
    DesiredBootInterfaces,
}

/// Counts failed per-host attempts; host identity stays in the warning context.
#[derive(Event)]
#[event(
    event_name = "host_reconciliation_failed",
    metric_name = "carbide_host_reconciliation_failures_total",
    component = "host-reconciler",
    metric = counter,
    describe = "Number of failed per-host reconciliation attempts, by phase.",
    labels(phase: HostReconciliationPhase),
)]
enum HostReconciliationFailed {
    #[event(
        labels(phase = HostReconciliationPhase::ExploredHosts),
        log = warn,
        message = "Explored host reconciliation failed; a later Site Explorer run will retry"
    )]
    ExploredHost {
        #[context]
        host_bmc_ip_address: IpAddr,
        #[context]
        error: String,
    },
    #[event(
        labels(phase = HostReconciliationPhase::DesiredBootInterfaces),
        log = warn,
        message = "Desired boot-interface reconciliation failed; a later Site Explorer run will retry"
    )]
    DesiredBootInterface {
        #[context]
        host_machine_id: HostMachineId,
        #[context]
        error: String,
    },
}

/// Records phase durations independently of Site Explorer's iteration metrics.
#[derive(Event)]
#[event(
    event_name = "host_reconciler_phase_finished",
    metric_name = "carbide_host_reconciler_phase_duration_milliseconds",
    component = "host-reconciler",
    log = off,
    metric = histogram,
    describe = "Time one host reconciler phase took, including unsuccessful passes, by phase."
)]
struct HostReconciliationPhaseFinished {
    #[label]
    phase: HostReconciliationPhase,
    #[observation]
    duration: Duration,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_instrument::testing::{ApproxHistogramSum, MetricsCapture, capture_logs};
    use carbide_test_harness::prelude::sqlx_testing;
    use carbide_test_support::value_scenarios;
    use carbide_uuid::machine::MachineId;
    use model::machine_boot_interface::{BootInterfaceSelectionSource, MachineBootInterface};
    use model::network_segment::NetworkSegmentType;

    use super::*;
    use crate::boot_interfaces::NicBootInterfaces;

    #[test]
    fn reconciliation_failures_count_attempts_and_log_host_identity() {
        let host_bmc_ip_address: IpAddr = "192.0.2.10".parse().unwrap();
        let host_machine_id: HostMachineId = carbide_uuid::machine::MachineId::new(
            carbide_uuid::machine::MachineIdSource::ProductBoardChassisSerial,
            [7; 32],
            carbide_uuid::machine::MachineType::Host,
        )
        .try_into()
        .unwrap();

        value_scenarios!(
            run = |(event, phase, identity_field, identity): (HostReconciliationFailed, &str, &str, String)| {
                let metrics = MetricsCapture::owned();
                let logs = capture_logs(|| metrics.instrumentation().emit(event));
                assert_eq!(logs.len(), 1);
                assert_eq!(logs[0].level, tracing::Level::WARN);
                assert_eq!(logs[0].field("event_name"), Some("host_reconciliation_failed"));
                assert_eq!(logs[0].field("error"), Some("test failure"));
                assert_eq!(logs[0].field(identity_field), Some(identity.as_str()));
                metrics.counter_delta(
                    "carbide_host_reconciliation_failures_total",
                    &[("phase", phase)],
                )
            };
            "explored host failures retain their BMC address" {
                (HostReconciliationFailed::ExploredHost {
                    host_bmc_ip_address,
                    error: "test failure".to_string(),
                }, "explored_hosts", "host_bmc_ip_address", host_bmc_ip_address.to_string()) => 1.0,
            }
            "boot interface failures retain their machine ID" {
                (HostReconciliationFailed::DesiredBootInterface {
                    host_machine_id,
                    error: "test failure".to_string(),
                }, "desired_boot_interfaces", "host_machine_id", host_machine_id.to_string()) => 1.0,
            }
        );
    }

    #[test]
    fn phase_durations_are_recorded_in_milliseconds() {
        const METRIC: &str = "carbide_host_reconciler_phase_duration_milliseconds";

        value_scenarios!(
            run = |(phase, label)| {
                let metrics = MetricsCapture::owned();
                metrics.instrumentation().emit(HostReconciliationPhaseFinished {
                    phase,
                    duration: Duration::from_millis(20),
                });
                let labels = [("phase", label)];
                (
                    metrics.histogram_count_delta(METRIC, &labels),
                    metrics.histogram_sum_delta(METRIC, &labels),
                )
            };
            "each phase records its duration under its own label" {
                (HostReconciliationPhase::ExploredHosts, "explored_hosts")
                    => (1, ApproxHistogramSum(20.0)),
                (HostReconciliationPhase::DesiredBootInterfaces, "desired_boot_interfaces")
                    => (1, ApproxHistogramSum(20.0)),
            }
        );
    }

    /// Refreshes a changed report ID for the selected MAC into a pending generation while retaining incomplete observations, preventing stale Dell slot targeting without losing independent boot intent.
    #[carbide_test_harness::prelude::sqlx_test]
    async fn reported_boot_interface_id_preserves_selection_and_requires_verification(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::machine::{MachineIdSource, MachineType};
        use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
        use model::site_explorer::{ComputerSystem, EthernetInterface};

        let boot_mac: MacAddress = "02:00:00:00:53:01".parse()?;
        let other_mac: MacAddress = "02:00:00:00:53:02".parse()?;
        for (index, (scenario, reported_mac, reported_ids, expected_id)) in [
            // A new ID for the same MAC must be applied and verified again instead of pinning the former slot.
            (
                "changed ID",
                Some(boot_mac),
                vec![Some("NIC.Slot.8-1-1")],
                "NIC.Slot.8-1-1",
            ),
            // Repeated observations must not reopen an already verified generation.
            (
                "unchanged ID",
                Some(boot_mac),
                vec![Some("NIC.Slot.7-1-1")],
                "NIC.Slot.7-1-1",
            ),
            // NIC mode can remove the selected MAC while leaving its ID, so another NIC must not replace boot intent.
            (
                "omitted MAC",
                None,
                vec![Some("NIC.Slot.8-1-1")],
                "NIC.Slot.7-1-1",
            ),
            // An incomplete ID cannot discard the selected NIC's last-known-good pair.
            ("omitted ID", Some(boot_mac), vec![None], "NIC.Slot.7-1-1"),
            // Padding must be removed once so desired and interface IDs cannot disagree about the same observation.
            (
                "padded ID",
                Some(boot_mac),
                vec![Some("  NIC.Slot.8-1-1\t")],
                "NIC.Slot.8-1-1",
            ),
            // Whitespace alone supplies no usable identity and must preserve the last-known Pair.
            (
                "blank ID",
                Some(boot_mac),
                vec![Some(" \t")],
                "NIC.Slot.7-1-1",
            ),
            // Duplicate MACs use one shared first usable ID for every store; a later blank cannot erase it.
            (
                "duplicate MAC",
                Some(boot_mac),
                vec![Some("NIC.Slot.8-1-1"), Some("NIC.Slot.7-1-1"), Some(" \t")],
                "NIC.Slot.8-1-1",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            // Each host starts with a verified Operator Pair; the report also contains another NIC beside the varied identity.
            let host_id: HostMachineId = MachineId::new(
                MachineIdSource::Tpm,
                [0x60 + index as u8; 32],
                MachineType::Host,
            )
            .try_into()?;
            let mut setup = pool.txn_begin().await?;
            db::machine::create(
                setup.as_pgconn(),
                None,
                &host_id,
                ManagedHostState::Ready,
                None,
                CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
            let desired = db::machine_desired_boot_interface::set(
                setup.as_pgconn(),
                &host_id,
                &MachineBootInterfaceTarget::Pair(MachineBootInterface {
                    mac_address: boot_mac, // Keep the boot choice independent of the other report NIC.
                    interface_id: "NIC.Slot.7-1-1".to_string(),
                }),
                BootInterfaceSelectionSource::Operator,
            )
            .await?
            .desired;
            // Mark the original generation verified to distinguish a changed ID from incomplete initial setup.
            assert_eq!(
                db::machine_desired_boot_interface::mark_verified(
                    setup.as_pgconn(),
                    &host_id,
                    desired.version,
                    chrono::Utc::now(),
                )
                .await?,
                db::ConditionalWrite::Applied(())
            );
            setup.commit().await?;

            // Reload the recorded selection before comparing the report's effects.
            let before = db::machine::find_one(&pool, &host_id, Default::default())
                .await?
                .expect("verified host");
            let selection = before
                .config
                .boot_interface_selection
                .expect("Operator selection");
            assert_eq!(selection.source, BootInterfaceSelectionSource::Operator);
            assert!(selection.updated_at.is_some());

            // Include another NIC beside the selected MAC's varied report identity.
            let report = EndpointExplorationReport {
                systems: vec![ComputerSystem {
                    ethernet_interfaces: std::iter::once(EthernetInterface {
                        mac_address: Some(other_mac),
                        id: Some("NIC.Embedded.1-1-1".to_string()),
                        ..Default::default()
                    })
                    .chain(
                        reported_ids
                            .into_iter()
                            .map(|reported_id| EthernetInterface {
                                mac_address: reported_mac, // Only a complete same-MAC observation may refresh the desired ID.
                                id: reported_id.map(str::to_string),
                                ..Default::default()
                            }),
                    )
                    .collect(),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let interfaces = NicBootInterfaces::new("192.0.2.10".parse()?, &report)
                .expect("the other NIC supplies a complete pair");

            // Reconcile the actual report pair without borrowing an ID from retained interface history.
            let mut refresh = pool.txn_begin().await?;
            HostReconciler::refresh_reported_boot_interface_id(
                refresh.as_pgconn(),
                &host_id,
                &interfaces.interfaces,
            )
            .await?;
            refresh.commit().await?;

            // Reload desired state and its observation to prove changed IDs become pending without reselecting boot intent.
            let after = db::machine::find_one(&pool, &host_id, Default::default())
                .await?
                .expect("host after report refresh");
            let refreshed = after
                .config
                .desired_boot_interface
                .as_ref()
                .expect("desired Pair");
            assert_eq!(
                refreshed.value,
                MachineBootInterfaceTarget::Pair(MachineBootInterface {
                    mac_address: boot_mac,
                    interface_id: expected_id.to_string(),
                }),
                "{scenario}"
            );
            let changed = expected_id != "NIC.Slot.7-1-1";
            assert_eq!(
                refreshed.version.version_nr(),
                desired.version.version_nr() + u64::from(changed),
                "{scenario}"
            );
            assert_eq!(
                after.pending_boot_interface_config_version(),
                changed.then_some(refreshed.version),
                "{scenario}"
            );
            assert_eq!(
                after.config.boot_interface_selection, before.config.boot_interface_selection,
                "{scenario}"
            );
            assert_eq!(
                after.status.boot_interface_status_observation,
                before.status.boot_interface_status_observation,
                "{scenario}"
            );
            assert_eq!(after.state.value, before.state.value, "{scenario}");
            assert_eq!(after.state.version, before.state.version, "{scenario}");
            assert_eq!(
                after.network_config.version, before.network_config.version,
                "{scenario}"
            );
        }
        Ok(())
    }

    /// Rolls back every ID store for a busy host while another host progresses, then retries the same cached reports without a probe so known NIC identities cannot remain split.
    /// An unset stable host remains uninitialized during interface contention and initializes the refreshed ID after retry, avoiding an unnecessary pending generation.
    /// Moving a verified host's BMC address after indexing preserves owned and predicted IDs and its verified Pair until a report at its current address is eligible, preventing a retired source from opening a pending generation.
    /// A deleted indexed BMC must not prevent later reports or the desired-interface sweep from progressing.
    /// Conflicting BMC aliases preserve a verified Pair across repeated passes even when only a nonprimary NIC disagrees; agreeing aliases permit one ID update and then remain stable.
    /// Conflicting aliases also retain a nonprimary NIC's predicted ID until agreement, preventing partial publication from disputed BMC reports.
    #[carbide_test_harness::prelude::sqlx_test]
    async fn reported_boot_ids_retry_cached_reports_without_partial_publication(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::machine::{MachineIdSource, MachineType};
        use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
        use model::machine_interface::InterfaceType;
        use model::machine_interface_address::MachineInterfaceAssociation;
        use model::network_segment::NetworkSegmentControllerState;
        use model::predicted_machine_interface::NewPredictedMachineInterface;
        use model::site_explorer::{ComputerSystem, EthernetInterface};
        use rpc::forge::forge_server::Forge;

        // The harness supplies shared resource handles and the public address API while this proof exercises real per-host ID publication and desired-interface sweeping.
        let harness = carbide_test_harness::TestHarness::builder(pool.clone())
            .build()
            .await;
        let reconciler = HostReconciler::new(
            pool.clone(),
            None,
            crate::config::SiteExplorerConfig::default_machine_creation_timeout(),
            ResourcePoolRegistry::default(),
            harness.api().common_pools().clone(),
            harness.api().instrumentation().clone(),
        );
        let mut setup = pool.txn_begin().await?;
        // Give BMCs Underlay addresses and keep the shared-MAC interfaces in distinct Admin segments.
        let mut segments = Vec::new();
        for index in 0..3 {
            segments.push(
                db::network_segment::persist(
                    model::network_segment::NewNetworkSegment {
                        id: uuid::Uuid::new_v4().into(),
                        name: format!("reported-boot-ids-{index}"),
                        subdomain_id: None,
                        vpc_id: None,
                        mtu: 1500,
                        prefixes: vec![model::network_prefix::NewNetworkPrefix {
                            prefix: format!("198.19.{index}.0/24").parse()?,
                            gateway: Some(format!("198.19.{index}.1").parse()?),
                            dhcpv6_link_address: None,
                            num_reserved: 3,
                        }],
                        vlan_id: None,
                        vni: None,
                        segment_type: if index == 0 {
                            NetworkSegmentType::Underlay
                        } else {
                            NetworkSegmentType::Admin
                        },
                        can_stretch: None,
                        allocation_strategy: model::network_segment::AllocationStrategy::Dynamic,
                        infer_slaac_eui64_addresses: false,
                    },
                    setup.as_pgconn(),
                    NetworkSegmentControllerState::Ready,
                )
                .await?,
            );
        }

        // Two hosts have verified Operator Pairs; a third stable Ready host has no desired metadata yet.
        // All three retain owned and predicted rows for their selected MAC so publication must settle both stores before completing intent.
        let mut hosts = Vec::new();
        let mut reports = Vec::new();
        for index in 0..3_u8 {
            let host_id: HostMachineId =
                MachineId::new(MachineIdSource::Tpm, [0x70 + index; 32], MachineType::Host)
                    .try_into()?;
            let boot_mac = MacAddress::new([2, 0, 0, 0, 0x70 + index, 1]);
            db::machine::create(
                setup.as_pgconn(),
                None,
                &host_id,
                ManagedHostState::Ready,
                None,
                CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
            let bmc = db::machine_interface::create(
                setup.as_pgconn(),
                std::slice::from_ref(&segments[0]),
                &MacAddress::new([2, 0, 0, 0, 0x70 + index, 0xff]),
                false,
                model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
                None,
            )
            .await?;
            db::machine_interface::associate_interface_with_machine(
                &bmc.id,
                MachineInterfaceAssociation::Machine(host_id.into()),
                setup.as_pgconn(),
            )
            .await?;
            db::machine_interface::set_interface_type(
                &bmc.id,
                InterfaceType::Bmc,
                setup.as_pgconn(),
            )
            .await?;
            let nic = db::machine_interface::create(
                setup.as_pgconn(),
                std::slice::from_ref(&segments[1]),
                &boot_mac,
                true,
                model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
                None,
            )
            .await?;
            db::machine_interface::associate_interface_with_machine(
                &nic.id,
                MachineInterfaceAssociation::Machine(host_id.into()),
                setup.as_pgconn(),
            )
            .await?;
            db::machine_interface::set_boot_interface_id(
                boot_mac,
                "NIC.Slot.7-1-1",
                setup.as_pgconn(),
            )
            .await?;
            db::predicted_machine_interface::create(
                NewPredictedMachineInterface {
                    machine_id: &host_id,
                    mac_address: boot_mac, // The same MAC in both stores must commit with desired intent.
                    expected_network_segment_type: NetworkSegmentType::HostInband,
                    boot_interface_id: Some("NIC.Slot.7-1-1"),
                    primary_interface: true,
                },
                setup.as_pgconn(),
            )
            .await?;
            // Leave the third host without desired metadata so the independent sweep can expose premature initialization.
            if index < 2 {
                let desired = db::machine_desired_boot_interface::set(
                    setup.as_pgconn(),
                    &host_id,
                    &MachineBootInterfaceTarget::Pair(MachineBootInterface {
                        mac_address: boot_mac,
                        interface_id: "NIC.Slot.7-1-1".to_string(),
                    }),
                    BootInterfaceSelectionSource::Operator,
                )
                .await?
                .desired;
                // Verify the old Pair so a changed ID must open a pending generation instead of carrying verification forward.
                assert_eq!(
                    db::machine_desired_boot_interface::mark_verified(
                        setup.as_pgconn(),
                        &host_id,
                        desired.version,
                        chrono::Utc::now(),
                    )
                    .await?,
                    db::ConditionalWrite::Applied(()),
                );
            }
            let mut bmc = db::machine_interface::find_one(setup.as_pgconn(), bmc.id).await?;
            let address = *bmc.addresses.first().expect("BMC address");
            // Simulate the pre-creation index while requiring the publisher to use the live persisted association.
            bmc.machine_id = None;
            hosts.push((host_id, bmc, nic.id, boot_mac));
            reports.push((
                address,
                EndpointExplorationReport {
                    endpoint_type: EndpointType::Bmc,
                    systems: vec![ComputerSystem {
                        ethernet_interfaces: vec![EthernetInterface {
                            mac_address: Some(boot_mac),
                            id: Some("  NIC.Slot.9-1-1\t".to_string()), // All stores must receive the same canonical ID.
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ));
        }

        // Reuse the selected MAC on another host and segment to detect accidental global-by-MAC writes.
        let foreign = db::machine_interface::create(
            setup.as_pgconn(),
            std::slice::from_ref(&segments[2]),
            &hosts[0].3,
            false,
            model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        db::machine_interface::associate_interface_with_machine(
            &foreign.id,
            MachineInterfaceAssociation::Machine(hosts[1].0.into()),
            setup.as_pgconn(),
        )
        .await?;
        setup.commit().await?;
        let mut before = Vec::new();
        for (host_id, _, _, _) in &hosts {
            before.push(
                db::machine::find_one(&pool, host_id, Default::default())
                    .await?
                    .expect("host"),
            );
        }
        // Model an unowned BMC deleted after indexing and order its report before the live hosts to exercise per-report recovery.
        let deleted_bmc_address = "198.19.0.2".parse()?;
        let mut deleted_bmc = hosts[0].1.clone();
        deleted_bmc.id = uuid::Uuid::new_v4().into();
        deleted_bmc.mac_address = MacAddress::new([2, 0, 0, 0, 0x7f, 0xff]);
        deleted_bmc.addresses = vec![deleted_bmc_address];
        reports.push((deleted_bmc_address, reports[0].1.clone()));
        let index = ExploredEndpointIndex::builder(
            Vec::new(),
            hosts
                .iter()
                .map(|(_, bmc, _, _)| bmc.clone())
                .chain(std::iter::once(deleted_bmc))
                .collect(),
        )
        .build();
        let ingested = reports.iter().map(|(address, _)| *address).collect();
        let probed = HashSet::new();

        // Block the first verified host at its machine row and the unset host at its NIC while leaving that host's machine group free.
        let mut blocker = pool.txn_begin().await?;
        db::machine::lock_by_id_for_update(blocker.as_pgconn(), hosts[0].0.as_machine_id()).await?;
        db::machine_interface::lock_for_address_assignment(blocker.as_pgconn(), hosts[2].2).await?;
        tokio::time::timeout(
            Duration::from_secs(5),
            reconciler.run_single_iteration(
                std::iter::empty(),
                reports.iter().map(|(address, report)| (*address, report)),
                &index,
                &ingested,
                &probed,
                &probed,
            ),
        )
        .await??;

        // Reload every ID store: the busy host stays verified at its old ID and the other host commits a pending Pair.
        for (position, (host_id, _, nic_id, boot_mac)) in hosts.iter().enumerate() {
            let changed = position == 1;
            let expected_id = if changed {
                "NIC.Slot.9-1-1"
            } else {
                "NIC.Slot.7-1-1"
            };
            let current = db::machine::find_one(&pool, host_id, Default::default())
                .await?
                .expect("host");
            if position == 2 {
                // The independent sweep must not complete this host from the stored ID while its report publication is blocked.
                assert!(current.config.desired_boot_interface.is_none());
                assert!(current.pending_boot_interface_config_version().is_none());
            } else {
                let desired = current
                    .config
                    .desired_boot_interface
                    .as_ref()
                    .expect("desired Pair");
                assert_eq!(desired.value.interface_id(), Some(expected_id));
                assert_eq!(
                    current.pending_boot_interface_config_version(),
                    changed.then_some(desired.version)
                );
            }
            assert_eq!(
                current.config.boot_interface_selection,
                before[position].config.boot_interface_selection
            );
            assert_eq!(
                db::machine_interface::find_one(&pool, *nic_id)
                    .await?
                    .boot_interface_id
                    .as_deref(),
                Some(expected_id)
            );
            assert_eq!(
                db::predicted_machine_interface::find_by_mac_address(
                    &mut *pool.acquire().await?,
                    *boot_mac,
                )
                .await?
                .expect("prediction")
                .boot_interface_id
                .as_deref(),
                Some(expected_id)
            );
        }
        assert_eq!(
            db::machine_interface::find_one(&pool, foreign.id)
                .await?
                .boot_interface_id,
            None
        );

        // Release the contention locks before exercising the independent BMC address update.
        blocker.rollback().await?;

        // Retire the verified host's indexed address after exercising contention, retaining its BMC interface and captured report.
        let moved_address: IpAddr = "198.19.0.200".parse()?;
        let moved = harness
            .api()
            .assign_static_address(tonic::Request::new(
                rpc::forge::AssignStaticAddressRequest {
                    interface_id: Some(hosts[0].1.id),
                    ip_address: moved_address.to_string(),
                },
            ))
            .await?
            .into_inner();
        assert_eq!(moved.interface_id, Some(hosts[0].1.id));
        assert_eq!(moved.ip_address, "198.19.0.200");

        // Verify the indexed IP no longer resolves while the same BMC interface remains associated with the original host.
        let persisted_bmc = db::machine_interface::find_one(&pool, hosts[0].1.id).await?;
        assert_eq!(persisted_bmc.machine_id, Some(hosts[0].0.into()));
        assert!(persisted_bmc.addresses.contains(&moved_address));
        assert!(
            db::machine_interface::find_by_ip(&pool, reports[0].0)
                .await?
                .is_none()
        );

        // Retry the captured reports and index so the unset host progresses while the retired source is rejected.
        tokio::time::timeout(
            Duration::from_secs(5),
            reconciler.run_single_iteration(
                std::iter::empty(),
                reports.iter().map(|(address, report)| (*address, report)),
                &index,
                &ingested,
                &probed,
                &probed,
            ),
        )
        .await??;

        // Reload desired intent, its verification and selection metadata, and owned and predicted IDs to prove the retired report changed none.
        let after_retired_report = db::machine::find_one(&pool, &hosts[0].0, Default::default())
            .await?
            .expect("host after retired report");
        assert_eq!(
            after_retired_report.config.desired_boot_interface,
            before[0].config.desired_boot_interface
        );
        assert_eq!(
            after_retired_report.config.boot_interface_selection,
            before[0].config.boot_interface_selection
        );
        assert_eq!(
            after_retired_report
                .status
                .boot_interface_status_observation,
            before[0].status.boot_interface_status_observation
        );
        assert!(
            after_retired_report
                .pending_boot_interface_config_version()
                .is_none()
        );
        assert_eq!(
            db::machine_interface::find_one(&pool, hosts[0].2)
                .await?
                .boot_interface_id
                .as_deref(),
            Some("NIC.Slot.7-1-1")
        );
        assert_eq!(
            db::predicted_machine_interface::find_by_mac_address(&pool, hosts[0].3)
                .await?
                .expect("retained prediction")
                .boot_interface_id
                .as_deref(),
            Some("NIC.Slot.7-1-1")
        );

        // Verify the independent host initialized from its cached report before any current-address report is supplied.
        let cached_retry = db::machine::find_one(&pool, &hosts[2].0, Default::default())
            .await?
            .expect("host after cached retry");
        assert_eq!(
            cached_retry
                .config
                .desired_boot_interface
                .as_ref()
                .expect("cached retry initialized intent")
                .value
                .interface_id(),
            Some("NIC.Slot.9-1-1")
        );

        // Supply the moved BMC's report with its current address and rebuilt index so ordinary publication can advance exactly once.
        reports[0].0 = moved_address;
        let current_index = ExploredEndpointIndex::builder(Vec::new(), vec![persisted_bmc]).build();
        let current_ingested = HashSet::from([moved_address]);
        reconciler
            .run_single_iteration(
                std::iter::empty(),
                std::iter::once((moved_address, &reports[0].1)),
                &current_index,
                &current_ingested,
                &probed,
                &probed,
            )
            .await?;

        // Fresh reads prove all three stores agree, each decision is preserved, and the already completed host did not churn its generation.
        for (position, (host_id, _, nic_id, boot_mac)) in hosts.iter().enumerate() {
            let current = db::machine::find_one(&pool, host_id, Default::default())
                .await?
                .expect("host");
            let desired = current
                .config
                .desired_boot_interface
                .as_ref()
                .expect("desired Pair");
            assert_eq!(desired.value.interface_id(), Some("NIC.Slot.9-1-1"));
            if let Some(previous) = before[position].config.desired_boot_interface.as_ref() {
                // Existing decisions keep their authority and open one pending generation for the changed ID.
                assert_eq!(
                    desired.version.version_nr(),
                    previous.version.version_nr() + 1
                );
                assert_eq!(
                    current.pending_boot_interface_config_version(),
                    Some(desired.version)
                );
                assert_eq!(
                    current.config.boot_interface_selection,
                    before[position].config.boot_interface_selection
                );
            } else {
                // An unset stable host initializes the fresh ID directly as its assumed baseline, avoiding remediation churn.
                assert!(current.pending_boot_interface_config_version().is_none());
                let observation = current
                    .status
                    .boot_interface_status_observation
                    .as_ref()
                    .expect("assumed baseline");
                assert!(observation.assumed);
                assert_eq!(observation.config_version, desired.version);
            }
            assert_eq!(current.state.version, before[position].state.version);
            assert_eq!(
                current.network_config.version,
                before[position].network_config.version
            );
            assert_eq!(
                db::machine_interface::find_one(&pool, *nic_id)
                    .await?
                    .boot_interface_id
                    .as_deref(),
                Some("NIC.Slot.9-1-1")
            );
            assert_eq!(
                db::predicted_machine_interface::find_by_mac_address(
                    &mut *pool.acquire().await?,
                    *boot_mac,
                )
                .await?
                .expect("prediction")
                .boot_interface_id
                .as_deref(),
                Some("NIC.Slot.9-1-1")
            );
        }
        assert_eq!(
            db::machine_interface::find_one(&pool, foreign.id)
                .await?
                .boot_interface_id,
            None
        );
        // Verify the settled Pair before adding aliases that agree on a selected-MAC replacement but disagree on a secondary NIC.
        let alias_address = "2001:db8::1".parse()?;
        let secondary_mac = MacAddress::new([2, 0, 0, 0, 0x70, 2]);
        let mut setup = pool.txn_begin().await?;
        let selected = db::machine_desired_boot_interface::get(setup.as_pgconn(), &hosts[0].0)
            .await?
            .expect("settled Pair");
        assert_eq!(
            db::machine_desired_boot_interface::mark_verified(
                setup.as_pgconn(),
                &hosts[0].0,
                selected.version,
                chrono::Utc::now()
            )
            .await?,
            db::ConditionalWrite::Applied(())
        );
        db::machine_interface_address::insert(
            setup.as_pgconn(),
            hosts[0].1.id,
            alias_address,
            model::allocation_type::AllocationType::Dhcp,
        )
        .await?;
        // Seed the shared secondary NIC with an older nonprimary prediction to expose partial publication.
        db::predicted_machine_interface::create(
            NewPredictedMachineInterface {
                machine_id: &hosts[0].0,
                mac_address: secondary_mac,
                expected_network_segment_type: NetworkSegmentType::HostInband,
                boot_interface_id: Some("NIC.Embedded.1"), // Distinguish retained state from the reported replacement.
                primary_interface: false, // Exercise a NIC other than the selected primary.
            },
            setup.as_pgconn(),
        )
        .await?;
        setup.commit().await?;
        let alias_before = db::machine::find_one(&pool, &hosts[0].0, Default::default())
            .await?
            .expect("verified host");
        let unaffected_before = db::machine_desired_boot_interface::get(&pool, &hosts[1].0)
            .await?
            .expect("unrelated Pair");
        let dual_stack_bmc = db::machine_interface::find_one(&pool, hosts[0].1.id).await?;
        assert!(dual_stack_bmc.addresses.contains(&alias_address));
        let alias_index =
            ExploredEndpointIndex::builder(Vec::new(), vec![dual_stack_bmc, hosts[1].1.clone()])
                .build();
        // Give the selected MAC a replacement ID before constructing a disagreement limited to the secondary NIC.
        let mut alias_report = reports[0].1.clone();
        alias_report.systems[0].ethernet_interfaces[0].id = Some("NIC.Slot.10-1-1".to_string());
        alias_report.systems[0]
            .ethernet_interfaces
            .push(EthernetInterface {
                mac_address: Some(secondary_mac),
                id: Some("NIC.Embedded.2".to_string()),
                ..Default::default()
            });
        // Retain the IPv4 secondary ID while keeping its selected-MAC replacement identical to IPv6.
        let mut ipv4_report = alias_report.clone();
        ipv4_report.systems[0].ethernet_interfaces[1].id = Some("NIC.Embedded.1".to_string());
        let mut unaffected_report = reports[1].1.clone();
        unaffected_report.systems[0].ethernet_interfaces[0].id =
            Some("NIC.Slot.10-1-1".to_string());
        let mut alias_reports = vec![
            (reports[0].0, ipv4_report),
            (alias_address, alias_report),
            (reports[1].0, unaffected_report),
        ];
        let alias_ingested = alias_reports.iter().map(|(address, _)| *address).collect();
        for (scenario, agreeing) in [
            // A secondary-NIC conflict must prevent both agreeing selected-MAC replacements from invalidating a verified Pair.
            ("first conflicting pass", false),
            // A second pass must preserve the same generation rather than oscillating twice in IP order.
            ("repeated conflicting pass", false),
            // Agreement admits one canonical replacement through the ordinary publication transaction.
            ("aliases agree", true),
            // Replaying both agreeing aliases must not require another verification generation.
            ("agreeing aliases replay", true),
        ] {
            if agreeing {
                // Padding differs from the original alias so agreement is based on the shared canonical ID.
                alias_reports[0].1.systems[0].ethernet_interfaces[1].id =
                    Some(" NIC.Embedded.2 ".to_string());
            }
            reconciler
                .run_single_iteration(
                    std::iter::empty(),
                    alias_reports
                        .iter()
                        .map(|(address, report)| (*address, report)),
                    &alias_index,
                    &alias_ingested,
                    &probed,
                    &probed,
                )
                .await?;

            // Fresh reads prove all ID stores agree with the admitted generation while retaining the selection decision.
            let expected_id = if agreeing {
                "NIC.Slot.10-1-1"
            } else {
                "NIC.Slot.9-1-1"
            };
            let current = db::machine::find_one(&pool, &hosts[0].0, Default::default())
                .await?
                .expect("alias host");
            let desired = current
                .config
                .desired_boot_interface
                .as_ref()
                .expect("desired Pair");
            assert_eq!(
                desired.value.interface_id(),
                Some(expected_id),
                "{scenario}"
            );
            assert_eq!(
                desired.version.version_nr(),
                alias_before
                    .config
                    .desired_boot_interface
                    .as_ref()
                    .expect("prior Pair")
                    .version
                    .version_nr()
                    + if agreeing { 1 } else { 0 },
                "{scenario}"
            );
            assert_eq!(
                current.config.boot_interface_selection,
                alias_before.config.boot_interface_selection,
                "{scenario}"
            );
            assert_eq!(
                current.pending_boot_interface_config_version(),
                agreeing.then_some(desired.version),
                "{scenario}"
            );
            if !agreeing {
                // Conflicting evidence must retain the exact prior verification, not merely leave a stable final ID.
                assert_eq!(
                    current.status.boot_interface_status_observation,
                    alias_before.status.boot_interface_status_observation,
                    "{scenario}"
                );
            }
            assert_eq!(
                db::machine_interface::find_one(&pool, hosts[0].2)
                    .await?
                    .boot_interface_id
                    .as_deref(),
                Some(expected_id),
                "{scenario}"
            );
            assert_eq!(
                db::predicted_machine_interface::find_by_mac_address(
                    &mut *pool.acquire().await?,
                    hosts[0].3
                )
                .await?
                .expect("prediction")
                .boot_interface_id
                .as_deref(),
                Some(expected_id),
                "{scenario}"
            );

            // Preserve the nonprimary prediction during conflict and admit its new ID only after alias agreement.
            assert_eq!(
                db::predicted_machine_interface::find_by_mac_address(&pool, secondary_mac)
                    .await?
                    .expect("shared secondary NIC prediction")
                    .boot_interface_id
                    .as_deref(),
                Some(if agreeing {
                    "NIC.Embedded.2"
                } else {
                    "NIC.Embedded.1"
                }),
                "{scenario}"
            );

            // Another BMC must publish during the conflict passes and retain that one generation afterward.
            let unaffected = db::machine_desired_boot_interface::get(&pool, &hosts[1].0)
                .await?
                .expect("unrelated Pair");
            assert_eq!(
                unaffected.value.interface_id(),
                Some("NIC.Slot.10-1-1"),
                "{scenario}"
            );
            assert_eq!(
                unaffected.version.version_nr(),
                unaffected_before.version.version_nr() + 1,
                "{scenario}"
            );
        }
        Ok(())
    }

    /// Enriches pending BF4 Operator MacOnly intent from a newly reported same-MAC ID without changing its selection decision, protecting enrichment from an overbroad pending guard.
    #[carbide_test_harness::prelude::sqlx_test]
    async fn pending_bf4_operator_boot_enriches_same_mac_from_report(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::machine::{DpuMachineId, MachineIdSource, MachineType};
        use model::hardware_info::{DpuData, HardwareInfo};
        use model::machine::{
            CURRENT_STATE_MODEL_VERSION, DpuInitState, DpuInitStates, ManagedHostState,
        };
        use model::machine_interface_address::MachineInterfaceAssociation;
        use model::network_segment::NetworkSegmentControllerState;
        use model::predicted_machine_interface::NewPredictedMachineInterface;
        use model::site_explorer::{ComputerSystem, EndpointExplorationReport, EthernetInterface};

        // Attach a BF4 DPU without PF facts to a host in DPUInit/WaitingForNetworkConfig; the remaining fixture supplies independent Operator boot intent.
        let host_id: HostMachineId =
            MachineId::new(MachineIdSource::Tpm, [0x51; 32], MachineType::Host).try_into()?;
        let dpu_id: DpuMachineId = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x52; 32],
            MachineType::Dpu,
        )
        .try_into()?;
        let state = ManagedHostState::DPUInit {
            dpu_states: DpuInitStates {
                states: HashMap::from([(dpu_id, DpuInitState::WaitingForNetworkConfig)]),
            },
        };
        let boot_mac: MacAddress = "02:00:00:00:51:09".parse()?;
        let mut setup = pool.txn_begin().await?;
        for id in [*host_id.as_machine_id(), *dpu_id.as_machine_id()] {
            db::machine::create(
                setup.as_pgconn(),
                None,
                &id,
                state.clone(),
                None,
                CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
        }
        let hardware = HardwareInfo {
            dpu_info: Some(DpuData {
                part_number: "900-9D4B4-CWAA-TSA".to_string(), // Retain BF4 classification while selected PF acquisition is incomplete.
                factory_mac_address: "02:00:00:00:51:01".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        db::machine_topology::create_or_update(
            setup.as_pgconn(),
            dpu_id.as_machine_id(),
            &hardware,
        )
        .await?;
        let segment = db::network_segment::persist(
            model::network_segment::NewNetworkSegment {
                id: uuid::Uuid::new_v4().into(),
                name: "pending-enrichment".to_string(),
                subdomain_id: None,
                vpc_id: None,
                mtu: 1500,
                prefixes: vec![model::network_prefix::NewNetworkPrefix {
                    prefix: "192.0.2.0/24".parse()?,
                    gateway: Some("192.0.2.1".parse()?),
                    dhcpv6_link_address: None,
                    num_reserved: 3,
                }],
                vlan_id: None,
                vni: None,
                segment_type: NetworkSegmentType::Admin,
                can_stretch: None,
                allocation_strategy: model::network_segment::AllocationStrategy::Dynamic,
                infer_slaac_eui64_addresses: false,
            },
            setup.as_pgconn(),
            NetworkSegmentControllerState::Ready,
        )
        .await?;
        let primary = db::machine_interface::create(
            setup.as_pgconn(),
            std::slice::from_ref(&segment),
            &"02:00:00:00:51:01".parse()?,
            true,
            model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        db::machine_interface::associate_interface_with_machine(
            &primary.id,
            MachineInterfaceAssociation::Machine(*host_id.as_machine_id()),
            setup.as_pgconn(),
        )
        .await?;
        db::machine_interface::associate_interface_with_dpu_machine(
            &primary.id,
            dpu_id.as_machine_id(),
            setup.as_pgconn(),
        )
        .await?;
        db::predicted_machine_interface::create(
            NewPredictedMachineInterface {
                machine_id: &host_id,
                mac_address: boot_mac,
                expected_network_segment_type: NetworkSegmentType::HostInband,
                boot_interface_id: None,
                primary_interface: true,
            },
            setup.as_pgconn(),
        )
        .await?;
        db::machine_desired_boot_interface::set(
            setup.as_pgconn(),
            &host_id,
            &MachineBootInterfaceTarget::MacOnly(boot_mac),
            BootInterfaceSelectionSource::Operator,
        )
        .await?;
        // Bind the BMC before taking the baseline so subsequent assertions isolate ID enrichment.
        let bmc = db::machine_interface::create(
            setup.as_pgconn(),
            std::slice::from_ref(&segment),
            &"02:00:00:00:51:ff".parse()?,
            false,
            model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        db::machine_interface::associate_interface_with_machine(
            &bmc.id,
            MachineInterfaceAssociation::Machine(*host_id.as_machine_id()),
            setup.as_pgconn(),
        )
        .await?;
        db::machine_interface::set_interface_type(
            &bmc.id,
            model::machine_interface::InterfaceType::Bmc,
            setup.as_pgconn(),
        )
        .await?;
        setup.commit().await?;
        let before = db::machine::find_one(&pool, &host_id, Default::default())
            .await?
            .expect("pending host");
        assert_eq!(
            before
                .config
                .desired_boot_interface
                .as_ref()
                .expect("MAC-only intent")
                .value,
            MachineBootInterfaceTarget::MacOnly(boot_mac)
        );
        let operator_selection = before
            .config
            .boot_interface_selection
            .expect("Operator decision");
        assert_eq!(
            operator_selection.source,
            BootInterfaceSelectionSource::Operator
        );
        assert!(operator_selection.updated_at.is_some());

        // Read the persisted BMC address because publication requires the report's source address to remain assigned.
        let report_ip = db::machine_interface::find_one(&pool, bmc.id)
            .await?
            .addresses
            .into_iter()
            .next()
            .expect("assigned BMC address");
        let report = EndpointExplorationReport {
            endpoint_type: model::site_explorer::EndpointType::Bmc,
            machine_id: Some(*host_id.as_machine_id()),
            systems: vec![ComputerSystem {
                ethernet_interfaces: vec![EthernetInterface {
                    mac_address: Some(boot_mac),
                    id: Some("NIC.Embedded.1-1-1".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        // Publish a new ID through the same scoped interface and desired writers while BF4 initialization remains pending.
        let mut txn = pool.txn_begin().await?;
        assert!(db::machine::bf4_host_pf_initialization_pending(txn.as_pgconn(), &host_id).await?);
        let interfaces = NicBootInterfaces::new(report_ip, &report).expect("reported boot pair");
        assert_eq!(interfaces.persist(&mut txn, bmc.id).await?, Some(host_id));
        HostReconciler::refresh_reported_boot_interface_id(
            txn.as_pgconn(),
            &host_id,
            &interfaces.interfaces,
        )
        .await?;
        txn.commit().await?;

        // Reload the desired Pair and unchanged decision after commit while the host remains pending and unfinalized.
        let after = db::machine::find_one(&pool, &host_id, Default::default())
            .await?
            .expect("enriched host");
        assert_eq!(
            after
                .config
                .desired_boot_interface
                .expect("enriched desired target")
                .value,
            MachineBootInterfaceTarget::Pair(MachineBootInterface {
                mac_address: boot_mac,
                interface_id: "NIC.Embedded.1-1-1".to_string()
            })
        );
        assert_eq!(
            after.config.boot_interface_selection,
            before.config.boot_interface_selection
        );
        assert_eq!(after.state.value, before.state.value);
        assert_eq!(after.state.version, before.state.version);
        assert_eq!(after.network_config.version, before.network_config.version);
        Ok(())
    }
}
