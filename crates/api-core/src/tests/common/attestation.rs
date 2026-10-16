/*
 * SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: LicenseRef-NvidiaProprietary
 *
 * NVIDIA CORPORATION, its affiliates and licensors retain all intellectual
 * property and proprietary rights in and to this material, related
 * documentation and any modifications thereto. Any use, reproduction,
 * disclosure or distribution of this material and related documentation
 * without an express license agreement from NVIDIA CORPORATION or
 * its affiliates is strictly prohibited.
 */

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use carbide_uuid::machine::MachineId;
use model::attestation::spdm::SpdmAttestationStatus;
use model::machine::{FailureDetails, ManagedHostState};
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    SpdmAttestationJobStatus, SpdmListAttestationRequest, SpdmListAttestationRequestSelector,
};
use sqlx::{Postgres, Transaction};
use tonic::Request;

use crate::tests::common::api_fixtures::{TestEnv, TestManagedHost};

// Only consumed by this crate's own `#[cfg(test)]` test cases (not by the `test-support` consumers),
// so gate it out of test-support-only builds to keep dead-code detection working.
#[cfg(test)]
pub(crate) async fn spdm_attestation_run_to_failed_then_to_success(
    env: &TestEnv,
    nras_should_fail_parsing_flag: Arc<AtomicBool>,
    mh: &TestManagedHost,
    txn: &mut Transaction<'_, Postgres>,
    poll_state: impl Fn(uuid::Uuid) -> ManagedHostState,
) {
    // first, let's get into a polling state
    let initial_poll_state = env
        .run_machine_state_controller_iteration_until_state_condition(
            &mh.host().id,
            10,
            |machine| {
                let Some(job_id) = spdm_poll_result_job_id(machine.current_state()) else {
                    return false;
                };
                machine.current_state() == &poll_state(job_id)
            },
        )
        .await;
    let initial_job_id = spdm_poll_result_job_id(&initial_poll_state)
        .expect("the initial polling state should include a job ID");

    // make sure the attestation is in progress
    // device attestations should be created now
    let statuses = list_machines_under_attestation(env)
        .await
        .expect("Didn't find a machine under attestation");

    assert_eq!(1, statuses.len());

    let machine_id = mh.host().id.into();

    // check that attestation's status is InProgress
    assert_eq!(
        rpc::forge::SpdmAttestationStatus::SpdmAttInProgress,
        rpc::forge::SpdmAttestationStatus::try_from(statuses[0].attestation_status)
            .expect("Could not convert attestation status")
    );

    // move the attestation until it reaches the Failed state
    for _ in 0..10 {
        env.run_spdm_controller_iteration_no_requeue().await;
    }

    let attestation_status = fetch_machine_attestation_status(env, machine_id)
        .await
        .expect("Could not get machine attestation status");

    assert_eq!(
        model::attestation::spdm::SpdmAttestationStatus::Failed,
        attestation_status
    );

    // since we set the NRAS verifier mock flag to fail,
    // we expect it to fail now
    for _ in 0..5 {
        env.run_machine_state_controller_iteration().await;
    }

    let host = mh.host().db_machine(txn).await;
    assert!(
        matches!(
            host.current_state(),
            ManagedHostState::Failed {
                details: FailureDetails {
                    cause: model::machine::FailureCause::SpdmAttestationFailed { .. },
                    ..
                },
                ..
            }
        ),
        "Host state is {}",
        host.current_state()
    );

    // "fix" the NRAS verifier mock to succeed
    nras_should_fail_parsing_flag.store(false, Ordering::Relaxed);

    // manually trigger attestation via admin-cli
    let res = env
        .api
        .trigger_machine_attestation(tonic::Request::new(
            rpc::forge::SpdmMachineAttestationTriggerRequest {
                machine_id: Some(host.id.into()),
                redfish_timeout_secs: u32::MAX,
            },
        ))
        .await
        .expect("Failed to trigger SPDM attestation")
        .into_inner();
    assert_eq!(3, res.devices_under_attestation);
    let operator_job_id = uuid::Uuid::try_from(
        res.job_id
            .expect("supported devices should produce an attestation job ID"),
    )
    .expect("attestation job ID should be a UUID");

    // An unfinished operator job must not start managed-host recovery.
    env.run_machine_state_controller_iteration().await;
    assert_eq!(
        mh.host().db_machine(txn).await.current_state(),
        host.current_state()
    );
    let statuses = list_machines_under_attestation(env).await.unwrap();
    assert_eq!(1, statuses.len());
    assert_eq!(
        uuid::Uuid::try_from(statuses[0].job_id.clone().unwrap()).unwrap(),
        operator_job_id
    );

    // Complete the operator job before allowing the machine controller to recover.
    for _ in 0..10 {
        env.run_spdm_controller_iteration_no_requeue().await;
    }
    assert_eq!(
        db::attestation::spdm::get_attestation_job_status(txn, &operator_job_id)
            .await
            .unwrap(),
        SpdmAttestationStatus::Passed
    );

    let recovery_poll_state = env
        .run_machine_state_controller_iteration_until_state_condition(&mh.host().id, 5, |machine| {
            let Some(job_id) = spdm_poll_result_job_id(machine.current_state()) else {
                return false;
            };
            machine.current_state() == &poll_state(job_id)
        })
        .await;
    let recovery_job_id = spdm_poll_result_job_id(&recovery_poll_state)
        .expect("the recovery polling state should include a job ID");
    assert_ne!(recovery_job_id, initial_job_id);
    assert_ne!(recovery_job_id, operator_job_id);
    let recovery_attestations =
        db::attestation::spdm::get_attestations_for_job_id(txn, &recovery_job_id)
            .await
            .unwrap();
    assert_eq!(3, recovery_attestations.len());
    assert!(
        recovery_attestations
            .iter()
            .all(|attestation| attestation.machine_id == machine_id)
    );
    assert_eq!(
        db::attestation::spdm::get_attestation_job_status(txn, &recovery_job_id)
            .await
            .unwrap(),
        SpdmAttestationStatus::InProgress
    );

    // Complete the new job owned by the managed-host state machine.
    for _ in 0..10 {
        env.run_spdm_controller_iteration_no_requeue().await;
    }
    assert_eq!(
        db::attestation::spdm::get_attestation_job_status(txn, &recovery_job_id)
            .await
            .unwrap(),
        SpdmAttestationStatus::Passed
    );
    let attestation_status = fetch_machine_attestation_status(env, machine_id)
        .await
        .expect("Could not get machine attestation status");

    assert_eq!(
        model::attestation::spdm::SpdmAttestationStatus::Passed,
        attestation_status
    );
}

