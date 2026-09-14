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

use ::rpc::forge as rpc;
use carbide_test_harness::prelude::*;
use model::site_explorer::{EndpointExplorationReport, UNRECOGNIZED_HARDWARE_CLASS};
use tonic::{Code, Request};

fn exact(id: &str) -> rpc::ComponentIdMatch {
    rpc::ComponentIdMatch {
        pattern: Some(rpc::component_id_match::Pattern::Exact(id.to_string())),
    }
}

fn prefix(value: &str) -> rpc::ComponentIdMatch {
    rpc::ComponentIdMatch {
        pattern: Some(rpc::component_id_match::Pattern::Prefix(value.to_string())),
    }
}

fn selection(
    mode: rpc::AttesterSelectionMode,
    component_ids: Vec<rpc::ComponentIdMatch>,
) -> rpc::AttesterSelection {
    rpc::AttesterSelection {
        mode: mode.into(),
        component_ids,
    }
}

fn gpu_allowlist() -> rpc::AttesterSelection {
    selection(
        rpc::AttesterSelectionMode::Allowlist,
        vec![prefix("HGX_IRoT_GPU_")],
    )
}

async fn create(
    env: &TestHarness,
    hardware_class: &str,
    selection: rpc::AttesterSelection,
) -> Result<rpc::AttestationProfile, tonic::Status> {
    env.api()
        .create_attestation_profile(Request::new(rpc::CreateAttestationProfileRequest {
            hardware_class: hardware_class.to_string(),
            selection: Some(selection),
        }))
        .await
        .map(tonic::Response::into_inner)
}

#[sqlx_test]
async fn a_profile_survives_the_round_trip_through_create_get_and_list(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let created = create(&env, "Gb200", gpu_allowlist())
        .await
        .expect("the profile is stored");
    assert_eq!(created.hardware_class, "Gb200");
    assert_eq!(created.selection, Some(gpu_allowlist()));
    assert!(
        !created.version.is_empty(),
        "the response carries the version that if_version_match takes"
    );
    assert!(
        !created.updated_by.is_empty(),
        "the server records who made the change"
    );

    create(
        &env,
        "any",
        selection(rpc::AttesterSelectionMode::All, vec![]),
    )
    .await
    .expect("the reserved any fallback is writable");

    let fetched = env
        .api()
        .get_attestation_profile(Request::new(rpc::GetAttestationProfileRequest {
            hardware_class: "Gb200".to_string(),
        }))
        .await
        .expect("the stored profile reads back")
        .into_inner();
    assert_eq!(fetched, created);

    let listed = env
        .api()
        .list_attestation_profiles(Request::new(()))
        .await
        .expect("profiles list")
        .into_inner();
    let classes: Vec<_> = listed
        .profiles
        .iter()
        .map(|profile| profile.hardware_class.as_str())
        .collect();
    assert_eq!(classes, ["Gb200", "any"]);
}

#[sqlx_test]
async fn update_and_delete_honour_the_version_the_caller_read(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let created = create(&env, "Gb200", gpu_allowlist()).await.unwrap();

    let update = |selection, if_version_match| {
        env.api()
            .update_attestation_profile(Request::new(rpc::UpdateAttestationProfileRequest {
                hardware_class: "Gb200".to_string(),
                selection: Some(selection),
                if_version_match,
            }))
    };

    let denylist = selection(
        rpc::AttesterSelectionMode::Denylist,
        vec![exact("HGX_BMC_0")],
    );
    let updated = update(denylist.clone(), Some(created.version.clone()))
        .await
        .expect("the version the caller read still matches")
        .into_inner();
    assert_eq!(updated.selection, Some(denylist));
    assert_ne!(
        updated.version, created.version,
        "an accepted write moves the version on"
    );

    let stale = update(gpu_allowlist(), Some(created.version.clone()))
        .await
        .expect_err("a stale version must not overwrite a newer policy");
    assert_eq!(stale.code(), Code::FailedPrecondition);

    // Omitting the field is how a caller says "whatever is stored now".
    let unconditional = update(gpu_allowlist(), None)
        .await
        .expect("an omitted version applies to the stored row")
        .into_inner();
    assert_eq!(unconditional.selection, Some(gpu_allowlist()));

    let delete = |if_version_match| {
        env.api()
            .delete_attestation_profile(Request::new(rpc::DeleteAttestationProfileRequest {
                hardware_class: "Gb200".to_string(),
                if_version_match,
            }))
    };

    let stale_delete = delete(Some(created.version.clone()))
        .await
        .expect_err("delete must refuse a stale version too");
    assert_eq!(stale_delete.code(), Code::FailedPrecondition);

    delete(Some(unconditional.version))
        .await
        .expect("the current version deletes");
    let gone = env
        .api()
        .get_attestation_profile(Request::new(rpc::GetAttestationProfileRequest {
            hardware_class: "Gb200".to_string(),
        }))
        .await
        .expect_err("the profile is gone");
    assert_eq!(gone.code(), Code::NotFound);
}

