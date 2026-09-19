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
use carbide_instrument::{DynamicLog, Event, LabelValue, LogAt, emit};
use carbide_network::ip::ipset::aggregate_prefixes;
use carbide_network::ip::prefix::IpPrefix;
use ipnetwork::IpNetwork;
use model::site_prefix::{
    NewTenantManagedSitePrefix, RetireTenantManagedSitePrefix, SitePrefix, SitePrefixAuthority,
    SitePrefixLifecycleState, UpdateSitePrefixMetadata,
};
use model::tenant::TenantOrganizationId;
use tonic::{Request, Response, Status};

use crate::api::{Api, log_request_data};
use crate::{CarbideError, CarbideResult};

#[derive(Clone, Copy, Debug, Eq, LabelValue, PartialEq)]
enum SitePrefixAdmissionResult {
    Created,
    Idempotent,
    Rejected,
    Failed,
}

#[derive(Event)]
#[event(
    event_name = "site_prefix_admission",
    metric_name = "carbide_site_prefix_admission_total",
    component = "nico-api",
    log = dynamic,
    metric = counter,
    message = "Processed a tenant SitePrefix admission request",
    describe = "Number of completed tenant SitePrefix admission attempts, by result."
)]
struct SitePrefixAdmission {
    #[label]
    result: SitePrefixAdmissionResult,
    #[context]
    site_prefix_id: String,
    #[context]
    tenant_organization_id: String,
    #[context]
    prefix: String,
    #[context]
    error: String,
}

impl DynamicLog for SitePrefixAdmission {
    fn log_at(&self) -> LogAt {
        match self.result {
            SitePrefixAdmissionResult::Created | SitePrefixAdmissionResult::Idempotent => {
                LogAt::Off
            }
            SitePrefixAdmissionResult::Rejected => LogAt::Level(tracing::Level::WARN),
            SitePrefixAdmissionResult::Failed => LogAt::Level(tracing::Level::ERROR),
        }
    }
}

fn admission_result_for_error(error: &CarbideError) -> SitePrefixAdmissionResult {
    match error {
        CarbideError::AlreadyFoundError { .. }
        | CarbideError::FailedPrecondition(_)
        | CarbideError::InvalidArgument(_)
        | CarbideError::InvalidConfiguration(_)
        | CarbideError::MissingArgument(_)
        | CarbideError::NotFoundError { .. }
        | CarbideError::ResourceExhausted(_)
        | CarbideError::TenantSitePrefixQuotaExceeded { .. }
        | CarbideError::SitePrefixIsolationLimitExceeded { .. } => {
            SitePrefixAdmissionResult::Rejected
        }
        _ => SitePrefixAdmissionResult::Failed,
    }
}

fn compact_prefixes(prefixes: impl IntoIterator<Item = IpNetwork>) -> Vec<IpNetwork> {
    let prefixes = prefixes.into_iter().map(|prefix| {
        // Configured prefixes can include host bits; `IpPrefix` requires the network address.
        IpPrefix::try_from((prefix.network(), prefix.prefix()))
            .expect("an existing IP network has a valid prefix length and network address")
    });
    aggregate_prefixes(prefixes)
        .into_iter()
        .map(|prefix| match prefix {
            IpPrefix::V4(prefix) => IpNetwork::V4(prefix.into()),
            IpPrefix::V6(prefix) => IpNetwork::V6(prefix.into()),
        })
        .collect()
}

/// Combines current configured roots with retained tenant roots for DPU isolation.
/// The caller holds `tenant_prefix_overlap::lock_config` or `lock_checks`.
/// Operator rows awaiting removal do not replace the configured-root source of truth.
pub(super) async fn protected_prefixes(
    api: &Api,
    txn: &mut sqlx::PgConnection,
) -> CarbideResult<Vec<IpNetwork>> {
    let tenant_prefixes = db::site_prefix::find_tenant_prefixes(txn).await?;
    Ok(compact_prefixes(
        api.eth_data
            .site_fabric_prefixes
            .as_ref()
            .map(|prefixes| prefixes.as_ip_slice())
            .unwrap_or_default()
            .iter()
            .copied()
            .chain(tenant_prefixes),
    ))
}

