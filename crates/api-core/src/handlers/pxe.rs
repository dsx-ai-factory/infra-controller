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

use ::rpc::forge as rpc;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::client_resolution::{
    resolve_cloud_init_instructions, resolve_machine_interface,
};
use crate::ipxe::{PxeInstructionRequest, PxeInstructions, PxeInstructionsInput};

// The carbide pxe server makes this RPC call
pub(crate) async fn get_pxe_instructions(
    api: &Api,
    request: Request<rpc::PxeInstructionRequest>,
) -> Result<Response<rpc::PxeInstructions>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let pxe_request: PxeInstructionRequest = request.into_inner().try_into()?;

    // Keep address ownership and provisioning instructions in one transaction.
    // Instance allocation takes ACCESS EXCLUSIVE on instance_addresses, so it
    // cannot add another owner while Core builds the response for this request.
    // Resolve the client_ip carbide-pxe observed (XFF or TCP peer) to a host
    // machine_interface, either via direct machine_interface_addresses lookup
    // or via instance_address for tenant-allocated machines.
    let iface = resolve_machine_interface(txn.as_pgconn(), pxe_request.client_ip).await?;

    let input = PxeInstructionsInput {
        interface_id: iface.id,
        arch: pxe_request.arch,
        product: pxe_request.product,
    };
    let pxe_script = PxeInstructions::get_pxe_instructions(&mut txn, input).await?;

    // For interfaces on the static-assignments segment, include
    // URL overrides so external hosts can reach services via an
    // alternate hostname or IP they can resolve and/or connect
    // to for carbide-pxe and carbide-api.
    let (api_url_override, pxe_url_override, static_pxe_url_override) = {
        let is_external = iface.segment_id
            == db::network_segment::static_assignments(txn.as_pgconn())
                .await
                .map(|s| s.id)
                .unwrap_or_default();

        if is_external {
            (
                api.runtime_config.external_api_url.clone(),
                api.runtime_config.external_pxe_url.clone(),
                api.runtime_config
                    .external_static_pxe_url
                    .clone()
                    .or_else(|| api.runtime_config.external_pxe_url.clone()),
            )
        } else {
            (None, None, None)
        }
    };

    txn.commit().await?;

    Ok(Response::new(rpc::PxeInstructions {
        pxe_script,
        api_url_override,
        pxe_url_override,
        static_pxe_url_override,
    }))
}

pub(crate) async fn get_cloud_init_instructions(
    api: &Api,
    request: Request<rpc::CloudInitInstructionsRequest>,
) -> Result<Response<rpc::CloudInitInstructions>, Status> {
    log_request_data(&request);

    let ip_str = &request.into_inner().ip;
    let ip: IpAddr = ip_str
        .parse()
        .map_err(|e| CarbideError::InvalidArgument(format!("failed parsing IP '{ip_str}': {e}")))?;

    // Note that this code path supports IPv6 at the *API layer*, but won't be
    // able to be exercised until DHCPv6 is working, which is a whole other thing
    // we need to work on: machines need an IPv6 address before they can request
    // cloud-init instructions over IPv6, and while we've made changes to site
    // prefix, network segment, and IP allocators behind the scenes for supporting
    // dual stacking interfaces, none of that means much until DHCPv6 is working
    // to actually hand those addresses out.

    // Keep address ownership, readiness, and response material in one
    // transaction. Instance allocation takes ACCESS EXCLUSIVE on
    // instance_addresses, so a second owner cannot appear while Core selects
    // tenant data.
    let mut txn = api.txn_begin().await?;
    let instructions = resolve_cloud_init_instructions(api, txn.as_pgconn(), ip).await?;
    txn.commit().await?;

    Ok(Response::new(instructions))
}
