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

use std::net::IpAddr;

use carbide_network::ip::IdentifyAddressFamily;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use rpc::forge as rpc;
use tonic::{Request, Response, Status};

use crate::api::{Api, log_request_data};
use crate::errors::CarbideError;
use crate::handlers::static_address_metrics::{
    StaticAddressAssignmentCompleted, StaticAddressRemovalCompleted,
};

/// Assign a static IP address to a machine interface.
///
/// Changing IPv4 keeps existing IPv6 addresses, and vice versa. If the new
/// address moves the interface to another network segment, those retained
/// addresses must also belong to that segment. Otherwise, reject the request
/// without changing any addresses or interface settings.
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
    api.instrumentation.emit(event);

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
    db::machine_interface::lock_for_address_assignment(txn.as_pgconn(), interface_id).await?;

    // Resolve the correct segment for this IP and update the interface
    // if needed. IPs within a managed prefix go on that prefix's segment.
    // External IPs go on the static-assignments anchor segment.
    let target_segment =
        db::network_segment::for_static_address(txn.as_pgconn(), ip_address).await?;

    let current_iface = db::machine_interface::find_one(txn.as_pgconn(), interface_id).await?;
    if current_iface.segment_id != target_segment.id
        && let Some(sibling) = current_iface.addresses.iter().find(|address| {
            address.address_family() != ip_address.address_family()
                && !target_segment
                    .prefixes
                    .iter()
                    .any(|prefix| prefix.prefix.contains(**address))
        })
    {
        return Err(CarbideError::FailedPrecondition(format!(
            "cannot assign {ip_address} to interface {interface_id}: retained address {sibling} \
             is outside target network segment {} ({}); replace or remove {sibling} first",
            target_segment.config.name, target_segment.id,
        )));
    }

    let result =
        db::machine_interface_address::assign_static(&mut txn, interface_id, ip_address).await?;
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
    api.instrumentation.emit(event);

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

/// Parse an optional MAC-address filter from an operator request.
fn parse_reserved_mac_filter(mac: Option<String>) -> Result<Option<MacAddress>, CarbideError> {
    mac.map(|mac| {
        mac.parse::<MacAddress>()
            .map_err(|e| CarbideError::InvalidArgument(format!("invalid MAC address {mac}: {e}")))
    })
    .transpose()
}

/// Parse an optional address filter from an operator request.
fn parse_reserved_address_filter(address: Option<String>) -> Result<Option<IpAddr>, CarbideError> {
    address
        .map(|address| {
            address.parse::<IpAddr>().map_err(|e| {
                CarbideError::InvalidArgument(format!("invalid IP address {address}: {e}"))
            })
        })
        .transpose()
}

pub(crate) async fn admin_find_reserved_address_ids(
    api: &Api,
    request: Request<rpc::AdminFindReservedAddressesRequest>,
) -> Result<Response<rpc::AdminReservedAddressIdList>, Status> {
    log_request_data(&request);
    let rpc::AdminFindReservedAddressesRequest {
        reserved_by_mac,
        ip_address,
    } = request.into_inner();
    let mac_filter = parse_reserved_mac_filter(reserved_by_mac)?;
    let address_filter = parse_reserved_address_filter(ip_address)?;

    let mut txn = api.txn_begin().await?;
    let ids = db::machine_interface_address::find_reserved_ids(
        txn.as_pgconn(),
        mac_filter,
        address_filter,
    )
    .await?;
    txn.commit().await?;

    Ok(Response::new(rpc::AdminReservedAddressIdList {
        ip_addresses: ids.into_iter().map(|a| a.to_string()).collect(),
    }))
}

pub(crate) async fn admin_find_reserved_addresses_by_ids(
    api: &Api,
    request: Request<rpc::AdminReservedAddressesByIdsRequest>,
) -> Result<Response<rpc::AdminFindReservedAddressesResponse>, Status> {
    log_request_data(&request);
    let addresses: Vec<IpAddr> = request
        .into_inner()
        .ip_addresses
        .iter()
        .map(|address| {
            address.parse::<IpAddr>().map_err(|e| {
                CarbideError::InvalidArgument(format!("invalid IP address {address}: {e}"))
            })
        })
        .collect::<Result<_, _>>()?;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if addresses.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if addresses.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;
    let reserved =
        db::machine_interface_address::find_reserved_by_ids(txn.as_pgconn(), &addresses).await?;
    txn.commit().await?;

    let reserved_addresses = reserved
        .into_iter()
        .map(|r| rpc::ReservedAddress {
            ip_address: r.address.to_string(),
            reserved_by_mac: r.reserved_by_mac.to_string(),
            allocation_type: allocation_type_label(r.allocation_type),
        })
        .collect();

    Ok(Response::new(rpc::AdminFindReservedAddressesResponse {
        reserved_addresses,
    }))
}

