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

use ::rpc::forge::{scout_stream_api_bound_message, scout_stream_scout_bound_message};
use ::rpc::protos::forge::ScoutStreamScoutBoundMessage;
use ::rpc::protos::mlx_device;
use carbide_uuid::machine::MachineId;
use libmlx::profile::serialization::SerializableProfile;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

pub(crate) async fn profile_sync(
    api: &Api,
    request: Request<mlx_device::MlxAdminProfileSyncRequest>,
) -> Result<Response<mlx_device::MlxAdminProfileSyncResponse>, Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response =
        handle_profile_sync(api, machine_id, request.device_id, request.profile_name).await?;
    Ok(Response::new(response))
}

pub(crate) fn profile_show(
    api: &Api,
    request: Request<mlx_device::MlxAdminProfileShowRequest>,
) -> Result<Response<mlx_device::MlxAdminProfileShowResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let response = handle_profile_show(api, request.profile_name)?;
    Ok(Response::new(response))
}

pub(crate) async fn profile_compare(
    api: &Api,
    request: Request<mlx_device::MlxAdminProfileCompareRequest>,
) -> Result<Response<mlx_device::MlxAdminProfileCompareResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response =
        handle_profile_compare(api, machine_id, request.device_id, request.profile_name).await?;
    Ok(Response::new(response))
}

pub(crate) fn profile_list(
    api: &Api,
    request: Request<mlx_device::MlxAdminProfileListRequest>,
) -> Result<Response<mlx_device::MlxAdminProfileListResponse>, Status> {
    log_request_data(&request);
    let response = handle_profile_list(api)?;
    Ok(Response::new(response))
}

pub(crate) async fn lockdown_lock(
    api: &Api,
    request: Request<mlx_device::MlxAdminLockdownLockRequest>,
) -> Result<Response<mlx_device::MlxAdminLockdownLockResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_lockdown_lock(api, machine_id, request.device_id).await?;
    Ok(Response::new(response))
}

pub(crate) async fn lockdown_unlock(
    api: &Api,
    request: Request<mlx_device::MlxAdminLockdownUnlockRequest>,
) -> Result<Response<mlx_device::MlxAdminLockdownUnlockResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_lockdown_unlock(api, machine_id, request.device_id).await?;
    Ok(Response::new(response))
}

pub(crate) async fn lockdown_status(
    api: &Api,
    request: Request<mlx_device::MlxAdminLockdownStatusRequest>,
) -> Result<Response<mlx_device::MlxAdminLockdownStatusResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_lockdown_status(api, machine_id, request.device_id).await?;
    Ok(Response::new(response))
}

pub(crate) async fn show_device_info(
    api: &Api,
    request: Request<mlx_device::MlxAdminDeviceInfoRequest>,
) -> Result<Response<mlx_device::MlxAdminDeviceInfoResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_show_device_info(api, machine_id, request.device_id).await?;
    Ok(Response::new(response))
}

pub(crate) async fn show_device_report(
    api: &Api,
    request: Request<mlx_device::MlxAdminDeviceReportRequest>,
) -> Result<Response<mlx_device::MlxAdminDeviceReportResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_show_device_report(api, machine_id).await?;
    Ok(Response::new(response))
}

pub(crate) async fn registry_list(
    api: &Api,
    request: Request<mlx_device::MlxAdminRegistryListRequest>,
) -> Result<Response<mlx_device::MlxAdminRegistryListResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_registry_list(api, machine_id).await?;
    Ok(Response::new(response))
}

pub(crate) async fn registry_show(
    api: &Api,
    request: Request<mlx_device::MlxAdminRegistryShowRequest>,
) -> Result<Response<mlx_device::MlxAdminRegistryShowResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_registry_show(api, machine_id, request.registry_name).await?;
    Ok(Response::new(response))
}

pub(crate) async fn config_query(
    api: &Api,
    request: Request<mlx_device::MlxAdminConfigQueryRequest>,
) -> Result<Response<mlx_device::MlxAdminConfigQueryResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_config_query(
        api,
        machine_id,
        request.device_id,
        request.registry_name,
        request.variables,
    )
    .await?;
    Ok(Response::new(response))
}

