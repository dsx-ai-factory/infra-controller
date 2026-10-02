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
use model::machine::{FailureDetails, ManagedHostState};
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    SpdmAttestationStatus, SpdmListAttestationMachinesRequest,
    SpdmListAttestationMachinesRequestSelector, SpdmMachineAttestationStatus,
    spdm_list_attestation_machines_request,
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
    poll_state: ManagedHostState,
) {
    // first, let's get into a polling state
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        poll_state.clone(),
    )
    .await;

    // make sure the attestation is in progress
    // device attestations should be created now
    let statuses = list_machines_under_attestation(env)
        .await
        .expect("Didn't find a machine under attestation");

    assert_eq!(1, statuses.len());

    let machine_id = statuses[0].machine_id.expect("missing machine id");

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
        rpc::forge::SpdmAttestationStatus::SpdmAttFailed,
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

    // and make sure that the mh state controller has returned to PollResult state
    env.run_machine_state_controller_iteration_until_state_matches(&mh.host().id, 5, poll_state)
        .await;

    // move the spdm state controller until it reaches the Passed state
    for _ in 0..10 {
        env.run_spdm_controller_iteration_no_requeue().await;
    }
    let attestation_status = fetch_machine_attestation_status(env, machine_id)
        .await
        .expect("Could not get machine attestation status");

    assert_eq!(
        rpc::forge::SpdmAttestationStatus::SpdmAttPassed,
        attestation_status
    );
}

async fn list_machines_under_attestation(
    env: &TestEnv,
) -> Result<Vec<SpdmMachineAttestationStatus>, tonic::Status> {
    env.api
        .list_attestation_machines(Request::new(SpdmListAttestationMachinesRequest {
            variant: Some(spdm_list_attestation_machines_request::Variant::Selector(
                SpdmListAttestationMachinesRequestSelector::SpdmListInProgress.into(),
            )),
        }))
        .await
        .map(|response| response.into_inner().statuses)
}

async fn fetch_machine_attestation_status(
    env: &TestEnv,
    machine_id: MachineId,
) -> Result<SpdmAttestationStatus, tonic::Status> {
    let mut statuses = env
        .api
        .list_attestation_machines(Request::new(SpdmListAttestationMachinesRequest {
            variant: Some(spdm_list_attestation_machines_request::Variant::MachineId(
                machine_id,
            )),
        }))
        .await?
        .into_inner()
        .statuses;

    Ok(statuses
        .pop()
        .expect("missing machine attestation status")
        .attestation_status())
}
