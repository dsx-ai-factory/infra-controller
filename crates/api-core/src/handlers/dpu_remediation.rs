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
use ::rpc::model::RpcTryFrom;
use db::dpu_remediation::AppliedRemediationIdQueryType;
use model::dpu_remediation::{
    ApproveRemediation, DisableRemediation, EnableRemediation, NewRemediation, RevokeRemediation,
};
use tonic::{Request, Response, Status};

use crate::api::Api;
use crate::auth;
use crate::errors::CarbideError;

/// all of the requests that modify a remediation _require_ an external_user_name from a client cert.
/// (even if that particular request doesn't actually persist the name, it's always at least logged)
/// this is how we enforce that an actual human is doing this with their own certificates, rather than
/// "something in the ether".  We do this so that we can always audit who did what to an env.
fn external_user_name<T>(request: &Request<T>) -> Result<String, CarbideError> {
    if let Some(external_user_name) = request
        .extensions()
        .get::<auth::AuthContext>()
        .and_then(|auth_context| auth_context.get_external_user_name())
        .map(String::from)
    {
        tracing::info!(
            external_user_name = %external_user_name,
            "Resolved remediation RPC user name from certificate",
        );
        Ok(external_user_name)
    } else {
        Err(CarbideError::ClientCertificateMissingInformation(
            "client certificate is missing external user name".to_string(),
        ))
    }
}

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateRemediationRequest>,
) -> Result<Response<rpc::CreateRemediationResponse>, Status> {
    crate::api::log_request_data(&request);
    let authored_by = external_user_name(&request)?;

    let mut txn = api.txn_begin().await?;
    let response = Ok(db::dpu_remediation::persist_remediation(
        NewRemediation::rpc_try_from((request.into_inner(), authored_by))?,
        &mut txn,
    )
    .await
    .map(rpc::CreateRemediationResponse::from)
    .map(Response::new)?);

    txn.commit().await?;

    response
}

