// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Exercise startup backfill's pass budget and owning cancellation.

use std::sync::atomic::Ordering;
use std::time::Duration;

use carbide_resource_pool::{PoolDefinitionSource, ResourcePoolBackendError};
use carbide_site_explorer::dpu_asn::{DPU_ASN_SLOT, update_dpu_asns};
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use db::resource_pool::remote;
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::resource_pool::OwnerType;
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, ResourcePoolAllocation, ResourcePoolValue,
};
use model::resource_pool::common::FNN_ASN;
use model::resource_pool::remote::AllocationResult;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::tests::common::resource_pool::{RemotePool, RemotePoolFixture};

/// Distinct reasons the public pass must stop and clean up its active request.
#[derive(Clone, Copy, Debug)]
enum StopBoundary {
    PassBudget,
    OwningCancellation,
}

/// The pass budget expires after earlier DPU assignments have committed and
/// cancels its active provider request before returning.
#[crate::sqlx_test]
async fn remote_dpu_asn_backfill_budget_keeps_partial_progress(
    database: PgPool,
) -> eyre::Result<()> {
    check_stopped_backfill(database, StopBoundary::PassBudget).await
}

/// Owning cancellation preserves earlier assignments and prevents dispatch of
/// the next DPU while its current provider reply is held.
#[crate::sqlx_test]
async fn remote_dpu_asn_backfill_owning_cancellation_keeps_partial_progress(
    database: PgPool,
) -> eyre::Result<()> {
    check_stopped_backfill(database, StopBoundary::OwningCancellation).await
}

