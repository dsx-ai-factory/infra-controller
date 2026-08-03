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

use ::rpc::forge::{self as rpc, IsBmcInManagedHostResponse};
use carbide_site_explorer::EndpointExplorationServiceError;
use config_version::ConfigVersion;
use model::bmc_suppression::BmcSuppressionSubsystem;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::utils::resolve_bmc_address;

pub(crate) async fn find_explored_endpoint_ids(
    api: &Api,
    request: Request<::rpc::site_explorer::ExploredEndpointSearchFilter>,
) -> Result<Response<::rpc::site_explorer::ExploredEndpointIdList>, Status> {
    log_request_data(&request);

    let filter: model::site_explorer::ExploredEndpointSearchFilter = request.into_inner().into();

    let endpoint_ips = db::explored_endpoints::find_ips(&api.database_connection, filter).await?;

    Ok(Response::new(
        ::rpc::site_explorer::ExploredEndpointIdList {
            endpoint_ids: endpoint_ips.iter().map(|ip| ip.to_string()).collect(),
        },
    ))
}

pub(crate) async fn find_explored_endpoints_by_ids(
    api: &Api,
    request: Request<::rpc::site_explorer::ExploredEndpointsByIdsRequest>,
) -> Result<Response<::rpc::site_explorer::ExploredEndpointList>, Status> {
    log_request_data(&request);

    let ips: Vec<IpAddr> = request
        .into_inner()
        .endpoint_ids
        .iter()
        .map(|rs| IpAddr::from_str(rs))
        .collect::<Result<Vec<IpAddr>, _>>()
        .map_err(CarbideError::AddressParseError)?;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if ips.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if ips.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let result = db::explored_endpoints::find_by_ips(&api.database_connection, ips)
        .await
        .map(|ep| ::rpc::site_explorer::ExploredEndpointList {
            endpoints: ep
                .into_iter()
                .map(::rpc::site_explorer::ExploredEndpoint::from)
                .collect(),
        })
        .map(Response::new)?;
    Ok(result)
}

pub(crate) async fn find_explored_managed_host_ids(
    api: &Api,
    request: Request<::rpc::site_explorer::ExploredManagedHostSearchFilter>,
) -> Result<Response<::rpc::site_explorer::ExploredManagedHostIdList>, Status> {
    log_request_data(&request);

    let filter: model::site_explorer::ExploredManagedHostSearchFilter = request.into_inner().into();

    let host_ips = db::explored_managed_host::find_ips(&api.database_connection, filter).await?;

    Ok(Response::new(
        ::rpc::site_explorer::ExploredManagedHostIdList {
            host_ids: host_ips.iter().map(|ip| ip.to_string()).collect(),
        },
    ))
}

pub(crate) async fn find_explored_managed_hosts_by_ids(
    api: &Api,
    request: Request<::rpc::site_explorer::ExploredManagedHostsByIdsRequest>,
) -> Result<Response<::rpc::site_explorer::ExploredManagedHostList>, Status> {
    log_request_data(&request);

    let ips: Vec<IpAddr> = request
        .into_inner()
        .host_ids
        .iter()
        .map(|rs| IpAddr::from_str(rs))
        .collect::<Result<Vec<IpAddr>, _>>()
        .map_err(CarbideError::AddressParseError)?;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if ips.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if ips.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let result = db::explored_managed_host::find_by_ips(&api.database_connection, ips)
        .await
        .map(|ep| ::rpc::site_explorer::ExploredManagedHostList {
            managed_hosts: ep
                .into_iter()
                .map(::rpc::site_explorer::ExploredManagedHost::from)
                .collect(),
        })
        .map(Response::new)?;

    Ok(result)
}

pub(crate) async fn get_site_exploration_report(
    api: &Api,
    request: tonic::Request<::rpc::forge::GetSiteExplorationRequest>,
) -> Result<Response<::rpc::site_explorer::SiteExplorationReport>, Status> {
    log_request_data(&request);

    let report = db::site_exploration_report::fetch(&mut api.db_reader()).await?;

    Ok(tonic::Response::new(report.into()))
}

pub(crate) async fn get_site_explorer_last_run(
    api: &Api,
    request: tonic::Request<()>,
) -> Result<Response<::rpc::site_explorer::SiteExplorerLastRunResponse>, Status> {
    log_request_data(&request);

    let last_run = db::site_explorer_run_status::fetch(&mut api.db_reader()).await?;

    Ok(tonic::Response::new(
        ::rpc::site_explorer::SiteExplorerLastRunResponse {
            last_run: last_run.map(Into::into),
        },
    ))
}