fn validate_isolation_admission(
    api: &Api,
    current_prefixes: &[IpNetwork],
    candidate: IpNetwork,
) -> CarbideResult<()> {
    use super::tenant_prefix_overlap::contains_prefix;

    if let Some(denied) =
        api.runtime_config.deny_prefixes.iter().find(|denied| {
            contains_prefix(**denied, candidate) || contains_prefix(candidate, **denied)
        })
    {
        return Err(CarbideError::InvalidArgument(format!(
            "tenant SitePrefix {candidate} overlaps configured deny prefix {denied}"
        )));
    }

    let used = current_prefixes.len();
    let requested = compact_prefixes(current_prefixes.iter().copied().chain([candidate])).len();
    let limit = api.runtime_config.max_site_prefix_isolation_rules;
    // A lowered limit freezes new roots, even if adding a sibling would
    // compact the rendered set back below the limit. Existing retries bypass
    // this check because they do not introduce another resource.
    if used > limit as usize || requested > limit as usize {
        return Err(CarbideError::SitePrefixIsolationLimitExceeded {
            used,
            requested,
            limit,
        });
    }
    Ok(())
}

fn emit_admission(
    result: SitePrefixAdmissionResult,
    site_prefix_id: &str,
    tenant_organization_id: &str,
    prefix: &str,
    error: impl ToString,
) {
    emit(SitePrefixAdmission {
        result,
        site_prefix_id: site_prefix_id.to_string(),
        tenant_organization_id: tenant_organization_id.to_string(),
        prefix: prefix.to_string(),
        error: error.to_string(),
    });
}

#[derive(Clone, Copy, Debug, Eq, LabelValue, PartialEq)]
enum SitePrefixLifecycleEventState {
    Provisioning,
    Ready,
    Deleting,
    Error,
}

