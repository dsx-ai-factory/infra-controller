// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Persist whole-host reservations for commands sent through Scout.

use carbide_uuid::machine::MachineId;
use model::hardware_dispatch::{HardwareDispatchOwner, MlxAdminCommand};
use rpc::forge::{
    ScoutStreamApiBoundMessage, ScoutStreamScoutBoundMessage,
    scout_stream_api_bound_message as output, scout_stream_scout_bound_message as input,
};
use rpc::protos::mlx_device as pb;
use sqlx::PgPool;
use tonic::Status;

use crate::api::Api;
use crate::scout_stream::ScoutRequestError;
use crate::{CarbideError, CarbideResult};

// Administrative callers may wait up to 30 minutes for a reserved command.
// This is a request-lifetime policy, not a tool deadline or a hardware timing
// guarantee. Earlier caller cancellation also retains the saved command.
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

// A small control-plane transaction gets ten seconds including pool acquisition
// and commit, so one blocked row cannot stop Scout reply routing indefinitely.
// Each database phase has its own bound, outside the longer reply wait.
const DATABASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Bound a database attempt without interpreting cancellation as completion.
async fn database_attempt<T>(
    machine_id: MachineId,
    operation: &'static str,
    attempt: impl std::future::Future<Output = CarbideResult<T>>,
) -> CarbideResult<T> {
    tokio::time::timeout(DATABASE_TIMEOUT, attempt)
        .await
        .map_err(|_| CarbideError::MlxCommandDeadlineExceeded {
            machine_id,
            operation,
        })?
}

/// Set a server-side statement bound as well as the outer attempt deadline.
/// A dropped SQLx future must not leave the server waiting indefinitely.
async fn database_transaction(pool: &PgPool) -> CarbideResult<db::Transaction<'_>> {
    let mut txn = db::Transaction::begin(pool).await?;
    let query = "SELECT set_config('statement_timeout', $1, true)";
    sqlx::query(query)
        .bind(format!("{}ms", DATABASE_TIMEOUT.as_millis()))
        .execute(txn.as_pgconn())
        .await
        .map_err(|error| db::DatabaseError::query(query, error))?;
    Ok(txn)
}

/// Commit ownership before the command can reach Scout. Only a recognized
/// successful reply or a local proof of non-enqueue settles this command.
pub(super) async fn send(
    api: &Api,
    machine_id: MachineId,
    request: ScoutStreamScoutBoundMessage,
) -> Result<ScoutStreamApiBoundMessage, Status> {
    let command = match request.payload.as_ref() {
        Some(input::Payload::MlxDeviceProfileSyncRequest(_)) => MlxAdminCommand::ProfileSync,
        Some(input::Payload::MlxDeviceConfigSetRequest(_)) => MlxAdminCommand::ConfigSet,
        Some(input::Payload::MlxDeviceConfigSyncRequest(_)) => MlxAdminCommand::ConfigSync,
        Some(input::Payload::MlxDeviceLockdownLockRequest(_)) => MlxAdminCommand::Lock,
        Some(input::Payload::MlxDeviceLockdownUnlockRequest(_)) => MlxAdminCommand::Unlock,
        _ => {
            return Err(CarbideError::Internal {
                message: "not a mutating MLX request".into(),
            }
            .into());
        }
    };
    let token = flow_token(request.flow_uuid.as_ref())?;
    let owner = HardwareDispatchOwner::MlxAdmin {
        machine_id,
        command,
    };
    let host = database_attempt(machine_id, "MLX command reservation", async {
        let mut txn = database_transaction(&api.database_connection).await?;
        let host = db::hardware_dispatch::begin(&mut txn, token, &owner).await?;
        txn.commit().await.map_err(CarbideError::from)?;
        Ok(host)
    })
    .await?;

    let send = api
        .scout_stream_registry
        .send_request_with_delivery(machine_id, request);
    // Unattached DPUs have no host reservation; their stream wait is unbounded.
    let result = if host.is_some() {
        tokio::time::timeout(REPLY_TIMEOUT, send)
            .await
            .map_err(|_| CarbideError::MlxCommandDeadlineExceeded {
                machine_id,
                operation: "MLX command reply",
            })?
    } else {
        send.await
    };
    let (response, not_sent) = match result {
        Ok(response) => (Ok(response), false),
        Err(ScoutRequestError::NotEnqueued(error)) => (Err(error), true),
        Err(ScoutRequestError::Uncertain(error)) => return Err(CarbideError::Internal {
            message: format!("MLX {command:?} command {token} to {machine_id} has an uncertain result; any host reservation is retained: {error}"),
        }.into()),
    };
    if let Some(host) = host
        && (not_sent
            || response
                .as_ref()
                .is_ok_and(|reply| completed(command, reply)))
    {
        // Ingress handles authenticated late replies. The waiter also settles
        // unbound replies and retries after an ingress persistence failure.
        if let Err(error) = database_attempt(machine_id, "MLX command settlement", async {
            let mut txn = database_transaction(&api.database_connection).await?;
            db::hardware_dispatch::settle(&mut txn, host, token, &owner).await?;
            txn.commit().await.map_err(CarbideError::from)?;
            Ok(())
        })
        .await
        {
            // Keep the known device result (or proven non-enqueue error). A
            // failed bookkeeping attempt must not invent a device failure.
            tracing::warn!(%machine_id, %host, %token, error = %error,
                "Failed to settle MLX command; saved reservation may remain pending");
        }
    }
    response
}

