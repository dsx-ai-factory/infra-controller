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
use carbide_uuid::machine::MachineId;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{BomValidating, ManagedHostState};
use model::sku::Sku;
use rpc::forge::{RemoveSkuRequest, SkuIdList};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

pub(crate) async fn create(
    api: &Api,
    request: Request<::rpc::forge::SkuList>,
) -> Result<Response<::rpc::forge::SkuIdList>, Status> {
    log_request_data(&request);
    let mut txn = api.txn_begin().await?;

    let sku_list = request.into_inner();

    let mut sku_ids = SkuIdList::default();

    for sku in sku_list.skus {
        let sku: Sku = sku.into();
        db::sku::create(&mut txn, &sku)
            .await
            .map_err(CarbideError::from)?;
        sku_ids.ids.push(sku.id);
    }

    txn.commit().await?;

    Ok(Response::new(sku_ids))
}

pub(crate) async fn delete(api: &Api, request: Request<SkuIdList>) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let mut txn = api.txn_begin().await?;

    let sku_id_list = request.into_inner().ids;
    let mut skus = db::sku::find(&mut txn, &sku_id_list).await?;

    let Some(sku) = skus.pop() else {
        return Err(CarbideError::InvalidArgument(format!(
            "the SKU {} does not exist",
            sku_id_list
                .first()
                .map(|id| id.to_string())
                .unwrap_or_default()
        ))
        .into());
    };
    if !skus.is_empty() {
        return Err(CarbideError::NotImplemented.into());
    }

    let machine_ids =
        db::machine::find_machine_ids_by_sku_ids(&mut txn, std::slice::from_ref(&sku.id)).await?;
    let machines_using_sku = machine_ids
        .get(&sku.id)
        .map(|machine_ids| machine_ids.len())
        .unwrap_or(0);
    if machines_using_sku > 0 {
        return Err(CarbideError::InvalidArgument(format!(
            "the SKU is in use by {machines_using_sku} machines"
        ))
        .into());
    }

    db::sku::delete(&mut txn, &sku.id).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn generate_from_machine(
    api: &Api,
    request: Request<MachineId>,
) -> Result<Response<::rpc::forge::Sku>, Status> {
    log_request_data(&request);
    let machine_id = convert_and_log_machine_id(Some(&request.into_inner()))?;

    let sku = db::sku::generate_sku_from_machine(&api.database_connection, &machine_id).await?;

    Ok(Response::new(sku.into()))
}

