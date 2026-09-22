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
use std::str::FromStr;

use carbide_uuid::machine::MachineId;
use common::api_fixtures::{create_managed_host, create_test_env};
use db::{self};
use model::machine::machine_search_config::MachineSearchConfig;
use rpc::forge::forge_server::Forge;

use crate::tests::common;

#[crate::sqlx_test]
async fn test_topology_update_on_machineid_update(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (host_machine_id, _dpu_machine_id) =
        common::api_fixtures::create_managed_host(&env).await.into();
    let mut txn = env.pool.begin().await.unwrap();
    let host = db::machine::find_one(
        txn.as_mut(),
        &host_machine_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(host.status.hardware_info.as_ref().is_some());

    let mut txn = env.pool.begin().await.unwrap();

    let query = r#"UPDATE machines SET id = $2 WHERE id=$1;"#;

    sqlx::query(query)
        .bind(host.id.to_string())
        .bind("fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg")
        .execute(&mut *txn)
        .await
        .expect("update failed");
    txn.commit().await.unwrap();

    let m_id =
        MachineId::from_str("fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg").unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    let host = db::machine::find_one(
        txn.as_mut(),
        &host_machine_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap();
    assert!(host.is_none());

    let host = db::machine::find_one(txn.as_mut(), &m_id, MachineSearchConfig::default())
        .await
        .unwrap()
        .unwrap();

    assert!(host.status.hardware_info.as_ref().is_some());
}

#[crate::sqlx_test]
async fn test_find_machine_ids_by_bmc_ips(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    // Setup
    let env = create_test_env(db_pool.clone()).await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();
    let host_machine = env.find_machine(&host_machine_id).await.remove(0);

    let bmc_ip = host_machine.bmc_info.as_ref().unwrap().ip();
    let req = tonic::Request::new(rpc::forge::BmcIpList {
        bmc_ips: vec![bmc_ip.to_string()],
    });
    let res = env.api.find_machine_ids_by_bmc_ips(req).await?.into_inner();
    assert_eq!(res.pairs.len(), 1);
    let m = res.pairs.first().unwrap();
    assert_eq!(
        m.machine_id.as_ref().unwrap().to_string(),
        host_machine_id.to_string()
    );
    assert_eq!(m.bmc_ip, bmc_ip);

    Ok(())
}
