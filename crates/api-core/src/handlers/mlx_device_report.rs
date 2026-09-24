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

use carbide_uuid::machine::{HostMachineId, MachineId};
use db::ConditionalWrite;
use libmlx::device::report::MlxDeviceReport;
use model::machine::status::MlxDeviceObservation;
use rpc::protos::mlx_device::MlxDeviceReport as MlxDeviceReportPb;
use sqlx::PgPool;
use tonic::Request;

use crate::auth::AuthContext;
use crate::{CarbideError, CarbideResult};

pub(super) fn authenticated_machine_id<T>(
    request: &Request<T>,
) -> CarbideResult<Option<MachineId>> {
    request
        .extensions()
        .get::<AuthContext>()
        .and_then(AuthContext::get_spiffe_machine_id)
        .map(|machine_id| {
            machine_id.parse().map_err(|error| {
                CarbideError::PermissionDeniedError(format!(
                    "invalid authenticated machine identity: {error}"
                ))
            })
        })
        .transpose()
}

// Both report producers use the authenticated source, not the report's
// optional machine ID. A live Scout response does not include that field.
pub(super) async fn persist(
    pool: &PgPool,
    machine_id: MachineId,
    report: &MlxDeviceReportPb,
) -> CarbideResult<()> {
    let (host_machine_id, observation) = observation(machine_id, report.clone())?;
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| db::DatabaseError::new("acquire MLX observation connection", error))?;
    match db::machine::update_mlx_device_observation(
        &mut connection,
        &host_machine_id,
        &observation,
    )
    .await?
    {
        ConditionalWrite::Applied(()) => {}
        ConditionalWrite::NotApplied(_) => {
            tracing::debug!(
                %machine_id,
                observed_at = %observation.observed_at,
                "MLX observation not stored: machine is absent or a newer observation exists"
            );
        }
    }
    Ok(())
}

fn observation(
    machine_id: MachineId,
    report: MlxDeviceReportPb,
) -> CarbideResult<(HostMachineId, MlxDeviceObservation)> {
    let host_machine_id = HostMachineId::try_from(machine_id).map_err(|error| {
        CarbideError::InvalidArgument(format!("MLX observation requires a host identity: {error}"))
    })?;
    if report
        .machine_id
        .is_some_and(|reported| reported != machine_id)
    {
        return Err(CarbideError::PermissionDeniedError(
            "MLX report machine ID does not match the authenticated source".into(),
        ));
    }
    let report: MlxDeviceReport = report.try_into().map_err(CarbideError::InvalidArgument)?;
    if report
        .filters
        .as_ref()
        .is_some_and(|filters| filters.has_filters())
    {
        return Err(CarbideError::InvalidArgument(
            "filtered MLX report cannot replace a whole-host observation".into(),
        ));
    }
    if report.devices.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "empty MLX report cannot replace a whole-host observation".into(),
        ));
    }
    Ok((
        host_machine_id,
        MlxDeviceObservation {
            observed_at: report.timestamp,
            devices: report.devices,
        },
    ))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use rpc::protos::mlx_device::{
        DeviceField, DeviceFilter, DeviceFilterSet, MatchMode, MlxDeviceInfo,
    };
    use tonic::{Code, Status};

    use super::*;

    #[test]
    fn only_whole_host_reports_become_observations() {
        let host: MachineId = "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30"
            .parse()
            .unwrap();
        let dpu: MachineId = "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng"
            .parse()
            .unwrap();
        let report = || MlxDeviceReportPb {
            timestamp: Some(
                prost_types::Timestamp {
                    seconds: 1,
                    nanos: 0,
                }
                .into(),
            ),
            devices: vec![MlxDeviceInfo {
                pci_name: "01:00.0".into(),
                device_type: "ConnectX-8".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        scenarios!(
            run = |(source, report)| observation(source, report)
                .map(|(_, observation)| observation.devices.len())
                .map_err(|error| Status::from(error).code());
            "partial facts and an empty filter set describe the whole host" {
                (host, MlxDeviceReportPb {
                    filters: Some(DeviceFilterSet { filters: vec![] }),
                    ..report()
                }) => Yields(1),
            }
            "a DPU cannot replace a host observation" {
                (dpu, report()) => FailsWith(Code::InvalidArgument),
            }
            "embedded machine ID cannot redirect the write" {
                (host, MlxDeviceReportPb { machine_id: Some(dpu), ..report() })
                    => FailsWith(Code::PermissionDenied),
            }
            "filtered results cannot replace the device list" {
                (host, MlxDeviceReportPb {
                    filters: Some(DeviceFilterSet { filters: vec![DeviceFilter {
                        field: DeviceField::DeviceType.into(),
                        values: vec!["ConnectX-8".into()],
                        match_mode: MatchMode::Exact.into(),
                    }] }),
                    ..report()
                }) => FailsWith(Code::InvalidArgument),
            }
            "empty report is not evidence that all devices disappeared" {
                (host, MlxDeviceReportPb { devices: vec![], ..report() })
                    => FailsWith(Code::InvalidArgument),
            }
            "missing collection time cannot be ordered against the snapshot" {
                (host, MlxDeviceReportPb { timestamp: None, ..report() })
                    => FailsWith(Code::InvalidArgument),
            }
        );
    }
}