/// Accept an authenticated reply even after its API caller stops waiting.
/// Scout does not replay replies after reconnect; a lost reply stays unresolved.
pub(in crate::handlers) async fn accept_reply(
    pool: &PgPool,
    machine_id: MachineId,
    response: &ScoutStreamApiBoundMessage,
) -> CarbideResult<()> {
    if !matches!(
        response.payload,
        Some(
            output::Payload::MlxDeviceProfileSyncResponse(_)
                | output::Payload::MlxDeviceConfigSetResponse(_)
                | output::Payload::MlxDeviceConfigSyncResponse(_)
                | output::Payload::MlxDeviceLockdownResponse(_)
        )
    ) {
        return Ok(());
    }
    let Ok(token) = flow_token(response.flow_uuid.as_ref()) else {
        return Ok(());
    };
    database_attempt(machine_id, "late MLX command settlement", async {
        let mut txn = database_transaction(pool).await?;
        if let Some((host, dispatch)) = db::hardware_dispatch::find(&mut txn, token).await?
            && dispatch.is_pending()
        {
            let HardwareDispatchOwner::MlxAdmin {
                machine_id: owner,
                command,
            } = dispatch.owner;
            if owner == machine_id && completed(command, response) {
                db::hardware_dispatch::settle(&mut txn, host, token, &dispatch.owner).await?;
            }
        }
        txn.commit().await?;
        Ok(())
    })
    .await
}

/// Decode the server-created flow UUID; invalid values are internal errors.
fn flow_token(token: Option<&rpc::common::Uuid>) -> CarbideResult<uuid::Uuid> {
    token
        .cloned()
        .ok_or_else(|| CarbideError::Internal {
            message: "missing MLX flow UUID".into(),
        })?
        .try_into()
        .map_err(|_| CarbideError::Internal {
            message: "invalid MLX flow UUID".into(),
        })
}

