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

use carbide_uuid::operating_system::OperatingSystemId;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::api::{Api, rpc};

/// Validates template requirements and returns the computed definition hash on success.
fn validate_template_requirements(
    template_id: &str,
    params: &[rpc::IpxeTemplateParameter],
    artifacts: &[rpc::IpxeTemplateArtifact],
) -> Result<String, Status> {
    use carbide_ipxe_renderer::IpxeScriptRenderer;

    for (i, p) in params.iter().enumerate() {
        if p.name.trim().is_empty() {
            return Err(Status::invalid_argument(format!(
                "ipxe_template_parameters[{i}]: name must not be empty"
            )));
        }
    }
    for (i, a) in artifacts.iter().enumerate() {
        if a.name.trim().is_empty() {
            return Err(Status::invalid_argument(format!(
                "ipxe_template_artifacts[{i}]: name must not be empty"
            )));
        }
        if a.url.trim().is_empty() {
            return Err(Status::invalid_argument(format!(
                "ipxe_template_artifacts[{i}] '{}': url must not be empty",
                a.name
            )));
        }
    }

    let renderer = carbide_ipxe_renderer::DefaultIpxeScriptRenderer::new();

    let ipxeos = rpc_to_ipxe_script(template_id, params, artifacts);
    let hash = renderer.hash(&ipxeos);
    let mut ipxeos_with_hash = ipxeos;
    ipxeos_with_hash.hash = hash.clone();

    renderer
        .validate(&ipxeos_with_hash)
        .map_err(|e| Status::invalid_argument(e.to_string()))?;

    Ok(hash)
}

fn rpc_to_ipxe_script(
    template_id: &str,
    params: &[rpc::IpxeTemplateParameter],
    artifacts: &[rpc::IpxeTemplateArtifact],
) -> carbide_ipxe_renderer::IpxeScript {
    let parameters = params
        .iter()
        .map(|p| carbide_ipxe_renderer::IpxeTemplateParameter {
            name: p.name.clone(),
            value: p.value.clone(),
        })
        .collect();

    let artifacts = artifacts
        .iter()
        .map(|a| {
            let cache_strategy = match a.cache_strategy {
                x if x == rpc::IpxeTemplateArtifactCacheStrategy::LocalOnly as i32 => {
                    carbide_ipxe_renderer::IpxeTemplateArtifactCacheStrategy::LocalOnly
                }
                x if x == rpc::IpxeTemplateArtifactCacheStrategy::CachedOnly as i32 => {
                    carbide_ipxe_renderer::IpxeTemplateArtifactCacheStrategy::CachedOnly
                }
                x if x == rpc::IpxeTemplateArtifactCacheStrategy::RemoteOnly as i32 => {
                    carbide_ipxe_renderer::IpxeTemplateArtifactCacheStrategy::RemoteOnly
                }
                _ => carbide_ipxe_renderer::IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
            };
            carbide_ipxe_renderer::IpxeTemplateArtifact {
                name: a.name.clone(),
                url: a.url.clone(),
                sha: a.sha.clone(),
                auth_type: a.auth_type.clone(),
                auth_token: a.auth_token.clone(),
                cache_strategy,
                cached_url: a.cached_url.clone(),
            }
        })
        .collect();

    carbide_ipxe_renderer::IpxeScript {
        name: String::new(),
        description: None,
        hash: String::new(),
        tenant_id: None,
        ipxe_template_id: template_id.to_string(),
        parameters,
        artifacts,
    }
}

fn params_from_json(json: Option<&serde_json::Value>) -> Vec<rpc::IpxeTemplateParameter> {
    let Some(serde_json::Value::Array(arr)) = json else {
        return vec![];
    };
    arr.iter()
        .filter_map(|v| {
            Some(rpc::IpxeTemplateParameter {
                name: v.get("name")?.as_str()?.to_string(),
                value: v.get("value")?.as_str().unwrap_or("").to_string(),
            })
        })
        .collect()
}

