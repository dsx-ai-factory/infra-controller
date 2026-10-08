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

use carbide_test_support::Outcome::FailsWith;
use carbide_test_support::{Case, check_cases_async};
use config_version::ConfigVersion;
use model::nic_firmware::NicFirmwareProfileId;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    CreateNicFirmwareProfileRequest, DeleteNicFirmwareProfileRequest,
    FindNicFirmwareProfileIdsRequest, FindNicFirmwareProfilesByIdsRequest, NicFirmwareApproach,
    NicFirmwareArtifact, NicFirmwareProfile, NicFirmwareProfileEntry,
    UpdateNicFirmwareProfileRequest,
};
use rpc::protos::mlx_device::FirmwareSpec;
use tonic::{Code, Request};
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;

use crate::logging::stream::{LogStream, LogStreamLayer};
use crate::tests::common::api_fixtures::create_test_env;

fn profile_config() -> rpc::forge::NicFirmwareProfileConfig {
    rpc::forge::NicFirmwareProfileConfig {
        entries: [
            ("MCX75310AAS-NEAT", "MT_0000000838", "28.43.1014"),
            ("900-9D3B4-00CV-TA0", "MT_0000000884", "32.43.1014"),
        ]
        .into_iter()
        .map(|(part_number, psid, version)| NicFirmwareProfileEntry {
            firmware: Some(FirmwareSpec {
                part_number: part_number.into(),
                psid: psid.into(),
                version: version.into(),
            }),
            image: Some(NicFirmwareArtifact {
                url: format!("https://firmware.invalid/{part_number}/image.bin"),
                sha256: "ab".repeat(32),
            }),
            device_config: Some(NicFirmwareArtifact {
                url: format!("https://firmware.invalid/{part_number}/config.bin"),
                sha256: "cd".repeat(32),
            }),
            approach: NicFirmwareApproach::Scout.into(),
        })
        .collect(),
    }
}

async fn with_redacted_request_log<T>(request: impl Future<Output = T>) -> T {
    let stream = LogStream::new(32, 64 * 1024);
    let subscriber = tracing_subscriber::registry().with(LogStreamLayer::new(stream.clone()));
    let result = async {
        request
            .instrument(tracing::info_span!(
                "nic_firmware_request",
                request = tracing::field::Empty
            ))
            .await
    }
    .with_subscriber(subscriber)
    .await;
    let summaries = stream.latest(32);
    let summary = summaries
        .iter()
        .find(|line| line.level == "SPAN" && line.message == "nic_firmware_request")
        .expect("request span closed");
    let recorded = summary
        .fields
        .get("request")
        .expect("request field recorded");
    assert!(recorded.contains("config_present:"));
    assert!(!recorded.contains("firmware.invalid"));
    result
}

#[crate::sqlx_test]
async fn profile_crud_persists_complete_definitions_and_orders_reads(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let config = profile_config();
    let mut created = Vec::new();
    for id in ["z-profile", "A-profile"] {
        let profile = env
            .api
            .create_nic_firmware_profile(Request::new(CreateNicFirmwareProfileRequest {
                id: id.into(),
                config: Some(config.clone()),
            }))
            .await
            .unwrap()
            .into_inner()
            .profile
            .unwrap();
        assert_eq!(profile.id, id);
        assert_eq!(profile.config, Some(config.clone()));
        assert_eq!(
            profile
                .version
                .parse::<ConfigVersion>()
                .unwrap()
                .version_nr(),
            1
        );
        created.push(profile);
    }

    let ids = env
        .api
        .find_nic_firmware_profile_ids(Request::new(FindNicFirmwareProfileIdsRequest {}))
        .await
        .unwrap()
        .into_inner()
        .profile_ids;
    assert_eq!(ids, ["A-profile", "z-profile"]);
    let found = env
        .api
        .find_nic_firmware_profiles_by_ids(Request::new(FindNicFirmwareProfilesByIdsRequest {
            profile_ids: vec![
                "z-profile".into(),
                "unknown".into(),
                "A-profile".into(),
                "z-profile".into(),
            ],
        }))
        .await
        .unwrap()
        .into_inner()
        .profiles;
    assert_eq!(found, [created[1].clone(), created[0].clone()]);

    let id: NicFirmwareProfileId = "z-profile".parse().unwrap();
    let mut replacement = config;
    replacement.entries.remove(0);
    replacement.entries[0].device_config = None;
    let updated = with_redacted_request_log(env.api.update_nic_firmware_profile(Request::new(
        UpdateNicFirmwareProfileRequest {
            id: id.to_string(),
            config: Some(replacement.clone()),
            if_version_match: Some(created[0].version.clone()),
        },
    )))
    .await
    .unwrap()
    .into_inner()
    .profile
    .unwrap();
    assert_eq!(updated.config, Some(replacement));
    assert_eq!(
        updated
            .version
            .parse::<ConfigVersion>()
            .unwrap()
            .version_nr(),
        2
    );
    let stored = db::nic_firmware::find_by_ids(&env.pool, std::slice::from_ref(&id))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(NicFirmwareProfile::from(stored), updated);

    env.api
        .delete_nic_firmware_profile(Request::new(DeleteNicFirmwareProfileRequest {
            id: id.to_string(),
            if_version_match: Some(updated.version),
        }))
        .await
        .unwrap();
    assert!(
        db::nic_firmware::find_by_ids(&env.pool, std::slice::from_ref(&id))
            .await
            .unwrap()
            .is_empty()
    );
}

