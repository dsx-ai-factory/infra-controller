// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::future::Future;
use std::time::Duration;

use carbide_instrument::red;
use db::direct_dispatch_rms_job::RmsJobKind as StoredJobKind;
use futures_util::{FutureExt, StreamExt, stream};
use librms::protos::rack_manager as rms;
use librms::{RackManagerError, RmsApi};
use mac_address::MacAddress;
use model::component_manager::FirmwareState;
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout_at};

use super::{
    RmsBackend, RmsSwitchSystemImageStatusApi, RmsTrackedFirmwareJob, StagedFirmwareObject,
    map_rms_firmware_job_state, map_rms_switch_system_image_job_state,
};
use crate::error::ComponentManagerError;
use crate::firmware_jobs::{
    ComponentFirmwareJobsResult, FirmwareJobKind as ObservedJobKind, FirmwareJobObservation,
    FirmwareJobReadError, StagedFirmwareJob,
};

// Bound database acquisition and both RMS protocols together. The endpoint
// workflows own their polls, so cancellation also drops unfinished RPCs.
pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(10);
// Cap retained input and queued work; eight endpoint slots allow unavailable
// jobs to wait independently. Writers retain at most one job per known kind.
const ENDPOINT_LIMIT: usize = 64;
const ENDPOINT_CONCURRENCY: usize = 8;
const JOB_CONCURRENCY: usize = 2;
// After snapshot setup, cap subsequent statements at five seconds without
// extending a tighter site limit. Acquisition and setup share READ_TIMEOUT too.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests;