fn artifacts_from_json(json: Option<&serde_json::Value>) -> Vec<rpc::IpxeTemplateArtifact> {
    let Some(serde_json::Value::Array(arr)) = json else {
        return vec![];
    };
    arr.iter()
        .filter_map(|v| {
            Some(rpc::IpxeTemplateArtifact {
                name: v.get("name")?.as_str()?.to_string(),
                url: v.get("url")?.as_str().unwrap_or("").to_string(),
                sha: v.get("sha").and_then(|v| v.as_str()).map(String::from),
                auth_type: v
                    .get("auth_type")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                auth_token: v
                    .get("auth_token")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                cache_strategy: v
                    .get("cache_strategy")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32,
                cached_url: v
                    .get("cached_url")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            })
        })
        .collect()
}

fn parameters_to_json(params: &[rpc::IpxeTemplateParameter]) -> serde_json::Value {
    serde_json::Value::Array(
        params
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "value": p.value,
                })
            })
            .collect(),
    )
}

fn artifacts_to_json(artifacts: &[rpc::IpxeTemplateArtifact]) -> serde_json::Value {
    serde_json::Value::Array(
        artifacts
            .iter()
            .map(|a| {
                serde_json::json!({
                    "name": a.name,
                    "url": a.url,
                    "sha": a.sha,
                    "auth_type": a.auth_type,
                    "auth_token": a.auth_token,
                    "cache_strategy": a.cache_strategy,
                    "cached_url": a.cached_url,
                })
            })
            .collect(),
    )
}

pub(crate) async fn create_operating_system(
    api: &Api,
    request: Request<rpc::CreateOperatingSystemRequest>,
) -> Result<Response<rpc::OperatingSystem>, Status> {
    let mut txn = api.txn_begin().await?;
    let req = request.into_inner();

    let (
        type_,
        ipxe_script,
        ipxe_template_id,
        ipxe_parameters,
        ipxe_artifacts,
        ipxe_definition_hash,
        status,
    ) = if let Some(ref script) = req.ipxe_script {
        (
            model::operating_system_definition::OS_TYPE_IPXE.to_string(),
            Some(script.clone()),
            None,
            None,
            None,
            None,
            db::operating_system::OS_STATUS_READY.to_string(),
        )
    } else if let Some(ref tmpl) = req.ipxe_template_id {
        let tmpl_str = tmpl.to_string();
        // cached_url can only be set via UpdateOperatingSystemCachableIpxeTemplateArtifacts;
        // strip it from regular create requests.
        let mut sanitized: Vec<rpc::IpxeTemplateArtifact> = req.ipxe_template_artifacts.clone();
        for a in &mut sanitized {
            a.cached_url = None;
        }

        let hash =
            validate_template_requirements(&tmpl_str, &req.ipxe_template_parameters, &sanitized)?;

        let params = if req.ipxe_template_parameters.is_empty() {
            None
        } else {
            Some(parameters_to_json(&req.ipxe_template_parameters))
        };
        let arts = if sanitized.is_empty() {
            None
        } else {
            Some(artifacts_to_json(&sanitized))
        };

        // PROVISIONING if any artifact has CACHED_ONLY strategy and cached_url is not set
        // (cached_url is always empty here since we stripped it above).
        let status = if sanitized
            .iter()
            .any(|a| a.cache_strategy == rpc::IpxeTemplateArtifactCacheStrategy::CachedOnly as i32)
        {
            db::operating_system::OS_STATUS_PROVISIONING.to_string()
        } else {
            db::operating_system::OS_STATUS_READY.to_string()
        };

        (
            model::operating_system_definition::OS_TYPE_TEMPLATED_IPXE.to_string(),
            None,
            Some(tmpl_str),
            params,
            arts,
            Some(hash),
            status,
        )
    } else {
        return Err(Status::invalid_argument(
            "exactly one OS variant must be specified: ipxe_script or ipxe_template_id",
        ));
    };

    if req.name.is_empty() {
        return Err(Status::invalid_argument("name is required"));
    }
    // if tenant_organization_id is provided it must not be empty:
    if req
        .tenant_organization_id
        .as_deref()
        .is_some_and(|org| org.is_empty())
    {
        return Err(Status::invalid_argument(
            "tenant_organization_id must not be empty when provided",
        ));
    }

    let id = req.id.map(Uuid::from);

    let input = db::operating_system::CreateOperatingSystem {
        id,
        name: req.name,
        description: req.description,
        org: req.tenant_organization_id,
        type_,
        status,
        is_active: req.is_active,
        allow_override: req.allow_override,
        phone_home_enabled: req.phone_home_enabled,
        user_data: req.user_data,
        ipxe_script,
        ipxe_template_id,
        ipxe_parameters,
        ipxe_artifacts,
        ipxe_definition_hash,
    };

    let row = db::operating_system::create(&mut txn, &input)
        .await
        .map_err(|e| Status::internal(e.to_string()))?;
    txn.commit()
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

    let def: rpc::OperatingSystem =
        model::operating_system_definition::OperatingSystem::from(&row).into();
    Ok(Response::new(def))
}

