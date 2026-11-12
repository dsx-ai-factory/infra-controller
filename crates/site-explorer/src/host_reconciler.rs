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

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use carbide_instrument::{Event, Instrumentation, LabelValue};
use carbide_resource_pool::ResourcePoolRegistry;
use carbide_uuid::machine::HostMachineId;
use db::Transaction;
use db::machine_interface::MachineInterfaces;
use futures_util::future::join_all;
use itertools::Itertools;
use mac_address::MacAddress;
use model::machine_boot_interface::MachineBootInterfaceTarget;
use model::resource_pool::common::CommonPools;
use model::site_explorer::{
    EndpointExplorationReport, EndpointType, ExploredDpu, ExploredManagedHost,
};
use sqlx::{PgConnection, PgPool};
use tokio_util::sync::CancellationToken;

use crate::boot_interfaces::{initial_selection_source, zero_dpu_primary_mac};
use crate::dpu_asn::PreparedDpuAsn;
use crate::dpu_loopback::{PreparedDpuLoopback, acquire_admin_admission};
use crate::endpoint_exploration_report_ext::EndpointExplorationReportExt;
use crate::errors::SiteExplorerResult;
use crate::explored_endpoint_index::ExploredEndpointIndex;
use crate::machine_interfaces::MachineInterfacesExt;
use crate::managed_host::{ZeroDpuHostLookup, find_zero_dpu_host};
use crate::metrics::SiteExplorerHostAdminAddressReconcileFailed;

const DESIRED_BOOT_INTERFACE_RECONCILE_PAGE_SIZE: i64 = 100;

/// Reconciles persisted state of existing hosts.
pub(super) struct HostReconciler {
    instrumentation: Instrumentation,
    database_connection: PgPool,
    retained_boot_interface_window: Option<chrono::Duration>,
    resource_pool_registry: ResourcePoolRegistry,
    common_pools: Arc<CommonPools>,
    pub(super) preparation_cancel_token: CancellationToken,
}

impl HostReconciler {
    pub(super) fn new(
        database_connection: PgPool,
        retained_boot_interface_window: Option<chrono::Duration>,
        resource_pool_registry: ResourcePoolRegistry,
        common_pools: Arc<CommonPools>,
        instrumentation: Instrumentation,
    ) -> Self {
        Self {
            instrumentation,
            database_connection,
            retained_boot_interface_window,
            resource_pool_registry,
            common_pools,
            preparation_cancel_token: CancellationToken::new(),
        }
    }

    /// Reconciles existing hosts selected from shared observations,
    /// then sweeps incomplete desired boot interfaces.
    ///
    /// The notification contains probed BMC addresses, including failed probes.
    /// Cached reports remain usable. Hosts with an unowned BMC are also retried
    /// without a fresh probe. Work selection uses neither ExpectedMachine nor
    /// creation queues, caps, or the `create_machines` setting.
    ///
    /// Host reports include fresh, preingestion-ready observations rejected by
    /// matching; BMC suppression has already been applied. The BMC ownership
    /// snapshot was loaded before creation. Sharing these inputs avoids reloading
    /// the inventory; persisted interface ownership is checked before each repair.
    pub(super) async fn run_single_iteration<'a>(
        &self,
        hosts: impl Iterator<Item = &'a ExploredManagedHost>,
        host_reports: impl Iterator<Item = (IpAddr, &'a EndpointExplorationReport)>,
        endpoint_index: &ExploredEndpointIndex,
        ingested_bmc_ips: &HashSet<IpAddr>,
        explored_this_run: &HashSet<IpAddr>,
    ) -> SiteExplorerResult<()> {
        let start = Instant::now();
        self.reconcile_hosts_with_dpus(hosts, ingested_bmc_ips, explored_this_run)
            .await;
        self.reconcile_hosts_without_dpus(
            host_reports,
            endpoint_index,
            ingested_bmc_ips,
            explored_this_run,
        )
        .await;
        self.instrumentation.emit(HostReconciliationPhaseFinished {
            phase: HostReconciliationPhase::ExploredHosts,
            duration: start.elapsed(),
        });

        let start = Instant::now();
        let result = self.reconcile_desired_boot_interfaces().await;
        self.instrumentation.emit(HostReconciliationPhaseFinished {
            phase: HostReconciliationPhase::DesiredBootInterfaces,
            duration: start.elapsed(),
        });
        result
    }

