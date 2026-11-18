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

use std::collections::HashMap;
use std::sync::Arc;

use carbide_secrets::credentials::{
    CredentialKey, CredentialReader, CredentialWriter, Credentials,
};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::mac_address_pool::{
    EXPECTED_SWITCH_BMC_MAC_ADDRESS_POOL, EXPECTED_SWITCH_NVOS_MAC_ADDRESS_POOL,
};
use carbide_test_harness::test_support::switch::attach_switches_with_nvos_credentials as attach_switches;
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::SwitchId;
use db::db_read::DbReader;
use db::{ObjectColumnFilter, rack as db_rack};
use model::expected_switch::{ExpectedNvosInterface, ExpectedSwitch};
use model::metadata::Metadata;
use model::rack::{
    FirmwareUpgradeState, MaintenanceActivity, Rack, RackConfig, RackMaintenanceState, RackState,
};
use tonic::Request;

/// Builds six expected-switch models without creating database records.
///
/// MAC addresses come from the shared fixture pools. Serials and names are fixed
/// (`SW-SN-001` through `SW-SN-006`). Use one set per database to keep switch
/// identities unique.
/// Switches four and five carry explicit NVOS credentials; the other four leave
/// them unset.
fn expected_switch_configs() -> [ExpectedSwitch; 6] {
    std::array::from_fn(|index| ExpectedSwitch {
        expected_switch_id: None,
        bmc_mac_address: EXPECTED_SWITCH_BMC_MAC_ADDRESS_POOL.allocate(),
        nvos_interfaces: vec![ExpectedNvosInterface {
            mac_address: EXPECTED_SWITCH_NVOS_MAC_ADDRESS_POOL.allocate(),
            is_primary: true,
        }],
        serial_number: format!("SW-SN-{:03}", index + 1),
        bmc_username: "ADMIN".into(),
        bmc_password: "Pwd2023x0x0x0x7".into(),
        nvos_username: if (3..=4).contains(&index) {
            Some(format!("nvos_admin{}", index - 2))
        } else {
            None
        },
        nvos_password: if (3..=4).contains(&index) {
            Some(format!("nvos_pass{}", index - 2))
        } else {
            None
        },
        bmc_ip_address: None,
        nvos_ip_address: None,
        metadata: Metadata {
            name: format!("Switch{}", index + 1),
            description: format!("Test Switch {}", index + 1),
            labels: HashMap::new(),
        },
        rack_id: None,
        bmc_retain_credentials: None,
    })
}

struct TestEnv {
    api: Arc<Api>,
    pool: PgPool,
    test_credential_manager: Arc<TestCredentialManager>,
    test_harness: TestHarness,
}

async fn environment(pool: PgPool) -> TestEnv {
    let credentials = Arc::new(TestCredentialManager::default());
    let api_credentials = credentials.clone();
    let harness = TestHarness::builder(pool.clone())
        .with_api_builder_fn(move |builder| builder.with_credential_manager(api_credentials))
        .build()
        .await;
    let domain = harness.test_domain().await;
    let nc = harness.network_controller();
    nc.create_underlay_segment(&domain).await;
    nc.create_admin_segment(&domain).await;
    TestEnv {
        api: harness.api_arc(),
        pool,
        test_credential_manager: credentials,
        test_harness: harness,
    }
}
impl TestEnv {
    fn db_reader(&self) -> db::db_read::PgPoolReader {
        self.pool.clone().into()
    }
}
async fn attach_switches_with_nvos_credentials(
    env: &TestEnv,
    rack_id: &RackId,
    count: usize,
) -> Result<Vec<SwitchId>, Box<dyn std::error::Error>> {
    let configs = expected_switch_configs();
    let mut expected_switches = Vec::with_capacity(configs.len());
    for config in configs {
        let expected_switch = env.test_harness.create_expected_switch(config.into()).await;
        let mut conn = env.pool.acquire().await?;
        expected_switches.push(expected_switch.db_record(conn.as_mut()).await);
    }
    attach_switches(env.api.as_ref(), rack_id, expected_switches, count).await
}

fn new_rack_id() -> RackId {
    RackId::new(uuid::Uuid::new_v4().to_string())
}

async fn create_ready_rack_with_switch(
    env: &TestEnv,
    pool: &sqlx::PgPool,
) -> Result<(RackId, SwitchId), Box<dyn std::error::Error>> {
    let (rack_id, mut switch_ids) = create_ready_rack_with_switches(env, pool, 1).await?;

    let switch_id = switch_ids
        .pop()
        .ok_or_else(|| eyre::eyre!("expected one switch fixture"))?;

    Ok((rack_id, switch_id))
}

