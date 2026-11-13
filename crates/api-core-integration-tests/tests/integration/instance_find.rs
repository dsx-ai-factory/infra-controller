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

use carbide_api_core::test_support::default_config;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::network::NetworkPrefixId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use config_version::ConfigVersion;
use mac_address::MacAddress;
use model::instance::config::extension_services::InstanceExtensionServiceConfig;
use model::instance::config::network::InstanceServiceInterfaceConfig;
use model::test_support::ManagedHostConfig;
use tonic::Request;

use super::instance_common::{InstanceEnv, find_instances_by_label};

#[sqlx_test]
async fn test_instance_search_based_on_labels(pool: sqlx::PgPool) {
    let env = InstanceEnv::new(pool.clone(), default_config::get()).await;
    let segment_id = env.segment_id;
    // Each search has one matching instance and two nonmatching instances.
    for i in [1, 4, 8] {
        let mh = env.create_managed_host(ManagedHostConfig::default()).await;

        let tinstance = mh
            .instance_builder(&env.harness)
            .single_interface_network_config(segment_id)
            .metadata(rpc::forge::Metadata {
                name: format!("instance_{i}{i}{i}").to_string(),
                description: format!("instance_{i}{i}{i} have labels").to_string(),
                labels: vec![
                    rpc::forge::Label {
                        key: format!("key_A_{i}{i}{i}").to_string(),
                        value: Some(format!("value_A_{i}{i}{i}").to_string()),
                    },
                    rpc::forge::Label {
                        key: format!("key_B_{i}{i}{i}").to_string(),
                        value: None,
                    },
                ],
            })
            .build()
            .await;
        env.advance_instance_to_ready(&mh, &tinstance).await;
    }

    struct Case {
        scenario: &'static str,
        label: rpc::forge::Label,
        expected_name: &'static str,
    }

    let cases = [
        Case {
            scenario: "value only",
            label: rpc::forge::Label {
                key: "".to_string(),
                value: Some("value_A_444".to_string()),
            },
            expected_name: "instance_444",
        },
        Case {
            scenario: "key only",
            label: rpc::forge::Label {
                key: "key_A_111".to_string(),
                value: None,
            },
            expected_name: "instance_111",
        },
        Case {
            scenario: "key and value",
            label: rpc::forge::Label {
                key: "key_A_888".to_string(),
                value: Some("value_A_888".to_string()),
            },
            expected_name: "instance_888",
        },
    ];

    for case in cases {
        let instances = find_instances_by_label(&env, case.label).await.instances;

        assert_eq!(instances.len(), 1, "{}", case.scenario);
        assert_eq!(
            instances[0].metadata.as_ref().unwrap().name,
            case.expected_name,
            "{}",
            case.scenario
        );
    }
}

/// Verifies a corrupt service-interface record is reported as an internal error,
/// so a valid public read is not blamed for a response conversion failure.
#[sqlx_test]
async fn test_find_instances_reports_corrupt_service_interface_as_internal(pool: sqlx::PgPool) {
    // Create an ordinary instance so only the seeded service-interface record is invalid.
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let segment_id = env.segment_id;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let instance = mh
        .instance_builder(&env.harness)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &instance).await;

    // Seed an active attachment whose service-interface record uses an unsupported ordinal.
    let attachment_id = uuid::Uuid::new_v4();
    let mut txn = env.harness.db_txn().await;
    let stored_instance = instance.db_instance(&mut txn).await;
    let current_extension_services = stored_instance.config.extension_services.clone();
    let mut seeded_extension_services = current_extension_services.clone();
    seeded_extension_services
        .service_configs
        .push(InstanceExtensionServiceConfig {
            id: Some(attachment_id),
            dpu_target: None,
            service_id: ExtensionServiceId::new(),
            version: ConfigVersion::initial(),
            removed: None,
        });
    let mut seeded_network_config = stored_instance.config.network.clone();
    seeded_network_config.service_interfaces = vec![InstanceServiceInterfaceConfig {
        attachment_id,
        interface_ordinal: 1,
        dpu_id: mh.first_dpu().id,
        slot_index: 0,
        vpc_id: VpcId::new(),
        vpc_prefix_id: VpcPrefixId::new(),
        network_segment_id: segment_id,
        network_prefix_id: NetworkPrefixId::new(),
        link_prefix: "192.0.2.0/31".parse().expect("valid service prefix"),
        mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
        internal_uuid: uuid::Uuid::new_v4(),
    }];
    assert_eq!(
        db::instance::update_extension_services_config(
            txn.as_mut(),
            stored_instance.id,
            stored_instance.extension_services_config_version,
            &current_extension_services,
            &seeded_extension_services,
            false,
        )
        .await
        .expect("seed active service attachment"),
        db::ConditionalWrite::Applied(())
    );
    db::instance::update_network_config(
        txn.as_mut(),
        stored_instance.id,
        stored_instance.network_config_version,
        &seeded_network_config,
        false,
    )
    .await
    .expect("seed corrupt service-interface record");
    txn.commit()
        .await
        .expect("commit corrupt service-interface record");

    // A valid request must attribute the response failure to server-owned state.
    let error = env
        .harness
        .api()
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![instance.id],
        }))
        .await
        .expect_err("corrupt service-interface record must fail response conversion");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(
        error
            .message()
            .starts_with("internal error: failed to convert managed-host snapshot for machine ")
    );
    assert!(error.message().contains("unsupported interface ordinals"));
}
