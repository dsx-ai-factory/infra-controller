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

use ::rpc::machine_discovery::Gpu;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialWriter, Credentials,
};
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use common::api_fixtures::instance::{
    create_instance_with_nvlink_config, update_instance_nvlink_config,
};
use common::api_fixtures::network_segment::{
    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY_2, create_network_segment,
};
use common::api_fixtures::nvl_logical_partition::create_nvl_logical_partition;
use common::api_fixtures::site_explorer::{
    TestRackDbBuilder, new_host, register_expected_switch_exploration_results,
};
use common::api_fixtures::{
    TestEnv, TestManagedHost, create_managed_host_with_hardware_info_template,
    insert_nvlink_nmxc_endpoint_from_managed_host,
};
use db::{ObjectColumnFilter, rack as db_rack, switch as db_switch};
use ipnetwork::IpNetwork;
use libnmxc::nmxc_model::{GetGpuInfoListRequest, GetPartitionInfoListRequest, GpuAttr};
use model::expected_switch::ExpectedSwitch;
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::metadata::Metadata;
use model::rack::{MaintenanceActivity, RackState};
use model::switch::{
    CONTROL_PLANE_STATE_CONFIGURED, FabricManagerState, FabricManagerStatus, NewSwitch,
    SwitchConfig, SwitchControllerState,
};
use model::test_support::{HardwareInfoTemplate, ManagedHostConfig};
use rpc::forge::TenantState;
use rpc::forge::forge_server::Forge;

use crate::test_support::fixture_config::{FixtureDefault, ManagedHostConfigExt};
use crate::test_support::mac_address_pool::{
    EXPECTED_SWITCH_BMC_MAC_ADDRESS_POOL, EXPECTED_SWITCH_NVOS_MAC_ADDRESS_POOL,
};
use crate::test_support::network::TEST_SITE_PREFIXES;
use crate::tests::common;
use crate::tests::common::api_fixtures::TestEnvOverrides;
use crate::tests::common::api_fixtures::nvl_logical_partition::NvlLogicalPartitionFixture;

const SWITCH_BMC_STATIC_IP: std::net::IpAddr =
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 1, 50));
const SWITCH_NVOS_STATIC_IP: std::net::IpAddr =
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1));
const NMXC_SIMULATOR_PORT: u16 = 9601;

#[crate::sqlx_test]
async fn test_nmx_c_partition_id_migration_deletes_legacy_nmx_m_rows(pool: sqlx::PgPool) {
    let mut conn = pool.acquire().await.unwrap();

    sqlx::query("ALTER TABLE nvlink_partitions RENAME COLUMN nmx_c_partition_id TO nmx_m_id")
        .execute(conn.as_mut())
        .await
        .unwrap();
    sqlx::query(
        "ALTER TABLE nvlink_partitions ALTER COLUMN nmx_m_id TYPE VARCHAR USING nmx_m_id::VARCHAR",
    )
    .execute(conn.as_mut())
    .await
    .unwrap();
    sqlx::query(
        "ALTER TABLE nvlink_partitions ADD CONSTRAINT nvlink_partitions_nmx_m_id_key UNIQUE (nmx_m_id)",
    )
    .execute(conn.as_mut())
    .await
    .unwrap();

    sqlx::query(
        "WITH lp AS (
            INSERT INTO nvlink_logical_partitions (tenant_organization_id, config_version)
            VALUES ('tenant', 'V1-T1666644937952267')
            RETURNING id
        )
        INSERT INTO nvlink_partitions (nmx_m_id, name, domain_uuid, logical_partition_id)
        SELECT '699c4bdb83acac93e9c1476f', 'legacy_partition', gen_random_uuid(), id
        FROM lp",
    )
    .execute(conn.as_mut())
    .await
    .unwrap();

    let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nvlink_partitions")
        .fetch_one(conn.as_mut())
        .await
        .unwrap();
    assert_eq!(row_count, 1);

    sqlx::raw_sql(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../api-db/migrations.pre-squash.20260708172302/20260526120000_nvlink_partitions_nmx_c_partition_id.sql"
    )))
    .execute(conn.as_mut())
    .await
    .unwrap();

    let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nvlink_partitions")
        .fetch_one(conn.as_mut())
        .await
        .unwrap();
    assert_eq!(row_count, 0);

    let legacy_column_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.columns
         WHERE table_name = 'nvlink_partitions'
             AND column_name = 'nmx_m_id'",
    )
    .fetch_one(conn.as_mut())
    .await
    .unwrap();
    assert_eq!(legacy_column_count, 0);

    let nmx_c_column_type: String = sqlx::query_scalar(
        "SELECT data_type
         FROM information_schema.columns
         WHERE table_name = 'nvlink_partitions'
             AND column_name = 'nmx_c_partition_id'",
    )
    .fetch_one(conn.as_mut())
    .await
    .unwrap();
    assert_eq!(nmx_c_column_type, "integer");
}

#[crate::sqlx_test]
async fn test_create_instance_with_nvl_config(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 1);

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    println!("{gpus:?}");

    let mut nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config.clone(), segment_id).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    // test getting all ids
    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 1);

    nvl_config.gpu_configs.iter_mut().for_each(|gpu| {
        gpu.logical_partition_id = None;
    });
    let mut txn = pool.begin().await.unwrap();
    update_instance_nvlink_config(
        &mut txn,
        &instance.id(),
        &InstanceNvLinkConfig::try_from(nvl_config).unwrap(),
    )
    .await;
    txn.commit().await.unwrap();

    // Run twice to record observation.
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 0);

    // delete logical partition. As no physical partitions are present, we expect logical partition to be
    // fully deleted after we run one iteration of monitor
    env.api
        .delete_nv_link_logical_partition(tonic::Request::new(
            rpc::forge::NvLinkLogicalPartitionDeletionRequest {
                id: Some(logical_partition_id),
            },
        ))
        .await
        .expect("expect deletion to succeed");

    let request_partitions = tonic::Request::new(rpc::forge::NvLinkLogicalPartitionsByIdsRequest {
        partition_ids: logical_ids_list.partition_ids,
        include_history: false,
    });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partitions.len(), 1);

    let clone3 = logical_partition_list.partitions[0].clone();
    assert_eq!(logical_partition_id, clone3.id.unwrap());
    assert_eq!(
        _logical_partition.config.unwrap().metadata.unwrap().name,
        clone3.config.unwrap().metadata.unwrap().name
    );
    let status = clone3.status.unwrap();
    assert_eq!(
        TenantState::try_from(status.state).unwrap(),
        TenantState::Terminating
    );

    env.run_nvl_partition_monitor_iteration().await;
    let request_all =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partition_ids.len(), 0);
}

