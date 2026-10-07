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

use ::rpc::protos::mlx_device::{
    FirmwareFlashReport as FirmwareFlashReportPb, MlxDeviceReport as MlxDeviceReportPb,
    PublishMlxDeviceReportRequest, PublishMlxDeviceReportResponse,
    PublishMlxObservationReportRequest, PublishMlxObservationReportResponse,
};
use carbide_instrument::emit;
use carbide_uuid::machine::MachineId;
use libmlx::device::discovery;
use libmlx::device::report::MlxDeviceReport;
use libmlx::firmware::config::FirmwareFlasherProfile;
use libmlx::firmware::credentials::Credentials;
use libmlx::firmware::flasher::FirmwareFlasher;
use libmlx::lockdown::error::{MlxError, MlxResult};
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
    ScoutMlxDeviceOperationFailed, ScoutMlxFirmwareFlashFailed,
    ScoutMlxFirmwareFlasherInitializationFailed, ScoutMlxOperationFailed,
    ScoutMlxProfileApplyFailed, ScoutMlxProfileOperationFailed, ScoutMlxProfileResetFailed,
    ScoutMlxRegistryLookupFailed, ScoutMlxRequestRejected,
};

// create_device_report_request is a one stop shop to collect
// Mellanox device data from the machine, create a report, convert
// it into the underlying protobuf type, and then return a request
// instance to publish to carbide-api.
pub(super) fn create_device_report_request(
    machine_id: MachineId,
) -> Result<PublishMlxDeviceReportRequest, String> {
    tracing::info!("creating PublishMlxDeviceReportRequest");
    let mut report = MlxDeviceReport::new().collect()?;
    report.machine_id = Some(machine_id);
    let report_pb: MlxDeviceReportPb = report.into();
    Ok(PublishMlxDeviceReportRequest {
        report: Some(report_pb),
    })
}

// publish_mlx_device_report is used to publish an MlxDeviceReport for the current
// machine, which will collect the hardware + firmware/version details of all Mellanox
// devices on the machine, including DPUs and DPAs. This is then published to carbide-api,
// which leverages this data for ensuring devices are synced with the correct mlxconfig
// settings, and have been (or will be instructed to) updated to the correct firmware version.
//
// When called from scout on a host, a report will contain *all* Mellanox devices on the host.
// When called from the agent on a DPU, a report will contain *only* the DPU its being called from.
pub(super) async fn publish_mlx_device_report(
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

pub(super) fn lockdown_error_context(error: &MlxError, key: &str) -> String {
    // Flint dry-run and command errors can echo the full invocation. Keep the
    // diagnostic while removing any non-empty key that reached the tool.
    let context = error.to_string();
    match error {
        MlxError::InvalidKey => context,
        _ if key.is_empty() => context,
        _ => context.replace(key, "REDACTED"),
    }
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
                error: lockdown_error_context(&e, &request.key),
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
                error: lockdown_error_context(&e, &request.key),
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

// apply_firmware executes the full firmware flash lifecycle for a device
// using the provided FirmwareFlasherProfile. Returns Some(report) on
// success (including partial success where flash worked but a post-flash
// step failed), or None if the flasher couldn't be created or the flash
// itself failed.
pub(super) async fn apply_firmware(
    device: &str,
    profile: &FirmwareFlasherProfile,
) -> Option<FirmwareFlashReportPb> {
    let firmware_url = LoggableFirmwareSource::new(&profile.flash_spec.firmware_url);
    let device_conf_url = profile
        .flash_spec
        .device_conf_url
        .as_deref()
        .map(LoggableFirmwareSource::new);
    let firmware_credential_type = profile
        .flash_spec
        .firmware_credentials
        .as_ref()
        .map(|c| c.type_name())
        .unwrap_or("none");

    let device_conf_credential_type = profile
        .flash_spec
        .device_conf_credentials
        .as_ref()
        .map(|c| c.type_name())
        .unwrap_or("none");

    tracing::info!(
        device = %device,
        part_number = %profile.firmware_spec.part_number,
        psid = %profile.firmware_spec.psid,
        firmware_url = %firmware_url.as_str(),
        firmware_credential_type,
        device_configuration_url = device_conf_url
            .as_ref()
            .map(LoggableFirmwareSource::as_str)
            .unwrap_or("none"),
        device_configuration_credential_type = device_conf_credential_type,
        target_version = %profile.firmware_spec.version,
        "applying firmware"
    );

    // Initialize a new FirmwareFlasher, leveraging new(..)
    // to validate the device identity matches FirmwareSpec.
    let flasher = match FirmwareFlasher::new(device, &profile.firmware_spec) {
        Ok(f) => f,
        Err(e) => {
            emit(ScoutMlxFirmwareFlasherInitializationFailed::new(
                device.to_string(),
                profile.firmware_spec.part_number.clone(),
                profile.firmware_spec.psid.clone(),
                firmware_error_context(&e.to_string(), profile),
            ));
            return None;
        }
    };

    // ...and now that we've got our FirmwareFlasher, lets take
    // the FirmwareFlasherProfile we got from carbide-api and
    // execute the full firmware lifecycle.
    match flasher.apply(profile).await {
        Ok(result) => {
            tracing::info!(
                device = %device,
                part_number = %profile.firmware_spec.part_number,
                psid = %profile.firmware_spec.psid,
                firmware_url = %firmware_url.as_str(),
                target_version = %profile.firmware_spec.version,
                "firmware flash successful"
            );
            Some(result.into())
        }
        Err(e) => {
            emit(ScoutMlxFirmwareFlashFailed::execute(
                device.to_string(),
                profile.firmware_spec.part_number.clone(),
                profile.firmware_spec.psid.clone(),
                firmware_url.as_str().to_string(),
                profile.firmware_spec.version.clone(),
                firmware_error_context(&e.to_string(), profile),
            ));
            None
        }
    }
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
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};
    use libmlx::firmware::config::{FirmwareSpec, FlashOptions, FlashSpec};

    use super::*;

    const MLX_FAILURE_METRIC: &str = "carbide_scout_mlx_failures_total";
    const DEVICE_ID: &str = "0000:01:00.0";
    const REGISTRY: &str = "missing-registry";

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

    #[test]
    fn lockdown_error_context_removes_valid_keys() {
        struct RedactionCase {
            error: MlxError,
            key: &'static str,
        }

        check_values(
            [
                Check {
                    scenario: "dry-run commands do not retain the key",
                    input: RedactionCase {
                        error: MlxError::DryRun(
                            "flint -d device hw_access enable deadbeef".to_string(),
                        ),
                        key: "deadbeef",
                    },
                    expect:
                        "dry run - would have executed: flint -d device hw_access enable REDACTED"
                            .to_string(),
                },
                Check {
                    scenario: "tool output does not retain the key",
                    input: RedactionCase {
                        error: MlxError::CommandFailed("flint rejected key DEADBEEF".to_string()),
                        key: "DEADBEEF",
                    },
                    expect: "command execution failed: flint rejected key REDACTED".to_string(),
                },
                Check {
                    scenario: "invalid keys cannot rewrite ordinary diagnostics",
                    input: RedactionCase {
                        error: MlxError::InvalidKey,
                        key: "a",
                    },
                    expect: "invalid key format or length".to_string(),
                },
            ],
            |case| lockdown_error_context(&case.error, case.key),
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
