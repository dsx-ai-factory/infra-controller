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

use std::sync::Arc;

use carbide_uuid::machine::HostMachineId;
use chrono::Utc;
use db::desired_firmware::desired_firmware_rows;
use db::host_machine_update::{find_update_gates, find_upgrade_needed};
use model::firmware::{Firmware, FirmwareComponentType};
use model::host_machine_update::{
    HostUpdateGateRow, UpdateBlockingReason, host_update_eligibility,
};
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    DeleteHostFirmwareConfigRequest, GetDesiredFirmwareVersionsRequest, HostFirmwareArtifact,
    HostFirmwareComponentConfigResponse, HostFirmwareComponentType, HostFirmwareVersionConfig,
    UpsertHostFirmwareComponentConfig, UpsertHostFirmwareConfigRequest,
};
use tonic::{Code, Request};

use crate::cfg::file::CarbideConfig;
use crate::machine_update_manager::MachineUpdateManager;
use crate::tests::common::api_fixtures::dpu::create_dpu_machine_in_waiting_for_network_install;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, TestManagedHost, create_managed_host, create_test_env,
    create_test_env_with_overrides, get_config,
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

/// One update gate scenario. `setup` is SQL run after the desired firmware
/// table is written, with `{host}` replaced by the host ID.
struct GateCase {
    scenario: &'static str,
    setup: &'static str,
    global_enabled: bool,
    ready_only: bool,
    explicit_start: bool,
    expect: Result<(), &'static str>,
}

