// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Host exclusion for commands that may outlive their Core request.

use carbide_uuid::machine::{HostMachineId, MachineId};
use model::hardware_dispatch::{HardwareDispatch, HardwareDispatchOwner};
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// Resolve a host or a DPU's non-BMC host attachment. Unattached targets have
/// no host to reserve; ambiguous attachments must not select an arbitrary host.
async fn owning_host(
    db: impl DbReader<'_>,
    machine_id: MachineId,
) -> DatabaseResult<Option<HostMachineId>> {
    if let Ok(host) = HostMachineId::try_from(machine_id) {
        return Ok(Some(host));
    }
    let query = "SELECT DISTINCT machine_id FROM machine_interfaces
                 WHERE attached_dpu_machine_id = $1 AND machine_id != $1
                 AND interface_type != 'Bmc' ORDER BY machine_id";
    let hosts: Vec<HostMachineId> = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    match hosts.as_slice() {
        [] => Ok(None),
        [host] => Ok(Some(*host)),
        _ => Err(DatabaseError::FailedPrecondition(
            "MLX command requires one unambiguous host attachment".into(),
        )),
    }
}

/// Reserve before enqueue and commit before awaiting Scout. Only the dispatch
/// column is written, so no state-history lock is needed. The host row lock
/// serializes with NIC ownership and other command reservations; typed loading
/// prevents malformed saved evidence from being overwritten as completed.
/// Missing hosts, unresolved commands and admitted NIC work reject the command.
/// Unattached DPUs proceed without a host reservation.
pub async fn begin(
    txn: &mut PgConnection,
    token: uuid::Uuid,
    owner: &HardwareDispatchOwner,
) -> DatabaseResult<Option<HostMachineId>> {
    let HardwareDispatchOwner::MlxAdmin { machine_id, .. } = owner;
    let machine_id = *machine_id;
    let Some(host) = owning_host(&mut *txn, machine_id).await? else {
        return Ok(None);
    };
    let query = "SELECT controller_state->>'state', hardware_dispatch
                 FROM machines WHERE id = $1 FOR NO KEY UPDATE";
    let current: Option<(String, Option<Json<HardwareDispatch>>)> = sqlx::query_as(query)
        .bind(host)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let Some((state, previous)) = current else {
        return Err(DatabaseError::NotFoundError {
            kind: "host",
            id: host.to_string(),
        });
    };
    if state == "nicfirmwareupdating" {
        return Err(DatabaseError::FailedPrecondition(
            "NIC firmware work already owns this host".into(),
        ));
    }
    if let Some(previous) = previous.as_ref().filter(|dispatch| dispatch.is_pending()) {
        return Err(DatabaseError::FailedPrecondition(format!(
            "host {host} is reserved by hardware command {} ({:?}, started {}); its exact result is still unresolved",
            previous.token, previous.owner, previous.started_at,
        )));
    }
    // Do not send using an attachment that changed while the host row was busy.
    if owning_host(&mut *txn, machine_id).await? != Some(host) {
        return Err(DatabaseError::FailedPrecondition(
            "MLX command's host attachment changed while reserving it".into(),
        ));
    }
    let dispatch = HardwareDispatch {
        token,
        owner: owner.clone(),
        started_at: crate::machine::database_now(&mut *txn).await?,
        settled_at: None,
    };
    let query = "UPDATE machines SET hardware_dispatch = $2 WHERE id = $1";
    sqlx::query(query)
        .bind(host)
        .bind(Json(dispatch))
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(Some(host))
}

/// Find the saved command by its token, independently of current attachments.
/// Returns None for an absent token; duplicate stored tokens fail closed.
pub async fn find(
    db: &mut PgConnection,
    token: uuid::Uuid,
) -> DatabaseResult<Option<(HostMachineId, HardwareDispatch)>> {
    let query = "SELECT id, hardware_dispatch FROM machines
                 WHERE hardware_dispatch->>'token' = $1 ORDER BY id LIMIT 2";
    let mut matches: Vec<(HostMachineId, Json<HardwareDispatch>)> = sqlx::query_as(query)
        .bind(token.to_string())
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop().map(|(host, dispatch)| (host, dispatch.0))),
        _ => Err(DatabaseError::FailedPrecondition(
            "hardware command token matches more than one host".into(),
        )),
    }
}

