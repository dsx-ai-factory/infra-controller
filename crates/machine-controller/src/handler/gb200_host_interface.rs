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

//! Checks and restores the BMC host interface that GB200 SBIOS uses for
//! Redfish access.

use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use carbide_redfish::libredfish::host_interface::{
    HostInterfaceStaticIpv4Repair, ensure_gb200_host_interface_static_ipv4,
    gb200_host_interface_static_ipv4_is_missing,
};
use libredfish::Redfish;
use model::machine::ManagedHostStateSnapshot;
use model::rack_type::RackProductFamily;
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use crate::context::MachineStateHandlerContextObjects;

/// Reports whether a GB200 waiting for `scout` needs its BMC host interface
/// restored before another boot can refresh the UEFI HTTP option.
pub(super) async fn gb200_host_interface_address_is_missing(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<bool, StateHandlerError> {
    if !is_reported_gb200(ctx, mh_snapshot).await? {
        return Ok(false);
    }

    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;
    gb200_host_interface_static_ipv4_is_missing(redfish_client.as_ref())
        .await
        .map_err(|error| redfish_error("gb200_host_interface_static_ipv4_is_missing", error))
}

/// Restores the expected address only for hosts identified as GB200 by their
/// persisted Site Explorer report.
pub(super) async fn repair_gb200_host_interface_if_needed(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<(), StateHandlerError> {
    if !is_reported_gb200(ctx, mh_snapshot).await? {
        return Ok(());
    }

    match ensure_gb200_host_interface_static_ipv4(redfish_client)
        .await
        .map_err(|error| redfish_error("ensure_gb200_host_interface_static_ipv4", error))?
    {
        HostInterfaceStaticIpv4Repair::AlreadyConfigured => {}
        HostInterfaceStaticIpv4Repair::Patched => tracing::warn!(
            machine_id = %mh_snapshot.host_snapshot.id,
            "Restored the GB200 BMC hostusb0 static IPv4 address before host platform configuration"
        ),
        HostInterfaceStaticIpv4Repair::Conflicting => {
            return Err(StateHandlerError::ManualInterventionRequired(format!(
                "GB200 host {} has unexpected static IPv4 configuration on hostusb0; refusing to overwrite it",
                mh_snapshot.host_snapshot.id,
            )));
        }
    }

    Ok(())
}

/// Uses only the persisted Redfish report so stale or missing rack association
/// data cannot enable this BMC write on other hardware.
async fn is_reported_gb200(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<bool, StateHandlerError> {
    let Some(host_bmc_ip) = mh_snapshot.host_snapshot.status.bmc_info.ip else {
        return Ok(false);
    };

    let endpoints =
        db::explored_endpoints::find_by_ips(&mut ctx.services.db_reader, vec![host_bmc_ip]).await?;
    let product_family = endpoints
        .first()
        .and_then(|endpoint| endpoint.report.model())
        .as_deref()
        .and_then(RackProductFamily::from_hardware_model);

    Ok(product_family == Some(RackProductFamily::Gb200))
}