    async fn reconcile_hosts_with_dpus<'a>(
        &self,
        hosts: impl Iterator<Item = &'a ExploredManagedHost>,
        ingested_bmc_ips: &HashSet<IpAddr>,
        explored_this_run: &HashSet<IpAddr>,
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
            if let Err(error) = self.reconcile_host_with_dpus(host).await {
                self.instrumentation
                    .emit(HostReconciliationFailed::ExploredHost {
                        host_bmc_ip_address: host.host_bmc_ip,
                        error: error.to_string(),
                    });
            }
        }
    }

    async fn reconcile_hosts_without_dpus<'a>(
        &self,
        host_reports: impl Iterator<Item = (IpAddr, &'a EndpointExplorationReport)>,
        endpoint_index: &ExploredEndpointIndex,
        ingested_bmc_ips: &HashSet<IpAddr>,
        explored_this_run: &HashSet<IpAddr>,
    ) {
        // Creation policy may exclude an existing zero-DPU host from matching
        // after its ExpectedMachine is removed. Use the shared BMC reports and
        // validate persisted interface ownership before refreshing an existing host.
        let host_reports = host_reports
            .filter(|(address, report)| {
                report.endpoint_type == EndpointType::Bmc
                    && !report.is_dpu()
                    && !report.is_switch()
                    && !report.is_power_shelf()
                    && (explored_this_run.contains(address) || !ingested_bmc_ips.contains(address))
                    && endpoint_index
                        .underlay_interface(address)
                        .is_some_and(|interface| interface.machine_id.is_some())
            })
            .sorted_by_key(|(address, _)| *address);
        for (address, report) in host_reports {
            if let Err(error) = self
                .reconcile_host_without_dpus(address, report, endpoint_index)
                .await
            {
                self.instrumentation
                    .emit(HostReconciliationFailed::ExploredHost {
                        host_bmc_ip_address: address,
                        error: error.to_string(),
                    });
            }
        }
    }

    async fn reconcile_host_without_dpus(
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
        let mut txn = Transaction::begin(&self.database_connection).await?;
        let (machine_id, predicted_id): (HostMachineId, _) =
            match find_zero_dpu_host(txn.as_pgconn(), bmc_interface.mac_address, &mac_addresses)
                .await?
            {
                ZeroDpuHostLookup::Predicted(machine_id) => (machine_id.into(), Some(machine_id)),
                ZeroDpuHostLookup::Stable(machine_id) => (machine_id.into(), None),
                ZeroDpuHostLookup::NotFound | ZeroDpuHostLookup::Conflict => {
                    txn.rollback().await?;
                    return Ok(());
                }
            };
        if let Some(predicted_id) = predicted_id {
            // Release the read transaction before waiting for admission so the
            // queue cannot occupy pool connections. Revalidate after the wait.
            txn.rollback().await?;
            _admin_admission = Some(db::machine_interface::admin_lock_admission().await);
            txn = Transaction::begin(&self.database_connection).await?;
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
            let report_boot_interface_ids = mac_addresses
                .iter()
                .filter_map(|mac| report.find_interface_id_for_mac(*mac).map(|id| (*mac, id)))
                .collect::<Vec<_>>();
            // Preserve the lock order against DHCP and admin address repair.
            db::machine_interface::lock_all_admin_segments(txn.as_pgconn()).await?;
            crate::host_interfaces::reconcile(
                txn.as_pgconn(),
                &machine_id,
                &mac_addresses,
                &report_boot_interface_ids,
                primary_mac,
            )
            .await?;
        }
        Self::reconcile_desired_boot_interface(
            txn.as_pgconn(),
            &machine_id,
            predicted_id.and(declared_primary),
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn reconcile_host_with_dpus(&self, host: &ExploredManagedHost) -> SiteExplorerResult<()> {
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
            let mut txn = Transaction::begin(&self.database_connection).await?;
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
            Self::reconcile_desired_boot_interface(txn.as_pgconn(), &host_machine_id, None).await?;
            for prepared in &loopbacks {
                prepared.attach(&mut txn).await?;
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
        if let Err(error) = db::machine_interface::reconcile_admin_addresses_after_commit(
            &self.database_connection,
            &host_machine_id,
        )
        .await
        {
            self.instrumentation
                .emit(SiteExplorerHostAdminAddressReconcileFailed {
                    host_machine_id,
                    error: error.to_string(),
                });
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
    async fn reconcile_desired_boot_interfaces(&self) -> SiteExplorerResult<()> {
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
                let mut txn = Transaction::begin(&self.database_connection)
                    .await
                    .inspect_err(|error| {
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
    use carbide_instrument::emit;
    use carbide_instrument::testing::{ApproxHistogramSum, MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;

    use super::*;

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
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| emit(event));
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
                let metrics = MetricsCapture::start();
                emit(HostReconciliationPhaseFinished {
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
}