pub(crate) async fn config_set(
    api: &Api,
    request: Request<mlx_device::MlxAdminConfigSetRequest>,
) -> Result<Response<mlx_device::MlxAdminConfigSetResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_config_set(
        api,
        machine_id,
        request.device_id,
        request.registry_name,
        request.assignments,
    )
    .await?;
    Ok(Response::new(response))
}

pub(crate) async fn config_sync(
    api: &Api,
    request: Request<mlx_device::MlxAdminConfigSyncRequest>,
) -> Result<Response<mlx_device::MlxAdminConfigSyncResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_config_sync(
        api,
        machine_id,
        request.device_id,
        request.registry_name,
        request.assignments,
    )
    .await?;
    Ok(Response::new(response))
}

pub(crate) async fn config_compare(
    api: &Api,
    request: Request<mlx_device::MlxAdminConfigCompareRequest>,
) -> Result<Response<mlx_device::MlxAdminConfigCompareResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let response = handle_config_compare(
        api,
        machine_id,
        request.device_id,
        request.registry_name,
        request.assignments,
    )
    .await?;
    Ok(Response::new(response))
}

// handle_profile_sync is an internal helper method for handling a profile sync call.
async fn handle_profile_sync(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
    profile_name: String,
) -> Result<mlx_device::MlxAdminProfileSyncResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    // Check if mlxconfig profiles are configured.
    let profiles = api
        .runtime_config
        .mlxconfig_profiles
        .as_ref()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "mlxconfig_profiles",
            id: "configured".into(),
        })?;

    // Get the profile from the loaded profiles.
    let profile = profiles
        .get(&profile_name)
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "mlxconfig_profile",
            id: profile_name.clone(),
        })?;

    // Convert MlxConfigProfile to SerializableProfile, then to JSON.
    let serializable_profile =
        SerializableProfile::from_profile(profile).map_err(|e| CarbideError::Internal {
            message: format!("failed to convert mlxconfig profile to serializable profile: {e}"),
        })?;

    let serializable_profile_pb: mlx_device::SerializableMlxConfigProfile = serializable_profile
        .try_into()
        .map_err(|e| CarbideError::Internal {
            message: format!("failed to convert serializable profile into pb: {e}"),
        })?;

    // Create the request to send to the agent.
    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceProfileSyncRequest(
            mlx_device::MlxDeviceProfileSyncRequest {
                profile_name: profile_name.clone(),
                device_id: device_id.clone(),
                serializable_profile: Some(serializable_profile_pb),
            },
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to sync profile to scout: {}",
                status.message()
            ),
        })?;

    // And now extract the response from the scout agent and
    // pass it back along to the administrative caller.
    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceProfileSyncResponse(
            profile_sync_response,
        )) => match profile_sync_response.reply {
            // Note: Right now this passes the protobuf-encoded SyncResult
            // straight on through back to the CLI without deserializing and
            // processing any of it. This could be an opportunity, if decided,
            // to deserialize and do any sort of processing, and then re-serialize.
            Some(mlx_device::mlx_device_profile_sync_response::Reply::SyncResult(sync_result)) => {
                Ok(mlx_device::MlxAdminProfileSyncResponse {
                    sync_result: Some(sync_result),
                })
            }
            Some(mlx_device::mlx_device_profile_sync_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error syncing profile to device (machine_id={machine_id}, device_id={device_id}, profile_name={profile_name}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty sync result reply (machine_id={machine_id}, device_id={device_id}, profile_name={profile_name})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for profile sync response (machine_id={machine_id}, device_id={device_id}, profile_name={profile_name})"
            ),
        }
        .into()),
    }
}