#[crate::sqlx_test]
async fn profile_requests_report_semantic_errors_without_changing_storage(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let api = &env.api;
    check_cases_async(
        [
            Case {
                scenario: "invalid ID",
                input: CreateNicFirmwareProfileRequest {
                    id: "".into(),
                    config: Some(profile_config()),
                },
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "missing config",
                input: CreateNicFirmwareProfileRequest {
                    id: "profile".into(),
                    config: None,
                },
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "invalid definition",
                input: CreateNicFirmwareProfileRequest {
                    id: "profile".into(),
                    config: Some({
                        let mut config = profile_config();
                        config.entries[0]
                            .image
                            .as_mut()
                            .unwrap()
                            .url
                            .push_str("?token=private");
                        config
                    }),
                },
                expect: FailsWith(Code::InvalidArgument),
            },
        ],
        |request| async move {
            with_redacted_request_log(api.create_nic_firmware_profile(Request::new(request)))
                .await
                .map(drop)
                .map_err(|error| error.code())
        },
    )
    .await;
    assert!(
        db::nic_firmware::find_ids(&env.pool)
            .await
            .unwrap()
            .is_empty()
    );

    let request = CreateNicFirmwareProfileRequest {
        id: "profile".into(),
        config: Some(profile_config()),
    };
    let created = env
        .api
        .create_nic_firmware_profile(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner()
        .profile
        .unwrap();
    let error = env
        .api
        .create_nic_firmware_profile(Request::new(request))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::AlreadyExists);

    let version = created.version.parse::<ConfigVersion>().unwrap();
    let wrong_timestamp = format!(
        "V{}-T{}",
        version.version_nr(),
        version.timestamp().timestamp_micros() + 1
    );
    check_cases_async(
        [
            Case {
                scenario: "missing config",
                input: UpdateNicFirmwareProfileRequest {
                    id: "profile".into(),
                    config: None,
                    if_version_match: Some(created.version.clone()),
                },
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "missing version",
                input: UpdateNicFirmwareProfileRequest {
                    id: "profile".into(),
                    config: Some(profile_config()),
                    if_version_match: None,
                },
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "invalid version",
                input: UpdateNicFirmwareProfileRequest {
                    id: "profile".into(),
                    config: Some(profile_config()),
                    if_version_match: Some("bad".into()),
                },
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "missing profile",
                input: UpdateNicFirmwareProfileRequest {
                    id: "unknown".into(),
                    config: Some(profile_config()),
                    if_version_match: Some(created.version.clone()),
                },
                expect: FailsWith(Code::NotFound),
            },
            Case {
                scenario: "same counter with different timestamp",
                input: UpdateNicFirmwareProfileRequest {
                    id: "profile".into(),
                    config: Some(profile_config()),
                    if_version_match: Some(wrong_timestamp.clone()),
                },
                expect: FailsWith(Code::FailedPrecondition),
            },
        ],
        |request| async move {
            api.update_nic_firmware_profile(Request::new(request))
                .await
                .map(drop)
                .map_err(|error| error.code())
        },
    )
    .await;
    let error = env
        .api
        .delete_nic_firmware_profile(Request::new(DeleteNicFirmwareProfileRequest {
            id: "profile".into(),
            if_version_match: Some(wrong_timestamp),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    let error = env
        .api
        .delete_nic_firmware_profile(Request::new(DeleteNicFirmwareProfileRequest {
            id: "unknown".into(),
            if_version_match: Some(created.version.clone()),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::NotFound);

    let stored = db::nic_firmware::find_by_ids(&env.pool, &["profile".parse().unwrap()])
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(NicFirmwareProfile::from(stored), created);
}

#[crate::sqlx_test]
async fn profile_fetch_requires_a_nonempty_bounded_batch(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let api = &env.api;
    check_cases_async(
        [
            Case {
                scenario: "empty batch",
                input: FindNicFirmwareProfilesByIdsRequest {
                    profile_ids: vec![],
                },
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "over limit",
                input: FindNicFirmwareProfilesByIdsRequest {
                    profile_ids: vec!["profile".into(); env.config.max_find_by_ids as usize + 1],
                },
                expect: FailsWith(Code::InvalidArgument),
            },
        ],
        |request| async move {
            api.find_nic_firmware_profiles_by_ids(Request::new(request))
                .await
                .map(drop)
                .map_err(|error| error.code())
        },
    )
    .await;
}