#[sqlx_test]
async fn the_api_refuses_what_section_6_2_forbids(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let already_exists = {
        create(&env, "Gb200", gpu_allowlist()).await.unwrap();
        create(&env, "Gb200", gpu_allowlist())
            .await
            .expect_err("a class holds at most one profile")
    };
    assert_eq!(already_exists.code(), Code::AlreadyExists);

    // The explorer's marker for hardware it could not classify. Such hardware
    // is covered through `any`, so a profile keyed to it would never be read.
    let reserved = create(&env, "unrecognized", gpu_allowlist())
        .await
        .expect_err("the unrecognized marker is not a profile key");
    assert_eq!(reserved.code(), Code::InvalidArgument);

    let empty_class = create(&env, "", gpu_allowlist())
        .await
        .expect_err("an empty class is not a profile key");
    assert_eq!(empty_class.code(), Code::InvalidArgument);

    // Resolution reads the class off the endpoint, so a profile keyed to a
    // class exploration never records could never apply to a machine.
    let misspelled_class = create(&env, "Gb2000", gpu_allowlist())
        .await
        .expect_err("a class no hardware is explored as is not a profile key");
    assert_eq!(misspelled_class.code(), Code::InvalidArgument);

    // An omitted mode decodes to the unset sentinel.
    let no_mode = create(
        &env,
        "DgxGb300",
        selection(
            rpc::AttesterSelectionMode::Unspecified,
            vec![prefix("HGX_IRoT_GPU_")],
        ),
    )
    .await
    .expect_err("there is no safe default mode");
    assert_eq!(no_mode.code(), Code::InvalidArgument);

    let empty_allowlist = create(
        &env,
        "DgxGb300",
        selection(rpc::AttesterSelectionMode::Allowlist, vec![]),
    )
    .await
    .expect_err("an allowlist of nothing can never be satisfied");
    assert_eq!(empty_allowlist.code(), Code::InvalidArgument);

    let all_with_patterns = create(
        &env,
        "DgxGb300",
        selection(rpc::AttesterSelectionMode::All, vec![exact("HGX_BMC_0")]),
    )
    .await
    .expect_err("ALL takes no patterns");
    assert_eq!(all_with_patterns.code(), Code::InvalidArgument);

    let unknown_class = env
        .api()
        .update_attestation_profile(Request::new(rpc::UpdateAttestationProfileRequest {
            hardware_class: "LenovoGb300".to_string(),
            selection: Some(gpu_allowlist()),
            if_version_match: None,
        }))
        .await
        .expect_err("update against a class with no profile is not a create");
    assert_eq!(unknown_class.code(), Code::NotFound);
}

/// Records an explored endpoint carrying `hardware_class`, which is what
/// resolution reads. `None` is an endpoint last explored before the class was
/// recorded at all.
async fn explored(env: &TestHarness, address: &str, hardware_class: Option<&str>) {
    let report = EndpointExplorationReport {
        hardware_class: hardware_class.map(str::to_string),
        ..Default::default()
    };
    let mut txn = env.db_txn().await;
    db::explored_endpoints::insert(address.parse().unwrap(), &report, false, &mut txn)
        .await
        .expect("the endpoint is recorded");
    txn.commit().await.expect("the api can read the endpoint");
}