// handle_profile_show is a helper method for returning an MlxConfigProfile.
fn handle_profile_show(
    api: &Api,
    profile_name: String,
) -> Result<mlx_device::MlxAdminProfileShowResponse, Status> {
    // Check if mlxconfig profiles are configured.
    let profiles = api
        .runtime_config
        .mlxconfig_profiles
        .as_ref()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "mlxconfig_profiles",
            id: "configured".into(),
        })?;

    // Get the profile from the loaded profiles.
    let profile = profiles
        .get(&profile_name)
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "mlxconfig_profile",
            id: profile_name.clone(),
        })?;

    // Convert MlxConfigProfile to SerializableProfile, then to JSON.
    let serializable =
        SerializableProfile::from_profile(profile).map_err(|e| CarbideError::Internal {
            message: format!("failed to convert mlxconfig profile to serializable profile: {e}"),
        })?;

    let serializable_profile_pb = serializable
        .try_into()
        .map_err(|e| CarbideError::Internal {
            message: format!("failed to serialize serializable profile pb: {e}"),
        })?;

    Ok(mlx_device::MlxAdminProfileShowResponse {
        serializable_profile: Some(serializable_profile_pb),
    })
}

// handle_profile_compare is a helper method for profile compare logic.
async fn handle_profile_compare(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
    profile_name: String,
) -> Result<mlx_device::MlxAdminProfileCompareResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    // Check if mlxconfig profiles are configured.
    let profiles = api
        .runtime_config
        .mlxconfig_profiles
        .as_ref()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "mlxconfig_profiles",
            id: "configured".into(),
        })?;

    // Get the profile from the loaded profiles.
    let profile = profiles
        .get(&profile_name)
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "mlxconfig_profile",
            id: profile_name.clone(),
        })?;

    // Convert MlxConfigProfile to SerializableProfile, then to protobuf.
    let serializable =
        SerializableProfile::from_profile(profile).map_err(|e| CarbideError::Internal {
            message: format!("failed to convert mlxconfig profile to serializable profile: {e}"),
        })?;

    let serializable_profile_pb = serializable
        .try_into()
        .map_err(|e| CarbideError::Internal {
            message: format!("failed to serialize serializable profile pb: {e}"),
        })?;

    // Create the request to send to the agent.
    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceProfileCompareRequest(
            mlx_device::MlxDeviceProfileCompareRequest {
                device_id: device_id.to_string(),
                profile_name: profile_name.to_string(),
                serializable_profile: Some(serializable_profile_pb),
            },
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to compare profile via scout: {}",
                status.message()
            ),
        })?;

    // And now extract the response from the scout agent and
    // pass it back along to the administrative caller.
    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceProfileCompareResponse(
            profile_compare_response,
        )) => match profile_compare_response.reply {
            // Note: Right now this passes the protobuf-encoded ComparisonResult
            // straight on through back to the CLI without deserializing and
            // processing any of it. This could be an opportunity, if decided,
            // to deserialize and do any sort of processing, and then re-serialize.
            Some(mlx_device::mlx_device_profile_compare_response::Reply::ComparisonResult(
                comparison_result,
            )) => Ok(mlx_device::MlxAdminProfileCompareResponse {
                comparison_result: Some(comparison_result),
            }),
            Some(mlx_device::mlx_device_profile_compare_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error comparing profile to device (machine_id={machine_id}, device_id={device_id}, profile_name={profile_name}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty compare result reply (machine_id={machine_id}, device_id={device_id}, profile_name={profile_name})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for profile compare response (machine_id={machine_id}, device_id={device_id}, profile_name={profile_name})"
            ),
        }
        .into()),
    }
}

// handle_profile_list is a helper method for listing profiles.
fn handle_profile_list(api: &Api) -> Result<mlx_device::MlxAdminProfileListResponse, Status> {
    // Check if mlxconfig profiles are configured.
    let profiles = api
        .runtime_config
        .mlxconfig_profiles
        .as_ref()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "mlxconfig_profiles",
            id: "configured".into(),
        })?;

    let profile_list: Vec<mlx_device::ProfileSummary> = profiles
        .iter()
        .map(|(name, profile)| mlx_device::ProfileSummary {
            name: name.clone(),
            description: profile.description.clone(),
            registry_name: profile.registry.name.clone(),
            variable_count: profile.config_values.len() as u32,
        })
        .collect();

    Ok(mlx_device::MlxAdminProfileListResponse {
        profiles: profile_list,
    })
}