#[crate::sqlx_test]
async fn update_gates_match_find_upgrade_needed(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_id = HostMachineId::from(&create_managed_host(&env).await.id);
    let catalog = env.config.get_firmware_config().create_snapshot();

    let case = |scenario, setup, expect| GateCase {
        scenario,
        setup,
        global_enabled: true,
        ready_only: false,
        explicit_start: false,
        expect,
    };
    let cases = [
        case(
            "own flag unset, global on",
            "UPDATE machines SET firmware_autoupdate = NULL WHERE id = '{host}'",
            Ok(()),
        ),
        GateCase {
            global_enabled: false,
            ..case(
                "own flag on, global off",
                "UPDATE machines SET firmware_autoupdate = true WHERE id = '{host}'",
                Ok(()),
            )
        },
        case(
            "own flag off",
            "UPDATE machines SET firmware_autoupdate = false WHERE id = '{host}'",
            Err("autoupdate_off_host"),
        ),
        GateCase {
            global_enabled: false,
            ..case(
                "own flag unset, global off",
                "UPDATE machines SET firmware_autoupdate = NULL WHERE id = '{host}'",
                Err("autoupdate_off_global"),
            )
        },
        case(
            "open reprovisioning request",
            "UPDATE machines SET host_reprovisioning_requested = '{}' WHERE id = '{host}'",
            Err("request_open"),
        ),
        case(
            "versions equal",
            "UPDATE explored_endpoints e
                SET exploration_report = jsonb_set(e.exploration_report, '{Versions}', d.versions->'Versions')
                FROM desired_firmware d
                WHERE d.vendor = e.exploration_report->>'Vendor' AND d.model = e.exploration_report->>'Model'",
            Err("firmware_current"),
        ),
        case(
            "report without Versions",
            "UPDATE explored_endpoints SET exploration_report = exploration_report - 'Versions'",
            Err("firmware_versions_unavailable"),
        ),
        case(
            "report Versions JSON null",
            "UPDATE explored_endpoints
                SET exploration_report = jsonb_set(exploration_report, '{Versions}', 'null')",
            Err("firmware_versions_unavailable"),
        ),
        case(
            "no target for the report model",
            "UPDATE explored_endpoints
                SET exploration_report = jsonb_set(exploration_report, '{Model}', '\"Unknown\"')",
            Err("no_firmware_target"),
        ),
        case(
            "no report",
            "DELETE FROM explored_endpoints",
            Err("no_explored_report"),
        ),
        case(
            "only a second BMC address has an outdated report",
            "INSERT INTO machine_interface_addresses (interface_id, address)
                SELECT id, 'fd00::8150' FROM machine_interfaces
                WHERE machine_id = '{host}' AND interface_type = 'Bmc';
            INSERT INTO explored_endpoints (address, exploration_report, version)
                SELECT 'fd00::8150', exploration_report, version FROM explored_endpoints
                WHERE exploration_report->>'Vendor' = 'Dell';
            UPDATE explored_endpoints SET exploration_report = exploration_report - 'Versions'
                WHERE address <> 'fd00::8150'",
            Ok(()),
        ),
        GateCase {
            explicit_start: true,
            ..case(
                "explicit start before the window",
                "UPDATE machines SET firmware_update_time_window_start = now() + interval '1 hour',
                    firmware_update_time_window_end = now() + interval '2 hours' WHERE id = '{host}'",
                Err("outside_update_window"),
            )
        },
        GateCase {
            explicit_start: true,
            ..case(
                "explicit start inside the window",
                "UPDATE machines SET firmware_update_time_window_start = now() - interval '1 hour',
                    firmware_update_time_window_end = now() + interval '1 hour' WHERE id = '{host}'",
                Ok(()),
            )
        },
        GateCase {
            explicit_start: true,
            ..case(
                "explicit start after the window",
                "UPDATE machines SET firmware_update_time_window_start = now() - interval '2 hours',
                    firmware_update_time_window_end = now() - interval '1 hour' WHERE id = '{host}'",
                Err("outside_update_window"),
            )
        },
        GateCase {
            ready_only: true,
            ..case(
                "manual tagging and not Ready",
                "UPDATE machines SET controller_state = jsonb_set(controller_state, '{state}', '\"assigned\"')
                    WHERE id = '{host}'",
                Err("not_ready"),
            )
        },
    ];

    for case in cases {
        let scenario = case.scenario;
        let models: Vec<Firmware> = catalog
            .values()
            .cloned()
            .map(|mut model| {
                model.explicit_start_needed |= case.explicit_start;
                model
            })
            .collect();
        // Each case runs in its own transaction, so both readers see its setup.
        let mut txn = env.pool.begin().await?;
        db::desired_firmware::snapshot_desired_firmware(&mut txn, &models).await?;
        // The setup is a test literal and the substituted host ID is generated.
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            case.setup.replace("{host}", &host_id.to_string()),
        ))
        .execute(&mut *txn)
        .await?;

        let selected = find_upgrade_needed(&mut txn, case.global_enabled, case.ready_only)
            .await?
            .iter()
            .any(|update| update.id == host_id);
        let rows = find_update_gates(
            &mut txn,
            Utc::now(),
            case.global_enabled,
            case.ready_only,
            &desired_firmware_rows(&models),
            &[host_id],
        )
        .await?;
        let firmware_autoupdate: Option<bool> =
            sqlx::query_scalar("SELECT firmware_autoupdate FROM machines WHERE id = $1")
                .bind(host_id)
                .fetch_one(&mut *txn)
                .await?;
        txn.rollback().await?;

        assert_eq!(
            rows.iter().any(HostUpdateGateRow::passes_manager_query),
            selected,
            "{scenario}"
        );
        assert_eq!(
            host_update_eligibility(&rows, None, firmware_autoupdate, false)
                .map_err(|reason| reason.code()),
            case.expect,
            "{scenario}"
        );
    }

    Ok(())
}

