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

use carbide_instrument::emit;
use carbide_network::ip::IdentifyAddressFamily;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::allocation_type::AllocationType;
use model::expected_machine::ExpectedInterface;
use model::machine_interface::InterfaceType;
use model::network_segment::NetworkSegmentType;
use rpc::forge as rpc;
use tonic::{Request, Response, Status};

use crate::api::Api;
use crate::errors::CarbideError;
use crate::handlers::static_address_metrics::{
    PreallocationSuccess, StaticAddressAssignmentCompleted, StaticAddressPreallocationCompleted,
    StaticAddressRemovalCompleted,
};

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
pub(super) async fn update_preallocated_machine_interface(
    txn: &mut sqlx::PgConnection,
    bmc_mac_address: MacAddress,
    bmc_ip: std::net::IpAddr,
    retained_window: Option<chrono::Duration>,
) -> Result<PreallocationSuccess, CarbideError> {
    emit_preallocation_error(
        update_preallocated_machine_interface_with_settings(
            txn,
            bmc_mac_address,
            bmc_ip,
            None,
            retained_window,
        )
        .await,
    )
}

/// Apply the existing safe update behavior to a fixed expected interface.
///
/// An addressed interface remains unchanged. A missing row is created with
/// the declared interface settings. An addressless row receives the fixed IP
/// only before that family's first stateful allocation, and only an
/// unassociated row also receives the role-derived settings.
pub(super) async fn update_preallocated_expected_machine_interface(
    txn: &mut sqlx::PgConnection,
    expected_interface: &ExpectedInterface,
    retained_window: Option<chrono::Duration>,
) -> Result<PreallocationSuccess, CarbideError> {
    emit_preallocation_error(
        update_preallocated_expected_machine_interface_inner(
            txn,
            expected_interface,
            retained_window,
        )
        .await,
    )
}

