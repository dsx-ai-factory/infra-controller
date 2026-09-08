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

//! The operator-driven release: `ReleaseDPUServiceSyncHold` and
//! `ListPendingDPUServiceSyncs`.
//!
//! What these have to prove is that relaxing the host-state requirement did not
//! relax anything else. A hold is still only lifted for a host whose DPUs all
//! match their DPUDeployment, and an assigned host is still protected unless the
//! operator named its tenant.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ::rpc::forge as rpc;
use carbide_uuid::machine::{MachineId, MachineIdSubtypeTrait};
use rpc::DpuServiceSyncReleaseStatus as ReleaseStatus;
use rpc::forge_server::Forge;
use tonic::Request;

use super::dpu_service_sync::{
    Fixture, is_outstanding, provisioned, provisioned_with_sync, request_sync,
    reset_host_to_waiting_for_ready,
};

fn by_machine_ids(ids: &[impl MachineIdSubtypeTrait]) -> rpc::ReleaseDpuServiceSyncHoldRequest {
    rpc::ReleaseDpuServiceSyncHoldRequest {
        target: Some(
            rpc::release_dpu_service_sync_hold_request::Target::MachineIds(
                ::rpc::common::MachineIdList {
                    machine_ids: ids.iter().copied().map(Into::into).collect::<Vec<_>>(),
                },
            ),
        ),
    }
}

async fn release(
    fixture: &Fixture,
    request: rpc::ReleaseDpuServiceSyncHoldRequest,
) -> Vec<(MachineId, ReleaseStatus, String)> {
    fixture
        .env
        .api
        .release_dpu_service_sync_hold(Request::new(request))
        .await
        .expect("release call")
        .into_inner()
        .results
        .into_iter()
        .map(|result| {
            (
                result.machine_id.expect("machine id"),
                result.status(),
                result.detail,
            )
        })
        .collect()
}

/// The worklist as an operator reads it: ids first, detail fetched per page.
async fn worklist_ids(fixture: &Fixture) -> Vec<MachineId> {
    fixture
        .env
        .api
        .find_pending_dpu_service_sync_ids(Request::new(
            rpc::FindPendingDpuServiceSyncIdsRequest {},
        ))
        .await
        .expect("worklist ids")
        .into_inner()
        .machine_ids
}

fn unsynced() -> (Arc<AtomicBool>, Arc<AtomicBool>) {
    (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )
}

/// Allocates a real instance onto the fixture's host.
///
/// Goes through the allocation RPC rather than writing the row directly, so the
/// host is assigned the same way production assigns it -- which is what the
/// release path re-reads.
async fn allocate(fixture: &Fixture) -> carbide_uuid::instance::InstanceId {
    let segment_id = fixture.env.create_vpc_and_tenant_segment().await;
    let (instance, _) = fixture
        .mh
        .instance_builer(&fixture.env)
        .single_interface_network_config(segment_id)
        .build_and_return()
        .await;
    instance.id
}

/// The first reason this RPC exists: a site that has turned the automatic
/// rollout off has no other way to finish one.
#[crate::sqlx_test]
async fn a_site_with_sync_disabled_can_still_be_released_by_hand(pool: sqlx::PgPool) {
    let (outdated, release_fails) = unsynced();
    let fixture = provisioned_with_sync(pool, outdated, release_fails, false).await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    let results = release(&fixture, by_machine_ids(&[fixture.mh.id])).await;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1, ReleaseStatus::Released);
    assert!(!is_outstanding(&fixture.pool, &fixture.mh.id).await);
    assert_eq!(fixture.calls.hold_releases.load(Ordering::SeqCst), 1);
}

/// The second reason: a host that never reaches `Ready` can never be released
/// automatically, and a bad DPUService version is exactly what strands one.
#[crate::sqlx_test]
async fn a_host_that_never_reaches_ready_can_be_released(pool: sqlx::PgPool) {
    let (outdated, release_fails) = unsynced();
    let fixture = provisioned(pool, outdated, release_fails).await;
    request_sync(&fixture.pool, &fixture.mh.id).await;
    reset_host_to_waiting_for_ready(&fixture.pool, &fixture.mh.id, &fixture.mh.dpu_ids[0]).await;

    let results = release(&fixture, by_machine_ids(&[fixture.mh.id])).await;

    assert_eq!(results[0].1, ReleaseStatus::Released);
    assert!(!is_outstanding(&fixture.pool, &fixture.mh.id).await);
}