#[crate::sqlx_test]
async fn test_detach_gpus_from_partition_by_clearing_nvlink_config(pool: sqlx::PgPool) {
    // In our tests so far, we detach a GPU from a partition by setting the logical partition ID in the
    // config to None. For tenants using the API, when they detach they omit the GPU they want to detach from the
    // gpu_configs array (so for detaching an entire instance we would get an empty array).

    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 1);

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    println!("{gpus:?}");

    let mut nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config.clone(), segment_id).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    // test getting all ids
    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 1);

    // Check that NMX-C reflects the partition (in-memory / test sim client).
    let mut nmxc_sim_client = env
        .nmxc_sim
        .create_client(libnmxc::Endpoint::new("http://localhost:9601").expect("NMX-C endpoint URI"))
        .await
        .unwrap();
    let nmxc_partitions = nmxc_sim_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;
    assert_eq!(nmxc_partitions.len(), 1);

    nvl_config.gpu_configs = vec![];
    let mut txn = pool.begin().await.unwrap();
    update_instance_nvlink_config(
        &mut txn,
        &instance.id(),
        &InstanceNvLinkConfig::try_from(nvl_config).unwrap(),
    )
    .await;
    txn.commit().await.unwrap();

    // Run twice to record observation.
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 0);

    let mut nmxc_sim_client = env
        .nmxc_sim
        .create_client(libnmxc::Endpoint::new("http://localhost:9601").expect("NMX-C endpoint URI"))
        .await
        .unwrap();
    let nmxc_partitions = nmxc_sim_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;
    assert_eq!(nmxc_partitions.len(), 0);

    // delete logical partition. As no physical partitions are present, we expect logical partition to be
    // fully deleted after we run one iteration of monitor
    env.api
        .delete_nv_link_logical_partition(tonic::Request::new(
            rpc::forge::NvLinkLogicalPartitionDeletionRequest {
                id: Some(logical_partition_id),
            },
        ))
        .await
        .expect("expect deletion to succeed");

    let request_partitions = tonic::Request::new(rpc::forge::NvLinkLogicalPartitionsByIdsRequest {
        partition_ids: logical_ids_list.partition_ids,
        include_history: false,
    });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partitions.len(), 1);

    let clone3 = logical_partition_list.partitions[0].clone();
    assert_eq!(logical_partition_id, clone3.id.unwrap());
    assert_eq!(
        _logical_partition.config.unwrap().metadata.unwrap().name,
        clone3.config.unwrap().metadata.unwrap().name
    );
    let status = clone3.status.unwrap();
    assert_eq!(
        TenantState::try_from(status.state).unwrap(),
        TenantState::Terminating
    );

    env.run_nvl_partition_monitor_iteration().await;
    let request_all =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partition_ids.len(), 0);
}

#[crate::sqlx_test]
async fn test_with_multiple_nv_link_logical_partitions(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    // create two nvlink logical partitions
    let NvlLogicalPartitionFixture {
        id: logical_partition_id1,
        logical_partition: _logical_partition1,
    } = create_nvl_logical_partition(&env, "test_partition1".to_string()).await;
    let NvlLogicalPartitionFixture {
        id: logical_partition_id2,
        logical_partition: _logical_partition2,
    } = create_nvl_logical_partition(&env, "test_partition2".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 2);

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    println!("{gpus:?}");

    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    let nvl_logical_partition_id = if platform_info.module_id - 1 > 2 {
                        Some(logical_partition_id2)
                    } else {
                        Some(logical_partition_id1)
                    };
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: nvl_logical_partition_id,
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config.clone(), segment_id).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    env.run_nvl_partition_monitor_iteration().await;

    // get all nvlink physical partition ids
    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    // if partition_monitor did its job, we expect two nvlink partitions to be created
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 2);
}

#[crate::sqlx_test]
async fn test_nvl_partition_monitor_adds_successful_partitions_when_some_creates_fail(
    pool: sqlx::PgPool,
) {
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    // Fail after one create succeeds.
    let mut overrides = TestEnvOverrides::with_config(config);
    overrides.nmxc_fail_after_n_creates = Some(1);

    let env = common::api_fixtures::create_test_env_with_overrides(pool.clone(), overrides).await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id1,
        logical_partition: _logical_partition1,
    } = create_nvl_logical_partition(&env, "test_partition1".to_string()).await;
    let NvlLogicalPartitionFixture {
        id: logical_partition_id2,
        logical_partition: _logical_partition2,
    } = create_nvl_logical_partition(&env, "test_partition2".to_string()).await;

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;

    let discovery_info = mh
        .host()
        .rpc_machine()
        .await
        .status
        .unwrap()
        .discovery_info
        .unwrap();
    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: None,
                    }
                })
            })
            .collect(),
    };

    let (_tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config.clone(), segment_id).await;

    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    let nvl_logical_partition_id = if platform_info.module_id - 1 > 2 {
                        Some(logical_partition_id2)
                    } else {
                        Some(logical_partition_id1)
                    };
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: nvl_logical_partition_id,
                    }
                })
            })
            .collect(),
    };
    let mut txn = pool.begin().await.unwrap();
    update_instance_nvlink_config(
        &mut txn,
        &instance.id(),
        &InstanceNvLinkConfig::try_from(nvl_config).unwrap(),
    )
    .await;
    txn.commit().await.unwrap();

    // The monitor should successfully create one partition, but the second creation should fail.
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();

    assert_eq!(
        ids_all.partition_ids.len(),
        1,
        "expected exactly one partition in DB when one NMX-C create fails"
    );
}

#[crate::sqlx_test]
async fn test_create_instances_with_nvl_configs_same_logical_partition_different_domains(
    pool: sqlx::PgPool,
) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_create_instances_with_nvl_configs_same_logical_partition_different_domains as nmxc simulator tests are not enabled"
        );
        return;
    }

    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 1);

    let mh1 = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine1 = mh1.host().rpc_machine().await;
    let m2 = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_2_INFO_JSON,
        ),
    )
    .await;
    let machine2 = m2.host().rpc_machine().await;

    assert_eq!(&machine1.state, "Ready");
    assert_eq!(&machine2.state, "Ready");
    let discovery_info1 = machine1
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    let discovery_info2 = machine2
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    assert_eq!(discovery_info1.gpus.len(), 4);
    assert_eq!(discovery_info2.gpus.len(), 4);
    let gpus1: Vec<Gpu> = discovery_info1.gpus.to_vec();
    let gpus2: Vec<Gpu> = discovery_info2.gpus.to_vec();

    let mut nvl_config1 = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus1
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let mut nvl_config2 = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus2
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance1, instance1) =
        create_instance_with_nvlink_config(&env, &mh1, nvl_config1.clone(), segment_id).await;

    let (tinstance2, instance2) =
        create_instance_with_nvlink_config(&env, &m2, nvl_config2.clone(), segment_id).await;

    let machine1 = mh1.host().rpc_machine().await;
    let machine2 = m2.host().rpc_machine().await;
    assert_eq!(&machine1.state, "Assigned/Ready");
    assert_eq!(&machine2.state, "Assigned/Ready");

    let check_instance1 = tinstance1.rpc_instance().await;
    let check_instance2 = tinstance2.rpc_instance().await;
    assert_eq!(instance1.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance2.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance1, check_instance1);
    assert_eq!(instance2, check_instance2);

    // test getting all ids
    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    // if partition_monitor did its job, we expect two new nvlink partitions to be created
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 2);

    nvl_config1.gpu_configs.iter_mut().for_each(|gpu| {
        gpu.logical_partition_id = None;
    });
    nvl_config2.gpu_configs.iter_mut().for_each(|gpu| {
        gpu.logical_partition_id = None;
    });
    let mut txn = pool.begin().await.unwrap();
    // add or remove instance_gpus_from_logical_partition doesn't seem to update db :(
    // till we root cause that, force direct db update from here
    update_instance_nvlink_config(
        &mut txn,
        &instance1.id(),
        &InstanceNvLinkConfig::try_from(nvl_config1).unwrap(),
    )
    .await;
    update_instance_nvlink_config(
        &mut txn,
        &instance2.id(),
        &InstanceNvLinkConfig::try_from(nvl_config2).unwrap(),
    )
    .await;
    txn.commit().await.unwrap();

    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    // if partition monitor did its job after we removed nvlink conifg from an instance, we expect
    // the nvlink partition to be deleted
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 0);

    // delete logical partition. As no physical partitions are present, we expect logical partition to be
    // fully deleted after we run one iteration of monitor
    env.api
        .delete_nv_link_logical_partition(tonic::Request::new(
            rpc::forge::NvLinkLogicalPartitionDeletionRequest {
                id: Some(logical_partition_id),
            },
        ))
        .await
        .expect("expect deletion to succeed");

    let request_partitions = tonic::Request::new(rpc::forge::NvLinkLogicalPartitionsByIdsRequest {
        partition_ids: logical_ids_list.partition_ids,
        include_history: false,
    });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partitions.len(), 1);

    let clone3 = logical_partition_list.partitions[0].clone();
    assert_eq!(logical_partition_id, clone3.id.unwrap());
    assert_eq!(
        _logical_partition.config.unwrap().metadata.unwrap().name,
        clone3.config.unwrap().metadata.unwrap().name
    );
    let status = clone3.status.unwrap();
    assert_eq!(
        TenantState::try_from(status.state).unwrap(),
        TenantState::Terminating
    );

    env.run_nvl_partition_monitor_iteration().await;
    let request_all =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partition_ids.len(), 0);
}

