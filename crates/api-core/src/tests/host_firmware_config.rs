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

use model::firmware::{DriveModel, FirmwareComponentType};
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    DeleteHostFirmwareConfigRequest, GetDesiredFirmwareVersionsRequest, HostFirmwareArtifact,
    HostFirmwareComponentConfigResponse, HostFirmwareComponentRef, HostFirmwareComponentType,
    HostFirmwareUpgradeScript, HostFirmwareVersionConfig, UpsertHostFirmwareComponentConfig,
    UpsertHostFirmwareConfigRequest,
};
use tonic::{Code, Request};

use crate::machine_update_manager::MachineUpdateManager;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_test_env, create_test_env_with_overrides,
    get_config,
};

#[crate::sqlx_test]
async fn upsert_host_firmware_config_creates_and_merges_versions(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    env.api
        .upsert_host_firmware_config(Request::new(upsert_request(
            vec![component_config(
                HostFirmwareComponentType::Cx7,
                vec![version_config_with_preingestion_exclusive_config(
                    "28.47.2682",
                    true,
                    Some(true),
                )],
                Some("28.48.1000"),
            )],
            vec![HostFirmwareComponentType::Cx7],
            Some(true),
        )))
        .await?;

    let response = env
        .api
        .upsert_host_firmware_config(Request::new(upsert_request(
            vec![component_config(
                HostFirmwareComponentType::Cx7,
                vec![version_config("28.48.1111", true)],
                None,
            )],
            Vec::new(),
            None,
        )))
        .await?
        .into_inner();

    assert!(response.explicit_start_needed);
    assert_eq!(
        response.component_ordering,
        vec![component_ref(HostFirmwareComponentType::Cx7)]
    );
    #[allow(deprecated)]
    let legacy_ordering = &response.ordering;
    assert_eq!(legacy_ordering, &[HostFirmwareComponentType::Cx7 as i32]);

    let cx7 = response_component(&response.components, HostFirmwareComponentType::Cx7);
    assert_eq!(
        cx7.preingest_upgrade_when_below.as_deref(),
        Some("28.48.1000")
    );
    assert_eq!(
        firmware_defaults(cx7),
        vec![("28.47.2682", false), ("28.48.1111", true)]
    );
    assert_eq!(
        firmware_preingestion_exclusive_configs(cx7),
        vec![("28.47.2682", Some(true)), ("28.48.1111", Some(false))]
    );

    let mut txn = env.pool.begin().await?;
    let stored = db::host_firmware_config::get(&mut txn, "Nvidia", "dgxh100")
        .await?
        .expect("stored host firmware config")
        .into_config();
    txn.commit().await?;

    assert_eq!(stored.ordering, vec![FirmwareComponentType::Cx7]);
    assert_eq!(stored.explicit_start_needed, Some(true));
    let stored_cx7 = stored
        .components
        .get(&FirmwareComponentType::Cx7)
        .expect("stored cx7 component");
    assert_eq!(
        stored_cx7.preingest_upgrade_when_below.as_deref(),
        Some("28.48.1000")
    );
    assert_eq!(
        stored_cx7
            .known_firmware
            .iter()
            .map(|firmware| {
                (
                    firmware.version.as_str(),
                    firmware.default,
                    firmware.preingestion_exclusive_config,
                )
            })
            .collect::<Vec<_>>(),
        vec![("28.47.2682", false, true), ("28.48.1111", true, false)]
    );

    Ok(())
}

#[crate::sqlx_test]
async fn upsert_host_firmware_config_rejects_create_without_ordering(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let error = env
        .api
        .upsert_host_firmware_config(Request::new(upsert_request(
            vec![component_config(
                HostFirmwareComponentType::Cx7,
                vec![version_config("28.47.2682", true)],
                None,
            )],
            Vec::new(),
            Some(false),
        )))
        .await
        .expect_err("create without ordering should fail");

    assert_eq!(error.code(), Code::InvalidArgument);

    Ok(())
}