// handle_lockdown_lock is a handler for locking a device.
async fn handle_lockdown_lock(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
) -> Result<mlx_device::MlxAdminLockdownLockResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let key = get_device_lockdown_key(api, machine_id, &device_id).await?;

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceLockdownLockRequest(
            mlx_device::MlxDeviceLockdownLockRequest {
                device_id: device_id.clone(),
                key,
            },
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to lockdown::lock via scout: {}",
                status.message()
            ),
        })?;
    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(
            lockdown_response,
        )) => match lockdown_response.reply {
            Some(mlx_device::mlx_device_lockdown_response::Reply::StatusReport(status_report)) => {
                Ok(mlx_device::MlxAdminLockdownLockResponse {
                    status_report: Some(status_report),
                })
            }
            Some(mlx_device::mlx_device_lockdown_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error fetching lockdown lock status (machine_id={machine_id}, device_id={device_id}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty lockdown lock status reply (machine_id={machine_id}, device_id={device_id})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for lockdown lock status response (machine_id={machine_id}, device_id={device_id})"
            ),
        }
        .into()),
    }
}

// handle_lockdown_unlock is a handler for unlocking a device.
async fn handle_lockdown_unlock(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
) -> Result<mlx_device::MlxAdminLockdownUnlockResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let key = get_device_lockdown_key(api, machine_id, &device_id).await?;

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceLockdownUnlockRequest(
            mlx_device::MlxDeviceLockdownUnlockRequest {
                device_id: device_id.clone(),
                key,
            },
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to lockdown::unlock via scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(
            lockdown_response,
        )) => match lockdown_response.reply {
            Some(mlx_device::mlx_device_lockdown_response::Reply::StatusReport(status_report)) => {
                Ok(mlx_device::MlxAdminLockdownUnlockResponse {
                    status_report: Some(status_report),
                })
            }
            Some(mlx_device::mlx_device_lockdown_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error fetching lockdown unlock status (machine_id={machine_id}, device_id={device_id}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty lockdown unlock status reply (machine_id={machine_id}, device_id={device_id})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for lockdown unlock status response (machine_id={machine_id}, device_id={device_id})"
            ),
        }
        .into()),
    }
}

// handle_lockdown_status is a handler for getting device lockdown status.
async fn handle_lockdown_status(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
) -> Result<mlx_device::MlxAdminLockdownStatusResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceLockdownStatusRequest(
            mlx_device::MlxDeviceLockdownStatusRequest {
                device_id: device_id.clone(),
            },
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to get lockdown status via scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceLockdownResponse(
            lockdown_response,
        )) => match lockdown_response.reply {
            Some(mlx_device::mlx_device_lockdown_response::Reply::StatusReport(status_report)) => {
                Ok(mlx_device::MlxAdminLockdownStatusResponse {
                    status_report: Some(status_report),
                })
            }
            Some(mlx_device::mlx_device_lockdown_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error fetching lockdown status (machine_id={machine_id}, device_id={device_id}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty lockdown status reply (machine_id={machine_id}, device_id={device_id})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for lockdown status response (machine_id={machine_id}, device_id={device_id})"
            ),
        }
        .into()),
    }
}

// handle_show_device_info is a helper method for getting info about a specific device.
async fn handle_show_device_info(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
) -> Result<mlx_device::MlxAdminDeviceInfoResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceInfoDeviceRequest(
            mlx_device::MlxDeviceInfoDeviceRequest {
                device_id: device_id.clone(),
            },
        ),
    );

    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error requesting device info from scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceInfoDeviceResponse(
            device_info_response,
        )) => match device_info_response.reply {
            Some(mlx_device::mlx_device_info_device_response::Reply::DeviceInfo(device_info)) => {
                Ok(mlx_device::MlxAdminDeviceInfoResponse {
                    device_info: Some(device_info),
                })
            }
            Some(mlx_device::mlx_device_info_device_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error fetching device info (machine_id={machine_id}, device_id={device_id}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty device info reply (machine_id={machine_id}, device_id={device_id})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for device info response (machine_id={machine_id}, device_id={device_id})"
            ),
        }
        .into()),
    }
}