pub(crate) async fn admin_release_reserved_addresses(
    api: &Api,
    request: Request<rpc::AdminReleaseReservedAddressesRequest>,
) -> Result<Response<rpc::AdminReleaseReservedAddressesResponse>, Status> {
    log_request_data(&request);
    let rpc::AdminReleaseReservedAddressesRequest {
        reserved_by_mac,
        ip_address,
    } = request.into_inner();
    let mac_filter = parse_reserved_mac_filter(reserved_by_mac)?;
    let address_filter = parse_reserved_address_filter(ip_address)?;

    // Require a scope so an operator cannot release every reservation at once.
    if mac_filter.is_none() && address_filter.is_none() {
        return Err(CarbideError::InvalidArgument(
            "a MAC address or an address is required to release a reservation".into(),
        )
        .into());
    }

    let mut txn = api.txn_begin().await?;
    let released = db::machine_interface_address::release_reserved(
        txn.as_pgconn(),
        mac_filter,
        address_filter,
    )
    .await?;
    txn.commit().await?;

    Ok(Response::new(rpc::AdminReleaseReservedAddressesResponse {
        released_ip_addresses: released.into_iter().map(|a| a.to_string()).collect(),
    }))
}

/// The wire label for an [`AllocationType`], matching the interface-address API.
fn allocation_type_label(allocation_type: AllocationType) -> String {
    match allocation_type {
        AllocationType::Dhcp => "dhcp".to_string(),
        AllocationType::Static => "static".to_string(),
        AllocationType::Slaac => "slaac".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The operator RPCs list a parked reservation and release it by MAC, and
    /// an unscoped release is rejected so a mistake cannot clear everything.
    #[crate::sqlx_test]
    async fn admin_reserved_address_rpcs_list_and_release(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let env = crate::tests::create_test_env(pool).await;
        let mac: mac_address::MacAddress = "02:00:00:00:44:10".parse()?;
        let parked: std::net::IpAddr = "192.0.2.70".parse()?;

        let mut txn = env.api.database_connection.begin().await?;
        db::machine_interface_address::insert_reserved(
            &mut txn,
            mac,
            parked,
            AllocationType::Static,
        )
        .await?;
        txn.commit().await?;

        // Listing with no filter reports the parked reservation's id, then
        // fetching by that id returns its full row.
        let ids = admin_find_reserved_address_ids(
            &env.api,
            Request::new(rpc::AdminFindReservedAddressesRequest {
                reserved_by_mac: None,
                ip_address: None,
            }),
        )
        .await?
        .into_inner();
        assert_eq!(ids.ip_addresses, vec![parked.to_string()]);
        let listed = admin_find_reserved_addresses_by_ids(
            &env.api,
            Request::new(rpc::AdminReservedAddressesByIdsRequest {
                ip_addresses: ids.ip_addresses.clone(),
            }),
        )
        .await?
        .into_inner();
        assert_eq!(listed.reserved_addresses.len(), 1);
        let reserved = &listed.reserved_addresses[0];
        assert_eq!(reserved.ip_address, parked.to_string());
        assert_eq!(reserved.reserved_by_mac, mac.to_string());
        assert_eq!(reserved.allocation_type, "static");

        // An unscoped release is rejected.
        let unscoped = admin_release_reserved_addresses(
            &env.api,
            Request::new(rpc::AdminReleaseReservedAddressesRequest {
                reserved_by_mac: None,
                ip_address: None,
            }),
        )
        .await;
        assert_eq!(unscoped.unwrap_err().code(), tonic::Code::InvalidArgument);

        // Releasing by MAC frees the address and empties the listing.
        let released = admin_release_reserved_addresses(
            &env.api,
            Request::new(rpc::AdminReleaseReservedAddressesRequest {
                reserved_by_mac: Some(mac.to_string()),
                ip_address: None,
            }),
        )
        .await?
        .into_inner();
        assert_eq!(released.released_ip_addresses, vec![parked.to_string()]);

        let after = admin_find_reserved_address_ids(
            &env.api,
            Request::new(rpc::AdminFindReservedAddressesRequest {
                reserved_by_mac: None,
                ip_address: None,
            }),
        )
        .await?
        .into_inner();
        assert!(after.ip_addresses.is_empty());

        Ok(())
    }
}