async fn create_ready_rack_with_switches(
    env: &TestEnv,
    pool: &sqlx::PgPool,
    count: usize,
) -> Result<(RackId, Vec<SwitchId>), Box<dyn std::error::Error>> {
    let rack_id = new_rack_id();
    let mut txn = pool.acquire().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        None,
    )
    .await?;
    drop(txn);

    let switch_ids = attach_switches_with_nvos_credentials(env, &rack_id, count).await?;

    let mut txn = pool.begin().await?;
    let rack = get_db_rack(txn.as_mut(), &rack_id).await;
    let updated = db_rack::try_update_controller_state(
        txn.as_mut(),
        &rack_id,
        rack.controller_state.version,
        rack.controller_state.version.increment(),
        &RackState::Ready,
    )
    .await?;
    assert_eq!(updated, db::ConditionalWrite::Applied(()));
    txn.commit().await?;

    Ok((rack_id, switch_ids))
}

async fn get_db_rack<DB>(conn: &mut DB, rack_id: &RackId) -> Rack
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    db_rack::find_by(conn, ObjectColumnFilter::One(db_rack::IdColumn, rack_id))
        .await
        .unwrap()
        .pop()
        .unwrap()
}

#[sqlx_test]
async fn test_on_demand_rack_maintenance_schedules_nvos_only_scope(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = environment(pool.clone()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    env.api
        .on_demand_rack_maintenance(Request::new(rpc::forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id.clone()),
            scope: Some(rpc::forge::RackMaintenanceScope {
                machine_ids: vec![],
                switch_ids: vec![switch_id.to_string()],
                power_shelf_ids: vec![],
                activities: vec![rpc::forge::MaintenanceActivityConfig {
                    activity: Some(
                        rpc::forge::maintenance_activity_config::Activity::NvosUpdate(
                            rpc::forge::NvosUpdateActivity {
                                config_json: r#"{"Id":"fw-nvos"}"#.to_string(),
                                access_token: Some("token".to_string()),
                            },
                        ),
                    ),
                }],
            }),
        }))
        .await?;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let scope = rack
        .config
        .maintenance_requested
        .expect("maintenance should be scheduled");
    assert_eq!(scope.switch_ids, vec![switch_id]);
    assert_eq!(scope.activities.len(), 1);
    assert!(matches!(
        &scope.activities[0],
        MaintenanceActivity::NvosUpdate {
            config_json,
        } if config_json == r#"{"Id":"fw-nvos"}"#
    ));

    Ok(())
}

#[sqlx_test]
async fn test_on_demand_rack_maintenance_schedules_configure_nmx_cluster_only_scope(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = environment(pool.clone()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    env.api
        .on_demand_rack_maintenance(Request::new(rpc::forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id.clone()),
            scope: Some(rpc::forge::RackMaintenanceScope {
                machine_ids: vec![],
                switch_ids: vec![switch_id.to_string()],
                power_shelf_ids: vec![],
                activities: vec![rpc::forge::MaintenanceActivityConfig {
                    activity: Some(
                        rpc::forge::maintenance_activity_config::Activity::ConfigureNmxCluster(
                            rpc::forge::ConfigureNmxClusterActivity {},
                        ),
                    ),
                }],
            }),
        }))
        .await?;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let scope = rack
        .config
        .maintenance_requested
        .expect("maintenance should be scheduled");
    assert_eq!(scope.switch_ids, vec![switch_id]);
    assert_eq!(scope.activities.len(), 1);
    assert!(matches!(
        &scope.activities[0],
        MaintenanceActivity::ConfigureNmxCluster
    ));

    Ok(())
}

