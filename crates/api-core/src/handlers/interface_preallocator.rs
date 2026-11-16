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

use carbide_instrument::Instrumentation;
use carbide_network::ip::IdentifyAddressFamily;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::expected_machine::{ExpectedInterface, ExpectedMachine};
use model::machine_interface::InterfaceType;
use model::network_segment::NetworkSegmentType;

use crate::CarbideError;
use crate::handlers::static_address_metrics::{
    PreallocationSuccess, StaticAddressPreallocationCompleted,
};

/// Reconciles fixed addresses using the caller's retention policy and metric destination.
///
/// The caller owns the outer transaction. Failed attempts are recorded immediately;
/// successful outcomes must be recorded only after that transaction commits.
pub(super) struct InterfacePreallocator<'a> {
    instrumentation: &'a Instrumentation,
    retained_window: Option<chrono::Duration>,
}

impl<'a> InterfacePreallocator<'a> {
    pub(super) fn new(
        instrumentation: &'a Instrumentation,
        retained_window: Option<chrono::Duration>,
    ) -> Self {
        Self {
            instrumentation,
            retained_window,
        }
    }

    /// Records successful outcomes after the caller commits the outer transaction.
    pub(super) fn record_committed(
        &self,
        outcomes: impl IntoIterator<Item = PreallocationSuccess>,
    ) {
        for outcome in outcomes {
            self.instrumentation
                .emit(StaticAddressPreallocationCompleted::from(outcome));
        }
    }

    /// Applies fixed-address declarations for an expected-machine update.
    ///
    /// The lower-level helpers create a missing reservation or fill an addressless
    /// row before that family's first stateful allocation, but leave existing
    /// addresses alone. An associated row may receive its configured fixed address,
    /// but its role-derived type and primary setting remain managed state.
    /// Operators still use the machine-interface address APIs to replace a live
    /// address. The effective Host BMC is applied separately so legacy-only rows
    /// use the same path without storing a nested declaration.
    pub(super) async fn update_machine_interfaces(
        &self,
        txn: &mut sqlx::PgConnection,
        machine: &ExpectedMachine,
    ) -> Result<Vec<PreallocationSuccess>, CarbideError> {
        let mut preallocations = Vec::new();
        let host_bmc = machine.effective_host_bmc();
        if host_bmc.fixed_ip.is_some() {
            preallocations.push(self.update_expected_interface(&mut *txn, &host_bmc).await?);
        }

        for interface in machine.data.interfaces.iter().filter(|interface| {
            !interface.role.is_dpu_loopback()
                && interface
                    .mac_address
                    .is_some_and(|mac| mac != machine.bmc_mac_address)
        }) {
            if interface.fixed_ip.is_some() {
                preallocations.push(self.update_expected_interface(&mut *txn, interface).await?);
            }
        }

        Ok(preallocations)
    }

    /// Update or create a machine_interface with a static address.
    ///
    /// If no interface exists for this MAC, creates a new one. If an
    /// interface exists but has no addresses, assigns the static IP.
    /// If an interface exists and already has addresses, we leave it
    /// alone -- this is not an error, because expected device updates
    /// are decoupled from managed device state. The expected data is
    /// updated in the database by the caller; we only touch the
    /// machine_interface if it's safe to do so (no existing addresses).
    /// To change the IP on a live interface, operators should use
    /// 'machine-interfaces assign-address' or 'remove-address'.
    pub(super) async fn update_interface(
        &self,
        txn: &mut sqlx::PgConnection,
        bmc_mac_address: MacAddress,
        bmc_ip: std::net::IpAddr,
    ) -> Result<PreallocationSuccess, CarbideError> {
        self.record_error(
            self.update_with_settings(txn, bmc_mac_address, bmc_ip, None)
                .await,
        )
    }

    /// Reconciles a fixed expected interface within a savepoint.
    ///
    /// An addressed interface remains unchanged. A missing row is created with
    /// the declared interface settings. An addressless row receives the fixed IP
    /// only before that family's first stateful allocation, and only an
    /// unassociated row also receives the role-derived settings.
    async fn update_expected_interface(
        &self,
        txn: &mut sqlx::PgConnection,
        expected_interface: &ExpectedInterface,
    ) -> Result<PreallocationSuccess, CarbideError> {
        self.record_error(
            self.update_expected_interface_inner(txn, expected_interface)
                .await,
        )
    }