pub(crate) async fn approve(
    api: &Api,
    request: Request<rpc::ApproveRemediationRequest>,
) -> Result<Response<()>, Status> {
    crate::api::log_request_data(&request);
    let approved_by = external_user_name(&request)?;

    let mut txn = api.txn_begin().await?;

    db::dpu_remediation::persist_approve_remediation(
        ApproveRemediation::rpc_try_from((request.into_inner(), approved_by))?,
        &mut txn,
    )
    .await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn revoke(
    api: &Api,
    request: Request<rpc::RevokeRemediationRequest>,
) -> Result<Response<()>, Status> {
    crate::api::log_request_data(&request);
    let revoked_by = external_user_name(&request)?;

    let mut txn = api.txn_begin().await?;

    db::dpu_remediation::persist_revoke_remediation(
        RevokeRemediation::rpc_try_from((request.into_inner(), revoked_by))?,
        &mut txn,
    )
    .await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn enable(
    api: &Api,
    request: Request<rpc::EnableRemediationRequest>,
) -> Result<Response<()>, Status> {
    crate::api::log_request_data(&request);
    let enabled_by = external_user_name(&request)?;

    let mut txn = api.txn_begin().await?;

    db::dpu_remediation::persist_enable_remediation(
        EnableRemediation::rpc_try_from((request.into_inner(), enabled_by))?,
        &mut txn,
    )
    .await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn disable(
    api: &Api,
    request: Request<rpc::DisableRemediationRequest>,
) -> Result<Response<()>, Status> {
    crate::api::log_request_data(&request);
    let disabled_by = external_user_name(&request)?;

    let mut txn = api.txn_begin().await?;

    db::dpu_remediation::persist_disable_remediation(
        DisableRemediation::rpc_try_from((request.into_inner(), disabled_by))?,
        &mut txn,
    )
    .await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn find_remediation_ids(
    api: &Api,
    request: Request<()>,
) -> Result<Response<rpc::RemediationIdList>, Status> {
    crate::api::log_request_data(&request);
    let mut txn = api.txn_begin().await?;

    let remediation_ids = db::dpu_remediation::find_remediation_ids(&mut txn).await?;
    let response = rpc::RemediationIdList { remediation_ids };

    txn.commit().await?;

    Ok(Response::new(response))
}

pub(crate) async fn find_remediations_by_ids(
    api: &Api,
    request: Request<rpc::RemediationIdList>,
) -> Result<Response<rpc::RemediationList>, Status> {
    crate::api::log_request_data(&request);
    let mut txn = api.txn_begin().await?;

    let remediation_ids = request.into_inner().remediation_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if remediation_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if remediation_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let db_remediations =
        db::dpu_remediation::find_remediations_by_ids(&mut txn, &remediation_ids).await?;

    let response = Response::new(rpc::RemediationList {
        remediations: db_remediations
            .into_iter()
            .map(rpc::Remediation::from)
            .collect::<Vec<_>>(),
    });

    txn.commit().await?;

    Ok(response)
}

pub(crate) async fn find_applied_remediation_ids(
    api: &Api,
    request: Request<rpc::FindAppliedRemediationIdsRequest>,
) -> Result<Response<rpc::AppliedRemediationIdList>, Status> {
    crate::api::log_request_data(&request);
    let mut txn = api.txn_begin().await?;

    let request = request.into_inner();

    let id_query_args = match (request.remediation_id, request.dpu_machine_id) {
        (Some(_remediation_id), Some(_machine_id)) => {
            //illegal, must provide exactly one
            Err(CarbideError::InvalidArgument(
                "cannot provide both remediation id and machine id, exactly one argument required"
                    .to_string(),
            ))
        }
        (None, None) => {
            //illegal, must provide exactly one
            Err(CarbideError::InvalidArgument(
                "must provide either remediation id or machine id, exactly one argument required"
                    .to_string(),
            ))
        }
        (Some(remediation_id), None) => {
            Ok(AppliedRemediationIdQueryType::RemediationId(remediation_id))
        }
        (None, Some(machine_id)) => Ok(AppliedRemediationIdQueryType::Machine(machine_id)),
    }?;

    let (remediation_ids, dpu_machine_ids) =
        db::dpu_remediation::find_applied_remediation_ids(&mut txn, id_query_args).await?;

    let response = rpc::AppliedRemediationIdList {
        remediation_ids,
        dpu_machine_ids,
    };

    txn.commit().await?;

    Ok(Response::new(response))
}

pub(crate) async fn find_applied_remediations(
    api: &Api,
    request: Request<rpc::FindAppliedRemediationsRequest>,
) -> Result<Response<rpc::AppliedRemediationList>, Status> {
    crate::api::log_request_data(&request);
    let mut txn = api.txn_begin().await?;

    let request = request.into_inner();

    let remediation_id = request
        .remediation_id
        .ok_or(CarbideError::MissingArgument("remediation id"))?;
    let machine_id = request
        .dpu_machine_id
        .ok_or(CarbideError::MissingArgument("dpu machine id"))?;

    let applied_remediations =
        db::dpu_remediation::find_remediations_by_remediation_id_and_machine(
            &mut txn,
            remediation_id,
            &machine_id,
        )
        .await?
        .into_iter()
        .map(|x| x.into())
        .collect();

    let response = rpc::AppliedRemediationList {
        applied_remediations,
    };

    txn.commit().await?;

    Ok(Response::new(response))
}

pub(crate) async fn get_next_remediation_for_machine(
    api: &Api,
    request: Request<rpc::GetNextRemediationForMachineRequest>,
) -> Result<Response<rpc::GetNextRemediationForMachineResponse>, Status> {
    crate::api::log_request_data(&request);
    let mut txn = api.txn_begin().await?;

    let request = request.into_inner();
    let machine_id = request
        .dpu_machine_id
        .ok_or(CarbideError::MissingArgument("machine id"))?;

    let remediation_to_apply =
        db::dpu_remediation::find_next_remediation_for_machine(&mut txn, machine_id).await?;

    let remediation_id = remediation_to_apply.as_ref().map(|r| r.id);
    let remediation_script = remediation_to_apply.map(|r| r.script);

    let response = Response::new(rpc::GetNextRemediationForMachineResponse {
        remediation_id,
        remediation_script,
    });

    txn.commit().await?;

    Ok(response)
}

pub(crate) async fn remediation_applied(
    api: &Api,
    request: Request<rpc::RemediationAppliedRequest>,
) -> Result<Response<()>, Status> {
    crate::api::log_request_data(&request);
    let mut txn = api.txn_begin().await?;

    let request = request.into_inner();
    let remediation_id = request
        .remediation_id
        .ok_or(CarbideError::MissingArgument("remediation id"))?;
    let machine_id = request
        .dpu_machine_id
        .ok_or(CarbideError::MissingArgument("machine id"))?;
    let status: model::dpu_remediation::RemediationApplicationStatus = request
        .status
        .ok_or(CarbideError::MissingArgument("status"))?
        .try_into()?;

    db::dpu_remediation::remediation_applied(&mut txn, machine_id, remediation_id, status).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}