pub(crate) async fn get_operating_system(
    api: &Api,
    request: Request<OperatingSystemId>,
) -> Result<Response<rpc::OperatingSystem>, Status> {
    let id = Uuid::from(request.into_inner());

    let row = db::operating_system::get(&mut api.db_reader(), id)
        .await
        .map_err(|e| {
            if e.is_not_found() {
                Status::not_found(format!("operating system {id} not found"))
            } else {
                Status::internal(e.to_string())
            }
        })?;

    let def: rpc::OperatingSystem =
        model::operating_system_definition::OperatingSystem::from(&row).into();
    Ok(Response::new(def))
}

pub(crate) async fn update_operating_system(
    api: &Api,
    request: Request<rpc::UpdateOperatingSystemRequest>,
) -> Result<Response<rpc::OperatingSystem>, Status> {
    let mut txn = api.txn_begin().await?;
    let req = request.into_inner();

    let id_proto = req
        .id
        .ok_or_else(|| Status::invalid_argument("id is required"))?;
    let id = Uuid::from(id_proto);

    let existing = db::operating_system::get(&mut txn, id).await.map_err(|e| {
        if e.is_not_found() {
            Status::not_found(format!("operating system {id} not found"))
        } else {
            Status::internal(e.to_string())
        }
    })?;

    let req_template_id: Option<String> = req.ipxe_template_id.map(|id| id.to_string());

    if existing.ipxe_script.is_some() && req_template_id.is_some() {
        return Err(Status::invalid_argument(
            "cannot switch from ipxe_script variant to ipxe_template_id variant",
        ));
    }
    if existing.ipxe_template_id.is_some() && req.ipxe_script.is_some() {
        return Err(Status::invalid_argument(
            "cannot switch from ipxe_template_id variant to ipxe_script variant",
        ));
    }

    let effective_template = req_template_id
        .as_deref()
        .or(existing.ipxe_template_id.as_deref());

    let req_params: Option<&[rpc::IpxeTemplateParameter]> = req
        .ipxe_template_parameters
        .as_ref()
        .map(|w| w.items.as_slice());
    let req_artifacts: Option<&[rpc::IpxeTemplateArtifact]> = req
        .ipxe_template_artifacts
        .as_ref()
        .map(|w| w.items.as_slice());

    let ipxe_definition_hash =
        if let Some(provided_hash) = req.ipxe_template_definition_hash.as_deref() {
            Some(provided_hash.to_owned())
        } else if let Some(tmpl) = effective_template {
            let effective_params: Vec<rpc::IpxeTemplateParameter> = match req_params {
                Some(p) => p.to_vec(),
                None => params_from_json(existing.ipxe_parameters.as_ref().map(|j| &j.0)),
            };
            let effective_artifacts: Vec<rpc::IpxeTemplateArtifact> = match req_artifacts {
                Some(a) => a.to_vec(),
                None => artifacts_from_json(existing.ipxe_artifacts.as_ref().map(|j| &j.0)),
            };
            Some(validate_template_requirements(
                tmpl,
                &effective_params,
                &effective_artifacts,
            )?)
        } else {
            None
        };

    let hash_changed = match (&ipxe_definition_hash, &existing.ipxe_definition_hash) {
        (Some(new_hash), Some(old_hash)) => new_hash != old_hash,
        (Some(_), None) => true,
        _ => false,
    };

    let ipxe_parameters = match req_params {
        Some([]) => Some(serde_json::json!([])),
        Some(p) => Some(parameters_to_json(p)),
        None => None,
    };

    let mut effective_artifacts_for_json = match req_artifacts {
        Some(a) => {
            // cached_url can only be set via UpdateOperatingSystemCachableIpxeTemplateArtifacts;
            // strip it from regular update requests.
            let mut v = a.to_vec();
            for art in &mut v {
                art.cached_url = None;
            }
            v
        }
        None => artifacts_from_json(existing.ipxe_artifacts.as_ref().map(|j| &j.0)),
    };

    if hash_changed {
        for a in &mut effective_artifacts_for_json {
            a.cached_url = None;
        }
    }

    let ipxe_artifacts = if hash_changed || req_artifacts.is_some() {
        Some(artifacts_to_json(&effective_artifacts_for_json))
    } else {
        None
    };

    let status = if hash_changed || req_artifacts.is_some() {
        let needs_provisioning = effective_artifacts_for_json.iter().any(|a| {
            a.cache_strategy == rpc::IpxeTemplateArtifactCacheStrategy::CachedOnly as i32
                && a.cached_url.as_deref().unwrap_or("").is_empty()
        });
        if needs_provisioning {
            Some(db::operating_system::OS_STATUS_PROVISIONING.to_string())
        } else {
            Some(db::operating_system::OS_STATUS_READY.to_string())
        }
    } else {
        None
    };

    let input = db::operating_system::UpdateOperatingSystem {
        id,
        name: req.name,
        description: req.description,
        is_active: req.is_active,
        allow_override: req.allow_override,
        phone_home_enabled: req.phone_home_enabled,
        user_data: req.user_data,
        ipxe_script: req.ipxe_script,
        ipxe_template_id: req_template_id,
        ipxe_parameters,
        ipxe_artifacts,
        ipxe_definition_hash,
        status,
    };

    let row = db::operating_system::update(&mut txn, &existing, &input)
        .await
        .map_err(|e| Status::internal(e.to_string()))?;
    txn.commit()
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

    let def: rpc::OperatingSystem =
        model::operating_system_definition::OperatingSystem::from(&row).into();
    Ok(Response::new(def))
}