#[crate::sqlx_test]
async fn test_instance_update_logical_partition(pool: sqlx::PgPool) {
    // Test updating directly from partition A to partition B.
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id_1,
        logical_partition: _logical_partition_1,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id_2,
        logical_partition: _logical_partition_2,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 2);

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    println!("{gpus:?}");

    let mut nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id_1),
                    }
                })
            })
            .collect(),
    };

    let (_tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config.clone(), segment_id).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();

    assert_eq!(
        ids_all.partition_ids.len(),
        1,
        "expected exactly one partition in DB"
    );

    let partition_id_1 = ids_all.partition_ids.first().unwrap();

    // Update the GPUs to be in the other logical partition.
    nvl_config.gpu_configs.iter_mut().for_each(|gpu| {
        gpu.logical_partition_id = Some(logical_partition_id_2);
    });
    let mut new_config = instance.config().inner().clone();
    new_config.nvlink = Some(nvl_config);

    env.api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: instance.id().into(),
                if_version_match: None,
                config: Some(new_config),
                metadata: Some(instance.metadata().clone()),
            },
        ))
        .await
        .expect("update nvlink config request should not return an error");

    // Run twice to record observation.
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();

    assert_eq!(
        ids_all.partition_ids.len(),
        1,
        "expected exactly one partition in DB"
    );

    let partition_id_2 = ids_all.partition_ids.first().unwrap();
    assert_ne!(
        partition_id_1, partition_id_2,
        "partition 1 should have been deleted and replaced with partition 2",
    );
}

#[crate::sqlx_test]
async fn test_instance_delete_with_nvl_config(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let mut test_overrides = TestEnvOverrides::with_config(config);
    test_overrides.nmxc_default_partition = Some(true);

    let env =
        common::api_fixtures::create_test_env_with_overrides(pool.clone(), test_overrides).await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 1);

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    println!("{gpus:?}");

    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config.clone(), segment_id).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    // test getting all ids
    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 1);

    // delete the instance. This should force the partition monitor to remove gpus
    // from that instance from physical nvlink partition
    tinstance.delete().await;

    // Run twice to record observation.
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 0);
}

#[crate::sqlx_test]
async fn test_create_instance_add_to_existing_partition(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 1);

    let mh1 = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine1 = mh1.host().rpc_machine().await;
    assert_eq!(&machine1.state, "Ready");
    let discovery_info1 = machine1
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info1.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info1.gpus.to_vec();
    println!("{gpus:?}");

    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh1, nvl_config.clone(), segment_id).await;

    let machine1 = mh1.host().rpc_machine().await;
    assert_eq!(&machine1.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh1.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 1);

    let mut nmxc_sim_client = env
        .nmxc_sim
        .create_client(libnmxc::Endpoint::new("http://localhost:4010").expect("NMX-C endpoint URI"))
        .await
        .unwrap();
    let nmxc_partitions = nmxc_sim_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;

    assert_eq!(nmxc_partitions.len(), 1);
    assert_eq!(nmxc_partitions[0].gpu_uid_list.len(), 4);

    // Now create another instance in the same logical partition and rack.
    let mh2 = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_3_INFO_JSON,
        ),
    )
    .await;
    let machine2 = mh2.host().rpc_machine().await;
    assert_eq!(&machine2.state, "Ready");
    let discovery_info2 = machine2
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    assert_eq!(discovery_info2.gpus.len(), 4);

    let gpus2: Vec<Gpu> = discovery_info2.gpus.to_vec();

    let nvl_config2 = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus2
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance2, instance2) =
        create_instance_with_nvlink_config(&env, &mh2, nvl_config2.clone(), segment_id).await;

    let machine2 = mh2.host().rpc_machine().await;
    assert_eq!(&machine2.state, "Assigned/Ready");
    let check_instance2 = tinstance2.rpc_instance().await;
    assert_eq!(instance2.machine_id(), mh2.id);
    assert_eq!(instance2.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance2, check_instance2);

    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 1);

    let nmxc_partitions = nmxc_sim_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;

    assert_eq!(nmxc_partitions.len(), 1);
    assert_eq!(nmxc_partitions[0].gpu_uid_list.len(), 8);
}

#[crate::sqlx_test]
async fn test_logical_partition_delete_with_instance_config(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;
    // create two nvlink logical partitions
    let NvlLogicalPartitionFixture {
        id: logical_partition_id1,
        logical_partition: _logical_partition1,
    } = create_nvl_logical_partition(&env, "test_partition1".to_string()).await;
    let NvlLogicalPartitionFixture {
        id: logical_partition_id2,
        logical_partition: _logical_partition2,
    } = create_nvl_logical_partition(&env, "test_partition2".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 2);

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    let mut nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id1),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config.clone(), segment_id).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    // test getting all ids
    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 1);

    nvl_config.gpu_configs.iter_mut().for_each(|gpu| {
        gpu.logical_partition_id = None;
    });
    let mut txn = pool.begin().await.unwrap();
    update_instance_nvlink_config(
        &mut txn,
        &instance.id(),
        &InstanceNvLinkConfig::try_from(nvl_config.clone()).unwrap(),
    )
    .await;
    txn.commit().await.unwrap();

    // Run twice to record observation.
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 0);

    // delete logical partition. As no physical partitions are present, we expect logical partition to be
    // fully deleted after we run one iteration of monitor
    env.api
        .delete_nv_link_logical_partition(tonic::Request::new(
            rpc::forge::NvLinkLogicalPartitionDeletionRequest {
                id: Some(logical_partition_id1),
            },
        ))
        .await
        .expect("expect deletion to succeed");

    let request_partitions = tonic::Request::new(rpc::forge::NvLinkLogicalPartitionsByIdsRequest {
        partition_ids: logical_ids_list.partition_ids,
        include_history: false,
    });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partitions.len(), 2);

    env.run_nvl_partition_monitor_iteration().await;
    let request_all =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    // logical partition should be deleted by now, after partition moinitor ran
    let logical_partition_list = env
        .api
        .find_nv_link_logical_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partition_ids.len(), 1);

    nvl_config.gpu_configs.iter_mut().for_each(|gpu| {
        gpu.logical_partition_id = Some(logical_partition_id2);
    });
    let mut txn = pool.begin().await.unwrap();
    update_instance_nvlink_config(
        &mut txn,
        &instance.id(),
        &InstanceNvLinkConfig::try_from(nvl_config).unwrap(),
    )
    .await;
    txn.commit().await.unwrap();

    // delete logical partition. As the partition monitor hasn't been run, there should not
    // be any physical partitions present, but the logical partition should not be deleted
    // as nvlink config in an instance still has reference to the logical partition
    let delete_result = env
        .api
        .delete_nv_link_logical_partition(tonic::Request::new(
            rpc::forge::NvLinkLogicalPartitionDeletionRequest {
                id: Some(logical_partition_id2),
            },
        ))
        .await;
    assert!(
        delete_result.is_err(),
        "deletion should fail while instance still references the logical partition"
    );
    let request_all =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partition_ids.len(), 1);
}

