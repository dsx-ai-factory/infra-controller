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

use std::collections::VecDeque;
use std::sync::Mutex;

use ::rpc::protos::mlx_device::{
    MlxDeviceReport as MlxDeviceReportPb, PublishMlxDeviceReportRequest,
    PublishMlxDeviceReportResponse, PublishMlxObservationReportRequest,
    PublishMlxObservationReportResponse,
};
use carbide_instrument::emit;
use carbide_uuid::machine::MachineId;
use libmlx::device::discovery;
use libmlx::device::report::MlxDeviceReport;
use libmlx::firmware::config::FirmwareFlasherProfile;
use libmlx::firmware::credentials::Credentials;
use libmlx::firmware::operation::{
    FirmwareOperation, FirmwareOperationState, FirmwareSupervisorClient,
    FirmwareSupervisorClientError, ObserverExitBehavior, SupervisorConfig, connect_or_launch,
    has_unresolved_firmware_claim,
};
use libmlx::lockdown::error::MlxResult;
use libmlx::lockdown::lockdown::{LockdownManager, StatusReport};
use libmlx::profile::error::MlxProfileError;
use libmlx::profile::serialization::SerializableProfile;
use libmlx::registry::registries;
use libmlx::runner::applier::MlxConfigApplier;
use libmlx::runner::result_types::{ComparisonResult, SyncResult};
use libmlx::runner::runner::MlxConfigRunner;
use rpc::protos::mlx_device as mlx_device_pb;
use scout::CarbideClientResult;

use crate::cfg::Options;
use crate::client;
use crate::metrics::{
    ScoutMlxConfigOperationFailed, ScoutMlxConfigRegistryLookupFailed,
    ScoutMlxDeviceOperationFailed, ScoutMlxFirmwareReceiptFailed, ScoutMlxOperationFailed,
    ScoutMlxProfileApplyFailed, ScoutMlxProfileOperationFailed, ScoutMlxProfileResetFailed,
    ScoutMlxRegistryLookupFailed, ScoutMlxRequestRejected,
};

/// Startup inventory defers to earlier firmware ownership. Its query
/// is bounded so a stalled MFT probe cannot indefinitely prevent control polling.
async fn create_device_report_request(
    machine_id: MachineId,
) -> Result<Option<PublishMlxDeviceReportRequest>, String> {
    let config = SupervisorConfig::from_env().map_err(|error| error.to_string())?;
    create_device_report_request_with_query(machine_id, &config, async {
        let mut report = MlxDeviceReport::new();
        report.devices = discovery::discover_pci_devices_bounded()
            .await
            .map_err(|error| error.to_string())?;
        Ok(report)
    })
    .await
}

/// Retry the startup inventory until Core accepts it. A firmware claim can
/// outlive Scout, so deferring the first query must not discard the publication.
pub(super) async fn publish_device_report_if_pending(
    config: &Options,
    machine_id: MachineId,
    pending: &mut bool,
) {
    publish_pending_device_report(
        pending,
        create_device_report_request(machine_id),
        |request| async {
            publish_mlx_device_report(config, request)
                .await
                .map_err(|error| format!("{error:?}"))
        },
    )
    .await;
}

/// Keep the publication pending on both hardware deferral and transport errors.
/// Once published, neither the query nor the publisher is polled again.
async fn publish_pending_device_report<PublishFuture>(
    pending: &mut bool,
    query: impl std::future::Future<Output = Result<Option<PublishMlxDeviceReportRequest>, String>>,
    publish: impl FnOnce(PublishMlxDeviceReportRequest) -> PublishFuture,
) where
    PublishFuture: std::future::Future<Output = Result<PublishMlxDeviceReportResponse, String>>,
{
    if !*pending {
        return;
    }
    let request = match query.await {
        Ok(Some(request)) => request,
        Ok(None) => return,
        Err(error) => {
            emit(ScoutMlxOperationFailed::DeviceReportCreate { error });
            return;
        }
    };
    match publish(request).await {
        Ok(response) => {
            *pending = false;
            tracing::info!(?response, "received PublishMlxDeviceReportResponse");
        }
        Err(error) => emit(ScoutMlxOperationFailed::DeviceReportPublish { error }),
    }
}

/// Check saved ownership before polling the inventory future. This sequences
/// Scout startup; it does not fence an independent operator's hardware command.
async fn create_device_report_request_with_query(
    machine_id: MachineId,
    config: &SupervisorConfig,
    query: impl std::future::Future<Output = Result<MlxDeviceReport, String>>,
) -> Result<Option<PublishMlxDeviceReportRequest>, String> {
    if has_unresolved_firmware_claim(config).map_err(|error| error.to_string())? {
        tracing::info!("deferring startup NIC inventory while firmware ownership is unresolved");
        return Ok(None);
    }
    tracing::info!("creating PublishMlxDeviceReportRequest");
    let mut report = query.await?;
    report.machine_id = Some(machine_id);
    let report_pb: MlxDeviceReportPb = report.into();
    Ok(Some(PublishMlxDeviceReportRequest {
        report: Some(report_pb),
    }))
}

// publish_mlx_device_report is used to publish an MlxDeviceReport for the current
// machine, which will collect the hardware + firmware/version details of all Mellanox
// devices on the machine, including DPUs and DPAs. This is then published to carbide-api,
// which leverages this data for ensuring devices are synced with the correct mlxconfig
// settings, and have been (or will be instructed to) updated to the correct firmware version.
//
// When called from scout on a host, a report will contain *all* Mellanox devices on the host.
// When called from the agent on a DPU, a report will contain *only* the DPU its being called from.
async fn publish_mlx_device_report(
    config: &Options,
    req: PublishMlxDeviceReportRequest,
) -> CarbideClientResult<PublishMlxDeviceReportResponse> {
    tracing::info!(request = ?req, "sending PublishMlxDeviceReportRequest");
    let request = tonic::Request::new(req);
    let mut client = client::create_forge_client(config).await?;
    let response = client
        .publish_mlx_device_report(request)
        .await?
        .into_inner();
    Ok(response)
}

pub(super) async fn publish_mlx_observation_report(
    config: &Options,
    req: PublishMlxObservationReportRequest,
) -> CarbideClientResult<PublishMlxObservationReportResponse> {
    tracing::info!(
        request = ?req,
        "sending PublishMlxObservationReportRequest",
    );
    let request = tonic::Request::new(req);
    let mut client = client::create_forge_client(config).await?;
    let response = client
        .publish_mlx_observation_report(request)
        .await?
        .into_inner();
    Ok(response)
}

// lock_device locks a device with a provided key. The device_address
// can either be a PCI address, or a /dev/mst/* path. Generally when
// going through the automation, we'll end up using whatever comes in
// via the mlx device reports, with the device info coming from
// mlxfwmanager, so if mst is running, it will probably be an mst path.
// BUT, even if mst is running, you can still provide a PCI address.
pub(super) fn lock_device(device_address: &str, key: &str) -> MlxResult<()> {
    let manager = LockdownManager::new()?;
    manager.lock_device(device_address, key)?;
    Ok(())
}

// unlock_device unlocks a device with a provided key. See above comments
// in lock_device about the device_address argument formatting options.
pub(super) fn unlock_device(device_address: &str, key: &str) -> MlxResult<()> {
    let manager = LockdownManager::new()?;
    manager.unlock_device(device_address, key)?;
    Ok(())
}

struct LoggableFirmwareSource {
    value: String,
    source_forms: Vec<String>,
}

impl LoggableFirmwareSource {
    fn new(raw: &str) -> Self {
        let Ok(parsed) = reqwest::Url::parse(raw) else {
            let scheme_length = ["http://", "https://", "ssh://"].iter().find_map(|scheme| {
                raw.get(..scheme.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
                    .then_some(scheme.len())
            });
            return Self {
                value: if scheme_length.is_some() {
                    "<invalid firmware source>".to_string()
                } else {
                    raw.to_string()
                },
                source_forms: scheme_length
                    .is_some_and(|length| raw.len() > length)
                    .then(|| raw.to_string())
                    .into_iter()
                    .collect(),
            };
        };

        if parsed.host_str().is_none() {
            return Self {
                value: raw.to_string(),
                source_forms: Vec::new(),
            };
        }

        let mut source_forms = vec![raw.to_string(), parsed.to_string()];
        for remove_fragment in [false, true] {
            for remove_query in [false, true] {
                let mut form = parsed.clone();
                if remove_fragment {
                    form.set_fragment(None);
                }
                if remove_query {
                    form.set_query(None);
                }
                source_forms.push(form.to_string());

                remove_url_credentials(&mut form);
                source_forms.push(form.to_string());
            }
        }

        let mut loggable = parsed;
        remove_url_credentials(&mut loggable);
        loggable.set_query(None);
        loggable.set_fragment(None);
        let value = loggable.to_string();

        source_forms.retain(|form| !form.is_empty() && form != &value);
        source_forms
            .sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        source_forms.dedup();

        Self {
            value,
            source_forms,
        }
    }

    fn redact_from(&self, text: &str) -> String {
        self.source_forms
            .iter()
            .fold(text.to_string(), |redacted, form| {
                redacted.replace(form, &self.value)
            })
    }

    #[cfg(test)]
    fn as_str(&self) -> &str {
        &self.value
    }
}

fn remove_url_credentials(url: &mut reqwest::Url) {
    let _ = url.set_username("");
    let _ = url.set_password(None);
}

fn firmware_error_context(error: &str, profile: &FirmwareFlasherProfile) -> String {
    let sources = std::iter::once(profile.flash_spec.firmware_url.as_str())
        .chain(profile.flash_spec.device_conf_url.as_deref());
    let redacted = sources.fold(error.to_string(), |text, source| {
        LoggableFirmwareSource::new(source).redact_from(&text)
    });

    mask_secret_values(&redacted, firmware_credentials(profile))
}

fn firmware_credentials(profile: &FirmwareFlasherProfile) -> impl Iterator<Item = &str> {
    profile
        .flash_spec
        .firmware_credentials
        .iter()
        .chain(profile.flash_spec.device_conf_credentials.iter())
        .filter_map(|credentials| match credentials {
            Credentials::BearerToken { token } => Some(token.as_str()),
            Credentials::BasicAuth {
                username: _,
                password,
            } => Some(password.as_str()),
            Credentials::Header { name: _, value } => Some(value.as_str()),
            Credentials::SshKey {
                path: _,
                passphrase,
            } => passphrase.as_deref(),
            Credentials::SshAgent {} => None,
        })
}

fn mask_secret_values<'a>(text: &str, secrets: impl Iterator<Item = &'a str>) -> String {
    // Merge touching matches before rendering them. Sequential replacement can
    // leave part of either secret behind when two credential values overlap.
    let mut ranges: Vec<(usize, usize)> = secrets
        .filter(|secret| !secret.is_empty())
        .flat_map(|secret| {
            let secret = secret.as_bytes();
            (0..=text.len().saturating_sub(secret.len()))
                .filter(move |&start| text.as_bytes()[start..].starts_with(secret))
                .map(move |start| (start, start + secret.len()))
        })
        .collect();
    ranges.sort_unstable();

    let mut redacted = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut range_index = 0;
    while range_index < ranges.len() {
        let (start, mut end) = ranges[range_index];
        range_index += 1;
        while range_index < ranges.len() && ranges[range_index].0 <= end {
            end = end.max(ranges[range_index].1);
            range_index += 1;
        }
        redacted.push_str(&text[cursor..start]);
        redacted.push_str("REDACTED");
        cursor = end;
    }
    redacted.push_str(&text[cursor..]);
    redacted
}