#[crate::sqlx_test]
async fn upsert_host_firmware_config_rejects_added_component_without_ordering_update(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    env.api
        .upsert_host_firmware_config(Request::new(upsert_request(
            vec![component_config(
                HostFirmwareComponentType::Cx7,
                vec![version_config("28.47.2682", true)],
                None,
            )],
            vec![HostFirmwareComponentType::Cx7],
            Some(false),
        )))
        .await?;

    let error = env
        .api
        .upsert_host_firmware_config(Request::new(upsert_request(
            vec![component_config(
                HostFirmwareComponentType::Uefi,
                vec![version_config("96.00.5E.00.01", true)],
                None,
            )],
            Vec::new(),
            None,
        )))
        .await
        .expect_err("adding component without ordering update should fail");

    assert_eq!(error.code(), Code::InvalidArgument);

    let mut txn = env.pool.begin().await?;
    let stored = db::host_firmware_config::get(&mut txn, "Nvidia", "DGXH100")
        .await?
        .expect("stored host firmware config")
        .into_config();
    txn.commit().await?;

    assert_eq!(stored.ordering, vec![FirmwareComponentType::Cx7]);
    assert!(stored.components.contains_key(&FirmwareComponentType::Cx7));
    assert!(!stored.components.contains_key(&FirmwareComponentType::Uefi));

    Ok(())
}

#[crate::sqlx_test]
async fn upsert_host_firmware_config_requires_and_round_trips_drive_upgrade_script(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let drive_ref = HostFirmwareComponentRef {
        r#type: HostFirmwareComponentType::Drive as i32,
        drive_model: Some("ST24000NM005H".to_string()),
    };
    let upgrade_script = HostFirmwareUpgradeScript {
        url: "https://scripts.example.invalid/scsi_drive_upgrade.sh".to_string(),
        sha256: "c60b4c1c2d46f7b9e50d7d2e2d77dbfbe7884030b0e9a86a4636b4bc85d8609e".to_string(),
        execution_timeout_seconds: 3600,
        artifact_download_timeout_seconds: 600,
    };
    let image_sha256 = "27925abb3edb48119579e8eaac1d15fbe7d2a3b12f277f1a13cdce1b8a6f8431";
    let drive_request = |upgrade_script: Option<HostFirmwareUpgradeScript>,
                         artifact_sha256: Option<&str>| {
        let mut drive = component_config(
            HostFirmwareComponentType::Drive,
            vec![HostFirmwareVersionConfig {
                power_drains_needed: Some(1),
                upgrade_script,
                artifacts: vec![HostFirmwareArtifact {
                    url: "https://firmware.example.invalid/ET07/et07.lod".to_string(),
                    sha256: artifact_sha256.map(str::to_string),
                }],
                ..version_config("ET07", true)
            }],
            None,
        );
        drive.drive_model = drive_ref.drive_model.clone();
        let mut request = upsert_request_for(
            "Lenovo",
            "ThinkSystem HS350X V3",
            vec![drive],
            Vec::new(),
            None,
        );
        request.component_ordering = vec![drive_ref.clone()];
        request
    };

    // No packaged script exists for this drive, so a version without its own
    // script is rejected: drives never fall back to a BMC upload. Scout also
    // verifies every artifact, so one without a sha256 is rejected.
    for (request, case) in [
        (
            drive_request(None, Some(image_sha256)),
            "drive version without a script",
        ),
        (
            drive_request(Some(upgrade_script.clone()), None),
            "drive artifact without a sha256",
        ),
    ] {
        let error = env
            .api
            .upsert_host_firmware_config(Request::new(request))
            .await
            .expect_err(case);
        assert_eq!(error.code(), Code::InvalidArgument, "{case}");
    }

    let response = env
        .api
        .upsert_host_firmware_config(Request::new(drive_request(
            Some(upgrade_script.clone()),
            Some(image_sha256),
        )))
        .await?
        .into_inner();

    // Re-sending the version replaces it as a whole; without its script that
    // is rejected too, and the stored script stays.
    let error = env
        .api
        .upsert_host_firmware_config(Request::new(drive_request(None, Some(image_sha256))))
        .await
        .expect_err("replacing the drive version without its script");
    assert_eq!(error.code(), Code::InvalidArgument);

    assert_eq!(response.component_ordering, vec![drive_ref]);
    // The type-only ordering cannot name a drive model, so it leaves drives out.
    #[allow(deprecated)]
    let legacy_ordering = &response.ordering;
    assert!(legacy_ordering.is_empty());
    let component = response_component(&response.components, HostFirmwareComponentType::Drive);
    assert_eq!(component.drive_model.as_deref(), Some("ST24000NM005H"));
    assert_eq!(
        component.current_version_reported_as.as_deref(),
        Some("^Drive_ST24000NM005H_")
    );
    assert_eq!(component.firmware[0].upgrade_script, Some(upgrade_script));

    let mut txn = env.pool.begin().await?;
    let stored = db::host_firmware_config::get(&mut txn, "Lenovo", "ThinkSystem HS350X V3")
        .await?
        .expect("stored host firmware config")
        .into_config();
    txn.commit().await?;

    let drive_type = FirmwareComponentType::Drive {
        model: DriveModel::new("ST24000NM005H").unwrap(),
    };
    assert_eq!(stored.ordering, vec![drive_type]);
    let stored_scout = stored.components[&drive_type].known_firmware[0]
        .scout
        .as_ref()
        .expect("stored scout config");
    assert_eq!(
        stored_scout.script_url().map(|(url, _)| url),
        Some("https://scripts.example.invalid/scsi_drive_upgrade.sh")
    );

    Ok(())
}

