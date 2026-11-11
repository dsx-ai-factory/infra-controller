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

use std::future::Future;
use std::time::Duration;

use ::rpc::forge as rpc;
use carbide_dpf::dpu_node_cr_name;
use carbide_uuid::machine::{HostMachineId, MachineId};
use db::ObjectFilter;
use db::machine::find_one;
use db::managed_host::load_snapshot;
use model::machine::LoadSnapshotOptions;
use model::machine::machine_search_config::MachineSearchConfig;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, RequestReceivedAt, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

// Match the default client timeout in `rpc::carbide_tls_client::REQUEST_TIMEOUT`.
const DPF_PREPARATION_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounds DPF pool acquisition, row admission, retries, waits, and commit.
/// The listener's receipt timestamp includes time in authorization and admission;
/// direct handler calls fall back to handler entry. Missing or malformed
/// `grpc-timeout` metadata keeps the 300-second cap; a valid shorter value
/// reduces it. The parser accepts the gRPC digit and unit grammar.
struct DpfPreparationDeadline {
    deadline: tokio::time::Instant,
}

impl DpfPreparationDeadline {
    fn from_request<T>(request: &tonic::Request<T>) -> Self {
        let started_at = request
            .extensions()
            .get::<RequestReceivedAt>()
            .map_or_else(tokio::time::Instant::now, |received| received.0);
        let budget = request
            .metadata()
            .get("grpc-timeout")
            .and_then(|value| value.to_str().ok())
            .and_then(grpc_timeout_duration)
            .unwrap_or(DPF_PREPARATION_TIMEOUT)
            .min(DPF_PREPARATION_TIMEOUT);
        Self {
            deadline: started_at + budget,
        }
    }

    /// `bound` constructs the transaction-owning future only while time remains.
    /// Expiry drops it; SQLx queues an open transaction's rollback. This does not
    /// guarantee immediate PostgreSQL cancellation or resolve an in-flight commit.
    async fn bound<T, MakeWork, Work>(self, make_work: MakeWork) -> Result<T, tonic::Status>
    where
        MakeWork: FnOnce() -> Work,
        Work: Future<Output = Result<T, tonic::Status>>,
    {
        let expired = || tonic::Status::from(CarbideError::DpfPreparationDeadlineExceeded);
        if tokio::time::Instant::now() >= self.deadline {
            return Err(expired());
        }
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(self.deadline) => Err(expired()),
            result = async move { make_work().await } => result,
        }
    }
}

fn grpc_timeout_duration(value: &str) -> Option<Duration> {
    let (&unit, digits) = value.as_bytes().split_last()?;
    if digits.is_empty() || digits.len() > 8 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let count = std::str::from_utf8(digits).ok()?.parse::<u64>().ok()?;
    Some(match unit {
        b'H' => Duration::from_secs(count * 3600),
        b'M' => Duration::from_secs(count * 60),
        b'S' => Duration::from_secs(count),
        b'm' => Duration::from_millis(count),
        b'u' => Duration::from_micros(count),
        b'n' => Duration::from_nanos(count),
        _ => return None,
    })
}

async fn lock_host_for_dpf_update(
    txn: &mut sqlx::PgConnection,
    machine_id: &HostMachineId,
) -> Result<(), CarbideError> {
    if !db::machine::lock_by_id_for_update_if_present(txn, &(*machine_id).into()).await? {
        return Err(CarbideError::NotFoundError {
            kind: "snapshot",
            id: machine_id.to_string(),
        });
    }
    Ok(())
}