pub(super) fn handle_profile_sync(
    request: mlx_device_pb::MlxDeviceProfileSyncRequest,
) -> mlx_device_pb::MlxDeviceProfileSyncResponse {
    tracing::info!(
        device_id = %request.device_id,
        profile_name = %request.profile_name,
        "[scout_stream::mlx_device] profile sync to device requested",
    );

    let Some(serializable_profile_pb) = request.serializable_profile else {
        emit(ScoutMlxRequestRejected::ProfileSync {});
        return mlx_device_pb::MlxDeviceProfileSyncResponse {
            reply: Some(
                mlx_device_pb::mlx_device_profile_sync_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: "no serializable profile data in message".into(),
                    },
                ),
            ),
        };
    };

    let serializable_profile: SerializableProfile = match serializable_profile_pb.try_into() {
        Ok(profile) => profile,
        Err(e) => {
            emit(ScoutMlxOperationFailed::ProfileSyncDecode {
                error: e.to_string(),
            });
            return mlx_device_pb::MlxDeviceProfileSyncResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_profile_sync_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!("failed to parse profile: {e}"),
                        },
                    ),
                ),
            };
        }
    };

    match load_and_sync_profile(&request.device_id, serializable_profile) {
        Ok(sync_result) => {
            tracing::info!(
                device_id = %request.device_id,
                profile_name = %request.profile_name,
                "[scout_stream::mlx_device] profile sync to device successful",
            );

            match sync_result.try_into() {
                Ok(sync_result_pb) => mlx_device_pb::MlxDeviceProfileSyncResponse {
                    reply: Some(
                        mlx_device_pb::mlx_device_profile_sync_response::Reply::SyncResult(
                            sync_result_pb,
                        ),
                    ),
                },
                Err(e) => {
                    emit(ScoutMlxOperationFailed::ProfileSyncSerialize {
                        error: e.to_string(),
                    });
                    mlx_device_pb::MlxDeviceProfileSyncResponse {
                        reply: Some(
                            mlx_device_pb::mlx_device_profile_sync_response::Reply::Error(
                                mlx_device_pb::MlxDeviceStreamError {
                                    status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal
                                        .into(),
                                    message: format!("failed to serialize sync result: {e}"),
                                },
                            ),
                        ),
                    }
                }
            }
        }
        Err(e) => {
            emit(ScoutMlxProfileOperationFailed::Sync {
                device_id: request.device_id.clone(),
                profile_name: request.profile_name.clone(),
                error: e.to_string(),
            });
            mlx_device_pb::MlxDeviceProfileSyncResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_profile_sync_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!("sync to device failed: {e}"),
                        },
                    ),
                ),
            }
        }
    }
}

pub(super) fn handle_profile_compare(
    request: mlx_device_pb::MlxDeviceProfileCompareRequest,
) -> mlx_device_pb::MlxDeviceProfileCompareResponse {
    tracing::info!(
        device_id = %request.device_id,
        profile_name = %request.profile_name,
        "[scout_stream::mlx_device] profile compare against device requested",
    );

    let Some(serializable_profile_pb) = request.serializable_profile else {
        emit(ScoutMlxRequestRejected::ProfileCompare {});
        return mlx_device_pb::MlxDeviceProfileCompareResponse {
            reply: Some(
                mlx_device_pb::mlx_device_profile_compare_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: "no serializable profile data in message".into(),
                    },
                ),
            ),
        };
    };

    let serializable_profile: SerializableProfile = match serializable_profile_pb.try_into() {
        Ok(profile) => profile,
        Err(e) => {
            emit(ScoutMlxOperationFailed::ProfileCompareDecode {
                error: e.to_string(),
            });
            return mlx_device_pb::MlxDeviceProfileCompareResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_profile_compare_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!("failed to parse profile: {e}"),
                        },
                    ),
                ),
            };
        }
    };

    match load_and_compare_profile(&request.device_id, serializable_profile) {
        Ok(comparison_result) => {
            tracing::info!(
                device_id = %request.device_id,
                profile_name = %request.profile_name,
                "[scout_stream::mlx_device] profile compare against device successful",
            );

            match comparison_result.try_into() {
                Ok(comparison_result_pb) => mlx_device_pb::MlxDeviceProfileCompareResponse {
                    reply: Some(
                        mlx_device_pb::mlx_device_profile_compare_response::Reply::ComparisonResult(
                            comparison_result_pb,
                        ),
                    ),
                },
                Err(e) => {
                    emit(ScoutMlxOperationFailed::ProfileCompareSerialize {
                        error: e.to_string(),
                    });
                    mlx_device_pb::MlxDeviceProfileCompareResponse {
                        reply: Some(
                            mlx_device_pb::mlx_device_profile_compare_response::Reply::Error(
                                mlx_device_pb::MlxDeviceStreamError {
                                    status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal
                                        .into(),
                                    message: format!("failed to serialize compare result: {e}"),
                                },
                            ),
                        ),
                    }
                }
            }
        }
        Err(e) => {
            emit(ScoutMlxProfileOperationFailed::Compare {
                device_id: request.device_id.clone(),
                profile_name: request.profile_name.clone(),
                error: e.to_string(),
            });
            mlx_device_pb::MlxDeviceProfileCompareResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_profile_compare_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!("compare against device failed: {e}"),
                        },
                    ),
                ),
            }
        }
    }
}

// handle_lockdown_lock locks a device.
pub(super) fn handle_lockdown_lock(
    request: mlx_device_pb::MlxDeviceLockdownLockRequest,
) -> mlx_device_pb::MlxDeviceLockdownResponse {
    tracing::info!(
        device_id = %request.device_id,
        "[scout_stream::mlx_device] lockdown lock requested",
    );

    let manager = match LockdownManager::new() {
        Ok(m) => m,
        Err(e) => {
            emit(ScoutMlxOperationFailed::LockdownLockInitialize {
                error: e.to_string(),
            });
            return mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: e.to_string(),
                    },
                )),
            };
        }
    };

    match manager.lock_device(&request.device_id, &request.key) {
        Ok(status) => {
            tracing::info!(
                device_id = %request.device_id,
                lockdown_status = %status,
                "[scout_stream::mlx_device] lockdown lock successful",
            );
            let report = StatusReport::new(request.device_id.clone(), status);
            mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_lockdown_response::Reply::StatusReport(report.into()),
                ),
            }
        }
        Err(e) => {
            emit(ScoutMlxDeviceOperationFailed::LockdownLockExecute {
                device_id: request.device_id.clone(),
                error: e.to_string(),
            });
            mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: format!("lockdown lock failed: {e}"),
                    },
                )),
            }
        }
    }
}

// handle_lockdown_unlock unlocks a device.
pub(super) fn handle_lockdown_unlock(
    request: mlx_device_pb::MlxDeviceLockdownUnlockRequest,
) -> mlx_device_pb::MlxDeviceLockdownResponse {
    tracing::info!(
        device_id = %request.device_id,
        "[scout_stream::mlx_device] lockdown unlock requested",
    );

    let manager = match LockdownManager::new() {
        Ok(m) => m,
        Err(e) => {
            emit(ScoutMlxOperationFailed::LockdownUnlockInitialize {
                error: e.to_string(),
            });
            return mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: format!("lockdown manager init failed: {e}"),
                    },
                )),
            };
        }
    };

    match manager.unlock_device(&request.device_id, &request.key) {
        Ok(status) => {
            tracing::info!(
                device_id = %request.device_id,
                lockdown_status = %status,
                "[scout_stream::mlx_device] lockdown unlock successful",
            );
            let report = StatusReport::new(request.device_id.clone(), status);
            mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_lockdown_response::Reply::StatusReport(report.into()),
                ),
            }
        }
        Err(e) => {
            emit(ScoutMlxDeviceOperationFailed::LockdownUnlockExecute {
                device_id: request.device_id.clone(),
                error: e.to_string(),
            });
            mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: format!("lockdown unlock failed: {e}"),
                    },
                )),
            }
        }
    }
}

// handle_lockdown_status gets the lockdown status of a device.
pub(super) fn handle_lockdown_status(
    request: mlx_device_pb::MlxDeviceLockdownStatusRequest,
) -> mlx_device_pb::MlxDeviceLockdownResponse {
    tracing::info!(
        device_id = %request.device_id,
        "[scout_stream::mlx_device] lockdown status check requested",
    );

    let manager = match LockdownManager::new() {
        Ok(m) => m,
        Err(e) => {
            emit(ScoutMlxOperationFailed::LockdownStatusInitialize {
                error: e.to_string(),
            });
            return mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: format!("lockdown manager init failed: {e}"),
                    },
                )),
            };
        }
    };

    match manager.get_status(&request.device_id) {
        Ok(status) => {
            tracing::info!(
                device_id = %request.device_id,
                lockdown_status = %status,
                "[scout_stream::mlx_device] lockdown status check successful",
            );
            let report = StatusReport::new(request.device_id.clone(), status);
            mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_lockdown_response::Reply::StatusReport(report.into()),
                ),
            }
        }
        Err(e) => {
            emit(ScoutMlxDeviceOperationFailed::LockdownStatusExecute {
                device_id: request.device_id.clone(),
                error: e.to_string(),
            });
            mlx_device_pb::MlxDeviceLockdownResponse {
                reply: Some(mlx_device_pb::mlx_device_lockdown_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: e.to_string(),
                    },
                )),
            }
        }
    }
}

