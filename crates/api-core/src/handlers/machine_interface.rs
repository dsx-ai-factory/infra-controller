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
use std::str::FromStr;

use ::rpc::forge as rpc;
use db::WithTransaction;
use futures_util::FutureExt;
use itertools::Itertools;
use mac_address::MacAddress;
use model::machine_interface::InterfaceType;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};

pub(crate) async fn find_interfaces(
    api: &Api,
    request: Request<rpc::InterfaceSearchQuery>,
) -> Result<Response<rpc::InterfaceList>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;

    let rpc::InterfaceSearchQuery { id, ip } = request.into_inner();

    let interfaces: Vec<rpc::MachineInterface> = match (id, ip) {
        (Some(id), _) => vec![
            db::machine_interface::find_one(&mut txn, id)
                .await
                .map_err(crate::CarbideError::from)?
                .into(),
        ],
        (None, Some(ip)) => match IpAddr::from_str(ip.as_ref()) {
            Ok(ip) => match db::machine_interface::find_by_ip(&mut txn, ip)
                .await
                .map_err(crate::CarbideError::from)?
            {
                Some(interface) => vec![interface.into()],
                None => {
                    return Err(CarbideError::internal(format!(
                        "no machine interface with IP {ip} was found"
                    ))
                    .into());
                }
            },
            Err(_) => {
                return Err(CarbideError::internal(
                    "could not marshall an IP from the request".to_string(),
                )
                .into());
            }
        },
        (None, None) => match db::machine_interface::find_all(&mut txn).await {
            Ok(machine_interfaces) => machine_interfaces
                .into_iter()
                .map(|i| i.into())
                .collect_vec(),
            Err(error) => return Err(CarbideError::from(error).into()),
        },
    };

    txn.commit().await.map_err(crate::CarbideError::from)?;

    Ok(Response::new(rpc::InterfaceList { interfaces }))
}

pub(crate) async fn delete_interface(
    api: &Api,
    request: Request<rpc::InterfaceDeleteQuery>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;

    let rpc::InterfaceDeleteQuery { id, mac_address } = request.into_inner();

    // Resolve the interfaces to delete. Deleting by MAC exists for the case where the
    // operator only has the BMC MAC and not the interface id; a MAC is unique per
    // network segment, so it can match more than one interface.
    let interfaces = match (id, mac_address) {
        (Some(id), None) => match db::machine_interface::find_one(&mut txn, id).await {
            Ok(interface) => vec![interface],
            // Report an unknown id as not-found rather than letting it fall through as an
            // internal error, matching the unknown-MAC arm below.
            Err(db::DatabaseError::FindOneReturnedNoResultsError(_)) => {
                return Err(CarbideError::NotFoundError {
                    kind: "Machine Interface",
                    id: id.to_string(),
                }
                .into());
            }
            Err(e) => return Err(CarbideError::from(e).into()),
        },
        (None, Some(mac_address)) => {
            let mac = MacAddress::from_str(&mac_address).map_err(|e| {
                CarbideError::InvalidArgument(format!("invalid MAC address {mac_address:?}: {e}"))
            })?;
            let mut interfaces = db::machine_interface::find_by_mac_address(txn.as_pgconn(), mac)
                .await
                .map_err(crate::CarbideError::from)?;
            if interfaces.is_empty() {
                return Err(CarbideError::NotFoundError {
                    kind: "Machine Interface",
                    id: mac.to_string(),
                }
                .into());
            }
            // `machine_interface::delete` retains each row's boot pair keyed by MAC, so
            // when several rows share this MAC the last one deleted decides the retained
            // `boot_interface_id`. The lookup has no inherent order, so sort to make that
            // outcome deterministic rather than dependent on the query plan.
            interfaces.sort_by_key(|interface| interface.id);
            interfaces
        }
        (Some(_), Some(_)) => {
            return Err(CarbideError::InvalidArgument(
                "specify either id or mac_address, not both".to_string(),
            )
            .into());
        }
        (None, None) => {
            return Err(
                CarbideError::MissingArgument("delete interface.id or .mac_address").into(),
            );
        }
    };

    // Check every interface before deleting any of them, so a MAC that matches one
    // deletable and one attached interface refuses as a whole instead of half-deleting.
    for interface in &interfaces {
        // There should not be any machine associated with this interface.
        if let Some(machine_id) = interface.machine_id {
            if interface.interface_type == InterfaceType::Bmc {
                return Err(CarbideError::InvalidArgument(format!(
                    "this looks like a BMC interface and attached with machine: {machine_id}. delete that first"
                ))
                .into());
            }
            return Err(CarbideError::InvalidArgument(format!(
                "already a machine {machine_id} is attached to this interface. delete that first"
            ))
            .into());
        }

        // Nor any other live owner. Unlike `machine_id`, these associations do not stop
        // the row being deleted at the database level, so they have to be refused here --
        // selecting by MAC can match rows the caller never saw, so a switch or power
        // shelf must not be taken out as collateral.
        if let Some(dpu_machine_id) = interface.attached_dpu_machine_id {
            return Err(CarbideError::InvalidArgument(format!(
                "this interface is attached to DPU machine {dpu_machine_id}. delete that first"
            ))
            .into());
        }
        if let Some(switch_id) = interface.switch_id {
            return Err(CarbideError::InvalidArgument(format!(
                "this interface belongs to switch {switch_id}. delete that first"
            ))
            .into());
        }
        if let Some(power_shelf_id) = interface.power_shelf_id {
            return Err(CarbideError::InvalidArgument(format!(
                "this interface belongs to power shelf {power_shelf_id}. delete that first"
            ))
            .into());
        }

        // There should not be any BMC information associated with any machine.
        for address in interface.addresses.iter() {
            let machine_id = db::machine_topology::find_machine_id_by_bmc_ip(
                txn.as_pgconn(),
                &address.to_string(),
            )
            .await
            .map_err(crate::CarbideError::from)?;

            if let Some(machine_id) = machine_id {
                return Err(CarbideError::InvalidArgument(format!(
                    "this looks like a BMC interface and attached with machine: {machine_id}. delete that first"
                ))
                .into());
            }
        }
    }

    for interface in &interfaces {
        db::machine_interface::delete(&interface.id, &mut txn)
            .await
            .map_err(crate::CarbideError::from)?;
    }

    txn.commit().await.map_err(crate::CarbideError::from)?;

    Ok(Response::new(()))
}