/// The invariant the whole feature protects. Relaxing the state gate must not
/// have relaxed this one: assert no release was attempted, not merely that the
/// status says so.
#[crate::sqlx_test]
async fn an_outdated_dpu_is_deferred_and_never_released(pool: sqlx::PgPool) {
    let fixture = provisioned(
        pool,
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    let results = release(&fixture, by_machine_ids(&[fixture.mh.id])).await;

    assert_eq!(results[0].1, ReleaseStatus::DeferredDpuOutdated);
    assert_eq!(
        fixture.calls.hold_releases.load(Ordering::SeqCst),
        0,
        "an outdated DPU must not have its hold lifted by any path"
    );
    assert!(
        is_outstanding(&fixture.pool, &fixture.mh.id).await,
        "deferring must leave the work owed"
    );
}

/// Re-running is how an operator drains a worklist, so a second call must be
/// harmless rather than an error.
#[crate::sqlx_test]
async fn a_repeat_call_reports_not_pending_and_releases_nothing(pool: sqlx::PgPool) {
    let (outdated, release_fails) = unsynced();
    let fixture = provisioned(pool, outdated, release_fails).await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    let first = release(&fixture, by_machine_ids(&[fixture.mh.id])).await;
    assert_eq!(first[0].1, ReleaseStatus::Released);

    let second = release(&fixture, by_machine_ids(&[fixture.mh.id])).await;
    assert_eq!(second[0].1, ReleaseStatus::NotPending);
    assert_eq!(
        fixture.calls.hold_releases.load(Ordering::SeqCst),
        1,
        "the second call must not re-release"
    );
}

/// A failed release is retryable, so the work must still be owed afterwards.
#[crate::sqlx_test]
async fn a_failed_release_reports_failed_and_keeps_the_action(pool: sqlx::PgPool) {
    let release_fails = Arc::new(AtomicBool::new(false));
    let fixture = provisioned(
        pool,
        Arc::new(AtomicBool::new(false)),
        release_fails.clone(),
    )
    .await;
    request_sync(&fixture.pool, &fixture.mh.id).await;
    release_fails.store(true, Ordering::SeqCst);

    let results = release(&fixture, by_machine_ids(&[fixture.mh.id])).await;

    assert_eq!(results[0].1, ReleaseStatus::Failed);
    assert!(is_outstanding(&fixture.pool, &fixture.mh.id).await);
}

/// A batch is expected to be partly inapplicable, so one entry declining must
/// not fail the call or stop the others.
#[crate::sqlx_test]
async fn a_mixed_batch_reports_one_result_per_machine_in_request_order(pool: sqlx::PgPool) {
    let outdated = Arc::new(AtomicBool::new(false));
    let fixture = provisioned(pool, outdated.clone(), Arc::new(AtomicBool::new(false))).await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    // The same host twice: pending on the first pass, already handled on the
    // second, which is the shape a partly-drained worklist produces.
    let results = release(&fixture, by_machine_ids(&[fixture.mh.id, fixture.mh.id])).await;

    assert_eq!(results.len(), 2, "one result per named machine");
    assert_eq!(results[0].0, fixture.mh.id.into());
    assert_eq!(results[0].1, ReleaseStatus::Released);
    assert_eq!(results[1].1, ReleaseStatus::NotPending);
}

/// Naming a machine says nothing about the tenant that happens to be on it, so
/// the bulk path protects them. An operator pasting a worklist is not making a
/// tenant-impact decision per line.
#[crate::sqlx_test]
async fn an_assigned_host_named_by_machine_id_is_deferred(pool: sqlx::PgPool) {
    let (outdated, release_fails) = unsynced();
    let fixture = provisioned(pool, outdated, release_fails).await;
    let _instance = allocate(&fixture).await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    let results = release(&fixture, by_machine_ids(&[fixture.mh.id])).await;

    assert_eq!(results[0].1, ReleaseStatus::DeferredHostAssigned);
    assert_eq!(
        fixture.calls.hold_releases.load(Ordering::SeqCst),
        0,
        "a tenant must not be disrupted by a request that never named them"
    );
    assert!(
        is_outstanding(&fixture.pool, &fixture.mh.id).await,
        "the work is still owed once the host frees up"
    );
}

/// Naming the tenant's instance *is* the consent, and it is the only way to
/// deliver a fix to a host whose DPU services are broken while it is allocated.
#[crate::sqlx_test]
async fn an_assigned_host_named_by_instance_id_is_released(pool: sqlx::PgPool) {
    let (outdated, release_fails) = unsynced();
    let fixture = provisioned(pool, outdated, release_fails).await;
    let instance_id = allocate(&fixture).await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    let results = release(
        &fixture,
        rpc::ReleaseDpuServiceSyncHoldRequest {
            target: Some(
                rpc::release_dpu_service_sync_hold_request::Target::InstanceIds(
                    rpc::InstanceIdList {
                        instance_ids: vec![instance_id],
                    },
                ),
            ),
        },
    )
    .await;

    assert_eq!(results.len(), 1, "the instance resolves to its one host");
    assert_eq!(results[0].0, fixture.mh.id.into());
    assert_eq!(results[0].1, ReleaseStatus::Released);
    assert!(!is_outstanding(&fixture.pool, &fixture.mh.id).await);
}

/// Naming something that cannot be released must not release anything else in
/// the same call: the action is irreversible, so validation happens first.
#[crate::sqlx_test]
async fn an_unknown_machine_rejects_the_whole_call_before_releasing(pool: sqlx::PgPool) {
    let (outdated, release_fails) = unsynced();
    let fixture = provisioned(pool, outdated, release_fails).await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    let unknown = MachineId::new(
        carbide_uuid::machine::MachineIdSource::ProductBoardChassisSerial,
        [0xAB; 32],
        carbide_uuid::machine::MachineType::Host,
    );
    let status = fixture
        .env
        .api
        .release_dpu_service_sync_hold(Request::new(by_machine_ids(&[
            fixture.mh.id.into(),
            unknown,
        ])))
        .await
        .expect_err("unknown machine should be rejected");

    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(
        fixture.calls.hold_releases.load(Ordering::SeqCst),
        0,
        "a rejected request must not have released the valid machines first"
    );
    assert!(is_outstanding(&fixture.pool, &fixture.mh.id).await);
}

/// The hold is per node, so honouring a DPU id would widen the request from one
/// DPU to every DPU on its host.
#[crate::sqlx_test]
async fn a_dpu_id_is_rejected_rather_than_resolved_to_its_host(pool: sqlx::PgPool) {
    let (outdated, release_fails) = unsynced();
    let fixture = provisioned(pool, outdated, release_fails).await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    let status = fixture
        .env
        .api
        .release_dpu_service_sync_hold(Request::new(by_machine_ids(&[fixture.mh.dpu_ids[0]])))
        .await
        .expect_err("a DPU id should be rejected");

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert_eq!(fixture.calls.hold_releases.load(Ordering::SeqCst), 0);
}

/// The worklist is what an operator reads before naming anything, so a released
/// machine must leave it and the history must record who acted.
#[crate::sqlx_test]
async fn the_worklist_empties_on_release_and_the_history_records_the_operator(pool: sqlx::PgPool) {
    let (outdated, release_fails) = unsynced();
    let fixture = provisioned(pool, outdated, release_fails).await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    let worklist = worklist_ids(&fixture).await;
    assert_eq!(worklist, vec![fixture.mh.id.into()]);

    let detail = fixture
        .env
        .api
        .find_pending_dpu_service_syncs_by_ids(Request::new(
            rpc::FindPendingDpuServiceSyncsByIdsRequest {
                machine_ids: worklist.clone(),
            },
        ))
        .await
        .expect("detail")
        .into_inner()
        .pending;
    assert_eq!(detail.len(), 1);
    let worklist = detail;
    assert!(
        worklist[0].completed_by.is_none(),
        "an outstanding action has nobody to credit; UpdateInitiator::AdminCli is zero, \
         so a default here would misreport every waiting machine"
    );

    release(&fixture, by_machine_ids(&[fixture.mh.id])).await;

    assert!(
        worklist_ids(&fixture).await.is_empty(),
        "a released machine leaves the worklist"
    );

    let history = fixture
        .env
        .api
        .list_dpu_service_sync_history(Request::new(rpc::ListDpuServiceSyncHistoryRequest {
            machine_id: Some(fixture.mh.id.into()),
        }))
        .await
        .expect("history")
        .into_inner()
        .pending;
    assert_eq!(history.len(), 1);
    assert!(history[0].completed_at.is_some());
    assert_eq!(
        history[0].completed_by(),
        rpc::UpdateInitiator::AdminCli,
        "an operator-driven release must be distinguishable from carbide's own"
    );
}