pub(super) fn handle_info_device(
    request: mlx_device_pb::MlxDeviceInfoDeviceRequest,
) -> mlx_device_pb::MlxDeviceInfoDeviceResponse {
    tracing::info!(
        device_id = %request.device_id,
        "[scout_stream::mlx_device] device info request",
    );

    match discovery::discover_device(&request.device_id) {
        Ok(device_info) => {
            tracing::info!(
                device_id = %request.device_id,
                "[scout_stream::mlx_device] device info retrieved successfully",
            );
            mlx_device_pb::MlxDeviceInfoDeviceResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_info_device_response::Reply::DeviceInfo(
                        device_info.into(),
                    ),
                ),
            }
        }
        Err(e) => {
            emit(ScoutMlxDeviceOperationFailed::DeviceInfoDiscover {
                device_id: request.device_id.clone(),
                error: e.clone(),
            });
            mlx_device_pb::MlxDeviceInfoDeviceResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_info_device_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: e,
                        },
                    ),
                ),
            }
        }
    }
}

pub(super) fn handle_info_report(
    request: mlx_device_pb::MlxDeviceInfoReportRequest,
) -> mlx_device_pb::MlxDeviceInfoReportResponse {
    tracing::info!("[scout_stream::mlx_device] device report requested");

    let report = if let Some(filter_set_pb) = request.filters {
        match libmlx::device::filters::DeviceFilterSet::try_from(filter_set_pb) {
            Ok(filters) => MlxDeviceReport::new().with_filter_set(filters),
            Err(e) => {
                emit(ScoutMlxOperationFailed::InfoReportDecode {
                    error: e.to_string(),
                });
                return mlx_device_pb::MlxDeviceInfoReportResponse {
                    reply: Some(
                        mlx_device_pb::mlx_device_info_report_response::Reply::Error(
                            mlx_device_pb::MlxDeviceStreamError {
                                status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                                message: format!("failed to parse filters: {e}"),
                            },
                        ),
                    ),
                };
            }
        }
    } else {
        MlxDeviceReport::new()
    };

    match report.collect() {
        Ok(report) => {
            tracing::info!(
                device_count = report.devices.len(),
                "[scout_stream::mlx_device] device report generated",
            );
            mlx_device_pb::MlxDeviceInfoReportResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_info_report_response::Reply::DeviceReport(
                        report.into(),
                    ),
                ),
            }
        }
        Err(e) => {
            emit(ScoutMlxOperationFailed::InfoReportExecute { error: e.clone() });
            mlx_device_pb::MlxDeviceInfoReportResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_info_report_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: e,
                        },
                    ),
                ),
            }
        }
    }
}

// handle_registry_list lists all available variable registries.
pub(super) fn handle_registry_list(
    _request: mlx_device_pb::MlxDeviceRegistryListRequest,
) -> mlx_device_pb::MlxDeviceRegistryListResponse {
    tracing::info!("[scout_stream::mlx_device] variable registry listing requested");

    let registry_names = registries::list().iter().map(|s| s.to_string()).collect();
    mlx_device_pb::MlxDeviceRegistryListResponse {
        reply: Some(
            mlx_device_pb::mlx_device_registry_list_response::Reply::RegistryListing(
                mlx_device_pb::RegistryListing { registry_names },
            ),
        ),
    }
}

// handle_registry_show returns a specific registry as JSON.
pub(super) fn handle_registry_show(
    request: mlx_device_pb::MlxDeviceRegistryShowRequest,
) -> mlx_device_pb::MlxDeviceRegistryShowResponse {
    tracing::info!(
        registry_name = %request.registry_name,
        "[scout_stream::mlx_device] variable registry details requested",
    );

    match registries::get(&request.registry_name) {
        Some(registry) => {
            let registry_pb = registry.clone().into();
            tracing::info!(
                registry_name = %request.registry_name,
                "[scout_stream::mlx_device] variable registry details generated",
            );
            mlx_device_pb::MlxDeviceRegistryShowResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_registry_show_response::Reply::VariableRegistry(
                        registry_pb,
                    ),
                ),
            }
        }
        None => {
            emit(ScoutMlxRegistryLookupFailed::registry_show_lookup(
                request.registry_name.clone(),
            ));
            mlx_device_pb::MlxDeviceRegistryShowResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_registry_show_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!("registry not found: {}", request.registry_name),
                        },
                    ),
                ),
            }
        }
    }
}

// handle_config_query queries one or more device variables against
// a given variable registry.
pub(super) fn handle_config_query(
    request: mlx_device_pb::MlxDeviceConfigQueryRequest,
) -> mlx_device_pb::MlxDeviceConfigQueryResponse {
    tracing::info!(
        device_id = %request.device_id,
        registry_name = %request.registry_name,
        variables = ?request.variables,
        "[scout_stream::mlx_device] config query requested",
    );

    let registry = match registries::get(&request.registry_name) {
        Some(r) => r.clone(),
        None => {
            emit(ScoutMlxConfigRegistryLookupFailed::Query {
                device_id: request.device_id.clone(),
                registry_name: request.registry_name.clone(),
            });
            return mlx_device_pb::MlxDeviceConfigQueryResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_config_query_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!(
                                "config registry not found (device_id:{}, registry_name:{})",
                                request.device_id, request.registry_name
                            ),
                        },
                    ),
                ),
            };
        }
    };

    // Now initialize a new mlxconfig runner and query for
    // either all, or some, variables, depending on what was
    // requested by the caller.
    let runner = MlxConfigRunner::new(request.device_id.clone(), registry);
    let result = if request.variables.is_empty() {
        runner.query_all()
    } else {
        runner.query(request.variables.as_slice())
    };

    match result {
        Ok(query_result) => {
            tracing::info!(
                device_id = %request.device_id,
                registry_name = %request.registry_name,
                "[scout_stream::mlx_device] config query against device successful",
            );

            match query_result.try_into() {
                Ok(query_result_pb) => mlx_device_pb::MlxDeviceConfigQueryResponse {
                    reply: Some(
                        mlx_device_pb::mlx_device_config_query_response::Reply::QueryResult(
                            query_result_pb,
                        ),
                    ),
                },
                Err(e) => {
                    emit(ScoutMlxConfigOperationFailed::QuerySerialize {
                        device_id: request.device_id.clone(),
                        registry_name: request.registry_name.clone(),
                        error: e.to_string(),
                    });
                    mlx_device_pb::MlxDeviceConfigQueryResponse {
                        reply: Some(
                            mlx_device_pb::mlx_device_config_query_response::Reply::Error(
                                mlx_device_pb::MlxDeviceStreamError {
                                    status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal
                                        .into(),
                                    message: format!(
                                        "failed to serialize query result (device_id:{}, registry_name:{}): {e}",
                                        request.device_id, request.registry_name
                                    ),
                                },
                            ),
                        ),
                    }
                }
            }
        }
        Err(e) => {
            emit(ScoutMlxConfigOperationFailed::QueryExecute {
                device_id: request.device_id.clone(),
                registry_name: request.registry_name.clone(),
                error: e.to_string(),
            });
            mlx_device_pb::MlxDeviceConfigQueryResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_config_query_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!(
                                "config query against device failed (device_id:{}, registry_name:{}): {e}",
                                request.device_id, request.registry_name
                            ),
                        },
                    ),
                ),
            }
        }
    }
}

// handle_config_set sets device variables.
pub(super) fn handle_config_set(
    request: mlx_device_pb::MlxDeviceConfigSetRequest,
) -> mlx_device_pb::MlxDeviceConfigSetResponse {
    tracing::info!(
        device_id = %request.device_id,
        registry_name = %request.registry_name,
        assignments = ?request.assignments,
        "[scout_stream::mlx_device] config set assignment requested",
    );

    let registry = match registries::get(&request.registry_name) {
        Some(r) => r.clone(),
        None => {
            emit(ScoutMlxConfigRegistryLookupFailed::Set {
                device_id: request.device_id.clone(),
                registry_name: request.registry_name.clone(),
            });
            return mlx_device_pb::MlxDeviceConfigSetResponse {
                reply: Some(mlx_device_pb::mlx_device_config_set_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: format!(
                            "config registry not found (device_id:{}, registry_name:{})",
                            request.device_id, request.registry_name
                        ),
                    },
                )),
            };
        }
    };

    let runner = MlxConfigRunner::new(request.device_id.clone(), registry);

    // Convert "assignments" from the proto to (String, String) tuples,
    // which are natively handled by a bunch of from impls that exist
    // for the runner.
    let assignments: Vec<(String, String)> = request
        .assignments
        .into_iter()
        .map(|a| (a.variable_name, a.value))
        .collect();

    let total_applied = assignments.len() as u32;

    match runner.set(assignments) {
        Ok(_) => {
            tracing::info!(
                device_id = %request.device_id,
                registry_name = %request.registry_name,
                "[scout_stream::mlx_device] config set on device successfully",
            );
            mlx_device_pb::MlxDeviceConfigSetResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_config_set_response::Reply::TotalApplied(
                        total_applied,
                    ),
                ),
            }
        }
        Err(e) => {
            emit(ScoutMlxConfigOperationFailed::SetExecute {
                device_id: request.device_id.clone(),
                registry_name: request.registry_name.clone(),
                error: e.to_string(),
            });
            mlx_device_pb::MlxDeviceConfigSetResponse {
                reply: Some(mlx_device_pb::mlx_device_config_set_response::Reply::Error(
                    mlx_device_pb::MlxDeviceStreamError {
                        status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                        message: format!(
                            "config set to device failed (device_id:{}, registry_name:{}): {e}",
                            request.device_id, request.registry_name,
                        ),
                    },
                )),
            }
        }
    }
}

