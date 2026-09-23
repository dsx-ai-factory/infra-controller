/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::rack::{RackGroupId, RackId, RackProfileId};
use model::rack_type::RackProfile;
use rpc::forge;
use rpc::forge::forge_server::Forge;
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_test_env_with_overrides, get_config,
};

#[crate::sqlx_test()]
async fn expected_rack_derived_profile(pool: sqlx::PgPool) {
    let profile = "GB200_NVL72R1_C2G4_WiWynn_NVIDIA_NO_POWERSHELF";
    let mut config = get_config();
    config
        .rack_profiles
        .rack_profiles
        .insert(profile.into(), RackProfile::default());
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;
    let rack_id: RackId = "rack-01".parse().unwrap();
    let request = forge::ExpectedRack {
        rack_id: Some(rack_id.clone()),
        rack_profile_id: None,
        metadata: None,
    };
    let group = forge::ExpectedRackGroup {
        rack_group_id: Some(RackGroupId::new("group-01")),
        topology: "gb200_nvl72r1_c2g4".into(),
        racks: vec![forge::ExpectedRackGroupRack {
            rack_id: Some(rack_id),
            members: vec![
                forge::ExpectedRackGroupMember {
                    r#type: "Compute".into(),
                    manufacturer: "WiWynn".into(),
                    id: "compute-01".into(),
                },
                forge::ExpectedRackGroupMember {
                    r#type: "Switch".into(),
                    manufacturer: "NVIDIA".into(),
                    id: "switch-01".into(),
                },
            ],
        }],
        metadata: None,
    };
    let missing = env
        .api
        .add_expected_rack(Request::new(request.clone()))
        .await
        .unwrap_err();
    assert_eq!(missing.code(), Code::InvalidArgument);
    env.api
        .add_expected_rack_group(Request::new(group.clone()))
        .await
        .unwrap();
    for supplied in [None, Some(RackProfileId::new("caller-profile-is-ignored"))] {
        let mut declaration = request.clone();
        declaration.rack_profile_id = supplied;
        env.api
            .replace_all_expected_racks(Request::new(forge::ExpectedRackList {
                expected_racks: vec![declaration],
            }))
            .await
            .unwrap();
        let stored = env
            .api
            .get_expected_rack(Request::new(forge::ExpectedRackRequest {
                rack_id: "rack-01".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(stored.rack_profile_id.unwrap().as_str(), profile);
    }
    // A failed replacement must preserve the predecessor declaration.
    let invalid = forge::ExpectedRack {
        rack_id: Some("unknown-rack".parse().unwrap()),
        ..request.clone()
    };
    let err = env
        .api
        .replace_all_expected_racks(Request::new(forge::ExpectedRackList {
            expected_racks: vec![request.clone(), invalid],
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    let rows = env
        .api
        .get_all_expected_racks(Request::new(()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(rows.expected_racks.len(), 1);
    assert_eq!(
        rows.expected_racks[0]
            .rack_profile_id
            .as_ref()
            .unwrap()
            .as_str(),
        profile
    );

    env.api
        .delete_all_expected_racks(Request::new(()))
        .await
        .unwrap();
    env.api
        .add_expected_rack(Request::new(request.clone()))
        .await
        .unwrap();
    let mut duplicate = group.clone();
    duplicate.rack_group_id = Some(RackGroupId::new("group-02"));
    env.api
        .add_expected_rack_group(Request::new(duplicate))
        .await
        .unwrap();
    let err = env
        .api
        .replace_all_expected_racks(Request::new(forge::ExpectedRackList {
            expected_racks: vec![request.clone()],
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);

    // Metadata updates do not rederive a profile from changed group membership.
    let mut update = request;
    update.rack_profile_id = Some(RackProfileId::new("do-not-overwrite"));
    update.metadata = Some(forge::Metadata {
        name: "updated".into(),
        ..Default::default()
    });
    env.api
        .update_expected_rack(Request::new(update))
        .await
        .unwrap();
    let stored = env
        .api
        .get_expected_rack(Request::new(forge::ExpectedRackRequest {
            rack_id: "rack-01".into(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(stored.rack_profile_id.unwrap().as_str(), profile);
    assert_eq!(stored.metadata.unwrap().name, "updated");

    let legacy_id = RackId::new("legacy-rack");
    let mut txn = pool.begin().await.unwrap();
    db::expected_rack::create(
        &mut txn,
        &model::expected_rack::ExpectedRack {
            rack_id: legacy_id.clone(),
            rack_profile_id: RackProfileId::new("legacy-profile"),
            metadata: Default::default(),
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    env.api
        .update_expected_rack(Request::new(forge::ExpectedRack {
            rack_id: Some(legacy_id.clone()),
            rack_profile_id: None,
            metadata: Some(forge::Metadata {
                name: "legacy-updated".into(),
                ..Default::default()
            }),
        }))
        .await
        .unwrap();
    let legacy = env
        .api
        .get_expected_rack(Request::new(forge::ExpectedRackRequest {
            rack_id: legacy_id.to_string(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(legacy.rack_profile_id.unwrap().as_str(), "legacy-profile");
    assert_eq!(legacy.metadata.unwrap().name, "legacy-updated");
}