pub(crate) async fn delete_operating_system(
    api: &Api,
    request: Request<rpc::DeleteOperatingSystemRequest>,
) -> Result<Response<rpc::DeleteOperatingSystemResponse>, Status> {
    let mut txn = api.txn_begin().await?;
    let req = request.into_inner();

    let id_proto = req
        .id
        .ok_or_else(|| Status::invalid_argument("id is required"))?;
    let id = Uuid::from(id_proto);

    db::operating_system::delete(&mut txn, id)
        .await
        .map_err(|e| {
            if e.is_not_found() {
                Status::not_found(format!("operating system {id} not found"))
            } else {
                Status::internal(e.to_string())
            }
        })?;
    txn.commit()
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

    Ok(Response::new(rpc::DeleteOperatingSystemResponse {}))
}

pub(crate) async fn find_operating_system_ids(
    api: &Api,
    request: Request<rpc::OperatingSystemSearchFilter>,
) -> Result<Response<rpc::OperatingSystemIdList>, Status> {
    let filter = request.into_inner();

    let ids = db::operating_system::list_ids(
        &mut api.db_reader(),
        filter.tenant_organization_id.as_deref(),
    )
    .await
    .map_err(|e| Status::internal(e.to_string()))?;

    let ids = ids.into_iter().map(OperatingSystemId::from).collect();

    Ok(Response::new(rpc::OperatingSystemIdList { ids }))
}

pub(crate) async fn get_operating_system_cachable_ipxe_script_artifacts(
    api: &Api,
    request: Request<rpc::GetOperatingSystemCachableIpxeTemplateArtifactsRequest>,
) -> Result<Response<rpc::IpxeTemplateArtifactList>, Status> {
    let req = request.into_inner();

    let id_proto = req
        .id
        .ok_or_else(|| Status::invalid_argument("id is required"))?;
    let id = Uuid::from(id_proto);

    let row = db::operating_system::get(&mut api.db_reader(), id)
        .await
        .map_err(|e| {
            if e.is_not_found() {
                Status::not_found(format!("operating system {id} not found"))
            } else {
                Status::internal(e.to_string())
            }
        })?;

    let artifacts = artifacts_from_json(row.ipxe_artifacts.as_ref().map(|j| &j.0));
    Ok(Response::new(rpc::IpxeTemplateArtifactList { artifacts }))
}