// handle_config_sync syncs device variables, only changing variables
// that differ from the observed values.
pub(super) fn handle_config_sync(
    request: mlx_device_pb::MlxDeviceConfigSyncRequest,
) -> mlx_device_pb::MlxDeviceConfigSyncResponse {
    tracing::info!(
        device_id = %request.device_id,
        registry_name = %request.registry_name,
        assignments = ?request.assignments,
        "[scout_stream::mlx_device] config sync requested",
    );

    let registry = match registries::get(&request.registry_name) {
        Some(r) => r.clone(),
        None => {
            emit(ScoutMlxConfigRegistryLookupFailed::Sync {
                device_id: request.device_id.clone(),
                registry_name: request.registry_name.clone(),
            });
            return mlx_device_pb::MlxDeviceConfigSyncResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_config_sync_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!(
                                "config registry not found (device_id:{}, registry_name:{})",
                                request.device_id, request.registry_name
                            ),
                        },
                    ),
                ),
            };
        }
    };

    let runner = MlxConfigRunner::new(request.device_id.clone(), registry);

    // Convert "assignments" from the proto to (String, String) tuples,
    // which are natively handled by a bunch of from impls that exist
    // for the runner.
    let assignments: Vec<(String, String)> = request
        .assignments
        .into_iter()
        .map(|a| (a.variable_name, a.value))
        .collect();

    match runner.sync(assignments) {
        Ok(sync_result) => {
            tracing::info!(
                device_id = %request.device_id,
                registry_name = %request.registry_name,
                "[scout_stream::mlx_device] config sync to device successful",
            );

            match sync_result.try_into() {
                Ok(sync_result_pb) => mlx_device_pb::MlxDeviceConfigSyncResponse {
                    reply: Some(
                        mlx_device_pb::mlx_device_config_sync_response::Reply::SyncResult(
                            sync_result_pb,
                        ),
                    ),
                },
                Err(e) => {
                    emit(ScoutMlxConfigOperationFailed::SyncSerialize {
                        device_id: request.device_id.clone(),
                        registry_name: request.registry_name.clone(),
                        error: e.to_string(),
                    });
                    mlx_device_pb::MlxDeviceConfigSyncResponse {
                        reply: Some(
                            mlx_device_pb::mlx_device_config_sync_response::Reply::Error(
                                mlx_device_pb::MlxDeviceStreamError {
                                    status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal
                                        .into(),
                                    message: format!(
                                        "failed to serialize sync result (device_id:{}, registry_name:{}): {e}",
                                        request.device_id, request.registry_name,
                                    ),
                                },
                            ),
                        ),
                    }
                }
            }
        }
        Err(e) => {
            emit(ScoutMlxConfigOperationFailed::SyncExecute {
                device_id: request.device_id.clone(),
                registry_name: request.registry_name.clone(),
                error: e.to_string(),
            });
            mlx_device_pb::MlxDeviceConfigSyncResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_config_sync_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!(
                                "config sync to device failed (device_id:{}, registry_name:{}): {e}",
                                request.device_id, request.registry_name,
                            ),
                        },
                    ),
                ),
            }
        }
    }
}

// handle_config_compare compares requested variable assignments
// against the current assignments on the device.
pub(super) fn handle_config_compare(
    request: mlx_device_pb::MlxDeviceConfigCompareRequest,
) -> mlx_device_pb::MlxDeviceConfigCompareResponse {
    tracing::info!(
        device_id = %request.device_id,
        registry_name = %request.registry_name,
        assignments = ?request.assignments,
        "[scout_stream::mlx_device] config compare requested",
    );

    let registry = match registries::get(&request.registry_name) {
        Some(r) => r.clone(),
        None => {
            emit(ScoutMlxConfigRegistryLookupFailed::Compare {
                device_id: request.device_id.clone(),
                registry_name: request.registry_name.clone(),
            });
            return mlx_device_pb::MlxDeviceConfigCompareResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_config_compare_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!(
                                "config registry not found (device_id:{}, registry_name:{})",
                                request.device_id, request.registry_name
                            ),
                        },
                    ),
                ),
            };
        }
    };

    let runner = MlxConfigRunner::new(request.device_id.clone(), registry);

    // Convert "assignments" from the proto to (String, String) tuples,
    // which are natively handled by a bunch of from impls that exist
    // for the runner.
    let assignments: Vec<(String, String)> = request
        .assignments
        .into_iter()
        .map(|a| (a.variable_name, a.value))
        .collect();

    match runner.compare(assignments) {
        Ok(comparison_result) => {
            tracing::info!(
                device_id = %request.device_id,
                registry_name = %request.registry_name,
                "[scout_stream::mlx_device] config compare against device successful",
            );

            match comparison_result.try_into() {
                Ok(comparison_result_pb) => mlx_device_pb::MlxDeviceConfigCompareResponse {
                    reply: Some(
                        mlx_device_pb::mlx_device_config_compare_response::Reply::ComparisonResult(
                            comparison_result_pb,
                        ),
                    ),
                },
                Err(e) => {
                    emit(ScoutMlxConfigOperationFailed::CompareSerialize {
                        device_id: request.device_id.clone(),
                        registry_name: request.registry_name.clone(),
                        error: e.to_string(),
                    });
                    mlx_device_pb::MlxDeviceConfigCompareResponse {
                        reply: Some(
                            mlx_device_pb::mlx_device_config_compare_response::Reply::Error(
                                mlx_device_pb::MlxDeviceStreamError {
                                    status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal
                                        .into(),
                                    message: format!(
                                        "failed to serialize config compare result (device_id:{}, registry_name:{}): {e}",
                                        request.device_id, request.registry_name
                                    ),
                                },
                            ),
                        ),
                    }
                }
            }
        }
        Err(e) => {
            emit(ScoutMlxConfigOperationFailed::CompareExecute {
                device_id: request.device_id.clone(),
                registry_name: request.registry_name.clone(),
                error: e.to_string(),
            });
            mlx_device_pb::MlxDeviceConfigCompareResponse {
                reply: Some(
                    mlx_device_pb::mlx_device_config_compare_response::Reply::Error(
                        mlx_device_pb::MlxDeviceStreamError {
                            status: mlx_device_pb::MlxDeviceStreamErrorStatus::Internal.into(),
                            message: format!(
                                "config compare against device failed (device_id:{}, registry_name:{}): {e}",
                                request.device_id, request.registry_name
                            ),
                        },
                    ),
                ),
            }
        }
    }
}

/// `apply_firmware` reads or submits one Core state entry's durable operation.
/// Missing IDs are rejected before launching a helper. Scout never executes the
/// firmware tools itself or retries uncertain work under another ID.
/// `None` defers to a later poll when supervisor or host capacity is occupied.
pub(super) async fn apply_firmware(
    device: &str,
    operation_id: &str,
    profile: Option<&FirmwareFlasherProfile>,
    config: &SupervisorConfig,
) -> Result<Option<mlx_device_pb::FirmwareOperation>, String> {
    if operation_id.is_empty() {
        return Err("firmware operation ID is required; no work was submitted".to_string());
    }
    let mut client = connect_or_launch(device, config)
        .await
        .map_err(|error| error.to_string())?;
    let operation = match poll_firmware_operation(&mut client, operation_id, profile).await {
        Ok(operation) => operation,
        Err(FirmwareSupervisorClientError::Rpc(status))
            if status.code() == tonic::Code::ResourceExhausted =>
        {
            tracing::debug!(
                device,
                operation_id,
                "deferring firmware request until supervisor capacity is available"
            );
            return Ok(None);
        }
        Err(error) => {
            return Err(match profile {
                Some(profile) => firmware_error_context(&error.to_string(), profile),
                None => error.to_string(),
            });
        }
    };

    // Keep telemetry bounded even if cards are replaced repeatedly. Restart or
    // eviction can recount a failure; ordinary receipt polling must not.
    static FAILURES: Mutex<FirmwareFailureObservations> = Mutex::new(FirmwareFailureObservations {
        devices: VecDeque::new(),
    });
    let new_failure = FAILURES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .observe(device, &operation);
    if new_failure {
        emit(ScoutMlxFirmwareReceiptFailed::observed(device, &operation));
    }
    Ok(Some(operation.into()))
}

/// Last observed operation per recently polled device, not an operation history.
#[derive(Default)]
struct FirmwareFailureObservations {
    devices: VecDeque<(String, String, FirmwareOperationState)>,
}

impl FirmwareFailureObservations {
    /// Bound telemetry memory independently of the number of completed jobs.
    const MAX_DEVICES: usize = 128;

    /// Count a newly observed failure, but not another poll of the same receipt.
    fn observe(&mut self, device: &str, operation: &FirmwareOperation) -> bool {
        let previous = self
            .devices
            .iter()
            .position(|(observed_device, _, _)| observed_device == device)
            .and_then(|index| self.devices.remove(index));
        let changed = previous.as_ref().is_none_or(|(_, id, state)| {
            id != &operation.operation_id || *state != operation.state
        });
        if self.devices.len() == Self::MAX_DEVICES {
            self.devices.pop_front();
        }
        self.devices.push_back((
            device.to_string(),
            operation.operation_id.clone(),
            operation.state,
        ));
        changed
            && matches!(
                operation.state,
                FirmwareOperationState::Failed
                    | FirmwareOperationState::Interrupted
                    | FirmwareOperationState::ManualIntervention
            )
    }
}

/// `poll_firmware_operation` preserves the first accepted request even if Core's
/// live profile changes or disappears while the same operation is running.
async fn poll_firmware_operation(
    client: &mut FirmwareSupervisorClient,
    operation_id: &str,
    profile: Option<&FirmwareFlasherProfile>,
) -> Result<FirmwareOperation, FirmwareSupervisorClientError> {
    match client.get(operation_id).await {
        Ok(operation) => return Ok(operation),
        Err(FirmwareSupervisorClientError::Rpc(status))
            if status.code() == tonic::Code::NotFound => {}
        Err(error) => return Err(error),
    }

    // Only NOT_FOUND proves this ID has no receipt. Connection loss or another
    // lookup error must not turn into a second submission or a no-profile skip.
    client
        .submit(
            operation_id,
            profile.cloned(),
            ObserverExitBehavior::Continue,
        )
        .await
}

// apply_profile resets the device's mlxconfig parameters to factory
// defaults and then, if a profile is provided, syncs it to the
// device. We always reset [first] to ensure a clean slate, so that
// stale/unexpected settings from a previous tenancy don't leak
// through to the next tenant.
//
// Returns the profile name (if any) and whether the operation
// succeeded, for reporting back via MlxObservation.
pub(super) fn apply_profile(
    device: &str,
    profile: Option<SerializableProfile>,
) -> (Option<String>, Option<bool>) {
    // Always reset to factory defaults first.
    let applier = MlxConfigApplier::new(device);
    if let Err(e) = applier.reset_config() {
        emit(ScoutMlxProfileResetFailed::execute(
            device.to_string(),
            e.to_string(),
        ));
        return (profile.map(|p| p.name), Some(false));
    }
    tracing::info!(device = %device, "mlxconfig reset to factory defaults");

    // If a profile was provided, sync it after the reset.
    let Some(profile) = profile else {
        return (None, Some(true));
    };

    let name = profile.name.clone();
    match load_and_sync_profile(device, profile) {
        Ok(result) => {
            tracing::info!(
                device = %device,
                profile = %name,
                variables_checked = result.variables_checked,
                variables_changed = result.variables_changed,
                "mlxconfig profile synced"
            );
            (Some(name), Some(true))
        }
        Err(e) => {
            emit(ScoutMlxProfileApplyFailed::execute(
                device.to_string(),
                name.clone(),
                e.to_string(),
            ));
            (Some(name), Some(false))
        }
    }
}