fn selection_mode(value: Option<i32>) -> Option<rpc::AttesterSelectionMode> {
    value.map(|mode| rpc::AttesterSelectionMode::try_from(mode).expect("a mode this build knows"))
}

/// One row per class, as the class, how many endpoints carry it, what would
/// cover it, and the mode of the profile that would apply. The empty class is
/// the endpoints carrying none.
fn reported(
    response: &rpc::GetAttestationCoverageResponse,
) -> Vec<(
    &str,
    i32,
    rpc::AttestationCoverage,
    Option<rpc::AttesterSelectionMode>,
)> {
    response
        .entries
        .iter()
        .map(|entry| {
            (
                entry.hardware_class.as_str(),
                entry.endpoints,
                entry.coverage(),
                selection_mode(entry.mode),
            )
        })
        .collect()
}

/// What an operator reads before enabling attestation: which profile would
/// supply the policy for each class the site actually has. The server answers
/// from the same resolution scheduling applies to a single machine, so this
/// also pins the rules an operator would otherwise have to infer — a class
/// profile wins over `any`, the `unrecognized` marker falls through to `any`,
/// and an endpoint with no class recorded is covered by nothing at all.
#[sqlx_test]
async fn coverage_reports_what_would_apply_to_each_class_the_site_has(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    explored(&env, "192.0.2.1", Some("Gb200")).await;
    explored(&env, "192.0.2.2", Some("Gb200")).await;
    explored(&env, "192.0.2.3", Some("DgxGb300")).await;
    explored(&env, "192.0.2.4", Some(UNRECOGNIZED_HARDWARE_CLASS)).await;
    explored(&env, "192.0.2.5", None).await;

    create(&env, "Gb200", gpu_allowlist()).await.unwrap();

    let coverage = env
        .api()
        .get_attestation_coverage(Request::new(()))
        .await
        .expect("coverage reads")
        .into_inner();
    assert_eq!(
        reported(&coverage),
        [
            ("DgxGb300", 1, rpc::AttestationCoverage::NoProfile, None),
            (
                "Gb200",
                2,
                rpc::AttestationCoverage::OwnProfile,
                Some(rpc::AttesterSelectionMode::Allowlist)
            ),
            (
                UNRECOGNIZED_HARDWARE_CLASS,
                1,
                rpc::AttestationCoverage::ClassUnrecognized,
                None
            ),
            ("", 1, rpc::AttestationCoverage::ClassNotRecorded, None),
        ]
    );
    assert_eq!(
        selection_mode(coverage.any_profile_mode),
        None,
        "no any profile is stored"
    );

    create(
        &env,
        "any",
        selection(rpc::AttesterSelectionMode::All, vec![]),
    )
    .await
    .unwrap();

    let coverage = env
        .api()
        .get_attestation_coverage(Request::new(()))
        .await
        .expect("coverage reads")
        .into_inner();
    assert_eq!(
        reported(&coverage),
        [
            (
                "DgxGb300",
                1,
                rpc::AttestationCoverage::AnyFallback,
                Some(rpc::AttesterSelectionMode::All)
            ),
            (
                "Gb200",
                2,
                rpc::AttestationCoverage::OwnProfile,
                Some(rpc::AttesterSelectionMode::Allowlist)
            ),
            (
                UNRECOGNIZED_HARDWARE_CLASS,
                1,
                rpc::AttestationCoverage::AnyFallback,
                Some(rpc::AttesterSelectionMode::All)
            ),
            ("", 1, rpc::AttestationCoverage::ClassNotRecorded, None),
        ]
    );
    assert_eq!(
        selection_mode(coverage.any_profile_mode),
        Some(rpc::AttesterSelectionMode::All),
        "the site's posture for everything unprofiled is visible on its own"
    );
}