// handle_show_device_report is a helper method for getting info about all devices on a machine.
async fn handle_show_device_report(
    api: &Api,
    machine_id: MachineId,
) -> Result<mlx_device::MlxAdminDeviceReportResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceInfoReportRequest(
            mlx_device::MlxDeviceInfoReportRequest { filters: None },
        ),
    );

    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error requesting device report from scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceInfoReportResponse(
            device_report_response,
        )) => match device_report_response.reply {
            Some(mlx_device::mlx_device_info_report_response::Reply::DeviceReport(
                device_report,
            )) => Ok(mlx_device::MlxAdminDeviceReportResponse {
                device_report: Some(device_report),
            }),
            Some(mlx_device::mlx_device_info_report_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error fetching device report (machine_id={machine_id}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty device report reply (machine_id={machine_id})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for device report response (machine_id={machine_id})"
            ),
        }
        .into()),
    }
}

// handle_registry_list is a helper method for registry list operations.
async fn handle_registry_list(
    api: &Api,
    machine_id: MachineId,
) -> Result<mlx_device::MlxAdminRegistryListResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceRegistryListRequest(
            mlx_device::MlxDeviceRegistryListRequest {},
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to list registry info via scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceRegistryListResponse(
            registry_list_response,
        )) => match registry_list_response.reply {
            Some(mlx_device::mlx_device_registry_list_response::Reply::RegistryListing(
                registry_listing,
            )) => Ok(mlx_device::MlxAdminRegistryListResponse {
                registry_listing: Some(mlx_device::RegistryListing {
                    registry_names: registry_listing.registry_names,
                }),
            }),
            Some(mlx_device::mlx_device_registry_list_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error fetching registry list (machine_id={machine_id}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty registry list reply (machine_id={machine_id})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for registry list response: {machine_id}"
            ),
        }
        .into()),
    }
}

// handle_registry_show is a helper method for registry show operations.
async fn handle_registry_show(
    api: &Api,
    machine_id: MachineId,
    registry_name: String,
) -> Result<mlx_device::MlxAdminRegistryShowResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceRegistryShowRequest(
            mlx_device::MlxDeviceRegistryShowRequest { registry_name },
        ),
    );

    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error requesting registry info from scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceRegistryShowResponse(
            registry_show_response,
        )) => match registry_show_response.reply {
            Some(mlx_device::mlx_device_registry_show_response::Reply::VariableRegistry(
                variable_registry,
            )) => Ok(mlx_device::MlxAdminRegistryShowResponse {
                variable_registry: Some(variable_registry),
            }),
            Some(mlx_device::mlx_device_registry_show_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error fetching registry info (machine_id={machine_id}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty registry info reply (machine_id={machine_id})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for registry info response (machine_id={machine_id})"
            ),
        }
        .into()),
    }
}

// handle_config_query is a helper method for config query operations.
async fn handle_config_query(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
    registry_name: String,
    variables: Vec<String>,
) -> Result<mlx_device::MlxAdminConfigQueryResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigQueryRequest(
            mlx_device::MlxDeviceConfigQueryRequest {
                device_id: device_id.clone(),
                registry_name: registry_name.clone(),
                variables,
            },
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to query config via scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceConfigQueryResponse(
            query_response,
        )) => {
            match query_response.reply {
                // Note: Right now this passes the protobuf-encoded ComparisonResult
                // straight on through back to the CLI without deserializing and
                // processing any of it. This could be an opportunity, if decided,
                // to deserialize and do any sort of processing, and then re-serialize.
                Some(mlx_device::mlx_device_config_query_response::Reply::QueryResult(
                    query_result,
                )) => Ok(mlx_device::MlxAdminConfigQueryResponse {
                    query_result: Some(query_result),
                }),
                Some(mlx_device::mlx_device_config_query_response::Reply::Error(error)) => {
                    Err(CarbideError::Internal {
                        message: format!(
                            "scout agent returned error querying config on device (machine_id={machine_id}, device_id={device_id}, registry_name={registry_name}): {}",
                            error.message
                        ),
                    }
                    .into())
                }
                None => Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned empty query result reply (machine_id={machine_id}, device_id={device_id}, registry_name={registry_name})"
                    ),
                }
                .into()),
            }
        }
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for config query response (machine_id={machine_id}, device_id={device_id}, registry_name={registry_name})"
            ),
        }
        .into()),
    }
}