#[crate::sqlx_test]
async fn delete_host_firmware_config_removes_existing_row(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    env.api
        .upsert_host_firmware_config(Request::new(upsert_request(
            vec![component_config(
                HostFirmwareComponentType::Cx7,
                vec![version_config("28.47.2682", true)],
                None,
            )],
            vec![HostFirmwareComponentType::Cx7],
            Some(false),
        )))
        .await?;

    env.api
        .delete_host_firmware_config(Request::new(DeleteHostFirmwareConfigRequest {
            vendor: " Nvidia ".to_string(),
            model: " dgxh100 ".to_string(),
        }))
        .await?;

    let mut txn = env.pool.begin().await?;
    let stored = db::host_firmware_config::get(&mut txn, "Nvidia", "DGXH100").await?;
    txn.commit().await?;

    assert!(stored.is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn delete_host_firmware_config_returns_not_found_for_missing_pair(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let error = env
        .api
        .delete_host_firmware_config(Request::new(DeleteHostFirmwareConfigRequest {
            vendor: "Nvidia".to_string(),
            model: "DGXH100".to_string(),
        }))
        .await
        .expect_err("missing host firmware config should fail");

    assert_eq!(error.code(), Code::NotFound);

    Ok(())
}

#[crate::sqlx_test]
async fn get_desired_firmware_versions_merges_runtime_config_with_static_catalog(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    env.api
        .upsert_host_firmware_config(Request::new(upsert_request_for(
            "Dell",
            "PowerEdge R750",
            vec![component_config(
                HostFirmwareComponentType::Bmc,
                vec![version_config("9.99.99.99", true)],
                None,
            )],
            vec![HostFirmwareComponentType::Bmc],
            Some(false),
        )))
        .await?;

    let response = env
        .api
        .get_desired_firmware_versions(Request::new(GetDesiredFirmwareVersionsRequest {}))
        .await?
        .into_inner();
    let dell_vendor = bmc_vendor::BMCVendor::Dell.to_string();
    let dell = response
        .entries
        .iter()
        .find(|entry| entry.vendor == dell_vendor && entry.model == "PowerEdge R750")
        .unwrap_or_else(|| {
            panic!(
                "Dell PowerEdge R750 desired firmware entry; got {:?}",
                response.entries
            )
        });

    assert_eq!(
        dell.component_versions.get("bmc").map(String::as_str),
        Some("9.99.99.99")
    );
    assert_eq!(
        dell.component_versions.get("uefi").map(String::as_str),
        Some("1.13.2")
    );

    Ok(())
}

#[crate::sqlx_test]
async fn upsert_host_firmware_config_drives_machine_update_manager(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.host_models.clear();
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;

    env.api
        .upsert_host_firmware_config(Request::new(upsert_request_for(
            "Dell",
            "PowerEdge R750",
            vec![component_config(
                HostFirmwareComponentType::Bmc,
                vec![version_config("9.99.99.99", true)],
                None,
            )],
            vec![HostFirmwareComponentType::Bmc],
            Some(false),
        )))
        .await?;

    let managed_host = create_managed_host(&env).await;
    let update_manager = MachineUpdateManager::new(
        env.pool.clone(),
        env.config.clone(),
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );

    update_manager.run_single_iteration().await?;

    let desired_bmc_version: Option<String> = sqlx::query_scalar(
        r#"
            SELECT versions->'Versions'->>'bmc'
            FROM desired_firmware
            WHERE vendor = 'Dell' AND model = 'PowerEdge R750'
        "#,
    )
    .fetch_optional(&env.pool)
    .await?;
    assert_eq!(desired_bmc_version.as_deref(), Some("9.99.99.99"));

    let mut txn = env.pool.begin().await?;
    let host = managed_host.host().db_machine(&mut txn).await;
    txn.commit().await?;

    assert!(host.host_reprovision_requested.is_some());

    Ok(())
}

fn upsert_request(
    components: Vec<UpsertHostFirmwareComponentConfig>,
    ordering: Vec<HostFirmwareComponentType>,
    explicit_start_needed: Option<bool>,
) -> UpsertHostFirmwareConfigRequest {
    upsert_request_for(
        "Nvidia",
        "DGXH100",
        components,
        ordering,
        explicit_start_needed,
    )
}

fn upsert_request_for(
    vendor: &str,
    model: &str,
    components: Vec<UpsertHostFirmwareComponentConfig>,
    ordering: Vec<HostFirmwareComponentType>,
    explicit_start_needed: Option<bool>,
) -> UpsertHostFirmwareConfigRequest {
    UpsertHostFirmwareConfigRequest {
        vendor: vendor.to_string(),
        model: model.to_string(),
        components,
        explicit_start_needed,
        component_ordering: ordering.into_iter().map(component_ref).collect(),
        ..Default::default()
    }
}

fn component_ref(component_type: HostFirmwareComponentType) -> HostFirmwareComponentRef {
    HostFirmwareComponentRef {
        r#type: component_type as i32,
        drive_model: None,
    }
}

fn component_config(
    component_type: HostFirmwareComponentType,
    firmware: Vec<HostFirmwareVersionConfig>,
    preingest_upgrade_when_below: Option<&str>,
) -> UpsertHostFirmwareComponentConfig {
    UpsertHostFirmwareComponentConfig {
        r#type: component_type as i32,
        firmware,
        preingest_upgrade_when_below: preingest_upgrade_when_below.map(str::to_string),
        drive_model: None,
    }
}

fn version_config(version: &str, default: bool) -> HostFirmwareVersionConfig {
    version_config_with_preingestion_exclusive_config(version, default, None)
}

fn version_config_with_preingestion_exclusive_config(
    version: &str,
    default: bool,
    preingestion_exclusive_config: Option<bool>,
) -> HostFirmwareVersionConfig {
    HostFirmwareVersionConfig {
        version: version.to_string(),
        default,
        artifacts: vec![HostFirmwareArtifact {
            url: format!("https://firmware.example.invalid/{version}/fw.bin"),
            sha256: None,
        }],
        install_only_specified: false,
        power_drains_needed: None,
        pre_update_resets: false,
        preingestion_exclusive_config,
        upgrade_script: None,
    }
}

fn response_component(
    components: &[HostFirmwareComponentConfigResponse],
    component_type: HostFirmwareComponentType,
) -> &HostFirmwareComponentConfigResponse {
    components
        .iter()
        .find(|component| component.r#type == component_type as i32)
        .expect("response component")
}

fn firmware_defaults(component: &HostFirmwareComponentConfigResponse) -> Vec<(&str, bool)> {
    let mut versions = component
        .firmware
        .iter()
        .map(|firmware| (firmware.version.as_str(), firmware.default))
        .collect::<Vec<_>>();
    versions.sort_by_key(|(version, _)| *version);
    versions
}

fn firmware_preingestion_exclusive_configs(
    component: &HostFirmwareComponentConfigResponse,
) -> Vec<(&str, Option<bool>)> {
    let mut versions = component
        .firmware
        .iter()
        .map(|firmware| {
            (
                firmware.version.as_str(),
                firmware.preingestion_exclusive_config,
            )
        })
        .collect::<Vec<_>>();
    versions.sort_by_key(|(version, _)| *version);
    versions
}
