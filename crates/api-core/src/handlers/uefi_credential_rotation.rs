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
use ::rpc::forge as rpc;
use ::rpc::forge::uefi_credential_rotation_request::Mode;
use carbide_uuid::machine::MachineId;
use mac_address::MacAddress;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_machine_id, log_request_data};

/// Operator force-converge escape hatch for UEFI: record (or clear) a request to
/// immediately rotate a machine's UEFI credential, bypassing the passive
/// site-wide gate and the device's backoff quarantine. The target machine is
/// addressed by its id, by its BMC MAC (which keys the UEFI rotation
/// bookkeeping), or a combination (see [`resolve_target`]); the flag is written
/// on that machine's row. The machine's state controller consumes the request on
/// its next sweep; this handler only writes the flag (it performs no Redfish
/// work itself).
///
/// Unlike a BMC, a UEFI credential only ever belongs to a machine (a host or a
/// DPU -- both are machine rows), never a switch or power shelf, so the target
/// is always a single machine id. A DPU is addressed by its own `machine_id` or
/// its DPU BMC MAC, driving `RotatingDpuUefi`; a host drives `RotatingHostUefi`.
pub(crate) async fn trigger_uefi_credential_rotation(
    api: &Api,
    request: Request<rpc::UefiCredentialRotationRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let mode = req.mode();

    let mut txn = api.txn_begin().await?;

    let machine_id = resolve_target(&mut txn, req.machine_id, req.bmc_mac).await?;

    match mode {
        Mode::Set => {
            db::machine::set_uefi_credential_rotation_requested(&mut txn, machine_id).await?;
        }
        Mode::Clear => {
            db::machine::clear_uefi_credential_rotation_requested(&mut txn, machine_id).await?;
        }
        // An omitted `mode` decodes as `Unspecified`; reject it rather than let a
        // request fall through to an action it did not name.
        Mode::Unspecified => {
            return Err(
                CarbideError::InvalidArgument("mode must be set or clear".to_string()).into(),
            );
        }
    };

    txn.commit().await?;

    Ok(Response::new(()))
}

/// Resolve the machine that owns the target UEFI credential from an operator
/// request carrying a `machine_id`, a BMC MAC, or both. A machine has exactly
/// one UEFI credential, so any single identifier uniquely names it. When a MAC
/// is supplied alongside a `machine_id` they must agree, which lets an operator
/// double-check that a MAC pulled from an alert really is the BMC of the machine
/// they mean.
async fn resolve_target(
    txn: &mut PgConnection,
    machine_id: Option<MachineId>,
    bmc_mac: Option<String>,
) -> Result<MachineId, CarbideError> {
    let bmc_mac = bmc_mac
        .map(|mac| {
            mac.parse::<MacAddress>().map_err(|_| {
                CarbideError::InvalidArgument(format!("bmc_mac '{mac}' is not a valid MAC address"))
            })
        })
        .transpose()?;

    // A MAC uniquely names the BMC of one machine; resolve which machine owns
    // it. UEFI rotation is keyed by the BMC MAC (host UEFI by the host BMC MAC,
    // DPU UEFI by the DPU BMC MAC), so the machine resolver's BMC-MAC lookup is
    // the right one for either -- `find_machine_id_by_bmc_mac` matches any
    // machine's BMC interface, DPU rows included -- and unlike a bare BMC
    // credential, a UEFI credential only ever belongs to a machine, never a
    // switch or power shelf.
    let mac_machine_id = match bmc_mac {
        Some(mac) => Some(
            db::machine_topology::find_machine_id_by_bmc_mac(txn, mac)
                .await?
                .ok_or(CarbideError::NotFoundError {
                    kind: "BMC",
                    id: mac.to_string(),
                })?,
        ),
        None => None,
    };

    let machine_id = match (machine_id, mac_machine_id) {
        (Some(id), None) => id,
        (Some(id), Some(mac_id)) => {
            if id != mac_id {
                return Err(CarbideError::InvalidArgument(format!(
                    "bmc {} belongs to machine {mac_id}, not the requested machine {id}",
                    bmc_mac.expect("a mac target implies a parsed mac")
                )));
            }
            id
        }
        (None, Some(mac_id)) => mac_id,
        (None, None) => {
            return Err(CarbideError::InvalidArgument(
                "one of machine_id or bmc_mac must be provided".to_string(),
            ));
        }
    };

    log_machine_id(&machine_id);
    Ok(machine_id)
}