    async fn update_expected_interface_inner(
        &self,
        txn: &mut sqlx::PgConnection,
        expected_interface: &ExpectedInterface,
    ) -> Result<PreallocationSuccess, CarbideError> {
        let mac_address = expected_interface
            .require_mac_address()
            .map_err(|message| CarbideError::InvalidArgument(message.to_string()))?;
        let fixed_ip = expected_interface
            .fixed_reservation_ip()
            .map_err(|message| {
                CarbideError::InvalidArgument(format!(
                    "expected interface {}: {message}",
                    expected_interface.identity(),
                ))
            })?;

        // Updates process declarations in request order. Release locks from a
        // skipped declaration before the next one can lock a different interface.
        let mut savepoint = db::Transaction::begin_inner(txn).await?;
        let result = self
            .update_with_settings(
                savepoint.as_pgconn(),
                mac_address,
                fixed_ip,
                Some(ExpectedInterfaceSettings {
                    interface_type: expected_interface.role.interface_type(),
                    primary_interface: expected_interface.role.primary_interface_override(),
                    segment_type_guard: expected_interface.segment_type_guard(),
                    require_managed_prefix: !expected_interface
                        .allows_static_assignments_fallback(),
                }),
            )
            .await?;
        if result == PreallocationSuccess::Skipped {
            savepoint.rollback().await?;
        } else {
            savepoint.commit().await?;
        }
        Ok(result)
    }

    /// Create or safely update a fixed-address reservation.
    ///
    /// Existing addressed rows remain unchanged. Addressless rows receive the
    /// fixed address, but ExpectedInterface reservations also require that family
    /// to have no prior stateful allocation. Their settings apply only while the
    /// row is unassociated. Passing no settings preserves generic preallocation.
    async fn update_with_settings(
        &self,
        txn: &mut sqlx::PgConnection,
        mac_address: MacAddress,
        ip_address: std::net::IpAddr,
        settings: Option<ExpectedInterfaceSettings>,
    ) -> Result<PreallocationSuccess, CarbideError> {
        let segment_type_guard = settings.and_then(|settings| settings.segment_type_guard);
        // Generalized ExpectedInterface declarations require a managed prefix
        // even when an addressed interface will otherwise remain unchanged.
        // Legacy callers resolve lazily to preserve that addressed-row no-op.
        let resolved_segment = if settings.is_some_and(|settings| settings.require_managed_prefix) {
            Some(
                db::network_segment::for_managed_static_address(
                    txn,
                    ip_address,
                    segment_type_guard,
                )
                .await?,
            )
        } else {
            None
        };
        let existing = db::machine_interface::find_by_mac_address(&mut *txn, mac_address).await?;

        if let Some(mut iface) = existing.into_iter().next() {
            // Skip the lock when the initial read already has an address.
            // Expected-device callers do not all acquire interface and inventory
            // locks in the same order.
            if iface.addresses.is_empty() {
                let family = ip_address.address_family();
                // Do not refill a family whose stateful address was removed.
                if settings.is_some()
                    && !db::machine_interface::can_apply_expected_allocation(txn, iface.id, family)
                        .await?
                {
                    return Ok(PreallocationSuccess::Skipped);
                }
                // Fixed assignment does not need a segment advisory lock. Keeping
                // one across a batch update would reverse DHCP's lock order:
                // ExpectedMachine first, then its allocation segment.
                db::machine_interface::lock_for_address_assignment(txn, iface.id).await?;
                iface = db::machine_interface::find_one(&mut *txn, iface.id).await?;
                if settings.is_some()
                    && (!iface.addresses.is_empty()
                        || !db::machine_interface::can_apply_expected_allocation(
                            txn, iface.id, family,
                        )
                        .await?)
                {
                    return Ok(PreallocationSuccess::Skipped);
                }
            }
            if iface.addresses.is_empty() {
                // No addresses -- safe to assign the static IP.
                db::machine_interface_address::assign_static(txn, iface.id, ip_address).await?;

                let segment = match resolved_segment {
                    Some(segment) => segment,
                    None => db::network_segment::for_static_address(txn, ip_address).await?,
                };
                if iface.segment_id != segment.id {
                    db::machine_interface::update_segment_id(
                        txn,
                        iface.id,
                        segment.id,
                        segment.config.subdomain_id,
                    )
                    .await?;
                }
                // Address reconciliation is independent from these settings. A
                // configured fixed address is still assigned above when an
                // associated row has no addresses; ownership only prevents role
                // and primary-interface changes.
                if let Some(settings) = settings {
                    db::machine_interface::update_unassociated_expected_interface_settings(
                        txn,
                        iface.id,
                        Some(settings.interface_type),
                        settings.primary_interface,
                    )
                    .await?;
                }
                db::machine_interface::sync_hostname_after_address_assignment(
                    txn,
                    iface.id,
                    segment.config.subdomain_id,
                )
                .await?;

                tracing::info!(
                    %mac_address,
                    ip_address = %ip_address,
                    machine_interface_id = %iface.id,
                    "Assigned static address to existing interface without addresses"
                );

                Ok(PreallocationSuccess::Assigned)
            } else {
                // Interface already has address(es). We don't touch it --
                // expected data updates are decoupled from managed state.
                // The caller updates the expected data table; we just log.
                tracing::info!(
                    %mac_address,
                    ip_address = %ip_address,
                    existing_addresses = ?iface.addresses,
                    "Interface already has addresses, updated expected data only"
                );

                Ok(PreallocationSuccess::Skipped)
            }
        } else {
            // No interface yet -- create a new one.
            let segment = match resolved_segment {
                Some(segment) => segment,
                None => db::network_segment::for_static_address(txn, ip_address).await?,
            };
            let interface_type = settings
                .map(|settings| settings.interface_type)
                .unwrap_or(InterfaceType::Data);
            let primary_interface = settings
                .and_then(|settings| settings.primary_interface)
                .unwrap_or(true);
            db::machine_interface::create_with_type(
                txn,
                std::slice::from_ref(&segment),
                &mac_address,
                primary_interface,
                AddressSelectionStrategy::StaticAddress(ip_address),
                interface_type,
                self.retained_window,
            )
            .await?;

            tracing::info!(
                %mac_address,
                ip_address = %ip_address,
                network_segment_id = %segment.id,
                "Pre-allocated static machine interface"
            );

            Ok(PreallocationSuccess::Created)
        }
    }

