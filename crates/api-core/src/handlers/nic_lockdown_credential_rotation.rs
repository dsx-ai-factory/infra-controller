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
use ::rpc::forge::nic_lockdown_credential_rotation_request::Mode;
use carbide_uuid::machine::HostMachineId;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_machine_id, log_request_data};

/// Operator force-converge escape hatch for NIC lockdown: record (or clear) a
/// request to immediately rekey a host's SuperNIC (SVPC) lockdown keys,
/// bypassing the passive site-wide gate (`nic_lockdown_ikm_rotation_enabled`)
/// and each card's backoff quarantine. The rekey cycle is host-level -- it
/// drives every lagging SVPC card on the host -- so the target is a single host
/// machine id and the one-shot flag is written on that machine's row. The flag
/// is consumed on whichever comes first: the host's next idle sweep
/// (`RotatingNicLockdown`), or its next tenant-allocation lock, where it forces
/// the card to migrate to the site-wide target even while the gate is off. The
/// flag is cleared when the idle sweep settles; this handler only writes it.
pub(crate) async fn trigger_nic_lockdown_credential_rotation(
    api: &Api,
    request: Request<rpc::NicLockdownCredentialRotationRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let mode = req.mode();

    let machine_id: HostMachineId = req
        .machine_id
        .ok_or_else(|| CarbideError::InvalidArgument("machine_id must be provided".to_string()))?;
    log_machine_id(&machine_id);

    let mut txn = api.txn_begin().await?;

    match mode {
        Mode::Set => {
            db::machine::set_lockdown_ikm_credential_rotation_requested(&mut txn, machine_id)
                .await?;
        }
        Mode::Clear => {
            db::machine::clear_lockdown_ikm_credential_rotation_requested(&mut txn, machine_id)
                .await?;
        }
        Mode::Unspecified => {
            return Err(
                CarbideError::InvalidArgument("mode must be set or clear".to_string()).into(),
            );
        }
    };

    txn.commit().await?;

    Ok(Response::new(()))
}
