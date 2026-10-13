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

use std::time::Duration;

use carbide_test_support::Outcome::FailsWith;
use carbide_test_support::{Case, check_cases_async};
use config_version::ConfigVersion;
use futures::{FutureExt, StreamExt};
use model::nic_firmware::NicFirmwareProfileId;
use rpc::forge::forge_server::Forge;
use rpc::forge::nic_firmware_device_target::Result as TargetResult;
use rpc::forge::set_machine_nic_firmware_override_request::Action as MachineAction;
use rpc::forge::set_nic_firmware_site_default_request::Action;
use rpc::forge::{
    CreateNicFirmwareProfileRequest, DeleteNicFirmwareProfileRequest,
    FindNicFirmwareProfileIdsRequest, FindNicFirmwareProfilesByIdsRequest,
    FindNicFirmwareSiteDefaultsByIdsRequest, GetDesiredNicFirmwareRequest, NicFirmwareApproach,
    NicFirmwareArtifact, NicFirmwareHardware, NicFirmwareProfile, NicFirmwareProfileEntry,
    NicFirmwareSiteDefaultSearchFilter, SetMachineNicFirmwareOverrideRequest,
    SetNicFirmwareSiteDefaultRequest, UpdateNicFirmwareProfileRequest,
};
use rpc::protos::mlx_device::FirmwareSpec;
use tonic::{Code, Request};
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;

use crate::logging::stream::{LogStream, LogStreamLayer};
use crate::tests::common::api_fixtures::create_test_env;
use crate::tests::mlx_device_report::{live_report, scout_connection};

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