#[crate::sqlx_test]
async fn test_subset_nvl_config_preserves_unconfigured_gpus_in_tray_partition(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let env = common::api_fixtures::create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;
    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;
    let gpus = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap()
        .gpus
        .clone();
    assert_eq!(gpus.len(), 4);

    let gpu_uid = |gpu: &Gpu| {
        let guid = &gpu.platform_info.as_ref().unwrap().fabric_guid;
        let guid = guid
            .strip_prefix("0x")
            .or_else(|| guid.strip_prefix("0X"))
            .unwrap_or(guid);
        u64::from_str_radix(guid, 16).unwrap()
    };

    // Establish the tray default partition before allocating the instance.
    env.run_nvl_partition_monitor_iteration().await;
    let mut nmxc_client = env
        .nmxc_sim
        .create_client(libnmxc::Endpoint::new("http://localhost:4010").unwrap())
        .await
        .unwrap();
    let partitions = nmxc_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: None,
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;
    let tray_partition = partitions
        .iter()
        .find(|partition| partition.name == "tray_partition_0")
        .expect("tray default partition should exist before instance allocation");
    assert_eq!(tray_partition.gpu_uid_list.len(), 4);

    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus[..2]
            .iter()
            .map(|gpu| rpc::forge::InstanceNvLinkGpuConfig {
                device_instance: gpu.platform_info.as_ref().unwrap().module_id - 1,
                logical_partition_id: Some(logical_partition_id),
            })
            .collect(),
    };
    create_instance_with_nvlink_config(&env, &mh, nvl_config, segment_id).await;

    // Run additional passes after instance allocation to verify omitted GPUs remain accounted for
    // after reconciliation converges.
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let partitions = nmxc_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: None,
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;
    assert_eq!(partitions.len(), 2);

    let tray_partition = partitions
        .iter()
        .find(|partition| partition.name == "tray_partition_0")
        .expect("tray default partition should be preserved");
    let tenant_partition = partitions
        .iter()
        .find(|partition| partition.name != "tray_partition_0")
        .expect("tenant partition should be created");

    let mut expected_tenant_uids: Vec<_> = gpus[..2].iter().map(gpu_uid).collect();
    let mut actual_tenant_uids = tenant_partition.gpu_uid_list.clone();
    expected_tenant_uids.sort_unstable();
    actual_tenant_uids.sort_unstable();
    assert_eq!(actual_tenant_uids, expected_tenant_uids);

    let mut expected_tray_uids: Vec<_> = gpus[2..].iter().map(gpu_uid).collect();
    let mut actual_tray_uids = tray_partition.gpu_uid_list.clone();
    expected_tray_uids.sort_unstable();
    actual_tray_uids.sort_unstable();
    assert_eq!(actual_tray_uids, expected_tray_uids);
}

#[crate::sqlx_test]
async fn test_create_instance_gpu_in_unknown_partition(pool: sqlx::PgPool) {
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let mut test_overrides = TestEnvOverrides::with_config(config);
    test_overrides.nmxc_unknown_partition = Some(true);
    let env =
        common::api_fixtures::create_test_env_with_overrides(pool.clone(), test_overrides).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 1);

    // There should be an "unknown" partition in NMX-C (partition id 12345 from sim preset).
    let mut nmxc_sim_client = env
        .nmxc_sim
        .create_client(libnmxc::Endpoint::new("http://localhost:4010").expect("NMX-C endpoint URI"))
        .await
        .unwrap();
    let nmxc_partitions = nmxc_sim_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;
    assert_eq!(nmxc_partitions.len(), 1);

    let mh1 = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON,
        ),
    )
    .await;
    let machine1 = mh1.host().rpc_machine().await;
    assert_eq!(&machine1.state, "Ready");
    let discovery_info1 = machine1
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info1.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info1.gpus.to_vec();

    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh1, nvl_config.clone(), segment_id).await;

    let machine1 = mh1.host().rpc_machine().await;
    assert_eq!(&machine1.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh1.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 1);

    let nmxc_partitions = nmxc_sim_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;
    // Should be 2 partitions in NMX-C
    assert_eq!(nmxc_partitions.len(), 2);
    let gpu_uid_count = nmxc_partitions
        .iter()
        .find(|p| {
            p.partition_id
                .as_ref()
                .is_some_and(|id| id.partition_id != 12345)
        })
        .unwrap()
        .gpu_uid_list
        .len();
    assert_eq!(gpu_uid_count, 4);
}

// `*_use_nmxc_simulator` integration tests only run when environment variable RUN_NMXC_SIMULATOR_TESTS is set (any value).
// Before running these tests, need to have nmx_simulator running on port 9601.
// Ex: "sudo ./install_simulators.sh -p 9601 -n 1 -g nmx-c-nvlink_2.0.0_2025-04-23_01-10_internal.tar.gz  -i 127.0.0.0 -m enabled -t gb200_nvl36r1_c2g4_topology -d true"
// Also nmxc_uid_start in simulator_config.json should be set to 1000 so that GPU UIDs are assinged starting from 1000.
const RUN_NMXC_SIMULATOR_TESTS: &str = "RUN_NMXC_SIMULATOR_TESTS";

const NMXC_SIMULATOR_TLS_CA: &str = "/etc/nmx-controller/nmxc-simulator-test-ca.crt";
const NMXC_SIMULATOR_TLS_CERT: &str = "/etc/nmx-controller/nmxc-simulator-test-tls.crt";
const NMXC_SIMULATOR_TLS_CLIENT_KEY: &str = "/etc/nmx-controller/nmxc-simulator-test-tls.key";
const NMXC_SIMULATOR_TLS_AUTHORITY: &str = "nmxc-simulator.test";

fn nmxc_simulator_tests_enabled() -> bool {
    std::env::var_os(RUN_NMXC_SIMULATOR_TESTS).is_some()
}

const GB200_TRAY_4_CHASSIS_SERIAL: &str = "27XYX27000001";