    fn record_error(
        &self,
        result: Result<PreallocationSuccess, CarbideError>,
    ) -> Result<PreallocationSuccess, CarbideError> {
        if let Err(error) = &result {
            self.instrumentation
                .emit(StaticAddressPreallocationCompleted::Error {
                    error: error.to_string(),
                });
        }

        result
    }
}

/// ExpectedInterface settings that may be applied with a fixed-address
/// reservation.
///
/// These settings configure a new row, but only update an existing row while
/// it is still unassociated. ExpectedMachine is an ingestion template and must
/// not reclassify an interface after another resource owns it.
#[derive(Clone, Copy)]
struct ExpectedInterfaceSettings {
    /// Database interface type represented by the configured role.
    interface_type: InterfaceType,
    /// Role-specific primary-interface value, when the role declares one.
    primary_interface: Option<bool>,
    /// Segment type that the fixed address must resolve to.
    segment_type_guard: Option<NetworkSegmentType>,
    /// Require a managed prefix to contain the fixed address instead of
    /// falling back to `static-assignments`.
    require_managed_prefix: bool,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_utils::test_support::test_meter::TestMeter;
    use carbide_uuid::machine::MachineInterfaceId;
    use carbide_uuid::network::NetworkSegmentId;
    use model::allocation_type::AllocationType;
    use opentelemetry::metrics::noop::NoopMeterProvider;

    use super::*;