async fn update_preallocated_expected_machine_interface_inner(
    txn: &mut sqlx::PgConnection,
    expected_interface: &ExpectedInterface,
    retained_window: Option<chrono::Duration>,
) -> Result<PreallocationSuccess, CarbideError> {
    let fixed_ip = expected_interface
        .fixed_reservation_ip()
        .map_err(|message| {
            CarbideError::InvalidArgument(format!(
                "expected interface {}: {message}",
                expected_interface.mac_address,
            ))
        })?;

    // Updates process declarations in request order. Release locks from a
    // skipped declaration before the next one can lock a different interface.
    let mut savepoint = db::Transaction::begin_inner(txn).await?;
    let result = update_preallocated_machine_interface_with_settings(
        savepoint.as_pgconn(),
        expected_interface.mac_address,
        fixed_ip,
        Some(ExpectedInterfaceSettings {
            interface_type: expected_interface.role.interface_type(),
            primary_interface: expected_interface.role.primary_interface_override(),
            segment_type_guard: expected_interface.segment_type_guard(),
            require_managed_prefix: !expected_interface.allows_static_assignments_fallback(),
        }),
        retained_window,
    )
    .await?;
    if result == PreallocationSuccess::Skipped {
        savepoint.rollback().await?;
    } else {
        savepoint.commit().await?;
    }
    Ok(result)
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

/// Create or safely update a fixed-address reservation.
///
/// Existing addressed rows remain unchanged. Addressless rows receive the
/// fixed address, but ExpectedInterface reservations also require that family
/// to have no prior stateful allocation. Their settings apply only while the
/// row is unassociated. Passing no settings preserves generic preallocation.
async fn update_preallocated_machine_interface_with_settings(
    txn: &mut sqlx::PgConnection,
    mac_address: MacAddress,
    ip_address: std::net::IpAddr,
    settings: Option<ExpectedInterfaceSettings>,
    retained_window: Option<chrono::Duration>,
) -> Result<PreallocationSuccess, CarbideError> {
    let segment_type_guard = settings.and_then(|settings| settings.segment_type_guard);
    // Generalized ExpectedInterface declarations require a managed prefix
    // even when an addressed interface will otherwise remain unchanged.
    // Legacy callers resolve lazily to preserve that addressed-row no-op.
    let resolved_segment = if settings.is_some_and(|settings| settings.require_managed_prefix) {
        Some(
            db::network_segment::for_managed_static_address(txn, ip_address, segment_type_guard)
                .await?,
        )
    } else {
        None
    };
    let existing = db::machine_interface::find_by_mac_address(&mut *txn, mac_address).await?;

    if let Some(mut iface) = existing.into_iter().next() {
        if settings.is_some() && iface.addresses.is_empty() {
            let family = ip_address.address_family();
            // Do not refill a family whose stateful address was removed.
            if !db::machine_interface::can_apply_expected_allocation(txn, iface.id, family).await? {
                return Ok(PreallocationSuccess::Skipped);
            }
            // Fixed assignment does not need a segment advisory lock. Keeping
            // one across a batch update would reverse DHCP's lock order:
            // ExpectedMachine first, then its allocation segment.
            db::machine_interface::lock_for_allocation(txn, iface.id).await?;
            iface = db::machine_interface::find_one(&mut *txn, iface.id).await?;
            if !iface.addresses.is_empty()
                || !db::machine_interface::can_apply_expected_allocation(txn, iface.id, family)
                    .await?
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
            retained_window,
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

fn emit_preallocation_error(
    result: Result<PreallocationSuccess, CarbideError>,
) -> Result<PreallocationSuccess, CarbideError> {
    if let Err(error) = &result {
        emit(StaticAddressPreallocationCompleted::Error {
            error: error.to_string(),
        });
    }

    result
}

pub(crate) async fn assign_static_address(
    api: &Api,
    request: Request<rpc::AssignStaticAddressRequest>,
) -> Result<Response<rpc::AssignStaticAddressResponse>, CarbideError> {
    let result = assign_static_address_inner(api, request).await;

    let event = match &result {
        Ok(response) => match rpc::AssignStaticAddressStatus::try_from(response.get_ref().status) {
            Ok(rpc::AssignStaticAddressStatus::Assigned) => {
                StaticAddressAssignmentCompleted::Assigned {}
            }
            Ok(rpc::AssignStaticAddressStatus::ReplacedStatic) => {
                StaticAddressAssignmentCompleted::ReplacedStatic {}
            }
            Ok(rpc::AssignStaticAddressStatus::ReplacedDhcp) => {
                StaticAddressAssignmentCompleted::ReplacedDhcp {}
            }
            Err(error) => StaticAddressAssignmentCompleted::Error {
                error: error.to_string(),
            },
        },
        Err(error) => StaticAddressAssignmentCompleted::Error {
            error: error.to_string(),
        },
    };
    emit(event);

    result
}

async fn assign_static_address_inner(
    api: &Api,
    request: Request<rpc::AssignStaticAddressRequest>,
) -> Result<Response<rpc::AssignStaticAddressResponse>, CarbideError> {
    let req = request.into_inner();
    let interface_id = req.interface_id.ok_or(CarbideError::InvalidArgument(
        "interface_id is required".into(),
    ))?;
    let ip_address: std::net::IpAddr = req.ip_address.parse()?;

    let mut txn = api.txn_begin().await?;
    let result =
        db::machine_interface_address::assign_static(&mut txn, interface_id, ip_address).await?;

    // Resolve the correct segment for this IP and update the interface
    // if needed. IPs within a managed prefix go on that prefix's segment.
    // External IPs go on the static-assignments anchor segment.
    let target_segment =
        db::network_segment::for_static_address(txn.as_pgconn(), ip_address).await?;

    let current_iface = db::machine_interface::find_one(txn.as_pgconn(), interface_id).await?;
    if current_iface.segment_id != target_segment.id {
        db::machine_interface::update_segment_id(
            &mut txn,
            interface_id,
            target_segment.id,
            target_segment.config.subdomain_id,
        )
        .await?;
        tracing::info!(
            machine_interface_id = %interface_id,
            %ip_address,
            previous_network_segment_id = %current_iface.segment_id,
            next_network_segment_id = %target_segment.id,
            "Moved interface to correct segment for static address"
        );
    }

    // Keep the interface's IP-derived hostname/domain consistent with the new
    // address, matching every other assignment path. Skipping this leaves a
    // stale hostname that encodes a now-freed IP and collides with the
    // fqdn_must_be_unique constraint when the allocator reuses that IP.
    db::machine_interface::sync_hostname_after_address_assignment(
        &mut txn,
        interface_id,
        target_segment.config.subdomain_id,
    )
    .await?;

    txn.commit().await?;

    let status: rpc::AssignStaticAddressStatus = result.into();
    tracing::info!(machine_interface_id = %interface_id, %ip_address, assignment_status = ?status, "Static address assignment");

    Ok(Response::new(rpc::AssignStaticAddressResponse {
        interface_id: Some(interface_id),
        ip_address: ip_address.to_string(),
        status: status.into(),
    }))
}

pub(crate) async fn remove_static_address(
    api: &Api,
    request: Request<rpc::RemoveStaticAddressRequest>,
) -> Result<Response<rpc::RemoveStaticAddressResponse>, CarbideError> {
    let result = remove_static_address_inner(api, request).await;

    let event = match &result {
        Ok(response) => match rpc::RemoveStaticAddressStatus::try_from(response.get_ref().status) {
            Ok(rpc::RemoveStaticAddressStatus::Removed) => {
                StaticAddressRemovalCompleted::Removed {}
            }
            Ok(rpc::RemoveStaticAddressStatus::NotFound) => {
                StaticAddressRemovalCompleted::NotFound {}
            }
            Err(error) => StaticAddressRemovalCompleted::Error {
                error: error.to_string(),
            },
        },
        Err(error) => StaticAddressRemovalCompleted::Error {
            error: error.to_string(),
        },
    };
    emit(event);

    result
}

async fn remove_static_address_inner(
    api: &Api,
    request: Request<rpc::RemoveStaticAddressRequest>,
) -> Result<Response<rpc::RemoveStaticAddressResponse>, CarbideError> {
    let req = request.into_inner();
    let interface_id = req.interface_id.ok_or(CarbideError::InvalidArgument(
        "interface_id is required".into(),
    ))?;
    let ip_address: std::net::IpAddr = req.ip_address.parse()?;

    let mut txn = api.txn_begin().await?;
    // Scope the delete to the caller's interface so remove-address only ever
    // removes that interface's own address, matching the command's contract
    // ("remove the address from a machine interface"). A mismatched interface_id
    // deletes nothing and returns NotFound rather than removing another
    // interface's row that happens to hold the same IP.
    let deleted = db::machine_interface_address::delete_by_interface_and_address(
        &mut txn,
        interface_id,
        ip_address,
        AllocationType::Static,
    )
    .await?;

    // Re-derive the interface's hostname/domain now that its address is gone,
    // matching the DHCP lease-expiry path. Without this the interface keeps a
    // hostname pinned to the removed IP.
    if deleted {
        db::machine_interface::sync_hostname_after_address_change(&mut txn, interface_id).await?;
    }

    txn.commit().await?;

    let status = if deleted {
        tracing::info!(machine_interface_id = %interface_id, %ip_address, "Removed static address");
        rpc::RemoveStaticAddressStatus::Removed
    } else {
        tracing::info!(machine_interface_id = %interface_id, %ip_address, "Static address not found");
        rpc::RemoveStaticAddressStatus::NotFound
    };

    Ok(Response::new(rpc::RemoveStaticAddressResponse {
        interface_id: Some(interface_id),
        ip_address: ip_address.to_string(),
        status: status.into(),
    }))
}

pub(crate) async fn find_interface_addresses(
    api: &Api,
    request: Request<rpc::FindInterfaceAddressesRequest>,
) -> Result<Response<rpc::FindInterfaceAddressesResponse>, Status> {
    let req = request.into_inner();
    let interface_id = req.interface_id.ok_or(CarbideError::InvalidArgument(
        "interface_id is required".into(),
    ))?;

    let mut txn = api.txn_begin().await?;
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    txn.commit().await?;

    let proto_addresses = addresses
        .into_iter()
        .map(|a| rpc::InterfaceAddress {
            address: a.address.to_string(),
            allocation_type: match a.allocation_type {
                AllocationType::Dhcp => "dhcp".to_string(),
                AllocationType::Static => "static".to_string(),
                AllocationType::Slaac => "slaac".to_string(),
            },
        })
        .collect();

    Ok(Response::new(rpc::FindInterfaceAddressesResponse {
        interface_id: Some(interface_id),
        addresses: proto_addresses,
    }))
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::capture_logs;

    use super::*;

    #[crate::sqlx_test]
    async fn expected_preallocation_rechecks_after_assignment_and_releases_skipped_locks(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
            mac_address,
            ip_allocation: Some(model::expected_machine::ExpectedInterfaceIpAllocation::Fixed),
            fixed_ip: Some("192.0.2.240".parse()?),
            ..Default::default()
        };

        // A successful assignment must not hold the segment lock while the
        // enclosing ExpectedMachine update processes other declarations.
        let mut allocating = pool.begin().await?;
        let result = update_preallocated_expected_machine_interface_inner(
            &mut allocating,
            &expected_interface,
            None,
        )
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
        db::machine_interface::lock_for_allocation(&mut assigning, interface.id).await?;
        let mut outer_txn = pool.begin().await?;
        let applying_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *outer_txn)
            .await?;
        let applying_interface = expected_interface.clone();
        let mut applying = tokio::task::JoinSet::new();
        applying.spawn(async move {
            let result = update_preallocated_expected_machine_interface_inner(
                &mut outer_txn,
                &applying_interface,
                None,
            )
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
        let result = update_preallocated_expected_machine_interface_inner(
            &mut txn,
            &expected_interface,
            None,
        )
        .await?;
        assert_eq!(result, PreallocationSuccess::Skipped);
        let addresses =
            db::machine_interface_address::find_for_interface(&mut txn, interface.id).await?;
        assert!(addresses.is_empty());
        txn.rollback().await?;
        Ok(())
    }

    #[test]
    fn preallocation_error_is_emitted_once() {
        let logs = capture_logs(|| {
            let result = emit_preallocation_error(Err(CarbideError::InvalidArgument(
                "operation failed".to_string(),
            )));
            assert!(matches!(result, Err(CarbideError::InvalidArgument(_))));
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::WARN);
    }
}