/// Removes the `nvlink_nmxc_endpoints` row for `chassis_serial` so NMX-C resolution fails in tests.
async fn delete_nvlink_nmxc_endpoint(pool: &sqlx::PgPool, chassis_serial: &str) {
    let mut txn = pool
        .begin()
        .await
        .expect("begin txn for nvlink_nmxc_endpoint delete");
    assert!(
        db::nvlink_nmxc_endpoints::delete(txn.as_mut(), chassis_serial)
            .await
            .expect("delete nvlink_nmxc_endpoint"),
        "nvlink_nmxc_endpoint row missing for {chassis_serial}"
    );
    txn.commit()
        .await
        .expect("commit nvlink_nmxc_endpoint delete");
}

/// Asserts the machine has a populated NVLink status observation with partition assignments.
async fn assert_machine_nvlink_observation_present(
    mh: &TestManagedHost,
    expected_gpu_count: usize,
) {
    let machine = mh.host().rpc_machine().await;
    let observation = machine
        .status
        .as_ref()
        .unwrap()
        .nvlink
        .as_ref()
        .expect("expected nvlink_status_observation to be set");
    assert_eq!(observation.gpu_status.len(), expected_gpu_count);
    for gpu_obs in &observation.gpu_status {
        assert!(
            gpu_obs.logical_partition_id.is_some(),
            "expected logical_partition_id on gpu observation"
        );
        assert!(
            gpu_obs.partition_id.is_some(),
            "expected partition_id on gpu observation"
        );
    }
}

/// Asserts `nvlink_status_observation` was cleared (null) via RPC and in the database.
async fn assert_machine_nvlink_observation_null(mh: &TestManagedHost, pool: &sqlx::PgPool) {
    let machine = mh.host().rpc_machine().await;
    assert!(
        machine.status.as_ref().unwrap().nvlink.is_none(),
        "expected null nvlink_status_observation via RPC, got {:?}",
        machine.status.as_ref().unwrap().nvlink
    );

    let mut txn = pool
        .begin()
        .await
        .expect("begin txn for nvlink observation check");
    let db_machine = mh.host().db_machine(&mut txn).await;
    assert!(
        db_machine.status.nvlink_status_observation.is_none(),
        "expected null nvlink_status_observation in DB, got {:?}",
        db_machine.status.nvlink_status_observation
    );
    txn.commit().await.expect("commit nvlink observation check");
}

async fn run_create_instance_with_nvl_config_nmxc_simulator_scenario(
    pool: sqlx::PgPool,
    with_mtls: bool,
) {
    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
        nvlink_config.nmx_c_endpoint_port = Some(NMXC_SIMULATOR_PORT);
        if with_mtls {
            nvlink_config.nmx_c_tls_ca_cert_path = Some(NMXC_SIMULATOR_TLS_CA.to_string());
            nvlink_config.nmx_c_tls_client_cert_path = Some(NMXC_SIMULATOR_TLS_CERT.to_string());
            nvlink_config.nmx_c_tls_client_key_path =
                Some(NMXC_SIMULATOR_TLS_CLIENT_KEY.to_string());
            nvlink_config.nmx_c_tls_authority = Some(NMXC_SIMULATOR_TLS_AUTHORITY.to_string());
        }
    }

    let mut overrides = TestEnvOverrides::with_config(config);
    overrides.nmxc_simulator = Some(true);

    let env = common::api_fixtures::create_test_env_with_overrides(pool.clone(), overrides).await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 1);

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_4_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    let mut nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config.clone(), segment_id).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 1);

    nvl_config.gpu_configs.iter_mut().for_each(|gpu| {
        gpu.logical_partition_id = None;
    });
    let mut txn = pool.begin().await.unwrap();
    update_instance_nvlink_config(
        &mut txn,
        &instance.id(),
        &InstanceNvLinkConfig::try_from(nvl_config).unwrap(),
    )
    .await;
    txn.commit().await.unwrap();

    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 0);

    env.api
        .delete_nv_link_logical_partition(tonic::Request::new(
            rpc::forge::NvLinkLogicalPartitionDeletionRequest {
                id: Some(logical_partition_id),
            },
        ))
        .await
        .expect("expect deletion to succeed");

    let request_partitions = tonic::Request::new(rpc::forge::NvLinkLogicalPartitionsByIdsRequest {
        partition_ids: logical_ids_list.partition_ids,
        include_history: false,
    });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partitions_by_ids(request_partitions)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partitions.len(), 1);

    let clone3 = logical_partition_list.partitions[0].clone();
    assert_eq!(logical_partition_id, clone3.id.unwrap());
    assert_eq!(
        _logical_partition.config.unwrap().metadata.unwrap().name,
        clone3.config.unwrap().metadata.unwrap().name
    );
    let status = clone3.status.unwrap();
    assert_eq!(
        TenantState::try_from(status.state).unwrap(),
        TenantState::Terminating
    );

    env.run_nvl_partition_monitor_iteration().await;
    let request_all =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_partition_list = env
        .api
        .find_nv_link_logical_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_partition_list.partition_ids.len(), 0);
}

#[crate::sqlx_test]
async fn test_create_instance_with_nvl_config_use_nmxc_simulator(pool: sqlx::PgPool) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_create_instance_with_nvl_config_use_nmxc_simulator as nmxc simulator tests are not enabled"
        );
        return;
    }
    run_create_instance_with_nvl_config_nmxc_simulator_scenario(pool, false).await;
}

async fn seed_switch_endpoint_records(
    txn: &mut sqlx::PgConnection,
    bmc_mac: mac_address::MacAddress,
    nvos_mac: mac_address::MacAddress,
    serial_number: &str,
    switch_name: &str,
    rack_id: &RackId,
) {
    db::expected_switch::create(
        txn,
        ExpectedSwitch {
            expected_switch_id: None,
            bmc_mac_address: bmc_mac,
            nvos_mac_addresses: vec![nvos_mac],
            serial_number: serial_number.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "Pwd2023x0x0x0x7".into(),
            nvos_username: None,
            nvos_password: None,
            bmc_ip_address: Some(SWITCH_BMC_STATIC_IP),
            nvos_ip_address: Some(SWITCH_NVOS_STATIC_IP),
            metadata: Metadata {
                name: switch_name.to_string(),
                description: "Test switch for NMX-C simulator".to_string(),
                labels: Default::default(),
            },
            rack_id: Some(rack_id.clone()),
            bmc_retain_credentials: None,
        },
    )
    .await
    .expect("create expected switch");

    db::machine_interface::preallocate_bmc_machine_interface(
        txn,
        bmc_mac,
        SWITCH_BMC_STATIC_IP,
        None,
    )
    .await
    .expect("BMC machine interface");
    db::machine_interface::preallocate_machine_interface(
        txn,
        nvos_mac,
        SWITCH_NVOS_STATIC_IP,
        None,
    )
    .await
    .expect("NVOS machine interface");
}