    #[crate::sqlx_test]
    async fn expected_preallocation_rechecks_after_assignment_and_releases_skipped_locks(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let instrumentation = Instrumentation::from_meter_provider(&NoopMeterProvider::new());
        let preallocator = InterfacePreallocator::new(&instrumentation, None);
        let env = crate::tests::create_test_env(pool).await;
        let pool = &env.api.database_connection;
        let mac_address = "02:00:00:00:41:88".parse()?;
        let mut txn = pool.begin().await?;
        let interface = db::machine_interface::find_or_create_observed_machine_interface(
            &mut txn,
            None,
            mac_address,
            &["192.0.2.1".parse()?],
            None,
            None,
            None,
        )
        .await?;
        txn.commit().await?;

        let expected_interface = ExpectedInterface {
            mac_address: Some(mac_address),
            ip_allocation: Some(model::expected_machine::ExpectedInterfaceIpAllocation::Fixed),
            fixed_ip: Some("192.0.2.240".parse()?),
            ..Default::default()
        };

        // A successful assignment must not hold the segment lock while the
        // enclosing ExpectedMachine update processes other declarations.
        let mut allocating = pool.begin().await?;
        let result = preallocator
            .update_expected_interface(&mut allocating, &expected_interface)
            .await?;
        assert_eq!(result, PreallocationSuccess::Assigned);
        let addresses =
            db::machine_interface_address::find_for_interface(&mut allocating, interface.id)
                .await?;
        assert_eq!(addresses.len(), 1);
        assert_eq!(Some(addresses[0].address), expected_interface.fixed_ip);
        assert_eq!(addresses[0].allocation_type, AllocationType::Static);
        let mut probe = pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout = '1s'")
            .execute(&mut *probe)
            .await?;
        db::machine_interface::lock_network_segments_exclusive(
            &mut probe,
            std::slice::from_ref(&interface.segment_id),
        )
        .await?;
        probe.rollback().await?;
        allocating.rollback().await?;

        let mut assigning = pool.begin().await?;
        let assigning_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *assigning)
            .await?;
        db::machine_interface::lock_for_address_assignment(&mut assigning, interface.id).await?;
        let mut outer_txn = pool.begin().await?;
        let applying_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *outer_txn)
            .await?;
        let applying_interface = expected_interface.clone();
        let mut applying = tokio::task::JoinSet::new();
        let applying_instrumentation = instrumentation.clone();
        applying.spawn(async move {
            let preallocator = InterfacePreallocator::new(&applying_instrumentation, None);
            let result = preallocator
                .update_expected_interface(&mut outer_txn, &applying_interface)
                .await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((outer_txn, result))
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                    .bind(assigning_pid)
                    .bind(applying_pid)
                    .fetch_one(pool)
                    .await?;
                if waiting {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await??;

        // The first eligibility read saw an empty family. Commit an operator
        // assignment while preallocation waits for the interface lock.
        let assigned_ip = "192.0.2.241".parse()?;
        db::machine_interface_address::assign_static(&mut assigning, interface.id, assigned_ip)
            .await?;
        assigning.commit().await?;
        let (outer_txn, result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), applying.join_next())
                .await?
                .expect("the preallocation task should finish")??;
        assert_eq!(result, PreallocationSuccess::Skipped);

        // A skipped declaration must release its locks even while the
        // enclosing ExpectedMachine update still has more declarations.
        let mut probe = pool.begin().await?;
        sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(interface.id)
            .fetch_one(&mut *probe)
            .await?;
        let addresses =
            db::machine_interface_address::find_for_interface(&mut probe, interface.id).await?;
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].address, assigned_ip);
        assert_eq!(addresses[0].allocation_type, AllocationType::Static);
        probe.rollback().await?;
        outer_txn.rollback().await?;

        // Removing the allocation does not make this family eligible again.
        let mut txn = pool.begin().await?;
        assert!(
            db::machine_interface_address::delete_by_interface_and_address(
                &mut txn,
                interface.id,
                assigned_ip,
                AllocationType::Static,
            )
            .await?
        );
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        let result = preallocator
            .update_expected_interface(&mut txn, &expected_interface)
            .await?;
        assert_eq!(result, PreallocationSuccess::Skipped);
        let addresses =
            db::machine_interface_address::find_for_interface(&mut txn, interface.id).await?;
        assert!(addresses.is_empty());
        txn.rollback().await?;
        Ok(())
    }

    #[test]
    fn preallocation_error_is_emitted_once_to_its_registry() {
        let meter = TestMeter::default();
        let unrelated_meter = TestMeter::default();
        let metrics = MetricsCapture::for_registry(meter.registry());
        let unrelated_metrics = MetricsCapture::for_registry(unrelated_meter.registry());
        let instrumentation = Instrumentation::new(meter.meter());
        let preallocator = InterfacePreallocator::new(&instrumentation, None);
        let logs = capture_logs(|| {
            let result = preallocator.record_error(Err(CarbideError::InvalidArgument(
                "operation failed".to_string(),
            )));
            assert!(matches!(result, Err(CarbideError::InvalidArgument(_))));
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::WARN);
        let metric = "carbide_static_address_preallocations_total";
        let labels = &[("outcome", "error")];
        assert_eq!(metrics.counter_delta(metric, labels), 1.0);
        assert_eq!(unrelated_metrics.counter_delta(metric, labels), 0.0);
    }

    #[crate::sqlx_test]
    async fn expected_update_skips_addressed_interfaces_without_locking(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let instrumentation = Instrumentation::from_meter_provider(&NoopMeterProvider::new());
        let preallocator = InterfacePreallocator::new(&instrumentation, None);
        let mut setup = pool.begin().await?;
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('expected-update-skip', 'V1-T0') RETURNING id",
        )
        .fetch_one(&mut *setup)
        .await?;
        let mac_address: MacAddress = "02:00:00:00:53:99".parse()?;
        let interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces (segment_id, mac_address, hostname, primary_interface)
             VALUES ($1, $2, 'expected-update-skip', true) RETURNING id",
        )
        .bind(segment_id)
        .bind(mac_address)
        .fetch_one(&mut *setup)
        .await?;
        let address = "2001:db8::5398".parse()?;
        db::machine_interface_address::insert(
            &mut setup,
            interface_id,
            address,
            AllocationType::Static,
        )
        .await?;
        setup.commit().await?;

        let mut update_txn = pool.begin().await?;
        let result = preallocator
            .update_interface(&mut update_txn, mac_address, "2001:db8::5399".parse()?)
            .await?;
        assert_eq!(result, PreallocationSuccess::Skipped);

        // Keep the expected update open. A primary-interface or inventory
        // writer must still be able to lock the skipped interface.
        let mut competing_txn = pool.begin().await?;
        sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(interface_id)
            .execute(&mut *competing_txn)
            .await?;
        let addresses =
            db::machine_interface_address::find_for_interface(&mut competing_txn, interface_id)
                .await?;
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].address, address);
        assert_eq!(addresses[0].allocation_type, AllocationType::Static);
        competing_txn.commit().await?;
        update_txn.commit().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn expected_update_preserves_an_address_committed_while_waiting(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let instrumentation = Instrumentation::from_meter_provider(&NoopMeterProvider::new());
        let preallocator = InterfacePreallocator::new(&instrumentation, None);
        let mut setup = pool.begin().await?;
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('expected-update-race', 'V1-T0') RETURNING id",
        )
        .fetch_one(&mut *setup)
        .await?;
        let mac_address: MacAddress = "02:00:00:00:53:98".parse()?;
        let interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces (segment_id, mac_address, hostname, primary_interface)
             VALUES ($1, $2, 'expected-update-race', true) RETURNING id",
        )
        .bind(segment_id)
        .bind(mac_address)
        .fetch_one(&mut *setup)
        .await?;
        setup.commit().await?;

        let inferred_address = "2001:db8::5398".parse()?;
        let mut holder = pool.begin().await?;
        sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
            .bind(interface_id)
            .execute(&mut *holder)
            .await?;
        db::machine_interface_address::insert(
            &mut holder,
            interface_id,
            inferred_address,
            AllocationType::Slaac,
        )
        .await?;
        let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *holder)
            .await?;
        let mut waiter = pool.begin().await?;
        let waiter_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *waiter)
            .await?;

        // The expected-data update starts with an addressless snapshot. Its
        // skip decision must use the SLAAC row committed while it waits.
        let update = async {
            let result = preallocator
                .update_interface(&mut waiter, mac_address, "2001:db8::5399".parse()?)
                .await?;
            waiter.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(result)
        };
        let release = async {
            loop {
                let blocked: bool = sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                    .bind(holder_pid)
                    .bind(waiter_pid)
                    .fetch_one(&pool)
                    .await?;
                if blocked {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            holder.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (updated, released) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(update, release)
        })
        .await?;
        released?;
        assert_eq!(updated?, PreallocationSuccess::Skipped);

        let mut connection = pool.acquire().await?;
        let addresses =
            db::machine_interface_address::find_for_interface(&mut connection, interface_id)
                .await?;
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].address, inferred_address);
        assert_eq!(addresses[0].allocation_type, AllocationType::Slaac);
        Ok(())
    }
}
