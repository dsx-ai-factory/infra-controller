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

use mac_address::MacAddress;
use rpc::forge as rpc;
use tonic::{Request, Response};

use crate::api::Api;
use crate::errors::CarbideError;

pub(crate) async fn expire_dhcp_lease(
    api: &Api,
    request: Request<rpc::ExpireDhcpLeaseRequest>,
) -> Result<Response<rpc::ExpireDhcpLeaseResponse>, CarbideError> {
    let rpc::ExpireDhcpLeaseRequest {
        ip_address,
        mac_address,
    } = request.into_inner();
    let ip_address: IpAddr = ip_address.parse()?;

    let mac_address: Option<MacAddress> = mac_address
        .as_deref()
        .map(|m| m.parse::<MacAddress>().map_err(CarbideError::from))
        .transpose()?;

    if !api.runtime_config.dhcp_lease_expiry_handling {
        // Controlled by the `dhcp_lease_expiry_handling` runtime config flag (default: disabled).
        // The problem with lease expiry handling is that
        // 1. If a BMC IP is released, there is no way to update it in machine_topologies table,
        //    which causes a mismatch between machine_interface and topology entry.
        // 2. Since this might cauase BMC IP to change, DPF right now does not support BMC IP
        //    change. Again a mismatch between DPF and NICo.
        // 3. State machine can't process the host since there is no address attached to a interface.
        // Blocking this handling for now and will revisit once DPF releases the fix.
        tracing::info!("Expire lease handling for DHCP is disabled.");
        return Ok(Response::new(rpc::ExpireDhcpLeaseResponse {
            ip_address: ip_address.to_string(),
            status: rpc::ExpireDhcpLeaseStatus::FeatureDisabled.into(),
        }));
    }

    let mut txn = api.txn_begin().await?;

    // When the caller provides the MAC, scope the delete to the (ip, mac) pair.
    // Otherwise use the address-only variant, which is what an admin-cli call
    // deleting a specific IP allocation would hit. Either way, both variants
    // return the interfaces whose rows were actually deleted, so we resync those
    // authoritative owners rather than a separately looked-up interface (which
    // could differ if ownership changed between the lookup and deletion).
    let resync_targets = match mac_address {
        Some(mac) => {
            db::machine_interface_address::delete_by_address_and_mac(
                &mut txn,
                ip_address,
                mac,
                model::allocation_type::AllocationType::Dhcp,
            )
            .await?
        }
        None => {
            db::machine_interface_address::delete_by_address(
                &mut txn,
                ip_address,
                model::allocation_type::AllocationType::Dhcp,
            )
            .await?
        }
    };
    let deleted = !resync_targets.is_empty();

    // Sync the hostname to the remaining address state so DNS stays
    // consistent: the IP style re-derives (and re-derives again from the next
    // allocated IP on rediscovery); the other styles keep their names and only
    // drop out of DNS while addressless.
    for iface_id in &resync_targets {
        db::machine_interface::sync_hostname_after_address_change(&mut txn, *iface_id).await?;
    }

    txn.commit().await?;

    let status = if deleted {
        tracing::info!(
            %ip_address,
            ?mac_address,
            "Released expired DHCP lease allocation"
        );
        rpc::ExpireDhcpLeaseStatus::Released
    } else {
        tracing::debug!(
            %ip_address,
            ?mac_address,
            "No allocation found for expired DHCP lease"
        );
        rpc::ExpireDhcpLeaseStatus::NotFound
    };

    Ok(Response::new(rpc::ExpireDhcpLeaseResponse {
        ip_address: ip_address.to_string(),
        status: status.into(),
    }))
}