async fn create_rack_switch_for_nmxc_simulator(env: &TestEnv, rack_id: &RackId) -> SwitchId {
    const SWITCH_SERIAL: &str = "SW-SN-001";
    let bmc_mac = EXPECTED_SWITCH_BMC_MAC_ADDRESS_POOL.allocate();
    let nvos_mac = EXPECTED_SWITCH_NVOS_MAC_ADDRESS_POOL.allocate();
    let switch_id = model::switch::switch_id::from_hardware_info(
        SWITCH_SERIAL,
        "NVIDIA",
        "Switch",
        carbide_uuid::switch::SwitchIdSource::ProductBoardChassisSerial,
        carbide_uuid::switch::SwitchType::NvLink,
    )
    .expect("switch id");

    let mut txn = env.pool.begin().await.expect("begin txn");
    seed_switch_endpoint_records(
        txn.as_mut(),
        bmc_mac,
        nvos_mac,
        SWITCH_SERIAL,
        "Switch1",
        rack_id,
    )
    .await;
    db_switch::create(
        txn.as_mut(),
        &NewSwitch {
            id: switch_id,
            config: SwitchConfig {
                name: "Switch1".to_string(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some(bmc_mac),
            metadata: None,
            rack_id: Some(rack_id.clone()),
            slot_number: Some(0),
            tray_index: Some(0),
        },
    )
    .await
    .expect("create switch");

    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await
        .expect("load switch")
        .expect("switch");
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::Ready,
    )
    .await
    .expect("set switch ready");
    db_switch::update_fabric_manager_status(
        txn.as_mut(),
        switch_id,
        Some(&FabricManagerStatus {
            fabric_manager_state: FabricManagerState::Ok,
            addition_info: Some(CONTROL_PLANE_STATE_CONFIGURED.to_string()),
            reason: None,
            error_message: None,
        }),
    )
    .await
    .expect("set fabric manager status");
    db_switch::set_primary_switch_for_rack(txn.as_mut(), rack_id, &switch_id)
        .await
        .expect("set primary switch");
    txn.commit().await.expect("commit switch");

    let credentials = Credentials::UsernamePassword {
        username: "admin".to_string(),
        password: "password".to_string(),
    };
    env.test_credential_manager
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: bmc_mac,
                },
            },
            &credentials,
        )
        .await
        .expect("set switch BMC credentials");
    env.test_credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: bmc_mac,
            },
            &credentials,
        )
        .await
        .expect("set switch NVOS credentials");

    switch_id
}

#[crate::sqlx_test]
async fn test_rack_switch_create_instance_with_nvl_config_use_nmxc_simulator(pool: sqlx::PgPool) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_rack_switch_create_nvl_config_use_nmxc_simulator as nmxc simulator tests are not enabled"
        );
        return;
    }

    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
        nvlink_config.nmx_c_endpoint_port = Some(NMXC_SIMULATOR_PORT);
        nvlink_config.allow_insecure = true;
    }

    let mut overrides = TestEnvOverrides::with_config(config);
    overrides.nmxc_simulator = Some(true);
    let mut site_prefixes = TEST_SITE_PREFIXES.clone();
    site_prefixes.push(*FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY_2);
    overrides.site_prefixes = Some(site_prefixes);

    let env = common::api_fixtures::create_test_env_with_overrides(pool.clone(), overrides).await;

    create_network_segment(
        &env.api,
        "ADMIN2",
        &IpNetwork::new(
            FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY_2.network(),
            FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY_2.prefix(),
        )
        .unwrap()
        .to_string(),
        &FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY_2
            .ip()
            .to_string(),
        rpc::forge::NetworkSegmentType::Admin,
        None,
        true,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let rack_id: RackId = "rack_1".parse().expect("rack id");
    let mut txn = pool.begin().await.expect("begin txn");
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await
        .expect("create rack");
    txn.commit().await.expect("commit rack");

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    // Switch must exist before host discovery so NMX-C endpoint resolution can use rack NVOS IP.
    let _switch_id = create_rack_switch_for_nmxc_simulator(&env, &rack_id).await;

    register_expected_switch_exploration_results(&env)
        .await
        .expect("register switch endpoint exploration mocks");

    let hardware_info_template = HardwareInfoTemplate::Custom(
        crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_4_INFO_JSON,
    );
    insert_nvlink_nmxc_endpoint_from_managed_host(&env, &hardware_info_template).await;
    let mh_snapshot = new_host(
        &env,
        ManagedHostConfig::default()
            .with_hardware_info_template(hardware_info_template)
            .with_admin_dhcp_fallback(),
    )
    .await
    .expect("create managed host");
    let mh = TestManagedHost {
        id: mh_snapshot
            .host_snapshot
            .id
            .try_into()
            .expect("host snapshot ID should be a valid HostMachineId"),
        dpu_ids: mh_snapshot
            .dpu_snapshots
            .into_iter()
            .map(|snapshot| snapshot.id)
            .collect(),
        api: env.api.clone(),
    };

    let mut txn = pool.begin().await.expect("begin txn");
    sqlx::query("UPDATE machines SET rack_id = $1 WHERE id = $2")
        .bind(rack_id.as_str())
        .bind(mh.id)
        .execute(txn.as_mut())
        .await
        .expect("set machine rack_id");
    txn.commit().await.expect("commit machine rack_id");

    let machine = mh.host().rpc_machine().await;
    assert_eq!(machine.rack_id.as_ref(), Some(&rack_id));
    assert_eq!(&machine.state, "Ready");

    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();
    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config, segment_id).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let ids_all = env
        .api
        .find_nv_link_partition_ids(tonic::Request::new(
            rpc::forge::NvLinkPartitionSearchFilter {
                name: None,
                tenant_organization_id: None,
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ids_all.partition_ids.len(), 1);
}

// mTLS scenario. For this test, the simulator needs to be configured with mTLS.
// Ex: "sudo ./install_simulators.sh -p 9601 -n 1 -g nmx-c-nvlink_2.0.0_2025-04-23_01-10_internal.tar.gz  -i 127.0.0.0 -m enabled -t gb200_nvl36r1_c2g4_topology -d true -c /etc/nmx-controller/nmxc-simulator-test-tls.crt -k /etc/nmx-controller/nmxc-simulator-test-tls.key -a /etc/nmx-controller/nmxc-simulator-test-ca.crt -e mtls"
// This test uses the following harcoded mtls config:
// nmxc-simulator-test-ca.crt is the CA certificate
// nmxc-simulator-test-tls.crt is the leaf certificate
// nmxc-simulator-test-tls.key is the client key
// nmxc-simulator.test is the authority
#[crate::sqlx_test]
async fn test_create_instance_with_nvl_config_mtls_use_nmxc_simulator(pool: sqlx::PgPool) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_create_instance_with_nvl_config_mtls_use_nmxc_simulator as nmxc simulator tests are not enabled"
        );
        return;
    }
    run_create_instance_with_nvl_config_nmxc_simulator_scenario(pool, true).await;
}