/// The gate rows for `host_id`, read with a catalog built fresh from config and
/// overrides, as the compact host read builds it.
async fn fresh_gate_rows(
    env: &TestEnv,
    host_id: HostMachineId,
) -> Result<Vec<HostUpdateGateRow>, Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    let overrides = db::host_firmware_config::list_configs(&mut *txn).await?;
    let catalog = env
        .config
        .get_firmware_config()
        .create_snapshot_with_overrides(overrides);
    let firmware_global = &env.config.firmware_global;
    let rows = find_update_gates(
        &mut txn,
        Utc::now(),
        firmware_global.autoupdate,
        firmware_global.instance_updates_manual_tagging,
        &desired_firmware_rows(catalog.values()),
        &[host_id],
    )
    .await?;
    txn.rollback().await?;
    Ok(rows)
}

pub(super) async fn host_reprovision_requested(env: &TestEnv, host: &TestManagedHost) -> bool {
    let mut txn = env.pool.begin().await.unwrap();
    let machine = host.host().db_machine(&mut txn).await;
    txn.rollback().await.unwrap();
    machine.host_reprovision_requested.is_some()
}

pub(super) fn update_manager(env: &TestEnv, config: Arc<CarbideConfig>) -> MachineUpdateManager {
    MachineUpdateManager::new(
        env.pool.clone(),
        config,
        env.test_meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    )
}

#[crate::sqlx_test]
async fn gates_follow_override_without_manager(
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
    let upsert_bmc_target = |explicit_start_needed| {
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
                Some(explicit_start_needed),
            )))
    };
    upsert_bmc_target(true).await?;
    let managed_host = create_managed_host(&env).await;
    let host_id = HostMachineId::from(&managed_host.id);
    let update_manager = update_manager(&env, env.config.clone());
    // The manager stores the explicit-start target, so the host waits for a window.
    update_manager.run_single_iteration().await?;
    assert!(!host_reprovision_requested(&env, &managed_host).await);

    upsert_bmc_target(false).await?;
    let rows = fresh_gate_rows(&env, host_id).await?;
    assert_eq!(host_update_eligibility(&rows, None, None, false), Ok(()));
    let mut txn = env.pool.begin().await?;
    let stale = find_upgrade_needed(&mut txn, true, false).await?;
    txn.rollback().await?;
    assert!(stale.iter().all(|update| update.id != host_id));

    update_manager.run_single_iteration().await?;
    assert!(host_reprovision_requested(&env, &managed_host).await);

    Ok(())
}

#[crate::sqlx_test]
async fn gates_with_empty_cache_and_zero_capacity(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config
        .machine_updater
        .max_concurrent_machine_updates_absolute = Some(0);
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;
    let managed_host = create_managed_host(&env).await;
    let host_id = HostMachineId::from(&managed_host.id);

    let rows = fresh_gate_rows(&env, host_id).await?;
    assert_eq!(host_update_eligibility(&rows, None, None, false), Ok(()));

    update_manager(&env, env.config.clone())
        .run_single_iteration()
        .await?;
    let desired_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM desired_firmware")
        .fetch_one(&env.pool)
        .await?;
    assert_eq!(
        desired_rows, 0,
        "a manager without capacity writes no targets"
    );
    assert!(!host_reprovision_requested(&env, &managed_host).await);

    let mut config = (*env.config).clone();
    config
        .machine_updater
        .max_concurrent_machine_updates_absolute = Some(10);
    update_manager(&env, Arc::new(config))
        .run_single_iteration()
        .await?;
    assert!(host_reprovision_requested(&env, &managed_host).await);

    Ok(())
}

#[crate::sqlx_test]
async fn predicted_host_has_report_but_not_stable_host(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let predicted_host =
        create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;

    let rows = fresh_gate_rows(&env, predicted_host.id.into()).await?;
    let row = rows
        .iter()
        .find(|row| row.report_version.is_some())
        .expect("the predicted host's BMC report is read");
    assert_eq!(row.report_vendor.as_deref(), Some("Dell"));
    assert_eq!(row.report_model.as_deref(), Some("PowerEdge R750"));
    assert!(row.report_versions.is_some());
    assert!(!row.stable_host);
    assert_eq!(
        host_update_eligibility(&rows, None, None, false),
        Err(UpdateBlockingReason::NotStableHost)
    );

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