pub(crate) async fn find_explored_mlx_device_host_ids(
    api: &Api,
    request: Request<::rpc::site_explorer::ExploredMlxDeviceHostSearchFilter>,
) -> Result<Response<::rpc::site_explorer::ExploredMlxDeviceHostIdList>, Status> {
    log_request_data(&request);

    // The host BMC IPs whose Redfish PCIe inventory carries a BlueField device --
    // the pages the client walks. DPU endpoints are excluded; they report no
    // host-side inventory and would yield no devices.
    let endpoints = db::explored_endpoints::find_all(&api.database_connection).await?;
    let host_ids = endpoints
        .iter()
        .filter(|ep| !ep.report.is_dpu() && ep.report.has_bluefield_devices())
        .map(|ep| ep.address.to_string())
        .collect();

    Ok(Response::new(
        ::rpc::site_explorer::ExploredMlxDeviceHostIdList { host_ids },
    ))
}

pub(crate) async fn find_explored_mlx_devices_by_ids(
    api: &Api,
    request: Request<::rpc::site_explorer::ExploredMlxDevicesByIdsRequest>,
) -> Result<Response<::rpc::site_explorer::ExploredMlxDeviceList>, Status> {
    log_request_data(&request);

    let ips: Vec<IpAddr> = request
        .into_inner()
        .host_ids
        .iter()
        .map(|rs| IpAddr::from_str(rs))
        .collect::<Result<Vec<IpAddr>, _>>()
        .map_err(CarbideError::AddressParseError)?;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if ips.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if ips.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    // Load only the requested host reports, derive the BlueField device serials
    // they hold, then fetch just the DPU endpoints those serials match -- rather
    // than scanning every explored endpoint per page. The serial query is
    // constrained to DPU reports, so a host with a coincidentally matching serial
    // is not pulled in.
    let mut endpoints = db::explored_endpoints::find_by_ips(&api.database_connection, ips).await?;
    let serials: Vec<String> = endpoints
        .iter()
        .flat_map(|ep| ep.report.bluefield_device_serials())
        .collect();
    if !serials.is_empty() {
        let dpus =
            db::explored_endpoints::find_by_dpu_serial_numbers(&api.database_connection, serials)
                .await?;
        endpoints.extend(dpus);
    }

    let devices = model::site_explorer::collect_explored_mlx_devices(&endpoints)
        .into_iter()
        .map(::rpc::site_explorer::ExploredMlxDevice::from)
        .collect();

    Ok(Response::new(::rpc::site_explorer::ExploredMlxDeviceList {
        devices,
    }))
}

pub(crate) async fn clear_site_exploration_error(
    api: &Api,
    request: Request<rpc::ClearSiteExplorationErrorRequest>,
) -> Result<Response<()>, tonic::Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let bmc_ip = IpAddr::from_str(&req.ip_address).map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    db::explored_endpoints::clear_last_known_error(bmc_ip, &mut txn).await?;
    // A terminal preingestion `Failed` state is the operator-visible error for a
    // stuck host, but it lives in `preingestion_state`, not in the exploration
    // report cleared above. Reset it to `Initial` here so clearing the error
    // actually retries preingestion instead of requiring a force-delete of the
    // endpoint. Non-failed states are left untouched.
    if db::explored_endpoints::reset_failed_preingestion(bmc_ip, &mut txn).await? {
        tracing::info!(
            bmc_ip_address = %bmc_ip,
            "Reset failed preingestion to initial after clearing the site exploration error",
        );
    }

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn re_explore_endpoint(
    api: &Api,
    request: Request<rpc::ReExploreEndpointRequest>,
) -> Result<Response<()>, tonic::Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let bmc_ip = IpAddr::from_str(&req.ip_address).map_err(CarbideError::from)?;
    let if_version_match = req
        .if_version_match
        .map(|v| v.parse::<ConfigVersion>())
        .transpose()
        .map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    let eps = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn).await?;
    if eps.is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "explored_endpoint",
            id: bmc_ip.to_string(),
        }
        .into());
    }

    if let Some(bmc_interface) = db::machine_interface::find_by_ip(&mut txn, bmc_ip).await?
        && db::bmc_suppression::is_suppressed(
            txn.as_pgconn(),
            bmc_interface.mac_address,
            BmcSuppressionSubsystem::SiteExplorer,
        )
        .await?
    {
        return Err(
            CarbideError::from(EndpointExplorationServiceError::Suppressed {
                bmc_ip,
                bmc_mac_address: bmc_interface.mac_address,
            })
            .into(),
        );
    }

    for ep in eps.iter() {
        let expected_version = match if_version_match {
            Some(v) => v,
            None => ep.report_version,
        };
        match db::explored_endpoints::re_explore_if_version_matches(
            bmc_ip,
            expected_version,
            &mut txn,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                return Err(CarbideError::ConcurrentModificationError(
                    "explored_endpoint",
                    expected_version.to_string(),
                )
                .into());
            }
            Err(e) => return Err(CarbideError::from(e).into()),
        }
    }

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn refresh_endpoint_report(
    api: &Api,
    request: Request<rpc::RefreshEndpointReportRequest>,
) -> Result<Response<::rpc::site_explorer::ExploredEndpoint>, tonic::Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let bmc_ip = IpAddr::from_str(&req.ip_address).map_err(CarbideError::from)?;
    let endpoint = api
        .endpoint_exploration_service
        .refresh_endpoint_report(bmc_ip)
        .await
        .map_err(CarbideError::from)?;

    Ok(Response::new(endpoint.into()))
}