async fn assert_switch_cert_monitor_nmxc_simulator_probe(
    pool: sqlx::PgPool,
    rotate_before_expiry: std::time::Duration,
    expected_certificates_needing_rotation: usize,
) {
    let mut config = common::api_fixtures::get_config_with_rack_profiles();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
        nvlink_config.nmx_c_endpoint_port = Some(NMXC_SIMULATOR_PORT);
        nvlink_config.nmx_c_tls_ca_cert_path = Some(NMXC_SIMULATOR_TLS_CA.to_string());
        nvlink_config.nmx_c_tls_client_cert_path = Some(NMXC_SIMULATOR_TLS_CERT.to_string());
        nvlink_config.nmx_c_tls_client_key_path = Some(NMXC_SIMULATOR_TLS_CLIENT_KEY.to_string());
        nvlink_config.nmx_c_tls_authority = Some(NMXC_SIMULATOR_TLS_AUTHORITY.to_string());
        nvlink_config.nmx_c_certificate_rotation.enabled = true;
        nvlink_config
            .nmx_c_certificate_rotation
            .rotate_before_expiry = rotate_before_expiry;
    }

    let mut overrides = TestEnvOverrides::with_config(config);
    overrides.nmxc_simulator = Some(true);
    let env = common::api_fixtures::create_test_env_with_overrides(pool.clone(), overrides).await;

    let rack_id: RackId = "rack-cert-monitor".parse().expect("rack id");
    let mut txn = pool.begin().await.expect("begin txn");
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await
        .expect("create rack");
    let rack = db_rack::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .expect("load rack")
    .pop()
    .expect("rack");
    let updated = db_rack::try_update_controller_state(
        txn.as_mut(),
        &rack_id,
        rack.controller_state.version,
        rack.controller_state.version.increment(),
        &RackState::Ready,
    )
    .await
    .expect("set rack ready");
    assert!(updated, "rack should transition to Ready for rotation");
    txn.commit().await.expect("commit rack");

    let switch_id = create_rack_switch_for_nmxc_simulator(&env, &rack_id).await;

    let result = env.run_switch_cert_monitor_iteration().await;
    assert_eq!(result.observed_endpoints, 1);
    assert_eq!(result.successful_probes, 1);
    assert_eq!(
        result.certificates_needing_rotation,
        expected_certificates_needing_rotation
    );
    assert_eq!(result.probe_errors, 0);
    assert_eq!(result.applied_updates, 0);
    assert_eq!(
        result.pending_updates,
        expected_certificates_needing_rotation
    );
    assert_eq!(result.apply_errors, 0);

    let mut txn = pool.begin().await.expect("begin txn");
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await
        .expect("load switch")
        .expect("switch");
    let rack = db_rack::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .expect("load rack")
    .pop()
    .expect("rack");
    txn.commit().await.expect("commit txn");

    if expected_certificates_needing_rotation > 0 {
        assert!(
            switch.switch_maintenance_requested.is_none(),
            "certificate rotation should not request per-switch maintenance"
        );
        let scope = rack
            .config
            .maintenance_requested
            .as_ref()
            .expect("rack NMX cluster maintenance request");
        assert!(scope.is_full_rack());
        assert_eq!(
            scope.activities,
            vec![MaintenanceActivity::ConfigureNmxCluster]
        );
    } else {
        assert!(
            switch.switch_maintenance_requested.is_none(),
            "certificate outside the rotation window should not request switch maintenance"
        );
        assert!(
            rack.config.maintenance_requested.is_none(),
            "certificate outside the rotation window should not request rack maintenance"
        );
    }

    let rms_requests = env
        .rms_sim
        .submitted_configure_switch_certificate_requests()
        .await;
    assert_eq!(
        rms_requests.len(),
        0,
        "switch cert monitor should queue the rack state machine instead of calling RMS directly"
    );
}

#[crate::sqlx_test]
async fn test_switch_cert_monitor_rotates_nmxc_simulator_cert_inside_rotation_window(
    pool: sqlx::PgPool,
) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_switch_cert_monitor_rotates_nmxc_simulator_cert_inside_rotation_window as nmxc simulator tests are not enabled"
        );
        return;
    }

    assert_switch_cert_monitor_nmxc_simulator_probe(
        pool,
        std::time::Duration::from_secs(100 * 365 * 24 * 60 * 60),
        1,
    )
    .await;
}

#[crate::sqlx_test]
async fn test_switch_cert_monitor_skips_nmxc_simulator_cert_outside_rotation_window(
    pool: sqlx::PgPool,
) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_switch_cert_monitor_skips_nmxc_simulator_cert_outside_rotation_window as nmxc simulator tests are not enabled"
        );
        return;
    }

    assert_switch_cert_monitor_nmxc_simulator_probe(pool, std::time::Duration::ZERO, 0).await;
}

// This test creates two instances in the same logical partition but on different domains.
// This test relies on having two NMX-C simulator instances running on ports 9601 and 9602.
// Simulator running on port 9601 simulates a tray as defined in GB200_COMPUTE_TRAY_4_INFO_JSON.
// Simulator running on port 9602 simulates a tray as defined in GB200_COMPUTE_TRAY_5_INFO_JSON.
#[crate::sqlx_test]
async fn test_create_instance_multiple_domains_use_nmxc_simulator(pool: sqlx::PgPool) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_create_instance_multiple_domains_use_nmxc_simulator as nmxc simulator tests are not enabled"
        );
        return;
    }

    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let mut test_overrides = TestEnvOverrides::with_config(config);
    test_overrides.nmxc_simulator = Some(true);

    let env =
        common::api_fixtures::create_test_env_with_overrides(pool.clone(), test_overrides).await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 1);

    common::api_fixtures::set_nvlink_nmxc_endpoint(&env, "27XYX27000001", "http://localhost:9601")
        .await;
    let mh4 = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_4_INFO_JSON,
        ),
    )
    .await;
    common::api_fixtures::set_nvlink_nmxc_endpoint(&env, "27ZYX27000001", "http://localhost:9602")
        .await;
    let mh5 = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_5_INFO_JSON,
        ),
    )
    .await;

    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let mut nmxc_client_9601 = env
        .nmxc_sim
        .create_client(libnmxc::Endpoint::new("http://localhost:9601").expect("NMX-C endpoint URI"))
        .await
        .expect("create NMX-C client for domain :9601");
    let _nmxc_gpus_9601 = nmxc_client_9601
        .get_gpu_info_list(GetGpuInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            attr: GpuAttr::NmxGpuAttrAll as i32,
            num_gpus: 0,
            loc: None,
            partition_id: None,
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
            gpu_health: 0,
        })
        .await
        .expect("GetGpuInfoList on :9601")
        .gpu_info_list;
    let mut nmxc_client_9602 = env
        .nmxc_sim
        .create_client(libnmxc::Endpoint::new("http://localhost:9602").expect("NMX-C endpoint URI"))
        .await
        .expect("create NMX-C client for domain :9602");
    let _nmxc_gpus_9602 = nmxc_client_9602
        .get_gpu_info_list(GetGpuInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            attr: GpuAttr::NmxGpuAttrAll as i32,
            num_gpus: 0,
            loc: None,
            partition_id: None,
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
            gpu_health: 0,
        })
        .await
        .expect("GetGpuInfoList on :9602")
        .gpu_info_list;

    let machine4 = mh4.host().rpc_machine().await;
    let machine5 = mh5.host().rpc_machine().await;

    assert_eq!(&machine4.state, "Ready");
    assert_eq!(&machine5.state, "Ready");

    let discovery_info4 = machine4
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    let discovery_info5 = machine5
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    assert_eq!(discovery_info4.gpus.len(), 4);
    assert_eq!(discovery_info5.gpus.len(), 4);

    let gpus4: Vec<Gpu> = discovery_info4.gpus.to_vec();
    let gpus5: Vec<Gpu> = discovery_info5.gpus.to_vec();

    let nvl_config5 = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus5
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let nvl_config4 = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus4
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance4, instance4) =
        create_instance_with_nvlink_config(&env, &mh4, nvl_config4.clone(), segment_id).await;
    let (tinstance5, instance5) =
        create_instance_with_nvlink_config(&env, &mh5, nvl_config5.clone(), segment_id).await;

    let machine4 = mh4.host().rpc_machine().await;
    let machine5 = mh5.host().rpc_machine().await;
    assert_eq!(&machine4.state, "Assigned/Ready");
    assert_eq!(&machine5.state, "Assigned/Ready");

    let check_instance4 = tinstance4.rpc_instance().await;
    let check_instance5 = tinstance5.rpc_instance().await;
    assert_eq!(instance4.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance5.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance4, check_instance4);
    assert_eq!(instance5, check_instance5);
    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 2);
}