/// Stop with committed progress and a held reply, then verify both survive correctly.
async fn check_stopped_backfill(database: PgPool, boundary: StopBoundary) -> eyre::Result<()> {
    tokio::time::timeout(Duration::from_secs(120), Box::pin(async move {
        let mut fixture = RemotePoolFixture::start_with_values(
            database,
            &[RemotePool {
                name: FNN_ASN,
                start: 64_512,
                auto_assign: true,
            }],
            &[64_512, 64_513, 64_514, 64_515, 64_516, 64_517, 64_518],
            None,
        )
        .await?;
        // The range end is exclusive; include all seven provider values.
        fixture
            .definitions
            .get_mut(FNN_ASN)
            .expect("test ASN definition")
            .ranges[0]
            .end = "64519".into();
        fixture
            .initialize(PoolDefinitionSource::SeedConfiguration)
            .await?;

        let mut machine_ids = [81, 82, 83, 84, 85, 86, 87, 88].map(|byte| {
            MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [byte; 32],
                MachineType::Dpu,
            )
        });
        machine_ids.sort();
        let mut txn = fixture.env.pool.begin().await?;
        for machine_id in machine_ids {
            db::machine::create(
                &mut txn,
                None,
                &machine_id,
                ManagedHostState::Created,
                None,
                CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
        }
        txn.commit().await?;

        let cancel = CancellationToken::new();
        let pause = &fixture.provider.allocate_pause;
        pause.pause_next.store(true, Ordering::SeqCst);
        let mut backfill = Box::pin(update_dpu_asns(
            &fixture.env.pool,
            &fixture.env.api.resource_pool_registry,
            &fixture.env.common_pools,
            &cancel,
        ));
        let mut committed = Vec::new();

        for index in 0..7 {
            tokio::select! {
                result = &mut backfill => {
                    panic!("{boundary:?}: backfill stopped before provider reply {}: {result:?}", index + 1);
                }
                result = tokio::time::timeout(Duration::from_secs(15), pause.entered.notified()) => {
                    result?;
                }
            }

            // The next held reply proves the previous attachment
            // committed, while giving us time to read its exact identity.
            if index > 0 {
                let machine_id = machine_ids[index - 1];
                let saved = asn_receipt(&fixture, machine_id).await?;
                assert_eq!(saved.state, AllocationState::Attached, "{boundary:?}");
                let asn = db::machine::get_asn(&fixture.env.pool, &machine_id)
                    .await?
                    .expect("committed DPU ASN");
                let AllocationSource::Remote {
                    result: Some(AllocationResult::Allocated(assignment)),
                    ..
                } = &saved.source
                else {
                    panic!("{boundary:?}: attached ASN retains its provider assignment");
                };
                assert_eq!(assignment.value, ResourcePoolValue::Integer(i64::from(asn)));
                committed.push((machine_id, saved, asn));
            }
            if index == 6 {
                break;
            }

            if matches!(boundary, StopBoundary::PassBudget) {
                // Six nine-second replies consume 54 seconds of the
                // pass, each below the provider's ten-second deadline.
                // Only Tokio advances; PostgreSQL still uses wall time.
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(9)).await;
                tokio::time::resume();
            }
            // Rearm before releasing this reply so the next allocation
            // cannot finish before we observe its reservation.
            pause.pause_next.store(true, Ordering::SeqCst);
            pause.resume.notify_one();
        }

        let active = asn_receipt(&fixture, machine_ids[6]).await?;
        assert_eq!(active.state, AllocationState::Pending, "{boundary:?}");
        assert!(matches!(&active.source, AllocationSource::Remote { result: None, .. }));
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 7);
        assert_eq!(fixture.provider.local.provisioned_count(), 7);
        assert!(!pause.dropped.is_cancelled());

        match boundary {
            StopBoundary::PassBudget => {
                // The seventh provider wait is still below ten seconds
                // when the enclosing one-minute pass expires. Resume
                // wall time before cancellation writes reach PostgreSQL.
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(7)).await;
                tokio::time::resume();
            }
            StopBoundary::OwningCancellation => cancel.cancel(),
        }
        let error = tokio::time::timeout(Duration::from_secs(15), &mut backfill)
            .await?
            .expect_err("stopped backfill returns its stop reason");
        match boundary {
            StopBoundary::PassBudget => assert!(matches!(
                error,
                ResourcePoolBackendError::DeadlineExceeded {
                    operation: "DPU ASN backfill"
                }
            )),
            StopBoundary::OwningCancellation => assert!(matches!(
                error,
                ResourcePoolBackendError::Cancelled {
                    operation: "DPU ASN allocation"
                }
            )),
        }

        // Cleanup must already be durable when the public call returns.
        // The provider's drop signal also proves its held RPC was closed.
        let cancelled = asn_receipt(&fixture, machine_ids[6]).await?;
        assert_eq!(cancelled.allocation_id, active.allocation_id);
        assert_eq!(cancelled.source, active.source);
        assert_eq!(cancelled.state, AllocationState::CancelRequested);
        tokio::time::timeout(Duration::from_secs(15), pause.dropped.cancelled()).await?;
        assert_eq!(db::machine::get_asn(&fixture.env.pool, &machine_ids[6]).await?, None);
        assert_eq!(db::machine::get_asn(&fixture.env.pool, &machine_ids[7]).await?, None);
        assert!(
            remote::latest_for_owner_slot(
                &fixture.env.pool,
                OwnerType::Machine,
                &machine_ids[7].to_string(),
                DPU_ASN_SLOT,
            )
            .await?
            .is_none()
        );
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 7);
        assert_eq!(fixture.provider.local.provisioned_count(), 7);

        fixture
            .remote(FNN_ASN)
            .reconcile(active.allocation_id, &fixture.cancel)
            .await?;
        let cleaned = asn_receipt(&fixture, machine_ids[6]).await?;
        assert_eq!(cleaned.allocation_id, active.allocation_id);
        assert_eq!(cleaned.state, AllocationState::Cancelled);
        assert_eq!(fixture.provider.local.provisioned_count(), 6);
        for (machine_id, saved, asn) in committed {
            assert_eq!(asn_receipt(&fixture, machine_id).await?, saved);
            assert_eq!(db::machine::get_asn(&fixture.env.pool, &machine_id).await?, Some(asn));
        }
        drop(backfill);
        fixture.stop().await
    }))
    .await?
}

/// Reload the original ASN request so cleanup cannot silently switch identities.
async fn asn_receipt(
    fixture: &RemotePoolFixture,
    machine_id: MachineId,
) -> eyre::Result<ResourcePoolAllocation> {
    remote::latest_for_owner_slot(
        &fixture.env.pool,
        OwnerType::Machine,
        &machine_id.to_string(),
        DPU_ASN_SLOT,
    )
    .await?
    .ok_or_else(|| eyre::eyre!("missing saved ASN request for {machine_id}"))
}