// handle_config_set is a helper method for config set operations.
async fn handle_config_set(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
    registry_name: String,
    assignments: Vec<mlx_device::VariableAssignment>,
) -> Result<mlx_device::MlxAdminConfigSetResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigSetRequest(
            mlx_device::MlxDeviceConfigSetRequest {
                device_id: device_id.clone(),
                registry_name: registry_name.clone(),
                assignments,
            },
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to set config via scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceConfigSetResponse(
            config_set_response,
        )) => match config_set_response.reply {
            Some(mlx_device::mlx_device_config_set_response::Reply::TotalApplied(
                total_applied,
            )) => Ok(mlx_device::MlxAdminConfigSetResponse { total_applied }),
            Some(mlx_device::mlx_device_config_set_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error setting config on device (machine_id={machine_id}, device_id={device_id}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty config set reply (machine_id={machine_id}, device_id={device_id})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for config set response (machine_id={machine_id}, device_id={device_id})"
            ),
        }
        .into()),
    }
}

// handle_config_sync is a helper method for config sync operations.
async fn handle_config_sync(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
    registry_name: String,
    assignments: Vec<mlx_device::VariableAssignment>,
) -> Result<mlx_device::MlxAdminConfigSyncResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigSyncRequest(
            mlx_device::MlxDeviceConfigSyncRequest {
                device_id: device_id.clone(),
                registry_name: registry_name.clone(),
                assignments,
            },
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to sync config via scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceConfigSyncResponse(
            config_sync_response,
        )) => {
            match config_sync_response.reply {
                // Note: Right now this passes the protobuf-encoded SyncResult
                // straight on through back to the CLI without deserializing and
                // processing any of it. This could be an opportunity, if decided,
                // to deserialize and do any sort of processing, and then re-serialize.
                Some(mlx_device::mlx_device_config_sync_response::Reply::SyncResult(
                    sync_result,
                )) => Ok(mlx_device::MlxAdminConfigSyncResponse {
                    sync_result: Some(sync_result),
                }),
                Some(mlx_device::mlx_device_config_sync_response::Reply::Error(error)) => {
                    Err(CarbideError::Internal {
                        message: format!(
                            "scout agent returned error syncing config to device (machine_id={machine_id}, device_id={device_id}): {}",
                            error.message
                        ),
                    }
                    .into())
                }
                None => Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned empty sync result reply (machine_id={machine_id}, device_id={device_id})"
                    ),
                }
                .into()),
            }
        }
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for config sync response (machine_id={machine_id}, device_id={device_id})"
            ),
        }
        .into()),
    }
}