pub(crate) async fn update_operating_system_cachable_ipxe_script_artifacts(
    api: &Api,
    request: Request<rpc::UpdateOperatingSystemIpxeTemplateArtifactRequest>,
) -> Result<Response<rpc::IpxeTemplateArtifactList>, Status> {
    let mut txn = api.txn_begin().await?;
    let req = request.into_inner();

    let id_proto = req
        .id
        .ok_or_else(|| Status::invalid_argument("id is required"))?;
    let id = Uuid::from(id_proto);

    let existing = db::operating_system::get(&mut txn, id).await.map_err(|e| {
        if e.is_not_found() {
            Status::not_found(format!("operating system {id} not found"))
        } else {
            Status::internal(e.to_string())
        }
    })?;

    let mut artifacts = artifacts_from_json(existing.ipxe_artifacts.as_ref().map(|j| &j.0));

    // Occurrence-based ordered matching:
    // Each update entry consumes the next unmatched artifact (in stored order) whose
    // name matches case-insensitively. This means duplicate names in the OS require
    // duplicate entries in the request to update all occurrences.
    let mut consumed = vec![false; artifacts.len()];
    for update in &req.updates {
        let name_lower = update.name.to_lowercase();
        let idx = consumed
            .iter()
            .zip(artifacts.iter())
            .position(|(used, a)| !used && a.name.to_lowercase() == name_lower)
            .ok_or_else(|| {
                Status::not_found(format!(
                    "artifact '{}' not found (or already matched) in operating system {id}",
                    update.name,
                ))
            })?;
        artifacts[idx].cached_url = update.cached_url.clone();
        consumed[idx] = true;
    }

    // State transition based on CACHED_ONLY artifacts:
    //  - Promote to READY when all CACHED_ONLY artifacts have a non-empty cached_url.
    //  - Demote to PROVISIONING when any CACHED_ONLY artifact loses its cached_url.
    // LOCAL_ONLY artifacts are excluded from this check (their cached_url is inherently set).
    let cached_only: Vec<_> = artifacts
        .iter()
        .filter(|a| a.cache_strategy == rpc::IpxeTemplateArtifactCacheStrategy::CachedOnly as i32)
        .collect();
    let all_cached_only_have_urls = !cached_only.is_empty()
        && cached_only
            .iter()
            .all(|a| a.cached_url.as_deref().is_some_and(|u| !u.is_empty()));
    let new_status =
        if existing.status != db::operating_system::OS_STATUS_READY && all_cached_only_have_urls {
            Some(db::operating_system::OS_STATUS_READY.to_string())
        } else if existing.status == db::operating_system::OS_STATUS_READY
            && !cached_only.is_empty()
            && !all_cached_only_have_urls
        {
            Some(db::operating_system::OS_STATUS_PROVISIONING.to_string())
        } else {
            None
        };

    let ipxe_artifacts = if artifacts.is_empty() {
        None
    } else {
        Some(artifacts_to_json(&artifacts))
    };

    let input = db::operating_system::UpdateOperatingSystem {
        id,
        name: None,
        description: None,
        is_active: None,
        allow_override: None,
        phone_home_enabled: None,
        user_data: None,
        ipxe_script: None,
        ipxe_template_id: None,
        ipxe_parameters: None,
        ipxe_artifacts,
        ipxe_definition_hash: None,
        status: new_status,
    };

    let row = db::operating_system::update(&mut txn, &existing, &input)
        .await
        .map_err(|e| Status::internal(e.to_string()))?;
    txn.commit()
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

    let artifacts = artifacts_from_json(row.ipxe_artifacts.as_ref().map(|j| &j.0));
    Ok(Response::new(rpc::IpxeTemplateArtifactList { artifacts }))
}

pub(crate) async fn find_operating_systems_by_ids(
    api: &Api,
    request: Request<rpc::OperatingSystemsByIdsRequest>,
) -> Result<Response<rpc::OperatingSystemList>, Status> {
    let req = request.into_inner();

    let ids: Vec<Uuid> = req.ids.iter().copied().map(Uuid::from).collect();

    let rows = db::operating_system::get_many(&mut api.db_reader(), &ids)
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

    let operating_systems: Vec<rpc::OperatingSystem> = rows
        .iter()
        .map(|row| model::operating_system_definition::OperatingSystem::from(row).into())
        .collect();

    Ok(Response::new(rpc::OperatingSystemList {
        operating_systems,
    }))
}