pub(crate) async fn assign_to_machine(
    api: &Api,
    request: Request<::rpc::forge::SkuMachinePair>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let mut txn = api.txn_begin().await?;

    let sku_machine_pair = request.into_inner();
    let machine_id = convert_and_log_machine_id::<MachineId>(sku_machine_pair.machine_id.as_ref())?;

    let machine =
        db::machine::find_one(&mut txn, &machine_id, MachineSearchConfig::default()).await?;

    let machine = machine.ok_or(CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;

    if !sku_machine_pair.force {
        if let Some(assigned_sku) = machine.config.hw_sku {
            return Err(CarbideError::FailedPrecondition(format!(
                "the specified machine already has a SKU assigned ({assigned_sku})"
            ))
            .into());
        }

        match machine.current_state() {
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(_),
            }
            | ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuVerificationFailed(_),
            }
            | ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuMissing(_),
            }
            | ManagedHostState::Ready => {}
            _ => {
                return Err(CarbideError::FailedPrecondition(
                    "specified machine is not in a valid state for assigning a SKU".to_string(),
                )
                .into());
            }
        }
    }

    let mut skus = db::sku::find(&mut txn, std::slice::from_ref(&sku_machine_pair.sku_id)).await?;

    let sku = skus.pop().ok_or(CarbideError::NotFoundError {
        kind: "SKU ID",
        id: sku_machine_pair.sku_id.clone(),
    })?;

    if !skus.is_empty() {
        return Err(CarbideError::internal(format!(
            "unexpected additional SKUs found for ID: {}",
            sku_machine_pair.sku_id.clone()
        ))
        .into());
    }

    db::machine::assign_sku(&mut txn, &machine_id, &sku.id).await?;

    db::machine::update_sku_status_verify_request_time(&mut txn, &machine_id).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn verify_for_machine(
    api: &Api,
    request: Request<MachineId>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let machine_id = convert_and_log_machine_id::<MachineId>(Some(&request.into_inner()))?;

    let mut txn = api.txn_begin().await?;

    let machine =
        db::machine::find_one(&mut txn, &machine_id, MachineSearchConfig::default()).await?;

    let machine = machine.ok_or(CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;

    match machine.current_state() {
        ManagedHostState::Ready
        | ManagedHostState::BomValidating {
            bom_validating_state: BomValidating::SkuVerificationFailed(_),
        } => {}
        _ => {
            return Err(CarbideError::FailedPrecondition(
                "specified machine is not in a valid state for machine SKU verification"
                    .to_string(),
            )
            .into());
        }
    }

    db::machine::update_sku_status_verify_request_time(&mut txn, &machine_id).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn remove_sku_association(
    api: &Api,
    request: Request<RemoveSkuRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id::<MachineId>(request.machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;

    let machine =
        db::machine::find_one(&mut txn, &machine_id, MachineSearchConfig::default()).await?;

    let machine = machine.ok_or(CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;

    if !request.force {
        match machine.current_state() {
            ManagedHostState::Ready
            | ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuVerificationFailed(_),
            } => {}
            _ => {
                return Err(CarbideError::FailedPrecondition(
                    "specified machine is not in a valid state for removing SKU association"
                        .to_string(),
                )
                .into());
            }
        }
    }
    db::machine::unassign_sku(&mut txn, &machine_id).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn get_all_ids(
    api: &Api,
    request: Request<()>,
) -> Result<Response<::rpc::forge::SkuIdList>, Status> {
    log_request_data(&request);
    let sku_ids = db::sku::get_sku_ids(&api.database_connection).await?;

    Ok(Response::new(::rpc::forge::SkuIdList {
        ids: sku_ids.into_iter().collect(),
    }))
}

pub(crate) async fn find_skus_by_ids(
    api: &Api,
    request: Request<::rpc::forge::SkusByIdsRequest>,
) -> Result<Response<::rpc::forge::SkuList>, Status> {
    log_request_data(&request);

    let sku_ids = request.into_inner().ids;
    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if sku_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if sku_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    let skus = db::sku::find(&mut txn, &sku_ids).await?;

    let mut rpc_skus: Vec<rpc::forge::Sku> =
        skus.into_iter().map(std::convert::Into::into).collect();

    let mut machine_ids_by_sku_ids =
        db::machine::find_machine_ids_by_sku_ids(&mut txn, &sku_ids).await?;
    txn.commit().await?;

    for rpc_sku in rpc_skus.iter_mut() {
        if let Some(associated_machine_ids) = machine_ids_by_sku_ids.remove(&rpc_sku.id) {
            rpc_sku.associated_machine_ids = associated_machine_ids;
        }
    }

    Ok(Response::new(rpc::forge::SkuList { skus: rpc_skus }))
}

pub(crate) async fn update_sku_metadata(
    api: &Api,
    request: Request<::rpc::forge::SkuUpdateMetadataRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let request = request.into_inner();

    let mut txn = api.txn_begin().await?;

    db::sku::update_metadata(
        &mut txn,
        request.sku_id,
        request.description,
        request.device_type,
    )
    .await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn replace_sku(
    api: &Api,
    request: Request<::rpc::forge::Sku>,
) -> Result<Response<rpc::forge::Sku>, Status> {
    let request = request.into_inner().into();
    let mut txn = api.txn_begin().await?;

    let sku = db::sku::replace(&mut txn, &request).await?;

    txn.commit().await?;

    Ok(Response::new(sku.into()))
}