// handle_config_compare is a helper method for config compare operations.
async fn handle_config_compare(
    api: &Api,
    machine_id: MachineId,
    device_id: String,
    registry_name: String,
    assignments: Vec<rpc::protos::mlx_device::VariableAssignment>,
) -> Result<mlx_device::MlxAdminConfigCompareResponse, Status> {
    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout_agent",
            id: format!("scout agent on machine is not connected: {machine_id}"),
        }
        .into());
    }

    let request = ScoutStreamScoutBoundMessage::new_flow(
        scout_stream_scout_bound_message::Payload::MlxDeviceConfigCompareRequest(
            rpc::protos::mlx_device::MlxDeviceConfigCompareRequest {
                device_id: device_id.clone(),
                registry_name: registry_name.clone(),
                assignments,
            },
        ),
    );

    // And now send the request off to the scout agent and wait for a response.
    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to compare config via scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(scout_stream_api_bound_message::Payload::MlxDeviceConfigCompareResponse(
            compare_response,
        )) => {
            match compare_response.reply {
                // Note: Right now this passes the protobuf-encoded ComparisonResult
                // straight on through back to the CLI without deserializing and
                // processing any of it. This could be an opportunity, if decided,
                // to deserialize and do any sort of processing, and then re-serialize.
                Some(mlx_device::mlx_device_config_compare_response::Reply::ComparisonResult(
                    comparison_result,
                )) => Ok(mlx_device::MlxAdminConfigCompareResponse {
                    comparison_result: Some(comparison_result),
                }),
                Some(mlx_device::mlx_device_config_compare_response::Reply::Error(error)) => {
                    Err(CarbideError::Internal {
                        message: format!(
                            "scout agent returned error comparing config to device (machine_id={machine_id}, device_id={device_id}, registry_name={registry_name}): {}",
                            error.message
                        ),
                    }
                    .into())
                }
                None => Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned empty compare result reply (machine_id={machine_id}, device_id={device_id}, registry_name={registry_name})"
                    ),
                }
                .into()),
            }
        }
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for config compare response (machine_id={machine_id}, device_id={device_id}, registry_name={registry_name})"
            ),
        }
        .into()),
    }
}

// get_device_lockdown_key looks up the DPA interface for the given
// machine + PCI device, then derives the lockdown key via HKDF.
//
async fn get_device_lockdown_key(
    api: &Api,
    machine_id: MachineId,
    device_id: &str,
) -> Result<String, Status> {
    // Note that, while all of the code up to this point, including the CLI,
    // and mlxconfig-* crates, refer to it as the "device_id", internally we
    // refer to as the "pci_name".
    //
    // In other words, device_id == pci_name.
    let host_machine_id = carbide_uuid::machine::HostMachineId::try_from(machine_id)
        .map_err(|error| CarbideError::InvalidArgument(error.to_string()))?;
    let dpa_interface =
        db::dpa_interface::get_for_pci_name(&api.database_connection, &host_machine_id, device_id)
            .await
            .map_err(|e| {
                CarbideError::NotFoundError {
                    kind: "dpa_interface",
                    id: format!(
                        "failed to find DPA interface for device (machine_id={machine_id}, device_id={device_id}): {e}"
                    ),
                }
            })?;

    // Derive at the version the card is tracked under: the in-flight
    // `rotating_to_version` if a lock is mid-flight, else the last-confirmed
    // `current_version`, else the seed. This admin escape hatch does not stage or
    // promote convergence, so it must not silently migrate a card: unlock uses
    // the IKM the card is actually locked under, and lock re-locks at that same
    // version rather than the (possibly advanced) site-wide target.
    let mut conn = api.database_connection.acquire().await.map_err(|e| {
        CarbideError::Internal {
            message: format!(
                "failed to acquire connection to resolve lockdown IKM version (machine_id={machine_id}, device_id={device_id}): {e}"
            ),
        }
    })?;
    // Versions are DB-native `i32` (Postgres has no unsigned int); convert to
    // `u32` once for the derivation call. No row / no tracked version falls back
    // to the seed version.
    let version = db::credential_rotation::device_rotation_operation_state(
        &mut *conn,
        db::credential_rotation::CredentialRotationType::LockdownIkm,
        dpa_interface.mac_address,
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!(
            "failed to read lockdown rotation state (machine_id={machine_id}, device_id={device_id}): {e}"
        ),
    })?
    .and_then(|s| s.rotating_to_version.or(s.current_version))
    .unwrap_or(crate::dpa::lockdown::SEED_LOCKDOWN_IKM_VERSION as i32);
    let ikm_version = u32::try_from(version).map_err(|e| CarbideError::Internal {
        message: format!(
            "lockdown IKM version {version} is negative (machine_id={machine_id}, device_id={device_id}): {e}"
        ),
    })?;
    drop(conn);

    let key = crate::dpa::lockdown::build_supernic_lockdown_key(
        &api.database_connection,
        dpa_interface.id,
        &*api.credential_manager,
        ikm_version,
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!(
            "failed to derive lockdown key (machine_id={machine_id}, device_id={device_id}): {e}"
        ),
    })?;

    Ok(key)
}