pub(crate) async fn pause_explored_endpoint_remediation(
    api: &Api,
    request: Request<rpc::PauseExploredEndpointRemediationRequest>,
) -> Result<Response<()>, tonic::Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let bmc_ip = IpAddr::from_str(&req.ip_address).map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    let eps = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn).await?;
    if eps.is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "explored_endpoint",
            id: bmc_ip.to_string(),
        }
        .into());
    }

    // Check if a machine exists for this endpoint
    let in_managed_host =
        carbide_site_explorer::is_endpoint_in_managed_host(bmc_ip, txn.as_pgconn())
            .await
            .map_err(|e| CarbideError::internal(e.to_string()))?;

    if in_managed_host {
        return Err(CarbideError::InvalidArgument(format!(
            "cannot pause/resume remediation for endpoint {bmc_ip} because a machine exists for it"
        ))
        .into());
    }

    db::explored_endpoints::set_pause_remediation(bmc_ip, req.pause, &mut txn).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn is_bmc_in_managed_host(
    api: &Api,
    request: tonic::Request<::rpc::forge::BmcEndpointRequest>,
) -> Result<Response<IsBmcInManagedHostResponse>, tonic::Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let bmc_addr = resolve_bmc_address(&req.ip_address).await?;

    let in_managed_host =
        carbide_site_explorer::is_endpoint_in_managed_host(bmc_addr.ip(), &api.database_connection)
            .await
            .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(Response::new(IsBmcInManagedHostResponse {
        in_managed_host,
    }))
}

pub(crate) async fn delete_explored_endpoint(
    api: &Api,
    request: Request<rpc::DeleteExploredEndpointRequest>,
) -> Result<Response<rpc::DeleteExploredEndpointResponse>, tonic::Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let bmc_ip = IpAddr::from_str(&req.ip_address).map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    // Check if the endpoint exists
    let endpoints = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn).await?;

    if endpoints.is_empty() {
        return Ok(Response::new(rpc::DeleteExploredEndpointResponse {
            deleted: false,
            message: Some(format!("No explored endpoint found with IP {bmc_ip}")),
        }));
    }

    // Check if a machine exists for this endpoint
    let in_managed_host =
        carbide_site_explorer::is_endpoint_in_managed_host(bmc_ip, txn.as_pgconn())
            .await
            .map_err(|e| CarbideError::internal(e.to_string()))?;

    if in_managed_host {
        return Err(CarbideError::InvalidArgument(format!(
            "cannot delete endpoint {bmc_ip} because a machine exists for it. did you mean to force-delete the machine?"
        ))
        .into());
    }

    // Delete the endpoint
    db::explored_endpoints::delete(&mut txn, bmc_ip).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::DeleteExploredEndpointResponse {
        deleted: true,
        message: Some(format!(
            "Successfully deleted explored endpoint with IP {bmc_ip}"
        )),
    }))
}