/// Settle only the reserved token and owner after a caller has established
/// successful completion or local non-enqueue. Transport errors, timeouts,
/// missing replies and inventory are never sufficient evidence. Returns false
/// for a missing host, stale token/owner, or an already settled command.
pub async fn settle(
    txn: &mut PgConnection,
    host: HostMachineId,
    token: uuid::Uuid,
    owner: &HardwareDispatchOwner,
) -> DatabaseResult<bool> {
    let query = "UPDATE machines SET hardware_dispatch = hardware_dispatch ||
                     jsonb_build_object('settled_at', clock_timestamp())
                 WHERE id = $1 AND hardware_dispatch->>'token' = $2
                   AND hardware_dispatch->'owner' = $3::jsonb
                   AND hardware_dispatch->>'settled_at' IS NULL
                 RETURNING id";
    Ok(sqlx::query_scalar::<_, HostMachineId>(query)
        .bind(host)
        .bind(token.to_string())
        .bind(Json(owner))
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
        .is_some())
}

/// Check current host exclusion while the caller holds its machine-row lock.
/// An unknown command remains pending until its exact sender settles it.
/// Returns false when the host or its pending record is absent.
pub async fn has_pending(db: impl DbReader<'_>, host: HostMachineId) -> DatabaseResult<bool> {
    let query = "SELECT EXISTS (SELECT 1 FROM machines WHERE id = $1
                 AND hardware_dispatch IS NOT NULL
                 AND hardware_dispatch->>'settled_at' IS NULL)";
    sqlx::query_scalar(query)
        .bind(host)
        .fetch_one(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

#[cfg(test)]
mod tests {
    use model::hardware_dispatch::MlxAdminCommand;
    use model::machine::ManagedHostState;
    use sqlx::PgPool;

    use super::*;

    /// Keep the stored command independent of an API or Scout connection.
    fn owner(machine_id: MachineId) -> HardwareDispatchOwner {
        HardwareDispatchOwner::MlxAdmin {
            machine_id,
            command: MlxAdminCommand::ConfigSet,
        }
    }

    /// Use ordinary machine creation so snapshot loading sees a valid Ready
    /// host, while the additive nullable column starts without a command.
    async fn seed_host(pool: &PgPool, host: HostMachineId) {
        let mut txn = pool.begin().await.unwrap();
        crate::machine::create(
            txn.as_mut(),
            None,
            &MachineId::from(host),
            ManagedHostState::Ready,
            None,
            2,
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
    }

    #[crate::sqlx_test]
    async fn hardware_dispatch_roundtrip_and_exact_settlement(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let host = *model::test_support::machine_snapshot::host_machine_id();
        seed_host(&pool, host).await;
        // Rows without saved commands must hydrate their nullable field as None.
        let before = crate::machine::find_one(&pool, &host, Default::default())
            .await?
            .unwrap();
        assert!(before.status.hardware_dispatch.is_none());
        let sender = owner(host.into());
        let token = uuid::Uuid::new_v4();
        let mut txn = pool.begin().await?;
        assert_eq!(begin(txn.as_mut(), token, &sender).await?, Some(host));
        txn.commit().await?;

        // Reload through the normal snapshot using a fresh connection, without
        // retaining the sender's transaction or any in-memory flow state.
        let saved = crate::machine::find_one(&pool, &host, Default::default())
            .await?
            .unwrap()
            .status
            .hardware_dispatch
            .unwrap();
        assert_eq!(saved.token, token);
        assert_eq!(saved.owner, sender);
        assert!(saved.is_pending());
        let mut txn = pool.begin().await?;
        assert!(matches!(
            begin(txn.as_mut(), uuid::Uuid::new_v4(), &sender).await,
            Err(DatabaseError::FailedPrecondition(_))
        ));
        txn.rollback().await?;

        // A right UUID with a different owner, and a stale UUID with the right
        // owner, must each leave the exact pending bytes unchanged.
        let other = HardwareDispatchOwner::MlxAdmin {
            machine_id: host.into(),
            command: MlxAdminCommand::Lock,
        };
        let other_machine = owner(model::test_support::machine_snapshot::dpu_machine_id(0).into());
        for (candidate, sender) in [
            (token, &other),
            (token, &other_machine),
            (uuid::Uuid::new_v4(), &sender),
        ] {
            let mut txn = pool.begin().await?;
            assert!(!settle(txn.as_mut(), host, candidate, sender).await?);
            txn.commit().await?;
            let mut conn = pool.acquire().await?;
            assert_eq!(find(&mut conn, token).await?.unwrap().1, saved);
        }

        // Exact completion permits one replacement, but delayed completion of
        // the old request cannot release that newer command.
        let mut txn = pool.begin().await?;
        assert!(settle(txn.as_mut(), host, token, &sender).await?);
        let settled = find(txn.as_mut(), token).await?.unwrap().1;
        assert!(!settle(txn.as_mut(), host, token, &sender).await?);
        assert_eq!(find(txn.as_mut(), token).await?.unwrap().1, settled);
        let replacement = uuid::Uuid::new_v4();
        assert_eq!(begin(txn.as_mut(), replacement, &sender).await?, Some(host));
        assert!(!settle(txn.as_mut(), host, token, &sender).await?);
        txn.commit().await?;
        let mut conn = pool.acquire().await?;
        let retained = find(&mut conn, replacement).await?.unwrap().1;
        assert_eq!(retained.token, replacement);
        assert!(retained.is_pending());
        Ok(())
    }

    #[crate::sqlx_test]
    async fn hardware_dispatch_rejects_committed_nic_ownership(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let host = *model::test_support::machine_snapshot::host_machine_id();
        seed_host(&pool, host).await;
        // The real admitted state, including a valid operation, owns the host
        // even before its first mutating tool is sent.
        let state = ManagedHostState::NicFirmwareUpdating {
            operation: model::test_support::nic_firmware::operation(),
        };
        sqlx::query("UPDATE machines SET controller_state = $2 WHERE id = $1")
            .bind(host)
            .bind(Json(state))
            .execute(&pool)
            .await?;
        let mut txn = pool.begin().await?;
        assert!(matches!(
            begin(txn.as_mut(), uuid::Uuid::new_v4(), &owner(host.into())).await,
            Err(DatabaseError::FailedPrecondition(_))
        ));
        txn.rollback().await?;
        let saved = crate::machine::find_one(&pool, &host, Default::default())
            .await?
            .unwrap();
        assert!(saved.status.hardware_dispatch.is_none());
        assert!(matches!(
            saved.state.value,
            ManagedHostState::NicFirmwareUpdating { .. }
        ));
        Ok(())
    }

    /// A waiter must read the first committed reservation after acquiring the
    /// row, rather than overwrite it using a snapshot taken before the lock.
    #[crate::sqlx_test]
    async fn hardware_dispatch_waiter_preserves_the_committed_owner(pool: PgPool) {
        let host = *model::test_support::machine_snapshot::host_machine_id();
        seed_host(&pool, host).await;
        let sender = owner(host.into());
        let token = uuid::Uuid::new_v4();
        let mut first = pool.begin().await.unwrap();
        let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(first.as_mut())
            .await
            .unwrap();
        begin(first.as_mut(), token, &sender).await.unwrap();

        let second_pool = pool.clone();
        let second_owner = sender.clone();
        let waiter = tokio::spawn(async move {
            let mut second = second_pool.begin().await.unwrap();
            let result = begin(second.as_mut(), uuid::Uuid::new_v4(), &second_owner).await;
            second.rollback().await.unwrap();
            result
        });
        // PostgreSQL's lock wait proves the competing command has reached the
        // actual reserve query before allowing the first owner to commit.
        crate::test_support::postgres::wait_for_blocked_query(
            &pool,
            blocker,
            "SELECT controller_state->>'state', hardware_dispatch",
        )
        .await;
        first.commit().await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(40), waiter)
            .await
            .expect("reservation waiter must finish after commit")
            .unwrap();
        assert!(matches!(result, Err(DatabaseError::FailedPrecondition(_))));
        let mut conn = pool.acquire().await.unwrap();
        let retained = find(&mut conn, token).await.unwrap().unwrap().1;
        assert_eq!(retained.token, token);
        assert_eq!(retained.owner, sender);
        assert!(retained.is_pending());
    }
}