// load_and_sync_profile loads a profile from data and syncs it to the device.
fn load_and_sync_profile(
    device_id: &str,
    serializable_profile: SerializableProfile,
) -> Result<SyncResult, MlxProfileError> {
    let profile = serializable_profile.into_profile()?;
    profile.sync(device_id, None)
}

// load_and_compare_profile is a helper function to load and compare a profile.
fn load_and_compare_profile(
    device_id: &str,
    serializable_profile: SerializableProfile,
) -> Result<ComparisonResult, Box<dyn std::error::Error>> {
    let profile = serializable_profile.into_profile()?;
    let comparison_result = profile.compare(device_id, None)?;
    Ok(comparison_result)
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs, capture_logs_async};
    use carbide_test_support::{Check, check_values};
    use libmlx::firmware::config::{FirmwareSpec, FlashOptions, FlashSpec};
    use libmlx::firmware::operation::FirmwareOperationPhase;

    use super::*;

    const MLX_FAILURE_METRIC: &str = "carbide_scout_mlx_failures_total";
    const DEVICE_ID: &str = "0000:01:00.0";
    const REGISTRY: &str = "missing-registry";

    /// Durable receipt polls keep the existing firmware failure series without
    /// counting the same failed operation once per Scout poll.
    #[test]
    fn firmware_receipt_transitions_preserve_failure_metrics() {
        use FirmwareOperationPhase::{Burn, Inspect};
        use FirmwareOperationState::{Canceled, Failed, Interrupted, ManualIntervention, Running};

        let mut observations = FirmwareFailureObservations::default();
        for (scenario, device, id, state, phase, expected_stage) in [
            // In-progress receipts are observations, not failed flash attempts.
            ("running", "card-a", "first", Running, Some(Inspect), None),
            // Constructor/inspection failures retain the initialize label.
            (
                "initialization failed",
                "card-a",
                "first",
                Failed,
                Some(Inspect),
                Some("initialize"),
            ),
            // Repeated polling must not turn one failure into a growing counter.
            (
                "same receipt",
                "card-a",
                "first",
                Failed,
                Some(Inspect),
                None,
            ),
            // Each card has its own observation history, even for equal IDs.
            (
                "another card",
                "card-b",
                "first",
                Failed,
                Some(Burn),
                Some("execute"),
            ),
            // A newly reported uncertain outcome is a distinct failure transition.
            (
                "reconciliation required",
                "card-a",
                "first",
                ManualIntervention,
                Some(Burn),
                Some("execute"),
            ),
            // Another generation can fail in the same state and still counts.
            (
                "new operation",
                "card-a",
                "second",
                ManualIntervention,
                Some(Burn),
                Some("execute"),
            ),
            // A requested stop at a safe boundary is not a firmware failure.
            ("canceled", "card-a", "third", Canceled, Some(Burn), None),
            // Lost admission without a phase is not evidence of flasher execution.
            (
                "no recorded phase",
                "card-a",
                "fourth",
                Interrupted,
                None,
                Some("supervisor"),
            ),
        ] {
            let operation = FirmwareOperation {
                operation_id: id.to_string(),
                state,
                phase,
                report: Default::default(),
                diagnostic: Some("firmware tool exited unsuccessfully".to_string()),
            };
            let metrics = MetricsCapture::start();
            let logs = capture_logs(|| {
                if observations.observe(device, &operation) {
                    emit(ScoutMlxFirmwareReceiptFailed::observed(device, &operation));
                }
            });
            assert_eq!(
                logs.len(),
                usize::from(expected_stage.is_some()),
                "{scenario}"
            );
            if let Some(stage) = expected_stage {
                assert_eq!(logs[0].field("failure_stage"), Some(stage), "{scenario}");
                assert_eq!(
                    logs[0].field("error"),
                    operation.diagnostic.as_deref(),
                    "{scenario}"
                );
                assert_eq!(
                    logs[0].field("operation_state"),
                    Some(format!("{state:?}").as_str()),
                    "{scenario}"
                );
                let operation_phase = phase
                    .map(|phase| format!("{phase:?}"))
                    .unwrap_or_else(|| "None".to_string());
                assert_eq!(
                    logs[0].field("operation_phase"),
                    Some(operation_phase.as_str()),
                    "{scenario}"
                );
            }
            for stage in ["initialize", "execute", "supervisor"] {
                assert_eq!(
                    metrics.counter_delta(
                        MLX_FAILURE_METRIC,
                        &[("operation", "firmware_flash"), ("failure_stage", stage)]
                    ),
                    if expected_stage == Some(stage) {
                        1.0
                    } else {
                        0.0
                    },
                    "{scenario}: {stage}"
                );
            }
        }
    }

    /// Telemetry keeps one operation per recent device and evicts old devices,
    /// so replacement cards and completed jobs cannot grow process memory forever.
    #[test]
    fn firmware_failure_observations_are_bounded_by_recent_devices() {
        let mut observations = FirmwareFailureObservations::default();
        let mut operation = FirmwareOperation {
            operation_id: "first".to_string(),
            state: FirmwareOperationState::Failed,
            phase: Some(FirmwareOperationPhase::Inspect),
            report: Default::default(),
            diagnostic: None,
        };
        for index in 0..FirmwareFailureObservations::MAX_DEVICES {
            assert!(observations.observe(&format!("card-{index}"), &operation));
        }
        // A new generation replaces the prior operation without growing the cache.
        operation.operation_id = "second".to_string();
        assert!(observations.observe("card-0", &operation));
        assert_eq!(
            observations.devices.len(),
            FirmwareFailureObservations::MAX_DEVICES
        );

        // Recent polling preserves card-0; adding a device evicts card-1 instead.
        assert!(observations.observe("new-card", &operation));
        assert_eq!(
            observations.devices.len(),
            FirmwareFailureObservations::MAX_DEVICES
        );
        assert!(!observations.observe("card-0", &operation));
        assert!(observations.observe("card-1", &operation));
    }

    #[derive(Clone, Copy)]
    enum HandlerFailure {
        MissingProfileCompare,
        MissingProfileSync,
        MissingRegistry,
        QueryMissingRegistry,
        CompareMissingRegistry,
        SetMissingRegistry,
        SyncMissingRegistry,
    }

    #[derive(Debug, PartialEq)]
    struct EventObservation {
        level: tracing::Level,
        message: String,
        event_name: String,
        operation: Option<String>,
        failure_stage: Option<String>,
        device_id: Option<String>,
        registry_name: Option<String>,
    }

    #[derive(Debug, PartialEq)]
    struct HandlerObservation {
        response_status: i32,
        response_message: String,
        events: Vec<EventObservation>,
        counter_delta: f64,
    }

    impl HandlerFailure {
        fn metric_labels(self) -> [(&'static str, &'static str); 2] {
            let operation = match self {
                Self::MissingProfileCompare => "profile_compare",
                Self::MissingProfileSync => "profile_sync",
                Self::MissingRegistry => "registry_show",
                Self::QueryMissingRegistry => "config_query",
                Self::CompareMissingRegistry => "config_compare",
                Self::SetMissingRegistry => "config_set",
                Self::SyncMissingRegistry => "config_sync",
            };
            let failure_stage = match self {
                Self::MissingProfileCompare | Self::MissingProfileSync => "validate",
                _ => "lookup",
            };
            [("operation", operation), ("failure_stage", failure_stage)]
        }

        fn invoke(self) -> mlx_device_pb::MlxDeviceStreamError {
            match self {
                Self::MissingProfileCompare => {
                    let response =
                        handle_profile_compare(mlx_device_pb::MlxDeviceProfileCompareRequest {
                            device_id: DEVICE_ID.to_string(),
                            profile_name: "default".to_string(),
                            serializable_profile: None,
                        });
                    match response.reply {
                        Some(mlx_device_pb::mlx_device_profile_compare_response::Reply::Error(
                            error,
                        )) => error,
                        _ => panic!("missing profile should return an error"),
                    }
                }
                Self::MissingProfileSync => {
                    let response =
                        handle_profile_sync(mlx_device_pb::MlxDeviceProfileSyncRequest {
                            device_id: DEVICE_ID.to_string(),
                            profile_name: "default".to_string(),
                            serializable_profile: None,
                        });
                    match response.reply {
                        Some(mlx_device_pb::mlx_device_profile_sync_response::Reply::Error(
                            error,
                        )) => error,
                        _ => panic!("missing sync profile should return an error"),
                    }
                }
                Self::MissingRegistry => {
                    let response =
                        handle_registry_show(mlx_device_pb::MlxDeviceRegistryShowRequest {
                            registry_name: REGISTRY.to_string(),
                        });
                    match response.reply {
                        Some(mlx_device_pb::mlx_device_registry_show_response::Reply::Error(
                            error,
                        )) => error,
                        _ => panic!("missing registry should return an error"),
                    }
                }
                Self::QueryMissingRegistry => {
                    let response =
                        handle_config_query(mlx_device_pb::MlxDeviceConfigQueryRequest {
                            device_id: DEVICE_ID.to_string(),
                            registry_name: REGISTRY.to_string(),
                            variables: Vec::new(),
                        });
                    match response.reply {
                        Some(mlx_device_pb::mlx_device_config_query_response::Reply::Error(
                            error,
                        )) => error,
                        _ => panic!("missing query registry should return an error"),
                    }
                }
                Self::CompareMissingRegistry => {
                    let response =
                        handle_config_compare(mlx_device_pb::MlxDeviceConfigCompareRequest {
                            device_id: DEVICE_ID.to_string(),
                            registry_name: REGISTRY.to_string(),
                            assignments: Vec::new(),
                        });
                    match response.reply {
                        Some(mlx_device_pb::mlx_device_config_compare_response::Reply::Error(
                            error,
                        )) => error,
                        _ => panic!("missing compare registry should return an error"),
                    }
                }
                Self::SetMissingRegistry => {
                    let response = handle_config_set(mlx_device_pb::MlxDeviceConfigSetRequest {
                        device_id: DEVICE_ID.to_string(),
                        registry_name: REGISTRY.to_string(),
                        assignments: Vec::new(),
                    });
                    match response.reply {
                        Some(mlx_device_pb::mlx_device_config_set_response::Reply::Error(
                            error,
                        )) => error,
                        _ => panic!("missing set registry should return an error"),
                    }
                }
                Self::SyncMissingRegistry => {
                    let response = handle_config_sync(mlx_device_pb::MlxDeviceConfigSyncRequest {
                        device_id: DEVICE_ID.to_string(),
                        registry_name: REGISTRY.to_string(),
                        assignments: Vec::new(),
                    });
                    match response.reply {
                        Some(mlx_device_pb::mlx_device_config_sync_response::Reply::Error(
                            error,
                        )) => error,
                        _ => panic!("missing sync registry should return an error"),
                    }
                }
            }
        }
    }

    #[test]
    fn mlx_handlers_preserve_responses_and_classify_failures() {
        let internal = mlx_device_pb::MlxDeviceStreamErrorStatus::Internal as i32;
        let missing_config = || {
            format!("config registry not found (device_id:{DEVICE_ID}, registry_name:{REGISTRY})")
        };

        check_values(
            [
                Check {
                    scenario: "profile comparison requires profile data",
                    input: HandlerFailure::MissingProfileCompare,
                    expect: HandlerObservation {
                        response_status: internal,
                        response_message: "no serializable profile data in message".to_string(),
                        events: Vec::new(),
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "profile synchronization requires profile data",
                    input: HandlerFailure::MissingProfileSync,
                    expect: HandlerObservation {
                        response_status: internal,
                        response_message: "no serializable profile data in message".to_string(),
                        events: Vec::new(),
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "registry show rejects an unknown registry",
                    input: HandlerFailure::MissingRegistry,
                    expect: HandlerObservation {
                        response_status: internal,
                        response_message: format!("registry not found: {REGISTRY}"),
                        events: vec![EventObservation {
                            level: tracing::Level::ERROR,
                            message: "[scout_stream::mlx_device] variable registry not found"
                                .to_string(),
                            event_name: "scout_mlx_registry_lookup_failed".to_string(),
                            operation: Some("registry_show".to_string()),
                            failure_stage: Some("lookup".to_string()),
                            device_id: None,
                            registry_name: Some(REGISTRY.to_string()),
                        }],
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "config query rejects an unknown registry",
                    input: HandlerFailure::QueryMissingRegistry,
                    expect: HandlerObservation {
                        response_status: internal,
                        response_message: missing_config(),
                        events: vec![EventObservation {
                            level: tracing::Level::WARN,
                            message: "[scout_stream::mlx_device] config registry not found"
                                .to_string(),
                            event_name: "scout_mlx_config_registry_lookup_failed".to_string(),
                            operation: Some("config_query".to_string()),
                            failure_stage: Some("lookup".to_string()),
                            device_id: Some(DEVICE_ID.to_string()),
                            registry_name: Some(REGISTRY.to_string()),
                        }],
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "config comparison rejects an unknown registry",
                    input: HandlerFailure::CompareMissingRegistry,
                    expect: HandlerObservation {
                        response_status: internal,
                        response_message: missing_config(),
                        events: vec![EventObservation {
                            level: tracing::Level::WARN,
                            message: "[scout_stream::mlx_device] config registry not found"
                                .to_string(),
                            event_name: "scout_mlx_config_registry_lookup_failed".to_string(),
                            operation: Some("config_compare".to_string()),
                            failure_stage: Some("lookup".to_string()),
                            device_id: Some(DEVICE_ID.to_string()),
                            registry_name: Some(REGISTRY.to_string()),
                        }],
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "config set rejects an unknown registry",
                    input: HandlerFailure::SetMissingRegistry,
                    expect: HandlerObservation {
                        response_status: internal,
                        response_message: missing_config(),
                        events: vec![EventObservation {
                            level: tracing::Level::WARN,
                            message: "[scout_stream::mlx_device] config registry not found"
                                .to_string(),
                            event_name: "scout_mlx_config_registry_lookup_failed".to_string(),
                            operation: Some("config_set".to_string()),
                            failure_stage: Some("lookup".to_string()),
                            device_id: Some(DEVICE_ID.to_string()),
                            registry_name: Some(REGISTRY.to_string()),
                        }],
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "config synchronization rejects an unknown registry",
                    input: HandlerFailure::SyncMissingRegistry,
                    expect: HandlerObservation {
                        response_status: internal,
                        response_message: missing_config(),
                        events: vec![EventObservation {
                            level: tracing::Level::WARN,
                            message: "[scout_stream::mlx_device] config registry not found"
                                .to_string(),
                            event_name: "scout_mlx_config_registry_lookup_failed".to_string(),
                            operation: Some("config_sync".to_string()),
                            failure_stage: Some("lookup".to_string()),
                            device_id: Some(DEVICE_ID.to_string()),
                            registry_name: Some(REGISTRY.to_string()),
                        }],
                        counter_delta: 1.0,
                    },
                },
            ],
            |fixture| {
                let metrics = MetricsCapture::start();
                let mut response = mlx_device_pb::MlxDeviceStreamError {
                    status: 0,
                    message: String::new(),
                };
                let logs = capture_logs(|| response = fixture.invoke());
                let events = logs
                    .iter()
                    .filter_map(|log| {
                        let event_name = log.field("event_name")?;
                        Some(EventObservation {
                            level: log.level,
                            message: log.message.clone(),
                            event_name: event_name.to_string(),
                            operation: log.field("operation").map(str::to_string),
                            failure_stage: log.field("failure_stage").map(str::to_string),
                            device_id: log.field("device_id").map(str::to_string),
                            registry_name: log.field("registry_name").map(str::to_string),
                        })
                    })
                    .collect();

                HandlerObservation {
                    response_status: response.status,
                    response_message: response.message,
                    events,
                    counter_delta: metrics
                        .counter_delta(MLX_FAILURE_METRIC, &fixture.metric_labels()),
                }
            },
        );
    }

    fn firmware_profile(
        firmware_url: &str,
        firmware_credentials: Option<Credentials>,
        device_conf_url: Option<&str>,
        device_conf_credentials: Option<Credentials>,
    ) -> FirmwareFlasherProfile {
        FirmwareFlasherProfile {
            firmware_spec: FirmwareSpec {
                part_number: "part-number".to_string(),
                psid: "psid".to_string(),
                version: "1.2.3".to_string(),
            },
            flash_spec: FlashSpec {
                firmware_url: firmware_url.to_string(),
                firmware_credentials,
                device_conf_url: device_conf_url.map(str::to_string),
                device_conf_credentials,
                verify_from_cache: false,
                cache_dir: None,
            },
            flash_options: FlashOptions::default(),
        }
    }

    /// Keep fixture state private and allow a trusted short root for Unix sockets.
    fn firmware_state_directory() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
            Some(root) => tempfile::tempdir_in(root),
            None => tempfile::tempdir(),
        }
        .unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    /// An empty internal operation ID still rejects before helper launch or
    /// device access; legacy wire commands are rejected during conversion.
    #[tokio::test]
    async fn firmware_without_operation_id_does_not_dispatch() {
        let error = apply_firmware("not-a-device", "", None, &SupervisorConfig::default())
            .await
            .expect_err("missing operation identity must reject");
        assert_eq!(
            error,
            "firmware operation ID is required; no work was submitted"
        );
    }

    /// Startup and automatic non-firmware commands must not touch hardware when
    /// a prior operation still owns the host, including unreadable ownership.
    #[tokio::test]
    async fn saved_firmware_ownership_defers_automatic_hardware_work() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        use carbide_host_support::dpa_cmds::{DpaCommand, OpCode};

        let machine_id = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
            .parse()
            .unwrap();
        for (scenario, mode, unsafe_claim) in [
            // An incomplete marker still represents ownership after process loss.
            ("saved firmware ownership", 0o600, false),
            // Unsafe existing state cannot be interpreted as no active work.
            ("unsafe saved ownership", 0o644, true),
        ] {
            let directory = firmware_state_directory();
            let claim = directory.path().join("mutation.json");
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(mode)
                .open(&claim)
                .unwrap();
            std::fs::set_permissions(&claim, std::fs::Permissions::from_mode(mode)).unwrap();
            let config = SupervisorConfig {
                state_directory: directory.path().to_path_buf(),
                ..Default::default()
            };
            let inventory = create_device_report_request_with_query(machine_id, &config, async {
                panic!("startup inventory must not probe unresolved firmware work")
            })
            .await;
            if unsafe_claim {
                assert!(inventory.is_err(), "{scenario}");
            } else {
                assert!(inventory.unwrap().is_none(), "{scenario}");
            }

            let commands = || {
                vec![
                    // Profile apply includes mlxconfig reset even without a profile.
                    (
                        DEVICE_ID.into(),
                        DpaCommand {
                            op: OpCode::ApplyProfile {
                                serialized_profile: None,
                            },
                        },
                    ),
                    (
                        DEVICE_ID.into(),
                        DpaCommand {
                            op: OpCode::Lock {
                                key: "fixture-key".into(),
                            },
                        },
                    ),
                    (
                        DEVICE_ID.into(),
                        DpaCommand {
                            op: OpCode::Unlock {
                                key: "fixture-key".into(),
                            },
                        },
                    ),
                ]
            };
            let metrics = MetricsCapture::start();
            crate::collect_and_publish_mlx_observations(
                &machine_id,
                commands(),
                &config,
                |_| panic!("deferred automatic commands must not even discover the card"),
                |_| async { panic!("deferred automatic commands must not publish a report") },
            )
            .await;
            assert_eq!(
                metrics.counter_delta(
                    MLX_FAILURE_METRIC,
                    &[
                        ("operation", "observation_report"),
                        ("failure_stage", "publish")
                    ],
                ),
                0.0,
                "{scenario}",
            );

            // Once ownership is released, a later poll reaches discovery again.
            // Stop there in this fixture; no test path invokes real vendor tools.
            std::fs::remove_file(claim).unwrap();
            let mut queries = 0;
            crate::collect_mlx_observations(&machine_id, commands(), &config, |_| {
                queries += 1;
                Err("fixture ends before mutation".into())
            })
            .await;
            assert_eq!(queries, 3, "{scenario}");
            let inventory = create_device_report_request_with_query(machine_id, &config, async {
                Ok(MlxDeviceReport::new())
            })
            .await
            .unwrap()
            .unwrap();
            assert_eq!(inventory.report.unwrap().machine_id, Some(machine_id));
        }
    }

    /// Startup inventory remains pending through query and publish failures.
    /// Each retry reads fresh device facts until Core accepts the report.
    #[tokio::test]
    async fn startup_device_report_retries_until_published() {
        use std::os::unix::fs::OpenOptionsExt;

        let metrics = MetricsCapture::start();
        let directory = firmware_state_directory();
        let claim = directory.path().join("mutation.json");
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&claim)
            .unwrap();
        let config = SupervisorConfig {
            state_directory: directory.path().to_path_buf(),
            ..Default::default()
        };
        let machine_id = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
            .parse()
            .unwrap();
        let mut pending = true;

        // Restarting Scout while a helper owns the host must leave this report
        // pending without probing hardware or submitting an empty replacement.
        publish_pending_device_report(
            &mut pending,
            create_device_report_request_with_query(machine_id, &config, async {
                panic!("inventory must defer while firmware owns hardware")
            }),
            |_| async { panic!("a deferred inventory must not be published") },
        )
        .await;
        assert!(pending);

        std::fs::remove_file(claim).unwrap();
        let mut queries = 0;
        publish_pending_device_report(
            &mut pending,
            create_device_report_request_with_query(machine_id, &config, async {
                queries += 1;
                Err("fixture inventory query failed".to_string())
            }),
            |_| async { panic!("a failed inventory query must not publish a report") },
        )
        .await;
        assert!(pending);
        assert_eq!(queries, 1);
        assert_eq!(
            metrics.counter_delta(
                MLX_FAILURE_METRIC,
                &[
                    ("operation", "device_report_publish"),
                    ("failure_stage", "create")
                ],
            ),
            1.0,
        );

        let mut inventory = MlxDeviceReport::new();
        inventory.devices = discovery::parse_mlxfwmanager_xml(
            r#"<Devices><Device pciName="0000:03:00.0" type="BlueField3"/></Devices>"#,
        )
        .unwrap();
        let mut requests = Vec::new();
        let mut expected_requests = Vec::new();
        for (version, publish_result, expected_pending) in [
            // A transient API failure must not consume the one-time report.
            (
                "32.43.1014",
                Err("Core is temporarily unavailable".to_string()),
                true,
            ),
            // Firmware can finish between polls. The next request must use
            // the new query, not replay the report Core failed to receive.
            (
                "32.44.1036",
                Ok(PublishMlxDeviceReportResponse::default()),
                false,
            ),
        ] {
            inventory.devices[0].fw_version_current = Some(version.to_string());
            let mut expected_inventory = inventory.clone();
            expected_inventory.machine_id = Some(machine_id);
            expected_requests.push(PublishMlxDeviceReportRequest {
                report: Some(expected_inventory.into()),
            });
            publish_pending_device_report(
                &mut pending,
                create_device_report_request_with_query(machine_id, &config, async {
                    queries += 1;
                    Ok(inventory.clone())
                }),
                |request| {
                    requests.push(request);
                    std::future::ready(publish_result)
                },
            )
            .await;
            assert_eq!(pending, expected_pending);
        }
        assert_eq!(queries, 3);
        assert_eq!(requests, expected_requests);

        // The service keeps polling, but its successful startup publication is
        // not repeated and no further inventory query is started.
        publish_pending_device_report(
            &mut pending,
            async { panic!("an already published inventory must not be queried again") },
            |_| async { panic!("an already published inventory must not be republished") },
        )
        .await;
        assert!(!pending);
        assert_eq!(requests.len(), 2);
    }

    /// Scout reads existing receipts before considering live profile changes,
    /// and only a definite missing receipt permits submission.
    #[tokio::test]
    async fn firmware_polls_preserve_accepted_work() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        use libmlx::firmware::operation::FirmwareOperationState;
        use rpc::protos::firmware_supervisor::firmware_supervisor_server::{
            FirmwareSupervisor, FirmwareSupervisorServer,
        };
        use rpc::protos::firmware_supervisor::{
            CancelFirmwareOperationRequest, CancelFirmwareOperationResponse,
            GetFirmwareOperationRequest, GetFirmwareOperationResponse, GetSupervisorStatusRequest,
            GetSupervisorStatusResponse, SubmitFirmwareOperationRequest,
            SubmitFirmwareOperationResponse,
        };
        use tokio_stream::wrappers::UnixListenerStream;
        use tokio_util::sync::CancellationToken;
        use tonic::{Request, Response, Status};

        /// The fake implements the real UDS protocol, recording Scout's calls
        /// without starting a helper or invoking any hardware tool.
        struct Supervisor {
            receipt: Mutex<Result<Option<mlx_device_pb::FirmwareOperation>, Status>>,
            admission: Result<mlx_device_pb::FirmwareOperation, Status>,
            retained_on_submit: Option<mlx_device_pb::FirmwareOperation>,
            submissions: Arc<Mutex<Vec<SubmitFirmwareOperationRequest>>>,
            lookups: Arc<AtomicUsize>,
            operation_id: String,
            boot_id: String,
        }

        #[tonic::async_trait]
        impl FirmwareSupervisor for Supervisor {
            async fn submit_firmware_operation(
                &self,
                request: Request<SubmitFirmwareOperationRequest>,
            ) -> Result<Response<SubmitFirmwareOperationResponse>, Status> {
                self.submissions.lock().unwrap().push(request.into_inner());
                if let Some(operation) = &self.retained_on_submit {
                    *self.receipt.lock().unwrap() = Ok(Some(operation.clone()));
                }
                self.admission.clone().map(|operation| {
                    Response::new(SubmitFirmwareOperationResponse {
                        operation: Some(operation),
                    })
                })
            }

            async fn get_firmware_operation(
                &self,
                request: Request<GetFirmwareOperationRequest>,
            ) -> Result<Response<GetFirmwareOperationResponse>, Status> {
                assert_eq!(request.get_ref().operation_id, self.operation_id);
                self.lookups.fetch_add(1, Ordering::SeqCst);
                self.receipt
                    .lock()
                    .unwrap()
                    .clone()
                    .map(|operation| Response::new(GetFirmwareOperationResponse { operation }))
            }

            async fn cancel_firmware_operation(
                &self,
                _: Request<CancelFirmwareOperationRequest>,
            ) -> Result<Response<CancelFirmwareOperationResponse>, Status> {
                Err(Status::unimplemented(
                    "polling does not cancel firmware work",
                ))
            }

            async fn get_supervisor_status(
                &self,
                _: Request<GetSupervisorStatusRequest>,
            ) -> Result<Response<GetSupervisorStatusResponse>, Status> {
                Ok(Response::new(GetSupervisorStatusResponse {
                    protocol_version: 1,
                    device_identity: "0000:03:00".to_string(),
                    boot_id: self.boot_id.clone(),
                    execution_device: "0000:03:00.0".to_string(),
                }))
            }
        }

        #[derive(Clone, Copy)]
        enum Expected {
            Receipt,
            Deferred,
            RequestFailure,
            RecoveredReceipt,
        }

        struct Case {
            name: &'static str,
            receipt: Result<Option<mlx_device_pb::FirmwareOperation>, Status>,
            profile: Option<FirmwareFlasherProfile>,
            admission_error: Option<Status>,
            submits: bool,
            expected: Expected,
        }

        let receipt: mlx_device_pb::FirmwareOperation = FirmwareOperation {
            operation_id: "state-entry-1".to_string(),
            state: FirmwareOperationState::ManualIntervention,
            phase: None,
            report: Default::default(),
            diagnostic: Some("burn completion is unknown".to_string()),
        }
        .into();
        let changed_profile = firmware_profile("/new-firmware.bin", None, None, None);
        let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
            .to_string();
        for (case_index, case) in [
            // Losing a live profile must not erase an unresolved burn receipt.
            Case {
                name: "existing work survives a missing profile",
                receipt: Ok(Some(receipt.clone())),
                profile: None,
                admission_error: None,
                submits: false,
                expected: Expected::Receipt,
            },
            // A catalog edit is not permission to replace this state's request.
            Case {
                name: "existing work ignores a changed live profile",
                receipt: Ok(Some(receipt.clone())),
                profile: Some(changed_profile.clone()),
                admission_error: None,
                submits: false,
                expected: Expected::Receipt,
            },
            // New work is submitted once with the caller's original state ID.
            Case {
                name: "missing receipt submits the profile",
                receipt: Err(Status::not_found("unknown operation")),
                profile: Some(changed_profile.clone()),
                admission_error: None,
                submits: true,
                expected: Expected::Receipt,
            },
            // A no-work decision still goes through durable admission.
            Case {
                name: "missing receipt submits the no-profile decision",
                receipt: Err(Status::not_found("unknown operation")),
                profile: None,
                admission_error: None,
                submits: true,
                expected: Expected::Receipt,
            },
            // Another card's operation defers this request without a failed
            // firmware observation or a failed-supervisor metric on each poll.
            Case {
                name: "host capacity defers submission",
                receipt: Err(Status::not_found("unknown operation")),
                profile: None,
                admission_error: Some(Status::resource_exhausted("host is owned")),
                submits: true,
                expected: Expected::Deferred,
            },
            // An inconclusive read cannot authorize resubmission or a skip.
            Case {
                name: "failed lookup does not submit",
                receipt: Err(Status::unavailable("journal unavailable")),
                profile: None,
                admission_error: None,
                submits: false,
                expected: Expected::RequestFailure,
            },
            // Transport success without a receipt is still inconclusive.
            Case {
                name: "malformed successful lookup does not submit",
                receipt: Ok(None),
                profile: None,
                admission_error: None,
                submits: false,
                expected: Expected::RequestFailure,
            },
            // The helper saved the operation before its Submit reply was lost.
            // Later polls must recover that receipt and count its failure once.
            Case {
                name: "lost submit response is recovered without resubmission",
                receipt: Err(Status::not_found("unknown operation")),
                profile: Some(firmware_profile("/original-firmware.bin", None, None, None)),
                admission_error: Some(Status::unavailable("submission response was lost")),
                submits: true,
                expected: Expected::RecoveredReceipt,
            },
        ]
        .into_iter()
        .enumerate()
        {
            let recovering = matches!(case.expected, Expected::RecoveredReceipt);
            // Each case gets a separate ID in Scout's process-wide metric cache.
            let operation_id = format!("{}-{case_index}", module_path!());
            let mut receipt = receipt.clone();
            receipt.operation_id = operation_id.clone();
            if recovering {
                receipt.state = mlx_device_pb::FirmwareOperationState::Failed.into();
                receipt.phase = Some(mlx_device_pb::FirmwareOperationPhase::Burn.into());
            }
            // Keep the Unix socket within its path limit even when the build's
            // temporary directory has a long absolute name.
            let directory = firmware_state_directory();
            let card_directory = directory.path().join("0000:03:00");
            std::fs::create_dir(&card_directory).unwrap();
            std::fs::set_permissions(&card_directory, std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let socket = card_directory.join("control.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
            let config = SupervisorConfig {
                state_directory: directory.path().to_path_buf(),
                supervisor_path: Some(directory.path().join("must-not-launch")),
                ..Default::default()
            };
            // The dispatcher must still poll firmware receipts while ordinary
            // hardware commands defer behind a claim left by admitted work.
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(directory.path().join("mutation.json"))
                .unwrap();
            let submissions = Arc::new(Mutex::new(Vec::new()));
            let lookups = Arc::new(AtomicUsize::new(0));
            let supervisor = Supervisor {
                receipt: Mutex::new(case.receipt.map(|operation| {
                    operation.map(|mut operation| {
                        operation.operation_id = operation_id.clone();
                        operation
                    })
                })),
                admission: case
                    .admission_error
                    .map_or_else(|| Ok(receipt.clone()), Err),
                retained_on_submit: recovering.then(|| receipt.clone()),
                submissions: submissions.clone(),
                lookups: lookups.clone(),
                operation_id: operation_id.clone(),
                boot_id: boot_id.clone(),
            };
            let stop = CancellationToken::new();
            let server = tokio::spawn(
                tonic::transport::Server::builder()
                    .add_service(FirmwareSupervisorServer::new(supervisor))
                    .serve_with_incoming_shutdown(
                        UnixListenerStream::new(listener),
                        stop.clone().cancelled_owned(),
                    ),
            );

            // Exercise the production trust checks during fixture setup so a
            // rejected directory/socket reports its actual launcher error.
            let client = connect_or_launch("0000:03:00.0", &config)
                .await
                .unwrap_or_else(|error| panic!("supervisor fixture failed trust checks: {error}"));
            drop(client);

            // Use the actual dispatcher and discovery's abbreviated address.
            // The fake protocol server never queries or mutates real hardware.
            let devices = discovery::parse_mlxfwmanager_xml(
                r#"<Devices><Device pciName="0000:03:00.0" type="BlueField3"/></Devices>"#,
            )
            .unwrap();
            assert_eq!(devices[0].pci_name, "03:00.0");
            let machine_id = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
                .parse()
                .unwrap();
            let metrics = MetricsCapture::start();
            let mut published = Vec::new();
            let mut logs = Vec::new();
            for poll in 0..if recovering { 3 } else { 1 } {
                let profile = if poll == 0 {
                    case.profile.clone()
                } else {
                    Some(changed_profile.clone())
                };
                let published_before = published.len();
                let ((), poll_logs) =
                    capture_logs_async(crate::collect_and_publish_mlx_observations(
                        &machine_id,
                        vec![
                            (
                                devices[0].pci_name.clone(),
                                carbide_host_support::dpa_cmds::DpaCommand {
                                    op: carbide_host_support::dpa_cmds::OpCode::ApplyFirmware {
                                        profile: profile.map(|profile| {
                                            Box::new(std::borrow::Cow::Owned(profile))
                                        }),
                                        operation_id: operation_id.clone(),
                                    },
                                },
                            ),
                            (
                                devices[0].pci_name.clone(),
                                carbide_host_support::dpa_cmds::DpaCommand {
                                    op: carbide_host_support::dpa_cmds::OpCode::ApplyProfile {
                                        serialized_profile: None,
                                    },
                                },
                            ),
                        ],
                        &config,
                        |_| panic!("a firmware claim must prevent automatic hardware discovery"),
                        |request| {
                            // Match Core's rejection so publishing a deferred
                            // report would also increment the failure counter.
                            let response = if request
                                .report
                                .as_ref()
                                .is_some_and(|report| !report.observations.is_empty())
                            {
                                Ok(PublishMlxObservationReportResponse::default())
                            } else {
                                Err(scout::CarbideClientError::GenericError(
                                    "fixture Core rejected an empty observation report".into(),
                                ))
                            };
                            published.push(request);
                            std::future::ready(response)
                        },
                    ))
                    .await;
                logs.extend(poll_logs);
                let expect_receipt =
                    matches!(case.expected, Expected::Receipt) || (recovering && poll > 0);
                assert_eq!(
                    published.len(),
                    published_before + usize::from(expect_receipt),
                    "{}: poll {poll}",
                    case.name,
                );
                if expect_receipt {
                    let observed = published.last().unwrap().report.as_ref().unwrap();
                    assert_eq!(observed.machine_id, Some(machine_id));
                    assert!(observed.timestamp.is_some());
                    assert_eq!(
                        observed.observations,
                        vec![mlx_device_pb::MlxObservation {
                            firmware_report: receipt.report.clone(),
                            firmware_operation: Some(receipt.clone()),
                            ..Default::default()
                        }],
                        "{}: poll {poll}",
                        case.name,
                    );
                }
                assert_eq!(lookups.load(Ordering::SeqCst), poll + 1, "{}", case.name);
                assert_eq!(
                    submissions.lock().unwrap().len(),
                    usize::from(case.submits),
                    "{}: poll {poll}",
                    case.name,
                );
                if recovering {
                    assert_eq!(
                        metrics.counter_delta(
                            MLX_FAILURE_METRIC,
                            &[
                                ("operation", "firmware_flash"),
                                ("failure_stage", "execute"),
                            ],
                        ),
                        if poll > 0 { 1.0 } else { 0.0 },
                        "{}: poll {poll}",
                        case.name,
                    );
                }
            }
            let request_failed = matches!(
                case.expected,
                Expected::RequestFailure | Expected::RecoveredReceipt
            );
            assert_eq!(
                logs.iter()
                    .filter(|log| {
                        log.field("event_name")
                            == Some("scout_mlx_firmware_supervisor_request_failed")
                    })
                    .count(),
                usize::from(request_failed),
                "{}",
                case.name,
            );
            assert_eq!(
                metrics.counter_delta(
                    MLX_FAILURE_METRIC,
                    &[
                        ("operation", "observation_report"),
                        ("failure_stage", "publish")
                    ],
                ),
                0.0,
                "{}",
                case.name,
            );
            if recovering {
                assert_eq!(
                    logs.iter()
                        .filter(|log| {
                            log.field("event_name") == Some("scout_mlx_firmware_receipt_failed")
                        })
                        .count(),
                    1,
                    "{}",
                    case.name,
                );
            }
            if !matches!(case.expected, Expected::Receipt) {
                assert_eq!(
                    metrics.counter_delta(
                        MLX_FAILURE_METRIC,
                        &[
                            ("operation", "firmware_flash"),
                            ("failure_stage", "supervisor")
                        ],
                    ),
                    if request_failed { 1.0 } else { 0.0 },
                    "{}",
                    case.name,
                );
            }
            drop(metrics);
            stop.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(5), server)
                .await
                .expect("supervisor fixture must stop after its client disconnects")
                .unwrap()
                .unwrap();

            let submitted = submissions.lock().unwrap();
            if case.submits {
                assert_eq!(
                    *submitted,
                    [SubmitFirmwareOperationRequest {
                        operation_id,
                        profile: case.profile.map(Into::into),
                        observer_exit_behavior: 0,
                        expected_identity: Some(
                            rpc::protos::firmware_supervisor::SupervisorIdentity {
                                protocol_version: 1,
                                device_identity: "0000:03:00".to_string(),
                                boot_id: boot_id.clone(),
                                execution_device: "0000:03:00.0".to_string(),
                            },
                        ),
                    }],
                    "{}",
                    case.name
                );
            } else {
                assert!(submitted.is_empty(), "{}", case.name);
            }
        }
    }

    #[test]
    fn firmware_context_excludes_urls_and_credentials() {
        const FIRMWARE_URL: &str =
            "https://user:userinfo-secret@firmware.example/fw.bin?token=url-secret#download";
        const DEVICE_CONF_URL: &str = "https://config.example/device.ini?token=config-secret#apply";
        const BEARER: &str = "bearer-secret";
        const PASSWORD: &str = "basic-password";

        let profile = firmware_profile(
            FIRMWARE_URL,
            Some(Credentials::bearer_token(BEARER)),
            Some(DEVICE_CONF_URL),
            Some(Credentials::basic_auth("user", PASSWORD)),
        );
        let error = format!(
            "firmware={FIRMWARE_URL}; config={DEVICE_CONF_URL}; bearer={BEARER}; password={PASSWORD}"
        );
        let context = firmware_error_context(&error, &profile);

        assert_eq!(
            LoggableFirmwareSource::new(FIRMWARE_URL).as_str(),
            "https://firmware.example/fw.bin"
        );
        assert_eq!(
            LoggableFirmwareSource::new(
                "ftp://user:password@firmware.example/fw.bin?token=secret#download"
            )
            .as_str(),
            "ftp://firmware.example/fw.bin"
        );
        let invalid_source = LoggableFirmwareSource::new("http://");
        assert_eq!(invalid_source.as_str(), "<invalid firmware source>");
        assert_eq!(
            invalid_source.redact_from("upstream=http://other.example/fw.bin"),
            "upstream=http://other.example/fw.bin"
        );
        assert_eq!(
            context,
            "firmware=https://firmware.example/fw.bin; \
             config=https://config.example/device.ini; bearer=REDACTED; password=REDACTED"
        );
        for secret in [
            "userinfo-secret",
            "url-secret",
            "config-secret",
            BEARER,
            PASSWORD,
        ] {
            assert!(!context.contains(secret));
        }
    }

    #[test]
    fn firmware_error_context_redacts_each_secret_credential_variant() {
        #[derive(Clone, Copy)]
        enum CredentialCase {
            Bearer,
            Basic,
            Header,
            SshPassphrase,
        }

        check_values(
            [
                Check {
                    scenario: "bearer token",
                    input: CredentialCase::Bearer,
                    expect: "failure: REDACTED".to_string(),
                },
                Check {
                    scenario: "basic password",
                    input: CredentialCase::Basic,
                    expect: "failure: REDACTED".to_string(),
                },
                Check {
                    scenario: "custom header value",
                    input: CredentialCase::Header,
                    expect: "failure: REDACTED".to_string(),
                },
                Check {
                    scenario: "SSH key passphrase",
                    input: CredentialCase::SshPassphrase,
                    expect: "failure: REDACTED".to_string(),
                },
            ],
            |case| {
                let (credentials, secret) = match case {
                    CredentialCase::Bearer => {
                        (Credentials::bearer_token("bearer-secret"), "bearer-secret")
                    }
                    CredentialCase::Basic => (
                        Credentials::basic_auth("user", "basic-secret"),
                        "basic-secret",
                    ),
                    CredentialCase::Header => (
                        Credentials::header("X-Firmware-Token", "header-secret"),
                        "header-secret",
                    ),
                    CredentialCase::SshPassphrase => (
                        Credentials::ssh_key_with_passphrase("/key", "passphrase-secret"),
                        "passphrase-secret",
                    ),
                };
                let profile = firmware_profile("/firmware.bin", Some(credentials), None, None);

                firmware_error_context(&format!("failure: {secret}"), &profile)
            },
        );
    }

    #[test]
    fn firmware_secret_redaction_merges_overlapping_matches() {
        assert_eq!(
            mask_secret_values("prefix abcde suffix", ["abc", "cde"].into_iter()),
            "prefix REDACTED suffix"
        );
    }
}
