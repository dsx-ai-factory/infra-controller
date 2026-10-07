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

use model::firmware::FirmwareComponentType;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    DeleteHostFirmwareConfigRequest, GetDesiredFirmwareVersionsRequest, HostFirmwareArtifact,
    HostFirmwareComponentConfigResponse, HostFirmwareComponentType, HostFirmwareVersionConfig,
    UpsertHostFirmwareComponentConfig, UpsertHostFirmwareConfigRequest,
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
        response.ordering,
        vec![HostFirmwareComponentType::Cx7 as i32]
    );

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
        ordering: ordering
            .into_iter()
            .map(|component_type| component_type as i32)
            .collect(),
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