pub(crate) async fn modify_dpf_state(
    api: &Api,
    request: Request<rpc::ModifyDpfStateRequest>,
) -> Result<Response<()>, Status> {
    let deadline = DpfPreparationDeadline::from_request(&request);
    log_request_data(&request);
    let request = request.get_ref();
    let machine_id: HostMachineId = convert_and_log_machine_id(request.machine_id.as_ref())?;

    deadline
        .bound(|| async {
            loop {
                let mut txn = api.txn_begin().await?;
                lock_host_for_dpf_update(txn.as_mut(), &machine_id).await?;
                match db::machine::try_lock_host_interfaces(txn.as_mut(), &machine_id).await? {
                    db::machine::HostInterfaceAdmission::Acquired => {}
                    db::machine::HostInterfaceAdmission::Contended(interface_id) => {
                        txn.rollback().await?;
                        db::machine::wait_for_host_interface(api.pg_pool(), &interface_id).await?;
                        continue;
                    }
                    db::machine::HostInterfaceAdmission::Changed => {
                        txn.rollback().await?;
                        continue;
                    }
                }
                let machine_snapshot =
                    load_snapshot(&mut txn, &machine_id, LoadSnapshotOptions::default())
                        .await?
                        .ok_or_else(|| CarbideError::NotFoundError {
                            kind: "snapshot",
                            id: machine_id.to_string(),
                        })?;

                if !request.dpf_enabled
                    && machine_snapshot.host_snapshot.config.dpf.used_for_ingestion
                {
                    return Err(CarbideError::FailedPrecondition(format!(
                        "cannot disable DPF for host {}: machine was ingested via DPF",
                        machine_id
                    ))
                    .into());
                }

                let mut dpu_ids = db::machine::MachineRowLockOrderIter::new(
                    machine_snapshot
                        .dpu_snapshots
                        .iter()
                        .map(|dpu| MachineId::from(dpu.id))
                        .collect(),
                )
                .collect::<Vec<_>>();
                dpu_ids.dedup();
                // `record_dpu_network_status` and queued controller writes can
                // hold a DPU row before they update its host or interfaces.
                // See api-db/LOCK_ORDERING.md for the rejection/wait protocol.
                let mut contended = None;
                for dpu_id in &dpu_ids {
                    if !db::machine::try_lock_by_id_for_no_key_update(txn.as_mut(), dpu_id).await? {
                        contended = Some(*dpu_id);
                        break;
                    }
                }
                if let Some(dpu_id) = contended {
                    txn.rollback().await?;
                    db::machine::wait_for_machine_no_key_update(api.pg_pool(), &dpu_id).await?;
                    continue;
                }
                db::machine::modify_dpf_state(&mut txn, &machine_id, request.dpf_enabled).await?;
                for dpu_id in dpu_ids {
                    db::machine::modify_dpf_state(&mut txn, &dpu_id, request.dpf_enabled).await?;
                }
                txn.commit().await?;
                return Ok(Response::new(()));
            }
        })
        .await
}

// Since this function sends only a bool with ids, we might not need pagination for this.
pub(crate) async fn get_dpf_state(
    api: &Api,
    request: Request<rpc::GetDpfStateRequest>,
) -> Result<Response<rpc::DpfStateResponse>, Status> {
    log_request_data(&request);
    let request = request.get_ref();

    for machine_id in &request.machine_ids {
        if machine_id.machine_type().is_dpu() {
            return Err(
                CarbideError::InvalidArgument("only host id is expected!!".to_string()).into(),
            );
        }
    }

    let mut txn = api.txn_begin().await?;
    let filter = if request.machine_ids.is_empty() {
        ObjectFilter::All
    } else {
        ObjectFilter::List(&request.machine_ids)
    };

    let dpf_states = db::machine::find(&mut txn, filter, MachineSearchConfig::default()).await?;
    txn.commit().await?;

    Ok(Response::new(rpc::DpfStateResponse {
        dpf_states: dpf_states
            .into_iter()
            .map(|machine| machine.into())
            .collect(),
    }))
}

