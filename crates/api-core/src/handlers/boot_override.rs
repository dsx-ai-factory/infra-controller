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
use carbide_uuid::machine::MachineInterfaceId;
use model::machine_boot_override::MachineBootOverride;

use crate::api::Api;

pub(crate) async fn get(
    api: &Api,
    request: tonic::Request<MachineInterfaceId>,
) -> Result<tonic::Response<rpc::MachineBootOverride>, tonic::Status> {
    crate::api::log_request_data(&request);

    let machine_interface_id = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let machine_id = match db::machine_interface::find_one(&mut txn, machine_interface_id).await {
        Ok(interface) => interface.machine_id,
        Err(_) => None,
    };

    if let Some(machine_id) = machine_id {
        crate::api::log_machine_id(&machine_id);
    }

    let mbo = match db::machine_boot_override::find_optional(txn.as_pgconn(), machine_interface_id)
        .await?
    {
        Some(mbo) => mbo,
        None => MachineBootOverride {
            machine_interface_id,
            custom_pxe: None,
            custom_user_data: None,
        },
    };

    txn.commit().await?;

    Ok(tonic::Response::new(mbo.into()))
}

pub(crate) async fn set(
    api: &Api,
    request: tonic::Request<rpc::MachineBootOverride>,
) -> Result<tonic::Response<()>, tonic::Status> {
    crate::api::log_request_data(&request);

    let mbo: MachineBootOverride = request.into_inner().try_into()?;
    let mut txn = api.txn_begin().await?;

    let machine_id = match db::machine_interface::find_one(&mut txn, mbo.machine_interface_id).await
    {
        Ok(interface) => interface.machine_id,
        Err(_) => None,
    };
    match machine_id {
        Some(machine_id) => {
            crate::api::log_machine_id(&machine_id);
            tracing::warn!(
                machine_interface_id = mbo.machine_interface_id.to_string(),
                machine_id = machine_id.to_string(),
                "Boot override for machine_interface_id is active. Bypassing regular boot"
            );
        }

        None => tracing::warn!(
            machine_interface_id = mbo.machine_interface_id.to_string(),
            "Boot override for machine_interface_id is active. Bypassing regular boot"
        ),
    }

    db::machine_boot_override::update_or_insert(&mbo, &mut txn).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

pub(crate) async fn clear(
    api: &Api,
    request: tonic::Request<MachineInterfaceId>,
) -> Result<tonic::Response<()>, tonic::Status> {
    crate::api::log_request_data(&request);

    let machine_interface_id = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let machine_id = match db::machine_interface::find_one(&mut txn, machine_interface_id).await {
        Ok(interface) => interface.machine_id,
        Err(_) => None,
    };
    match machine_id {
        Some(machine_id) => {
            crate::api::log_machine_id(&machine_id);
            tracing::info!(
                machine_interface_id = machine_interface_id.to_string(),
                machine_id = machine_id.to_string(),
                "Boot override for machine_interface_id disabled."
            );
        }

        None => tracing::info!(
            machine_interface_id = machine_interface_id.to_string(),
            "Boot override for machine_interface_id disabled"
        ),
    }
    db::machine_boot_override::clear(&mut txn, machine_interface_id).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}