pub(crate) async fn find_mac_address_by_bmc_ip(
    api: &Api,
    request: Request<rpc::BmcIp>,
) -> Result<Response<rpc::MacAddressBmcIp>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let bmc_ip = req.bmc_ip;

    let interface = db::machine_interface::find_by_ip(
        &api.database_connection,
        bmc_ip
            .parse()
            .map_err(|e| CarbideError::InvalidArgument(format!("invalid IP address: {e}")))?,
    )
    .await
    .map_err(crate::CarbideError::from)?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "machine_interface",
        id: bmc_ip.clone(),
    })?;

    Ok(Response::new(rpc::MacAddressBmcIp {
        bmc_ip,
        mac_address: interface.mac_address.to_string(),
    }))
}

pub(crate) async fn find_bmc_ips(
    api: &Api,
    request: Request<rpc::FindBmcIpsRequest>,
) -> Result<Response<rpc::BmcIpList>, Status> {
    use rpc::find_bmc_ips_request::LookupBy;

    log_request_data(&request);

    let req = request.into_inner();

    let bmc_ips = match req.lookup_by {
        Some(LookupBy::MacAddress(mac_address)) => {
            db::machine_interface::lookup_bmc_ip_by_mac_address(
                &api.database_connection,
                mac_address.parse().map_err(|e| {
                    CarbideError::InvalidArgument(format!("invalid MAC address: {e}"))
                })?,
            )
            .await
            .map_err(crate::CarbideError::from)?
        }
        Some(LookupBy::Serial(serial)) => {
            // Get the machine ID for this serial
            let machine_ids =
                db::machine_topology::find_by_serial(&api.database_connection, &serial)
                    .await
                    .map_err(crate::CarbideError::from)?;
            if machine_ids.len() > 1 {
                tracing::warn!(
                    serial,
                    "Multiple machines match serial number, cannot resolve to BMC IP"
                );
                return Ok(Response::new(rpc::BmcIpList::default()));
            }
            let Some(machine_id) = machine_ids.into_iter().next() else {
                return Ok(Response::new(rpc::BmcIpList::default()));
            };

            // Resolve the BMC IP from the live interface, not the discovery topology
            // snapshot, so a released or changed lease can't surface a stale IP.
            let Some(bmc_ip) = api
                .with_txn(|txn| {
                    async move {
                        db::machine_topology::find_machine_bmc_pairs_by_machine_id(
                            txn,
                            vec![machine_id],
                        )
                        .await
                    }
                    .boxed()
                })
                .await
                .map_err(crate::CarbideError::from)?
                .map_err(crate::CarbideError::from)?
                .into_iter()
                .find_map(|(_, ip)| ip)
            else {
                return Ok(Response::new(rpc::BmcIpList::default()));
            };

            // The address comes from a Postgres `inet` column, so it parses today --
            // but don't silently swallow the error if that ever changes: warn and skip.
            match bmc_ip.parse::<IpAddr>() {
                Ok(ip) => vec![ip],
                Err(e) => {
                    tracing::warn!(
                        bmc_ip_address = %bmc_ip,
                        error = %e,
                        "BMC IP from machine_interfaces did not parse; skipping"
                    );
                    return Ok(Response::new(rpc::BmcIpList::default()));
                }
            }
        }
        None => return Err(CarbideError::MissingArgument("lookup_by").into()),
    };

    Ok(Response::new(rpc::BmcIpList {
        bmc_ips: bmc_ips.into_iter().map(|ip| ip.to_string()).collect(),
    }))
}