impl From<SitePrefixLifecycleState> for SitePrefixLifecycleEventState {
    fn from(value: SitePrefixLifecycleState) -> Self {
        match value {
            SitePrefixLifecycleState::Provisioning => Self::Provisioning,
            SitePrefixLifecycleState::Ready => Self::Ready,
            SitePrefixLifecycleState::Deleting => Self::Deleting,
            SitePrefixLifecycleState::Error => Self::Error,
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "site_prefix_retirement",
    metric_name = "carbide_site_prefix_retirements_total",
    component = "nico-api",
    log = info,
    metric = counter,
    message = "Recorded tenant SitePrefix retirement intent",
    describe = "Number of tenant SitePrefix retirements, by previous lifecycle state."
)]
struct SitePrefixRetirement {
    #[label]
    previous_state: SitePrefixLifecycleEventState,
    #[context]
    site_prefix_id: String,
    #[context]
    tenant_organization_id: String,
}

fn authorize_tenant_mutation(
    site_prefix: &SitePrefix,
    tenant_organization_id: &TenantOrganizationId,
) -> Result<(), CarbideError> {
    if site_prefix.status.authority != SitePrefixAuthority::TenantManaged {
        return Err(CarbideError::FailedPrecondition(
            "operator-managed SitePrefixes cannot be changed through the tenant API".to_string(),
        ));
    }
    if site_prefix.config.tenant_organization_id.as_ref() != Some(tenant_organization_id) {
        return Err(CarbideError::PermissionDeniedError(
            "the SitePrefix is not owned by the requested tenant".to_string(),
        ));
    }
    Ok(())
}

fn with_quota(
    site_prefix: SitePrefix,
    quota_use: &std::collections::HashMap<String, u32>,
    quota_limit: u32,
) -> rpc::SitePrefix {
    let tenant_organization_id = site_prefix
        .config
        .tenant_organization_id
        .as_ref()
        .map(ToString::to_string);
    let site_prefix = rpc::SitePrefix::from(site_prefix);
    match tenant_organization_id {
        Some(tenant_organization_id) => site_prefix.with_quota(
            quota_use
                .get(&tenant_organization_id)
                .copied()
                .unwrap_or_default(),
            quota_limit,
        ),
        None => site_prefix,
    }
}

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::SitePrefixCreationRequest>,
) -> Result<Response<rpc::SitePrefix>, Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let site_prefix_id = request
        .id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let tenant_organization_id = request.tenant_organization_id.clone();
    let prefix = request.prefix.clone();
    let new_site_prefix = match NewTenantManagedSitePrefix::try_from(request) {
        Ok(site_prefix) => site_prefix,
        Err(error) => {
            emit_admission(
                SitePrefixAdmissionResult::Rejected,
                &site_prefix_id,
                &tenant_organization_id,
                &prefix,
                &error,
            );
            return Err(error.into());
        }
    };
    let quota_tenant_organization_id = new_site_prefix.tenant_organization_id.clone();

    let quota_limit = api.runtime_config.max_site_prefixes_per_tenant;
    let mut txn = match api.txn_begin().await {
        Ok(txn) => txn,
        Err(error) => {
            emit_admission(
                SitePrefixAdmissionResult::Failed,
                &site_prefix_id,
                &tenant_organization_id,
                &prefix,
                &error,
            );
            return Err(CarbideError::from(error).into());
        }
    };
    let result: CarbideResult<_> = async {
        db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
        let current_prefixes = protected_prefixes(api, &mut txn).await?;
        let result =
            db::site_prefix::create_tenant_managed(new_site_prefix, quota_limit, &mut txn).await?;
        // Resolve retries first so later policy changes do not reject an existing root.
        if result.disposition == db::site_prefix::CreateDisposition::Created {
            validate_isolation_admission(api, &current_prefixes, result.site_prefix.config.prefix)?;
        }
        Ok(result)
    }
    .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            emit_admission(
                admission_result_for_error(&error),
                &site_prefix_id,
                &tenant_organization_id,
                &prefix,
                &error,
            );
            return Err(error.into());
        }
    };
    let used = match db::site_prefix::count_tenant_managed(&mut txn, &quota_tenant_organization_id)
        .await
    {
        Ok(used) => used,
        Err(error) => {
            emit_admission(
                SitePrefixAdmissionResult::Failed,
                &site_prefix_id,
                &tenant_organization_id,
                &prefix,
                &error,
            );
            return Err(CarbideError::from(error).into());
        }
    };
    let response = rpc::SitePrefix::from(result.site_prefix).with_quota(used, quota_limit);
    if let Err(error) = txn.commit().await {
        emit_admission(
            SitePrefixAdmissionResult::Failed,
            &site_prefix_id,
            &tenant_organization_id,
            &prefix,
            &error,
        );
        return Err(CarbideError::from(error).into());
    }

    let admission_result = match result.disposition {
        db::site_prefix::CreateDisposition::Created => SitePrefixAdmissionResult::Created,
        db::site_prefix::CreateDisposition::Existing => SitePrefixAdmissionResult::Idempotent,
    };
    emit_admission(
        admission_result,
        &site_prefix_id,
        &tenant_organization_id,
        &prefix,
        "",
    );

    Ok(Response::new(response))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::SitePrefixUpdateRequest>,
) -> Result<Response<rpc::SitePrefix>, Status> {
    log_request_data(&request);

    let update = UpdateSitePrefixMetadata::try_from(request.into_inner())?;
    let mut txn = api.txn_begin().await?;
    let current = db::site_prefix::find_by_id_for_update(&mut txn, update.id)
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "site prefix",
            id: update.id.to_string(),
        })?;
    authorize_tenant_mutation(&current, &update.tenant_organization_id)?;
    if current.status.lifecycle_state == SitePrefixLifecycleState::Deleting {
        return Err(CarbideError::FailedPrecondition(
            "a deleting SitePrefix cannot be updated".to_string(),
        )
        .into());
    }

    let expected_version = update.if_version_match.unwrap_or(current.version);
    let site_prefix =
        db::site_prefix::update_tenant_metadata(&update, expected_version, &mut txn).await?;
    let used =
        db::site_prefix::count_tenant_managed(&mut txn, &update.tenant_organization_id).await?;
    let response = rpc::SitePrefix::from(site_prefix)
        .with_quota(used, api.runtime_config.max_site_prefixes_per_tenant);
    txn.commit().await?;

    Ok(Response::new(response))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::SitePrefixDeletionRequest>,
) -> Result<Response<rpc::SitePrefixDeletionResult>, Status> {
    log_request_data(&request);

    let retire = RetireTenantManagedSitePrefix::try_from(request.into_inner())?;
    let mut txn = api.txn_begin().await?;
    let current = db::site_prefix::find_by_id_for_update(&mut txn, retire.id)
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "site prefix",
            id: retire.id.to_string(),
        })?;
    authorize_tenant_mutation(&current, &retire.tenant_organization_id)?;
    let previous_state = current.status.lifecycle_state;

    let site_prefix = db::site_prefix::retire_tenant_managed(&retire, &current, &mut txn).await?;
    let used =
        db::site_prefix::count_tenant_managed(&mut txn, &retire.tenant_organization_id).await?;
    let response = rpc::SitePrefix::from(site_prefix)
        .with_quota(used, api.runtime_config.max_site_prefixes_per_tenant);
    txn.commit().await?;

    if previous_state != SitePrefixLifecycleState::Deleting {
        emit(SitePrefixRetirement {
            previous_state: previous_state.into(),
            site_prefix_id: retire.id.to_string(),
            tenant_organization_id: retire.tenant_organization_id.to_string(),
        });
    }

    Ok(Response::new(rpc::SitePrefixDeletionResult {
        site_prefix: Some(response),
    }))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::SitePrefixSearchFilter>,
) -> Result<Response<rpc::SitePrefixIdList>, Status> {
    log_request_data(&request);

    let filter: model::site_prefix::SitePrefixSearchFilter = request.into_inner().try_into()?;
    let site_prefix_ids = db::site_prefix::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::SitePrefixIdList { site_prefix_ids }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::SitePrefixesByIdsRequest>,
) -> Result<Response<rpc::SitePrefixList>, Status> {
    log_request_data(&request);

    let site_prefix_ids = request.into_inner().site_prefix_ids;
    if site_prefix_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if site_prefix_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    }

    let mut reader = api.db_reader();
    let site_prefixes =
        db::site_prefix::find_by_ids(reader.as_mut(), site_prefix_ids.as_slice()).await?;
    let tenant_organization_ids: Vec<_> = site_prefixes
        .iter()
        .filter_map(|site_prefix| site_prefix.config.tenant_organization_id.clone())
        .collect();
    let quota_use = db::site_prefix::count_tenant_managed_by_organizations(
        reader.as_mut(),
        &tenant_organization_ids,
    )
    .await?;
    let quota_limit = api.runtime_config.max_site_prefixes_per_tenant;
    let site_prefixes = site_prefixes
        .into_iter()
        .map(|site_prefix| with_quota(site_prefix, &quota_use, quota_limit))
        .collect();

    Ok(Response::new(rpc::SitePrefixList { site_prefixes }))
}

pub(crate) async fn find_state_histories(
    api: &Api,
    request: Request<rpc::SitePrefixStateHistoriesRequest>,
) -> Result<Response<rpc::StateHistories>, Status> {
    log_request_data(&request);

    let site_prefix_ids = request.into_inner().site_prefix_ids;
    if site_prefix_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }
    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if site_prefix_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    }

    let mut txn = api.txn_begin().await?;
    let results = db::state_history::find_by_object_ids(
        &mut txn,
        db::state_history::StateHistoryTableId::SitePrefix,
        &site_prefix_ids,
    )
    .await?;
    let mut response = rpc::StateHistories::default();
    for (site_prefix_id, records) in results {
        response.histories.insert(
            site_prefix_id,
            rpc::StateHistoryRecords {
                records: records.into_iter().map(Into::into).collect(),
            },
        );
    }
    txn.commit().await?;

    Ok(Response::new(response))
}