impl RmsBackend {
    /// Reads a projection of tracking without holding updater locks during I/O.
    /// Staged payloads and failure strings never enter the observation snapshot.
    pub(super) async fn observe_firmware_jobs(
        &self,
        macs: &[MacAddress],
        include_switch_jobs: bool,
        deadline: Instant,
    ) -> Result<Vec<ComponentFirmwareJobsResult>, ComponentManagerError> {
        if macs.len() > ENDPOINT_LIMIT {
            return Err(ComponentManagerError::InvalidArgument(format!(
                "passive firmware jobs allow at most {ENDPOINT_LIMIT} management MACs"
            )));
        }
        let mut snapshots: Vec<_> = {
            // Readers take jobs then staged, holding both only for this sample.
            // Writers do not publish the pair atomically; the phase may change
            // before any RPC finishes, so this is evidence, not an update fence.
            let jobs = self.firmware_jobs.lock().unwrap();
            let staged = self.staged_firmware_objects.lock().unwrap();
            macs.iter()
                .enumerate()
                .map(|(index, mac)| {
                    let known_jobs = jobs
                        .get(mac)
                        .into_iter()
                        .flatten()
                        .filter(|job| {
                            include_switch_jobs
                                || matches!(job, RmsTrackedFirmwareJob::FirmwareObject(_))
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    let phase = if include_switch_jobs {
                        staged.get(mac).map(|phase| match phase {
                            StagedFirmwareObject::Staged {
                                system_image_job, ..
                            } => StagedFirmwareJob::Queued {
                                system_image_job_id: system_image_job.clone(),
                            },
                            StagedFirmwareObject::Submitting { system_image_job } => {
                                StagedFirmwareJob::Submitting {
                                    system_image_job_id: system_image_job.clone(),
                                }
                            }
                            StagedFirmwareObject::Failed(_) => StagedFirmwareJob::Failed,
                        })
                    } else {
                        None
                    };
                    (index, (*mac, known_jobs, phase))
                })
                .collect()
        };

        // Poll cached jobs before fallback workflows can occupy all eight
        // slots waiting for DB capacity. Keep each original index for output.
        snapshots.sort_by_key(|(_, (_, jobs, _))| jobs.is_empty());

        // One fallback may own a DB session at a time. This is local to the
        // request, so in-memory peers need not wait for database preparation.
        let tracking_reads = Semaphore::new(1);
        let mut results: Vec<_> = stream::iter(snapshots)
            .map(|(index, (mac_address, known_jobs, staged))| {
                let tracking_reads = &tracking_reads;
                async move {
                    let jobs = self
                        .read_known_firmware_jobs(
                            mac_address,
                            known_jobs,
                            include_switch_jobs,
                            tracking_reads,
                            deadline,
                        )
                        .await;
                    (
                        index,
                        ComponentFirmwareJobsResult {
                            mac_address,
                            jobs,
                            staged,
                        },
                    )
                }
            })
            .buffer_unordered(ENDPOINT_CONCURRENCY)
            .collect()
            .await;
        results.sort_by_key(|(index, _)| *index);
        Ok(results.into_iter().map(|(_, result)| result).collect())
    }

    /// Recovers IDs only when memory has none, then polls each job independently.
    /// The snapshot session ends before the reader waits for RMS.
    async fn read_known_firmware_jobs(
        &self,
        mac: MacAddress,
        mut jobs: Vec<RmsTrackedFirmwareJob>,
        include_switch_jobs: bool,
        tracking_reads: &Semaphore,
        deadline: Instant,
    ) -> Result<Vec<FirmwareJobObservation>, FirmwareJobReadError> {
        if jobs.is_empty() {
            // A normal pooled query can retain its slot during SQLx cleanup.
            // Abandoning our guard instead detaches the connection immediately.
            let tracking_read = async {
                let _permit = tracking_reads
                    .acquire()
                    .await
                    .expect("the request never closes its tracking semaphore");
                let mut session = db::DbSessionGuard::acquire(&self.db).await?;
                let mut txn = session.begin_read_only_snapshot(STATEMENT_TIMEOUT).await?;
                let rows = db::direct_dispatch_rms_job::get_firmware_jobs(&mut txn, mac).await?;
                txn.commit().await?;
                session.return_to_pool();
                Ok::<_, db::DatabaseError>(rows)
            };
            let classified_read = tracking_read.map(|result| {
                result.map_err(|error| {
                    // Match only driver categories. Display/Debug would include
                    // SQL, connection details or backend diagnostic text.
                    let database_error_class = match &error {
                        db::DatabaseError::Sqlx(error) => match &error.source {
                            sqlx::Error::PoolClosed => "pool_closed",
                            sqlx::Error::PoolTimedOut => "pool_timeout",
                            sqlx::Error::Database(_) => "database",
                            sqlx::Error::Io(_) => "io",
                            sqlx::Error::Tls(_) => "tls",
                            sqlx::Error::Protocol(_) => "protocol",
                            sqlx::Error::Configuration(_) => "configuration",
                            sqlx::Error::RowNotFound => "row_not_found",
                            _ => "other",
                        },
                        _ => "other",
                    };
                    tracing::debug!(
                        management_mac_address = %mac,
                        failure_class = ?FirmwareJobReadError::TrackingReadFailed,
                        database_error_class,
                        "firmware job tracking read failed"
                    );
                    FirmwareJobReadError::TrackingReadFailed
                })
            });
            let rows = before_deadline(deadline, classified_read)
                .await
                .inspect_err(|error| {
                    if *error == FirmwareJobReadError::DeadlineExceeded {
                        tracing::debug!(
                            management_mac_address = %mac,
                            failure_class = ?error,
                            "firmware job tracking read failed"
                        );
                    }
                })?;
            jobs = rows
                .into_iter()
                .filter(|(kind, _)| include_switch_jobs || *kind == StoredJobKind::FirmwareObject)
                .filter_map(|(kind, id)| RmsTrackedFirmwareJob::from_persisted(kind, id))
                .collect();
        }
        // The observation contract's order is independent of storage enum order.
        jobs.sort_by_key(|job| match job {
            RmsTrackedFirmwareJob::FirmwareObject(_) => 0,
            RmsTrackedFirmwareJob::SwitchSystemImage(_) => 1,
        });

        Ok(stream::iter(jobs)
            .map(|job| async move {
                let state = observe_job(
                    self.client.as_ref(),
                    self.switch_system_image_client.as_deref(),
                    &job,
                    deadline,
                )
                .await;
                FirmwareJobObservation {
                    kind: match &job {
                        RmsTrackedFirmwareJob::FirmwareObject(_) => ObservedJobKind::FirmwareObject,
                        RmsTrackedFirmwareJob::SwitchSystemImage(_) => {
                            ObservedJobKind::SwitchSystemImage
                        }
                    },
                    job_id: job.job_id().to_owned(),
                    state,
                }
            })
            .buffered(JOB_CONCURRENCY)
            .collect()
            .await)
    }
}

/// Rejects late evidence even when an inner future returns ready after spending
/// the deadline in one poll. `timeout_at` alone accepts that ready result.
async fn before_deadline<T>(
    deadline: Instant,
    read: impl Future<Output = Result<T, FirmwareJobReadError>>,
) -> Result<T, FirmwareJobReadError> {
    if Instant::now() >= deadline {
        return Err(FirmwareJobReadError::DeadlineExceeded);
    }
    let result = timeout_at(deadline, read)
        .await
        .map_err(|_| FirmwareJobReadError::DeadlineExceeded)?;
    if Instant::now() >= deadline {
        return Err(FirmwareJobReadError::DeadlineExceeded);
    }
    result
}

/// Keeps only transport class and known request context. gRPC NotFound does
/// not establish application job absence; Unimplemented identifies an unavailable protocol.
fn poll_error(error: RackManagerError, job: &RmsTrackedFirmwareJob) -> FirmwareJobReadError {
    let job_kind = match job {
        RmsTrackedFirmwareJob::FirmwareObject(_) => "firmware_object",
        RmsTrackedFirmwareJob::SwitchSystemImage(_) => "switch_system_image",
    };
    match error {
        RackManagerError::ApiInvocationError(status) => {
            let grpc_status_code = status.code();
            tracing::debug!(
                ?grpc_status_code,
                job_kind,
                job_id = job.job_id(),
                "firmware job RPC failed"
            );
            if grpc_status_code == tonic::Code::Unimplemented {
                FirmwareJobReadError::StatusUnavailable
            } else {
                FirmwareJobReadError::PollFailed
            }
        }
        RackManagerError::TlsError(_) => {
            tracing::debug!(
                job_kind,
                job_id = job.job_id(),
                failure_class = "tls",
                "firmware job connection failed"
            );
            FirmwareJobReadError::PollFailed
        }
    }
}

/// Queries the tracked protocol and accepts only matching, recognized evidence.
/// This mapper discards free-form response/error text before returning or logging.
/// RED measures the client call, including its local deadline. Protocol Failure
/// or a reported Failed state remains response evidence, not a transport error.
async fn observe_job(
    client: &dyn RmsApi,
    system_image_client: Option<&dyn RmsSwitchSystemImageStatusApi>,
    job: &RmsTrackedFirmwareJob,
    deadline: Instant,
) -> Result<FirmwareState, FirmwareJobReadError> {
    // Expired work was never dispatched, so it must not count as an outbound
    // RED observation or invoke either status protocol.
    if Instant::now() >= deadline {
        return Err(FirmwareJobReadError::DeadlineExceeded);
    }
    let (status, echoed_id, state) = match job {
        RmsTrackedFirmwareJob::FirmwareObject(job_id) => {
            let response = red::instrumented(
                "rms",
                "get_firmware_job_status",
                before_deadline(deadline, async {
                    client
                        .get_firmware_job_status(rms::GetFirmwareJobStatusRequest {
                            job_id: job_id.clone(),
                        })
                        .await
                        .map_err(|error| poll_error(error, job))
                }),
            )
            .await?;
            (
                response.status,
                response.job_id,
                map_rms_firmware_job_state(response.job_state),
            )
        }
        RmsTrackedFirmwareJob::SwitchSystemImage(job_id) => {
            let Some(client) = system_image_client else {
                return Err(FirmwareJobReadError::StatusUnavailable);
            };
            let response = red::instrumented(
                "rms",
                "get_switch_system_image_job_status",
                before_deadline(deadline, async {
                    client
                        .get_switch_system_image_job_status(
                            rms::GetSwitchSystemImageJobStatusRequest {
                                job_id: job_id.clone(),
                            },
                        )
                        .await
                        .map_err(|error| poll_error(error, job))
                }),
            )
            .await?;
            (
                response.status,
                response.job_id,
                map_rms_switch_system_image_job_state(&response.state),
            )
        }
    };
    // RPC timing alone does not cover synchronous state normalization above.
    // Reject that evidence too if it spent the remaining shared budget.
    if Instant::now() >= deadline {
        return Err(FirmwareJobReadError::DeadlineExceeded);
    }
    if !echoed_id.is_empty() && echoed_id != job.job_id() {
        return Err(FirmwareJobReadError::StatusUnavailable);
    }
    // Both pinned status protocols define Failure as an absent job. Neither
    // absence nor an unrecognized response proves the update itself failed.
    match rms::ReturnCode::try_from(status) {
        Ok(rms::ReturnCode::Failure) => Err(FirmwareJobReadError::JobNotFound),
        Ok(rms::ReturnCode::Success)
            if echoed_id == job.job_id() && state != FirmwareState::Unknown =>
        {
            Ok(state)
        }
        _ => Err(FirmwareJobReadError::StatusUnavailable),
    }
}