#[crate::sqlx_test]
async fn test_instance_delete_with_nvl_config_use_nmxc_simulator(pool: sqlx::PgPool) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_instance_delete_with_nvl_config_use_nmxc_simulator as nmxc simulator tests are not enabled"
        );
        return;
    }

    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let mut test_overrides = TestEnvOverrides::with_config(config);
    test_overrides.nmxc_simulator = Some(true);

    let env =
        common::api_fixtures::create_test_env_with_overrides(pool.clone(), test_overrides).await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let request_logical_ids =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });

    let logical_ids_list = env
        .api
        .find_nv_link_logical_partition_ids(request_logical_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(logical_ids_list.partition_ids.len(), 1);

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_4_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    println!("{gpus:?}");

    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config.clone(), segment_id).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Assigned/Ready");

    let check_instance = tinstance.rpc_instance().await;
    assert_eq!(instance.machine_id(), mh.id);
    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
    assert_eq!(instance, check_instance);

    // test getting all ids
    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });
    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 1);

    // delete the instance. This should force the partition monitor to remove gpus
    // from that instance from physical nvlink partition
    tinstance.delete().await;

    // Run twice to record observation.
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    env.run_nvl_partition_monitor_iteration().await;

    let request_all = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let ids_all = env
        .api
        .find_nv_link_partition_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.partition_ids.len(), 0);
}

#[crate::sqlx_test]
async fn test_managed_host_creation_with_tray_default_partition_use_nmxc_simulator(
    pool: sqlx::PgPool,
) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_instance_delete_with_nvl_config_use_nmxc_simulator as nmxc simulator tests are not enabled"
        );
        return;
    }

    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let mut test_overrides = TestEnvOverrides::with_config(config);
    test_overrides.nmxc_simulator = Some(true);

    let env =
        common::api_fixtures::create_test_env_with_overrides(pool.clone(), test_overrides).await;

    let _segment_id = env.create_vpc_and_tenant_segment().await;

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_4_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;

    assert_eq!(&machine.state, "Ready");
    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();

    assert_eq!(discovery_info.gpus.len(), 4);

    let gpus: Vec<Gpu> = discovery_info.gpus.to_vec();

    println!("{gpus:?}");

    // Run twice to record observation.
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let mut nmxc_sim_client = env
        .nmxc_sim
        .create_client(libnmxc::Endpoint::new("http://localhost:9601").expect("NMX-C endpoint URI"))
        .await
        .unwrap();
    let nmxc_partitions = nmxc_sim_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;
    assert_eq!(nmxc_partitions.len(), 1);
    assert_eq!(nmxc_partitions[0].name, "tray_partition_1");
}

/// Verifies null `nvlink_status_observation` is written when the NMX-C endpoint cannot be resolved.
/// Verifies the NVLink config is not synced when NMX-C is unreachable.
#[crate::sqlx_test]
async fn test_null_nvlink_observation_after_nmxc_unreachable_use_nmxc_simulator(
    pool: sqlx::PgPool,
) {
    if !nmxc_simulator_tests_enabled() {
        println!(
            "skipping test_null_nvlink_observation_after_nmxc_unreachable_use_nmxc_simulator as nmxc simulator tests are not enabled"
        );
        return;
    }

    let mut config = common::api_fixtures::get_config();
    if let Some(nvlink_config) = config.nvlink_config.as_mut() {
        nvlink_config.enabled = true;
    }

    let mut test_overrides = TestEnvOverrides::with_config(config);
    test_overrides.nmxc_simulator = Some(true);

    let env =
        common::api_fixtures::create_test_env_with_overrides(pool.clone(), test_overrides).await;

    let segment_id = env.create_vpc_and_tenant_segment().await;

    let NvlLogicalPartitionFixture {
        id: logical_partition_id,
        logical_partition: _logical_partition,
    } = create_nvl_logical_partition(&env, "test_partition".to_string()).await;

    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(
            crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_4_INFO_JSON,
        ),
    )
    .await;
    let machine = mh.host().rpc_machine().await;
    assert_eq!(&machine.state, "Ready");

    let discovery_info = machine
        .status
        .as_ref()
        .unwrap()
        .discovery_info
        .as_ref()
        .unwrap();
    assert_eq!(discovery_info.gpus.len(), 4);

    let nvl_config = rpc::forge::InstanceNvLinkConfig {
        gpu_configs: discovery_info
            .gpus
            .iter()
            .filter_map(|gpu| {
                gpu.platform_info.as_ref().map(|platform_info| {
                    rpc::forge::InstanceNvLinkGpuConfig {
                        device_instance: platform_info.module_id - 1,
                        logical_partition_id: Some(logical_partition_id),
                    }
                })
            })
            .collect(),
    };

    let (tinstance, instance) =
        create_instance_with_nvlink_config(&env, &mh, nvl_config, segment_id).await;

    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);

    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;

    let ids_all = env
        .api
        .find_nv_link_partition_ids(tonic::Request::new(
            rpc::forge::NvLinkPartitionSearchFilter {
                name: None,
                tenant_organization_id: None,
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ids_all.partition_ids.len(), 1);

    let mut nmxc_sim_client = env
        .nmxc_sim
        .create_client(libnmxc::Endpoint::new("http://localhost:9601").expect("NMX-C endpoint URI"))
        .await
        .unwrap();
    let nmxc_partitions = nmxc_sim_client
        .get_partition_info_list(GetPartitionInfoListRequest {
            context: Some(libnmxc::nmxc_model::Context {
                context: String::new(),
            }),
            partition_id_list: vec![],
            partition_name_list: vec![],
            gateway_id: libnmxc::NMX_C_GATEWAY_ID.into(),
        })
        .await
        .unwrap()
        .partition_info_list;
    assert_eq!(nmxc_partitions.len(), 1);

    assert_machine_nvlink_observation_present(&mh, 4).await;

    let instance = tinstance.rpc_instance().await;
    let instance_status = instance.status();
    let nvl_status = instance_status.inner().nvlink.as_ref().unwrap();
    assert_eq!(nvl_status.configs_synced(), rpc::SyncState::Synced);

    delete_nvlink_nmxc_endpoint(&pool, GB200_TRAY_4_CHASSIS_SERIAL).await;

    env.run_nvl_partition_monitor_iteration().await;

    assert_machine_nvlink_observation_null(&mh, &pool).await;

    let instance_after_failure = tinstance.rpc_instance().await;
    let instance_status_after_failure = instance_after_failure.status();
    let nvlink_status_after_failure = instance_status_after_failure
        .inner()
        .nvlink
        .as_ref()
        .expect("expected nvlink status after monitor iteration");
    assert_ne!(
        nvlink_status_after_failure.configs_synced(),
        rpc::SyncState::Synced,
        "nvlink config must not remain Synced when NMX-C is unreachable"
    );
}