#[crate::sqlx_test]
async fn site_defaults_validate_compatibility_and_assignment_versions(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    for id in ["z-profile", "a-profile", "incompatible"] {
        let mut config = profile_config();
        if id == "incompatible" {
            config.entries.remove(0);
        }
        env.api
            .create_nic_firmware_profile(Request::new(CreateNicFirmwareProfileRequest {
                id: id.into(),
                config: Some(config),
            }))
            .await
            .unwrap();
    }
    let hardware = NicFirmwareHardware {
        part_number: "MCX75310AAS-NEAT".into(),
        psid: "MT_0000000838".into(),
    };
    let ids = env
        .api
        .find_nic_firmware_profile_ids(Request::new(FindNicFirmwareProfileIdsRequest {
            hardware: Some(hardware.clone()),
        }))
        .await
        .unwrap()
        .into_inner()
        .profile_ids;
    assert_eq!(ids, ["a-profile", "z-profile"]);
    assert!(
        env.api
            .find_nic_firmware_profile_ids(Request::new(FindNicFirmwareProfileIdsRequest {
                hardware: Some(NicFirmwareHardware {
                    part_number: "unknown".into(),
                    psid: hardware.psid.clone(),
                }),
            }))
            .await
            .unwrap()
            .into_inner()
            .profile_ids
            .is_empty()
    );
    let invalid_filter = FindNicFirmwareProfileIdsRequest {
        hardware: Some(NicFirmwareHardware {
            part_number: "界".repeat(rpc::MAX_ERR_MSG_SIZE as usize),
            psid: hardware.psid.clone(),
        }),
    };
    // Without escaping, request logging would cut through a UTF-8 character.
    assert!(!format!("{invalid_filter:?}").is_char_boundary(rpc::MAX_ERR_MSG_SIZE as usize));
    let error = env
        .api
        .find_nic_firmware_profile_ids(Request::new(invalid_filter))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(error.metadata().get("nico-error-text").is_some());
    let find_request = FindNicFirmwareSiteDefaultsByIdsRequest {
        hardware: vec![hardware.clone()],
    };
    let mut request = SetNicFirmwareSiteDefaultRequest {
        hardware: Some(hardware),
        action: Some(Action::ProfileId("z-profile".into())),
        if_version_match: None,
    };
    let created = env
        .api
        .set_nic_firmware_site_default(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner()
        .site_default
        .unwrap();
    assert_eq!(created.hardware, request.hardware);
    assert_eq!(created.profile_id, "z-profile");
    assert_eq!(
        env.api
            .set_nic_firmware_site_default(Request::new(request.clone()))
            .await
            .unwrap_err()
            .code(),
        Code::AlreadyExists
    );
    request.if_version_match = Some(created.version.clone());
    request.action = Some(Action::ProfileId("a-profile".into()));
    let updated = env
        .api
        .set_nic_firmware_site_default(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner()
        .site_default
        .unwrap();
    assert_ne!(updated.version, created.version);
    assert_eq!(updated.profile_id, "a-profile");
    assert_eq!(updated.hardware, request.hardware);
    let filtered_hardware = env
        .api
        .find_nic_firmware_site_default_ids(Request::new(NicFirmwareSiteDefaultSearchFilter {
            profile_id: Some("a-profile".into()),
        }))
        .await
        .unwrap()
        .into_inner()
        .hardware;
    assert_eq!(filtered_hardware, find_request.hardware);
    assert!(
        env.api
            .find_nic_firmware_site_default_ids(Request::new(NicFirmwareSiteDefaultSearchFilter {
                profile_id: Some("z-profile".into()),
            }))
            .await
            .unwrap()
            .into_inner()
            .hardware
            .is_empty()
    );
    for (scenario, input, expected) in [
        ("stale replace", request.clone(), Code::FailedPrecondition),
        (
            "missing profile precedes stale assignment token",
            SetNicFirmwareSiteDefaultRequest {
                action: Some(Action::ProfileId("missing".into())),
                ..request.clone()
            },
            Code::NotFound,
        ),
        (
            "stale clear",
            SetNicFirmwareSiteDefaultRequest {
                action: Some(Action::Clear(())),
                ..request.clone()
            },
            Code::FailedPrecondition,
        ),
        (
            "unsupported pair",
            SetNicFirmwareSiteDefaultRequest {
                action: Some(Action::ProfileId("incompatible".into())),
                if_version_match: Some(updated.version.clone()),
                ..request.clone()
            },
            Code::FailedPrecondition,
        ),
        (
            "clear needs token",
            SetNicFirmwareSiteDefaultRequest {
                action: Some(Action::Clear(())),
                if_version_match: None,
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "missing action does not clear",
            SetNicFirmwareSiteDefaultRequest {
                action: None,
                if_version_match: Some(updated.version.clone()),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "missing hardware",
            SetNicFirmwareSiteDefaultRequest {
                hardware: None,
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "empty profile ID does not clear",
            SetNicFirmwareSiteDefaultRequest {
                action: Some(Action::ProfileId(String::new())),
                if_version_match: Some(updated.version.clone()),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "malformed token does not create",
            SetNicFirmwareSiteDefaultRequest {
                if_version_match: Some("bad".into()),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "oversized Unicode selector is rejected without a log truncation panic",
            SetNicFirmwareSiteDefaultRequest {
                hardware: Some(NicFirmwareHardware {
                    part_number: "🦀".repeat(1500),
                    psid: "PSID1".into(),
                }),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
    ] {
        assert_eq!(
            env.api
                .set_nic_firmware_site_default(Request::new(input))
                .await
                .unwrap_err()
                .code(),
            expected,
            "{scenario}"
        );
        assert_eq!(
            env.api
                .find_nic_firmware_site_defaults_by_ids(Request::new(find_request.clone()))
                .await
                .unwrap()
                .into_inner()
                .site_defaults,
            std::slice::from_ref(&updated),
            "{scenario}"
        );
    }
    request.if_version_match = Some(updated.version);
    request.action = Some(Action::Clear(()));
    assert!(
        env.api
            .set_nic_firmware_site_default(Request::new(request.clone()))
            .await
            .unwrap()
            .into_inner()
            .site_default
            .is_none()
    );
    assert!(
        env.api
            .find_nic_firmware_site_defaults_by_ids(Request::new(find_request.clone()))
            .await
            .unwrap()
            .into_inner()
            .site_defaults
            .is_empty()
    );
    assert_eq!(
        env.api
            .set_nic_firmware_site_default(Request::new(request.clone()))
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert_eq!(
        env.api
            .set_nic_firmware_site_default(Request::new(SetNicFirmwareSiteDefaultRequest {
                action: Some(Action::ProfileId("a-profile".into())),
                ..request.clone()
            }))
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert!(
        env.api
            .find_nic_firmware_site_default_ids(Request::new(
                NicFirmwareSiteDefaultSearchFilter::default(),
            ))
            .await
            .unwrap()
            .into_inner()
            .hardware
            .is_empty()
    );
    let recreated = env
        .api
        .set_nic_firmware_site_default(Request::new(SetNicFirmwareSiteDefaultRequest {
            action: Some(Action::ProfileId("a-profile".into())),
            if_version_match: None,
            ..request.clone()
        }))
        .await
        .unwrap()
        .into_inner()
        .site_default
        .unwrap();
    assert_ne!(recreated.version, created.version);
    assert_eq!(
        recreated
            .version
            .parse::<ConfigVersion>()
            .unwrap()
            .version_nr(),
        created
            .version
            .parse::<ConfigVersion>()
            .unwrap()
            .version_nr()
    );
    request.if_version_match = Some(created.version);
    for action in [Action::Clear(()), Action::ProfileId("z-profile".into())] {
        request.action = Some(action);
        assert_eq!(
            env.api
                .set_nic_firmware_site_default(Request::new(request.clone()))
                .await
                .unwrap_err()
                .code(),
            Code::FailedPrecondition
        );
    }
    assert_eq!(
        env.api
            .find_nic_firmware_site_defaults_by_ids(Request::new(find_request))
            .await
            .unwrap()
            .into_inner()
            .site_defaults,
        std::slice::from_ref(&recreated)
    );
}

#[crate::sqlx_test]
async fn profile_references_reject_changes_until_defaults_are_cleared(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mut config = profile_config();
    let profile = env
        .api
        .create_nic_firmware_profile(Request::new(CreateNicFirmwareProfileRequest {
            id: "baseline".into(),
            config: Some(config.clone()),
        }))
        .await
        .unwrap()
        .into_inner()
        .profile
        .unwrap();
    let mut bindings = Vec::new();
    for entry in config.entries.iter().rev() {
        let firmware = entry.firmware.as_ref().unwrap();
        bindings.push(
            env.api
                .set_nic_firmware_site_default(Request::new(SetNicFirmwareSiteDefaultRequest {
                    hardware: Some(NicFirmwareHardware {
                        part_number: firmware.part_number.clone(),
                        psid: firmware.psid.clone(),
                    }),
                    action: Some(Action::ProfileId(profile.id.clone())),
                    if_version_match: None,
                }))
                .await
                .unwrap()
                .into_inner()
                .site_default
                .unwrap(),
        );
    }
    let listed = env
        .api
        .find_nic_firmware_site_default_ids(Request::new(
            NicFirmwareSiteDefaultSearchFilter::default(),
        ))
        .await
        .unwrap()
        .into_inner()
        .hardware;
    assert_eq!(
        listed,
        bindings
            .iter()
            .map(|binding| binding.hardware.clone().unwrap())
            .collect::<Vec<_>>()
    );
    let details = FindNicFirmwareSiteDefaultsByIdsRequest { hardware: listed };
    let delete = DeleteNicFirmwareProfileRequest {
        id: profile.id.clone(),
        if_version_match: Some(profile.version.clone()),
    };
    config.entries.remove(0);
    for delete_profile in [false, true] {
        let error = if delete_profile {
            env.api
                .delete_nic_firmware_profile(Request::new(delete.clone()))
                .await
                .unwrap_err()
        } else {
            env.api
                .update_nic_firmware_profile(Request::new(UpdateNicFirmwareProfileRequest {
                    id: profile.id.clone(),
                    config: Some(config.clone()),
                    if_version_match: Some(profile.version.clone()),
                }))
                .await
                .unwrap_err()
        };
        assert_eq!(error.code(), Code::FailedPrecondition);
        assert_eq!(
            env.api
                .find_nic_firmware_profiles_by_ids(Request::new(
                    FindNicFirmwareProfilesByIdsRequest {
                        profile_ids: vec![profile.id.clone()],
                    }
                ))
                .await
                .unwrap()
                .into_inner()
                .profiles,
            std::slice::from_ref(&profile)
        );
        assert_eq!(
            env.api
                .find_nic_firmware_site_defaults_by_ids(Request::new(details.clone()))
                .await
                .unwrap()
                .into_inner()
                .site_defaults,
            bindings
        );
    }
    for (index, binding) in bindings.iter().enumerate() {
        env.api
            .set_nic_firmware_site_default(Request::new(SetNicFirmwareSiteDefaultRequest {
                hardware: binding.hardware.clone(),
                action: Some(Action::Clear(())),
                if_version_match: Some(binding.version.clone()),
            }))
            .await
            .unwrap();
        // Enumeration and detail reads are separate calls; cleared keys must not
        // become placeholders when a client fetches its previously listed keys.
        assert_eq!(
            env.api
                .find_nic_firmware_site_defaults_by_ids(Request::new(details.clone()))
                .await
                .unwrap()
                .into_inner()
                .site_defaults,
            bindings[index + 1..]
        );
    }
    env.api
        .delete_nic_firmware_profile(Request::new(delete))
        .await
        .unwrap();
    assert!(
        env.api
            .find_nic_firmware_profiles_by_ids(Request::new(FindNicFirmwareProfilesByIdsRequest {
                profile_ids: vec![profile.id],
            }))
            .await
            .unwrap()
            .into_inner()
            .profiles
            .is_empty()
    );
}

#[crate::sqlx_test]
async fn site_default_reads_validate_filters_and_batch_bounds(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let api = &env.api;
    assert_eq!(
        api.find_nic_firmware_site_default_ids(Request::new(NicFirmwareSiteDefaultSearchFilter {
            profile_id: Some(String::new()),
        }))
        .await
        .unwrap_err()
        .code(),
        Code::InvalidArgument
    );
    let hardware = NicFirmwareHardware {
        part_number: "MCX75310AAS-NEAT".into(),
        psid: "MT_0000000838".into(),
    };
    api.create_nic_firmware_profile(Request::new(CreateNicFirmwareProfileRequest {
        id: "baseline".into(),
        config: Some(profile_config()),
    }))
    .await
    .unwrap();
    let binding = api
        .set_nic_firmware_site_default(Request::new(SetNicFirmwareSiteDefaultRequest {
            hardware: Some(hardware.clone()),
            action: Some(Action::ProfileId("baseline".into())),
            if_version_match: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .site_default
        .unwrap();
    let limit = env.config.max_find_by_ids as usize;
    env.api
        .find_nic_firmware_site_defaults_by_ids(Request::new(
            FindNicFirmwareSiteDefaultsByIdsRequest {
                hardware: vec![hardware.clone(); limit],
            },
        ))
        .await
        .expect("a batch at the configured limit must be accepted");
    check_cases_async(
        [
            Case {
                scenario: "empty batch",
                input: vec![],
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "over limit before deduplication",
                input: vec![hardware.clone(); limit + 1],
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "invalid member rejects the whole populated batch",
                input: vec![
                    hardware.clone(),
                    NicFirmwareHardware {
                        part_number: String::new(),
                        ..hardware.clone()
                    },
                ],
                expect: FailsWith(Code::InvalidArgument),
            },
        ],
        |hardware| async move {
            api.find_nic_firmware_site_defaults_by_ids(Request::new(
                FindNicFirmwareSiteDefaultsByIdsRequest { hardware },
            ))
            .await
            .map(drop)
            .map_err(|error| error.code())
        },
    )
    .await;
    assert_eq!(
        api.find_nic_firmware_site_defaults_by_ids(Request::new(
            FindNicFirmwareSiteDefaultsByIdsRequest {
                hardware: vec![hardware],
            }
        ))
        .await
        .unwrap()
        .into_inner()
        .site_defaults,
        [binding]
    );
}

// The public write/read path must preserve operator intent as Scout reports
// change, while clearing the override restores site inheritance.
#[crate::sqlx_test]
async fn machine_override_is_exposed_resolved_and_cleared_with_changing_inventory(
    pool: sqlx::PgPool,
) {
    use model::machine::status::MlxDeviceObservation;
    use rpc::forge::NicFirmwareSelectionSource;
    use rpc::protos::mlx_device::MlxDeviceInfo;

    use crate::tests::common::api_fixtures::create_managed_host;

    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let before = host.host().rpc_machine().await;
    let snapshot = db::machine::find_one(&env.pool, &host.id, Default::default())
        .await
        .unwrap()
        .unwrap();
    let managed = snapshot
        .status
        .interfaces
        .iter()
        .find(|interface| interface.attached_dpu_machine_id.is_some())
        .unwrap();
    let baseline = profile_config();
    let mut override_config = baseline.clone();
    for entry in &mut override_config.entries {
        entry.firmware.as_mut().unwrap().version = "override-target".into();
    }
    // Matching the first device is insufficient: both usable pairs constrain
    // the assignment.
    let mut incompatible = override_config.clone();
    incompatible.entries.truncate(1);
    for (id, config) in [
        ("baseline", baseline.clone()),
        ("override", override_config.clone()),
        ("incompatible", incompatible),
    ] {
        env.api
            .create_nic_firmware_profile(Request::new(CreateNicFirmwareProfileRequest {
                id: id.into(),
                config: Some(config),
            }))
            .await
            .unwrap();
    }
    let mut devices = Vec::new();
    for (index, entry) in baseline.entries.iter().enumerate() {
        let firmware = entry.firmware.as_ref().unwrap();
        env.api
            .set_nic_firmware_site_default(Request::new(SetNicFirmwareSiteDefaultRequest {
                hardware: Some(NicFirmwareHardware {
                    part_number: firmware.part_number.clone(),
                    psid: firmware.psid.clone(),
                }),
                action: Some(Action::ProfileId("baseline".into())),
                if_version_match: None,
            }))
            .await
            .unwrap();
        devices.push(MlxDeviceInfo {
            pci_name: format!("0{index}:00.0"),
            part_number: firmware.part_number.clone(),
            psid: firmware.psid.clone(),
            ..Default::default()
        });
    }
    // Managed-DPU hardware and missing or invalid facts must not block the
    // assignment. Desired reads still need to report each device's error.
    devices.extend([
        MlxDeviceInfo {
            pci_name: "02:00.0".into(),
            part_number: "managed-DPU-only".into(),
            psid: "managed-DPU-only".into(),
            base_mac: managed.mac_address.to_string(),
            ..Default::default()
        },
        MlxDeviceInfo {
            pci_name: "03:00.0".into(),
            part_number: "incomplete".into(),
            ..Default::default()
        },
        MlxDeviceInfo {
            pci_name: "04:00.0".into(),
            part_number: " padded ".into(),
            psid: "unusable-pair".into(),
            ..Default::default()
        },
    ]);
    let mut observation = MlxDeviceObservation {
        observed_at: "2026-10-12T00:00:00Z".parse().unwrap(),
        devices: devices
            .into_iter()
            .map(|device| device.try_into().unwrap())
            .collect(),
    };
    let mut connection = env.pool.acquire().await.unwrap();
    assert_eq!(
        db::machine::update_mlx_device_observation(&mut connection, &host.id, &observation)
            .await
            .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    drop(connection);

    env.api
        .set_machine_nic_firmware_override(Request::new(SetMachineNicFirmwareOverrideRequest {
            machine_id: Some(host.id.into()),
            if_version_match: Some(before.version.clone()),
            action: Some(MachineAction::ProfileId("override".into())),
        }))
        .await
        .unwrap();
    let configured = host.host().rpc_machine().await;
    assert_eq!(
        configured
            .config
            .as_ref()
            .unwrap()
            .nic_firmware_profile_id
            .as_deref(),
        Some("override")
    );
    assert_ne!(configured.version, before.version);
    assert_eq!(configured.state, before.state);
    assert_eq!(configured.state_version, before.state_version);
    let request = GetDesiredNicFirmwareRequest {
        machine_id: Some(host.id.into()),
        allocation_profile_id: None,
    };
    let desired = env
        .api
        .get_desired_nic_firmware(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(desired.targets.len(), 5);
    for (target, entry) in desired.targets[..2].iter().zip(&override_config.entries) {
        let Some(TargetResult::Firmware(resolved)) = &target.result else {
            panic!("expected machine override: {target:?}");
        };
        assert_eq!(
            resolved.source,
            NicFirmwareSelectionSource::MachineOverride as i32
        );
        assert_eq!(resolved.profile_id, "override");
        assert_eq!(resolved.entry.as_ref(), Some(entry));
    }
    assert!(
        matches!(&desired.targets[2].result, Some(TargetResult::Error(error)) if error.contains("managed-DPU"))
    );
    assert_eq!(
        desired.targets[2]
            .identity
            .as_ref()
            .unwrap()
            .managed_dpu_machine_ids,
        [managed.attached_dpu_machine_id.unwrap().into()]
    );
    assert!(
        matches!(&desired.targets[3].result, Some(TargetResult::Error(error)) if error.contains("missing a PSID"))
    );
    assert!(
        matches!(&desired.targets[4].result, Some(TargetResult::Error(error)) if error.contains("surrounding whitespace"))
    );

    let Some(TargetResult::Firmware(resolved)) = &desired.targets[0].result else {
        panic!("expected machine override: {:?}", desired.targets[0]);
    };
    override_config.entries[0]
        .firmware
        .as_mut()
        .unwrap()
        .version = "updated-override-target".into();
    let updated = env
        .api
        .update_nic_firmware_profile(Request::new(UpdateNicFirmwareProfileRequest {
            id: "override".into(),
            config: Some(override_config.clone()),
            if_version_match: Some(resolved.profile_version.clone()),
        }))
        .await
        .unwrap()
        .into_inner()
        .profile
        .unwrap();
    assert_ne!(updated.version, resolved.profile_version);
    let desired = env
        .api
        .get_desired_nic_firmware(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    for (target, entry) in desired.targets[..2].iter().zip(&override_config.entries) {
        let Some(TargetResult::Firmware(resolved)) = &target.result else {
            panic!("expected updated machine override: {target:?}");
        };
        assert_eq!(
            resolved.source,
            NicFirmwareSelectionSource::MachineOverride as i32
        );
        assert_eq!(resolved.profile_id, "override");
        assert_eq!(resolved.profile_version, updated.version);
        assert_eq!(resolved.entry.as_ref(), Some(entry));
    }
    let after_profile_edit = host.host().rpc_machine().await;
    assert_eq!(after_profile_edit.config, configured.config);
    assert_eq!(after_profile_edit.version, configured.version);

    let allocation_conflict = env
        .api
        .get_desired_nic_firmware(Request::new(GetDesiredNicFirmwareRequest {
            allocation_profile_id: Some("baseline".into()),
            ..request.clone()
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        matches!(&allocation_conflict.targets[0].result, Some(TargetResult::Error(error)) if error.contains("conflicts with the allocation requirement"))
    );

    let error = env
        .api
        .set_machine_nic_firmware_override(Request::new(SetMachineNicFirmwareOverrideRequest {
            machine_id: Some(host.id.into()),
            if_version_match: Some(configured.version.clone()),
            action: Some(MachineAction::ProfileId("incompatible".into())),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    let rejected = host.host().rpc_machine().await;
    assert_eq!(rejected.config, configured.config);
    assert_eq!(rejected.version, configured.version);

    // A later report can introduce incompatible hardware. Keep the saved
    // selection and expose the mismatch until an operator changes it.
    observation.observed_at = "2026-10-12T00:01:00Z".parse().unwrap();
    observation.devices[0].part_number = Some("new-hardware".into());
    let mut connection = env.pool.acquire().await.unwrap();
    assert_eq!(
        db::machine::update_mlx_device_observation(&mut connection, &host.id, &observation)
            .await
            .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    drop(connection);
    let changed = env
        .api
        .get_desired_nic_firmware(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    assert!(
        matches!(&changed.targets[0].result, Some(TargetResult::Error(error)) if error.contains("does not support"))
    );
    assert_eq!(changed.targets[1..], desired.targets[1..]);
    let still_configured = host.host().rpc_machine().await;
    assert_eq!(still_configured.config, configured.config);
    assert_eq!(still_configured.version, configured.version);

    // Clearing remains available even after inventory becomes incompatible.
    env.api
        .set_machine_nic_firmware_override(Request::new(SetMachineNicFirmwareOverrideRequest {
            machine_id: Some(host.id.into()),
            if_version_match: Some(configured.version.clone()),
            action: Some(MachineAction::Clear(())),
        }))
        .await
        .unwrap();
    let cleared = host.host().rpc_machine().await;
    assert_eq!(cleared.config, before.config);
    assert_ne!(cleared.version, configured.version);
    assert_eq!(cleared.state_version, before.state_version);
    let inherited = env
        .api
        .get_desired_nic_firmware(Request::new(request))
        .await
        .unwrap()
        .into_inner();
    assert!(inherited.targets[0].result.is_none());
    let Some(TargetResult::Firmware(resolved)) = &inherited.targets[1].result else {
        panic!("expected restored site default: {:?}", inherited.targets[1]);
    };
    assert_eq!(
        resolved.source,
        NicFirmwareSelectionSource::SiteDefault as i32
    );
    assert_eq!(resolved.profile_id, "baseline");
    assert_eq!(resolved.entry.as_ref(), Some(&baseline.entries[1]));
    assert_eq!(inherited.targets[2..], desired.targets[2..]);
}

// An error status alone would miss a partially applied mutation. Rejected
// requests must preserve both the stored selection and its edit token.
#[crate::sqlx_test]
async fn machine_override_rejects_invalid_requests_without_changing_configuration(
    pool: sqlx::PgPool,
) {
    use crate::tests::common::api_fixtures::create_managed_host;

    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let before = host.host().rpc_machine().await;
    env.api
        .create_nic_firmware_profile(Request::new(CreateNicFirmwareProfileRequest {
            id: "override".into(),
            config: Some(profile_config()),
        }))
        .await
        .unwrap();
    // Start with a saved selection so an accidental clear cannot look like
    // preservation of an already-empty setting.
    let mut request = SetMachineNicFirmwareOverrideRequest {
        machine_id: Some(host.id.into()),
        if_version_match: Some(before.version.clone()),
        action: Some(MachineAction::ProfileId("override".into())),
    };
    env.api
        .set_machine_nic_firmware_override(Request::new(request.clone()))
        .await
        .unwrap();
    let configured = host.host().rpc_machine().await;
    request.if_version_match = Some(configured.version.clone());
    let version = configured.version.parse::<ConfigVersion>().unwrap();
    let wrong_timestamp = format!(
        "V{}-T{}",
        version.version_nr(),
        version.timestamp().timestamp_micros() + 1
    );
    let invalid_profile = SetMachineNicFirmwareOverrideRequest {
        action: Some(MachineAction::ProfileId(format!(
            "{} ",
            "界".repeat(rpc::MAX_ERR_MSG_SIZE as usize)
        ))),
        ..request.clone()
    };
    // Without escaping, request logging would cut through a UTF-8 character.
    assert!(!format!("{invalid_profile:?}").is_char_boundary(rpc::MAX_ERR_MSG_SIZE as usize));
    let cases = [
        (
            "missing host",
            SetMachineNicFirmwareOverrideRequest {
                machine_id: None,
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "DPU is not a host",
            SetMachineNicFirmwareOverrideRequest {
                machine_id: Some(
                    "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
                        .parse()
                        .unwrap(),
                ),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "missing token",
            SetMachineNicFirmwareOverrideRequest {
                if_version_match: None,
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "malformed token",
            SetMachineNicFirmwareOverrideRequest {
                if_version_match: Some("V1".into()),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "missing action",
            SetMachineNicFirmwareOverrideRequest {
                action: None,
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "empty profile ID does not clear",
            SetMachineNicFirmwareOverrideRequest {
                action: Some(MachineAction::ProfileId(String::new())),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "malformed profile",
            SetMachineNicFirmwareOverrideRequest {
                action: Some(MachineAction::ProfileId(" ".into())),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "Unicode profile ID with trailing whitespace",
            invalid_profile,
            Code::InvalidArgument,
        ),
        (
            "missing profile precedes stale machine token",
            SetMachineNicFirmwareOverrideRequest {
                action: Some(MachineAction::ProfileId("missing".into())),
                if_version_match: Some(before.version.clone()),
                ..request.clone()
            },
            Code::NotFound,
        ),
        (
            "same counter with different timestamp",
            SetMachineNicFirmwareOverrideRequest {
                if_version_match: Some(wrong_timestamp),
                ..request.clone()
            },
            Code::FailedPrecondition,
        ),
        (
            "stale token",
            SetMachineNicFirmwareOverrideRequest {
                if_version_match: Some(before.version),
                ..request.clone()
            },
            Code::FailedPrecondition,
        ),
        (
            "unknown host",
            SetMachineNicFirmwareOverrideRequest {
                machine_id: Some(
                    "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530"
                        .parse()
                        .unwrap(),
                ),
                action: Some(MachineAction::Clear(())),
                ..request.clone()
            },
            Code::NotFound,
        ),
    ];
    let api = &env.api;
    let host = &host;
    let configured = &configured;
    check_cases_async(
        cases.into_iter().map(|(scenario, request, code)| Case {
            scenario,
            input: (scenario, request),
            expect: FailsWith(code),
        }),
        |(scenario, request)| async move {
            let result = api
                .set_machine_nic_firmware_override(Request::new(request))
                .await;
            let unchanged = host.host().rpc_machine().await;
            assert_eq!(unchanged.config, configured.config, "{scenario}");
            assert_eq!(unchanged.version, configured.version, "{scenario}");
            result.map(drop).map_err(|error| error.code())
        },
    )
    .await;

    // Close only this test's pool after the persistence checks. A transaction
    // start failure must retain Core's operator error metadata, not just its code.
    env.pool.close().await;
    let error = env
        .api
        .set_machine_nic_firmware_override(Request::new(request))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Internal);
    assert_eq!(
        error.metadata().get("nico-error-code").unwrap(),
        "NICO-API-500"
    );
}

#[crate::sqlx_test]
async fn desired_nic_firmware_requests_require_a_host_and_valid_allocation(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let api = &env.api;
    let absent_host = "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530"
        .parse()
        .unwrap();
    let invalid_allocation = GetDesiredNicFirmwareRequest {
        machine_id: Some(absent_host),
        allocation_profile_id: Some(format!("{} ", "界".repeat(rpc::MAX_ERR_MSG_SIZE as usize))),
    };
    // Also exercise the desired firmware request's protection around shared log truncation.
    assert!(!format!("{invalid_allocation:?}").is_char_boundary(rpc::MAX_ERR_MSG_SIZE as usize));
    check_cases_async(
        [
            Case {
                scenario: "machine ID is required",
                input: GetDesiredNicFirmwareRequest::default(),
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "DPU ID cannot select a host",
                input: GetDesiredNicFirmwareRequest {
                    machine_id: Some(
                        "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
                            .parse()
                            .unwrap(),
                    ),
                    allocation_profile_id: None,
                },
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "malformed allocation is rejected before looking up the host",
                input: invalid_allocation,
                expect: FailsWith(Code::InvalidArgument),
            },
            Case {
                scenario: "missing host",
                input: GetDesiredNicFirmwareRequest {
                    machine_id: Some(absent_host),
                    allocation_profile_id: None,
                },
                expect: FailsWith(Code::NotFound),
            },
        ],
        |request| async move {
            api.get_desired_nic_firmware(Request::new(request))
                .await
                .map(drop)
                .map_err(|error| error.code())
        },
    )
    .await;
}

#[crate::sqlx_test]
async fn desired_nic_firmware_uses_stored_devices_and_live_profiles_without_scout(
    pool: sqlx::PgPool,
) {
    use model::machine::status::MlxDeviceObservation;
    use rpc::forge::NicFirmwareSelectionSource;
    use rpc::protos::mlx_device::{MlxDeviceInfo, MlxDeviceReport};

    use crate::tests::common::api_fixtures::create_managed_host_multi_dpu;
    let env = create_test_env(pool).await;
    let host = create_managed_host_multi_dpu(&env, 2).await;
    let before = db::machine::find_one(&env.pool, &host.id, Default::default())
        .await
        .unwrap()
        .unwrap();
    let request = GetDesiredNicFirmwareRequest {
        machine_id: Some(host.id.into()),
        allocation_profile_id: None,
    };
    let absent = env
        .api
        .get_desired_nic_firmware(Request::new(GetDesiredNicFirmwareRequest {
            allocation_profile_id: Some("missing".into()),
            ..request.clone()
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(absent.observed_at.is_none() && absent.targets.is_empty());
    let mut config = profile_config();
    let profile = env
        .api
        .create_nic_firmware_profile(Request::new(CreateNicFirmwareProfileRequest {
            id: "baseline".into(),
            config: Some(config.clone()),
        }))
        .await
        .unwrap()
        .into_inner()
        .profile
        .unwrap();
    let mut devices = Vec::new();
    let mut binding_versions = Vec::new();
    for (index, entry) in config.entries.iter().enumerate() {
        let firmware = entry.firmware.as_ref().unwrap();
        let binding = env
            .api
            .set_nic_firmware_site_default(Request::new(SetNicFirmwareSiteDefaultRequest {
                hardware: Some(NicFirmwareHardware {
                    part_number: firmware.part_number.clone(),
                    psid: firmware.psid.clone(),
                }),
                action: Some(Action::ProfileId(profile.id.clone())),
                if_version_match: None,
            }))
            .await
            .unwrap()
            .into_inner()
            .site_default
            .unwrap();
        binding_versions.push(binding.version);
        devices.push(MlxDeviceInfo {
            pci_name: ["05:00.0", "02:00.0"][index].into(),
            part_number: firmware.part_number.clone(),
            psid: firmware.psid.clone(),
            fw_version_current: "99.99.9999".into(),
            ..Default::default()
        });
    }
    devices[0].base_mac = "02:AA:BB:CC:DD:EE".into();
    devices.push(MlxDeviceInfo {
        pci_name: "03:00.0".into(),
        part_number: "unmanaged".into(),
        psid: "unmanaged".into(),
        ..Default::default()
    });
    devices.push(MlxDeviceInfo {
        pci_name: "04:00.0".into(),
        ..Default::default()
    });
    let secondary = before
        .status
        .interfaces
        .iter()
        .find(|interface| {
            !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
        })
        .unwrap();
    devices.push(MlxDeviceInfo {
        pci_name: "01:00.0".into(),
        base_mac: secondary.mac_address.to_string(),
        ..devices[0].clone()
    });
    devices.push(MlxDeviceInfo {
        pci_name: "06:00.0".into(),
        part_number: "PN1".into(),
        ..Default::default()
    });
    devices.push(MlxDeviceInfo {
        pci_name: "07:00.0".into(),
        part_number: " PN1".into(),
        psid: "PSID1".into(),
        ..Default::default()
    });
    devices.push(MlxDeviceInfo {
        pci_name: "08:00.0".into(),
        part_number: " invalid".into(),
        psid: String::new(),
        ..devices[4].clone()
    });
    let observation = MlxDeviceObservation {
        observed_at: "2026-10-03T00:00:00Z".parse().unwrap(),
        devices: devices
            .iter()
            .cloned()
            .map(TryInto::try_into)
            .collect::<Result<_, _>>()
            .unwrap(),
    };
    let mut connection = env.pool.acquire().await.unwrap();
    assert_eq!(
        db::machine::update_mlx_device_observation(&mut connection, &host.id, &observation)
            .await
            .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    drop(connection);
    let (sender, mut scout_requests) =
        scout_connection(&env.api, host.id.into(), Some(host.id.into()))
            .await
            .unwrap();
    // Prove requests reach the connected Scout stream before checking its silence.
    // Otherwise, a broken fixture could look like a read-only firmware lookup.
    live_report(
        &env.api,
        host.id.into(),
        &sender,
        &mut scout_requests,
        MlxDeviceReport {
            timestamp: Some(observation.observed_at.into()),
            devices: devices.clone(),
            ..Default::default()
        },
    )
    .await;
    assert!(
        env.api
            .scout_stream_registry
            .is_connected(host.id.into())
            .await
    );
    let desired = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            biased;
            command = scout_requests.next() => {
                panic!("desired firmware lookup contacted Scout: {command:?}");
            }
            result = env.api.get_desired_nic_firmware(Request::new(request.clone())) => result,
        }
    })
    .await
    .expect("desired firmware lookup did not complete within the test deadline")
    .unwrap()
    .into_inner();
    assert!(
        scout_requests.next().now_or_never().is_none(),
        "desired firmware lookup left a Scout command queued"
    );
    assert_eq!(desired.observed_at, Some(observation.observed_at.into()));
    assert_eq!(desired.targets.len(), devices.len());
    for (index, target) in desired.targets.iter().enumerate() {
        assert_eq!(
            target.identity.as_ref().unwrap().device_info.as_ref(),
            Some(&devices[index])
        );
    }
    for (index, (target, expected)) in desired.targets[..2].iter().zip(&config.entries).enumerate()
    {
        let Some(TargetResult::Firmware(resolved)) = target.result.as_ref() else {
            panic!("expected site default for device {index}: {target:?}");
        };
        assert_eq!(resolved.entry.as_ref(), Some(expected));
        assert_eq!(
            resolved.source,
            NicFirmwareSelectionSource::SiteDefault as i32
        );
        assert_eq!(resolved.profile_version, profile.version);
        assert_eq!(
            target.site_default_version.as_deref(),
            Some(binding_versions[index].as_str())
        );
    }
    assert!(
        desired.targets[0]
            .identity
            .as_ref()
            .unwrap()
            .managed_dpu_machine_ids
            .is_empty()
    );
    assert!(desired.targets[2].result.is_none());
    assert!(matches!(
        desired.targets[3].result.as_ref(),
        Some(TargetResult::Error(error)) if error.contains("missing a part number")
    ));
    for index in [4, 7] {
        assert!(matches!(
            desired.targets[index].result.as_ref(),
            Some(TargetResult::Error(error)) if error.contains("managed-DPU")
        ));
        assert_eq!(
            desired.targets[index]
                .identity
                .as_ref()
                .unwrap()
                .managed_dpu_machine_ids,
            [secondary.attached_dpu_machine_id.unwrap().into()]
        );
        assert!(desired.targets[index].site_default_version.is_none());
    }
    for (index, expected) in [
        (5, "device report is missing a PSID"),
        (6, "invalid value: part number and PSID must each contain"),
    ] {
        assert!(matches!(
            desired.targets[index].result.as_ref(),
            Some(TargetResult::Error(error)) if error.starts_with(expected)
        ));
        assert!(desired.targets[index].site_default_version.is_none());
    }
    config.entries[0].firmware.as_mut().unwrap().version = "28.42.1000".into();
    let updated = env
        .api
        .update_nic_firmware_profile(Request::new(UpdateNicFirmwareProfileRequest {
            id: profile.id.clone(),
            config: Some(config.clone()),
            if_version_match: Some(profile.version),
        }))
        .await
        .unwrap()
        .into_inner()
        .profile
        .unwrap();
    let refreshed = env
        .api
        .get_desired_nic_firmware(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    let Some(TargetResult::Firmware(resolved)) = refreshed.targets[0].result.as_ref() else {
        panic!("expected the updated profile: {:?}", refreshed.targets[0]);
    };
    assert_eq!(resolved.profile_version, updated.version);
    assert_eq!(resolved.entry.as_ref(), Some(&config.entries[0]));
    assert_eq!(
        refreshed.targets[0].site_default_version,
        desired.targets[0].site_default_version
    );
    // The allocation also applies to compatible hardware without a site default.
    let unbound = config.entries[1].firmware.as_ref().unwrap();
    env.api
        .set_nic_firmware_site_default(Request::new(SetNicFirmwareSiteDefaultRequest {
            hardware: Some(NicFirmwareHardware {
                part_number: unbound.part_number.clone(),
                psid: unbound.psid.clone(),
            }),
            action: Some(Action::Clear(())),
            if_version_match: binding_versions.pop(),
        }))
        .await
        .unwrap();
    let mut allocation_config = config.clone();
    allocation_config.entries[0]
        .firmware
        .as_mut()
        .unwrap()
        .version = "allocation-target".into();
    let allocation = env
        .api
        .create_nic_firmware_profile(Request::new(CreateNicFirmwareProfileRequest {
            id: "allocation-only".into(),
            config: Some(allocation_config.clone()),
        }))
        .await
        .unwrap()
        .into_inner()
        .profile
        .unwrap();
    let preview = env
        .api
        .get_desired_nic_firmware(Request::new(GetDesiredNicFirmwareRequest {
            allocation_profile_id: Some(allocation.id.clone()),
            ..request.clone()
        }))
        .await
        .unwrap()
        .into_inner();
    for (index, target) in preview.targets[..2].iter().enumerate() {
        let Some(TargetResult::Firmware(resolved)) = target.result.as_ref() else {
            panic!("expected allocation profile for device {index}: {target:?}");
        };
        assert_eq!(
            resolved.source,
            NicFirmwareSelectionSource::Allocation as i32
        );
        assert_eq!(resolved.profile_id, allocation.id);
        assert_eq!(resolved.profile_version, allocation.version);
        assert_eq!(
            resolved.entry.as_ref(),
            Some(&allocation_config.entries[index])
        );
        assert_eq!(
            target.site_default_version.as_deref(),
            binding_versions.get(index).map(String::as_str)
        );
    }
    assert_eq!(preview.targets[4], desired.targets[4]);
    assert!(matches!(
        preview.targets[2].result.as_ref(),
        Some(TargetResult::Error(error)) if error.contains("does not support")
    ));
    assert!(preview.targets[2].site_default_version.is_none());
    let preview = env
        .api
        .get_desired_nic_firmware(Request::new(GetDesiredNicFirmwareRequest {
            allocation_profile_id: Some("missing".into()),
            ..request.clone()
        }))
        .await
        .unwrap()
        .into_inner();
    for index in [4, 7] {
        assert_eq!(preview.targets[index], desired.targets[index]);
    }
    for index in [0, 2] {
        let target = &preview.targets[index];
        assert!(
            matches!(
                target.result.as_ref(),
                Some(TargetResult::Error(error)) if error == "invalid value: NIC firmware profile missing is missing"
            ),
            "device {index}"
        );
        assert_eq!(
            target.site_default_version.as_ref(),
            binding_versions.get(index)
        );
    }
    let after = db::machine::find_one(&env.pool, &host.id, Default::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state.value, before.state.value);
    assert_eq!(after.state.version, before.state.version);
    assert_eq!(after.status.mlx_device_observation, Some(observation));
    assert!(
        env.api
            .scout_stream_registry
            .is_connected(host.id.into())
            .await
    );
    assert!(
        scout_requests.next().now_or_never().is_none(),
        "desired firmware lookup left a Scout command queued"
    );

    // Association changes do not require collecting another device observation.
    let mut expected = env
        .api
        .get_desired_nic_firmware(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    let new_dpu = before
        .status
        .interfaces
        .iter()
        .find_map(|interface| {
            interface
                .attached_dpu_machine_id
                .filter(|id| Some(*id) != secondary.attached_dpu_machine_id)
        })
        .unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    db::machine_interface::associate_interface_with_dpu_machine(&secondary.id, &new_dpu, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let reassociated = env
        .api
        .get_desired_nic_firmware(Request::new(request))
        .await
        .unwrap()
        .into_inner();
    for index in [4, 7] {
        expected.targets[index]
            .identity
            .as_mut()
            .unwrap()
            .managed_dpu_machine_ids = vec![new_dpu.into()];
    }
    assert_eq!(reassociated, expected);
}

#[crate::sqlx_test]
async fn desired_nic_firmware_keeps_selections_and_profiles_in_one_snapshot(pool: sqlx::PgPool) {
    use model::machine::status::MlxDeviceObservation;
    use rpc::protos::mlx_device::MlxDeviceInfo;

    use crate::tests::common::api_fixtures::create_managed_host;

    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    assert!(
        !env.api
            .scout_stream_registry
            .is_connected(host.id.into())
            .await
    );
    let config = profile_config();
    let mut profiles = Vec::new();
    for id in ["original", "replacement"] {
        profiles.push(
            env.api
                .create_nic_firmware_profile(Request::new(CreateNicFirmwareProfileRequest {
                    id: id.into(),
                    config: Some(config.clone()),
                }))
                .await
                .unwrap()
                .into_inner()
                .profile
                .unwrap(),
        );
    }
    let firmware = config.entries[0].firmware.as_ref().unwrap();
    let hardware = NicFirmwareHardware {
        part_number: firmware.part_number.clone(),
        psid: firmware.psid.clone(),
    };
    let binding = env
        .api
        .set_nic_firmware_site_default(Request::new(SetNicFirmwareSiteDefaultRequest {
            hardware: Some(hardware.clone()),
            action: Some(Action::ProfileId(profiles[0].id.clone())),
            if_version_match: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .site_default
        .unwrap();
    let observation = MlxDeviceObservation {
        observed_at: "2026-10-03T00:00:00Z".parse().unwrap(),
        devices: vec![
            MlxDeviceInfo {
                pci_name: "01:00.0".into(),
                part_number: hardware.part_number.clone(),
                psid: hardware.psid.clone(),
                ..Default::default()
            }
            .try_into()
            .unwrap(),
        ],
    };
    let mut connection = env.pool.acquire().await.unwrap();
    assert_eq!(
        db::machine::update_mlx_device_observation(&mut connection, &host.id, &observation)
            .await
            .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    drop(connection);

    env.api
        .set_machine_nic_firmware_override(Request::new(SetMachineNicFirmwareOverrideRequest {
            machine_id: Some(host.id.into()),
            if_version_match: Some(host.host().rpc_machine().await.version),
            action: Some(MachineAction::ProfileId(profiles[0].id.clone())),
        }))
        .await
        .unwrap();
    let machine_version = host.host().rpc_machine().await.version.parse().unwrap();

    // Block the profile SELECT after the lookup has read both selections. The
    // writer replaces them and removes the old profile's hardware entry in one
    // commit, so mixing snapshots would report a false incompatibility.
    let mut writer = env.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE nic_firmware_profiles IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *writer)
        .await
        .unwrap();
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    let request = GetDesiredNicFirmwareRequest {
        machine_id: Some(host.id.into()),
        allocation_profile_id: None,
    };
    let read = env
        .api
        .get_desired_nic_firmware(Request::new(request.clone()));
    let change = async {
        let reader_pid = db::test_support::postgres::wait_for_blocked_query(
            &env.pool,
            writer_pid,
            "FROM nic_firmware_profiles WHERE id",
        )
        .await;
        // The transaction retains the machine read's relation lock. A read
        // through the pool would release it before reaching the profile query.
        let retains_machine_lock: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM pg_locks
                WHERE pid = $1 AND locktype = 'relation'
                  AND relation = 'machines'::regclass
                  AND mode = 'AccessShareLock' AND granted
            )",
        )
        .bind(reader_pid)
        .fetch_one(&env.pool)
        .await
        .unwrap();
        assert!(
            retains_machine_lock,
            "desired firmware reader did not retain the machines relation lock"
        );
        let hardware = hardware.try_into().unwrap();
        let replacement = db::nic_firmware::set_site_default(
            &mut writer,
            &hardware,
            &profiles[1].id.parse().unwrap(),
            Some(binding.version.parse().unwrap()),
        )
        .await
        .unwrap();
        db::nic_firmware::set_machine_override(
            &mut writer,
            &host.id,
            Some(&profiles[1].id.parse().unwrap()),
            machine_version,
        )
        .await
        .unwrap();
        let mut old_config = config.clone();
        old_config.entries.remove(0);
        db::nic_firmware::update(
            &mut writer,
            &profiles[0].id.parse().unwrap(),
            &old_config.try_into().unwrap(),
            profiles[0].version.parse().unwrap(),
        )
        .await
        .unwrap();
        writer.commit().await.unwrap();
        replacement
    };
    let (desired, replacement) = tokio::join!(read, change);
    let desired = desired.unwrap().into_inner();
    let Some(TargetResult::Firmware(resolved)) = desired.targets[0].result.as_ref() else {
        panic!(
            "expected original consistent profile: {:?}",
            desired.targets[0]
        );
    };
    assert_eq!(resolved.profile_id, profiles[0].id);
    assert_eq!(
        resolved.source,
        rpc::forge::NicFirmwareSelectionSource::MachineOverride as i32
    );
    assert_eq!(resolved.profile_version, profiles[0].version);
    assert_eq!(resolved.entry.as_ref(), Some(&config.entries[0]));
    assert_eq!(
        desired.targets[0].site_default_version.as_ref(),
        Some(&binding.version)
    );

    let refreshed = env
        .api
        .get_desired_nic_firmware(Request::new(request))
        .await
        .unwrap()
        .into_inner();
    let Some(TargetResult::Firmware(resolved)) = refreshed.targets[0].result.as_ref() else {
        panic!("expected replacement profile: {:?}", refreshed.targets[0]);
    };
    assert_eq!(resolved.profile_id, profiles[1].id);
    assert_eq!(
        resolved.source,
        rpc::forge::NicFirmwareSelectionSource::MachineOverride as i32
    );
    assert_eq!(
        refreshed.targets[0].site_default_version,
        Some(replacement.version.to_string())
    );
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
        .find_nic_firmware_profile_ids(Request::new(FindNicFirmwareProfileIdsRequest::default()))
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