fn spdm_poll_result_job_id(state: &ManagedHostState) -> Option<uuid::Uuid> {
    match state {
        ManagedHostState::HostInit {
            machine_state:
                model::machine::MachineState::SpdmMeasuring {
                    spdm_measuring_state: model::machine::SpdmMeasuringState::PollResult(job_id),
                },
        }
        | ManagedHostState::PreAssignedMeasuring {
            spdm_measuring_state: model::machine::SpdmMeasuringState::PollResult(job_id),
        }
        | ManagedHostState::PostAssignedMeasuring {
            attestation_mode:
                model::machine::AttestationMode::SpdmAttestation {
                    spdm_measuring_state: model::machine::SpdmMeasuringState::PollResult(job_id),
                },
        } => Some(*job_id),
        _ => None,
    }
}

async fn list_machines_under_attestation(
    env: &TestEnv,
) -> Result<Vec<SpdmAttestationJobStatus>, tonic::Status> {
    env.api
        .list_attestation(Request::new(SpdmListAttestationRequest {
            selector: SpdmListAttestationRequestSelector::SpdmListInProgress.into(),
        }))
        .await
        .map(|response| response.into_inner().statuses)
}

async fn fetch_machine_attestation_status(
    env: &TestEnv,
    machine_id: MachineId,
) -> Result<model::attestation::spdm::SpdmAttestationStatus, tonic::Status> {
    let mut txn = env.db_txn().await;
    Ok(
        db::attestation::spdm::list_single_machine_attestation_status(&mut txn, &machine_id, None)
            .await?,
    )
}