/// Device errors may follow effects and therefore never establish settlement.
fn completed(command: MlxAdminCommand, response: &ScoutStreamApiBoundMessage) -> bool {
    match (command, response.payload.as_ref()) {
        (
            MlxAdminCommand::ProfileSync,
            Some(output::Payload::MlxDeviceProfileSyncResponse(reply)),
        ) => matches!(
            reply.reply,
            Some(pb::mlx_device_profile_sync_response::Reply::SyncResult(_))
        ),
        (MlxAdminCommand::ConfigSet, Some(output::Payload::MlxDeviceConfigSetResponse(reply))) => {
            matches!(
                reply.reply,
                Some(pb::mlx_device_config_set_response::Reply::TotalApplied(_))
            )
        }
        (
            MlxAdminCommand::ConfigSync,
            Some(output::Payload::MlxDeviceConfigSyncResponse(reply)),
        ) => matches!(
            reply.reply,
            Some(pb::mlx_device_config_sync_response::Reply::SyncResult(_))
        ),
        (
            MlxAdminCommand::Lock | MlxAdminCommand::Unlock,
            Some(output::Payload::MlxDeviceLockdownResponse(reply)),
        ) => {
            let expected = if command == MlxAdminCommand::Lock {
                pb::LockStatus::Locked
            } else {
                pb::LockStatus::Unlocked
            };
            matches!(&reply.reply, Some(pb::mlx_device_lockdown_response::Reply::StatusReport(report))
                if report.status == i32::from(expected))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    /// Successful payloads are distinct contracts. Conflicting lock state,
    /// another command's payload, missing evidence and vendor errors retain ownership.
    #[test]
    fn only_matching_success_settles_hardware_dispatch() {
        let lock = |status: pb::LockStatus| {
            output::Payload::MlxDeviceLockdownResponse(pb::MlxDeviceLockdownResponse {
                reply: Some(pb::mlx_device_lockdown_response::Reply::StatusReport(
                    pb::StatusReport {
                        status: status.into(),
                        ..Default::default()
                    },
                )),
            })
        };
        let set = || {
            output::Payload::MlxDeviceConfigSetResponse(pb::MlxDeviceConfigSetResponse {
                reply: Some(pb::mlx_device_config_set_response::Reply::TotalApplied(0)),
            })
        };
        let rows = [
            (
                "profile",
                MlxAdminCommand::ProfileSync,
                Some(output::Payload::MlxDeviceProfileSyncResponse(
                    pb::MlxDeviceProfileSyncResponse {
                        reply: Some(pb::mlx_device_profile_sync_response::Reply::SyncResult(
                            Default::default(),
                        )),
                    },
                )),
                true,
            ),
            ("set", MlxAdminCommand::ConfigSet, Some(set()), true),
            (
                "sync",
                MlxAdminCommand::ConfigSync,
                Some(output::Payload::MlxDeviceConfigSyncResponse(
                    pb::MlxDeviceConfigSyncResponse {
                        reply: Some(pb::mlx_device_config_sync_response::Reply::SyncResult(
                            Default::default(),
                        )),
                    },
                )),
                true,
            ),
            (
                "lock",
                MlxAdminCommand::Lock,
                Some(lock(pb::LockStatus::Locked)),
                true,
            ),
            (
                "unlock",
                MlxAdminCommand::Unlock,
                Some(lock(pb::LockStatus::Unlocked)),
                true,
            ),
            (
                "wrong lockdown state",
                MlxAdminCommand::Lock,
                Some(lock(pb::LockStatus::Unlocked)),
                false,
            ),
            (
                "unknown lock state",
                MlxAdminCommand::Lock,
                Some(lock(pb::LockStatus::Unknown)),
                false,
            ),
            (
                "unlock still locked",
                MlxAdminCommand::Unlock,
                Some(lock(pb::LockStatus::Locked)),
                false,
            ),
            (
                "lockdown error",
                MlxAdminCommand::Lock,
                Some(output::Payload::MlxDeviceLockdownResponse(
                    pb::MlxDeviceLockdownResponse {
                        reply: Some(pb::mlx_device_lockdown_response::Reply::Error(
                            Default::default(),
                        )),
                    },
                )),
                false,
            ),
            (
                "different command",
                MlxAdminCommand::ProfileSync,
                Some(set()),
                false,
            ),
            ("missing payload", MlxAdminCommand::ConfigSet, None, false),
            (
                "missing result",
                MlxAdminCommand::ConfigSet,
                Some(output::Payload::MlxDeviceConfigSetResponse(
                    Default::default(),
                )),
                false,
            ),
            (
                "possibly partial error",
                MlxAdminCommand::ConfigSet,
                Some(output::Payload::MlxDeviceConfigSetResponse(
                    pb::MlxDeviceConfigSetResponse {
                        reply: Some(pb::mlx_device_config_set_response::Reply::Error(
                            Default::default(),
                        )),
                    },
                )),
                false,
            ),
        ];
        check_values(
            rows.into_iter()
                .map(|(scenario, command, payload, expect)| Check {
                    scenario,
                    input: (
                        command,
                        ScoutStreamApiBoundMessage {
                            flow_uuid: None,
                            payload,
                        },
                    ),
                    expect,
                }),
            |(command, response)| completed(command, &response),
        );
    }
}