#[sqlx_test]
async fn test_on_demand_rack_maintenance_schedules_firmware_and_nvos_scope(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = environment(pool.clone()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    env.api
        .on_demand_rack_maintenance(Request::new(rpc::forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id.clone()),
            scope: Some(rpc::forge::RackMaintenanceScope {
                machine_ids: vec![],
                switch_ids: vec![switch_id.to_string()],
                power_shelf_ids: vec![],
                activities: vec![
                    rpc::forge::MaintenanceActivityConfig {
                        activity: Some(
                            rpc::forge::maintenance_activity_config::Activity::FirmwareUpgrade(
                                rpc::forge::FirmwareUpgradeActivity {
                                    firmware_version: r#"{"Id":"fw-mixed"}"#.to_string(),
                                    components: vec!["BMC".to_string()],
                                    access_token: Some("token".to_string()),
                                    force_update: false,
                                },
                            ),
                        ),
                    },
                    rpc::forge::MaintenanceActivityConfig {
                        activity: Some(
                            rpc::forge::maintenance_activity_config::Activity::NvosUpdate(
                                rpc::forge::NvosUpdateActivity {
                                    config_json: r#"{"Id":"fw-mixed"}"#.to_string(),
                                    access_token: Some("token".to_string()),
                                },
                            ),
                        ),
                    },
                ],
            }),
        }))
        .await?;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    let scope = rack
        .config
        .maintenance_requested
        .expect("maintenance should be scheduled");
    assert_eq!(scope.switch_ids, vec![switch_id]);
    assert_eq!(scope.activities.len(), 2);
    assert!(matches!(
        &scope.activities[0],
        MaintenanceActivity::FirmwareUpgrade {
            firmware_version: Some(id),
            components,
            ..
        } if id == r#"{"Id":"fw-mixed"}"# && components == &vec!["BMC".to_string()]
    ));
    assert!(matches!(
        &scope.activities[1],
        MaintenanceActivity::NvosUpdate {
            config_json,
        } if config_json == r#"{"Id":"fw-mixed"}"#
    ));
    let token_credentials = env
        .test_credential_manager
        .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
            rack_id: rack_id.clone(),
        })
        .await
        .expect("credential lookup should succeed")
        .expect("access token should be stored as a credential");
    assert_eq!(
        token_credentials,
        Credentials::UsernamePassword {
            username: "access_token".to_string(),
            password: "token".to_string(),
        }
    );

    Ok(())
}

#[sqlx_test]
async fn test_on_demand_rack_maintenance_defaults_missing_access_token_to_noauth(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = environment(pool.clone()).await;
    let (rack_id, switch_id) = create_ready_rack_with_switch(&env, &pool).await?;

    env.api
        .on_demand_rack_maintenance(Request::new(rpc::forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id.clone()),
            scope: Some(rpc::forge::RackMaintenanceScope {
                machine_ids: vec![],
                switch_ids: vec![switch_id.to_string()],
                power_shelf_ids: vec![],
                activities: vec![rpc::forge::MaintenanceActivityConfig {
                    activity: Some(
                        rpc::forge::maintenance_activity_config::Activity::FirmwareUpgrade(
                            rpc::forge::FirmwareUpgradeActivity {
                                firmware_version: r#"{"Id":"fw-mixed"}"#.to_string(),
                                components: vec!["BMC".to_string()],
                                access_token: None,
                                force_update: false,
                            },
                        ),
                    ),
                }],
            }),
        }))
        .await?;

    let token_credentials = env
        .test_credential_manager
        .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
            rack_id: rack_id.clone(),
        })
        .await
        .expect("credential lookup should succeed")
        .expect("access token should be stored as a credential");
    assert_eq!(
        token_credentials,
        Credentials::UsernamePassword {
            username: "access_token".to_string(),
            password: carbide_rack::firmware_object::RMS_NOAUTH_ACCESS_TOKEN.to_string(),
        }
    );

    Ok(())
}

#[sqlx_test]
async fn test_terminate_rack_maintenance_latches_request_and_cleans_access_token(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = environment(pool.clone()).await;
    let (rack_id, _) = create_ready_rack_with_switch(&env, &pool).await?;
    let request = || {
        Request::new(rpc::forge::RackMaintenanceTerminateRequest {
            rack_id: Some(rack_id.clone()),
        })
    };

    let error = env
        .api
        .terminate_rack_maintenance(request())
        .await
        .expect_err("Ready rack should reject maintenance termination");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);

    let mut txn = pool.begin().await?;
    let rack = get_db_rack(txn.as_mut(), &rack_id).await;
    assert_eq!(
        db_rack::try_update_controller_state(
            txn.as_mut(),
            &rack_id,
            rack.controller_state.version,
            rack.controller_state.version.increment(),
            &RackState::Maintenance {
                maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                    rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
                },
            },
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    txn.commit().await?;

    env.api
        .credential_manager()
        .set_credentials(
            &CredentialKey::RackMaintenanceAccessToken {
                rack_id: rack_id.clone(),
            },
            &Credentials::UsernamePassword {
                username: "access_token".to_string(),
                password: "token".to_string(),
            },
        )
        .await
        .map_err(|error| eyre::eyre!("failed to set maintenance access token: {}", error))?;

    env.api.terminate_rack_maintenance(request()).await?;
    // An exact retry while the controller has not consumed the latch is
    // idempotent.
    env.api.terminate_rack_maintenance(request()).await?;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(matches!(
        rack.controller_state.value,
        RackState::Maintenance { .. }
    ));
    assert!(rack.config.maintenance_termination_requested);
    assert!(
        env.test_credential_manager
            .get_credentials(&CredentialKey::RackMaintenanceAccessToken {
                rack_id: rack_id.clone(),
            })
            .await
            .map_err(|error| eyre::eyre!("failed to get maintenance access token: {}", error))?
            .is_none()
    );

    Ok(())
}