pub(crate) async fn get_dpf_host_snapshot(
    api: &Api,
    request: Request<rpc::GetDpfHostSnapshotRequest>,
) -> Result<Response<rpc::DpfHostSnapshotResponse>, Status> {
    log_request_data(&request);
    let request = request.get_ref();
    let machine_id: HostMachineId = convert_and_log_machine_id(request.host_machine_id.as_ref())?;

    let Some(ops) = api.dpf_sdk.as_ref() else {
        return Err(CarbideError::InvalidArgument(
            "DPF is not enabled on this carbide instance".to_string(),
        )
        .into());
    };

    let mut txn = api.txn_begin().await?;
    let machine = find_one(&mut txn, &machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;
    txn.commit().await?;

    let host_dpf_id = machine.dpf_id().ok_or_else(|| {
        CarbideError::InvalidArgument(format!(
            "host {machine_id} has no BMC MAC; cannot derive DPF node name"
        ))
    })?;
    let node_name = dpu_node_cr_name(&host_dpf_id);

    let snapshot = ops
        .snapshot_host(&node_name)
        .await
        .map_err(CarbideError::DpfError)?;

    let json_payload = serde_json::to_string_pretty(&snapshot).map_err(|e| {
        CarbideError::internal(format!("failed to serialize DPF host snapshot: {e}"))
    })?;

    Ok(Response::new(rpc::DpfHostSnapshotResponse { json_payload }))
}

pub(crate) async fn get_dpf_service_versions(
    api: &Api,
    request: Request<rpc::GetDpfServiceVersionsRequest>,
) -> Result<Response<rpc::DpfServiceVersionsResponse>, Status> {
    log_request_data(&request);

    let cfg = &api.runtime_config.dpf.services;
    let configured = [
        &cfg.dts,
        &cfg.doca_hbn,
        &cfg.dpu_agent,
        &cfg.dhcp_server,
        &cfg.fmds,
        &cfg.otel,
    ];

    let live = if let Some(ops) = api.dpf_sdk.as_ref() {
        ops.list_service_template_versions()
            .await
            .map_err(CarbideError::DpfError)?
    } else {
        Vec::new()
    };

    let services = configured
        .iter()
        .map(|svc_cfg| {
            let matched = live
                .iter()
                .find(|t| t.deployment_service_name == svc_cfg.name);
            rpc::DpfServiceVersion {
                service: svc_cfg.name.clone(),
                config_helm_version: svc_cfg.helm_version.clone(),
                config_docker_image_tag: svc_cfg.docker_image_tag.clone(),
                live_helm_version: matched.map(|t| t.helm_version.clone()).unwrap_or_default(),
                live_docker_image_tag: matched
                    .map(|t| t.docker_image_tag.clone())
                    .unwrap_or_default(),
            }
        })
        .collect();

    Ok(Response::new(rpc::DpfServiceVersionsResponse { services }))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn dpf_preparation_deadline_uses_the_finite_caller_budget() {
        value_scenarios!(
            run = |header: Option<&str>| {
                let mut request = tonic::Request::new(());
                if let Some(header) = header {
                    request.metadata_mut().insert("grpc-timeout", header.parse().unwrap());
                }
                DpfPreparationDeadline::from_request(&request)
                    .deadline.duration_since(tokio::time::Instant::now())
            };
            "finite fallback and cap" {
                None => Duration::from_secs(300),
                Some("600S") => Duration::from_secs(300),
            }
            "gRPC timeout units" {
                Some("99999999H") => Duration::from_secs(300),
                Some("10000000u") => Duration::from_secs(10),
                Some("1M") => Duration::from_secs(60),
                Some("1S") => Duration::from_secs(1),
                Some("1m") => Duration::from_millis(1),
                Some("1u") => Duration::from_micros(1),
                Some("1n") => Duration::from_nanos(1),
                Some("0n") => Duration::ZERO,
            }
            "malformed metadata keeps the finite cap" {
                Some("S") => Duration::from_secs(300),
                Some("123456789S") => Duration::from_secs(300),
                Some("+1S") => Duration::from_secs(300),
                Some("1s") => Duration::from_secs(300),
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_dpf_preparation_does_not_construct_ready_work() {
        for (scenario, budget) in [
            ("zero", Duration::ZERO),
            ("expired", Duration::from_secs(1)),
        ] {
            let mut request = tonic::Request::new(());
            request.set_timeout(budget);
            let deadline = DpfPreparationDeadline::from_request(&request);
            tokio::time::advance(budget).await;
            let constructed = std::cell::Cell::new(false);
            let error = deadline
                .bound(|| {
                    constructed.set(true);
                    std::future::ready(Ok::<_, tonic::Status>(()))
                })
                .await
                .unwrap_err();
            assert_eq!(error.code(), tonic::Code::DeadlineExceeded, "{scenario}");
            assert!(
                !constructed.get(),
                "{scenario}: expired work was constructed"
            );
        }
    }

    // Make both branches ready after expiry. A successful result must not hide
    // the deadline or replace its typed error metadata.
    #[tokio::test(start_paused = true)]
    async fn dpf_preparation_expiry_precedes_an_already_ready_result() {
        let mut request = tonic::Request::new(());
        request.set_timeout(Duration::from_secs(1));
        let deadline = DpfPreparationDeadline::from_request(&request);
        let (ready, result) = tokio::sync::oneshot::channel();
        let work = deadline.bound(|| async move {
            result.await.unwrap();
            Ok(())
        });
        tokio::pin!(work);
        assert!(futures_util::poll!(&mut work).is_pending());
        tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
        ready.send(()).unwrap();
        let error = work.await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::DeadlineExceeded);
    }

    // A queued request must not receive a fresh metadata budget at handler entry.
    #[tokio::test(start_paused = true)]
    async fn dpf_preparation_deadline_includes_time_before_handler_entry() {
        let mut request = tonic::Request::new(());
        request.set_timeout(Duration::from_secs(1));
        request
            .extensions_mut()
            .insert(RequestReceivedAt(tokio::time::Instant::now()));
        tokio::time::advance(Duration::from_secs(2)).await;
        let constructed = std::cell::Cell::new(false);
        let error = DpfPreparationDeadline::from_request(&request)
            .bound(|| {
                constructed.set(true);
                std::future::ready(Ok::<_, tonic::Status>(()))
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::DeadlineExceeded);
        assert!(!constructed.get(), "expired queued work was constructed");
    }

    #[crate::sqlx_test]
    async fn dpf_host_admission_rejects_missing_host(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let host_id = HostMachineId::try_from(MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [53; 32],
            MachineType::Host,
        ))?;
        let mut attempt = pool.begin().await?;
        let rejected = lock_host_for_dpf_update(attempt.as_mut(), &host_id).await;
        attempt.rollback().await?;
        assert!(matches!(
            rejected,
            Err(CarbideError::NotFoundError { kind: "snapshot", id }) if id == host_id.to_string()
        ));
        Ok(())
    }
}
