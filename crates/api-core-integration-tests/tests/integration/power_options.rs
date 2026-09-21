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

use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::power_manager::PowerState;
use model::test_support::ManagedHostConfig;
use rpc::forge::{MaintenanceOperation, MaintenanceRequest, PowerOptionUpdateRequest};
use tonic::Request;

struct TestContext {
    env: TestHarness,
    mh: TestManagedHost,
}

async fn init(pool: PgPool) -> TestContext {
    let mut runtime_config = carbide_test_harness::test_support::default_config::get();
    runtime_config.power_manager_options.enabled = true;
    let runtime_config = Arc::new(runtime_config);
    let env = TestHarness::builder(pool)
        .with_api_builder_fn(move |builder| builder.with_runtime_config(runtime_config))
        .build()
        .await;

    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let mh = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await
        .0;

    TestContext { env, mh }
}

#[sqlx_test]
async fn creates_and_updates_power_options(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let TestContext { env, mh } = init(pool).await;

    let mut txn = env.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options.len(), 1);
    assert_eq!(power_options[0].host_id, mh.host.id);
    assert_eq!(power_options[0].desired_power_state, PowerState::On);
    txn.rollback().await?;

    env.api()
        .set_maintenance(Request::new(MaintenanceRequest {
            operation: MaintenanceOperation::Enable as i32,
            host_id: Some(mh.host.id.into()),
            reference: Some("testing".to_string()),
        }))
        .await?;

    env.api()
        .update_power_option(Request::new(PowerOptionUpdateRequest {
            machine_id: Some(mh.host.id),
            power_state: rpc::forge::PowerState::Off as i32,
        }))
        .await?;

    let mut txn = env.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;

    assert_eq!(power_options.len(), 1);
    assert_eq!(power_options[0].desired_power_state, PowerState::Off);
    txn.rollback().await?;
    Ok(())
}

#[sqlx_test]
async fn rejects_update_without_maintenance(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let TestContext { env, mh } = init(pool).await;

    let mut txn = env.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options.len(), 1);
    assert_eq!(power_options[0].host_id, mh.host.id);
    assert_eq!(power_options[0].desired_power_state, PowerState::On);
    txn.rollback().await?;

    let error = env
        .api()
        .update_power_option(Request::new(PowerOptionUpdateRequest {
            machine_id: Some(mh.host.id),
            power_state: rpc::forge::PowerState::Off as i32,
        }))
        .await
        .unwrap_err();

    assert_eq!(
        error.message(),
        "machine must have a 'maintenance' health alert with 'SupressExternalAlerting' classification"
    );
    Ok(())
}
